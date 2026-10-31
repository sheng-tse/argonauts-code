//! A goal around a run's workflow: the workflow runs, the check decides whether the goal holds,
//! and when it doesn't the orchestrator plans the next round from what failed, added before
//! Merge, which waits at the end. A run takes its goal at the start (`/goal`, the request's
//! own words, `goal.default`), or on the way: when its workflow's own steps state one, when you
//! turn it into a goal, or when its final check finds a line unmet and it keeps going.
//!
//! Only code, from the check, marks a goal met or stuck. There's no fixed number of rounds:
//! the same failure `goal.stuck_after` rounds running changes approach once, and stuck again
//! the goal stops and says why. On autopilot it never asks.

use argonauts_core::TaskId;
use argonauts_core::base::names::{ModelRef, RoleName, WorkflowId};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, FreeText, GoalPacket,
};
use argonauts_core::runs::events::goal::{
    ChoiceOption, Criterion, Decider, GoalAction, GoalChecked, GoalEvent, GoalFile, GoalQuestion,
    GoalRun, GoalStatus, GoalVerdict, Try,
};
use argonauts_core::runs::events::run::{Party, RunMode, RunOutcome, WaitReason};
use argonauts_core::runs::events::task::{TaskKind, TaskSpec};
use argonauts_core::runs::events::workflow::{RunNote, StepEnd, WorkflowEvent};
use argonauts_core::safety::policy::EvidenceFacts;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::AtHardLimit;
use argonauts_core::settings::limit::LimitUnit;
use argonauts_core::workflows::goal::keep::{self, Limits, Next};
use argonauts_core::workflows::goal::{
    CheckMeans, CheckWith, GoalDefault, GoalFrom, GoalScope, GoalSpec, KeptBy,
};
use argonauts_core::workflows::steps::pursue::{PlanTry, PlanTryInput, TryPlan};
use argonauts_core::workflows::steps::understand::DiffTag;
use argonauts_core::workflows::workflow::{Phase, StandardPhase, StepKind, Workflow};

use crate::approvals::{self, ApprovalRequest};
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::prompts::goal::NEXT_TRY;
use crate::roles;
use crate::roles::captain;
use crate::state::RunState;
use crate::understand::{draft, file};
use crate::workflow::{Flow, StepCall, StepDone, change, runner, shape, yaml};

use super::check;

pub const KEEP_GOING: &str = "Keep going as a goal?";
pub const KEEP: &str = "Keep going as a goal";
pub const TO_THE_MERGE: &str = "Go on to the merge";
pub const STUCK: &str = "The goal is stuck. What now?";
pub const KEEP_TRYING: &str = "Keep trying";
pub const DELIVER: &str = "Deliver what holds";
pub const STOP: &str = "Stop and keep the branch";
pub const TURN_INTO_GOAL: &str = "Turn this run into a goal?";
pub const OVER_LIMIT: &str = "The goal is over its limit. What now?";
pub const CHECK_WHEN_IT_ENDS: &str = "Yes, and check it when this workflow ends";

/// Whether a goal goes round the run's workflow: one it started with (`goal.until`, or
/// `goal.default = "from-request"`), or one it took on the way. A Captain run's goal is its one
/// task's instead, kept in `task`.
pub fn pursuing(state: &RunState, settings: &Settings) -> bool {
    state.goal.pursuit.is_some()
        || runner::run_mode(state) == Some(RunMode::Crew)
            && (configured(settings).is_some() || settings.goal.default == GoalDefault::FromRequest)
}

/// The goal the settings give a run: `goal.until`, from whichever layer set it, which is the
/// conversation's for `/goal` and the request's for its own words.
pub fn configured(settings: &Settings) -> Option<GoalSpec> {
    let until = settings.goal.until.as_deref()?.trim();
    if until.is_empty() {
        return None;
    }
    let means = CheckMeans {
        project_checks: !settings.checks.run.is_empty(),
    };
    // A condition `command` can't check was refused where it was typed. From a file it still
    // gets a check: the judge reads the lines no command names.
    let mut goal = GoalSpec::from_words(until, settings.goal.check, means, GoalFrom::Setting)
        .or_else(|_| GoalSpec::from_words(until, CheckWith::Auto, means, GoalFrom::Setting))
        .ok()?;
    goal.max_rounds = settings.goal.max_rounds;
    Some(goal)
}

/// Whether the run's later phases are its goal's rounds, which the orchestrator plans from each
/// check rather than at phase ends: true once a round has been checked.
pub fn plans_its_rounds(state: &RunState) -> bool {
    state
        .goal
        .pursuit
        .as_ref()
        .is_some_and(|goal| goal.tries.iter().any(|a_try| a_try.verdict.is_some()))
}

/// The goal's lines: its own once it's set, else the ones the settings give, else the
/// done-when list the final check judges.
pub fn criteria(state: &RunState, settings: &Settings) -> Vec<Criterion> {
    spec(state, settings).lines
}

/// The done-when list before a goal pursues it: the main agent's, else the approved goal file's.
pub fn planned_criteria(state: &RunState) -> Vec<Criterion> {
    let brief = state
        .run
        .named
        .as_ref()
        .map(|named| named.done_when.clone())
        .unwrap_or_default();
    if !brief.is_empty() {
        return brief;
    }
    let Some(approved) = &state.goal.approved else {
        return Vec::new();
    };
    if approved.criteria.is_empty() && !approved.summary.trim().is_empty() {
        return vec![Criterion::new("G1", approved.summary.trim())];
    }
    approved.criteria.clone()
}

/// The condition in words: the goal's own, else the run's description.
pub fn objective(state: &RunState) -> String {
    if let Some(goal) = &state.goal.pursuit {
        return goal.objective.clone();
    }
    let started = state.run.started.as_ref();
    state
        .run
        .named
        .as_ref()
        .map(|named| named.description.clone())
        .or_else(|| started.map(|started| started.request.clone()))
        .unwrap_or_default()
}

