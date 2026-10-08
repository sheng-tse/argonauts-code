//! Cards: the scheduler over the task graph, and each card's pipeline from its owner's session
//! through gates, review, and integration.

use argonauts_core::CardId;
use argonauts_core::PhaseStatus;
use argonauts_core::events::card::{CardEvent, CardSpec, CardState};
use argonauts_core::events::run::RunPhase;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::workflow::{Count, Step, StepKind};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::goal::pursue;
use crate::workflow::runner;
use crate::workflow::{Flow, StepCall, StepDone};
use crate::{roles, solo};

pub mod gates;
pub mod machine;
pub mod pipeline;
pub mod prompt;
pub mod recovery;
pub mod scheduler;
pub mod takeover;
pub mod worktree;

mod asked_anyway;
mod budget;
pub(crate) mod claims;
mod contracts;
mod escalation;
pub(crate) mod session;
pub(crate) mod support;

/// The step input that asks for spike cards, which the judge step runs itself.
pub const SPIKES: &str = "spikes";

/// The step input that builds the goal as one solo card, owned by the default agent.
pub const GOAL: &str = "goal";

/// The `different_family_from` value that makes a solo card's review come from another model
/// family than its owner.
pub const AUTHOR: &str = "author";

/// `build`: with `input: goal`, the solo card (`count` independent attempts of it when above
/// 1, reviewed by another family with `different_family_from: author`); with `input: plan`,
/// every card of the approved graph through its pipeline, in dependency order, within
/// `concurrency` and budget. `input: spikes` is left to the judge step.
pub async fn build_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    match call.step.input.as_deref() {
        Some(SPIKES) => Ok(StepDone::skipped(
            "experiment tasks run in the judge step and are never merged into the run",
        )),
        Some(GOAL) => build_goal(host, call).await,
        _ => scheduler::run_cards(host, call).await,
    }
}

/// Whether a step builds the solo card rather than the plan's cards.
pub fn builds_goal(step: &Step) -> bool {
    step.kind == StepKind::Build && step.input.as_deref() == Some(GOAL)
}

async fn build_goal(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    // With a goal around the run, each round builds its own task on what the earlier ones
    // merged.
    let mut spec = {
        let snapshot = host.read(Clone::clone);
        if snapshot.goal.pursuit.is_some() {
            pursue::attempt_card(&snapshot, host)?
        } else {
            solo::card(&snapshot, host)?
        }
    };
    // `solo.review` may already have given the card its reviewer, and a workflow never takes
    // a review away.
    if call.step.different_family_from.as_deref() == Some(AUTHOR) && spec.reviewers.is_empty() {
        let owner = spec
            .owner
            .clone()
            .unwrap_or_else(|| host.settings().main_agent.clone());
        spec.reviewers = vec![other_provider_reviewer(host, &owner)?];
    }
    let attempts = match call.step.count {
        Some(Count::Fixed(count)) => count.max(1),
        _ => 1,
    };
    if attempts == 1 {
        create(host, spec)?;
        return scheduler::run_cards(host, call).await;
    }
    best_of(host, call, spec, attempts).await
}

/// The first reviewer candidate (the reviewer role's, else every available worker) from another
/// family than the owner, with no role conflict.
fn other_provider_reviewer(host: &Host, owner: &WorkerRef) -> EngineResult<WorkerRef> {
    let family = |worker: &WorkerRef| roles::family(host, worker);
    let owner_family = family(owner);
    let mut pool = roles::candidates(host.settings(), RoleName::Reviewer);
    pool.extend(host.services().workers.available());
    pool.into_iter()
        .find(|worker| {
            worker != owner
                && family(worker) != owner_family
                && roles::conflict(RoleName::Reviewer, worker, Some(owner), &family).is_none()
        })
        .ok_or_else(|| {
            EngineError::Refused(format!(
                "this workflow reviews the solo task by another provider than {owner} \
                 ({owner_family}), and no reviewer from another provider is available"
            ))
        })
}

/// Best of n: independent attempts, each its own card, run until their visible gates pass;
/// `solo::pick_attempt` keeps one, which goes on through review and integration. The others
/// stay on their branches, unmerged.
async fn best_of(
    host: &Host,
    call: &StepCall,
    spec: CardSpec,
    attempts: u32,
) -> EngineResult<StepDone> {
    let mut ids = Vec::new();
    for n in 1..=attempts {
        let id = CardId::new(format!("{}-attempt-{n}", spec.id))
            .map_err(|err| EngineError::Refused(err.to_string()))?;
        let attempt = CardSpec {
            id: id.clone(),
            title: format!("{} (attempt {n})", spec.title),
            ..spec.clone()
        };
        create(host, attempt)?;
        ids.push(id);
    }
    contracts::record_phase(host, RunPhase::Build, PhaseStatus::Started, None)?;
    let runs = ids
        .iter()
        .map(|id| pipeline::drive(host, id, &call.at, pipeline::Until::GatesPassed));
    for result in futures::future::join_all(runs).await {
        result?;
    }
    let snapshot = host.read(Clone::clone);
    let Some(kept) = solo::pick_attempt(&snapshot, &ids) else {
        if solo::escalates_to_crew(&snapshot, host) {
            return Ok(StepDone {
                flow: Flow::Switch(runner::crew_workflow(host)),
                ..StepDone::done("no attempt passed its checks, so the crew takes over")
            });
        }
        let summary = format!("none of {attempts} attempts passed its checks");
        contracts::record_phase(
            host,
            RunPhase::Build,
            PhaseStatus::Failed,
            Some(summary.clone()),
        )?;
        return Ok(StepDone {
            end: argonauts_core::events::workflow::StepEnd::Failed,
            ..StepDone::done(summary)
        });
    };
    let state = pipeline::run_card(host, &kept, &call.at).await?;
    let summary = match state {
        CardState::Integrated => format!("kept {kept} of {attempts} attempts, merged into the run"),
        other => format!("kept {kept} of {attempts} attempts, now {other:?}"),
    };
    contracts::record_phase(
        host,
        RunPhase::Build,
        PhaseStatus::Finished,
        Some(summary.clone()),
    )?;
    Ok(StepDone::done(summary))
}

/// Records the card once, as `Waiting`.
pub(crate) fn create(host: &Host, spec: CardSpec) -> EngineResult<()> {
    let id = spec.id.clone();
    if host.read(|state| state.cards.get(&id).is_some()) {
        return Ok(());
    }
    support::record_card(host, &id, CardEvent::Created(Box::new(spec)))?;
    let first = CardEvent::State {
        from: None,
        to: CardState::Waiting,
        reason: "created".to_owned(),
    };
    support::record_card(host, &id, first)?;
    Ok(())
}
