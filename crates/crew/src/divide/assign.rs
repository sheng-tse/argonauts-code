//! Owners, the review ring, and chores for a validated graph. What it knows about each worker
//! comes in through [`Crew`], so the whole assignment is plain code over its inputs.

use std::collections::BTreeMap;

use rand::RngExt;
use rand::rngs::StdRng;

use argonauts_core::CardId;
use argonauts_core::events::card::{CardKind, CardSpec};
use argonauts_core::events::plan::TaskGraph;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{ChoreAssignment, ReviewAssignment, RoutingAlgorithm};

use argonauts_learning::routing::{Draw, RunSoFar};

use super::ring;
use super::router::{self, Candidate, CardToRoute, Choice};
use crate::roles;

pub struct Crew<'a> {
    pub settings: &'a Settings,
    /// The run's risk labels, which may rule out exploring.
    pub labels: &'a [String],
    /// The run's task type from triage, for build cards.
    pub run_task_type: Option<&'a str>,
    /// The owner candidates for a task type, with records, health, quota, and cost.
    pub owners: &'a dyn Fn(&str) -> Vec<Candidate>,
    pub family: &'a dyn Fn(&WorkerRef) -> String,
    /// `limits.protected_paths`, so a card that touches them is never explored on.
    pub protected: &'a [String],
}

/// One owner pick, for `Divide::Owner`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub card: CardId,
    pub algorithm: RoutingAlgorithm,
    pub choice: Choice,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub cards: Vec<CardSpec>,
    pub picks: Vec<Pick>,
    /// The router's draws on learning's records, one per card it routed that way.
    pub draws: Vec<Draw>,
}

/// The key for track records and budgets from history.
pub fn task_type(kind: &CardKind, run_task_type: Option<&str>) -> String {
    let fixed = match kind {
        CardKind::Build | CardKind::Solo => return run_task_type.unwrap_or("build").to_owned(),
        CardKind::Interface => "interface",
        CardKind::AcceptanceTests { .. } => "acceptance_tests",
        CardKind::Chore => "chore",
        CardKind::Docs => "docs",
        CardKind::Spike { .. } => "spike",
    };
    fixed.to_owned()
}

pub fn assign(mut cards: Vec<CardSpec>, crew: &Crew, rng: &mut StdRng) -> Assignment {
    let settings = crew.settings;
    let mut load: BTreeMap<WorkerRef, u32> = BTreeMap::new();
    let mut picks = Vec::new();
    let mut routed = 0;
    let mut chores = 0;
    let mut single: Option<Choice> = None;
    let mut draws = Vec::new();
    let mut so_far = RunSoFar::default();

    for card in cards.iter_mut().filter(|card| !is_test(card)) {
        let kind = task_type(&card.kind, crew.run_task_type);
        let mut candidates = (crew.owners)(&kind);
        for candidate in &mut candidates {
            candidate.running_cards += load.get(&candidate.worker).copied().unwrap_or(0);
        }
        let rotating = matches!(card.kind, CardKind::Chore | CardKind::Docs)
            && settings.divide.chores == ChoreAssignment::Rotate;
        let (algorithm, choice) = if settings.divide.single_owner && single.is_some() {
            let choice = single.clone().map(|mut choice| {
                choice.explored = false;
                choice.reason = "one owner builds the whole plan".to_owned();
                choice
            });
            (settings.routing.algorithm, choice)
        } else if rotating && !settings.divide.single_owner {
            let choice = route(RoutingAlgorithm::RoundRobin, &candidates, crew, rng, chores).map(
                |mut choice| {
                    choice.reason = format!("rotating chore, turn {}", chores + 1);
                    choice
                },
            );
            chores += 1;
            (RoutingAlgorithm::RoundRobin, choice)
        } else {
            let algorithm = settings.routing.algorithm;
            let to_route = CardToRoute {
                card: &card.id,
                task_type: &kind,
                labels: crew.labels,
                touches_protected: touches(&card.owned_paths, crew.protected),
            };
            let choice = router::route_card(
                algorithm,
                &candidates,
                &to_route,
                &settings.routing,
                rng,
                routed,
                &mut so_far,
            )
            .map(|(choice, draw)| {
                draws.extend(draw);
                choice
            });
            routed += 1;
            if settings.divide.single_owner {
                single.clone_from(&choice);
            }
            (algorithm, choice)
        };
        if let Some(choice) = choice {
            card.owner = Some(choice.worker.clone());
            *load.entry(choice.worker.clone()).or_default() += 1;
            picks.push(Pick {
                card: card.id.clone(),
                algorithm,
                choice,
            });
        }
    }

    let pool = roles::candidates(settings, RoleName::Reviewer);
    let mut offset = match settings.review.assign {
        ReviewAssignment::Random => rng.random_range(0..pool.len().max(1)),
        _ => 0,
    };
    for card in cards.iter_mut().filter(|card| !is_test(card)) {
        if let Some(owner) = card.owner.clone() {
            card.reviewers = reviewers_for(&owner, None, &pool, crew, offset);
            if settings.review.assign != ReviewAssignment::Fixed {
                offset += 1;
            }
        }
    }

    let tested: BTreeMap<CardId, (Option<WorkerRef>, Vec<WorkerRef>)> = cards
        .iter()
        .map(|card| {
            (
                card.id.clone(),
                (card.owner.clone(), card.reviewers.clone()),
            )
        })
        .collect();
    for card in cards.iter_mut() {
        let CardKind::AcceptanceTests { for_card } = &card.kind else {
            continue;
        };
        let (build_owner, build_reviewers) = tested.get(for_card).cloned().unwrap_or_default();
        let choice = match build_reviewers.first() {
            Some(reviewer) => Some(Choice {
                worker: reviewer.clone(),
                explored: false,
                reason: format!("{for_card}'s reviewer writes its acceptance tests"),
            }),
            None => {
                let candidates: Vec<Candidate> = (crew.owners)("acceptance_tests")
                    .into_iter()
                    .filter(|candidate| {
                        build_owner.as_ref().is_none_or(|owner| {
                            roles::conflict(
                                RoleName::Reviewer,
                                &candidate.worker,
                                Some(owner),
                                crew.family,
                            )
                            .is_none()
                        })
                    })
                    .collect();
                route(settings.routing.algorithm, &candidates, crew, rng, routed)
            }
        };
        if let Some(choice) = choice {
            card.owner = Some(choice.worker.clone());
            card.reviewers =
                reviewers_for(&choice.worker, build_owner.as_ref(), &pool, crew, offset);
            offset += 1;
            picks.push(Pick {
                card: card.id.clone(),
                algorithm: settings.routing.algorithm,
                choice,
            });
        }
    }
    Assignment {
        cards,
        picks,
        draws,
    }
}

