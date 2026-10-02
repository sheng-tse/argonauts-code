//! The run as a whole: its start, mode, phase, pause and stop requests, approval mode, parking,
//! budget, vendors, and outcome.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::CardId;
use argonauts_core::events::RunEvent;
use argonauts_core::events::checkpoint::CheckpointEvent;
use argonauts_core::events::run::{
    ParkReason, PhaseChange, ResumeCause, RunLifecycle, RunMode, RunOutcome, RunPhase, RunStarted,
};
use argonauts_core::events::vendor::{BudgetEvent, HandoffEvent, RunBudget, VendorEvent};
use argonauts_core::limits::{HealthState, QuotaStatus};
use argonauts_core::names::{CheckpointId, VendorId};
use argonauts_core::settings::approvals::ApprovalMode;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    pub started: Option<Box<RunStarted>>,
    pub mode: Option<RunMode>,
    pub phase: Option<RunPhase>,
    /// `Some(now)` while a pause is asked for and not cancelled.
    pub pause_requested: Option<bool>,
    pub paused: bool,
    pub stop_requested: bool,
    /// Set with Shift+Tab. It applies to every decision point from then on.
    pub approval_mode: Option<ApprovalMode>,
    pub parked: Option<ParkReason>,
    pub finished: Option<RunOutcome>,
    pub budget: Option<RunBudget>,
    /// The seq of the newest `Run::Snapshot`.
    pub last_snapshot: Option<u64>,
    /// How many times the run woke after parking or a crash.
    pub resumes: u32,
    pub health: BTreeMap<VendorId, HealthState>,
    pub quota: BTreeMap<VendorId, QuotaStatus>,
    /// When a vendor whose quota ran out said it resets, in Unix seconds.
    #[serde(default)]
    pub quota_resets: BTreeMap<VendorId, i64>,
    pub disabled: BTreeMap<VendorId, String>,
    /// Cards waiting for a vendor to come back, and until when if it said.
    pub vendor_waits: BTreeMap<CardId, VendorWait>,
    /// When each checkpoint opened, in Unix seconds, for the handoff ask timeout.
    pub opened_at: BTreeMap<CheckpointId, i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorWait {
    pub vendor: VendorId,
    /// Unix seconds.
    pub until: Option<i64>,
}

impl RunInfo {
    pub fn apply(&mut self, event: &RunLifecycle, envelope: &RunEvent) {
        match event {
            RunLifecycle::Started(started) => {
                self.mode = Some(started.mode);
                self.started = Some(started.clone());
            }
            RunLifecycle::Resumed(resumed) => {
                self.resumes += 1;
                // A crash that cut a pause short keeps the pause: only a parked run's resume
                // ends it. Reopening a stopped run ends its stop.
                let reopened = self.finished.take().is_some();
                if self.parked.is_some() || reopened {
                    self.paused = false;
                    self.pause_requested = None;
                    self.stop_requested = false;
                }
                self.parked = None;
                if resumed.cause == ResumeCause::VendorBack {
                    self.vendor_waits.clear();
                }
            }
            RunLifecycle::PauseRequested { now, .. } => self.pause_requested = Some(*now),
            RunLifecycle::ApprovalModeChanged { mode, .. } => self.approval_mode = Some(*mode),
            RunLifecycle::PauseCancelled => {
                self.pause_requested = None;
                self.paused = false;
            }
            RunLifecycle::Paused { .. } => {
                self.pause_requested = None;
                self.paused = true;
            }
            RunLifecycle::StopRequested { .. } => self.stop_requested = true,
            RunLifecycle::Parked { reason, .. } => self.parked = Some(reason.clone()),
            RunLifecycle::Snapshot(_) => self.last_snapshot = Some(envelope.seq),
            RunLifecycle::Finished { outcome, .. } => self.finished = Some(*outcome),
        }
    }

    pub fn apply_phase(&mut self, event: &PhaseChange, envelope: &RunEvent) {
        let _ = envelope;
        self.phase = Some(event.phase);
    }

