//! The coordinated crew: the coordinator runs `Coordinate` (plan, cards, owners, reviewers) in a
//! run-lifetime session. Code checks it against the rules (review ring families, owned paths,
//! budget, roles allowed, the task graph); a broken rule is rejected with the reason and retried
//! once, then the plan checkpoint asks you. The coordinator can't raise a budget or approve
//! anything. Dividing the work takes its cards and assignments as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, CardSpec};
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind};
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{RoutingAlgorithm, TestAuthor};
use argonauts_core::steps::plan::{
    Assignment, CardProposal, Coordinate, CoordinateInput, CoordinateOutput, TaskGraphProposal,
};
use argonauts_core::worker::Access;
use argonauts_core::write_private_file;

use crate::divide::assign::Pick;
use crate::divide::router::Choice;
use crate::divide::{self, ring};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{self, crew_for, families};
use crate::plan::drafts;
use crate::prompts::plan::COORDINATE;
use crate::roles;
use crate::state::RunState;
use crate::state::plan::{self, CoordinatedRecord, SealedDraft};
use crate::workflow::{StepCall, StepDone};

/// A rejected answer is retried once; the second rejection goes to you.
const ATTEMPTS: usize = 2;

/// The coordinator's cards and assignments, sealed beside its plan for dividing the work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoordinatedCards {
    pub cards: Vec<CardProposal>,
    pub assignments: Vec<Assignment>,
}

/// The coordinator's plan broke the rules on every try since you last sent it back.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    pub coordinator: WorkerRef,
    /// Why each try was rejected, oldest first.
    pub reasons: Vec<String>,
}

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    context::phase_started(host)?;
    let coordinator = context::workers_for(call, host.settings(), RoleName::Coordinator, Some(1))
        .into_iter()
        .next()
        .unwrap_or_else(|| host.settings().default_agent.clone());
    loop {
        // A plan you sent back gets two new tries, in the coordinator's own session, with your
        // note. A settled plan sent back starts a new planning round.
        let (records, sent_back, replanning, asked, session) = host.read(|state| {
            (
                attempts(state),
                sent_back(state),
                plan::sent_back(state).is_some(),
                state.plan.coordinated.len(),
                state
                    .plan
                    .coordinated
                    .iter()
                    .rev()
                    .find_map(|r| r.session.clone()),
            )
        });
        if records.iter().any(|record| record.rejection.is_none()) {
            return Ok(StepDone::done("The coordinator's plan follows the rules"));
        }
        if records.len() >= ATTEMPTS {
            let summary =
                "The coordinator's plan broke the rules twice; the plan checkpoint asks you";
            context::phase_finished(host, summary)?;
            return Ok(StepDone::done(summary));
        }
        let rejection = match records.last() {
            Some(record) => record.rejection.clone(),
            None => sent_back.map(|sent| match sent.note {
                Some(note) => format!("You sent the plan back with this note: {note}"),
                None => "You sent the plan back.".to_owned(),
            }),
        };
        let input = host.read(|state| CoordinateInput {
            goal: context::goal_brief(state),
            crew: crew_lines(host),
            rules: rules(host.settings()),
            rejection,
        });
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Coordinator,
            coordinator.clone(),
            COORDINATE,
            input,
        );
        ask.access = Access::ReadOnly;
        // Counted over the whole run, so an answer after a send-back never reuses a slot.
        ask.slot = asked as u32;
        ask.resume = session;
        ask.parts
            .push(context::status_part(host, RoleName::Coordinator, None));
        let answered = match host.ask::<Coordinate>(ask).await {
            Ok(answered) => answered,
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => {
                reject(
                    host,
                    &coordinator,
                    None,
                    format!("the answer failed: {error}"),
                )?;
                continue;
            }
        };
        let mut broken = check(&answered.output, host.settings(), &families(host));
        if broken.is_empty() {
            broken = graph_problems(host, &answered.output.cards);
        }
        if !broken.is_empty() {
            reject(
                host,
                &coordinator,
                Some(&answered.session),
                broken.join("; "),
            )?;
            continue;
        }
        let round = host.read(|state| state.plan.blind_round(replanning));
        let draft = drafts::seal(
            host,
            1,
            &coordinator,
            round,
            &answered.output.plan,
            Some(&answered.session),
        )?;
        let sealed = host
            .read(|state| state.plan.drafts.iter().find(|d| d.draft == draft).cloned())
            .ok_or_else(|| EngineError::Refused(format!("draft {draft} was not recorded")))?;
        let cards = CoordinatedCards {
            cards: answered.output.cards.clone(),
            assignments: answered.output.assignments.clone(),
        };
        write_private_file(
            &cards_path(&sealed.path),
            &serde_json::to_vec_pretty(&cards)?,
        )?;
        host.record(
            None,
            Some(&answered.session),
            RunEventKind::Plan(PlanEvent::Coordinated {
                coordinator: coordinator.clone(),
                rejection: None,
            }),
        )?;
        let summary = format!(
            "The coordinator planned {} card{}",
            cards.cards.len(),
            if cards.cards.len() == 1 { "" } else { "s" }
        );
        context::phase_finished(host, &summary)?;
        return Ok(StepDone::done(summary));
    }
}