/// The goal as the check takes it: the run's own once set, else `goal.until`, else the
/// done-when list, which under `goal.default = "from-request"` is the request's goal and
/// otherwise is read by the judge as the final check always was.
fn spec(state: &RunState, settings: &Settings) -> GoalSpec {
    if let Some(goal) = &state.goal.pursuit {
        return goal.spec();
    }
    if runner::run_mode(state) == Some(RunMode::Crew)
        && let Some(goal) = configured(settings)
    {
        return goal;
    }
    let from_request = settings.goal.default == GoalDefault::FromRequest;
    let mut goal = GoalSpec::from_lines(
        &objective(state),
        planned_criteria(state),
        if from_request {
            settings.goal.check
        } else {
            CheckWith::Judge
        },
        if from_request {
            GoalFrom::Request
        } else {
            GoalFrom::WorkflowFile
        },
    );
    if from_request {
        goal.max_rounds = settings.goal.max_rounds;
    }
    goal
}

/// When the run's goal stops being tried: its own cap, else `goal.max_rounds`.
pub(super) fn limits(state: &RunState, settings: &Settings) -> Limits {
    Limits {
        stuck_after: settings.goal.stuck_after,
        max_rounds: state
            .goal
            .pursuit
            .as_ref()
            .and_then(|goal| goal.max_rounds)
            .or(settings.goal.max_rounds),
    }
}

/// Before a phase of the run's workflow begins. A run that starts with a goal takes it here,
/// and its workflow is round 1. A phase the orchestrator added after a check is the next round.
///
/// Whatever shaped the workflow, a goal nothing has checked gets its check before Merge.
/// Returns true when that changed the phases ahead, this one maybe among them, so the runner
/// looks at them again.
pub fn begin_try(host: &Host, id: &WorkflowId, phase: u32) -> EngineResult<bool> {
    let root = host.read(|state| {
        state
            .workflows
            .by_id
            .get(id)
            .is_some_and(|record| record.parent.is_none())
    });
    if !root {
        return Ok(false);
    }
    let taken = take_goal(host, phase)?;
    if check_before_merge(host, id, phase)? {
        return Ok(true);
    }
    if taken {
        return Ok(false);
    }
    let Some((title, n)) = host.read(|state| {
        let goal = state.goal.pursuit.as_ref()?;
        // Until round 1 is checked every phase is the workflow's own, its Check included.
        if !plans_its_rounds(state) {
            return None;
        }
        let definition = state.workflows.by_id.get(id)?.phase_definition(phase)?;
        if !shape::is_try(definition) {
            return None;
        }
        if goal.tries.iter().any(|a_try| a_try.phase == phase) {
            return None;
        }
        let n = u32::try_from(goal.tries.len()).unwrap_or(u32::MAX) + 1;
        Some((definition.name.clone(), n))
    }) else {
        return Ok(false);
    };
    try_started(host, n, title, phase)?;
    Ok(false)
}

const CHECKED_BEFORE_MERGE: &str = "The run has a goal, so its check comes before Merge";
const TURNED_INTO_GOAL: &str =
    "You turned the run into a goal, so a check judges the workflow before Merge";

/// Why the run owes a check before Merge: its goal is still kept and nothing has checked the
/// newest round, or you turned the workflow into a goal and nothing has judged it yet.
fn owes_a_check(state: &RunState) -> Option<&'static str> {
    let judged = |round: Option<u32>| {
        state.goal.checks.iter().any(|checked| {
            checked.task.is_none() && round.is_none_or(|round| checked.try_number == round)
        })
    };
    let Some(goal) = &state.goal.pursuit else {
        return (turned_into_goal(state) && !judged(None)).then_some(TURNED_INTO_GOAL);
    };
    let kept = matches!(goal.status, GoalStatus::Pursuing | GoalStatus::Paused);
    let newest = goal.tries.last().map_or(1, |a_try| a_try.n);
    (kept && !judged(Some(newest))).then_some(CHECKED_BEFORE_MERGE)
}

/// As phase `phase` of the run's workflow is about to begin: puts the check the run owes
/// before Merge, unless a phase from here on has it. The usual workflow has. One with
/// `workflows.final_check` off that you turn into a goal doesn't, and neither does one whose
/// own steps make the rounds: they may never ask whether the goal holds, and only code may say
/// that it does. Returns whether the phases changed.
fn check_before_merge(host: &Host, id: &WorkflowId, phase: u32) -> EngineResult<bool> {
    let Some((workflow, why)) = host.read(|state| {
        let why = owes_a_check(state)?;
        let record = state.workflows.by_id.get(id)?;
        // A change recorded while a phase runs counts that phase as finished from then on, so
        // a run picked up in the middle of one waits for the next phase to begin.
        (!record.phase_started(phase)).then(|| (record.workflow.clone(), why))
    }) else {
        return Ok(false);
    };
    let ahead = workflow.phases.iter().skip(phase as usize);
    if ahead
        .clone()
        .any(|phase| shape::has_kind(&phase.steps, StepKind::GoalCheck))
    {
        return Ok(false);
    }
    let Some(merge) = ahead
        .clone()
        .position(|phase| shape::has_kind(&phase.steps, StepKind::Merge))
    else {
        return Ok(false);
    };
    let mut proposed = workflow.clone();
    proposed
        .phases
        .insert(phase as usize + merge, shape::check_phase());
    change::record(host, id, proposed, None, why.to_owned())?;
    Ok(true)
}