    pub fn apply_vendor(&mut self, event: &VendorEvent, envelope: &RunEvent) {
        let _ = envelope;
        match event {
            VendorEvent::Quota(reading) => {
                self.quota.insert(reading.vendor.clone(), reading.status);
                match reading.resets_at {
                    Some(at) if reading.status == QuotaStatus::Rejected => {
                        self.quota_resets
                            .insert(reading.vendor.clone(), at.unix_timestamp());
                    }
                    _ => {
                        self.quota_resets.remove(&reading.vendor);
                    }
                }
            }
            VendorEvent::Health(health) => {
                self.health.insert(health.vendor.clone(), health.state);
                match &health.disabled {
                    Some(reason) => self.disabled.insert(health.vendor.clone(), reason.clone()),
                    None => self.disabled.remove(&health.vendor),
                };
            }
            VendorEvent::Disabled { vendor, reason } => {
                self.disabled.insert(vendor.clone(), reason.clone());
            }
        }
    }

    pub fn apply_budget(&mut self, event: &BudgetEvent, envelope: &RunEvent) {
        let _ = envelope;
        if let BudgetEvent::Run(budget) = event {
            self.budget = Some(budget.clone());
        }
    }

    /// Which cards wait for a vendor. Their handoffs are the card fold's; this is for parking.
    pub fn apply_handoff(&mut self, event: &HandoffEvent, envelope: &RunEvent) {
        let _ = envelope;
        match event {
            HandoffEvent::Waiting {
                card,
                vendor,
                until,
            } => {
                let wait = VendorWait {
                    vendor: vendor.clone(),
                    until: until.map(OffsetDateTime::unix_timestamp),
                };
                self.vendor_waits.insert(card.clone(), wait);
            }
            HandoffEvent::HandedOff { card, .. } => {
                self.vendor_waits.remove(card);
            }
            HandoffEvent::Needed(_) | HandoffEvent::Packet { .. } => {}
        }
    }

    pub fn apply_checkpoint(&mut self, event: &CheckpointEvent, envelope: &RunEvent) {
        match event {
            CheckpointEvent::Opened(opened) => {
                self.opened_at
                    .insert(opened.id.clone(), envelope.ts.unix_timestamp());
            }
            CheckpointEvent::Decided(decided) => {
                self.opened_at.remove(&decided.id);
            }
            CheckpointEvent::Withdrawn { id, .. } => {
                self.opened_at.remove(id);
            }
            CheckpointEvent::Checked(_) | CheckpointEvent::SpotCheckQueued { .. } => {}
        }
    }

    /// A card with work again no longer waits for its vendor.
    pub fn card_active(&mut self, card: &CardId) {
        self.vendor_waits.remove(card);
    }

    /// Whether this vendor was out, by health or quota, as far as the log says.
    pub fn vendor_out(&self, vendor: &VendorId) -> bool {
        self.health_out(vendor) || self.quota.get(vendor) == Some(&QuotaStatus::Rejected)
    }

    /// Whether it's still out at `now`: a quota that ran out counts until the reset it reported
    /// has passed, as the handoff planner reads it.
    pub fn vendor_out_at(&self, vendor: &VendorId, now: OffsetDateTime) -> bool {
        self.health_out(vendor) || self.quota_out_at(vendor, now)
    }

    fn health_out(&self, vendor: &VendorId) -> bool {
        matches!(
            self.health.get(vendor),
            Some(HealthState::Down | HealthState::Degraded)
        )
    }

    fn quota_out_at(&self, vendor: &VendorId, now: OffsetDateTime) -> bool {
        self.quota.get(vendor) == Some(&QuotaStatus::Rejected)
            && self
                .quota_resets
                .get(vendor)
                .is_none_or(|at| *at > now.unix_timestamp())
    }

    /// Vendors whose quota ran out and has reset since, and that are out no other way.
    pub fn quota_reset_passed(&self, now: OffsetDateTime) -> Vec<VendorId> {
        self.quota_resets
            .iter()
            .filter(|(vendor, at)| {
                **at <= now.unix_timestamp()
                    && self.quota.get(*vendor) == Some(&QuotaStatus::Rejected)
                    && !self.health_out(vendor)
            })
            .map(|(vendor, _)| vendor.clone())
            .collect()
    }

    /// The soonest reset still to come of a quota that ran out, when one was reported.
    pub fn next_quota_reset(&self, now: OffsetDateTime) -> Option<i64> {
        self.quota_resets
            .iter()
            .filter(|(vendor, at)| {
                self.quota.get(*vendor) == Some(&QuotaStatus::Rejected)
                    && **at > now.unix_timestamp()
            })
            .map(|(_, at)| *at)
            .min()
    }
}
