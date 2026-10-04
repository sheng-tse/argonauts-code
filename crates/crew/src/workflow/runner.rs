//! Walks a workflow's phases and steps from where the log says it stopped, so a recovered run
//! re-enters the unfinished step and skips the finished ones.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::RunMode;
use argonauts_core::events::workflow::{
    PhaseMark, StepEnd, StepFinished, StepStarted, WorkflowDeclared, WorkflowEvent, WorkflowStarted,
};
use argonauts_core::future::BoxFuture;
use argonauts_core::names::{WorkerRef, WorkflowId};
use argonauts_core::paths::Paths;
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::UnderstandWhen;
use argonauts_core::workflow::{
    StandardPhase, Step, StepRef, Workflow, WorkflowParams, WorkflowRef,
};
use argonauts_core::{canonical_json, sha256_hex};

use crate::card;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::goal::pursue;
use crate::roles;
use crate::state::RunState;
use crate::state::ids::IdKind;
use crate::workflow::validate::{self, Context};
use crate::workflow::{
    Flow, StepAt, StepCall, StepDone, builtin, change, conditions, describe, kinds, params, replay,
    shape, yaml,
};
use argonauts_core::steps::StepName;

/// The run's workflow from the settings: the preset's `workflow`, looked up like any other. A
/// solo run keeps that workflow when it builds the solo card, and otherwise follows the solo
/// built-in its `solo` settings pick. A run that already started one (a recovered run, or one
/// that switched) gets that one back.
pub fn workflow_for_run(host: &Host) -> EngineResult<(Option<WorkflowRef>, Workflow)> {
    let running = host.read(|state| {
        let root = state.workflows.root.as_ref()?;
        let record = state.workflows.by_id.get(root)?;
        Some((record.from.clone(), record.workflow.clone()))
    });
    if let Some(running) = running {
        return Ok(running);
    }
    let settings = host.settings();
    let configured = lookup(host, settings.workflow.as_str());
    if host.read(run_mode) != Some(RunMode::Solo) {
        return Ok((Some(settings.workflow.clone()), configured?));
    }
    // A crew workflow that can't be found doesn't matter to a solo run that wouldn't use it.
    if let Ok(workflow) = configured
        && builds_goal(&workflow)
    {
        return Ok((Some(settings.workflow.clone()), workflow));
    }
    let name = WorkflowRef::new(solo_workflow(settings));
    let workflow = lookup(host, name.as_str())?;
    Ok((Some(name), workflow))
}

/// The run's mode, once it's known.
pub fn run_mode(state: &RunState) -> Option<RunMode> {
    state
        .run
        .mode
        .or_else(|| state.run.started.as_ref().map(|started| started.mode))
}

/// A crew run following a crew workflow. Solo runs, and runs that switched to a solo workflow
/// (such as "run it solo" at the plan checkpoint), are not. Phase-end adaptation runs only in
/// these.
pub fn crew_run(state: &RunState, id: &WorkflowId) -> bool {
    run_mode(state) != Some(RunMode::Solo) && !in_solo_workflow(state, id)
}

/// Whether the workflow, or one it runs inside, builds the solo card.
pub fn in_solo_workflow(state: &RunState, id: &WorkflowId) -> bool {
    let mut at = state.workflows.by_id.get(id);
    while let Some(record) = at {
        if builds_goal(&record.workflow) {
            return true;
        }
        at = record
            .parent
            .as_ref()
            .and_then(|parent| state.workflows.by_id.get(&parent.workflow));
    }
    false
}

/// Which built-in a solo run follows when the preset's workflow doesn't build the solo card,
/// and where "run it solo" at the plan checkpoint goes.
pub fn solo_workflow(settings: &Settings) -> &'static str {
    if settings.solo.attempts > 1 {
        "best-of-n"
    } else if settings.solo.review {
        "solo-reviewed"
    } else {
        "solo"
    }
}

/// Where a solo card that escalates goes: the preset's workflow, unless it builds the solo card
/// too, and then the default crew workflow.
pub fn crew_workflow(host: &Host) -> WorkflowRef {
    let configured = &host.settings().workflow;
    match lookup(host, configured.as_str()) {
        Ok(workflow) if builds_goal(&workflow) => WorkflowRef::new(DEFAULT_CREW_WORKFLOW),
        _ => configured.clone(),
    }
}

/// Where the crew goes when the preset's workflow is a solo one.
pub const DEFAULT_CREW_WORKFLOW: &str = "peer-crew";