/// Whether any of a card's paths shares a prefix with a protected pattern, before either's
/// first wildcard. Cheap and on the safe side: a near miss only means the card isn't explored.
pub fn touches(owned: &[String], protected: &[String]) -> bool {
    let literal = |pattern: &str| -> String {
        pattern
            .split(['*', '?', '[', '{'])
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    owned.iter().any(|path| {
        let path = literal(path);
        protected.iter().any(|pattern| {
            let pattern = literal(pattern);
            pattern.is_empty() || path.starts_with(&pattern) || pattern.starts_with(&path)
        })
    })
}

/// What the plan checkpoint should flag: cards without an owner, and cards with fewer
/// reviewers than `review.per_card`.
pub fn shortfalls(graph: &TaskGraph, settings: &Settings) -> Vec<String> {
    let per_card = settings.review.per_card as usize;
    let mut flags = Vec::new();
    for card in &graph.cards {
        if card.owner.is_none() {
            flags.push(format!(
                "No agent is up and under its quota to own {} (\"{}\").",
                card.id, card.title
            ));
        } else if card.reviewers.len() < per_card {
            let families = if settings.review.other_family {
                " from another model family"
            } else {
                ""
            };
            flags.push(format!(
                "{} has {} of {per_card} reviewers{families}.",
                card.id,
                card.reviewers.len()
            ));
        }
    }
    flags
}

fn route(
    algorithm: RoutingAlgorithm,
    candidates: &[Candidate],
    crew: &Crew,
    rng: &mut StdRng,
    turn: usize,
) -> Option<Choice> {
    router::route(
        algorithm,
        candidates,
        crew.labels,
        &crew.settings.routing,
        rng,
        turn,
    )
}

/// The ring around `owner`, never `owner` itself or `also_not` (a test card's tested owner).
fn reviewers_for(
    owner: &WorkerRef,
    also_not: Option<&WorkerRef>,
    pool: &[WorkerRef],
    crew: &Crew,
    offset: usize,
) -> Vec<WorkerRef> {
    let clear = |worker: &WorkerRef, of: Option<&WorkerRef>| {
        roles::conflict(RoleName::Reviewer, worker, of, crew.family).is_none()
    };
    let candidates: Vec<(WorkerRef, String)> = pool
        .iter()
        .filter(|worker| clear(worker, Some(owner)) && clear(worker, also_not))
        .map(|worker| (worker.clone(), (crew.family)(worker)))
        .collect();
    let review = &crew.settings.review;
    ring::reviewers(
        &(crew.family)(owner),
        &candidates,
        review.per_card as usize,
        review.other_family,
        offset,
    )
}

fn is_test(card: &CardSpec) -> bool {
    matches!(card.kind, CardKind::AcceptanceTests { .. })
}

#[cfg(test)]
mod tests {
    use argonauts_core::limits::{CostEstimate, HealthState, TrackRecord};
    use argonauts_core::steps::plan::{CardProposal, ProposedKind, TaskGraphProposal};
    use rand::SeedableRng;

    use super::*;
    use crate::divide::{contracts, graph};

    fn worker(text: &str) -> WorkerRef {
        text.parse().unwrap()
    }

    fn family(worker: &WorkerRef) -> String {
        match worker.worker.as_str() {
            "claude-code" => "claude",
            "codex" => "gpt",
            _ => "gemini",
        }
        .to_owned()
    }

    fn settings() -> Settings {
        let mut settings = Settings {
            crew: vec![
                worker("claude-code/opus"),
                worker("codex/gpt-5.6"),
                worker("agy/gemini-3.6-pro"),
            ],
            ..Settings::default()
        };
        settings.routing.explore_rate = 0.0;
        settings
    }

    fn cards() -> Vec<CardSpec> {
        let card = |key: &str, kind, owned: &str| CardProposal {
            key: key.to_owned(),
            title: key.to_owned(),
            task: key.to_owned(),
            kind,
            owned_paths: vec![owned.to_owned()],
            read_only_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: vec!["G1".to_owned()],
        };
        let proposal = TaskGraphProposal {
            cards: vec![
                card("api", ProposedKind::Interface, "src/api.rs"),
                card("store", ProposedKind::Build, "src/store/**"),
                card("middleware", ProposedKind::Build, "src/mw/**"),
                card("docs", ProposedKind::Docs, "docs/a.md"),
                card("cleanup", ProposedKind::Chore, "scripts/**"),
            ],
        };
        contracts::contracts_first(graph::convert(&proposal).0, true)
    }

    fn assigned(settings: &Settings, seed: u64) -> Assignment {
        let owners = |_: &str| -> Vec<Candidate> {
            settings
                .crew
                .iter()
                .map(|worker| Candidate {
                    worker: worker.clone(),
                    record: Some(TrackRecord { passed: 8, of: 10 }),
                    health: HealthState::Healthy,
                    quota_used: None,
                    cost: CostEstimate::Unknown,
                    running_cards: 0,
                    learned: None,
                })
                .collect()
        };
        let crew = Crew {
            settings,
            labels: &[],
            run_task_type: Some("feature"),
            owners: &owners,
            family: &family,
            protected: &[],
        };
        assign(cards(), &crew, &mut StdRng::seed_from_u64(seed))
    }

    #[test]
    fn every_card_gets_an_owner_and_a_ring_from_other_families() {
        let settings = settings();
        let assignment = assigned(&settings, 42);
        assert_eq!(assignment, assigned(&settings, 42));
        assert_eq!(assignment.picks.len(), assignment.cards.len());
        let by_id: BTreeMap<&CardId, &CardSpec> = assignment
            .cards
            .iter()
            .map(|card| (&card.id, card))
            .collect();
        for card in &assignment.cards {
            let owner = card.owner.as_ref().unwrap();
            assert_eq!(card.reviewers.len(), 1, "{}", card.id);
            for reviewer in &card.reviewers {
                assert_ne!(family(reviewer), family(owner), "{}", card.id);
            }
            if let CardKind::AcceptanceTests { for_card } = &card.kind {
                let tested = by_id[for_card];
                assert_eq!(Some(owner), tested.reviewers.first());
                assert_ne!(Some(owner), tested.owner.as_ref());
                assert!(!card.reviewers.contains(tested.owner.as_ref().unwrap()));
            }
        }
        let docs = &assignment
            .picks
            .iter()
            .find(|p| p.card.as_str() == "card-004")
            .unwrap();
        assert_eq!(docs.algorithm, RoutingAlgorithm::RoundRobin);
        assert_eq!(docs.choice.reason, "rotating chore, turn 1");
        let graph = TaskGraph {
            cards: assignment.cards,
            critical_path: Vec::new(),
        };
        assert!(shortfalls(&graph, &settings).is_empty());
    }

    #[test]
    fn one_owner_for_the_whole_plan() {
        let mut settings = settings();
        settings.divide.single_owner = true;
        let assignment = assigned(&settings, 3);
        let owners: Vec<&WorkerRef> = assignment
            .cards
            .iter()
            .filter(|card| !is_test(card))
            .filter_map(|card| card.owner.as_ref())
            .collect();
        assert!(owners.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn a_one_family_crew_is_flagged() {
        let mut settings = settings();
        settings.crew = vec![worker("claude-code/opus"), worker("claude-code/sonnet")];
        let assignment = assigned(&settings, 1);
        let graph = TaskGraph {
            cards: assignment.cards,
            critical_path: Vec::new(),
        };
        let flags = shortfalls(&graph, &settings);
        assert!(
            flags
                .iter()
                .any(|flag| flag.contains("0 of 1 reviewers from another model family"))
        );
    }
}
