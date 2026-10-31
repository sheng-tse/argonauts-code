//! A condition a workflow's own steps keep: a step's `until`, or the `goal` at the top of its
//! file. The workflow's runner makes the tries and goes round, and comes here for the rest.
//! [`for_run`] gives a file that states no goal the one the run was started with, so `/goal`
//! and a request's own words go round a workflow as a file's `goal` does.
//! [`around`] sets a file's goal before the first round. [`work_as`] hands a step's `until` to
//! the task its writer is about to work, so the task is worked as that goal inside the
//! writer's own turn (see `task`). After each try or round, [`held`] says whether the
//! condition holds, what still fails for the next try, and whether to stop trying. A script
//! may also ask about a condition of its own: it is checked the same way and told the answer,
//! and nothing is kept.
//!
//! The answers come from the same check and the same rules as any goal: commands, the judge or
//! both, no fixed number of tries, and stuck once the same failure comes back
//! `goal.stuck_after` times running, with one change of approach before it stops. A task's
//! goal and a file's are recorded as every goal is, so the goal panels show them.
//!
//! A runner that never asks changes nothing about what the run owes: a round of the run's goal
//! that nothing checked gets its check before Merge (`pursue::begin_try`), and from there
//! the orchestrator plans the next round.

use std::path::Path;

use argonauts_core::TaskId;
use argonauts_core::base::names::RoleName;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::goal::{
    GoalChecked, GoalEvent, GoalRun, GoalStatus, GoalVerdict,
};
use argonauts_core::runs::events::run::{Party, RunMode};
use argonauts_core::workflows::goal::keep::{self, Limits, Next};
use argonauts_core::workflows::goal::{
    CheckMeans, CheckWith, GoalDefault, GoalFrom, GoalSpec, RoleGoal,
};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::merge::final_check::run_base;
use crate::workflow::{StepAt, runner};

use super::check::{self, Place};
use super::pursue;
use super::task::{self, AfterTry};

/// What a workflow's runner knows about the condition it asks about. Three things are asked:
/// a step's `until` (`step`, with `task` when its agent works on one), the `goal` at the top
/// of the file (`round`), and, with none of these, a condition a script asks about by itself.
#[derive(Debug, Clone, Copy)]
pub struct Asked<'a> {
    /// The condition as the file words it, with the task's name filled in.
    pub condition: &'a str,
    /// Commands that check the condition as a whole, `check = [...]`. Each must exit 0. Empty
    /// for a file's goal, they are read from beside `goal =` in the workflow file in use.
    pub commands: &'a [String],
    /// The step whose `until` it is.
    pub step: Option<&'a str>,
    /// The task whose work a try just changed, when the step works on one.
    pub task: Option<&'a TaskId>,
    /// `cli` or `argonauts`, when the step says who keeps it going.
    pub kept_by: Option<&'a str>,
    /// The try of the step this follows, from 1.
    pub try_number: Option<u32>,
    /// The round of the whole workflow this follows, from 1. Only the `goal` at the top of the
    /// file has one.
    pub round: Option<u32>,
    /// The most tries, when the file sets one.
    pub max_rounds: Option<u32>,
}

/// What the runner does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub holds: bool,
    /// What still fails, for the next try. Why it stopped, when `stop` is set.
    pub failed: String,
    /// Stop trying even though it doesn't hold: it's stuck, or you paused or cleared it.
    pub stop: bool,
}

impl Held {
    fn holds() -> Self {
        Self {
            holds: true,
            failed: String::new(),
            stop: false,
        }
    }

    fn not_yet(failed: String) -> Self {
        Self {
            holds: false,
            failed,
            stop: false,
        }
    }

    fn stopped(why: String) -> Self {
        Self {
            holds: false,
            failed: why,
            stop: true,
        }
    }
}

/// The condition the run was started with, for a workflow file that states no `goal` of its
/// own: `/goal <condition>` or the request's own words, else, under
/// `goal.default = "from-request"`, the request itself. `None` when the run has no goal, and
/// for a Captain run, whose goal is its one task's.
pub fn for_run(host: &Host) -> Option<String> {
    let settings = host.settings();
    host.read(|state| {
        if runner::run_mode(state) == Some(RunMode::Captain) {
            return None;
        }
        if let Some(goal) = &state.goal.pursuit {
            return Some(goal.objective.clone());
        }
        if let Some(goal) = pursue::configured(settings) {
            return Some(goal.condition);
        }
        let from_request = settings.goal.default == GoalDefault::FromRequest;
        Some(pursue::objective(state)).filter(|request| from_request && !request.trim().is_empty())
    })
}