/// Sets the goal the run starts with, once its lines are known, with the workflow as round 1
/// from `phase`. Returns whether it did.
pub(super) fn take_goal(host: &Host, phase: u32) -> EngineResult<bool> {
    let settings = host.settings();
    let Some((goal, title)) = host.read(|state| {
        if state.goal.pursuit.is_some() || !pursuing(state, settings) {
            return None;
        }
        let spec = spec(state, settings);
        if spec.lines.is_empty() {
            return None;
        }
        let goal = GoalRun::new(&spec, GoalScope::Workflow, KeptBy::Argonauts);
        Some((goal, workflow_title(state)))
    }) else {
        return Ok(false);
    };
    set(host, goal, title, phase)?;
    Ok(true)
}

/// A workflow file's `goal =`, with its `check = [...]` in `spec.commands`: the run takes
/// `spec` as its goal whatever the settings say, and the workflow is round 1 from `phase`. For
/// the workflow runner, before the workflow's first step. Returns false, and changes nothing,
/// for a run that already has a goal and for a Captain run, whose goal is its one task's.
pub fn around(host: &Host, spec: &GoalSpec, phase: u32) -> EngineResult<bool> {
    let Some(title) = host.read(|state| {
        let takes = state.goal.pursuit.is_none()
            && runner::run_mode(state) != Some(RunMode::Captain)
            && !spec.lines.is_empty();
        takes.then(|| workflow_title(state))
    }) else {
        return Ok(false);
    };
    let goal = GoalRun::new(spec, GoalScope::Workflow, KeptBy::Argonauts);
    set(host, goal, title, phase)?;
    Ok(true)
}

/// Records the run's goal, says so in one line, and starts round 1.
fn set(host: &Host, goal: GoalRun, title: String, phase: u32) -> EngineResult<()> {
    let stated = format!(
        "{} · {}",
        goal.spec().size_words(),
        goal.spec().checked_by()
    );
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Set(Box::new(goal))),
    )?;
    note(host, format!("Goal set · {stated}"))?;
    try_started(host, 1, title, phase)
}

pub(super) fn try_started(host: &Host, n: u32, title: String, phase: u32) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::TryStarted {
            n,
            title,
            phase,
            task: None,
        }),
    )?;
    Ok(())
}

/// The workflow's own name, as round 1 of a goal goes by it.
fn workflow_title(state: &RunState) -> String {
    state
        .run
        .named
        .as_ref()
        .map_or_else(|| "The workflow".to_owned(), |named| named.name.clone())
}

/// `check`: the goal's check after a round, or, in a workflow with no goal and
/// `workflows.final_check`, the done-when list judged before Merge, where a line left unmet asks
/// whether to keep going.
pub async fn check_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let settings = host.settings();
    if host.read(runner::run_mode) == Some(RunMode::Captain) {
        return Ok(StepDone::skipped("a Captain run's checks are its check"));
    }
    let goal_run = host.read(|state| pursuing(state, settings));
    if !goal_run && !settings.workflows.final_check && !host.read(turned_into_goal) {
        return Ok(StepDone::skipped("workflows.final_check is off"));
    }
    // A goal whose lines only the plan could give, as from-request's are, is set by now.
    if goal_run {
        take_goal(host, call.at.step.phase)?;
    }
    let (goal, try_number) = host.read(|state| {
        let try_number = state
            .goal
            .pursuit
            .as_ref()
            .and_then(|goal| goal.tries.last())
            .map_or(1, |a_try| a_try.n);
        (spec(state, settings), try_number)
    });
    if goal.lines.is_empty() {
        return Ok(StepDone::skipped("there's no done-when list to check"));
    }
    // A check already recorded for this round stands, so a re-entered step never asks again.
    let recorded = host.read(|state| {
        state
            .goal
            .checks
            .iter()
            .rev()
            .find(|checked| checked.try_number == try_number && checked.task.is_none())
            .cloned()
    });
    let checked = match recorded {
        Some(checked) => checked,
        None => check::judge(host, call, try_number, &goal).await?,
    };
    if host.read(|state| state.goal.pursuit.is_some()) {
        after_try(host, call, &checked).await
    } else {
        after_final_check(host, call, &checked).await
    }
}

/// A round was checked: the goal holds, the next round, a change of approach, or it stops.
async fn after_try(host: &Host, call: &StepCall, checked: &GoalChecked) -> EngineResult<StepDone> {
    let (rounds, status, next) = host.read(|state| {
        let goal = state.goal.pursuit.as_ref();
        (
            goal.map_or(1, |goal| goal.tries.len()),
            goal.map(|goal| goal.status),
            keep::next(
                &state.goal.checks_of(None),
                limits(state, host.settings()),
                state.goal.approach_changed(None),
            ),
        )
    });
    let total = checked.verdicts.len();
    let met = check::met_count(&checked.verdicts);
    if checked.overall == GoalVerdict::Met {
        status_changed(host, GoalStatus::Met, Party::ArgonautsCode, None)?;
        let rounds = match rounds {
            1 => "1 round".to_owned(),
            n => format!("{n} rounds"),
        };
        return Ok(StepDone::done(format!(
            "Goal met · {total} of {total} · {rounds}"
        )));
    }
    if status == Some(GoalStatus::Cleared) {
        return Ok(StepDone::done(format!(
            "Goal cleared · {met} of {total} met · Merge takes what holds if every check passes"
        )));
    }
    if host.read(paused_by_you) {
        return Ok(StepDone::done(format!(
            "Goal paused · {met} of {total} met · Merge takes what holds if every check passes"
        )));
    }
    if let Some(spent) = host.read(over_limit) {
        return when_over_limit(host, call, checked, &spent).await;
    }
    match next {
        Next::Stop { why } => return when_stuck(host, call, checked, &why).await,
        Next::ChangeApproach { why } => {
            host.record(
                None,
                None,
                RunEventKind::Goal(GoalEvent::ApproachChanged {
                    after: checked.try_number,
                    why: why.clone(),
                    task: None,
                }),
            )?;
            note(
                host,
                format!("The goal is stuck, {why}, so the next round takes another approach"),
            )?;
        }
        Next::Again | Next::Met => {}
    }
    next_try(host, call, checked).await?;
    Ok(StepDone::done(format!("Not yet met · {met} of {total}")))
}

