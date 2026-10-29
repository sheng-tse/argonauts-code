//! Which executor runs each step kind, plus the kinds that belong to the runner itself:
//! `fan_out`, `for_each`, `loop_until`, `check`, a generic `approval`, and `workflow`.

use futures::future::join_all;
use serde_json::Value;

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::workflow::{RunNote, StepEnd, WorkflowEvent};
use argonauts_core::future::BoxFuture;
use argonauts_core::names::{RoleName, WorkflowId};
use argonauts_core::workflow::{StandardPhase, Step, StepKind, StepRef, Workflow};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::plan::context;
use crate::state::RunState;
use crate::task::checks::CheckVerdict;
use crate::workflow::runner::{self, run_step};
use crate::workflow::{Flow, StepAt, StepCall, StepDone, conditions, describe, params};
use crate::{divide, goal, merge, plan, review, task, understand};

/// Runs one step with its kind's executor. The runner records `StepStarted` and
/// `StepFinished` around it.
pub fn execute<'a>(host: &'a Host, call: &'a StepCall) -> BoxFuture<'a, EngineResult<StepDone>> {
    Box::pin(async move {
        // The validator refuses it elsewhere too; this holds for a workflow that skipped it.
        if call.step.different_provider_from.is_some()
            && !matches!(
                call.step.kind,
                StepKind::Review | StepKind::FanOut | StepKind::Build
            )
        {
            return Err(EngineError::Refused(format!(
                "{}: only reviewers, and the Captain task's review, pick their workers by `different_provider_from`; a `{}` step picks its own",
                describe(&call.at.step),
                crate::workflow::validate::kind_name(call.step.kind)
            )));
        }
        if plans(call) {
            host.read(context::require_approved_goal)?;
        }
        match call.step.kind {
            StepKind::Read => understand::read_step(host, call).await,
            StepKind::Compare => understand::compare_step(host, call).await,
            StepKind::Draft => plan::draft_step(host, call).await,
            StepKind::Map => plan::map_step(host, call).await,
            StepKind::Critique => plan::critique_step(host, call).await,
            StepKind::Synthesize => plan::synthesize_step(host, call).await,
            StepKind::Judge => plan::judge_step(host, call).await,
            StepKind::Review => review::review_step(host, call).await,
            StepKind::Build => {
                if !matches!(
                    call.step.input.as_deref(),
                    Some(task::EXPERIMENTS | task::GOAL)
                ) {
                    // Before any task opens a worktree, so the task a council's seat builds
                    // opens in the folder the seat talked in.
                    crate::council::talk::folder::hand_to_builders(host).await?;
                }
                task::build_step(host, call).await
            }
            StepKind::Merge => merge::step::merge_step(host, call).await,
            StepKind::Approval => approval(host, call).await,
            StepKind::FanOut => fan_out(host, call).await,
            StepKind::ForEach => for_each(host, call).await,
            StepKind::LoopUntil => loop_until(host, call).await,
            StepKind::Checks => check(host, call).await,
            StepKind::Workflow => nested(host, call).await,
            StepKind::GoalCheck => goal::pursue::check_step(host, call).await,
            StepKind::Script => crate::script::script_step(host, call).await,
            StepKind::Talk => crate::council::talk::step(host, call).await,
        }
    })
}

/// Steps that plan against the goal. Outside a plan phase `critique` and `synthesize` work on their
/// input instead, and `judge` settles reviewers who disagree.
fn plans(call: &StepCall) -> bool {
    let in_plan = call.standard == StandardPhase::Plan;
    match call.step.kind {
        StepKind::Draft | StepKind::Map => true,
        StepKind::Critique | StepKind::Synthesize | StepKind::Judge => in_plan,
        _ => false,
    }
}

/// Understand and plan approvals belong to their modules; the rest are opened here.
async fn approval(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    match call.step.approval {
        Some(ApprovalKind::Understand) => understand::approval_step(host, call).await,
        Some(ApprovalKind::Plan) => divide::plan_approval_step(host, call).await,
        _ => generic_approval(host, call).await,
    }
}

/// An `approval` step opens the Understand or plan approval. The validator refuses every other
/// kind, since those open by themselves when their event happens, and this refuses it again for
/// a workflow that skipped validation.
async fn generic_approval(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let _ = host;
    let kind = call
        .step
        .approval
        .map_or_else(|| "no".to_owned(), |kind| format!("a {kind:?}"));
    Err(EngineError::Refused(format!(
        "{} opens {kind} approval; an `approval` step opens only the Understand or plan approval",
        describe(&call.at.step)
    )))
}

