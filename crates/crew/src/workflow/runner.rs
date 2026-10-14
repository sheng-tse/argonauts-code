//! Walks a workflow's phases and steps from where the log says it stopped, so a recovered run
//! re-enters the unfinished step and skips the finished ones.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::RunMode;
use argonauts_core::events::workflow::{
    PhaseMark, StepEnd, StepFinished, StepStarted, WorkflowDeclared, WorkflowEvent, WorkflowStarted,
};
use argonauts_core::future::BoxFuture;
use argonauts_core::names::{ModelRef, WorkflowId};
use argonauts_core::paths::Paths;
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::UnderstandWhen;
use argonauts_core::workflow::{
    StandardPhase, Step, StepRef, Workflow, WorkflowParams, WorkflowRef,
};
use argonauts_core::{canonical_json, sha256_hex, workflow_files};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::goal::pursue;
use crate::roles;
use crate::state::RunState;
use crate::state::ids::IdKind;
use crate::task;
use crate::workflow::validate::{self, Context};
use crate::workflow::{
    Flow, StepAt, StepCall, StepDone, builtin, change, conditions, describe, kinds, params, replay,
    shape, yaml,
};
use argonauts_core::steps::StepName;

/// The run's workflow from the settings: the one `workflow` picks, looked up like any other. A
/// Captain run keeps that workflow when it builds the Captain task, and otherwise follows the
/// Captain built-in its `captain` settings pick. A run that already started one (a recovered run,
/// or one that switched) gets that one back.
pub fn workflow_for_run(host: &Host) -> EngineResult<(Option<WorkflowRef>, Workflow)> {
    let running = host.read(|state| {
        let root = state.workflows.root.as_ref()?;
        let record = state.workflows.by_id.get(root)?;
        Some((record.from.clone(), record.workflow.clone()))
    });
    if let Some(running) = running {
        return Ok(running);
    }
    let in_use = phases_in_use(host);
    let configured = lookup(host, in_use.as_str());
    if host.read(run_mode) != Some(RunMode::Captain) {
        return Ok((Some(in_use), configured?));
    }
    // A crew workflow that can't be found doesn't matter to a Captain run that wouldn't use it.
    if let Ok(workflow) = configured
        && builds_goal(&workflow)
    {
        return Ok((Some(in_use), workflow));
    }
    let name = WorkflowRef::new(captain_workflow(host.settings()));
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

/// A crew run following a crew workflow. Captain runs, and runs that switched to a Captain workflow
/// (such as "run it as Captain" at the plan approval), are not. Phase-end adaptation runs only in
/// these.
pub fn crew_run(state: &RunState, id: &WorkflowId) -> bool {
    run_mode(state) != Some(RunMode::Captain) && !in_captain_workflow(state, id)
}

/// Whether the workflow, or one it runs inside, builds the Captain task.
pub fn in_captain_workflow(state: &RunState, id: &WorkflowId) -> bool {
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

/// Which built-in a Captain run follows when the chosen workflow doesn't build the Captain task,
/// and where "run it as Captain" at the plan approval goes.
pub fn captain_workflow(settings: &Settings) -> &'static str {
    if settings.captain.tries > 1 {
        "best-of-n"
    } else if settings.captain.review {
        "maker-checker"
    } else {
        "captain"
    }
}

/// Where a Captain task that needs you goes: the workflow it names, unless it builds the Captain
/// task too, and then the default crew workflow.
pub fn crew_workflow(host: &Host) -> WorkflowRef {
    let configured = phases_in_use(host);
    match lookup(host, configured.as_str()) {
        Ok(workflow) if builds_goal(&workflow) => WorkflowRef::new(DEFAULT_CREW_WORKFLOW),
        _ => configured,
    }
}

/// The workflow whose phases a run starts from: the one in use when it has phases, and
/// otherwise the default crew workflow. A file of steps, a script and a workflow that only sets
/// settings have none of their own.
pub fn phases_in_use(host: &Host) -> WorkflowRef {
    let name = host.settings().workflow.as_str();
    if has_phases(host, name) {
        WorkflowRef::new(name)
    } else {
        WorkflowRef::new(DEFAULT_CREW_WORKFLOW)
    }
}

/// Whether `lookup` has phases to find under this name. A project's file that waits for your
/// trust doesn't count, as in the lookup.
fn has_phases(host: &Host, name: &str) -> bool {
    if Path::new(name).is_absolute() || builtin::source(name).is_some() {
        return true;
    }
    let stem = name.strip_suffix(".star").unwrap_or(name);
    let waiting = crate::prompts::files::waiting(host);
    workflow_dirs(host).iter().any(|dir| {
        let there = |file: String| {
            let place = dir.join(file);
            (place.is_file() && !waiting.contains(&place)).then_some(place)
        };
        let own_file = there(format!("{stem}.toml"))
            .and_then(|place| std::fs::read_to_string(place).ok())
            .is_some_and(|text| workflow_files::has_table(&text, workflow_files::PHASES_TABLE));
        own_file
            || there(format!("{stem}.yaml")).is_some()
            || there(format!("{stem}.star")).is_some()
    })
}

/// Where the crew goes when the chosen workflow is a Captain one.
pub const DEFAULT_CREW_WORKFLOW: &str = "council";

/// Whether any step of the workflow builds the Captain task from the goal.
pub fn builds_goal(workflow: &Workflow) -> bool {
    fn any(steps: &[Step]) -> bool {
        steps
            .iter()
            .any(|step| task::builds_goal(step) || any(&step.steps))
    }
    workflow.phases.iter().any(|phase| any(&phase.steps))
}

/// Values the settings give a workflow's own params: `tries` from `captain.tries` above 1.
/// Only params the workflow declares are filled, so its record never gains one it doesn't use.
pub fn settings_params(workflow: &Workflow, settings: &Settings) -> WorkflowParams {
    let mut values = WorkflowParams::default();
    if settings.captain.tries > 1 && workflow.params.0.contains_key(TRIES) {
        values
            .0
            .insert(TRIES.to_owned(), Value::from(settings.captain.tries));
    }
    values
}

/// The param `best-of-n` counts its tries with.
pub const TRIES: &str = "tries";

/// Where your own workflow files live: `.argonauts/workflows/` in the repo, then
/// `~/.argonauts/workflows/`.
pub fn workflow_dirs(host: &Host) -> Vec<PathBuf> {
    let mut dirs = vec![host.repo().root.join(".argonauts/workflows")];
    if let Ok(paths) = Paths::from_env() {
        dirs.push(paths.home().join("workflows"));
    }
    dirs
}

/// A workflow's phases by name: the `[[phases]]` of `.argonauts/workflows/<name>.toml`, else
/// `<name>.yaml` beside it, a file of phases alone, else the same two in
/// `~/.argonauts/workflows/`, else the built-in. Your own file with a built-in's name replaces
/// it. A `<name>.star` file in the same places builds its workflow with Starlark's loops and
/// functions, through learning's own small evaluator, and is validated and run like any other.
/// A learned workflow's own workflow file comes by its full path, and only from beside the run's
/// learned workflow version.
pub fn lookup(host: &Host, name: &str) -> EngineResult<Workflow> {
    if Path::new(name).is_absolute() {
        return workflow_beside_learned(host, Path::new(name));
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
    let own_file = format!("{stem}.toml");
    let file = format!("{stem}.yaml");
    let star = format!("{stem}.star");
    // A project's file that waits for your trust is passed over, so a built-in stays in place.
    let waiting = crate::prompts::files::waiting(host);
    let read = |place: &Path| match std::fs::read_to_string(place) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(EngineError::from(err)),
    };
    for dir in workflow_dirs(host) {
        let refused =
            |place: &Path, err: String| EngineError::Refused(format!("{}: {err}", place.display()));
        // A workflow's own file may leave its phases out: it then only sets steps or settings.
        let place = dir.join(&own_file);
        if stem == name
            && !waiting.contains(&place)
            && let Some(text) = read(&place)?
            && let Some(workflow) =
                workflow_files::phases(&text, stem).map_err(|err| refused(&place, err))?
        {
            return Ok(workflow);
        }
        let place = dir.join(&file);
        if stem == name
            && !waiting.contains(&place)
            && let Some(text) = read(&place)?
        {
            return yaml::parse(&text).map_err(|err| refused(&place, err));
        }
        let place = dir.join(&star);
        if place.is_file() && !waiting.contains(&place) {
            return argonauts_learning::learned_workflows::loops::build_file(&place)
                .map_err(|err| EngineError::Refused(format!("{}: {err}", place.display())));
        }
    }
    if builtin::source(name).is_some() {
        return builtin::load(name).map_err(EngineError::Refused);
    }
    Err(EngineError::Refused(format!(
        "no workflow is named `{name}`: neither .argonauts/workflows/ nor ~/.argonauts/workflows/ has {own_file} with phases or {file}, and it isn't built in"
    )))
}

/// The `.star` or YAML workflow file beside the run's learned workflow version.
fn workflow_beside_learned(host: &Host, path: &Path) -> EngineResult<Workflow> {
    let refused = |why: String| EngineError::Refused(format!("{}: {why}", path.display()));
    let Some(file) =
        crate::run_review::learned_workflow::learned_workflow_file(&host.repo().run_dir, path)
    else {
        return Err(refused(
            "a phases file named by its full path must be a file beside this run's learned workflow".to_owned(),
        ));
    };
    if file
        .extension()
        .is_some_and(|extension| extension == "star")
    {
        return argonauts_learning::learned_workflows::loops::build_file(&file)
            .map_err(|err| refused(err.to_string()));
    }
    yaml::parse(&std::fs::read_to_string(&file)?).map_err(|err| refused(err.to_string()))
}

/// Workers a workflow may name: the main agent, the crew, and every role's workers.
pub fn allowed_workers(settings: &Settings) -> Vec<ModelRef> {
    let mut allowed = vec![settings.crew.main.clone()];
    allowed.extend(settings.crew.members());
    for role in settings.roles.values() {
        allowed.extend(role.models.iter().cloned());
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

/// Checks the workflow at `depth` against the run's settings and what's left of its limit.
pub fn check(
    host: &Host,
    workflow: &Workflow,
    depth: u32,
) -> Result<(), Vec<validate::WorkflowError>> {
    let settings = host.settings();
    let allowed = allowed_workers(settings);
    let limit_left_usd = host.read(|state| {
        let spend = state.run.spend.as_ref()?;
        let limit = spend.limit.as_ref()?;
        (limit.unit == argonauts_core::settings::limit::LimitUnit::Usd)
            .then_some(limit.amount - spend.spent.cost_usd)
    });
    let providers: BTreeMap<ModelRef, String> = allowed
        .iter()
        .map(|worker| (worker.clone(), roles::provider(host, worker)))
        .collect();
    let lookup = |name: &str| lookup(host, name).map_err(|err| err.to_string());
    let as_you_asked = host.read(|state| {
        let instructions = state
            .run
            .started
            .as_ref()
            .map(|started| started.instructions.as_slice())
            .unwrap_or_default();
        validate::roles_as_you_asked(instructions, settings)
    });
    validate::validate(
        workflow,
        &Context {
            settings,
            allowed: &allowed,
            providers: &providers,
            lookup: &lookup,
            depth,
            limit_left_usd,
            as_you_asked: &as_you_asked,
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
/// the main agent may propose a change (`change::adapt_at_phase_end`). A step that fails stops it.
pub fn run<'a>(host: &'a Host, id: &'a WorkflowId) -> BoxFuture<'a, EngineResult<Flow>> {
    Box::pin(async move {
        // A decision the log already holds stands, even when the engine stopped before acting
        // on it.
        let recorded = host.read(|state| {
            let record = state.workflows.by_id.get(id)?;
            Some(match replay::failed(record) {
                // An Understand loop out of rounds failed, but the run it stopped isn't a failure.
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
        let adapts = host.settings().workflows.adapt_at_phase_ends
            && host.read(|state| crew_run(state, id) && !crate::script::runs(state, id));
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
                name: phase.name.clone(),
            };
            // Asked when the next phase is about to begin, so a recovered run that stopped
            // between two phases still gets the chance, and an approved change takes effect
            // from this phase.
            // A goal's later rounds are planned from its checks instead.
            if next > 0
                && !started
                && adapts
                && !host.read(pursue::plans_its_rounds)
                && adapted.insert(next)
            {
                change::adapt_at_phase_end(host, id, next - 1).await?;
                continue;
            }
            // A goal's check may have gone in before this very phase.
            if pursue::begin_try(host, id, next)? {
                continue;
            }
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
    let Some(mut declared) = host.read(|state| {
        let record = state.workflows.by_id.get(id)?;
        (record.declared.as_ref() != Some(&record.version))
            .then(|| WorkflowDeclared::of(id, &record.workflow))
    }) else {
        return Ok(());
    };
    if host.read(|state| crate::script::runs(state, id)) {
        declared.required = crate::script::required::names(host);
    }
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
            name: phase.name.clone(),
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
        name: target.to_owned(),
    })
}

/// The step that built `task`: where its worker's build sessions run. A prediction made while
/// the plan was divided names the task too, from the planning step, so it isn't one.
pub fn task_step(state: &RunState, task: &TaskId) -> Option<StepRef> {
    state.sessions.in_order().into_iter().find_map(|session| {
        (session.task.as_ref() == Some(task) && session.started.step == Some(StepName::Build))
            .then(|| session.started.workflow_step.clone())
            .flatten()
    })
}

/// Where a task's review shows in the workflow: the later phase its build's `review_in`
/// names, else the build's own step, in the round the build is in.
pub fn review_at(state: &RunState, task: &TaskId) -> Option<StepAt> {
    let step = task_step(state, task)?;
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

/// A task's review is starting under `step`: the phase that shows it starts, once.
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

/// A phase's steps are over. One whose every step was skipped, such as Captain's Understand when
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
                    name: phase.name.clone(),
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
/// and your prompt is the goal. The Understand approval code put before the work takes it and asks
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
                shape::understand_approval_runs(&record.workflow, UnderstandWhen::Off)
            })
    });
    if !asked_later {
        crate::captain::prompt_is_the_goal(host).await?;
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