/// The run's limit is spent: `$0.61 of $0.50`. Only a limit in dollars is checked here; the
/// tasks' own limits stop them as they go.
fn over_limit(state: &RunState) -> Option<String> {
    let spend = state.run.spend.as_ref()?;
    let limit = spend.limit.as_ref()?;
    (limit.unit == LimitUnit::Usd && spend.spent.cost_usd >= limit.amount)
        .then(|| format!("${:.2} of ${:.2} spent", spend.spent.cost_usd, limit.amount))
}

/// A goal over its limit is a hard limit: it asks you whatever the mode, to deliver what holds or
/// stop.
async fn when_over_limit(
    host: &Host,
    call: &StepCall,
    checked: &GoalChecked,
    spent: &str,
) -> EngineResult<StepDone> {
    status_changed(
        host,
        GoalStatus::OverLimit,
        Party::ArgonautsCode,
        Some(spent.to_owned()),
    )?;
    let met = check::met_count(&checked.verdicts);
    let total = checked.verdicts.len();
    let why = format!("{met} of {total} met · {spent}");
    note(host, format!("The goal is over its limit: {spent}"))?;
    let answer = choose(
        host,
        call,
        OVER_LIMIT,
        Topic::HardLimit("limit"),
        &[DELIVER, STOP],
        &why,
    )
    .await?;
    if answer == STOP {
        return Ok(StepDone {
            end: StepEnd::Done,
            summary: Some(format!(
                "Goal over its limit · {met} of {total} met · stopped"
            )),
            flow: Flow::Finish(RunOutcome::BranchLeft),
        });
    }
    Ok(StepDone::done(format!(
        "Goal over its limit · {met} of {total} met · delivers what holds"
    )))
}

/// The goal stops short. On autopilot, or when the main agent answers for you, nobody is asked:
/// the goal ends, says why, and Merge takes what holds. Otherwise you choose.
async fn when_stuck(
    host: &Host,
    call: &StepCall,
    checked: &GoalChecked,
    why: &str,
) -> EngineResult<StepDone> {
    status_changed(
        host,
        GoalStatus::Stuck,
        Party::ArgonautsCode,
        Some(why.to_owned()),
    )?;
    note(host, format!("The goal is stuck: {why}"))?;
    let options = &[DELIVER, KEEP_TRYING, STOP];
    let answer = choose(host, call, STUCK, Topic::Crew("stuck goal"), options, why).await?;
    let met = check::met_count(&checked.verdicts);
    let total = checked.verdicts.len();
    match answer.as_str() {
        KEEP_TRYING => {
            status_changed(host, GoalStatus::Pursuing, Party::User, None)?;
            next_try(host, call, checked).await?;
            Ok(StepDone::done(format!(
                "Goal stuck · {met} of {total} met · one more round"
            )))
        }
        STOP => Ok(StepDone {
            end: StepEnd::Done,
            summary: Some(format!("Goal stuck · {met} of {total} met · stopped")),
            flow: Flow::Finish(RunOutcome::BranchLeft),
        }),
        _ => {
            let left: Vec<&str> = check::unmet(&checked.verdicts).collect();
            note(
                host,
                format!(
                    "The goal delivers what holds, {met} of {total} met; still open: {}",
                    left.join(", ")
                ),
            )?;
            Ok(StepDone::done(format!(
                "Goal stuck · {met} of {total} met · delivers what holds"
            )))
        }
    }
}

/// A workflow's final check: done when every criterion is met, else keep going as a goal, merge
/// what holds, or stop.
async fn after_final_check(
    host: &Host,
    call: &StepCall,
    checked: &GoalChecked,
) -> EngineResult<StepDone> {
    let total = checked.verdicts.len();
    let met = check::met_count(&checked.verdicts);
    if checked.overall == GoalVerdict::Met {
        return Ok(StepDone::done(format!("{met} of {total} met")));
    }
    let why = check::summary(&checked.verdicts);
    // You already said to turn the run into a goal, so nobody is asked again.
    let answer = if host.read(turned_into_goal) {
        KEEP.to_owned()
    } else {
        choose(
            host,
            call,
            KEEP_GOING,
            Topic::Crew("keep going"),
            &[KEEP, TO_THE_MERGE, STOP],
            &why,
        )
        .await?
    };
    match answer.as_str() {
        KEEP => {
            keep_going(host, call, checked)?;
            next_try(host, call, checked).await?;
            Ok(StepDone::done(format!(
                "{met} of {total} met · keeps going as a goal"
            )))
        }
        STOP => Ok(StepDone {
            end: StepEnd::Done,
            summary: Some(format!("{met} of {total} met · stopped")),
            flow: Flow::Finish(RunOutcome::BranchLeft),
        }),
        _ => Ok(StepDone::done(format!("{met} of {total} met"))),
    }
}

/// The workflow becomes try 1 of a goal with its done-when list, on the same branch.
fn keep_going(host: &Host, call: &StepCall, checked: &GoalChecked) -> EngineResult<()> {
    if host.read(|state| state.goal.pursuit.is_some()) {
        return Ok(());
    }
    let goal = host.read(|state| {
        let spec = spec(state, host.settings());
        let mut goal = GoalRun::new(&spec, GoalScope::Workflow, KeptBy::Argonauts);
        goal.max_rounds = goal.max_rounds.or(host.settings().goal.max_rounds);
        goal.tries = vec![Try {
            n: checked.try_number,
            title: workflow_title(state),
            phase: call.at.step.phase,
            verdict: Some(checked.overall),
        }];
        goal
    });
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Set(Box::new(goal))),
    )?;
    note(
        host,
        format!(
            "check:final found {}, so the run keeps going as a goal",
            check::summary(&checked.verdicts)
        ),
    )
}