/// The step kind that does a role's work.
pub fn kind_for_role(role: RoleName) -> Option<StepKind> {
    Some(match role {
        RoleName::Reader => StepKind::Read,
        RoleName::Comparer => StepKind::Compare,
        RoleName::Draft | RoleName::Orchestrator => StepKind::Draft,
        RoleName::Map => StepKind::Map,
        RoleName::Critic => StepKind::Critique,
        RoleName::Judge => StepKind::Judge,
        RoleName::Reviewer => StepKind::Review,
        RoleName::Worker | RoleName::Experiment => StepKind::Build,
        RoleName::Main | RoleName::Consultant | RoleName::TieBreaker => StepKind::Synthesize,
        RoleName::RunReview | RoleName::Narrator => return None,
    })
}

/// The same task to several workers or roles at once: each role's own step kind runs with the
/// step's `count`, `models`, `blind`, and `different_provider_from`.
async fn fan_out(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let roles: Vec<RoleName> = if call.step.roles.is_empty() {
        call.step.role.into_iter().collect()
    } else {
        call.step.roles.clone()
    };
    let mut calls = Vec::with_capacity(roles.len());
    for (index, role) in roles.iter().enumerate() {
        let kind = kind_for_role(*role).ok_or_else(|| {
            EngineError::Refused(format!(
                "{}: the {} role has no step kind to fan out",
                describe(&call.at.step),
                role.as_str()
            ))
        })?;
        let step = Step {
            kind,
            name: None,
            role: Some(*role),
            roles: Vec::new(),
            when: None,
            steps: Vec::new(),
            until: None,
            max: None,
            approval: None,
            workflow: None,
            params: Default::default(),
            ..call.step.clone()
        };
        calls.push(inner(
            call,
            index,
            round_for(host, call, index),
            step,
            call.item.clone(),
        ));
    }
    let results = join_all(calls.iter().map(|inner| run_step(host, inner))).await;
    let mut dones = Vec::with_capacity(results.len());
    for result in results {
        dones.push(result?);
    }
    let flow = dones
        .iter()
        .map(|done| done.flow.clone())
        .find(|flow| *flow != Flow::Continue)
        .unwrap_or(Flow::Continue);
    let failed = dones
        .iter()
        .filter(|done| done.end == StepEnd::Failed)
        .count();
    let names: Vec<&str> = roles.iter().map(RoleName::as_str).collect();
    Ok(StepDone {
        end: if failed > 0 {
            StepEnd::Failed
        } else {
            StepEnd::Done
        },
        summary: Some(match failed {
            0 => format!("{} done", names.join(", ")),
            _ => format!("{failed} of {} failed", names.len()),
        }),
        flow,
    })
}

/// One round of the inner steps per item of the `input` step's output.
async fn for_each(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let input = call.step.input.as_deref().unwrap_or_default();
    let items = items(host, call, input)?;
    let since = own_since(host, call);
    for (number, item) in items.iter().enumerate() {
        let iteration = number + 1;
        for (index, step) in call.step.steps.iter().enumerate() {
            let round = iteration_round(host, call, index, since, iteration);
            let inner_call = inner(call, index, round, step.clone(), Some(item.clone()));
            let done = run_step(host, &inner_call).await?;
            if done.end == StepEnd::Failed {
                return Ok(inner_failed(&inner_call, &done));
            }
            if done.flow != Flow::Continue {
                return Ok(StepDone {
                    end: StepEnd::Done,
                    summary: Some(format!("stopped at item {iteration} of {}", items.len())),
                    flow: done.flow,
                });
            }
        }
    }
    Ok(StepDone::done(match items.len() {
        0 => format!("nothing in {input}"),
        1 => "1 item".to_owned(),
        count => format!("{count} items"),
    }))
}

/// Repeats the inner steps until `until` holds, never past `max` rounds. `until` is asked
/// before each round, so a loop whose condition already holds runs no round at all. A loop
/// that uses up its rounds without `until` fails, since what comes after it needs `until`.
/// A `›` note on why the plan moved: a loop runs again because its condition doesn't hold yet.
fn note_round(host: &Host, call: &StepCall, round: usize, until: &str) -> EngineResult<()> {
    let phase = host.read(|state| {
        let record = state.workflows.by_id.get(call.workflow())?;
        Some(record.phase_definition(call.at.step.phase)?.name.clone())
    });
    let what = call
        .step
        .name
        .clone()
        .or(phase)
        .unwrap_or_else(|| "the loop".to_owned());
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Note(RunNote {
            text: format!("{what} runs round {round}, since {until} doesn't hold yet"),
            workflow: Some(call.workflow().clone()),
            left: false,
        })),
    )?;
    Ok(())
}

