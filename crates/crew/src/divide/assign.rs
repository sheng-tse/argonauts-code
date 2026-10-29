//! Workers, the review ring, and chores for a validated graph. What it knows about each worker
//! comes in through [`Crew`], so the whole assignment is plain code over its inputs.

use std::collections::BTreeMap;

use rand::RngExt;
use rand::rngs::StdRng;

use argonauts_core::TaskId;
use argonauts_core::events::plan::TaskGraph;
use argonauts_core::events::task::{TaskKind, TaskSpec};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{ChoreAssignment, ReviewAssignment, RoutingAlgorithm};

use argonauts_learning::routing::{Draw, RunSoFar};

use super::ring;
use super::router::{self, Candidate, Choice, TaskToRoute};
use crate::roles;

pub struct Crew<'a> {
    pub settings: &'a Settings,
    /// The run's risk labels, which may rule out exploring.
    pub labels: &'a [String],
    /// The run's task type from its estimate, for build tasks.
    pub run_task_type: Option<&'a str>,
    /// The worker candidates for a task type, with records, health, plan usage, and cost.
    pub workers: &'a dyn Fn(&str) -> Vec<Candidate>,
    pub provider: &'a dyn Fn(&ModelRef) -> String,
    /// `limits.protected_paths`, so a task that touches them is never explored on.
    pub protected: &'a [String],
}

/// One worker pick, for `Divide::Worker`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub task: TaskId,
    pub algorithm: RoutingAlgorithm,
    pub choice: Choice,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub tasks: Vec<TaskSpec>,
    pub picks: Vec<Pick>,
    /// The router's draws on learning's records, one per task it routed that way.
    pub draws: Vec<Draw>,
}

/// The key for track records and limits from history.
pub fn task_type(kind: &TaskKind, run_task_type: Option<&str>) -> String {
    let fixed = match kind {
        TaskKind::Build | TaskKind::Captain => return run_task_type.unwrap_or("build").to_owned(),
        TaskKind::Interface => "interface",
        TaskKind::AcceptanceTests { .. } => "acceptance_tests",
        TaskKind::Chore => "chore",
        TaskKind::Docs => "docs",
        TaskKind::Experiment { .. } => "experiment",
    };
    fixed.to_owned()
}

