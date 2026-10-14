//! Understand: readings in parallel, comparers comparing them, materiality rules in code,
//! the goal file, and the Understand approval. Until the goal is approved only read-only and
//! no-tools sessions start; once approved it's frozen by its hash.

use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::run::RunMode;
use argonauts_core::names::ModelRef;
use argonauts_core::steps::understand::{CompareOutput, Reading};

use crate::approvals;
use crate::captain;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::workflow::{StepCall, StepDone};

pub mod approval;
pub mod changes;
pub mod compare;
pub mod context;
pub mod draft;
pub mod file;
pub mod main_agent;
pub mod materiality;
pub mod read;
pub mod round;

/// `read`: every reader (every crew member unless `understand.readers`) in parallel, fresh,
/// read-only, in the base worktree. In Captain it records the prompt as the goal instead, unless
/// `captain.understand_first`.
pub async fn read_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
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
    // Recorded, not read back: no agent reads it, so the phase reads as skipped.
    if mode == RunMode::Captain && !host.settings().captain.understand_first {
        captain::goal_from_prompt(host).await?;
        return Ok(StepDone::skipped("Your prompt is the goal"));
    }
    round::begin(host)?;

    let readings = read::read(host, call, guidance.as_deref()).await?;
    if mode == RunMode::Captain {
        // The main agent's one reading is the draft; there is nothing to compare.
        settle_alone(host, &readings, 1).await?;
        return Ok(StepDone::done("The main agent read the request back"));
    }
    Ok(StepDone::done(match readings.len() {
        1 => "1 agent read the request back".to_owned(),
        n => format!("{n} agents read the request back"),
    }))
}

/// `compare`: `understand.comparers` comparers from different providers compare the labeled
/// readings; code settles the differences and drafts the goal file.
pub async fn compare_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let (approved, settled, readings, mode) = host.read(|state| {
        (
            state.goal.approved.is_some(),
            state.goal.settled.is_some() && !round::sent_back(state),
            round::readings(state),
            round::mode(state),
        )
    });
    if approved {
        return Ok(StepDone::done("The goal is already approved"));
    }
    let enough = if mode == RunMode::Captain { 1 } else { 2 };
    if settled {
        // A re-entered step, or Captain's reading settled the round already.
        settle_alone(host, &readings, enough).await?;
        return Ok(StepDone::done("The differences are settled"));
    }
    if readings.len() < enough {
        settle_alone(host, &readings, enough).await?;
        return Ok(StepDone::done(
            "Too few readings to compare, so your prompt is the draft",
        ));
    }
    if mode == RunMode::Captain {
        settle_alone(host, &readings, enough).await?;
        return Ok(StepDone::done("One reading, nothing to compare"));
    }

    let listings = compare::compare(host, call).await?;
    let outputs: Vec<CompareOutput> = listings.into_iter().map(|(_, output)| output).collect();
    let autopilot = approvals::autopilot::applies_now(host, ApprovalKind::Understand);
    let understand = materiality::understand_settings(host.settings(), autopilot);
    let settled = materiality::settle(&outputs, readings.len(), &understand);
    let request = host.read(round::request)?;
    round::settle_and_draft(host, host.run_id(), &request, &readings, &settled, enough).await?;
    Ok(StepDone::done(summary(outputs.len(), &settled)))
}

/// Settles the round with nothing to compare and drafts it; or, when a re-entered step finds
/// it settled, drafts it if the draft is missing.
async fn settle_alone(
    host: &Host,
    readings: &[(String, ModelRef, Reading)],
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
            round::draft_goal(host, run, &request, readings, &settled.assumptions, enough).await?;
        }
        None => {
            let nothing = materiality::Settled::default();
            round::settle_and_draft(host, run, &request, readings, &nothing, enough).await?;
        }
    }
    Ok(())
}

/// `approval: understand`: the questions as options, then approval, or the approval question itself
/// when nothing is left to ask.
pub async fn approval_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    approval::run(host, call).await
}

/// The approved goal's hash, which every build and experiment session must carry. Refused before
/// the goal is approved.
pub fn required_hash(state: &RunState) -> EngineResult<String> {
    state
        .goal
        .approved_sha256()
        .map(str::to_owned)
        .ok_or_else(|| {
            EngineError::Refused(
                "a build or experiment session needs the approved goal's hash, and the goal isn't \
                 approved yet"
                    .to_owned(),
            )
        })
}

fn summary(listings: usize, settled: &materiality::Settled) -> String {
    if listings == 0 {
        return "No comparer compared the readings, so the goal waits for you".to_owned();
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
