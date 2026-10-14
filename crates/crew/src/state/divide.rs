//! Dividing the work: the task graph, workers, reviewers, predictions, and limit lines.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::plan::{DivideEvent, LimitPlan, TaskGraph};
use argonauts_core::names::ModelRef;
use argonauts_core::settings::protocol::RoutingAlgorithm;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DivideState {
    pub graph: Option<TaskGraph>,
    /// The seq of the newest `Divide::Graph`, which a later send-back makes stale.
    pub graph_seq: Option<u64>,
    pub limit: Option<LimitPlan>,
    /// Why each task got its worker, for the task view.
    pub workers: BTreeMap<TaskId, WorkerPick>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerPick {
    pub worker: ModelRef,
    pub algorithm: RoutingAlgorithm,
    pub explored: bool,
    pub reason: String,
}

impl DivideState {
    /// Workers, reviewers, and predictions recorded after the graph (your edits) update its tasks.
    pub fn apply(&mut self, event: &DivideEvent, envelope: &RunEvent) {
        match event {
            DivideEvent::Graph(graph) => {
                self.workers
                    .retain(|task, _| graph.tasks.iter().any(|spec| &spec.id == task));
                self.graph = Some(graph.clone());
                self.graph_seq = Some(envelope.seq);
            }
            DivideEvent::Worker {
                task,
                worker,
                algorithm,
                explored,
                reason,
            } => {
                self.workers.insert(
                    task.clone(),
                    WorkerPick {
                        worker: worker.clone(),
                        algorithm: *algorithm,
                        explored: *explored,
                        reason: reason.clone(),
                    },
                );
                if let Some(spec) = self.task_mut(task) {
                    spec.worker = Some(worker.clone());
                }
            }
            DivideEvent::Reviewers {
                task, reviewers, ..
            } => {
                if let Some(spec) = self.task_mut(task) {
                    spec.reviewers.clone_from(reviewers);
                }
            }
            DivideEvent::Predicted { task, predictions } => {
                if let Some(spec) = self.task_mut(task) {
                    spec.predictions = Some(*predictions);
                }
            }
            DivideEvent::Limit(plan) => self.limit = Some(plan.clone()),
        }
    }

    fn task_mut(&mut self, task: &TaskId) -> Option<&mut argonauts_core::events::task::TaskSpec> {
        self.graph
            .as_mut()?
            .tasks
            .iter_mut()
            .find(|spec| &spec.id == task)
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::task::{TaskKind, TaskLimit, TaskSpec};
    use argonauts_core::steps::task::Predictions;

    use super::*;

    fn event(seq: u64, kind: DivideEvent) -> RunEvent {
        RunEvent {
            seq,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Divide(kind),
        }
    }

    fn spec(n: u32) -> TaskSpec {
        TaskSpec {
            id: TaskId::numbered(n),
            title: format!("task {n}"),
            task: String::new(),
            kind: TaskKind::Build,
            owned_paths: vec![format!("src/{n}/**")],
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: None,
            reviewers: Vec::new(),
            limit: TaskLimit::default(),
            predictions: None,
            item: None,
        }
    }

    #[test]
    fn the_fold_keeps_the_newest_graph_and_its_edits() {
        let codex: ModelRef = "codex".parse().unwrap();
        let agy: ModelRef = "agy".parse().unwrap();
        let events = [
            event(
                3,
                DivideEvent::Worker {
                    task: TaskId::numbered(1),
                    worker: codex.clone(),
                    algorithm: RoutingAlgorithm::Bandit,
                    explored: false,
                    reason: "9 of 10 similar tasks passed".into(),
                },
            ),
            event(
                4,
                DivideEvent::Graph(TaskGraph {
                    tasks: vec![spec(1), spec(2)],
                    critical_path: vec![TaskId::numbered(1)],
                }),
            ),
            event(
                7,
                DivideEvent::Reviewers {
                    task: TaskId::numbered(2),
                    reviewers: vec![agy.clone()],
                    blind: false,
                },
            ),
            event(
                8,
                DivideEvent::Predicted {
                    task: TaskId::numbered(2),
                    predictions: Predictions {
                        p_pass_checks_first_try: 0.7,
                        p_within_time_cap: 0.8,
                        p_at_most_two_tries: 0.9,
                    },
                },
            ),
        ];
        let mut state = DivideState::default();
        for event in &events {
            let RunEventKind::Divide(kind) = &event.kind else {
                unreachable!()
            };
            state.apply(kind, event);
        }
        assert_eq!(state.graph_seq, Some(4));
        assert_eq!(state.workers[&TaskId::numbered(1)].worker, codex);
        let graph = state.graph.as_ref().unwrap();
        assert_eq!(graph.tasks[1].reviewers, [agy]);
        assert_eq!(graph.tasks[1].predictions.unwrap().p_within_time_cap, 0.8);
    }
}
