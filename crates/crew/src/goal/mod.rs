//! Goal alignment: restatements in parallel, differs comparing them, materiality rules in code,
//! the goal file, and the goal checkpoint. Until the goal is approved only read-only and
//! no-tools sessions start; once approved it's frozen by its hash.

use argonauts_core::events::run::RunMode;
use argonauts_core::names::WorkerRef;
use argonauts_core::steps::goal::{DifferOutput, Restatement};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::solo;
use crate::state::RunState;
use crate::workflow::{StepCall, StepDone};

pub mod changes;
pub mod checkpoint;
pub mod context;
pub mod differ;
pub mod draft;
pub mod file;
pub mod materiality;
pub mod restate;
pub mod round;

/// `restate`: every restater (every crew member unless `goal.restaters`) in parallel, fresh,
/// read-only, in the base worktree. In solo it records the prompt as the goal instead, unless
/// `solo.restate_first`.
pub async fn restate_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let (approved, mode, guidance) = host.read(|state| {
        (
            state.goal.approved.is_some(),
            round::mode(state),
            round::guidance(state),
        )
    });
    if approved {
        return Ok(StepDone::done("The goal is already approved"));
    }
    if mode == RunMode::Solo && !host.settings().solo.restate_first {
        solo::goal_from_prompt(host).await?;
        return Ok(StepDone::done("Your prompt is the goal"));
    }
    round::begin(host)?;

    let restated = restate::restate(host, call, guidance.as_deref()).await?;
    if mode == RunMode::Solo {
        // The default agent's one restatement is the draft; there is nothing to compare.
        settle_alone(host, &restated, 1).await?;
        return Ok(StepDone::done("The default agent restated the goal"));
    }
    Ok(StepDone::done(match restated.len() {
        1 => "1 agent restated the goal".to_owned(),
        n => format!("{n} agents restated the goal"),
    }))
}

/// `differ`: `goal.differs` differs from different families compare the labeled restatements;
/// code settles the differences and drafts the goal file.
pub async fn differ_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let (approved, settled, restated, mode) = host.read(|state| {
        (
            state.goal.approved.is_some(),
            state.goal.settled.is_some() && !round::sent_back(state),
            round::restatements(state),
            round::mode(state),
        )
    });
    if approved {
        return Ok(StepDone::done("The goal is already approved"));
    }
    let enough = if mode == RunMode::Solo { 1 } else { 2 };
    if settled {
        // A re-entered step, or solo's restate settled the round already.
        settle_alone(host, &restated, enough).await?;
        return Ok(StepDone::done("The differences are settled"));
    }
    if restated.len() < enough {
        settle_alone(host, &restated, enough).await?;
        return Ok(StepDone::done(
            "Too few restatements to compare, so your prompt is the draft",
        ));
    }
    if mode == RunMode::Solo {
        settle_alone(host, &restated, enough).await?;
        return Ok(StepDone::done("One restatement, nothing to compare"));
    }

    let listings = differ::compare(host, call).await?;
    let outputs: Vec<DifferOutput> = listings.into_iter().map(|(_, output)| output).collect();
    let settled = materiality::settle(&outputs, restated.len(), &host.settings().goal);
    let request = host.read(round::request)?;
    round::settle_and_draft(host, host.run_id(), &request, &restated, &settled, enough).await?;
    Ok(StepDone::done(summary(outputs.len(), &settled)))
}

/// Settles the round with nothing to compare and drafts it; or, when a re-entered step finds
/// it settled, drafts it if the draft is missing.
async fn settle_alone(
    host: &Host,
    restated: &[(String, WorkerRef, Restatement)],
    enough: usize,
) -> EngineResult<()> {
    let (settled, drafted, request) = host.read(|state| {
        let fresh = !round::sent_back(state);
        (
            state.goal.settled.clone().filter(|_| fresh),
            state.goal.drafted.is_some() && fresh,
            round::request(state),
        )
    });
    let request = request?;
    let run = host.run_id();
    match settled {
        Some(_) if drafted => {}
        Some(settled) => {
            round::draft_goal(host, run, &request, restated, &settled.assumptions, enough).await?;
        }
        None => {
            let nothing = materiality::Settled::default();
            round::settle_and_draft(host, run, &request, restated, &nothing, enough).await?;
        }
    }
    Ok(())
}

/// `checkpoint: goal`: the questions as options, then approval, or the approval card itself
/// when nothing is left to ask.
pub async fn goal_checkpoint_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    checkpoint::run(host, call).await
}

/// The approved goal's hash, which every build and spike session must carry. Refused before
/// the goal is approved.
pub fn required_hash(state: &RunState) -> EngineResult<String> {
    state
        .goal
        .approved_sha256()
        .map(str::to_owned)
        .ok_or_else(|| {
            EngineError::Refused(
                "a build or spike session needs the approved goal's hash, and the goal isn't \
                 approved yet"
                    .to_owned(),
            )
        })
}

fn summary(listings: usize, settled: &materiality::Settled) -> String {
    if listings == 0 {
        return "No differ compared the restatements, so the goal waits for you".to_owned();
    }
    let count = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    format!(
        "{}, {}, {} dropped as wording",
        count(settled.questions.len(), "question", "questions"),
        count(settled.assumptions.len(), "assumption", "assumptions"),
        settled.dropped
    )
}
