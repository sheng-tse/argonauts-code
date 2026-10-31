//! The run branch: what merged, conflicts, main moving, the final check, and how the
//! run merged.

use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::base::names::ModelRef;
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::task::MergeEvent;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MergeState {
    pub branch: Option<String>,
    pub base: Option<String>,
    /// `(task, merge commit)` in merge order. A task merged again after a change request keeps
    /// its place with its newest merge commit.
    pub merged: Vec<(TaskId, String)>,
    /// Open conflicts and who resolves them.
    pub conflicts: Vec<Conflict>,
    /// The main commit the run branch last took in: the base, or where main was when
    /// it was last merged in.
    pub main_seen: Option<String>,
    /// Set while a `MainMoved` waits for its `MainMergedIn`.
    pub main_moving: Option<String>,
    /// Cleared by anything that changes the run branch afterwards.
    pub final_check: Option<bool>,
    pub dependencies: Vec<String>,
    pub run_merged: Option<RunMerged>,
    /// Your files in the way of landing, while the merge waits for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_to_merge: Vec<String>,
    pub pull_request: Option<PullRequest>,
    /// The branches the run had when you abandoned it, which go with it.
    pub abandoned: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub task: TaskId,
    pub files: Vec<String>,
    pub assigned_to: ModelRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMerged {
    pub target: String,
    pub sha: String,
    pub fast_forward: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub branch: String,
    pub url: Option<String>,
    pub command: Option<Vec<String>>,
}

impl MergeState {
    pub fn apply(&mut self, event: &MergeEvent, envelope: &RunEvent) {
        let _ = envelope;
        match event {
            MergeEvent::BranchCreated { branch, base } => {
                self.branch = Some(branch.clone());
                self.base = Some(base.clone());
                self.main_seen.get_or_insert_with(|| base.clone());
            }
            MergeEvent::Merged { task, sha } => {
                match self.merged.iter_mut().find(|(merged, _)| merged == task) {
                    Some(entry) => entry.1 = sha.clone(),
                    None => self.merged.push((task.clone(), sha.clone())),
                }
                self.conflicts.retain(|conflict| &conflict.task != task);
                self.final_check = None;
            }
            MergeEvent::Conflict {
                task,
                files,
                assigned_to,
            } => {
                self.conflicts.retain(|conflict| &conflict.task != task);
                self.conflicts.push(Conflict {
                    task: task.clone(),
                    files: files.clone(),
                    assigned_to: assigned_to.clone(),
                });
            }
            MergeEvent::MainMoved { to, .. } => {
                self.main_moving = Some(to.clone());
            }
            MergeEvent::MainMergedIn { .. } => {
                if let Some(main) = self.main_moving.take() {
                    self.main_seen = Some(main);
                }
                self.final_check = None;
            }
            MergeEvent::FinalCheck { passed } => self.final_check = Some(*passed),
            MergeEvent::RunMerged {
                target,
                sha,
                fast_forward,
            } => {
                self.waiting_to_merge.clear();
                self.run_merged = Some(RunMerged {
                    target: target.clone(),
                    sha: sha.clone(),
                    fast_forward: *fast_forward,
                });
            }
            MergeEvent::WaitingToMerge { in_the_way, .. } => {
                self.waiting_to_merge = in_the_way.clone();
            }
            MergeEvent::PullRequest {
                branch,
                url,
                command,
            } => {
                self.pull_request = Some(PullRequest {
                    branch: branch.clone(),
                    url: url.clone(),
                    command: command.clone(),
                });
            }
            MergeEvent::Abandoned { branches } => self.abandoned = Some(branches.clone()),
            MergeEvent::DependenciesAdded { packages, .. } => {
                for package in packages {
                    if !self.dependencies.contains(package) {
                        self.dependencies.push(package.clone());
                    }
                }
                self.final_check = None;
            }
        }
    }

    pub fn is_merged(&self, task: &TaskId) -> bool {
        self.merged.iter().any(|(merged, _)| merged == task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::RunId;
    use argonauts_core::runs::events::RunEventKind;
    use time::macros::datetime;

    fn envelope(event: &MergeEvent) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: datetime!(2026-09-30 10:00 UTC),
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Merge(event.clone()),
        }
    }

    fn fold(events: &[MergeEvent]) -> MergeState {
        let mut state = MergeState::default();
        for event in events {
            state.apply(event, &envelope(event));
        }
        state
    }

    fn task(n: u32) -> TaskId {
        TaskId::numbered(n)
    }

    #[test]
    fn merges_conflicts_and_the_final_check_fold() {
        let worker = ModelRef::new("codex", Some("gpt-5.6".into()));
        let state = fold(&[
            MergeEvent::BranchCreated {
                branch: "argonauts/run-01/merged".into(),
                base: "b0".into(),
            },
            MergeEvent::Merged {
                task: task(1),
                sha: "m1".into(),
            },
            MergeEvent::Conflict {
                task: task(2),
                files: vec!["src/a.rs".into()],
                assigned_to: worker.clone(),
            },
            MergeEvent::FinalCheck { passed: true },
        ]);
        assert_eq!(state.branch.as_deref(), Some("argonauts/run-01/merged"));
        assert_eq!(state.main_seen.as_deref(), Some("b0"));
        assert_eq!(state.merged, vec![(task(1), "m1".to_owned())]);
        assert_eq!(state.conflicts.len(), 1);
        assert_eq!(state.final_check, Some(true));

        // The conflict resolved and a task merged again: the check has to run again.
        let mut state = state;
        for event in [
            MergeEvent::Merged {
                task: task(2),
                sha: "m2".into(),
            },
            MergeEvent::Merged {
                task: task(1),
                sha: "m3".into(),
            },
        ] {
            state.apply(&event, &envelope(&event));
        }
        assert!(state.conflicts.is_empty());
        assert_eq!(
            state.merged,
            vec![(task(1), "m3".to_owned()), (task(2), "m2".to_owned())]
        );
        assert_eq!(state.final_check, None);
        assert!(state.is_merged(&task(2)));
    }

    #[test]
    fn main_moving_updates_what_the_branch_has_seen() {
        let state = fold(&[
            MergeEvent::BranchCreated {
                branch: "argonauts/run-01/merged".into(),
                base: "b0".into(),
            },
            MergeEvent::FinalCheck { passed: false },
            MergeEvent::MainMoved {
                from: "b0".into(),
                to: "b1".into(),
                commits: 2,
            },
        ]);
        assert_eq!(state.main_seen.as_deref(), Some("b0"));
        assert_eq!(state.main_moving.as_deref(), Some("b1"));

        let mut state = state;
        let event = MergeEvent::MainMergedIn { sha: "m9".into() };
        state.apply(&event, &envelope(&event));
        assert_eq!(state.main_seen.as_deref(), Some("b1"));
        assert_eq!(state.main_moving, None);
        assert_eq!(state.final_check, None);
    }

    #[test]
    fn how_the_run_merged() {
        let state = fold(&[
            MergeEvent::RunMerged {
                target: "main".into(),
                sha: "h1".into(),
                fast_forward: true,
            },
            MergeEvent::PullRequest {
                branch: "argonauts/run-01/merged".into(),
                url: None,
                command: Some(vec!["gh".into(), "pr".into(), "create".into()]),
            },
            MergeEvent::Abandoned {
                branches: vec!["argonauts/run-01/merged".into()],
            },
            MergeEvent::DependenciesAdded {
                packages: vec!["serde".into(), "serde".into()],
                sha: "d1".into(),
            },
        ]);
        assert_eq!(
            state.run_merged,
            Some(RunMerged {
                target: "main".into(),
                sha: "h1".into(),
                fast_forward: true
            })
        );
        assert_eq!(state.pull_request.unwrap().url, None);
        assert_eq!(state.abandoned.unwrap().len(), 1);
        assert_eq!(state.dependencies, vec!["serde".to_owned()]);
    }
}