/// Whether any step of the workflow builds the solo card from the goal.
pub fn builds_goal(workflow: &Workflow) -> bool {
    fn any(steps: &[Step]) -> bool {
        steps
            .iter()
            .any(|step| card::builds_goal(step) || any(&step.steps))
    }
    workflow.phases.iter().any(|phase| any(&phase.steps))
}

/// Values the settings give a workflow's own params: `attempts` from `solo.attempts` above 1.
/// Only params the workflow declares are filled, so its record never gains one it doesn't use.
pub fn settings_params(workflow: &Workflow, settings: &Settings) -> WorkflowParams {
    let mut values = WorkflowParams::default();
    if settings.solo.attempts > 1 && workflow.params.0.contains_key(ATTEMPTS) {
        values
            .0
            .insert(ATTEMPTS.to_owned(), Value::from(settings.solo.attempts));
    }
    values
}

/// The param `best-of-n` counts its attempts with.
pub const ATTEMPTS: &str = "attempts";

/// Where your own workflow files live: `.argonauts/workflows/` in the repo, then
/// `~/.argonauts/workflows/`.
pub fn workflow_dirs(host: &Host) -> Vec<PathBuf> {
    let mut dirs = vec![host.repo().root.join(".argonauts/workflows")];
    if let Ok(paths) = Paths::from_env() {
        dirs.push(paths.home().join("workflows"));
    }
    dirs
}