/// The orchestrator plans the next round from what the check found failing, and it's added
/// before Merge. An answer that doesn't make a valid phase gives way to the usual round, with a
/// note.
async fn next_try(host: &Host, call: &StepCall, checked: &GoalChecked) -> EngineResult<()> {
    let id = call.at.step.workflow.clone();
    let Some((workflow, from, input)) = host.read(|state| {
        let record = state.workflows.by_id.get(&id)?;
        let goal = state.goal.pursuit.as_ref()?;
        let input = PlanTryInput {
            goal: goal.objective.clone(),
            criteria: goal.criteria.clone(),
            last_check: Some(argonauts_core::workflows::steps::pursue::GoalCheck {
                verdicts: checked.verdicts.clone(),
                overall: checked.overall,
                reason: checked.reason.clone(),
            }),
            tries: goal
                .tries
                .iter()
                .map(|a_try| {
                    let verdict = match a_try.verdict {
                        Some(GoalVerdict::Met) => "met",
                        Some(GoalVerdict::Impossible) => "can't be met",
                        Some(GoalVerdict::NotYet) => "not yet",
                        None => "not checked",
                    };
                    format!("{}. {}: {verdict}", a_try.n, a_try.title)
                })
                .collect(),
            change_approach: state.goal.stuck_at.contains(&checked.try_number).then(|| {
                "The same failure came back round after round. Plan this round another \
                     way: a different fix, or a different worker."
                    .to_owned()
            }),
        };
        Some((record.workflow.clone(), record.change_from(), input))
    }) else {
        return Err(EngineError::Refused(
            "there's no goal to plan a try for".to_owned(),
        ));
    };
    let n = u32::try_from(input.tries.len()).unwrap_or(u32::MAX) + 1;
    let planned = host
        .ask::<PlanTry>(Ask {
            slot: n,
            ..Ask::new(
                Some(call.at.clone()),
                RoleName::TieBreaker,
                change::tie_breaker(host),
                NEXT_TRY,
                input,
            )
        })
        .await;
    let mut problem = None;
    let phase = match planned {
        Ok(answered) => match try_from_plan(&answered.output, n) {
            Ok(phase) => phase,
            Err(why) => {
                problem = Some(why);
                shape::try_phase(n, name_of(&answered.output))
            }
        },
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(err) => {
            problem = Some(err.to_string());
            shape::try_phase(n, None)
        }
    };
    let phase = unique_name(&workflow, phase);
    let mut proposed = with_try(&workflow, from, phase.clone());
    if let Err(errors) = runner::check(host, &proposed, 0) {
        problem = Some(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        );
        proposed = with_try(
            &workflow,
            from,
            unique_name(&workflow, shape::try_phase(n, Some(&phase.name))),
        );
    }
    if let Some(problem) = problem {
        note(
            host,
            format!(
                "The main agent's plan for round {n} didn't hold, so it runs as the usual round: \
                 {problem}"
            ),
        )?;
    }
    let unmet: Vec<&str> = check::unmet(&checked.verdicts).collect();
    let reason = format!(
        "check:{} found {} met, so round {n} aims at {}",
        checked.try_number,
        check::met_count(&checked.verdicts),
        if unmet.is_empty() {
            "what the check said is missing".to_owned()
        } else {
            unmet.join(", ")
        }
    );
    change::record(host, &id, proposed, None, reason)
}

/// The workflow with the try's `phase` added at `at`, before Merge.
fn with_try(workflow: &Workflow, at: u32, phase: Phase) -> Workflow {
    let mut proposed = workflow.clone();
    let at = (at as usize).min(proposed.phases.len());
    proposed.phases.insert(at, phase);
    proposed
}

/// The first try the main agent planned with the goal, in its brief.
pub fn first_try(plan: &TryPlan) -> Result<Phase, String> {
    try_from_plan(plan, 1)
}

/// The main agent's try as a phase that ends with its check.
fn try_from_plan(plan: &TryPlan, n: u32) -> Result<Phase, String> {
    let steps = if plan.steps.trim().is_empty() {
        Vec::new()
    } else {
        yaml::parse_steps(&plan.steps)?
    };
    let usual = shape::try_phase(n, None);
    let steps = if steps
        .iter()
        .any(|step| !matches!(step.kind, StepKind::Merge | StepKind::GoalCheck))
    {
        steps
    } else {
        usual.steps
    };
    Ok(shape::with_check(Phase {
        name: name_of(plan).map_or(usual.name, str::to_owned),
        detail: match plan.detail.trim() {
            "" => usual.detail,
            detail => detail.to_owned(),
        },
        standard: StandardPhase::Do,
        steps,
    }))
}

fn name_of(plan: &TryPlan) -> Option<&str> {
    Some(plan.name.trim()).filter(|name| !name.is_empty() && name.len() <= 40)
}

/// A name no earlier phase has, so each try is its own row.
fn unique_name(workflow: &Workflow, mut phase: Phase) -> Phase {
    let taken = |name: &str| workflow.phases.iter().any(|phase| phase.name == name);
    if taken(&phase.name) {
        let base = phase.name.clone();
        let mut n = 2;
        while taken(&format!("{base} {n}")) {
            n += 1;
        }
        phase.name = format!("{base} {n}");
    }
    phase
}