/// Before a workflow's first round: the run takes its goal, with the workflow as round 1. The
/// goal it was started with comes first, as the request's own words beat a file's, then the
/// file's `goal`. A run that already has a goal keeps it.
pub fn around(host: &Host, at: &StepAt, asked: &Asked<'_>) -> EngineResult<()> {
    let has_one = host.read(|state| state.goal.pursuit.is_some());
    if has_one || pursue::take_goal(host, at.step.phase)? {
        return Ok(());
    }
    let spec = spec(host, asked, GoalFrom::WorkflowFile)?;
    pursue::around(host, &spec, at.step.phase)?;
    Ok(())
}

/// Before a writer's first turn at `asked.task`: the task is worked as the step's `until`,
/// kept by its CLI's own goal or by Argonauts Code as `kept_by` says. The same session then tries,
/// is checked and tries again inside that turn. A task that already has a goal keeps it.
pub fn work_as(host: &Host, asked: &Asked<'_>) -> EngineResult<()> {
    let Some(task) = asked.task else {
        return Ok(());
    };
    let spec = spec(host, asked, GoalFrom::Step)?;
    task::work_as(host, task, &spec, keeper(host, asked.kept_by)?)
}

/// After a try of a step, or a round of the whole workflow, whose work is in `workdir`:
/// whether the condition holds now, what still fails, and whether to stop trying.
pub async fn held(
    host: &Host,
    at: &StepAt,
    asked: &Asked<'_>,
    workdir: &Path,
) -> EngineResult<Held> {
    match (asked.step, asked.task, asked.round) {
        (Some(_), Some(task), _) => {
            work_as(host, asked)?;
            of_task(host, at, task).await
        }
        (None, None, Some(round)) => {
            around(host, at, asked)?;
            if host.read(|state| state.goal.pursuit.is_some()) {
                of_run(host, at, round, workdir).await
            } else {
                unrecorded(host, at, asked, workdir).await
            }
        }
        // A step that works on no task, or a condition a script asks about by itself: neither
        // is a goal of the run or of a task, whatever goals they have.
        _ => unrecorded(host, at, asked, workdir).await,
    }
}

/// A task's goal. Its writer's turn usually settled it already; a try nothing checked yet is
/// checked here by the same code.
async fn of_task(host: &Host, at: &StepAt, task: &TaskId) -> EngineResult<Held> {
    let status = host.read(|state| state.goal.goal_of(Some(task)).map(|goal| goal.status));
    match status {
        Some(GoalStatus::Met) => Ok(Held::holds()),
        Some(GoalStatus::Pursuing) => match task::after_try(host, task, at, true).await? {
            AfterTry::Holds => Ok(Held::holds()),
            AfterTry::Again(told) => Ok(Held::not_yet(told.text)),
            AfterTry::GaveUp => Ok(stopped(host, Some(task))),
        },
        _ => Ok(stopped(host, Some(task))),
    }
}

/// The run's goal, with the workflow's rounds as its tries.
async fn of_run(host: &Host, at: &StepAt, round: u32, workdir: &Path) -> EngineResult<Held> {
    let Some(goal) = host.read(|state| state.goal.pursuit.clone()) else {
        return Ok(stopped(host, None));
    };
    let newest = goal.tries.last().map_or(1, |a_try| a_try.n);
    let recorded = |round: u32| {
        host.read(|state| {
            state
                .goal
                .checks
                .iter()
                .rev()
                .find(|checked| checked.try_number == round && checked.task.is_none())
                .cloned()
        })
    };
    // A round the workflow asks about again, as when the run picked it up where it stopped:
    // what was found stands, and nothing is recorded twice.
    if round < newest
        && let Some(found) = recorded(round)
    {
        return Ok(found_again(&goal, &found));
    }
    match goal.status {
        GoalStatus::Met => return Ok(Held::holds()),
        GoalStatus::Pursuing | GoalStatus::Paused => {}
        _ => return Ok(stopped(host, None)),
    }
    let checked = match recorded(newest) {
        Some(found) => found,
        None => {
            let place = Place {
                base: run_base(host),
                at: Some(at.clone()),
                authors: host.read(|state| check::authors(state, newest)),
                ..Place::new(workdir, newest)
            };
            check::goal(host, &goal.spec(), &place).await?
        }
    };
    if checked.overall == GoalVerdict::Met {
        pursue::status_changed(host, GoalStatus::Met, Party::ArgonautsCode, None)?;
        return Ok(Held::holds());
    }
    // You may have paused or cleared it while the round ran: the round was checked, and no
    // other follows.
    let pursued = host.read(|state| {
        state
            .goal
            .pursuit
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Pursuing)
    });
    if !pursued {
        return Ok(stopped(host, None));
    }
    let settings = host.settings();
    let next = host.read(|state| {
        keep::next(
            &state.goal.checks_of(None),
            pursue::limits(state, settings),
            state.goal.approach_changed(None),
        )
    });
    let change = match next {
        Next::Met => return Ok(Held::holds()),
        Next::Stop { why } => {
            pursue::status_changed(
                host,
                GoalStatus::Stuck,
                Party::ArgonautsCode,
                Some(why.clone()),
            )?;
            pursue::note(host, format!("The goal is stuck: {why}"))?;
            return Ok(Held::stopped(why));
        }
        Next::Again => None,
        Next::ChangeApproach { why } => {
            host.record(
                None,
                None,
                RunEventKind::Goal(GoalEvent::ApproachChanged {
                    after: newest,
                    why: why.clone(),
                    task: None,
                }),
            )?;
            pursue::note(
                host,
                format!("The goal is stuck, {why}, so the next round takes another approach"),
            )?;
            Some(why)
        }
    };
    let round = newest + 1;
    pursue::try_started(host, round, format!("Round {round}"), at.step.phase)?;
    Ok(Held::not_yet(keep::send_back(
        &goal.criteria,
        &checked,
        change.as_deref(),
    )))
}