/// A workflow by name: `.argonauts/workflows/<name>.yaml`, else
/// `~/.argonauts/workflows/<name>.yaml`, else the built-in. Your own file with a built-in's
/// name replaces it. A `<name>.star` file in the same places builds its workflow with
/// Starlark's loops and functions, through learning's own small evaluator, and is validated
/// and run like any other. A recipe's own workflow file comes by its full path, and only from
/// beside the run's recipe version.
pub fn lookup(host: &Host, name: &str) -> EngineResult<Workflow> {
    if Path::new(name).is_absolute() {
        return recipe_workflow(host, Path::new(name));
    }
    let plain = Path::new(name)
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if name.is_empty() || !plain || name.contains('/') {
        return Err(EngineError::Refused(format!(
            "`{name}` isn't a workflow name"
        )));
    }
    let stem = name.strip_suffix(".star").unwrap_or(name);
    let file = format!("{stem}.yaml");
    let star = format!("{stem}.star");
    for dir in workflow_dirs(host) {
        let place = dir.join(&file);
        if stem == name {
            match std::fs::read_to_string(&place) {
                Ok(text) => {
                    return yaml::parse(&text).map_err(|err| {
                        EngineError::Refused(format!("{}: {err}", place.display()))
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        let place = dir.join(&star);
        if place.is_file() {
            return argonauts_learning::recipes::loops::build_file(&place)
                .map_err(|err| EngineError::Refused(format!("{}: {err}", place.display())));
        }
    }
    if builtin::source(name).is_some() {
        return builtin::load(name).map_err(EngineError::Refused);
    }
    Err(EngineError::Refused(format!(
        "no workflow is named `{name}`: neither .argonauts/workflows/ nor ~/.argonauts/workflows/ has {file}, and it isn't built in"
    )))
}

/// The `.star` or YAML workflow file beside the run's recipe version.
fn recipe_workflow(host: &Host, path: &Path) -> EngineResult<Workflow> {
    let refused = |why: String| EngineError::Refused(format!("{}: {why}", path.display()));
    let Some(file) = crate::retro::recipe::recipe_file(&host.repo().run_dir, path) else {
        return Err(refused(
            "a workflow named by its full path must be a file beside this run's recipe".to_owned(),
        ));
    };
    if file
        .extension()
        .is_some_and(|extension| extension == "star")
    {
        return argonauts_learning::recipes::loops::build_file(&file)
            .map_err(|err| refused(err.to_string()));
    }
    yaml::parse(&std::fs::read_to_string(&file)?).map_err(|err| refused(err.to_string()))
}

/// Workers a workflow may name: the default agent, the crew, and every role's workers.
pub fn allowed_workers(settings: &Settings) -> Vec<WorkerRef> {
    let mut allowed = vec![settings.default_agent.clone()];
    allowed.extend(settings.crew.iter().cloned());
    for role in settings.roles.values() {
        allowed.extend(role.workers.iter().cloned());
    }
    allowed.sort();
    allowed.dedup();
    allowed
}

/// SHA-256 of the definition's canonical JSON.
pub fn version(workflow: &Workflow) -> EngineResult<String> {
    let value = serde_json::to_value(workflow)?;
    Ok(sha256_hex(canonical_json(&value).as_bytes()))
}

/// Checks the workflow at `depth` against the run's settings and what's left of its budget.
pub fn check(
    host: &Host,
    workflow: &Workflow,
    depth: u32,
) -> Result<(), Vec<validate::WorkflowError>> {
    let settings = host.settings();
    let allowed = allowed_workers(settings);
    let budget_left_usd = host.read(|state| {
        let budget = state.run.budget.as_ref()?;
        let limit = budget.limit.as_ref()?;
        (limit.unit == argonauts_core::settings::budget::BudgetUnit::Usd)
            .then_some(limit.amount - budget.spent.cost_usd)
    });
    let families: BTreeMap<WorkerRef, String> = allowed
        .iter()
        .map(|worker| (worker.clone(), roles::family(host, worker)))
        .collect();
    let lookup = |name: &str| lookup(host, name).map_err(|err| err.to_string());
    validate::validate(
        workflow,
        &Context {
            settings,
            allowed: &allowed,
            families: &families,
            lookup: &lookup,
            depth,
            budget_left_usd,
        },
    )
}

/// Validates the workflow, records `Workflow::Started` (version = SHA-256 of its canonical
/// JSON), and returns its id. A nested one names the `workflow` step that started it.
pub fn start(
    host: &Host,
    from: Option<WorkflowRef>,
    workflow: Workflow,
    parent: Option<StepRef>,
) -> EngineResult<WorkflowId> {
    start_as(host, None, from, workflow, parent)
}

/// [`start`] under an id taken earlier, such as the run's own workflow after its goal was
/// aligned under that id.
pub fn start_as(
    host: &Host,
    id: Option<WorkflowId>,
    from: Option<WorkflowRef>,
    workflow: Workflow,
    parent: Option<StepRef>,
) -> EngineResult<WorkflowId> {
    // The run's own workflow comes by name, from the settings or a switch, so nothing has
    // filled its placeholders yet. A nested one was filled by its `workflow` step.
    let workflow = match parent {
        None => params::fill(&workflow, &settings_params(&workflow, host.settings())).map_err(
            |errors| {
                EngineError::Refused(format!(
                    "workflow `{}`: {}",
                    workflow.name,
                    errors.join("; ")
                ))
            },
        )?,
        Some(_) => workflow,
    };
    let version = version(&workflow)?;
    // A recovered run gets back the one it started. A `workflow` step finds its own child
    // before it gets here.
    let existing = host.read(|state| {
        if parent.is_some() {
            return None;
        }
        let workflows = &state.workflows;
        let candidate = workflows.by_id.get(workflows.root.as_ref()?)?;
        (!candidate.finished && candidate.from == from && candidate.version == version)
            .then(|| candidate.id.clone())
    });
    if let Some(id) = existing {
        return Ok(id);
    }
    let depth = match &parent {
        None => 0,
        Some(step) => host.read(|state| state.workflows.depth(&step.workflow)) + 1,
    };
    check(host, &workflow, depth).map_err(|errors| {
        EngineError::Refused(format!(
            "workflow `{}` isn't valid: {}",
            workflow.name,
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        ))
    })?;
    let id = match id {
        Some(id) => id,
        None => WorkflowId::numbered(host.allocate(IdKind::Workflow)?),
    };
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Started(Box::new(WorkflowStarted {
            id: id.clone(),
            from,
            version,
            parent,
            workflow,
        }))),
    )?;
    Ok(id)
}

/// Runs the workflow to its end from its first unfinished step, taking approved changes from
/// the next phase on. At each phase end of a crew run, with `workflows.adapt_at_phase_ends`,
/// the lead may propose a change (`change::adapt_at_phase_end`). A step that fails stops it.
pub fn run<'a>(host: &'a Host, id: &'a WorkflowId) -> BoxFuture<'a, EngineResult<Flow>> {
    Box::pin(async move {
        // A decision the log already holds stands, even when the engine stopped before acting
        // on it.
        let recorded = host.read(|state| {
            let record = state.workflows.by_id.get(id)?;
            Some(match replay::failed(record) {
                // A goal loop out of rounds failed, but the run it stopped isn't a failure.
                Some(progress) => match replay::step(state, host, record, progress) {
                    Flow::Continue => Err(failed(&progress.step, progress.summary.as_deref())),
                    flow => Ok(flow),
                },
                None => Ok(replay::workflow(state, host, record)),
            })
        });
        match recorded {
            Some(Err(err)) => return Err(err),
            Some(Ok(flow)) if flow != Flow::Continue => return Ok(flow),
            _ => {}
        }
        let depth = host.read(|state| state.workflows.depth(id));
        // A goal's attempts are planned from its checks instead.
        let adapts = host.settings().workflows.adapt_at_phase_ends
            && host.read(|state| crew_run(state, id) && !pursue::pursuing(state));
        let mut adapted = BTreeSet::new();
        loop {
            // Declared before its first phase, and again after an approved change.
            declare(host, id)?;
            let Some((workflow, next, started)) = host.read(|state| {
                let record = state.workflows.by_id.get(id)?;
                let next = record.next_phase();
                Some((record.workflow.clone(), next, record.phase_started(next)))
            }) else {
                return Err(EngineError::Refused(format!(
                    "no workflow {id} was started"
                )));
            };
            let Some(phase) = workflow.phases.get(next as usize) else {
                return Ok(Flow::Continue);
            };
            let mark = PhaseMark {
                id: id.clone(),
                index: next,
                title: phase.name.clone(),
            };
            // Asked when the next phase is about to begin, so a recovered run that stopped
            // between two phases still gets the chance, and an approved change takes effect
            // from this phase.
            if next > 0 && !started && adapts && adapted.insert(next) {
                change::adapt_at_phase_end(host, id, next - 1).await?;
                continue;
            }
            pursue::begin_attempt(host, id, next)?;
            for (index, step) in phase.steps.iter().enumerate() {
                let call = StepCall {
                    at: StepAt {
                        step: StepRef {
                            workflow: id.clone(),
                            phase: next,
                            path: vec![index as u32],
                        },
                        round: 1,
                    },
                    depth,
                    standard: phase.standard,
                    step: step.clone(),
                    item: None,
                };
                let done = run_step(host, &call).await?;
                // A failed step that ends the run itself, such as a goal never approved, says
                // how.
                if done.flow != Flow::Continue {
                    end_phase(host, &mark, done.end)?;
                    skip_the_rest(host, id, next + 1, &done.flow)?;
                    return Ok(done.flow);
                }
                if done.end == StepEnd::Failed {
                    end_phase(host, &mark, StepEnd::Failed)?;
                    return Err(failed(&call.at.step, done.summary.as_deref()));
                }
            }
            end_phase(host, &mark, StepEnd::Done)?;
        }
    })
}

/// Records what the workflow declares, its name, description, and phases, unless the log
/// already holds this version's declaration. Only declared phases exist in the views.
fn declare(host: &Host, id: &WorkflowId) -> EngineResult<()> {
    let Some(declared) = host.read(|state| {
        let record = state.workflows.by_id.get(id)?;
        (record.declared.as_ref() != Some(&record.version))
            .then(|| WorkflowDeclared::of(id, &record.workflow))
    }) else {
        return Ok(());
    };
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Declared(Box::new(declared))),
    )?;
    Ok(())
}

/// A phase's first step is starting: says so once.
fn start_phase(host: &Host, step: &StepRef) -> EngineResult<()> {
    if step.path.len() != 1 {
        return Ok(());
    }
    let Some(mark) = host.read(|state| {
        let record = state.workflows.by_id.get(&step.workflow)?;
        let phase = record.workflow.phases.get(step.phase as usize)?;
        (!record.phases_started.contains(&step.phase)).then(|| PhaseMark {
            id: step.workflow.clone(),
            index: step.phase,
            title: phase.name.clone(),
        })
    }) else {
        return Ok(());
    };
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::PhaseStarted(mark)),
    )?;
    Ok(())
}