/// What the task graph validator and contracts first say about the cards, which dividing the
/// work would otherwise find only after the coordinator is done.
fn graph_problems(host: &Host, cards: &[CardProposal]) -> Vec<String> {
    let proposal = TaskGraphProposal {
        cards: cards.to_vec(),
    };
    divide::build(host.run_id(), &proposal, host.settings())
        .err()
        .unwrap_or_default()
}

struct SentBack {
    /// The seq of the plan checkpoint that was answered.
    since: u64,
    note: Option<String>,
}

/// The last time you sent the plan back at the plan checkpoint.
fn sent_back(state: &RunState) -> Option<SentBack> {
    state
        .checkpoints
        .by_id
        .values()
        .filter(|record| record.opened.kind == CheckpointKind::Plan && record.withdrawn.is_none())
        .filter_map(|record| {
            let decided = record.decided.as_ref()?;
            (decided.action == AnswerAction::SendBack).then(|| SentBack {
                since: record.since,
                note: decided
                    .text
                    .as_deref()
                    .map(str::trim)
                    .filter(|note| !note.is_empty())
                    .map(str::to_owned),
            })
        })
        .max_by_key(|sent| sent.since)
}

/// The coordinator's answers since you last sent its plan back.
fn attempts(state: &RunState) -> Vec<CoordinatedRecord> {
    let since = sent_back(state).map(|sent| sent.since);
    state
        .plan
        .coordinated
        .iter()
        .filter(|record| since.is_none_or(|since| record.seq > since))
        .cloned()
        .collect()
}

/// Why the coordinator's plan was rejected, once it has used every try since you last sent it
/// back without an answer that stands.
pub fn rejected(state: &RunState) -> Option<Rejected> {
    let attempts = attempts(state);
    if attempts.len() < ATTEMPTS || attempts.iter().any(|record| record.rejection.is_none()) {
        return None;
    }
    Some(Rejected {
        coordinator: attempts.last()?.coordinator.clone(),
        reasons: attempts
            .into_iter()
            .filter_map(|record| record.rejection)
            .collect(),
    })
}

fn reject(
    host: &Host,
    coordinator: &WorkerRef,
    session: Option<&SessionKey>,
    reason: String,
) -> EngineResult<()> {
    host.record(
        None,
        session,
        RunEventKind::Plan(PlanEvent::Coordinated {
            coordinator: coordinator.clone(),
            rejection: Some(reason),
        }),
    )?;
    Ok(())
}

fn cards_path(plan: &Path) -> PathBuf {
    plan.with_extension("cards.json")
}