async fn loop_until(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let until = call.step.until.as_deref().unwrap_or_default();
    let max = call.step.max.unwrap_or(1).max(1);
    let since = own_since(host, call);
    for iteration in 1..=max as usize {
        // A round that already began was asked about when it began; asking again after a
        // crash could end the loop in the middle of that round.
        let began =
            !call.step.steps.is_empty() && started_since(host, call, 0, since).len() >= iteration;
        if !began {
            let at = StepAt {
                step: call.at.step.clone(),
                round: iteration as u32,
            };
            let holds = host
                .read(|state| conditions::holds(state, host.settings(), until, &at))
                .map_err(EngineError::Refused)?;
            if holds {
                return Ok(StepDone::done(match iteration - 1 {
                    0 => format!("{until} held, no round needed"),
                    1 => format!("{until} after 1 round"),
                    rounds => format!("{until} after {rounds} rounds"),
                }));
            }
            if iteration > 1 {
                note_round(host, call, iteration, until)?;
            }
        }
        for (index, step) in call.step.steps.iter().enumerate() {
            let round = iteration_round(host, call, index, since, iteration);
            let inner_call = inner(call, index, round, step.clone(), call.item.clone());
            let done = run_step(host, &inner_call).await?;
            if done.end == StepEnd::Failed {
                return Ok(inner_failed(&inner_call, &done));
            }
            if done.flow != Flow::Continue {
                return Ok(StepDone {
                    end: StepEnd::Done,
                    summary: Some(format!("stopped in round {iteration}")),
                    flow: done.flow,
                });
            }
        }
    }
    let at = StepAt {
        step: call.at.step.clone(),
        round: max + 1,
    };
    let held = host
        .read(|state| conditions::holds(state, host.settings(), until, &at))
        .map_err(EngineError::Refused)?;
    if held {
        return Ok(StepDone::done(format!("{until} after {max} rounds")));
    }
    if until == conditions::UNDERSTOOD {
        // Everything after the goal needs it approved, so the run can't go on.
        let text = match max {
            1 => "The goal wasn't approved, so the run stopped".to_owned(),
            rounds => format!("The goal wasn't approved in {rounds} rounds, so the run stopped"),
        };
        context::warn(host, None, format!("{text}."))?;
        return Ok(StepDone {
            end: StepEnd::Failed,
            summary: Some(text),
            flow: Flow::Finish(RunOutcome::Stopped),
        });
    }
    Ok(StepDone {
        end: StepEnd::Failed,
        summary: Some(format!(
            "{until} didn't hold after its maximum of {max} round{}",
            if max == 1 { "" } else { "s" }
        )),
        flow: Flow::Continue,
    })
}

/// A loop or `for_each` stops where an inner step failed, and fails with it.
fn inner_failed(inner: &StepCall, done: &StepDone) -> StepDone {
    StepDone {
        end: StepEnd::Failed,
        summary: Some(format!(
            "{} failed: {}",
            describe(&inner.at.step),
            done.summary.as_deref().unwrap_or("it gave no reason")
        )),
        flow: Flow::Continue,
    }
}

/// Whether the nearest `loop_until` around the step at `at` may run another round after the
/// current one. A step in no loop gets none.
pub fn another_round(state: &RunState, at: &StepRef) -> bool {
    rounds_left(state, at) == Some(true)
}

/// Whether `at` sits in a `loop_until` running its last round, where nothing is left to send
/// it back to.
pub fn in_last_loop_round(state: &RunState, at: &StepAt) -> bool {
    rounds_left(state, &at.step) == Some(false)
}