/// The later phase a `build` names in `review_in`, when it has no steps of its own: it shows
/// the build's reviews, so it starts with the first one, beside the build's phase, as a
/// pipeline.
fn review_phase(state: &RunState, step: &StepRef) -> Option<PhaseMark> {
    let record = state.workflows.by_id.get(&step.workflow)?;
    let target = record.step_definition(step)?.review_in.as_deref()?;
    let index = record
        .workflow
        .phases
        .iter()
        .position(|phase| phase.name == target && phase.steps.is_empty())?;
    let index = u32::try_from(index)
        .ok()
        .filter(|index| *index > step.phase)?;
    Some(PhaseMark {
        id: step.workflow.clone(),
        index,
        title: target.to_owned(),
    })
}

/// The step that built `card`: where its owner's build sessions run. A prediction made while
/// the plan was divided names the card too, from the planning step, so it isn't one.
pub fn card_step(state: &RunState, card: &CardId) -> Option<StepRef> {
    state.sessions.in_order().into_iter().find_map(|session| {
        (session.card.as_ref() == Some(card) && session.started.step == Some(StepName::Build))
            .then(|| session.started.workflow_step.clone())
            .flatten()
    })
}

/// Where a card's review shows in the workflow: the later phase its build's `review_in`
/// names, else the build's own step, in the round the build is in.
pub fn review_at(state: &RunState, card: &CardId) -> Option<StepAt> {
    let step = card_step(state, card)?;
    if let Some(mark) = review_phase(state, &step) {
        return Some(StepAt {
            step: StepRef {
                workflow: mark.id,
                phase: mark.index,
                path: Vec::new(),
            },
            round: 1,
        });
    }
    let round = state
        .workflows
        .by_id
        .get(&step.workflow)?
        .steps
        .iter()
        .filter(|progress| progress.step == step)
        .map(|progress| progress.round)
        .max()
        .unwrap_or(1);
    Some(StepAt { step, round })
}

