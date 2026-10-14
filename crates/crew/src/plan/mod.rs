//! Planning: blind sealed drafts, a quote-checked map, critique of disputed topics, revision,
//! experiments, cross-judging aggregated in code, and decision records. The fast path merges drafts
//! when nothing is disputed.

use argonauts_core::names::RoleName;
use argonauts_core::settings::protocol::PlanningBy;
use argonauts_core::workflow::StandardPhase;

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::orchestrated;
use crate::review;
use crate::workflow::{StepCall, StepDone};

pub mod context;
pub mod critique;
pub mod decisions;
pub mod drafting;
pub mod drafts;
pub mod experiments;
pub mod fast_path;
pub mod judge;
pub mod judging;
pub mod map;
pub mod scan;
mod together;

pub use context::part;

/// `draft`: blind drafts from every drafting agent, sealed. In a later round of a loop, each
/// resumes its own session with the critique addressed to it (revision). With role
/// `orchestrator`, the orchestrator's plan (`orchestrated::run`). A step that names no role
/// follows `drafting.by`; one that names a role is drafted that way whatever it says.
pub async fn draft_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let orchestrator = match call.step.role {
        Some(role) => role == RoleName::Orchestrator,
        None => host.settings().drafting.by == PlanningBy::Orchestrator,
    };
    if orchestrator {
        orchestrated::run(host, call).await
    } else {
        drafting::run(host, call).await
    }
}

/// `map`: a fresh mapping agent over the sealed texts under anonymous labels, every quote checked.
pub async fn map_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    map::run(host, call).await
}

/// `critique`: critique of the disputed topics with the fixed checklist, one designated critic
/// arguing against the leading plan. Outside planning, it tries to disprove the input's claim.
pub async fn critique_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    critique::run(host, call).await
}

/// `synthesize`: the fast path, one agent merging the drafts and a judge checking completeness;
/// outside planning, one agent combining the input's results.
pub async fn synthesize_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    fast_path::merge(host, call).await
}

/// `judge`: in a plan phase, experiments first when a proposal fits, then scoring by the step's
/// `method` param (else `judging.method`), then decision records. In any other phase it rules on
/// reviewers who disagree (`review::settle`).
pub async fn judge_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    if call.standard == StandardPhase::Plan {
        judge::run(host, call).await
    } else {
        review::settle::step(host, call).await
    }
}
