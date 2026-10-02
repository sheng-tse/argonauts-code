//! Recent checkpoint decisions, kept so a spot check can say what was decided and why, and the
//! spot checks waiting on you.

use std::collections::VecDeque;

use argonauts_core::RunId;
use argonauts_core::app::AppEvent;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::{
    CheckpointDecided, CheckpointEvent, CheckpointOpened, PolicyCheck,
};
use argonauts_core::names::CheckpointId;

use crate::state::AppState;

/// As many as the screen keeps for its spot checks.
const RECENT: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub run: RunId,
    pub opened: CheckpointOpened,
    pub decided: Option<CheckpointDecided>,
    pub check: Option<PolicyCheck>,
}

#[derive(Debug, Default)]
pub struct Decisions {
    recent: VecDeque<Decision>,
    /// Oldest first.
    spot_checks: Vec<Decision>,
}

impl Decisions {
    /// Takes note of `event` before `state` folds it in, while its checkpoint is still open
    /// there.
    pub fn saw(&mut self, event: &AppEvent, state: &AppState) {
        let AppEvent::Run { event } = event else {
            return;
        };
        let RunEventKind::Checkpoint(checkpoint) = &event.kind else {
            return;
        };
        let run = &event.run_id;
        match checkpoint {
            CheckpointEvent::Checked(check) => {
                if let Some(decision) = self.find_or_open(run, &check.id, state) {
                    decision.check = Some(check.clone());
                }
            }
            CheckpointEvent::Decided(decided) => {
                if let Some(decision) = self.find_or_open(run, &decided.id, state) {
                    decision.decided = Some(decided.clone());
                }
            }
            CheckpointEvent::SpotCheckQueued { id } => {
                let queued = self.spot_check(run, id).is_some();
                // Decided before this screen attached: it waits in `/approvals`.
                if let Some(decision) = self.find(run, id).filter(|_| !queued) {
                    self.spot_checks.push(decision.clone());
                }
            }
            CheckpointEvent::Opened(_) | CheckpointEvent::Withdrawn { .. } => {}
        }
    }

    pub fn spot_checks(&self) -> &[Decision] {
        &self.spot_checks
    }

    pub fn spot_check(&self, run: &RunId, id: &CheckpointId) -> Option<&Decision> {
        self.spot_checks
            .iter()
            .find(|decision| decision.run == *run && decision.opened.id == *id)
    }

    /// You answered it.
    pub fn checked(&mut self, run: &RunId, id: &CheckpointId) {
        self.spot_checks
            .retain(|decision| !(decision.run == *run && decision.opened.id == *id));
    }

    fn find(&self, run: &RunId, id: &CheckpointId) -> Option<&Decision> {
        self.recent
            .iter()
            .rev()
            .find(|decision| decision.run == *run && decision.opened.id == *id)
    }

    fn find_or_open(
        &mut self,
        run: &RunId,
        id: &CheckpointId,
        state: &AppState,
    ) -> Option<&mut Decision> {
        let known = self
            .recent
            .iter()
            .rposition(|decision| decision.run == *run && decision.opened.id == *id);
        let at = match known {
            Some(at) => at,
            None => {
                let opened = state.runs.get(run)?.checkpoint(id)?.opened.as_ref().clone();
                self.recent.push_back(Decision {
                    run: run.clone(),
                    opened,
                    decided: None,
                    check: None,
                });
                while self.recent.len() > RECENT {
                    self.recent.pop_front();
                }
                self.recent.len() - 1
            }
        };
        self.recent.get_mut(at)
    }
}