/// A card's review is starting under `step`: the phase that shows it starts, once.
pub fn review_started(host: &Host, step: &StepRef) -> EngineResult<()> {
    let Some(mark) = host.read(|state| {
        review_phase(state, step).filter(|mark| {
            state
                .workflows
                .by_id
                .get(&mark.id)
                .is_some_and(|record| !record.phases_started.contains(&mark.index))
        })
    }) else {
        return Ok(());
    };
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::PhaseStarted(mark)),
    )?;
    Ok(())
}

/// The build is over, and with it the reviews its later phase showed.
fn end_review_phase(host: &Host, step: &StepRef) -> EngineResult<()> {
    let Some(mark) = host.read(|state| {
        review_phase(state, step).filter(|mark| {
            state.workflows.by_id.get(&mark.id).is_some_and(|record| {
                record.phases_started.contains(&mark.index)
                    && !record.phases_ended.contains(&mark.index)
            })
        })
    }) else {
        return Ok(());
    };
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::PhaseEnded {
            phase: mark,
            end: StepEnd::Done,
        }),
    )?;
    Ok(())
}

/// A phase's steps are over. One whose every step was skipped, such as solo's Understand when
/// your prompt is the goal, never ran, so it's recorded as skipped with the first reason.
fn end_phase(host: &Host, mark: &PhaseMark, end: StepEnd) -> EngineResult<()> {
    let Some(skipped) = host.read(|state| {
        let record = state.workflows.by_id.get(&mark.id)?;
        if record.phases_ended.contains(&mark.index) || record.phases_skipped.contains(&mark.index)
        {
            return None;
        }
        let tops: Vec<_> = record
            .steps
            .iter()
            .filter(|progress| progress.step.phase == mark.index && progress.step.path.len() == 1)
            .collect();
        let all_skipped = !tops.is_empty()
            && tops
                .iter()
                .all(|progress| progress.end == Some(StepEnd::Skipped));
        Some(all_skipped.then(|| {
            tops.iter()
                .find_map(|progress| progress.summary.clone())
                .unwrap_or_else(|| "nothing in it had to run".to_owned())
        }))
    }) else {
        return Ok(());
    };
    let event = match skipped {
        Some(why) if end != StepEnd::Failed => WorkflowEvent::PhaseSkipped {
            phase: mark.clone(),
            why,
        },
        _ => WorkflowEvent::PhaseEnded {
            phase: mark.clone(),
            end,
        },
    };
    host.record(None, None, RunEventKind::Workflow(event))?;
    Ok(())
}

/// The phases after `from` that won't run now the run ended or switched, recorded as skipped
/// so no view waits on them.
fn skip_the_rest(host: &Host, id: &WorkflowId, from: u32, flow: &Flow) -> EngineResult<()> {
    let why = match flow {
        Flow::Switch(to) => format!("the run switched to {to}"),
        Flow::Finish(_) | Flow::Continue => "the run ended before it".to_owned(),
    };
    let marks = host.read(|state| {
        let Some(record) = state.workflows.by_id.get(id) else {
            return Vec::new();
        };
        record
            .workflow
            .phases
            .iter()
            .enumerate()
            .skip(from as usize)
            .filter_map(|(index, phase)| {
                let index = u32::try_from(index).ok()?;
                let recorded = record.phases_started.contains(&index)
                    || record.phases_ended.contains(&index)
                    || record.phases_skipped.contains(&index);
                (!recorded).then(|| PhaseMark {
                    id: id.clone(),
                    index,
                    title: phase.name.clone(),
                })
            })
            .collect::<Vec<_>>()
    });
    for mark in marks {
        host.record(
            None,
            None,
            RunEventKind::Workflow(WorkflowEvent::PhaseSkipped {
                phase: mark,
                why: why.clone(),
            }),
        )?;
    }
    Ok(())
}