/// The accepted coordinator's cards and assignments, for dividing the work.
pub fn load_cards(sealed: &SealedDraft) -> EngineResult<CoordinatedCards> {
    let bytes = std::fs::read(cards_path(&sealed.path))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Puts the coordinator's owners and reviewers on the cards built from its proposal, where the
/// proposal's n-th card became `card-00n`. Each acceptance-test card goes to its build card's
/// first reviewer, with a review ring of its own. A card left without an owner is the router's.
pub fn delegate(
    cards: &mut [CardSpec],
    delegated: &CoordinatedCards,
    settings: &Settings,
    family: &dyn Fn(&WorkerRef) -> String,
) -> Vec<Pick> {
    let ids: BTreeMap<&str, CardId> = delegated
        .cards
        .iter()
        .enumerate()
        .map(|(n, card)| (card.key.trim(), CardId::numbered(n as u32 + 1)))
        .collect();
    let mut picks = Vec::new();
    for assignment in &delegated.assignments {
        let Some(id) = ids.get(assignment.card.trim()) else {
            continue;
        };
        let (Some(card), Ok(owner)) = (
            cards.iter_mut().find(|card| &card.id == id),
            assignment.owner.parse::<WorkerRef>(),
        ) else {
            continue;
        };
        card.owner = Some(owner.clone());
        card.reviewers = assignment
            .reviewers
            .iter()
            .filter_map(|reviewer| reviewer.parse().ok())
            .collect();
        picks.push(Pick {
            card: card.id.clone(),
            algorithm: RoutingAlgorithm::Coordinator,
            choice: Choice {
                worker: owner,
                explored: false,
                reason: "the coordinator's assignment".to_owned(),
            },
        });
    }

    let built: BTreeMap<CardId, (WorkerRef, Vec<WorkerRef>)> = cards
        .iter()
        .filter_map(|card| {
            Some((
                card.id.clone(),
                (card.owner.clone()?, card.reviewers.clone()),
            ))
        })
        .collect();
    let pool = roles::candidates(settings, RoleName::Reviewer);
    for (offset, card) in cards.iter_mut().enumerate() {
        let CardKind::AcceptanceTests { for_card } = &card.kind else {
            continue;
        };
        let Some((owner, reviewers)) = built.get(for_card) else {
            continue;
        };
        let Some(writer) = reviewers.first() else {
            continue;
        };
        card.owner = Some(writer.clone());
        card.reviewers = test_reviewers(writer, owner, &pool, settings, family, offset);
        picks.push(Pick {
            card: card.id.clone(),
            algorithm: RoutingAlgorithm::Coordinator,
            choice: Choice {
                worker: writer.clone(),
                explored: false,
                reason: format!("{for_card}'s reviewer writes its acceptance tests"),
            },
        });
    }
    picks
}

/// The ring around a test card's writer, never the writer or the tested card's owner.
fn test_reviewers(
    writer: &WorkerRef,
    tested_owner: &WorkerRef,
    pool: &[WorkerRef],
    settings: &Settings,
    family: &dyn Fn(&WorkerRef) -> String,
    offset: usize,
) -> Vec<WorkerRef> {
    let clear = |worker: &WorkerRef, of: &WorkerRef| {
        roles::conflict(RoleName::Reviewer, worker, Some(of), family).is_none()
    };
    let candidates: Vec<(WorkerRef, String)> = pool
        .iter()
        .filter(|worker| clear(worker, writer) && clear(worker, tested_owner))
        .map(|worker| (worker.clone(), family(worker)))
        .collect();
    ring::reviewers(
        &family(writer),
        &candidates,
        settings.review.per_card as usize,
        settings.review.other_family,
        offset,
    )
}

/// `worker/model (family)` for everyone the coordinator may assign.
fn crew_lines(host: &Host) -> Vec<String> {
    let settings = host.settings();
    let mut workers = crew_for(settings, RoleName::Owner);
    workers.extend(crew_for(settings, RoleName::Reviewer));
    let mut seen = BTreeSet::new();
    workers
        .into_iter()
        .filter(|worker| seen.insert(worker.clone()))
        .map(|worker| format!("{worker} ({})", crate::roles::family(host, &worker)))
        .collect()
}

/// The rules code checks the coordinator's output against, as the coordinator reads them.
pub fn rules(settings: &Settings) -> Vec<String> {
    let mut rules = vec![
        format!(
            "At most {} cards, each with a unique key.",
            settings.divide.max_cards
        ),
        "A card depends only on other cards' keys, with no cycles.".to_owned(),
        "No two cards own the same path, and every card owns at least one path.".to_owned(),
        format!(
            "No card owns a protected path: .argonauts/**, {}.",
            settings.limits.protected_paths.join(", ")
        ),
        "Every card has exactly one assignment, with an owner from the crew list.".to_owned(),
        format!(
            "Every card has at least {} reviewers from the crew list, never its owner{}.",
            settings.review.per_card,
            if settings.review.other_family {
                ", each from another model family than the owner"
            } else {
                ""
            }
        ),
        "You can't raise a budget or approve anything.".to_owned(),
    ];
    if settings.divide.contracts_first {
        rules.push(
            "Exactly one card has kind interface: the stubs every build card codes against."
                .to_owned(),
        );
        if settings.divide.acceptance_tests_by == TestAuthor::Reviewer {
            rules.push(
                "Code adds an acceptance-test card for each build card, written by its first \
                 reviewer."
                    .to_owned(),
            );
        }
    }
    if let Some(limit) = card_limit(settings) {
        rules.push(format!(
            "Each card gets ${:.2} of the run's ${:.2}, so at most {limit} cards fit.",
            settings.budget.per_card_usd.unwrap_or_default(),
            settings.budget.run_usd.unwrap_or_default(),
        ));
    }
    rules
}

/// How many cards the run budget holds at the per-card budget, when both are in dollars.
fn card_limit(settings: &Settings) -> Option<usize> {
    let budget = &settings.budget;
    match (budget.enabled, budget.run_usd, budget.per_card_usd) {
        (true, Some(run), Some(card)) if card > 0.0 => Some((run / card).floor() as usize),
        _ => None,
    }
}

/// Every rule the output breaks, one line each. Empty means it stands. `families` gives a
/// worker's model family from the registry.
pub fn check(
    output: &CoordinateOutput,
    settings: &Settings,
    families: &dyn Fn(&WorkerRef) -> String,
) -> Vec<String> {
    let mut broken = Vec::new();
    check_cards(&output.cards, settings, &mut broken);
    check_paths(&output.cards, settings, &mut broken);
    check_assignments(output, settings, families, &mut broken);
    broken
}

fn check_cards(cards: &[CardProposal], settings: &Settings, broken: &mut Vec<String>) {
    if cards.is_empty() {
        broken.push("The plan has no cards.".to_owned());
    }
    if cards.len() > settings.divide.max_cards as usize {
        broken.push(format!(
            "{} cards is more than the {} allowed.",
            cards.len(),
            settings.divide.max_cards
        ));
    }
    if let Some(limit) = card_limit(settings)
        && cards.len() > limit
    {
        broken.push(format!(
            "{} cards don't fit the run budget, which holds {limit}.",
            cards.len()
        ));
    }
    let mut keys = BTreeSet::new();
    for card in cards {
        if card.key.trim().is_empty() {
            broken.push(format!("Card \"{}\" has an empty key.", card.title));
        } else if !keys.insert(card.key.as_str()) {
            broken.push(format!("The key {} is used twice.", card.key));
        }
    }
    for card in cards {
        for dep in &card.depends_on {
            if dep == &card.key {
                broken.push(format!("Card {} depends on itself.", card.key));
            } else if !keys.contains(dep.as_str()) {
                broken.push(format!("Card {} depends on unknown card {dep}.", card.key));
            }
        }
    }
    if let Some(key) = first_cycle(cards) {
        broken.push(format!("Card {key} is part of a dependency cycle."));
    }
}

fn first_cycle(cards: &[CardProposal]) -> Option<String> {
    let deps: BTreeMap<&str, Vec<&str>> = cards
        .iter()
        .map(|c| {
            (
                c.key.as_str(),
                c.depends_on.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    fn visit<'a>(
        key: &'a str,
        deps: &BTreeMap<&'a str, Vec<&'a str>>,
        path: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<String> {
        if path.contains(&key) {
            return Some(key.to_owned());
        }
        if !done.insert(key) {
            return None;
        }
        path.push(key);
        for dep in deps.get(key).into_iter().flatten() {
            if *dep != key
                && deps.contains_key(dep)
                && let Some(cycle) = visit(dep, deps, path, done)
            {
                return Some(cycle);
            }
        }
        path.pop();
        None
    }
    let mut done = BTreeSet::new();
    deps.keys()
        .find_map(|key| visit(key, &deps, &mut Vec::new(), &mut done))
}

fn check_paths(cards: &[CardProposal], settings: &Settings, broken: &mut Vec<String>) {
    let protected = protected_set(settings);
    let mut owners: Vec<(&str, String)> = Vec::new();
    for card in cards {
        if card.owned_paths.is_empty() && cards.len() > 1 {
            broken.push(format!("Card {} owns no path.", card.key));
        }
        for path in &card.owned_paths {
            let base = path_base(path);
            let probe = format!("{base}/file");
            if base.is_empty() || base.starts_with('/') || base.split('/').any(|c| c == "..") {
                broken.push(format!("Card {} owns an invalid path {path}.", card.key));
                continue;
            }
            if protected.is_match(&base) || protected.is_match(&probe) {
                broken.push(format!("Card {} owns protected path {path}.", card.key));
            }
            for (other, other_base) in &owners {
                if *other != card.key.as_str() && overlaps(&base, other_base) {
                    broken.push(format!("Cards {other} and {} both own {path}.", card.key));
                }
            }
        }
        for path in &card.owned_paths {
            owners.push((card.key.as_str(), path_base(path)));
        }
    }
}

/// The literal part of a path or glob, without `./` or a trailing slash.
fn path_base(path: &str) -> String {
    let path = path.trim().strip_prefix("./").unwrap_or(path.trim());
    let literal = match path.find(['*', '?', '[', '{']) {
        Some(at) => &path[..at],
        None => path,
    };
    literal.trim_end_matches('/').to_owned()
}

fn overlaps(a: &str, b: &str) -> bool {
    let inside =
        |x: &str, dir: &str| dir.is_empty() || x == dir || x.starts_with(&format!("{dir}/"));
    inside(a, b) || inside(b, a)
}

fn protected_set(settings: &Settings) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    let always = [".argonauts/**".to_owned(), ".argonauts".to_owned()];
    for pattern in always.iter().chain(&settings.limits.protected_paths) {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

fn check_assignments(
    output: &CoordinateOutput,
    settings: &Settings,
    families: &dyn Fn(&WorkerRef) -> String,
    broken: &mut Vec<String>,
) {
    let owners = crew_for(settings, RoleName::Owner);
    let reviewers = crew_for(settings, RoleName::Reviewer);
    let keys: BTreeSet<&str> = output.cards.iter().map(|c| c.key.as_str()).collect();
    let mut assigned: BTreeMap<&str, u32> = BTreeMap::new();
    for assignment in &output.assignments {
        let key = assignment.card.as_str();
        if !keys.contains(key) {
            broken.push(format!("An assignment names unknown card {key}."));
            continue;
        }
        *assigned.entry(key).or_default() += 1;
        let owner = match assignment.owner.parse::<WorkerRef>() {
            Ok(owner) if owners.contains(&owner) => owner,
            Ok(owner) => {
                broken.push(format!("{owner} may not own card {key}."));
                owner
            }
            Err(_) => {
                broken.push(format!(
                    "Card {key} has an invalid owner {}.",
                    assignment.owner
                ));
                continue;
            }
        };
        let owner_family = families(&owner);
        let mut seen = BTreeSet::new();
        for text in &assignment.reviewers {
            let Ok(reviewer) = text.parse::<WorkerRef>() else {
                broken.push(format!("Card {key} has an invalid reviewer {text}."));
                continue;
            };
            if !seen.insert(reviewer.clone()) {
                broken.push(format!("{reviewer} reviews card {key} twice."));
            } else if reviewer == owner {
                broken.push(format!("{reviewer} would review its own card {key}."));
            } else if !reviewers.contains(&reviewer) {
                broken.push(format!("{reviewer} may not review card {key}."));
            } else if settings.review.other_family && families(&reviewer) == owner_family {
                broken.push(format!(
                    "{reviewer} is of the owner's family {owner_family} on card {key}."
                ));
            }
        }
        if (seen.len() as u32) < settings.review.per_card {
            broken.push(format!(
                "Card {key} has {} reviewers; it needs {}.",
                seen.len(),
                settings.review.per_card
            ));
        }
    }
    for key in &keys {
        match assigned.get(key).copied().unwrap_or(0) {
            0 => broken.push(format!("Card {key} has no assignment.")),
            1 => {}
            n => broken.push(format!("Card {key} has {n} assignments.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::plan::{PlanDraft, ProposedKind};

    use super::*;

    fn family(worker: &WorkerRef) -> String {
        match worker.worker.as_str() {
            "claude-code" => "claude",
            "codex" | "opencode" => "gpt",
            _ => "gemini",
        }
        .to_owned()
    }

    fn settings() -> Settings {
        let mut settings = Settings {
            crew: ["claude-code", "codex", "antigravity-cli", "opencode"]
                .iter()
                .map(|w| w.parse().unwrap())
                .collect(),
            ..Settings::default()
        };
        settings.review.per_card = 2;
        settings
    }

    fn card(key: &str, paths: &[&str], deps: &[&str]) -> CardProposal {
        CardProposal {
            key: key.to_owned(),
            title: key.to_owned(),
            task: format!("build {key}"),
            kind: ProposedKind::Build,
            owned_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            read_only_paths: Vec::new(),
            depends_on: deps.iter().map(|d| (*d).to_owned()).collect(),
            criteria: vec!["G1".to_owned()],
        }
    }

    fn assign(key: &str, owner: &str, reviewers: &[&str]) -> Assignment {
        Assignment {
            card: key.to_owned(),
            owner: owner.to_owned(),
            reviewers: reviewers.iter().map(|r| (*r).to_owned()).collect(),
        }
    }

    fn plan() -> PlanDraft {
        PlanDraft {
            summary: "two cards".to_owned(),
            approach: String::new(),
            pieces: Vec::new(),
            choices: Vec::new(),
            risks: Vec::new(),
            test_strategy: String::new(),
            new_dependencies: Vec::new(),
        }
    }

    fn good() -> CoordinateOutput {
        CoordinateOutput {
            plan: plan(),
            cards: vec![
                card("store", &["src/store/"], &[]),
                card("api", &["src/api.rs"], &["store"]),
            ],
            assignments: vec![
                assign("store", "claude-code", &["codex", "antigravity-cli"]),
                assign("api", "codex", &["claude-code", "antigravity-cli"]),
            ],
        }
    }

    fn broken(output: &CoordinateOutput) -> Vec<String> {
        check(output, &settings(), &family)
    }

    #[test]
    fn a_plan_that_keeps_the_rules_stands() {
        assert_eq!(broken(&good()), Vec::<String>::new());
    }

    #[test]
    fn reviewers_of_the_owners_family_or_the_owner_are_rejected() {
        let mut output = good();
        output.assignments[1] = assign("api", "codex", &["opencode", "codex"]);
        let broken = broken(&output);
        assert!(
            broken
                .iter()
                .any(|b| b.contains("opencode is of the owner's family gpt"))
        );
        assert!(
            broken
                .iter()
                .any(|b| b.contains("codex would review its own card api"))
        );
    }

    #[test]
    fn overlapping_and_protected_paths_are_rejected() {
        let mut output = good();
        output.cards[1].owned_paths = vec!["src/store/redis.rs".to_owned(), ".github/".to_owned()];
        let broken = broken(&output);
        assert!(
            broken
                .iter()
                .any(|b| b.contains("both own src/store/redis.rs")),
            "{broken:?}"
        );
        assert!(
            broken.iter().any(|b| b.contains("protected path .github/")),
            "{broken:?}"
        );
    }

    #[test]
    fn graph_errors_are_rejected() {
        let mut output = good();
        output.cards[0].depends_on = vec!["api".to_owned()];
        output.cards.push(card("web", &["src/web.rs"], &["ghost"]));
        output.cards.push(card("web", &["src/web2.rs"], &[]));
        let broken = broken(&output);
        assert!(broken.iter().any(|b| b.contains("key web is used twice")));
        assert!(broken.iter().any(|b| b.contains("unknown card ghost")));
        assert!(broken.iter().any(|b| b.contains("dependency cycle")));
    }

    #[test]
    fn owners_outside_the_crew_and_missing_assignments_are_rejected() {
        let mut output = good();
        output.assignments[0] = assign("store", "stranger", &["codex", "antigravity-cli"]);
        output.assignments.pop();
        let broken = broken(&output);
        assert!(
            broken
                .iter()
                .any(|b| b.contains("stranger may not own card store"))
        );
        assert!(
            broken
                .iter()
                .any(|b| b.contains("Card api has no assignment"))
        );
    }

    #[test]
    fn too_many_cards_for_the_budget_are_rejected() {
        let mut output = good();
        for n in 0..6 {
            let key = format!("extra{n}");
            output
                .cards
                .push(card(&key, &[&format!("src/extra{n}.rs")], &[]));
            output
                .assignments
                .push(assign(&key, "claude-code", &["codex", "antigravity-cli"]));
        }
        let broken = broken(&output);
        assert_eq!(broken, ["8 cards don't fit the run budget, which holds 6."]);
        assert!(
            rules(&settings())
                .iter()
                .any(|r| r.contains("at most 6 cards fit"))
        );
    }

    #[test]
    fn too_few_reviewers_are_rejected() {
        let mut output = good();
        output.assignments[0].reviewers.pop();
        assert_eq!(broken(&output), ["Card store has 1 reviewers; it needs 2."]);
    }
}
