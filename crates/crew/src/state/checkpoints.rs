//! Every checkpoint the run opened, its policy checks, and its decision or withdrawal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::checkpoint::{
    CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened, PolicyCheck,
};
use argonauts_core::names::CheckpointId;
use argonauts_core::policy::Verdict;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Checkpoints {
    pub by_id: BTreeMap<CheckpointId, CheckpointRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub opened: CheckpointOpened,
    /// The seq of its `Checkpoint::Opened`.
    pub since: u64,
    pub checks: Vec<PolicyCheck>,
    pub decided: Option<CheckpointDecided>,
    pub withdrawn: Option<String>,
    /// Set when an auto-approval joined your spot-check queue.
    #[serde(default)]
    pub spot_check: bool,
}

impl CheckpointRecord {
    pub fn is_open(&self) -> bool {
        self.decided.is_none() && self.withdrawn.is_none()
    }

    /// The policy's own outcome, not a shadow record of one that came late.
    pub fn policy_check(&self) -> Option<&PolicyCheck> {
        self.checks.iter().find(|check| !check.shadow)
    }
}

impl Checkpoints {
    pub fn apply(&mut self, event: &CheckpointEvent, envelope: &RunEvent) {
        match event {
            CheckpointEvent::Opened(opened) => {
                self.by_id.insert(
                    opened.id.clone(),
                    CheckpointRecord {
                        opened: (**opened).clone(),
                        since: envelope.seq,
                        checks: Vec::new(),
                        decided: None,
                        withdrawn: None,
                        spot_check: false,
                    },
                );
            }
            CheckpointEvent::Checked(check) => {
                if let Some(record) = self.by_id.get_mut(&check.id) {
                    if !check.shadow {
                        record.opened.needs_you =
                            !matches!(check.verdict, Some(Verdict::Approve | Verdict::Reject));
                    }
                    record.checks.push(check.clone());
                }
            }
            CheckpointEvent::Decided(decided) => {
                if let Some(record) = self.by_id.get_mut(&decided.id) {
                    // The first decision wins, so a replayed log never overwrites it.
                    if record.decided.is_none() {
                        record.decided = Some(decided.clone());
                    }
                }
            }
            CheckpointEvent::Withdrawn { id, reason } => {
                if let Some(record) = self.by_id.get_mut(id) {
                    record.withdrawn = Some(reason.clone());
                }
            }
            CheckpointEvent::SpotCheckQueued { id } => {
                if let Some(record) = self.by_id.get_mut(id) {
                    record.spot_check = true;
                }
            }
        }
    }

    /// Opened and neither decided nor withdrawn.
    pub fn open(&self) -> impl Iterator<Item = &CheckpointRecord> {
        self.by_id.values().filter(|record| record.is_open())
    }

    /// The open checkpoint of `kind` for `card`. There is at most one.
    pub fn open_for(
        &self,
        kind: CheckpointKind,
        card: Option<&CardId>,
    ) -> Option<&CheckpointRecord> {
        self.open()
            .find(|record| record.opened.kind == kind && record.opened.card.as_ref() == card)
    }

    /// The newest decided, not withdrawn checkpoint of `kind`, for `card` when one is given.
    pub fn last_decided(
        &self,
        kind: CheckpointKind,
        card: Option<&CardId>,
    ) -> Option<&CheckpointRecord> {
        self.by_id
            .values()
            .filter(|record| {
                record.opened.kind == kind
                    && record.decided.is_some()
                    && record.withdrawn.is_none()
                    && card.is_none_or(|card| record.opened.card.as_ref() == Some(card))
            })
            .max_by_key(|record| record.since)
    }
}
