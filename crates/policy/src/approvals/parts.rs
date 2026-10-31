//! The seams between the policy's parts. Deciding an approval asks the hard limits, the
//! judge, and the autonomy records through these, so each part can be built and
//! tested with the others faked.

use std::sync::Arc;

use argonauts_core::base::clock::Clock;
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::ModelRef;
use argonauts_core::safety::policy::{
    ApprovalsView, AutoPreviewRecord, DecisionPoint, EvidencePacket, HardLimits, UserDecision,
    Verdict, Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;

use crate::approvals::settings::SettingsSource;

pub struct PolicyParts {
    pub source: SettingsSource,
    pub limits: Arc<dyn HardLimits>,
    pub judge: Arc<dyn Approve>,
    pub autonomy: Arc<dyn Autonomy>,
    pub clock: Arc<dyn Clock>,
}

/// Your judge, or two. Anything that goes wrong comes back as `AskYou`, never as an
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
    pub judge: Option<ModelRef>,
    pub reasons: Vec<String>,
}

/// Earned autonomy, spot checks, `auto-off`, and the daily caps, kept in the state directory
/// and `~/.argonauts`.
pub trait Autonomy: Send + Sync {
    /// `configured`, lowered to supervised by `auto-off`, and from auto to auto preview for a kind
    /// that lost its autonomy. Autopilot is never lowered to auto preview. Never more autonomous
    /// than `configured`.
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
    /// What auto would have decided. It's scored when your answer for the same approval
    /// arrives, in either order. `evidence` holds the rules document the judge read.
    fn note_auto_preview(
        &self,
        point: &DecisionPoint,
        auto_preview: &AutoPreviewRecord,
        evidence: &EvidencePacket,
        settings: &Settings,
    );
    /// Scores the auto preview record, and gives back a reservation your answer beat.
    fn user_decided(&self, decision: &UserDecision, settings: &Settings);
    /// A wrong auto-approval demotes its kind. A wrong approval you approved yourself, such
    /// as a reverted merge, turns its match into a miss. Reporting it again changes nothing.
    fn spot_checked(&self, point: &DecisionPoint, correct: bool, settings: &Settings);
    /// Landing what auto approved at `point` was refused: the auto-merge it took goes back.
    fn landing_refused(&self, point: &DecisionPoint) {
        let _ = point;
    }
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
    /// Your answer for this approval came first, so auto's outcome is only an auto preview record.
    AlreadyDecided,
    /// A daily cap is used up.
    Capped(Vec<Violation>),
    /// Auto can't approve this kind right now, such as after a wrong spot check or with too
    /// many spot checks waiting, so the verdict is only an auto preview record.
    Paused(String),
    /// The records couldn't be read or written, so auto asks instead.
    Unavailable(String),
}