/// The task round `n` builds when the orchestrator planned no tasks of its own: the whole goal,
/// aimed at what the last check found failing, on top of what earlier rounds merged. After a
/// change of approach, another provider's model takes it.
pub fn try_task(state: &RunState, host: &Host) -> EngineResult<TaskSpec> {
    let mut spec = captain::task(state, host)?;
    let Some(goal) = &state.goal.pursuit else {
        return Ok(spec);
    };
    let Some(last_try) = goal.tries.last() else {
        return Ok(spec);
    };
    let last = state.goal.checks_of(None).last().copied();
    let unmet: Vec<&Criterion> = goal
        .criteria
        .iter()
        .filter(|criterion| {
            last.is_none_or(|checked| check::unmet(&checked.verdicts).any(|id| id == criterion.id))
        })
        .collect();
    let mut task = format!(
        "{}\n\nThe goal: {}\n\nThis is round {}. Make these hold:\n",
        spec.task, goal.objective, last_try.n
    );
    for criterion in &unmet {
        task.push_str(&format!("- {}: {}\n", criterion.id, criterion.text));
    }
    if let Some(checked) = last {
        task.push_str(&format!("\nThe last check said: {}\n", checked.reason));
    }
    let changed = state.goal.stuck_at.contains(&last_try.n.saturating_sub(1));
    if changed {
        task.push_str(
            "\nThe same failure came back round after round. Take a different approach this \
             time.\n",
        );
    }
    spec.id = try_task_id(last_try.n);
    spec.title = last_try.title.clone();
    spec.task = task;
    spec.kind = TaskKind::Captain;
    spec.criteria = unmet.iter().map(|criterion| criterion.id.clone()).collect();
    if changed && let Some(other) = other_provider(host, spec.worker.as_ref()) {
        spec.worker = Some(other);
        spec.reviewers.clear();
    }
    Ok(spec)
}

pub fn try_task_id(n: u32) -> TaskId {
    TaskId::new(format!("try-{n}")).unwrap_or_else(|_| TaskId::numbered(n))
}

/// The first crew member from another provider than `worker` that can build. A model on a
/// server has no tools, so a round of building never goes to it.
fn other_provider(host: &Host, worker: Option<&ModelRef>) -> Option<ModelRef> {
    let worker = worker?;
    let provider = roles::provider(host, worker);
    let workers = &host.services().workers;
    host.settings()
        .crew
        .members()
        .into_iter()
        .filter(|model| model != worker && roles::provider(host, model) != provider)
        .find(|model| {
            workers
                .get(model)
                .is_ok_and(|runner| runner.capabilities().can_build)
        })
}

/// What a question is about: one the crew's answerer takes, or a hard limit, which comes to you
/// in every mode but autopilot.
#[derive(Debug, Clone, Copy)]
enum Topic<'a> {
    Crew(&'a str),
    HardLimit(&'a str),
}

fn topic_of(topic: &str, hard_limit: bool) -> Topic<'_> {
    if hard_limit {
        Topic::HardLimit(topic)
    } else {
        Topic::Crew(topic)
    }
}

/// Asks whoever answers the crew's questions to pick one of `options`, the recommended first. The
/// main agent takes the recommended one and says so; you get a choose-only question, unless the
/// topic is one `approvals.ask_you_about` keeps yours. In autopilot nothing waits for you: the main
/// agent answers, and a hard limit does what `approvals.understand.at_hard_limit` says, which stops
/// it unless that's `wait`.
async fn choose(
    host: &Host,
    call: &StepCall,
    question: &str,
    topic: Topic<'_>,
    options: &[&str],
    why: &str,
) -> EngineResult<String> {
    let recommended = options[0].to_owned();
    let (topic, hard_limit) = match topic {
        Topic::Crew(topic) => (topic, false),
        Topic::HardLimit(topic) => (topic, true),
    };
    let autopilot = approvals::autopilot::applies_now(host, ApprovalKind::Understand);
    if autopilot
        && hard_limit
        && approvals::configured(host.settings(), ApprovalKind::Understand).at_hard_limit
            != AtHardLimit::Wait
        && let Some(stop) = options.iter().find(|option| **option == STOP)
    {
        answered(host, question, stop, Decider::Rules)?;
        note(
            host,
            format!(
                "Autopilot stopped at the goal's {topic}, a hard limit, and kept the branch, as \
                 approvals.understand.at_hard_limit says"
            ),
        )?;
        return Ok((*stop).to_owned());
    }
    let yours = hard_limit || approvals::keeps_yours(host, ApprovalKind::Understand, &[topic]);
    if !yours && approvals::answerer(host, ApprovalKind::Understand).answers_for_you() {
        answered(host, question, &recommended, Decider::MainAgent)?;
        note(
            host,
            format!(
                "The main agent answered \"{question}\": {recommended}, the recommended answer"
            ),
        )?;
        return Ok(recommended);
    }
    // A run whose goal was never approved, as when your prompt was the goal, shows what it
    // pursues: the objective and the done-when list.
    let goal = host.read(|state| {
        state
            .goal
            .approved
            .clone()
            .unwrap_or_else(|| goal_so_far(state, host))
    });
    let choices: Vec<ChoiceOption> = options
        .iter()
        .enumerate()
        .map(|(index, label)| ChoiceOption {
            key: char::from(b'a' + u8::try_from(index).unwrap_or(25)),
            label: (*label).to_owned(),
            recommended: index == 0,
            basis: (index == 0).then(|| why.to_owned()),
        })
        .collect();
    let mut answers: Vec<AnswerOption> = choices
        .iter()
        .map(|choice| AnswerOption {
            key: choice.key,
            label: choice.label.clone(),
            action: AnswerAction::Choose {
                value: choice.label.clone(),
            },
            recommended: choice.recommended,
            detail: choice.basis.clone(),
        })
        .collect();
    answers.push(AnswerOption {
        key: 'd',
        label: "Show the last check".to_owned(),
        action: AnswerAction::ShowDiff,
        recommended: false,
        detail: None,
    });
    let request = ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Understand,
        title: question.to_owned(),
        task: None,
        packet: ApprovalPacket::Understand(GoalPacket {
            goal,
            agreements: Vec::new(),
            assumptions: Vec::new(),
            questions: vec![GoalQuestion {
                n: 1,
                of: 1,
                text: question.to_owned(),
                options: choices,
                tag: DiffTag::Scope,
                topic: Some(topic.to_owned()),
            }],
        }),
        facts: EvidenceFacts {
            open_questions: Some(1),
            ..EvidenceFacts::default()
        },
        agent_outputs: vec![why.to_owned()],
        documents: Vec::new(),
        options: Some(answers),
        free_text: FreeText::None,
        authors: Vec::new(),
    };
    let opening = if hard_limit {
        let because = format!("the {topic} is a hard limit, so it comes to you in every mode");
        approvals::open_yours(host, request, because)?
    } else if yours {
        let because = approvals::yours_because(&format!("{topic} questions"));
        approvals::open_yours(host, request, because)?
    } else {
        let because = "you answer the crew's questions".to_owned();
        approvals::open_because(host, request, Some(because))?
    };
    let decided = match opening.decided_or_withdrawn(host).await? {
        Ok(decided) => decided,
        // Autopilot came on while it waited for you, so it's answered as autopilot answers it.
        Err(left) if left == approvals::autopilot::LEFT_TO_THE_MAIN_AGENT => {
            return Box::pin(choose(
                host,
                call,
                question,
                topic_of(topic, hard_limit),
                options,
                why,
            ))
            .await;
        }
        Err(left) => {
            return Err(EngineError::Refused(format!(
                "\"{question}\" was withdrawn: {left}"
            )));
        }
    };
    let answer = match &decided.action {
        AnswerAction::Choose { value } if options.contains(&value.as_str()) => value.clone(),
        AnswerAction::Stop | AnswerAction::Abandon => STOP.to_owned(),
        _ => recommended,
    };
    answered(
        host,
        question,
        &answer,
        crate::understand::approval::decider(&decided.by),
    )?;
    Ok(answer)
}