/// What an earlier round's check found, said again.
fn found_again(goal: &GoalRun, found: &GoalChecked) -> Held {
    if found.overall == GoalVerdict::Met {
        Held::holds()
    } else {
        Held::not_yet(keep::send_back(&goal.criteria, found, None))
    }
}

/// A step that works on no task, or a run that holds no goal of its own: the condition is
/// checked, and nothing is recorded, since there's no goal to record it under. With no record
/// to tell one failure from another, every failed try counts as the same failure.
async fn unrecorded(
    host: &Host,
    at: &StepAt,
    asked: &Asked<'_>,
    workdir: &Path,
) -> EngineResult<Held> {
    let spec = spec(host, asked, GoalFrom::Step)?;
    let try_number = asked.try_number.unwrap_or(1).max(1);
    let place = Place {
        base: run_base(host),
        at: Some(at.clone()),
        ..Place::new(workdir, try_number)
    };
    let checked = check::unrecorded(host, &spec, &place).await?;
    let limits = Limits {
        stuck_after: host.settings().goal.stuck_after,
        max_rounds: spec.max_rounds,
    };
    let next = match asked.step {
        Some(_) => with_no_record(&checked, try_number, limits),
        // A script that asks by itself counts its own tries, so it is only told what it asked.
        None if checked.overall == GoalVerdict::Met => Next::Met,
        None => Next::Again,
    };
    Ok(match next {
        Next::Met => Held::holds(),
        Next::Stop { why } => Held::stopped(why),
        Next::Again => Held::not_yet(keep::send_back(&spec.lines, &checked, None)),
        Next::ChangeApproach { why } => {
            Held::not_yet(keep::send_back(&spec.lines, &checked, Some(&why)))
        }
    })
}

/// What follows try `try_number` of a condition nothing records. It gets the tries a recorded goal
/// gets that fails the same way every time: `stuck_after` of them, then one more after a
/// change of approach.
fn with_no_record(checked: &GoalChecked, try_number: u32, limits: Limits) -> Next {
    match checked.overall {
        GoalVerdict::Met => return Next::Met,
        GoalVerdict::Impossible => {
            return Next::Stop {
                why: format!("the check says it can't be met: {}", checked.reason),
            };
        }
        GoalVerdict::NotYet => {}
    }
    let tries = |n: u32| if n == 1 { "try" } else { "tries" };
    if let Some(max) = limits.max_rounds
        && try_number >= max.max(1)
    {
        return Next::Stop {
            why: format!("it didn't hold after the {max} {} you allow", tries(max)),
        };
    }
    let same = limits.stuck_after.max(1);
    if try_number > same {
        Next::Stop {
            why: format!(
                "it didn't hold in {try_number} {}, after a change of approach",
                tries(try_number)
            ),
        }
    } else if try_number == same {
        Next::ChangeApproach {
            why: format!("{same} {} without it holding", tries(same)),
        }
    } else {
        Next::Again
    }
}

/// Why a goal that doesn't hold gets no further try.
fn stopped(host: &Host, task: Option<&TaskId>) -> Held {
    let (status, why) = host.read(|state| {
        let status = state.goal.goal_of(task).map(|goal| goal.status);
        let why = match task {
            Some(task) => state
                .goal
                .tasks
                .get(task)
                .and_then(|kept| kept.ended_because.clone()),
            None => state.goal.ended_because.clone(),
        };
        (status, why)
    });
    Held::stopped(why.unwrap_or_else(|| {
        match status {
            Some(GoalStatus::Paused) => "its goal is paused",
            Some(GoalStatus::Cleared) => "you cleared its goal",
            _ => "its goal stopped being kept",
        }
        .to_owned()
    }))
}

