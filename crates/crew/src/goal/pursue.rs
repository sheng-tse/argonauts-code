//! The goal style: an objective and a done-when list pursued in attempts. Each
//! attempt is a phase that ends with its check; when the check says "not yet", the lead plans
//! the next attempt from what's missing and it's added before Merge, which waits at the end. A
//! workflow whose final check finds a criterion unmet may keep going as a goal the same way.
//!
//! Only code, from the check's answers, marks a goal met, stuck, or out of attempts. A stuck
//! goal, and "keep going as a goal", go to whoever answers the crew's questions: you, or in
//! auto mode the lead, which takes the recommended answer and says so.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, CardSpec};
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointKind, CheckpointPacket, FreeText, GoalPacket,
};
use argonauts_core::events::goal::{
    Attempt, ChoiceOption, Criterion, Decider, GoalAction, GoalChecked, GoalEvent, GoalFile,
    GoalQuestion, GoalRun, GoalStatus, GoalVerdict,
};
use argonauts_core::events::run::{ParkReason, Party, RunMode, RunOutcome};
use argonauts_core::events::workflow::{RunNote, StepEnd, WorkflowEvent};
use argonauts_core::names::{RoleName, WorkerRef, WorkflowId};
use argonauts_core::policy::EvidenceFacts;
use argonauts_core::settings::approvals::AtHardLimit;
use argonauts_core::settings::budget::BudgetUnit;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::goal::DiffTag;
use argonauts_core::steps::pursue::{AttemptPlan, PlanAttempt, PlanAttemptInput};
use argonauts_core::workflow::{Phase, StandardPhase, StepKind, Workflow};
use argonauts_core::{CardId, RunId};

use crate::checkpoints::{self, CheckpointRequest};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::prompts::goal::ATTEMPT;
use crate::roles;
use crate::solo;
use crate::state::RunState;
use crate::workflow::{Flow, StepCall, StepDone, change, runner, shape, yaml};

use super::{check, draft, file};

/// The longest objective `/goal` takes, as Codex's does.
pub const MAX_OBJECTIVE: usize = 4_000;

pub const KEEP_GOING: &str = "Keep going as a goal?";
pub const KEEP: &str = "Keep going as a goal";
pub const TO_THE_MERGE: &str = "Go on to the merge";
pub const STUCK: &str = "The goal is stuck. What now?";
pub const ONE_MORE: &str = "One more attempt, by another model";
pub const DELIVER: &str = "Deliver what holds";
pub const STOP: &str = "Stop and keep the branch";
pub const TURN_INTO_GOAL: &str = "Turn this run into a goal?";
pub const OVER_BUDGET: &str = "The goal is over budget. What now?";
pub const CHECK_WHEN_IT_ENDS: &str = "Yes, and check it when this workflow ends";

/// Whether the run pursues a goal: one that started as a goal, or a workflow that kept going as
/// one.
pub fn pursuing(state: &RunState) -> bool {
    state.goal.pursuit.is_some()
        || runner::run_mode(state) == Some(RunMode::Crew)
            && state
                .run
                .named
                .as_ref()
                .is_some_and(|named| named.style == RunStyle::Goal)
}

/// The run's done-when list: the goal's once it's set, else the lead's, else the approved goal
/// file's criteria. A goal that's your prompt alone has one criterion: the prompt done.
pub fn criteria(state: &RunState) -> Vec<Criterion> {
    if let Some(goal) = &state.goal.pursuit {
        return goal.criteria.clone();
    }
    planned_criteria(state)
}

/// The done-when list before a goal pursues it: the lead's, else the approved goal file's.
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