/// The goal as a run pursues it before any goal file was approved: the objective and the
/// done-when list.
fn goal_so_far(state: &RunState, host: &Host) -> GoalFile {
    let mut goal = draft::from_prompt(host.run_id(), &objective(state));
    goal.criteria = criteria(state, host.settings());
    file::hashed(goal)
}

fn answered(host: &Host, question: &str, answer: &str, by: Decider) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Answered {
            question: question.to_owned(),
            answer: answer.to_owned(),
            by,
        }),
    )?;
    Ok(())
}

/// You said to turn the running workflow into a goal.
fn turned_into_goal(state: &RunState) -> bool {
    state
        .goal
        .answers
        .iter()
        .any(|(question, answer, _)| question == TURN_INTO_GOAL && answer == CHECK_WHEN_IT_ENDS)
}

/// What you do to the run's goal: clear it, turn the workflow into one, or change an
/// assumption. `Err` says why it can't be done now.
pub async fn act(host: &Host, action: GoalAction) -> Result<(), String> {
    let refused = |err: EngineError| err.to_string();
    match action {
        GoalAction::Clear { land } => {
            if !host.read(|state| state.goal.pursuit.is_some()) {
                return Err("no goal is being pursued in this run".to_owned());
            }
            status_changed(host, GoalStatus::Cleared, Party::User, None).map_err(refused)?;
            if land {
                note(
                    host,
                    "You stopped pursuing the goal; Merge takes what holds once every check \
                     passes"
                        .to_owned(),
                )
                .map_err(refused)
            } else {
                note(
                    host,
                    "You stopped pursuing the goal, and the run keeps its branch".to_owned(),
                )
                .map_err(refused)?;
                crate::engine::commands::stop(host).await.map_err(refused)
            }
        }
        GoalAction::EditLines { task, lines } => edit_lines(host, task.as_ref(), &lines),
        GoalAction::CheckNow { task } => super::now::start(host, task.as_ref()),
        GoalAction::Pause { task: Some(task) } => super::task::pause(host, &task).map_err(refused),
        GoalAction::Resume { task: Some(task) } => {
            super::task::resume(host, &task).map_err(refused)
        }
        GoalAction::Pause { task: None } => {
            let pursued = host.read(|state| {
                state
                    .goal
                    .pursuit
                    .as_ref()
                    .is_some_and(|goal| goal.status == GoalStatus::Pursuing)
            });
            if !pursued {
                return Err("no goal is being pursued in this run".to_owned());
            }
            status_changed(
                host,
                GoalStatus::Paused,
                Party::User,
                Some(YOU_PAUSED.to_owned()),
            )
            .map_err(refused)?;
            note(
                host,
                "You paused the goal: this round finishes and is checked, and no other follows \
                 until you resume it"
                    .to_owned(),
            )
            .map_err(refused)
        }
        GoalAction::Resume { task: None } => {
            if !host.read(paused_by_you) {
                return Err("this run's goal isn't paused".to_owned());
            }
            status_changed(host, GoalStatus::Pursuing, Party::User, None).map_err(refused)
        }
        GoalAction::TurnIntoGoal => {
            if host.read(|state| pursuing(state, host.settings())) {
                return Err("this run already pursues a goal".to_owned());
            }
            if host.read(turned_into_goal) {
                return Ok(());
            }
            // Its check goes in before Merge when the next phase begins.
            answered(host, TURN_INTO_GOAL, CHECK_WHEN_IT_ENDS, Decider::You).map_err(refused)
        }
        GoalAction::ChangeAssumption { id, to } => {
            let to = to.trim().to_owned();
            let Some(was) = host.read(|state| {
                let goal = state.goal.approved.as_ref()?;
                let index = usize::try_from(id).ok()?.checked_sub(1)?;
                goal.assumptions.get(index).map(|kept| kept.text.clone())
            }) else {
                return Err(format!("the goal has no assumption {id}"));
            };
            if to.is_empty() {
                return Err("say the reading the assumption takes".to_owned());
            }
            host.record(
                None,
                None,
                RunEventKind::Goal(GoalEvent::AssumptionChanged {
                    id,
                    to: to.clone(),
                    by: Decider::You,
                }),
            )
            .map_err(refused)?;
            // Working agents hear it now; the next try or phase end plans with it.
            crate::blackboard::commands::guidance(
                host,
                format!("You changed an assumption. It was: {was}. It is now: {to}."),
            )
            .await
            .map(|_| ())
        }
    }
}