/// Whether the nearest `loop_until` around the step has a round left after the current one,
/// or `None` when no loop is around it.
fn rounds_left(state: &RunState, at: &StepRef) -> Option<bool> {
    let record = state.workflows.by_id.get(&at.workflow)?;
    let phase = record.workflow.phases.get(at.phase as usize)?;
    let mut steps = &phase.steps;
    let mut enclosing = None;
    for (depth, index) in at.path.iter().enumerate() {
        let step = steps.get(*index as usize)?;
        if step.kind == StepKind::LoopUntil && depth + 1 < at.path.len() {
            enclosing = Some((depth, step));
        }
        steps = &step.steps;
    }
    let (depth, looped) = enclosing?;
    let loop_ref = StepRef {
        workflow: at.workflow.clone(),
        phase: at.phase,
        path: at.path[..=depth].to_vec(),
    };
    let mut first = loop_ref.clone();
    first.path.push(0);
    let since = record.progress(&loop_ref)?.since;
    // The loop's first inner step starts once per round, so its starts count the rounds.
    let rounds = record
        .steps
        .iter()
        .filter(|progress| progress.step == first && progress.since > since)
        .count();
    let max = looped.max.unwrap_or(1).max(1) as usize;
    Some(rounds < max)
}

/// Runs the configured `checks.run` commands on the step's target. A workflow can't add any.
/// The target is the current item's task inside `for_each`, else every task with a worktree.
async fn check(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let tasks: Vec<TaskId> = match call.item.as_ref().and_then(item_task) {
        Some(task) => vec![task],
        None => host.read(|state| {
            state
                .tasks
                .by_id
                .values()
                .filter(|record| record.worktree.is_some())
                .map(|record| record.spec.id.clone())
                .collect()
        }),
    };
    if tasks.is_empty() {
        return Ok(StepDone::skipped("no task to run the checks on"));
    }
    let mut failed = Vec::new();
    for task in &tasks {
        match task::checks::run(host, task).await? {
            CheckVerdict::Passed => {}
            CheckVerdict::Failed { .. } | CheckVerdict::Stopped { .. } => {
                failed.push(task.to_string())
            }
        }
    }
    Ok(if failed.is_empty() {
        StepDone::done(match tasks.len() {
            1 => format!("checks passed on {}", tasks[0]),
            count => format!("checks passed on {count} tasks"),
        })
    } else {
        StepDone {
            end: StepEnd::Failed,
            summary: Some(format!("checks failed on {}", failed.join(", "))),
            flow: Flow::Continue,
        }
    })
}

/// Starts the named workflow one level down and runs it to its end.
async fn nested(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let Some(name) = call.step.workflow.clone() else {
        return Err(EngineError::Refused(format!(
            "{} names no workflow to run",
            describe(&call.at.step)
        )));
    };
    // Re-entered, it runs the child this round started, as the log holds it, even after an
    // approved change or an edit to the file.
    let id = match host.read(|state| child_of(state, &call.at)) {
        Some(id) => id,
        None => {
            let workflow = runner::lookup(host, name.as_str())?;
            let workflow = params::fill(&workflow, &call.step.params)
                .map_err(|errors| EngineError::Refused(format!("{name}: {}", errors.join("; "))))?;
            runner::start(
                host,
                Some(name.clone()),
                workflow,
                Some(call.at.step.clone()),
            )?
        }
    };
    let flow = runner::run(host, &id).await?;
    let (steps, failed) = host.read(|state| {
        let Some(record) = state.workflows.by_id.get(&id) else {
            return (0, 0);
        };
        let finished = record
            .steps
            .iter()
            .filter(|step| step.end.is_some() && step.step.path.len() == 1);
        let failed = finished
            .clone()
            .filter(|step| step.end == Some(StepEnd::Failed))
            .count();
        (finished.count(), failed)
    });
    Ok(StepDone {
        end: if failed > 0 {
            StepEnd::Failed
        } else {
            StepEnd::Done
        },
        summary: Some(format!(
            "{name} ran {steps} step{}{}",
            if steps == 1 { "" } else { "s" },
            if failed > 0 {
                format!(", {failed} failed")
            } else {
                String::new()
            }
        )),
        flow,
    })
}

/// The workflow this round of a `workflow` step started, if it did.
fn child_of(state: &RunState, at: &StepAt) -> Option<WorkflowId> {
    let since = state
        .workflows
        .by_id
        .get(&at.step.workflow)?
        .round(&at.step, at.round)?
        .since;
    state
        .workflows
        .by_id
        .values()
        .filter(|child| child.parent.as_ref() == Some(&at.step) && child.since > since)
        .max_by_key(|child| child.since)
        .map(|child| child.id.clone())
}

