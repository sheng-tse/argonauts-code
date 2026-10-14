//! A run's recent decisions, kept so a spot check can show the call it asks about and its
//! evidence until you mark it.

use std::collections::VecDeque;

use argonauts_core::events::approval::{ApprovalDecided, ApprovalOpened, PolicyCheck};
use argonauts_core::names::ApprovalId;

/// Decisions kept beyond the ones still waiting for a spot check.
const RECENT: usize = 32;

/// What the screen saw of one decision: what was asked, the call, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRecord {
    pub opened: Box<ApprovalOpened>,
    /// `None` while the policy is still deciding.
    pub decided: Option<ApprovalDecided>,
    pub check: Option<PolicyCheck>,
}

/// Newest last.
#[derive(Debug, Clone, Default)]
pub(super) struct Decisions(VecDeque<DecisionRecord>);

impl Decisions {
    pub fn get(&self, id: &ApprovalId) -> Option<&DecisionRecord> {
        self.0.iter().rev().find(|record| record.opened.id == *id)
    }

    /// `waiting` are the spot checks not yet marked, whose records are never dropped.
    pub fn checked(
        &mut self,
        opened: Option<&ApprovalOpened>,
        check: &PolicyCheck,
        waiting: &[ApprovalId],
    ) {
        if let Some(record) = self
            .0
            .iter_mut()
            .rev()
            .find(|record| record.opened.id == check.id)
        {
            record.check = Some(check.clone());
        } else if let Some(opened) = opened {
            let record = DecisionRecord {
                opened: Box::new(opened.clone()),
                decided: None,
                check: Some(check.clone()),
            };
            self.keep(record, waiting);
        }
    }

    pub fn decided(
        &mut self,
        opened: Option<&ApprovalOpened>,
        decided: &ApprovalDecided,
        waiting: &[ApprovalId],
    ) {
        let earlier = self.take(&decided.id);
        let check = earlier.as_ref().and_then(|record| record.check.clone());
        let opened = opened
            .map(|opened| Box::new(opened.clone()))
            .or_else(|| earlier.map(|record| record.opened));
        if let Some(opened) = opened {
            let record = DecisionRecord {
                opened,
                decided: Some(decided.clone()),
                check,
            };
            self.keep(record, waiting);
        }
    }

    pub fn forget(&mut self, id: &ApprovalId) {
        self.0.retain(|record| record.opened.id != *id);
    }

    fn take(&mut self, id: &ApprovalId) -> Option<DecisionRecord> {
        let at = self.0.iter().rposition(|record| record.opened.id == *id)?;
        let record = self.0.remove(at);
        self.forget(id);
        record
    }

    fn keep(&mut self, record: DecisionRecord, waiting: &[ApprovalId]) {
        self.0.push_back(record);
        while self.0.len() > RECENT {
            let Some(oldest) = self
                .0
                .iter()
                .position(|record| !waiting.contains(&record.opened.id))
            else {
                break;
            };
            self.0.remove(oldest);
        }
    }
}