/// `e` on a goal: its lines as you rewrote them, each with the check `goal.check` gives it.
/// The tries so far stay, and the next check reads the new lines.
fn edit_lines(host: &Host, task: Option<&TaskId>, lines: &[String]) -> Result<(), String> {
    let Some(mut goal) = host.read(|state| state.goal.goal_of(task).cloned()) else {
        return Err("there's no goal here to change".to_owned());
    };
    let settings = host.settings();
    let means = CheckMeans {
        project_checks: !settings.checks.run.is_empty(),
    };
    let from = goal.from.unwrap_or(GoalFrom::Command);
    let edited = GoalSpec::from_words(&lines.join("\n"), goal.check, means, from)?;
    goal.objective = edited.condition;
    goal.criteria = edited.lines;
    host.record(
        task,
        None,
        RunEventKind::Goal(GoalEvent::Set(Box::new(goal))),
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

/// The run waits: a goal being pursued is paused, by you or by a usage limit, until it goes on.
pub fn on_wait(host: &Host, reason: &WaitReason) -> EngineResult<()> {
    let by = match reason {
        WaitReason::Approval => return Ok(()),
        WaitReason::Paused => Party::User,
        WaitReason::AllAgentsOut { .. }
        | WaitReason::WaitingForAgent { .. }
        | WaitReason::NoFallback { .. } => Party::ArgonautsCode,
    };
    let pursuing = host.read(|state| {
        state
            .goal
            .pursuit
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Pursuing)
    });
    if pursuing {
        status_changed(host, GoalStatus::Paused, by, None)?;
    }
    Ok(())
}

/// The run goes on after a wait: a goal the wait paused is pursued again. One you paused
/// yourself waits for `/goal resume`.
pub fn on_resume(host: &Host) -> EngineResult<()> {
    let waiting = host.read(|state| {
        state
            .goal
            .pursuit
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Paused)
            && !paused_by_you(state)
    });
    if waiting {
        status_changed(host, GoalStatus::Pursuing, Party::User, None)?;
    }
    Ok(())
}

/// Why a goal you paused is paused, which tells it from one a waiting run paused.
pub const YOU_PAUSED: &str = "you paused it";

fn paused_by_you(state: &RunState) -> bool {
    state
        .goal
        .pursuit
        .as_ref()
        .is_some_and(|goal| goal.status == GoalStatus::Paused)
        && state.goal.ended_because.as_deref() == Some(YOU_PAUSED)
}

pub(super) fn status_changed(
    host: &Host,
    status: GoalStatus,
    by: Party,
    why: Option<String>,
) -> EngineResult<()> {
    let current = host.read(|state| state.goal.pursuit.as_ref().map(|goal| goal.status));
    if current.is_none() || current == Some(status) {
        return Ok(());
    }
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::StatusChanged {
            status,
            by,
            task: None,
            why,
        }),
    )?;
    Ok(())
}

/// A `›` note on why the plan moved, on the run's own workflow.
pub(super) fn note(host: &Host, text: String) -> EngineResult<()> {
    let workflow = host.read(|state| state.workflows.root.clone());
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Note(RunNote {
            text,
            workflow,
            left: false,
        })),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(name: &str, steps: &str) -> TryPlan {
        TryPlan {
            name: name.to_owned(),
            detail: "one worker fixes it".to_owned(),
            steps: steps.to_owned(),
        }
    }

    #[test]
    fn the_main_agents_try_ends_with_its_check_and_never_merges() {
        let steps = "- { kind: build, input: goal, label: \"fix:empty\" }\n- { kind: merge }\n";
        let phase = try_from_plan(&plan("Empty List", steps), 2).unwrap();
        assert_eq!(phase.name, "Empty List");
        let kinds: Vec<StepKind> = phase.steps.iter().map(|step| step.kind).collect();
        assert_eq!(kinds, [StepKind::Build, StepKind::GoalCheck]);
        assert!(shape::is_try(&phase));
    }

    #[test]
    fn an_empty_plan_runs_the_usual_try() {
        let phase = try_from_plan(&plan("", ""), 3).unwrap();
        assert_eq!(phase.name, "Try 3");
        assert_eq!(phase.steps[0].input.as_deref(), Some(crate::task::GOAL));
        assert!(try_from_plan(&plan("Fix", "- { kind: nonsense }"), 3).is_err());
    }

    #[test]
    fn a_goal_is_over_limit_once_its_dollars_are_spent() {
        use argonauts_core::models::meter::Spend;
        use argonauts_core::runs::events::agent_status::RunSpend;
        use argonauts_core::settings::limit::LimitAmount;
        let spend = |spent: f64| RunSpend {
            spent: Spend {
                cost_usd: spent,
                ..Spend::default()
            },
            limit: Some(LimitAmount {
                unit: LimitUnit::Usd,
                amount: 0.5,
            }),
            lines: Vec::new(),
            reserve_left: None,
        };
        let mut state = RunState::default();
        assert_eq!(over_limit(&state), None);
        state.run.spend = Some(spend(0.61));
        assert_eq!(over_limit(&state).as_deref(), Some("$0.61 of $0.50 spent"));
        state.run.spend = Some(spend(0.2));
        assert_eq!(over_limit(&state), None);
    }

    #[test]
    fn each_try_has_its_own_title() {
        let workflow = Workflow {
            name: "w".to_owned(),
            description: String::new(),
            based_on: None,
            params: Default::default(),
            phases: vec![
                shape::try_phase(1, Some("Fix")),
                shape::try_phase(2, Some("Fix 2")),
            ],
        };
        let phase = unique_name(&workflow, shape::try_phase(3, Some("Fix")));
        assert_eq!(phase.name, "Fix 3");
    }
}
