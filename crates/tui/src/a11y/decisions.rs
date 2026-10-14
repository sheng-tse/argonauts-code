//! Recent approval decisions, kept so a spot check can say what was decided and why, and the
//! spot checks waiting on you.

use std::collections::VecDeque;

use argonauts_core::RunId;
use argonauts_core::app::AppEvent;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    ApprovalDecided, ApprovalEvent, ApprovalOpened, PolicyCheck,
};
use argonauts_core::names::ApprovalId;

use crate::state::AppState;

/// As many as the screen keeps for its spot checks.
const RECENT: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub run: RunId,
    pub opened: ApprovalOpened,
    pub decided: Option<ApprovalDecided>,
    pub check: Option<PolicyCheck>,
}

#[derive(Debug, Default)]
pub struct Decisions {
    recent: VecDeque<Decision>,
    /// Oldest first.
    spot_checks: Vec<Decision>,
}

impl Decisions {
    /// Takes note of `event` before `state` folds it in, while its approval is still open
    /// there.
    pub fn saw(&mut self, event: &AppEvent, state: &AppState) {
        let AppEvent::Run { event } = event else {
            return;
        };
        let RunEventKind::Approval(approval) = &event.kind else {
            return;
        };
        let run = &event.run_id;
        match approval {
            ApprovalEvent::Checked(check) => {
                if let Some(decision) = self.find_or_open(run, &check.id, state) {
                    decision.check = Some(check.clone());
                }
            }
            ApprovalEvent::Decided(decided) => {
                if let Some(decision) = self.find_or_open(run, &decided.id, state) {
                    decision.decided = Some(decided.clone());
                }
            }
            ApprovalEvent::SpotCheckQueued { id } => {
                let queued = self.spot_check(run, id).is_some();
                // Decided before this screen attached: it waits in `/approvals`.
                if let Some(decision) = self.find(run, id).filter(|_| !queued) {
                    self.spot_checks.push(decision.clone());
                }
            }
            ApprovalEvent::Opened(_) | ApprovalEvent::Withdrawn { .. } => {}
        }
    }

    pub fn spot_checks(&self) -> &[Decision] {
        &self.spot_checks
    }

    pub fn spot_check(&self, run: &RunId, id: &ApprovalId) -> Option<&Decision> {
        self.spot_checks
            .iter()
            .find(|decision| decision.run == *run && decision.opened.id == *id)
    }

    /// You answered it.
    pub fn checked(&mut self, run: &RunId, id: &ApprovalId) {
        self.spot_checks
            .retain(|decision| !(decision.run == *run && decision.opened.id == *id));
    }

    fn find(&self, run: &RunId, id: &ApprovalId) -> Option<&Decision> {
        self.recent
            .iter()
            .rev()
            .find(|decision| decision.run == *run && decision.opened.id == *id)
    }

    fn find_or_open(
        &mut self,
        run: &RunId,
        id: &ApprovalId,
        state: &AppState,
    ) -> Option<&mut Decision> {
        let known = self
            .recent
            .iter()
            .rposition(|decision| decision.run == *run && decision.opened.id == *id);
        let at = match known {
            Some(at) => at,
            None => {
                let opened = state.runs.get(run)?.approval(id)?.opened.as_ref().clone();
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
