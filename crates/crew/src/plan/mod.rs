//! Planning: blind sealed drafts, a quote-checked map, critique of disputed topics, revision,
//! spikes, cross-judging aggregated in code, and decision records. The fast path merges drafts
//! when nothing is disputed.

use argonauts_core::names::RoleName;
use argonauts_core::settings::protocol::PlanningBy;

use crate::coordinated;
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::review;
use crate::workflow::{StepCall, StepDone};

pub mod context;
pub mod critique;
pub mod decisions;
pub mod drafting;
pub mod drafts;
pub mod fast_path;
pub mod judge;
pub mod judging;
pub mod map;
pub mod scan;
pub mod spikes;

pub use context::part;

/// `draft`: blind drafts from every drafter, sealed. In a later round of a loop, each drafter
/// resumes its own session with the critique addressed to it (revision). With role
/// `coordinator`, the coordinated crew's plan (`coordinated::run`). A step that names no role
/// follows `drafting.by`; one that names a role is drafted that way whatever it says.
pub async fn draft_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let coordinator = match call.step.role {
        Some(role) => role == RoleName::Coordinator,
        None => host.settings().drafting.by == PlanningBy::Coordinator,
    };
    if coordinator {
        coordinated::run(host, call).await
    } else {
        drafting::run(host, call).await
    }
}

/// `map`: a fresh mapper over the sealed texts under anonymous labels, every quote checked.
pub async fn map_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    map::run(host, call).await
}

/// `refute`: critique of the disputed topics with the fixed checklist, one designated critic
/// arguing against the leading plan. Outside planning, it tries to disprove the input's claim.
pub async fn refute_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    critique::run(host, call).await
}

/// `synthesize`: the fast path, one agent merging the drafts and a judge checking completeness;
/// outside planning, one agent combining the input's results.
pub async fn synthesize_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    fast_path::merge(host, call).await
}

/// `judge`: spikes first when a proposal fits, then scoring by the step's `method` param (else
/// `judging.method`), then decision records. With role `adjudicator` it rules on disagreeing reviews (`review::adjudicate`).
pub async fn judge_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    if call.step.role == Some(RoleName::Adjudicator) {
        review::adjudicate::step(host, call).await
    } else {
        judge::run(host, call).await
    }
}
