//! Every approval the run opened, its policy checks, and its decision or withdrawal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::base::names::ApprovalId;
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::approval::{
    ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, PolicyCheck,
};
use argonauts_core::safety::policy::Verdict;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Approvals {
    pub by_id: BTreeMap<ApprovalId, ApprovalRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub opened: ApprovalOpened,
    /// The seq of its `Approval::Opened`.
    pub since: u64,
    pub checks: Vec<PolicyCheck>,
    pub decided: Option<ApprovalDecided>,
    pub withdrawn: Option<String>,
    /// Set when an auto-approval joined your spot-check queue.
    #[serde(default)]
    pub spot_check: bool,
}

impl ApprovalRecord {
    pub fn is_open(&self) -> bool {
        self.decided.is_none() && self.withdrawn.is_none()
    }

    /// The policy's own outcome, not an auto preview record of one that came late.
    pub fn policy_check(&self) -> Option<&PolicyCheck> {
        self.checks.iter().find(|check| !check.auto_preview)
    }
}

impl Approvals {
    pub fn apply(&mut self, event: &ApprovalEvent, envelope: &RunEvent) {
        match event {
            ApprovalEvent::Opened(opened) => {
                self.by_id.insert(
                    opened.id.clone(),
                    ApprovalRecord {
                        opened: (**opened).clone(),
                        since: envelope.seq,
                        checks: Vec::new(),
                        decided: None,
                        withdrawn: None,
                        spot_check: false,
                    },
                );
            }
            ApprovalEvent::Checked(check) => {
                if let Some(record) = self.by_id.get_mut(&check.id) {
                    if !check.auto_preview {
                        record.opened.needs_you =
                            !matches!(check.verdict, Some(Verdict::Approve | Verdict::Reject));
                    }
                    record.checks.push(check.clone());
                }
            }
            ApprovalEvent::Decided(decided) => {
                if let Some(record) = self.by_id.get_mut(&decided.id) {
                    // The first decision wins, so a replayed log never overwrites it.
                    if record.decided.is_none() {
                        record.decided = Some(decided.clone());
                    }
                }
            }
            ApprovalEvent::Withdrawn { id, reason } => {
                if let Some(record) = self.by_id.get_mut(id) {
                    record.withdrawn = Some(reason.clone());
                }
            }
            ApprovalEvent::SpotCheckQueued { id } => {
                if let Some(record) = self.by_id.get_mut(id) {
                    record.spot_check = true;
                }
            }
        }
    }

    /// Opened and neither decided nor withdrawn.
    pub fn open(&self) -> impl Iterator<Item = &ApprovalRecord> {
        self.by_id.values().filter(|record| record.is_open())
    }

    /// The open approval of `kind` for `task`. There is at most one.
    pub fn open_for(&self, kind: ApprovalKind, task: Option<&TaskId>) -> Option<&ApprovalRecord> {
        self.open()
            .find(|record| record.opened.kind == kind && record.opened.task.as_ref() == task)
    }

    /// The newest decided, not withdrawn approval of `kind`, for `task` when one is given.
    pub fn last_decided(
        &self,
        kind: ApprovalKind,
        task: Option<&TaskId>,
    ) -> Option<&ApprovalRecord> {
        self.by_id
            .values()
            .filter(|record| {
                record.opened.kind == kind
                    && record.decided.is_some()
                    && record.withdrawn.is_none()
                    && task.is_none_or(|task| record.opened.task.as_ref() == Some(task))
            })
            .max_by_key(|record| record.since)
    }
}
