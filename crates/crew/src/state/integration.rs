//! The integration branch: what merged, conflicts, main moving, the final check, and how the
//! run landed.

use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::names::WorkerRef;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IntegrationState {
    pub branch: Option<String>,
    pub base: Option<String>,
    /// `(card, merge commit)` in merge order. A card merged again after a change request keeps
    /// its place with its newest merge commit.
    pub merged: Vec<(CardId, String)>,
    /// Open conflicts and who resolves them.
    pub conflicts: Vec<Conflict>,
    /// The main commit the integration branch last took in: the base, or where main was when
    /// it was last merged in.
    pub main_seen: Option<String>,
    /// Set while a `MainMoved` waits for its `MainMergedIn`.
    pub main_moving: Option<String>,
    /// Cleared by anything that changes the integration branch afterwards.
    pub final_check: Option<bool>,
    pub dependencies: Vec<String>,
    pub landed: Option<Landed>,
    pub pull_request: Option<PullRequest>,
    /// The branches kept when you abandoned the run.
    pub abandoned: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub card: CardId,
    pub files: Vec<String>,
    pub assigned_to: WorkerRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landed {
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

impl IntegrationState {
    pub fn apply(&mut self, event: &IntegrationEvent, envelope: &RunEvent) {
        let _ = envelope;
        match event {
            IntegrationEvent::BranchCreated { branch, base } => {
                self.branch = Some(branch.clone());
                self.base = Some(base.clone());
                self.main_seen.get_or_insert_with(|| base.clone());
            }
            IntegrationEvent::Merged { card, sha } => {
                match self.merged.iter_mut().find(|(merged, _)| merged == card) {
                    Some(entry) => entry.1 = sha.clone(),
                    None => self.merged.push((card.clone(), sha.clone())),
                }
                self.conflicts.retain(|conflict| &conflict.card != card);
                self.final_check = None;
            }
            IntegrationEvent::Conflict {
                card,
                files,
                assigned_to,
            } => {
                self.conflicts.retain(|conflict| &conflict.card != card);
                self.conflicts.push(Conflict {
                    card: card.clone(),
                    files: files.clone(),
                    assigned_to: assigned_to.clone(),
                });
            }
            IntegrationEvent::MainMoved { to, .. } => {
                self.main_moving = Some(to.clone());
            }
            IntegrationEvent::MainMergedIn { .. } => {
                if let Some(main) = self.main_moving.take() {
                    self.main_seen = Some(main);
                }
                self.final_check = None;
            }
            IntegrationEvent::FinalCheck { passed } => self.final_check = Some(*passed),
            IntegrationEvent::Landed {
                target,
                sha,
                fast_forward,
            } => {
                self.landed = Some(Landed {
                    target: target.clone(),
                    sha: sha.clone(),
                    fast_forward: *fast_forward,
                });
            }
            IntegrationEvent::PullRequest {
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
            IntegrationEvent::Abandoned { branches } => self.abandoned = Some(branches.clone()),
            IntegrationEvent::DependenciesAdded { packages, .. } => {
                for package in packages {
                    if !self.dependencies.contains(package) {
                        self.dependencies.push(package.clone());
                    }
                }
                self.final_check = None;
            }
        }
    }

    pub fn is_merged(&self, card: &CardId) -> bool {
        self.merged.iter().any(|(merged, _)| merged == card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::RunId;
    use argonauts_core::events::RunEventKind;
    use time::macros::datetime;

    fn envelope(event: &IntegrationEvent) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: datetime!(2026-09-30 10:00 UTC),
            run_id: RunId::new("run-01").unwrap(),
            card_id: None,
            session: None,
            source: "conductor".to_owned(),
            kind: RunEventKind::Integration(event.clone()),
        }
    }

    fn fold(events: &[IntegrationEvent]) -> IntegrationState {
        let mut state = IntegrationState::default();
        for event in events {
            state.apply(event, &envelope(event));
        }
        state
    }

    fn card(n: u32) -> CardId {
        CardId::numbered(n)
    }

    #[test]
    fn merges_conflicts_and_the_final_check_fold() {
        let owner = WorkerRef::new("codex", Some("gpt-5.6".into()));
        let state = fold(&[
            IntegrationEvent::BranchCreated {
                branch: "argonauts/run-01/integration".into(),
                base: "b0".into(),
            },
            IntegrationEvent::Merged {
                card: card(1),
                sha: "m1".into(),
            },
            IntegrationEvent::Conflict {
                card: card(2),
                files: vec!["src/a.rs".into()],
                assigned_to: owner.clone(),
            },
            IntegrationEvent::FinalCheck { passed: true },
        ]);
        assert_eq!(
            state.branch.as_deref(),
            Some("argonauts/run-01/integration")
        );
        assert_eq!(state.main_seen.as_deref(), Some("b0"));
        assert_eq!(state.merged, vec![(card(1), "m1".to_owned())]);
        assert_eq!(state.conflicts.len(), 1);
        assert_eq!(state.final_check, Some(true));

        // The conflict resolved and a card merged again: the check has to run again.
        let mut state = state;
        for event in [
            IntegrationEvent::Merged {
                card: card(2),
                sha: "m2".into(),
            },
            IntegrationEvent::Merged {
                card: card(1),
                sha: "m3".into(),
            },
        ] {
            state.apply(&event, &envelope(&event));
        }
        assert!(state.conflicts.is_empty());
        assert_eq!(
            state.merged,
            vec![(card(1), "m3".to_owned()), (card(2), "m2".to_owned())]
        );
        assert_eq!(state.final_check, None);
        assert!(state.is_merged(&card(2)));
    }

    #[test]
    fn main_moving_updates_what_the_branch_has_seen() {
        let state = fold(&[
            IntegrationEvent::BranchCreated {
                branch: "argonauts/run-01/integration".into(),
                base: "b0".into(),
            },
            IntegrationEvent::FinalCheck { passed: false },
            IntegrationEvent::MainMoved {
                from: "b0".into(),
                to: "b1".into(),
                commits: 2,
            },
        ]);
        assert_eq!(state.main_seen.as_deref(), Some("b0"));
        assert_eq!(state.main_moving.as_deref(), Some("b1"));

        let mut state = state;
        let event = IntegrationEvent::MainMergedIn { sha: "m9".into() };
        state.apply(&event, &envelope(&event));
        assert_eq!(state.main_seen.as_deref(), Some("b1"));
        assert_eq!(state.main_moving, None);
        assert_eq!(state.final_check, None);
    }

    #[test]
    fn how_the_run_landed() {
        let state = fold(&[
            IntegrationEvent::Landed {
                target: "main".into(),
                sha: "h1".into(),
                fast_forward: true,
            },
            IntegrationEvent::PullRequest {
                branch: "argonauts/run-01/integration".into(),
                url: None,
                command: Some(vec!["gh".into(), "pr".into(), "create".into()]),
            },
            IntegrationEvent::Abandoned {
                branches: vec!["argonauts/run-01/integration".into()],
            },
            IntegrationEvent::DependenciesAdded {
                packages: vec!["serde".into(), "serde".into()],
                sha: "d1".into(),
            },
        ]);
        assert_eq!(
            state.landed,
            Some(Landed {
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