/// The objective: your words from `/goal <objective>`, verbatim, else the run's description.
pub fn objective(state: &RunState) -> String {
    if let Some(goal) = &state.goal.pursuit {
        return goal.objective.clone();
    }
    let started = state.run.started.as_ref();
    let asked_as_goal = started.is_some_and(|started| {
        started.forced
            && started
                .overrides
                .iter()
                .any(|set| set.key == "workflows.style" && set.value.trim_matches('"') == "goal")
    });
    if asked_as_goal && let Some(started) = started {
        return started.request.chars().take(MAX_OBJECTIVE).collect();
    }
    state
        .run
        .named
        .as_ref()
        .map(|named| named.description.clone())
        .or_else(|| started.map(|started| started.request.clone()))
        .unwrap_or_default()
}

/// Before a goal run's attempt phase begins: the goal, the first time, then the attempt.
pub fn begin_attempt(host: &Host, id: &WorkflowId, phase: u32) -> EngineResult<()> {
    let Some((title, set, n)) = host.read(|state| {
        if !pursuing(state) {
            return None;
        }
        let record = state.workflows.by_id.get(id)?;
        if record.parent.is_some() {
            return None;
        }
        let definition = record.phase_definition(phase)?;
        if !shape::is_attempt(definition) {
            return None;
        }
        let attempts = state
            .goal
            .pursuit
            .as_ref()
            .map(|goal| goal.attempts.as_slice())
            .unwrap_or_default();
        if attempts.iter().any(|attempt| attempt.phase == phase) {
            return None;
        }
        let n = u32::try_from(attempts.len()).unwrap_or(u32::MAX) + 1;
        Some((definition.name.clone(), state.goal.pursuit.is_none(), n))
    }) else {
        return Ok(());
    };
    if set {
        let goal = host.read(|state| GoalRun {
            objective: objective(state),
            criteria: criteria(state),
            attempts: Vec::new(),
            status: GoalStatus::Pursuing,
            max_attempts: host.settings().goal.max_attempts.max(1),
        });
        host.record(
            None,
            None,
            RunEventKind::Goal(GoalEvent::Set(Box::new(goal))),
        )?;
    }
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::AttemptStarted { n, title, phase }),
    )?;
    Ok(())
}

/// `check`: the done-when list judged after the gates. In a goal it decides the attempt; in a
/// workflow, with `workflows.final_check`, it runs before Merge, and a criterion left unmet
/// asks whether to keep going as a goal.
pub async fn check_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let (solo, goal_run, criteria, attempt, objective) = host.read(|state| {
        let attempt = state
            .goal
            .pursuit
            .as_ref()
            .and_then(|goal| goal.attempts.last())
            .map_or(1, |attempt| attempt.n);
        (
            runner::run_mode(state) == Some(RunMode::Solo),
            pursuing(state),
            criteria(state),
            attempt,
            objective(state),
        )
    });
    if solo {
        return Ok(StepDone::skipped("a solo run's gates are its check"));
    }
    if !goal_run && !host.settings().workflows.final_check && !host.read(turned_into_goal) {
        return Ok(StepDone::skipped("workflows.final_check is off"));
    }
    if criteria.is_empty() {
        return Ok(StepDone::skipped("there's no done-when list to check"));
    }
    // A check already recorded for this attempt stands, so a re-entered step never asks again.
    let recorded = host.read(|state| {
        state
            .goal
            .checks
            .iter()
            .rev()
            .find(|checked| checked.attempt == attempt)
            .cloned()
    });
    let checked = match recorded {
        Some(checked) => checked,
        None => check::judge(host, call, attempt, &objective, &criteria).await?,
    };
    if goal_run {
        after_attempt(host, call, &checked).await
    } else {
        after_final_check(host, call, &checked).await
    }
}