/// An inner step of `call`, at `index` in its `steps` (or its roles, for `fan_out`).
fn inner(call: &StepCall, index: usize, round: u32, step: Step, item: Option<Value>) -> StepCall {
    let mut path = call.at.step.path.clone();
    path.push(index as u32);
    StepCall {
        at: StepAt {
            step: StepRef {
                workflow: call.at.step.workflow.clone(),
                phase: call.at.step.phase,
                path,
            },
            round,
        },
        depth: call.depth,
        standard: call.standard,
        step,
        item,
    }
}

fn inner_ref(call: &StepCall, index: usize) -> StepRef {
    let mut path = call.at.step.path.clone();
    path.push(index as u32);
    StepRef {
        workflow: call.at.step.workflow.clone(),
        phase: call.at.step.phase,
        path,
    }
}

/// The seq this round of `call` started at.
fn own_since(host: &Host, call: &StepCall) -> u64 {
    host.read(|state| {
        state
            .workflows
            .by_id
            .get(call.workflow())
            .and_then(|record| record.round(&call.at.step, call.at.round))
            .map_or(0, |progress| progress.since)
    })
}

/// The rounds of inner step `index` that started during this round of `call`, in order.
fn started_since(host: &Host, call: &StepCall, index: usize, since: u64) -> Vec<(u32, bool)> {
    let step = inner_ref(call, index);
    host.read(|state| {
        state
            .workflows
            .by_id
            .get(call.workflow())
            .map(|record| {
                record
                    .steps
                    .iter()
                    .filter(|progress| progress.step == step && progress.since > since)
                    .map(|progress| (progress.round, progress.end.is_some()))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Inner steps count their rounds across every round of what encloses them, so a round
/// number names one run. The `iteration`-th run during this round of `call` reuses its round
/// when it already started; otherwise it takes the next number.
fn iteration_round(
    host: &Host,
    call: &StepCall,
    index: usize,
    since: u64,
    iteration: usize,
) -> u32 {
    let started = started_since(host, call, index, since);
    if let Some((round, _)) = started.get(iteration - 1) {
        return *round;
    }
    round_for(host, call, index)
}

/// The next round number for inner step `index`, or the unfinished one to re-enter.
fn round_for(host: &Host, call: &StepCall, index: usize) -> u32 {
    let step = inner_ref(call, index);
    host.read(|state| match state.workflows.progress(&step) {
        None => 1,
        Some(progress) if progress.end.is_none() => progress.round,
        Some(progress) => progress.round + 1,
    })
}

/// A task id from a `for_each` item: a task spec, or the id itself.
pub(crate) fn item_task(item: &Value) -> Option<TaskId> {
    let text = item.get("id").unwrap_or(item).as_str()?;
    TaskId::new(text).ok()
}

/// The items a `for_each` goes over: the plan's tasks, the goal's criteria, the merged tasks
/// for `diff`, or each structured answer an earlier step's sessions gave, flattening lists.
fn items(host: &Host, call: &StepCall, input: &str) -> EngineResult<Vec<Value>> {
    match input {
        "plan" => host
            .read(|state| {
                let tasks = state
                    .divide
                    .graph
                    .as_ref()
                    .map(|graph| graph.tasks.clone())
                    .unwrap_or_default();
                tasks
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(Into::into),
        "goal" => host
            .read(|state| {
                let criteria = state
                    .goal
                    .approved
                    .as_ref()
                    .map(|goal| goal.criteria.clone())
                    .unwrap_or_default();
                criteria
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(Into::into),
        "diff" => Ok(host.read(|state| {
            state
                .merge
                .merged
                .iter()
                .map(|(task, _)| Value::String(task.to_string()))
                .collect()
        })),
        "experiments" => Ok(Vec::new()),
        named => {
            let Some(step) = host.read(|state| {
                let record = state.workflows.by_id.get(call.workflow())?;
                find_named(&record.workflow, named).map(|(phase, path)| StepRef {
                    workflow: record.id.clone(),
                    phase,
                    path,
                })
            }) else {
                return Err(EngineError::Refused(format!(
                    "{}: `input: {named}` names no step",
                    describe(&call.at.step)
                )));
            };
            Ok(host.read(|state| {
                let mut sessions: Vec<_> = state
                    .sessions
                    .by_key
                    .values()
                    .filter(|record| {
                        record.started.workflow_step.as_ref().is_some_and(|at| {
                            at.workflow == step.workflow
                                && at.phase == step.phase
                                && at.path.starts_with(&step.path)
                        })
                    })
                    .collect();
                sessions.sort_by_key(|record| record.since);
                sessions
                    .into_iter()
                    .filter_map(|record| record.end.as_ref()?.structured.clone())
                    .flat_map(|value| match value {
                        Value::Array(items) => items,
                        other => vec![other],
                    })
                    .collect()
            }))
        }
    }
}

/// The phase and path of the step called `name`, searching loops and `for_each` too.
pub fn find_named(workflow: &Workflow, name: &str) -> Option<(u32, Vec<u32>)> {
    fn search(steps: &[Step], name: &str, path: &mut Vec<u32>) -> bool {
        for (index, step) in steps.iter().enumerate() {
            path.push(index as u32);
            if step.name.as_deref() == Some(name) || search(&step.steps, name, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    workflow
        .phases
        .iter()
        .enumerate()
        .find_map(|(phase, definition)| {
            let mut path = Vec::new();
            search(&definition.steps, name, &mut path).then_some((phase as u32, path))
        })
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::WorkflowId;

    use super::*;
    use crate::state::workflow::{StepProgress, WorkflowRecord};
    use crate::workflow::builtin;

    fn at(path: &[u32]) -> StepRef {
        StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: path.to_vec(),
        }
    }

    fn started(path: &[u32], kind: StepKind, round: u32, since: u64) -> StepProgress {
        StepProgress {
            step: at(path),
            kind,
            standard: StandardPhase::Understand,
            round,
            since,
            end: None,
            summary: None,
            ended_at: None,
        }
    }

    /// A crew run in its Understand loop, with `rounds` rounds of reading started.
    fn understand_loop(name: &str, rounds: u32) -> RunState {
        let workflow = builtin::load(name).unwrap();
        let mut steps = vec![started(&[0], StepKind::LoopUntil, 1, 1)];
        for round in 1..=rounds {
            let since = u64::from(round) * 10;
            steps.push(started(&[0, 0], StepKind::Read, round, since));
            steps.push(started(&[0, 2], StepKind::Approval, round, since + 1));
        }
        let mut state = RunState::default();
        state.workflows.by_id.insert(
            WorkflowId::numbered(1),
            WorkflowRecord {
                id: WorkflowId::numbered(1),
                from: None,
                version: String::new(),
                parent: None,
                workflow,
                steps,
                finished: false,
                changed_from_phase: None,
                since: 0,
                changes: Vec::new(),
                declared: None,
                phases_started: Default::default(),
                phases_ended: Default::default(),
                phases_skipped: Default::default(),
                left: Vec::new(),
            },
        );
        state
    }

    #[test]
    fn the_understand_loop_has_a_round_left_only_before_its_last() {
        let first = understand_loop("council", 1);
        assert!(another_round(&first, &at(&[0, 2])));
        let last = understand_loop("council", 2);
        assert!(!another_round(&last, &at(&[0, 2])));
        let last = understand_loop("orchestrator", 2);
        assert!(!another_round(&last, &at(&[0, 2])));
    }

    #[test]
    fn a_step_in_no_loop_has_no_round_left() {
        let captain = understand_loop("captain", 0);
        assert!(!another_round(&captain, &at(&[1])));
        assert!(!another_round(&RunState::default(), &at(&[0, 2])));
    }

    fn call(step: Value, standard: StandardPhase) -> StepCall {
        StepCall {
            at: StepAt {
                step: at(&[0]),
                round: 1,
            },
            depth: 0,
            standard,
            step: serde_json::from_value(step).unwrap(),
            item: None,
        }
    }

    #[test]
    fn planning_steps_are_the_ones_that_need_an_approved_goal() {
        use serde_json::json;
        let draft = json!({ "kind": "draft", "role": "draft" });
        assert!(plans(&call(draft.clone(), StandardPhase::Plan)));
        assert!(plans(&call(draft, StandardPhase::Do)));
        let critique = json!({ "kind": "critique", "role": "critic" });
        assert!(plans(&call(critique.clone(), StandardPhase::Plan)));
        assert!(!plans(&call(critique, StandardPhase::Check)));
        let judge = json!({ "kind": "judge", "role": "judge" });
        assert!(plans(&call(judge.clone(), StandardPhase::Plan)));
        assert!(!plans(&call(judge, StandardPhase::Decide)));
        let read_step = json!({ "kind": "read", "role": "reader" });
        assert!(!plans(&call(read_step, StandardPhase::Understand)));
    }
}
