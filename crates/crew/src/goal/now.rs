//! `c` on an agent's goal: its checks run now, beside the try under way, in the task's own
//! worktree. What they find shows as the goal's last check and decides nothing. Only a try's
//! own check, which runs once its agent reports, sends the work round again or ends the goal,
//! so an answer taken while the work is half written never ends a goal early or costs it a
//! try.
//!
//! The reply says the checks began. They run beside the loop, hold the run from waiting the way
//! a session at work does, and are dropped when the run pauses or stops.

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::goal::{GoalChecked, GoalEvent, GoalRun, GoalVerdict};
use argonauts_core::workflows::goal::GoalSpec;

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::engine::shared::{Admission, lock};
use crate::sessions::labels;
use crate::state::RunState;
use crate::task::support::worker_of;
use crate::workflow::StepAt;

use super::check::{self, Place};
use super::pursue;

/// Starts the checks of `task`'s goal and returns once they're under way. `Err` says why they
/// can't run now.
pub fn start(host: &Host, task: Option<&TaskId>) -> Result<(), String> {
    let Some(task) = task else {
        return Err(
            "a workflow's goal is checked when its round ends. Open one of its agents to run \
             that agent's own checks now"
                .to_owned(),
        );
    };
    if host.pausing() {
        return Err("the run is pausing or stopping, so nothing new starts".to_owned());
    }
    let asked = host.read(|state| asked(host, state, task))?;
    let Some(running) = Running::begin(host, task) else {
        return Err("its checks are already running".to_owned());
    };
    host.record(
        Some(task),
        None,
        RunEventKind::Goal(GoalEvent::CheckingNow {
            task: Some(task.clone()),
            try_number: asked.place.try_number,
        }),
    )
    .map_err(|err| err.to_string())?;
    let beside = host.clone();
    host.shared().tasks.spawn(async move {
        let halt = beside.shared().halt.clone();
        let found = tokio::select! {
            found = check::unrecorded(&beside, &asked.goal, &asked.place) => found,
            () = closing(&beside) => Err(EngineError::Interrupted),
            // Once the loop halts nothing more is recorded.
            () = halt.cancelled() => return,
        };
        // Released before what it found is recorded: recording wakes the loop, which may wait
        // now that nothing holds it.
        drop(running);
        if let Err(err) = record(&beside, &asked, found) {
            tracing::warn!(%err, task = %asked.task, "a goal's check on ask wasn't recorded");
        }
    });
    Ok(())
}

/// One check you asked for: whose goal, and where it's checked.
struct Asked {
    task: TaskId,
    goal: GoalSpec,
    place: Place,
}

/// The goal `task` is worked as and where its work is, or why there's nothing to check.
fn asked(host: &Host, state: &RunState, task: &TaskId) -> Result<Asked, String> {
    let Some(goal) = state.goal.goal_of(Some(task)) else {
        return Err("that task isn't worked as a goal".to_owned());
    };
    let record = state
        .tasks
        .get(task)
        .ok_or_else(|| format!("there is no task {task}"))?;
    let dir = record
        .worktree
        .clone()
        .filter(|dir| dir.is_dir())
        .ok_or("its work hasn't started, so there's nothing to check yet")?;
    let place = Place {
        base: record.base.clone(),
        task: Some(task.clone()),
        at: step_of(state, task),
        authors: vec![worker_of(host, record)],
        ..Place::new(dir, try_under_way(goal))
    };
    Ok(Asked {
        task: task.clone(),
        goal: goal.spec(),
        place,
    })
}

fn try_under_way(goal: &GoalRun) -> u32 {
    goal.tries.last().map_or(1, |a_try| a_try.n)
}

/// The step `task` is built in, in its newest round, which places the judge beside its agent
/// in the workflow view.
fn step_of(state: &RunState, task: &TaskId) -> Option<StepAt> {
    let step = state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .find(|session| {
            session.task.as_ref() == Some(task) && session.started.access == Access::Build
        })?
        .started
        .workflow_step
        .clone()?;
    let round = state
        .workflows
        .by_id
        .get(&step.workflow)?
        .steps
        .iter()
        .filter(|progress| progress.step == step)
        .map(|progress| progress.round)
        .max()?;
    Some(StepAt { step, round })
}

/// Returns once the run stops taking new work: a pause, a stop, or a shutdown.
async fn closing(host: &Host) {
    let mut admission = host.shared().admission.subscribe();
    // A dropped sender means the run is gone, which closes it as well.
    let _ = admission
        .wait_for(|admission| *admission != Admission::Open)
        .await;
}