/// The condition as a goal, each line with the check `goal.check` gives it.
fn spec(host: &Host, asked: &Asked<'_>, from: GoalFrom) -> EngineResult<GoalSpec> {
    let settings = host.settings();
    let means = CheckMeans {
        project_checks: !settings.checks.run.is_empty(),
    };
    // A condition `goal.check = "command"` can't check still gets a check from a file: the
    // judge reads the lines no command names.
    let mut spec = GoalSpec::from_words(asked.condition, settings.goal.check, means, from)
        .or_else(|_| GoalSpec::from_words(asked.condition, CheckWith::Auto, means, from))
        .map_err(EngineError::Refused)?;
    spec.commands = match from {
        GoalFrom::WorkflowFile if asked.commands.is_empty() => {
            commands_in_file(host, asked.condition)
        }
        _ => asked.commands.to_vec(),
    };
    spec.max_rounds = asked.max_rounds.or(settings.goal.max_rounds);
    Ok(spec)
}

/// The commands the workflow file in use lists beside its `goal`. A runner's question names
/// the condition alone, so a file's `check = [...]` is read here when none came with it.
fn commands_in_file(host: &Host, condition: &str) -> Vec<String> {
    let name = host.settings().workflow.as_str();
    // A name, never a path out of the workflow folders.
    if Path::new(name).file_name().is_none_or(|file| file != name) {
        return Vec::new();
    }
    // A file that waits for your trust runs none of its commands.
    let waiting = crate::prompts::files::waiting(host);
    runner::workflow_dirs(host)
        .into_iter()
        .map(|dir| dir.join(format!("{name}.toml")))
        .filter(|path| !waiting.contains(path))
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|file| argonauts_core::workflows::goal::checks_beside(&file, condition))
        .unwrap_or_default()
}

/// Who a step asks to keep its `until`. A step that doesn't say follows `[roles.worker] goal`,
/// and is kept by Argonauts Code where that doesn't say either: writing `until` asked for a goal.
fn keeper(host: &Host, kept_by: Option<&str>) -> EngineResult<RoleGoal> {
    match kept_by.map(str::trim) {
        Some("cli") => Ok(RoleGoal::Cli),
        Some("argonauts") => Ok(RoleGoal::Argonauts),
        Some(other) => Err(EngineError::Refused(format!(
            "kept_by is cli or argonauts, not {other}"
        ))),
        None => {
            let role = host.settings().roles.get(&RoleName::Worker);
            Ok(match role.and_then(|role| role.goal) {
                Some(RoleGoal::Cli) => RoleGoal::Cli,
                _ => RoleGoal::Argonauts,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(overall: GoalVerdict) -> GoalChecked {
        GoalChecked {
            try_number: 1,
            verdicts: Vec::new(),
            overall,
            reason: "the API it needs is gone".to_owned(),
            task: None,
            checks: Vec::new(),
        }
    }

    /// A condition nothing records gets the tries a goal gets that fails the same way each
    /// time: three, then one more after a change of approach.
    #[test]
    fn an_unrecorded_condition_changes_approach_once_then_stops() {
        let limits = Limits {
            stuck_after: 3,
            max_rounds: None,
        };
        let not_yet = checked(GoalVerdict::NotYet);
        let after = |try_number| with_no_record(&not_yet, try_number, limits);
        assert_eq!(after(1), Next::Again);
        assert_eq!(after(2), Next::Again);
        assert_eq!(
            after(3),
            Next::ChangeApproach {
                why: "3 tries without it holding".to_owned()
            }
        );
        assert_eq!(
            after(4),
            Next::Stop {
                why: "it didn't hold in 4 tries, after a change of approach".to_owned()
            }
        );
        // It holds or can't be met whatever the try, and a cap you set comes first.
        assert_eq!(
            with_no_record(&checked(GoalVerdict::Met), 9, limits),
            Next::Met
        );
        assert!(matches!(
            with_no_record(&checked(GoalVerdict::Impossible), 1, limits),
            Next::Stop { why } if why.contains("can't be met")
        ));
        let capped = Limits {
            stuck_after: 3,
            max_rounds: Some(2),
        };
        assert_eq!(
            with_no_record(&not_yet, 2, capped),
            Next::Stop {
                why: "it didn't hold after the 2 tries you allow".to_owned()
            }
        );
    }
}
