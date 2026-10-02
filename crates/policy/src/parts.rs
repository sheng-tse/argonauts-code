//! The seams between the policy's parts. Deciding a checkpoint asks the hard limits, the
//! approver model, and the autonomy records through these, so each part can be built and
//! tested with the others faked.

use std::sync::Arc;

use argonauts_core::clock::Clock;
use argonauts_core::future::BoxFuture;
use argonauts_core::names::WorkerRef;
use argonauts_core::policy::{
    ApprovalsView, DecisionPoint, EvidencePacket, HardLimits, ShadowRecord, UserDecision, Verdict,
    Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;

use crate::approvals::settings::SettingsSource;

pub struct PolicyParts {
    pub source: SettingsSource,
    pub limits: Arc<dyn HardLimits>,
    pub approver: Arc<dyn Approve>,
    pub autonomy: Arc<dyn Autonomy>,
    pub clock: Arc<dyn Clock>,
}

/// Your approver model, or two. Anything that goes wrong comes back as `Escalate`, never as an
/// approval.
pub trait Approve: Send + Sync {
    fn verdict<'a>(
        &'a self,
        point: &'a DecisionPoint,
        evidence: &'a EvidencePacket,
        settings: &'a Settings,
    ) -> BoxFuture<'a, ModelVerdict>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelVerdict {
    pub verdict: Verdict,
    /// The model that answered, or the first of two.
    pub approver: Option<WorkerRef>,
    pub reasons: Vec<String>,
}

/// Earned autonomy, spot checks, `auto-off`, and the daily caps, kept in the state directory
/// and `~/.argonauts`.
pub trait Autonomy: Send + Sync {
    /// `configured`, lowered to supervised by `auto-off`, and from auto to shadow for a kind
    /// that lost its autonomy. Never more autonomous than `configured`.
    fn effective_mode(&self, point: &DecisionPoint, configured: ApprovalMode) -> ApprovalMode;
    /// Why new auto-approvals ask right now, such as too many spot checks waiting.
    fn auto_paused(&self, settings: &Settings) -> Option<String>;
    /// Takes what an auto-approval uses, under the daily file lock: one of today's auto-merges
    /// for a merge, the day's spend, and a spot-check draw.
    fn reserve(
        &self,
        point: &DecisionPoint,
        evidence: &EvidencePacket,
        settings: &Settings,
    ) -> Result<Reservation, Refusal>;
    /// What auto would have decided. It's scored when your answer for the same checkpoint
    /// arrives, in either order. `evidence` holds the rules document the approver model read.
    fn note_shadow(
        &self,
        point: &DecisionPoint,
        shadow: &ShadowRecord,
        evidence: &EvidencePacket,
        settings: &Settings,
    );
    /// Scores the shadow record, and gives back a reservation your answer beat.
    fn user_decided(&self, decision: &UserDecision, settings: &Settings);
    /// A wrong auto-approval demotes its kind. A wrong checkpoint you approved yourself, such
    /// as a reverted merge, turns its match into a miss. Reporting it again changes nothing.
    fn spot_checked(&self, point: &DecisionPoint, correct: bool, settings: &Settings);
    fn auto_off(&self);
    fn view(&self, settings: &Settings) -> ApprovalsView;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    /// The approval joined your spot-check queue.
    pub spot_check: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// Your answer for this checkpoint came first, so auto's outcome is only a shadow record.
    AlreadyDecided,
    /// A daily cap is used up.
    Capped(Vec<Violation>),
    /// Auto can't approve this kind right now, such as after a wrong spot check or with too
    /// many spot checks waiting, so the verdict is only a shadow record.
    Paused(String),
    /// The records couldn't be read or written, so auto asks instead.
    Unavailable(String),
}
