//! Whether a condition holds yet: a step's `until`, or a file's `goal`. This is the one door
//! the runner's checks go through, and behind it is the goal's own check (`goal::until`): the
//! same commands, judge and rules as any goal, recorded so the goal panels show them.

use std::path::Path;

use argonauts_core::TaskId;
use argonauts_runner::{Check, Checked};

use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::goal::until::{self, Asked};
use crate::workflow::StepCall;

/// Checks `check` against the work in `workdir`. `task` is the task the script's task is
/// worked on, and `max_rounds` what the file allows the step or the whole file.
pub async fn check(
    host: &Host,
    call: &StepCall,
    check: &Check,
    task: Option<&TaskId>,
    max_rounds: Option<u32>,
    workdir: &Path,
) -> EngineResult<Checked> {
    let asked = Asked {
        condition: &check.condition,
        // A file's goal reads its commands from beside `goal =` in the file in use.
        commands: &[],
        step: check.step.as_deref(),
        task,
        kept_by: check.kept_by.as_deref(),
        try_number: check.try_number,
        round: check.round,
        max_rounds,
    };
    let held = until::held(host, &call.at, &asked, workdir).await?;
    Ok(Checked {
        holds: held.holds,
        failed: held.failed,
        stop: held.stop,
    })
}

/// Before a writer's first turn at `task`, when its step keeps going `until` a condition: the
/// task is worked as that goal inside the writer's own turn.
pub fn work_as(
    host: &Host,
    until: &str,
    step: Option<&str>,
    task: &TaskId,
    kept_by: Option<&str>,
    max_rounds: Option<u32>,
) -> EngineResult<()> {
    let asked = Asked {
        condition: until,
        commands: &[],
        step,
        task: Some(task),
        kept_by,
        try_number: None,
        round: None,
        max_rounds,
    };
    until::work_as(host, &asked)
}