/// A goal's attempt was checked: met, the next attempt, or stuck.
async fn after_attempt(
    host: &Host,
    call: &StepCall,
    checked: &GoalChecked,
) -> EngineResult<StepDone> {
    let (attempts, status) = host.read(|state| {
        let goal = state.goal.pursuit.as_ref();
        (
            goal.map_or(1, |goal| goal.attempts.len()),
            goal.map(|goal| goal.status),
        )
    });
    let total = checked.verdicts.len();
    let met = check::met_count(&checked.verdicts);
    if checked.overall == GoalVerdict::Met {
        status_changed(host, GoalStatus::Met, Party::Conductor)?;
        let attempts = match attempts {
            1 => "1 attempt".to_owned(),
            n => format!("{n} attempts"),
        };
        return Ok(StepDone::done(format!(
            "Goal met · {total} of {total} · {attempts}"
        )));
    }
    if status == Some(GoalStatus::Cleared) {
        return Ok(StepDone::done(format!(
            "Goal cleared · {met} of {total} met · Merge lands what holds if every check passes"
        )));
    }
    if let Some(spent) = host.read(over_budget) {
        return when_over_budget(host, call, checked, &spent).await;
    }
    if let Some(why) = host.read(|state| stuck(state, host, checked)) {
        return when_stuck(host, call, checked, &why).await;
    }
    next_attempt(host, call, checked).await?;
    Ok(StepDone::done(format!("Not yet met · {met} of {total}")))
}

/// Why the goal can't go on as it is, if it can't: the check says it can't be met, no attempts
/// are left, the same blocker ended `goal.stuck_after` attempts running, or `goal.idle_after`
/// attempts met nothing new.
fn stuck(state: &RunState, host: &Host, checked: &GoalChecked) -> Option<String> {
    let settings = &host.settings().goal;
    let goal = state.goal.pursuit.as_ref()?;
    if checked.overall == GoalVerdict::Impossible {
        return Some(format!(
            "the check says it can't be met: {}",
            checked.reason
        ));
    }
    let used = u32::try_from(goal.attempts.len()).unwrap_or(u32::MAX);
    // A goal that was stuck once and got one more attempt stops after it.
    let allowed = goal.max_attempts + u32::try_from(state.goal.stuck_at.len()).unwrap_or(0);
    if used >= allowed {
        return Some(format!("no attempts left, {used} of {used} used"));
    }
    let checks = goal_checks(state);
    let same = settings.stuck_after.max(1) as usize;
    if checks.len() >= same {
        let blockers: Vec<Vec<&str>> = checks[checks.len() - same..]
            .iter()
            .map(|checked| check::unmet(&checked.verdicts).collect())
            .collect();
        if blockers.iter().all(|blocker| *blocker == blockers[0]) && !blockers[0].is_empty() {
            return Some(format!("the same blocker {same} attempts running"));
        }
    }
    let idle = settings.idle_after.max(1) as usize;
    if checks.len() > idle {
        let window = &checks[checks.len() - idle - 1..];
        let progressed = window.windows(2).any(|pair| {
            let before: Vec<&str> = met_ids(pair[0]).collect();
            met_ids(pair[1]).any(|id| !before.contains(&id))
        });
        let wrote = window[1..]
            .iter()
            .any(|checked| attempt_wrote(state, checked.attempt));
        if !progressed && !wrote {
            return Some(format!(
                "{idle} attempts in a row met no new criterion and changed no file"
            ));
        }
    }
    None
}

fn goal_checks(state: &RunState) -> Vec<&GoalChecked> {
    let Some(goal) = &state.goal.pursuit else {
        return Vec::new();
    };
    goal.attempts
        .iter()
        .filter_map(|attempt| {
            state
                .goal
                .checks
                .iter()
                .rev()
                .find(|checked| checked.attempt == attempt.n)
        })
        .collect()
}

fn met_ids(checked: &GoalChecked) -> impl Iterator<Item = &str> {
    checked
        .verdicts
        .iter()
        .filter(|verdict| verdict.verdict == argonauts_core::events::goal::CriterionState::Met)
        .map(|verdict| verdict.id.as_str())
}

/// The run's budget is spent: `$0.61 of $0.50`. Only a budget in dollars is checked here; the
/// cards' own limits stop them as they go.
fn over_budget(state: &RunState) -> Option<String> {
    let budget = state.run.budget.as_ref()?;
    let limit = budget.limit.as_ref()?;
    (limit.unit == BudgetUnit::Usd && budget.spent.cost_usd >= limit.amount).then(|| {
        format!(
            "${:.2} of ${:.2} spent",
            budget.spent.cost_usd, limit.amount
        )
    })
}

