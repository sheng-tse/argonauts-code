//! Deciding one checkpoint: hard limits first, then the mode and `decide_with`.

pub mod decide;
pub mod evaluate;
pub mod settings;
#[cfg(test)]
mod tests;

use argonauts_core::future::BoxFuture;
use argonauts_core::policy::{
    ApprovalPolicy, ApprovalsView, DecisionPoint, EvidencePacket, HardLimits, PolicyOutcome,
    UserDecision,
};
use argonauts_core::settings::approvals::ApprovalMode;

use crate::parts::PolicyParts;

pub(crate) use evaluate::in_full_auto;

pub struct ApprovalsPolicy {
    parts: PolicyParts,
}

impl ApprovalsPolicy {
    pub fn from_parts(parts: PolicyParts) -> Self {
        Self { parts }
    }

    pub fn parts(&self) -> &PolicyParts {
        &self.parts
    }
}

impl ApprovalPolicy for ApprovalsPolicy {
    fn evaluate<'a>(
        &'a self,
        point: &'a DecisionPoint,
        evidence: &'a EvidencePacket,
    ) -> BoxFuture<'a, PolicyOutcome> {
        Box::pin(evaluate::evaluate(&self.parts, point, evidence))
    }

    fn user_decided(&self, decision: &UserDecision) {
        let settings = self.parts.source.current();
        self.parts.autonomy.user_decided(decision, &settings);
    }

    fn spot_checked(&self, point: &DecisionPoint, correct: bool) {
        let settings = self.parts.source.current();
        self.parts.autonomy.spot_checked(point, correct, &settings);
    }

    fn effective_mode(&self, point: &DecisionPoint, requested: ApprovalMode) -> ApprovalMode {
        settings::less_autonomous(
            requested,
            self.parts.autonomy.effective_mode(point, requested),
        )
    }

    fn landing_refused(&self, point: &DecisionPoint) {
        self.parts.autonomy.landing_refused(point);
    }

    fn auto_off(&self) {
        self.parts.autonomy.auto_off();
    }

    fn hard_limits(&self) -> &dyn HardLimits {
        &*self.parts.limits
    }

    fn view(&self) -> ApprovalsView {
        let settings = self.parts.source.current();
        self.parts.autonomy.view(&settings)
    }
}