pub fn assign(mut tasks: Vec<TaskSpec>, crew: &Crew, rng: &mut StdRng) -> Assignment {
    let settings = crew.settings;
    let mut load: BTreeMap<ModelRef, u32> = BTreeMap::new();
    let mut picks = Vec::new();
    let mut routed = 0;
    let mut chores = 0;
    let mut single: Option<Choice> = None;
    let mut draws = Vec::new();
    let mut so_far = RunSoFar::default();

    for task in tasks.iter_mut().filter(|task| !is_test(task)) {
        let kind = task_type(&task.kind, crew.run_task_type);
        let mut candidates = (crew.workers)(&kind);
        for candidate in &mut candidates {
            candidate.running_tasks += load.get(&candidate.worker).copied().unwrap_or(0);
        }
        let rotating = matches!(task.kind, TaskKind::Chore | TaskKind::Docs)
            && settings.divide.chores == ChoreAssignment::Rotate;
        let (algorithm, choice) = if settings.divide.single_worker && single.is_some() {
            let choice = single.clone().map(|mut choice| {
                choice.explored = false;
                choice.reason = "one worker builds the whole plan".to_owned();
                choice
            });
            (settings.routing.algorithm, choice)
        } else if rotating && !settings.divide.single_worker {
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
            let to_route = TaskToRoute {
                task: &task.id,
                task_type: &kind,
                labels: crew.labels,
                touches_protected: touches(&task.owned_paths, crew.protected),
            };
            // With the orchestrator picking, each task takes the best by its own order.
            let choice = if settings.crew.orchestrator_picks.worker {
                router::pick(&candidates)
            } else {
                router::route_task(
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
                })
            };
            routed += 1;
            if settings.divide.single_worker {
                single.clone_from(&choice);
            }
            (algorithm, choice)
        };
        if let Some(choice) = choice {
            task.worker = Some(choice.worker.clone());
            *load.entry(choice.worker.clone()).or_default() += 1;
            picks.push(Pick {
                task: task.id.clone(),
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
    for task in tasks.iter_mut().filter(|task| !is_test(task)) {
        if let Some(worker) = task.worker.clone() {
            task.reviewers = reviewers_for(&worker, None, &pool, crew, offset);
            if settings.review.assign != ReviewAssignment::Fixed {
                offset += 1;
            }
        }
    }

    let tested: BTreeMap<TaskId, (Option<ModelRef>, Vec<ModelRef>)> = tasks
        .iter()
        .map(|task| {
            (
                task.id.clone(),
                (task.worker.clone(), task.reviewers.clone()),
            )
        })
        .collect();
    for task in tasks.iter_mut() {
        let TaskKind::AcceptanceTests { for_task } = &task.kind else {
            continue;
        };
        let (build_worker, build_reviewers) = tested.get(for_task).cloned().unwrap_or_default();
        let choice = match build_reviewers.first() {
            Some(reviewer) => Some(Choice {
                worker: reviewer.clone(),
                explored: false,
                reason: format!("{for_task}'s reviewer writes its acceptance tests"),
            }),
            None => {
                let candidates: Vec<Candidate> = (crew.workers)("acceptance_tests")
                    .into_iter()
                    .filter(|candidate| {
                        build_worker.as_ref().is_none_or(|worker| {
                            roles::conflict(
                                RoleName::Reviewer,
                                &candidate.worker,
                                Some(worker),
                                crew.provider,
                            )
                            .is_none()
                        })
                    })
                    .collect();
                route(settings.routing.algorithm, &candidates, crew, rng, routed)
            }
        };
        if let Some(choice) = choice {
            task.worker = Some(choice.worker.clone());
            task.reviewers =
                reviewers_for(&choice.worker, build_worker.as_ref(), &pool, crew, offset);
            offset += 1;
            picks.push(Pick {
                task: task.id.clone(),
                algorithm: settings.routing.algorithm,
                choice,
            });
        }
    }
    Assignment {
        tasks,
        picks,
        draws,
    }
}

/// Whether any of a task's paths shares a prefix with a protected pattern, before either's
/// first wildcard. Cheap and on the safe side: a near miss only means the task isn't explored.
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

/// What the plan approval should flag: tasks without a worker, and tasks with fewer
/// reviewers than `review.per_task`.
pub fn shortfalls(graph: &TaskGraph, settings: &Settings) -> Vec<String> {
    let per_task = settings.review.per_task as usize;
    let mut flags = Vec::new();
    for task in &graph.tasks {
        if task.worker.is_none() {
            flags.push(format!(
                "No agent is up with plan usage left to own {} (\"{}\").",
                task.id, task.title
            ));
        } else if task.reviewers.len() < per_task {
            let providers = if settings.review.other_provider {
                " from another provider"
            } else {
                ""
            };
            flags.push(format!(
                "{} has {} of {per_task} reviewers{providers}.",
                task.id,
                task.reviewers.len()
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

/// The ring around `worker`, never `worker` itself or `also_not` (a test task's tested worker).
fn reviewers_for(
    worker: &ModelRef,
    also_not: Option<&ModelRef>,
    pool: &[ModelRef],
    crew: &Crew,
    offset: usize,
) -> Vec<ModelRef> {
    let clear = |model: &ModelRef, of: Option<&ModelRef>| {
        roles::conflict(RoleName::Reviewer, model, of, crew.provider).is_none()
    };
    let candidates: Vec<(ModelRef, String)> = pool
        .iter()
        .filter(|model| clear(model, Some(worker)) && clear(model, also_not))
        .map(|model| (model.clone(), (crew.provider)(model)))
        .collect();
    let review = &crew.settings.review;
    ring::reviewers(
        &(crew.provider)(worker),
        &candidates,
        review.per_task as usize,
        review.other_provider,
        offset,
    )
}

fn is_test(task: &TaskSpec) -> bool {
    matches!(task.kind, TaskKind::AcceptanceTests { .. })
}

#[cfg(test)]
mod tests {
    use argonauts_core::limits::{CostEstimate, HealthState, TrackRecord};
    use argonauts_core::steps::plan::{ProposedKind, TaskGraphProposal, TaskProposal};
    use rand::SeedableRng;

    use super::*;
    use crate::divide::{contracts, graph};

    fn worker(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    fn provider(worker: &ModelRef) -> String {
        match worker.agent.as_str() {
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
            ]
            .into(),
            ..Settings::default()
        };
        settings.routing.explore_rate = 0.0;
        settings
    }

    fn tasks() -> Vec<TaskSpec> {
        let task = |key: &str, kind, owned: &str| TaskProposal {
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
            tasks: vec![
                task("api", ProposedKind::Interface, "src/api.rs"),
                task("store", ProposedKind::Build, "src/store/**"),
                task("middleware", ProposedKind::Build, "src/mw/**"),
                task("docs", ProposedKind::Docs, "docs/a.md"),
                task("cleanup", ProposedKind::Chore, "scripts/**"),
            ],
        };
        contracts::contracts_first(graph::convert(&proposal).0, true)
    }

    fn assigned(settings: &Settings, seed: u64) -> Assignment {
        let workers = |_: &str| -> Vec<Candidate> {
            settings
                .crew
                .members()
                .iter()
                .map(|model| Candidate {
                    worker: model.clone(),
                    record: Some(TrackRecord { passed: 8, of: 10 }),
                    health: HealthState::Healthy,
                    plan_usage_used: None,
                    plan_limit: None,
                    cost: CostEstimate::Unknown,
                    running_tasks: 0,
                    learned: None,
                })
                .collect()
        };
        let crew = Crew {
            settings,
            labels: &[],
            run_task_type: Some("feature"),
            workers: &workers,
            provider: &provider,
            protected: &[],
        };
        assign(tasks(), &crew, &mut StdRng::seed_from_u64(seed))
    }

    #[test]
    fn every_task_gets_a_worker_and_a_ring_from_other_providers() {
        let settings = settings();
        let assignment = assigned(&settings, 42);
        assert_eq!(assignment, assigned(&settings, 42));
        assert_eq!(assignment.picks.len(), assignment.tasks.len());
        let by_id: BTreeMap<&TaskId, &TaskSpec> = assignment
            .tasks
            .iter()
            .map(|task| (&task.id, task))
            .collect();
        for task in &assignment.tasks {
            let worker = task.worker.as_ref().unwrap();
            assert_eq!(task.reviewers.len(), 1, "{}", task.id);
            for reviewer in &task.reviewers {
                assert_ne!(provider(reviewer), provider(worker), "{}", task.id);
            }
            if let TaskKind::AcceptanceTests { for_task } = &task.kind {
                let tested = by_id[for_task];
                assert_eq!(Some(worker), tested.reviewers.first());
                assert_ne!(Some(worker), tested.worker.as_ref());
                assert!(!task.reviewers.contains(tested.worker.as_ref().unwrap()));
            }
        }
        let docs = &assignment
            .picks
            .iter()
            .find(|p| p.task.as_str() == "task-004")
            .unwrap();
        assert_eq!(docs.algorithm, RoutingAlgorithm::RoundRobin);
        assert_eq!(docs.choice.reason, "rotating chore, turn 1");
        let graph = TaskGraph {
            tasks: assignment.tasks,
            critical_path: Vec::new(),
        };
        assert!(shortfalls(&graph, &settings).is_empty());
    }

    #[test]
    fn with_the_orchestrator_picking_each_task_takes_the_best_by_its_order() {
        let mut settings = settings();
        settings.crew.orchestrator_picks.worker = true;
        let assignment = assigned(&settings, 7);
        assert_eq!(assignment, assigned(&settings, 99), "no draw decides it");
        // Chores rotate and acceptance tests go to a reviewer of the task they test, as before.
        let tests: Vec<&TaskId> = assignment
            .tasks
            .iter()
            .filter(|task| is_test(task))
            .map(|task| &task.id)
            .collect();
        let built: Vec<&Pick> = assignment
            .picks
            .iter()
            .filter(|pick| pick.algorithm != RoutingAlgorithm::RoundRobin)
            .filter(|pick| !tests.contains(&&pick.task))
            .collect();
        assert!(!built.is_empty());
        for pick in built {
            assert_eq!(
                pick.choice.worker,
                worker("claude-code/opus"),
                "{}",
                pick.task
            );
            assert!(
                pick.choice.reason.starts_with("the orchestrator's pick"),
                "{}",
                pick.choice.reason
            );
        }
        assert!(assignment.draws.is_empty());
    }

    #[test]
    fn one_worker_for_the_whole_plan() {
        let mut settings = settings();
        settings.divide.single_worker = true;
        let assignment = assigned(&settings, 3);
        let workers: Vec<&ModelRef> = assignment
            .tasks
            .iter()
            .filter(|task| !is_test(task))
            .filter_map(|task| task.worker.as_ref())
            .collect();
        assert!(workers.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn a_one_provider_crew_is_flagged() {
        let mut settings = settings();
        settings.crew = vec![worker("claude-code/opus"), worker("claude-code/sonnet")].into();
        let assignment = assigned(&settings, 1);
        let graph = TaskGraph {
            tasks: assignment.tasks,
            critical_path: Vec::new(),
        };
        let flags = shortfalls(&graph, &settings);
        assert!(
            flags
                .iter()
                .any(|flag| flag.contains("0 of 1 reviewers from another provider"))
        );
    }
}