/// Over budget is a hard limit: it asks you whatever the mode, to deliver what holds or stop.
async fn when_over_budget(
    host: &Host,
    call: &StepCall,
    checked: &GoalChecked,
    spent: &str,
) -> EngineResult<StepDone> {
    status_changed(host, GoalStatus::OverBudget, Party::Conductor)?;
    let met = check::met_count(&checked.verdicts);
    let total = checked.verdicts.len();
    let why = format!("{met} of {total} met · {spent}");
    note(host, format!("The goal is over budget: {spent}"))?;
    let answer = choose(
        host,
        call,
        OVER_BUDGET,
        Topic::HardLimit("budget"),
        &[DELIVER, STOP],
        &why,
    )
    .await?;
    if answer == STOP {
        return Ok(StepDone {
            end: StepEnd::Done,
            summary: Some(format!("Goal over budget · {met} of {total} met · stopped")),
            flow: Flow::Finish(RunOutcome::BranchLeft),
        });
    }
    Ok(StepDone::done(format!(
        "Goal over budget · {met} of {total} met · delivers what holds"
    )))
}

/// Whether attempt `n`'s card committed anything.
fn attempt_wrote(state: &RunState, n: u32) -> bool {
    state
        .cards
        .get(&attempt_card_id(n))
        .is_some_and(|record| !record.commits.is_empty())
}