/// A failed step ends the run, with its reason.
fn failed(step: &StepRef, summary: Option<&str>) -> EngineError {
    EngineError::Refused(format!(
        "{} failed: {}",
        describe(step),
        summary.unwrap_or("it gave no reason")
    ))
}

/// Checks `when`, records `StepStarted`, runs the step's executor, and records `StepFinished`
/// once it returns. Loops, `for_each`, and nested workflows call it for their inner steps. A
/// round that already finished returns what it recorded, and what it decided, without running
/// again.
pub fn run_step<'a>(host: &'a Host, call: &'a StepCall) -> BoxFuture<'a, EngineResult<StepDone>> {
    Box::pin(async move {
        let recorded = host.read(|state| {
            let record = state.workflows.by_id.get(call.workflow())?;
            let progress = record.round(&call.at.step, call.at.round)?;
            Some(progress.end.map(|end| StepDone {
                end,
                summary: progress.summary.clone(),
                flow: replay::step(state, host, record, progress),
            }))
        });
        match recorded {
            Some(Some(done)) => return Ok(done),
            // Started before a crash or a pause. A step whose `when` didn't hold never starts,
            // so this one's held.
            Some(None) => {}
            None => {
                if let Some(done) = understand_off(host, call).await? {
                    record_finished(host, call, &done)?;
                    return Ok(done);
                }
                let holds = match &call.step.when {
                    Some(condition) => host
                        .read(|state| {
                            conditions::holds(state, host.settings(), condition, &call.at)
                        })
                        .map_err(EngineError::Refused)?,
                    None => true,
                };
                if !holds {
                    let condition = call.step.when.as_deref().unwrap_or_default();
                    let done = StepDone::skipped(format!("{condition} doesn't hold"));
                    record_finished(host, call, &done)?;
                    return Ok(done);
                }
                start_phase(host, &call.at.step)?;
                record_started(host, call)?;
            }
        }
        // An error leaves the round unfinished: a run that crashes on it runs the step again
        // when it resumes, and a run that fails on it never resumes.
        let done = kinds::execute(host, call).await?;
        record_finished(host, call, &done)?;
        end_review_phase(host, &call.at.step)?;
        Ok(done)
    })
}

/// With `understand.when = "off"`, a crew run's Understand phase doesn't run: its steps skip,
/// and your prompt is the goal. The goal checkpoint code put before the work takes it and asks
/// the go-ahead; a workflow without one, which code didn't shape, takes it here.
async fn understand_off(host: &Host, call: &StepCall) -> EngineResult<Option<StepDone>> {
    let off = host.settings().understand.when == UnderstandWhen::Off
        && call.standard == StandardPhase::Understand
        && call.at.step.path.len() == 1
        && host.read(run_mode) == Some(RunMode::Crew);
    if !off {
        return Ok(None);
    }
    let asked_later = host.read(|state| {
        state
            .workflows
            .by_id
            .get(&call.at.step.workflow)
            .is_some_and(|record| {
                shape::goal_checkpoint_runs(&record.workflow, UnderstandWhen::Off)
            })
    });
    if !asked_later {
        crate::solo::prompt_is_the_goal(host).await?;
    }
    Ok(Some(StepDone::skipped(
        "understand.when is off, so your prompt is the goal",
    )))
}

fn record_started(host: &Host, call: &StepCall) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::StepStarted(StepStarted {
            step: call.at.step.clone(),
            kind: call.step.kind,
            standard: call.standard,
            round: call.at.round,
        })),
    )?;
    Ok(())
}

fn record_finished(host: &Host, call: &StepCall, done: &StepDone) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::StepFinished(StepFinished {
            step: call.at.step.clone(),
            kind: call.step.kind,
            standard: call.standard,
            round: call.at.round,
            end: done.end,
            summary: done.summary.clone(),
        })),
    )?;
    Ok(())
}