/// Records what the check found, with one line in the run's notes, or that it found nothing.
fn record(host: &Host, asked: &Asked, found: EngineResult<GoalChecked>) -> EngineResult<()> {
    let task = &asked.task;
    let (label, at_work) = host.read(|state| {
        (
            labels::task_label(state, task),
            super::task::pursued(state, task),
        )
    });
    match found {
        Ok(checked) => {
            let said = found_words(&label, &checked, at_work);
            host.record(
                Some(task),
                None,
                RunEventKind::Goal(GoalEvent::CheckedNow(Box::new(checked))),
            )?;
            pursue::note(host, said)
        }
        Err(err) => {
            let why = match err {
                EngineError::Interrupted => "the run paused or stopped".to_owned(),
                err => err.to_string(),
            };
            host.record(
                Some(task),
                None,
                RunEventKind::Goal(GoalEvent::CheckNowDropped {
                    task: Some(task.clone()),
                    why: why.clone(),
                }),
            )?;
            pursue::note(
                host,
                format!("The checks of {label}'s goal didn't finish: {why}"),
            )
        }
    }
}

/// `The goal of build:cli, checked now: 2 of 3 lines met · cargo test overdue: 1 of 12 fails`.
/// A goal that holds while its agent is still `at_work` says what happens next, since nothing
/// does until the agent reports.
fn found_words(label: &str, checked: &GoalChecked, at_work: bool) -> String {
    let total = checked.verdicts.len();
    let met = check::met_count(&checked.verdicts);
    let lines = if total == 1 { "line" } else { "lines" };
    if checked.overall == GoalVerdict::Met {
        let mut said = format!("The goal of {label} holds right now: {met} of {total} {lines} met");
        if at_work {
            said.push_str(" · the check after its agent reports decides");
        }
        return said;
    }
    let mut said = format!("The goal of {label}, checked now: {met} of {total} {lines} met");
    if let Some(first) = checked.reason.lines().find(|line| !line.trim().is_empty()) {
        said.push_str(" · ");
        said.push_str(first.trim());
    }
    said
}

/// Marks `task`'s goal as being checked on your ask until it's dropped.
struct Running {
    host: Host,
    task: TaskId,
}

impl Running {
    /// `None` when its checks already run.
    fn begin(host: &Host, task: &TaskId) -> Option<Self> {
        lock(&host.shared().checking_goals)
            .insert(task.clone())
            .then(|| Self {
                host: host.clone(),
                task: task.clone(),
            })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        lock(&self.host.shared().checking_goals).remove(&self.task);
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::goal::{CriterionState, CriterionVerdict};

    use super::*;

    fn checked(met: &[bool], reason: &str) -> GoalChecked {
        let verdicts: Vec<CriterionVerdict> = met
            .iter()
            .enumerate()
            .map(|(at, met)| CriterionVerdict {
                id: format!("G{}", at + 1),
                verdict: if *met {
                    CriterionState::Met
                } else {
                    CriterionState::NotYet
                },
                evidence: String::new(),
                missing: String::new(),
            })
            .collect();
        GoalChecked {
            try_number: 3,
            overall: if met.iter().all(|met| *met) {
                GoalVerdict::Met
            } else {
                GoalVerdict::NotYet
            },
            verdicts,
            reason: reason.to_owned(),
            task: None,
            checks: Vec::new(),
        }
    }

    #[test]
    fn the_note_says_where_the_goal_stands_and_what_still_fails() {
        let not_yet = checked(
            &[true, true, false],
            "every test in tests/overdue.rs passes: cargo test overdue: 1 of 12 fails\n",
        );
        assert_eq!(
            found_words("build:cli", &not_yet, true),
            "The goal of build:cli, checked now: 2 of 3 lines met · every test in \
             tests/overdue.rs passes: cargo test overdue: 1 of 12 fails"
        );
        // A goal that holds mid-try still waits for its try's own check.
        let holds = checked(&[true], "1 of 1 met");
        assert_eq!(
            found_words("build:cli", &holds, true),
            "The goal of build:cli holds right now: 1 of 1 line met · the check after its \
             agent reports decides"
        );
        assert_eq!(
            found_words("build:cli", &holds, false),
            "The goal of build:cli holds right now: 1 of 1 line met"
        );
    }
}