/// The stuck goal goes to whoever answers the crew's questions. The lead takes one more attempt
/// led by another company's model, then delivers what holds.
async fn when_stuck(
    host: &Host,
    call: &StepCall,
    checked: &GoalChecked,
    why: &str,
) -> EngineResult<StepDone> {
    status_changed(host, GoalStatus::Stuck, Party::Conductor)?;
    note(host, format!("The goal is stuck: {why}"))?;
    let extra_taken = host.read(|state| {
        state
            .goal
            .answers
            .iter()
            .any(|(question, answer, _)| question == STUCK && answer == ONE_MORE)
    });
    let options: &[&str] = if extra_taken {
        &[DELIVER, ONE_MORE, STOP]
    } else {
        &[ONE_MORE, DELIVER, STOP]
    };
    let answer = choose(host, call, STUCK, Topic::Crew("stuck goal"), options, why).await?;
    let met = check::met_count(&checked.verdicts);
    let total = checked.verdicts.len();
    match answer.as_str() {
        ONE_MORE => {
            status_changed(host, GoalStatus::Pursuing, Party::Conductor)?;
            next_attempt(host, call, checked).await?;
            Ok(StepDone::done(format!(
                "Goal stuck · {met} of {total} met · one more attempt"
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
            next_attempt(host, call, checked).await?;
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

/// The workflow becomes attempt 1 of a goal with its done-when list, on the same branch.
fn keep_going(host: &Host, call: &StepCall, checked: &GoalChecked) -> EngineResult<()> {
    if host.read(|state| state.goal.pursuit.is_some()) {
        return Ok(());
    }
    let goal = host.read(|state| GoalRun {
        objective: objective(state),
        criteria: criteria(state),
        attempts: vec![Attempt {
            n: checked.attempt,
            title: state
                .run
                .named
                .as_ref()
                .map_or_else(|| "The workflow".to_owned(), |named| named.name.clone()),
            phase: call.at.step.phase,
            verdict: Some(checked.overall),
        }],
        status: GoalStatus::Pursuing,
        max_attempts: host.settings().goal.max_attempts.max(1),
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

/// The lead plans the next attempt from the check, and it's added before Merge. An answer that
/// doesn't make a valid phase gives way to the usual attempt, with a note.
async fn next_attempt(host: &Host, call: &StepCall, checked: &GoalChecked) -> EngineResult<()> {
    let id = call.at.step.workflow.clone();
    let Some((workflow, from, input)) = host.read(|state| {
        let record = state.workflows.by_id.get(&id)?;
        let goal = state.goal.pursuit.as_ref()?;
        let input = PlanAttemptInput {
            goal: goal.objective.clone(),
            criteria: goal.criteria.clone(),
            last_check: Some(argonauts_core::steps::pursue::GoalCheck {
                verdicts: checked.verdicts.clone(),
                overall: checked.overall,
                reason: checked.reason.clone(),
            }),
            attempts: goal
                .attempts
                .iter()
                .map(|attempt| {
                    let verdict = match attempt.verdict {
                        Some(GoalVerdict::Met) => "met",
                        Some(GoalVerdict::Impossible) => "can't be met",
                        Some(GoalVerdict::NotYet) => "not yet",
                        None => "not checked",
                    };
                    format!("{}. {}: {verdict}", attempt.n, attempt.title)
                })
                .collect(),
        };
        Some((record.workflow.clone(), record.change_from(), input))
    }) else {
        return Err(EngineError::Refused(
            "there's no goal to plan an attempt for".to_owned(),
        ));
    };
    let n = u32::try_from(input.attempts.len()).unwrap_or(u32::MAX) + 1;
    let planned = host
        .ask::<PlanAttempt>(Ask {
            slot: n,
            ..Ask::new(
                Some(call.at.clone()),
                RoleName::PhaseLead,
                change::lead(host),
                ATTEMPT,
                input,
            )
        })
        .await;
    let mut problem = None;
    let phase = match planned {
        Ok(answered) => match attempt_from_plan(&answered.output, n) {
            Ok(phase) => phase,
            Err(why) => {
                problem = Some(why);
                shape::attempt_phase(n, title_of(&answered.output))
            }
        },
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(err) => {
            problem = Some(err.to_string());
            shape::attempt_phase(n, None)
        }
    };
    let phase = unique_title(&workflow, phase);
    let mut proposed = with_attempt(&workflow, from, phase.clone());
    if let Err(errors) = runner::check(host, &proposed, 0) {
        problem = Some(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        );
        proposed = with_attempt(
            &workflow,
            from,
            unique_title(&workflow, shape::attempt_phase(n, Some(&phase.name))),
        );
    }
    if let Some(problem) = problem {
        note(
            host,
            format!(
                "The lead's attempt {n} didn't hold, so it runs as the usual attempt: {problem}"
            ),
        )?;
    }
    let unmet: Vec<&str> = check::unmet(&checked.verdicts).collect();
    let reason = format!(
        "check:{} found {} met, so attempt {n} aims at {}",
        checked.attempt,
        check::met_count(&checked.verdicts),
        if unmet.is_empty() {
            "what the check said is missing".to_owned()
        } else {
            unmet.join(", ")
        }
    );
    change::record(host, &id, proposed, None, reason)
}

/// The workflow with `attempt` added at `at`, before Merge.
fn with_attempt(workflow: &Workflow, at: u32, attempt: Phase) -> Workflow {
    let mut proposed = workflow.clone();
    let at = (at as usize).min(proposed.phases.len());
    proposed.phases.insert(at, attempt);
    proposed
}

/// The first attempt the lead planned with the goal, in its brief.
pub fn first_attempt(plan: &AttemptPlan) -> Result<Phase, String> {
    attempt_from_plan(plan, 1)
}

/// The lead's attempt as a phase that ends with its check.
fn attempt_from_plan(plan: &AttemptPlan, n: u32) -> Result<Phase, String> {
    let steps = if plan.steps.trim().is_empty() {
        Vec::new()
    } else {
        yaml::parse_steps(&plan.steps)?
    };
    let usual = shape::attempt_phase(n, None);
    let steps = if steps
        .iter()
        .any(|step| !matches!(step.kind, StepKind::Merge | StepKind::Check))
    {
        steps
    } else {
        usual.steps
    };
    Ok(shape::with_check(Phase {
        name: title_of(plan).map_or(usual.name, str::to_owned),
        detail: match plan.detail.trim() {
            "" => usual.detail,
            detail => detail.to_owned(),
        },
        standard: StandardPhase::Do,
        steps,
    }))
}

fn title_of(plan: &AttemptPlan) -> Option<&str> {
    Some(plan.title.trim()).filter(|title| !title.is_empty() && title.len() <= 40)
}

/// A title no earlier phase has, so each attempt is its own row.
fn unique_title(workflow: &Workflow, mut phase: Phase) -> Phase {
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

/// The card attempt `n` builds: the whole objective, aimed at what the last check said is
/// missing, on top of what earlier attempts integrated. After a stuck goal, another company's
/// model owns it.
pub fn attempt_card(state: &RunState, host: &Host) -> EngineResult<CardSpec> {
    let mut spec = solo::card(state, host)?;
    let Some(goal) = &state.goal.pursuit else {
        return Ok(spec);
    };
    let Some(attempt) = goal.attempts.last() else {
        return Ok(spec);
    };
    let last = goal_checks(state).last().copied();
    let unmet: Vec<&Criterion> = goal
        .criteria
        .iter()
        .filter(|criterion| {
            last.is_none_or(|checked| check::unmet(&checked.verdicts).any(|id| id == criterion.id))
        })
        .collect();
    let mut task = format!(
        "{}\n\nThe goal: {}\n\nThis is attempt {} of up to {}. Make these hold:\n",
        spec.task, goal.objective, attempt.n, goal.max_attempts
    );
    for criterion in &unmet {
        task.push_str(&format!("- {}: {}\n", criterion.id, criterion.text));
    }
    if let Some(checked) = last {
        task.push_str(&format!("\nThe last check said: {}\n", checked.reason));
    }
    spec.id = attempt_card_id(attempt.n);
    spec.title = attempt.title.clone();
    spec.task = task;
    spec.kind = CardKind::Solo;
    spec.criteria = unmet.iter().map(|criterion| criterion.id.clone()).collect();
    if state.goal.stuck_at.contains(&(attempt.n - 1))
        && let Some(other) = other_company(host, spec.owner.as_ref())
    {
        spec.owner = Some(other);
        spec.reviewers.clear();
    }
    Ok(spec)
}

pub fn attempt_card_id(n: u32) -> CardId {
    CardId::new(format!("attempt-{n}")).unwrap_or_else(|_| CardId::numbered(n))
}

/// The first crew member from another company than `owner`.
fn other_company(host: &Host, owner: Option<&WorkerRef>) -> Option<WorkerRef> {
    let owner = owner?;
    let family = roles::family(host, owner);
    host.settings()
        .crew
        .iter()
        .find(|worker| *worker != owner && roles::family(host, worker) != family)
        .cloned()
}

/// What a question is about: one the crew's answerer takes, or a hard limit, which comes to you
/// in every mode but full auto.
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

/// Asks whoever answers the crew's questions to pick one of `options`, the recommended first.
/// The lead takes the recommended one and says so; you get a choose-only card, unless the topic
/// is one `approvals.ask_you_about` keeps yours. In full auto nothing waits for you: the lead
/// answers, and a hard limit does what `approvals.goal.at_hard_limit` says, which stops it
/// unless that's `park`.
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
    let full_auto = checkpoints::full_auto::applies_now(host, CheckpointKind::Goal);
    if full_auto
        && hard_limit
        && checkpoints::configured(host.settings(), CheckpointKind::Goal).at_hard_limit
            != AtHardLimit::Park
        && let Some(stop) = options.iter().find(|option| **option == STOP)
    {
        answered(host, question, stop, Decider::Rules)?;
        note(
            host,
            format!(
                "Full auto stopped at the goal's {topic}, a hard limit, and kept the branch, as \
                 approvals.goal.at_hard_limit says"
            ),
        )?;
        return Ok((*stop).to_owned());
    }
    let yours = hard_limit || checkpoints::keeps_yours(host, CheckpointKind::Goal, &[topic]);
    if !yours && checkpoints::answerer(host, CheckpointKind::Goal).answers_for_you() {
        answered(host, question, &recommended, Decider::Lead)?;
        note(
            host,
            format!("The lead answered \"{question}\": {recommended}, the recommended answer"),
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
            .unwrap_or_else(|| goal_so_far(state, host.run_id()))
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
    let request = CheckpointRequest {
        at: Some(call.at.clone()),
        kind: CheckpointKind::Goal,
        title: question.to_owned(),
        card: None,
        packet: CheckpointPacket::Goal(GoalPacket {
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
        checkpoints::open_yours(host, request, because)?
    } else if yours {
        let because = checkpoints::yours_because(&format!("{topic} questions"));
        checkpoints::open_yours(host, request, because)?
    } else {
        let because = "you answer the crew's questions".to_owned();
        checkpoints::open_because(host, request, Some(because))?
    };
    let decided = match opening.decided_or_withdrawn(host).await? {
        Ok(decided) => decided,
        // Full auto came on while it waited for you, so it's answered as full auto answers it.
        Err(left) if left == checkpoints::full_auto::LEFT_TO_THE_LEAD => {
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
        super::checkpoint::decider(&decided.by),
    )?;
    Ok(answer)
}

/// The goal as a run pursues it before any goal file was approved: the objective and the
/// done-when list.
fn goal_so_far(state: &RunState, run: &RunId) -> GoalFile {
    let mut goal = draft::from_prompt(run, &objective(state));
    goal.criteria = criteria(state);
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
            status_changed(host, GoalStatus::Cleared, Party::User).map_err(refused)?;
            if land {
                note(
                    host,
                    "You stopped pursuing the goal; Merge lands what holds once every check \
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
        GoalAction::TurnIntoGoal => {
            if host.read(pursuing) {
                return Err("this run already pursues a goal".to_owned());
            }
            if host.read(turned_into_goal) {
                return Ok(());
            }
            answered(host, TURN_INTO_GOAL, CHECK_WHEN_IT_ENDS, Decider::You).map_err(refused)?;
            check_before_merge(host).map_err(refused)
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
            // Working agents hear it now; the next attempt or phase end plans with it.
            crate::blackboard::commands::guidance(
                host,
                format!("You changed an assumption. It was: {was}. It is now: {to}."),
            )
            .await
            .map(|_| ())
        }
    }
}

/// A workflow you turned into a goal gets its check before Merge, even with
/// `workflows.final_check` off, so the goal has an attempt 1 to judge.
fn check_before_merge(host: &Host) -> EngineResult<()> {
    let Some((id, workflow, from)) = host.read(|state| {
        let id = state.workflows.root.clone()?;
        let record = state.workflows.by_id.get(&id)?;
        Some((id, record.workflow.clone(), record.change_from()))
    }) else {
        return Ok(());
    };
    let ahead = &workflow.phases[(from as usize).min(workflow.phases.len())..];
    if ahead
        .iter()
        .any(|phase| phase.steps.iter().any(|step| step.kind == StepKind::Check))
    {
        return Ok(());
    }
    let Some(merge) = workflow
        .phases
        .iter()
        .position(|phase| phase.steps.iter().any(|step| step.kind == StepKind::Merge))
        .filter(|merge| *merge >= from as usize)
    else {
        return Ok(());
    };
    let mut proposed = workflow.clone();
    proposed.phases.insert(merge, shape::check_phase());
    change::record(
        host,
        &id,
        proposed,
        None,
        "You turned the run into a goal, so a check judges the workflow before Merge".to_owned(),
    )
}

/// The run parks: a goal being pursued is paused, by you or by a usage limit, until it goes on.
pub fn on_park(host: &Host, reason: &ParkReason) -> EngineResult<()> {
    let by = match reason {
        ParkReason::Checkpoint => return Ok(()),
        ParkReason::Paused => Party::User,
        ParkReason::AllVendorsOut { .. }
        | ParkReason::WaitingForVendor { .. }
        | ParkReason::NoStandIn { .. } => Party::Conductor,
    };
    let pursuing = host.read(|state| {
        state
            .goal
            .pursuit
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Pursuing)
    });
    if pursuing {
        status_changed(host, GoalStatus::Paused, by)?;
    }
    Ok(())
}

/// The run goes on after a park: a paused goal is pursued again.
pub fn on_resume(host: &Host) -> EngineResult<()> {
    let paused = host.read(|state| {
        state
            .goal
            .pursuit
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Paused)
    });
    if paused {
        status_changed(host, GoalStatus::Pursuing, Party::User)?;
    }
    Ok(())
}

fn status_changed(host: &Host, status: GoalStatus, by: Party) -> EngineResult<()> {
    let current = host.read(|state| state.goal.pursuit.as_ref().map(|goal| goal.status));
    if current.is_none() || current == Some(status) {
        return Ok(());
    }
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::StatusChanged { status, by }),
    )?;
    Ok(())
}

/// A `›` note on why the plan moved, on the run's own workflow.
fn note(host: &Host, text: String) -> EngineResult<()> {
    let workflow = host.read(|state| state.workflows.root.clone());
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Note(RunNote { text, workflow })),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(title: &str, steps: &str) -> AttemptPlan {
        AttemptPlan {
            title: title.to_owned(),
            detail: "one owner fixes it".to_owned(),
            steps: steps.to_owned(),
        }
    }

    #[test]
    fn the_leads_attempt_ends_with_its_check_and_never_merges() {
        let steps = "- { kind: build, input: goal, label: \"fix:empty\" }\n- { kind: merge }\n";
        let phase = attempt_from_plan(&plan("Empty List", steps), 2).unwrap();
        assert_eq!(phase.name, "Empty List");
        let kinds: Vec<StepKind> = phase.steps.iter().map(|step| step.kind).collect();
        assert_eq!(kinds, [StepKind::Build, StepKind::Check]);
        assert!(shape::is_attempt(&phase));
    }

    #[test]
    fn an_empty_plan_runs_the_usual_attempt() {
        let phase = attempt_from_plan(&plan("", ""), 3).unwrap();
        assert_eq!(phase.name, "Attempt 3");
        assert_eq!(phase.steps[0].input.as_deref(), Some(crate::card::GOAL));
        assert!(attempt_from_plan(&plan("Fix", "- { kind: nonsense }"), 3).is_err());
    }

    #[test]
    fn a_goal_is_over_budget_once_its_dollars_are_spent() {
        use argonauts_core::budget::Spend;
        use argonauts_core::events::vendor::RunBudget;
        use argonauts_core::settings::budget::BudgetAmount;
        let budget = |spent: f64| RunBudget {
            spent: Spend {
                cost_usd: spent,
                ..Spend::default()
            },
            limit: Some(BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: 0.5,
            }),
            lines: Vec::new(),
            reserve_left: None,
        };
        let mut state = RunState::default();
        assert_eq!(over_budget(&state), None);
        state.run.budget = Some(budget(0.61));
        assert_eq!(over_budget(&state).as_deref(), Some("$0.61 of $0.50 spent"));
        state.run.budget = Some(budget(0.2));
        assert_eq!(over_budget(&state), None);
    }

    #[test]
    fn each_attempt_has_its_own_title() {
        let workflow = Workflow {
            name: "w".to_owned(),
            description: String::new(),
            based_on: None,
            params: Default::default(),
            phases: vec![
                shape::attempt_phase(1, Some("Fix")),
                shape::attempt_phase(2, Some("Fix 2")),
            ],
        };
        let phase = unique_title(&workflow, shape::attempt_phase(3, Some("Fix")));
        assert_eq!(phase.name, "Fix 3");
    }
}
