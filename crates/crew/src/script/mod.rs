//! A crew run that follows the workflow in use by its own steps: a file of `[[step]]` entries,
//! or a script. The runner executes it in its sandbox, and here each `agent()` becomes a
//! session on the CLI and model its role names. Workers build in worktrees of their own as
//! tasks, with their checks; what the workflow's reviews accepted is merged into the run, and
//! the merge approval follows as in any run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use argonauts_core::events::run::RunMode;
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::WorkflowId;
use argonauts_core::paths::Paths;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::workflow::{Workflow, WorkflowRef};
use argonauts_runner::{
    Library, LibraryError, Limits, Loaded, Required, RunInput, Script, ScriptEnd, WorkflowFile,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::captain;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::understand::round;
use crate::workflow::runner::run_mode;
use crate::workflow::{StepCall, StepDone, naming, yaml};
use crate::workflow_size;
use crate::{claude_codes, ultra};

mod bridge;
pub mod goal;
mod journal;
mod plan;
pub mod required;
mod tasks;
pub(crate) mod write;

use bridge::Bridge;

/// The phases a run of this kind has: the workflow's own steps as one step, then the merge.
const PHASES: &str = include_str!("phases.yaml");

/// What the phases file is recorded as having come from.
pub const PHASES_NAME: &str = "script";

/// The two folders a workflow of your own may sit in.
pub fn library(host: &Host) -> Library {
    Library {
        project: Some(host.repo().root.join(".argonauts/workflows")),
        yours: Paths::from_env()
            .ok()
            .map(|paths| paths.home().join("workflows")),
        waiting: crate::prompts::files::waiting(host),
    }
}

/// The workflow in use, when the runner is what runs it: a script, a file whose steps run as
/// written, or a file the orchestrator writes a script for. `None` leaves the run to the phases
/// of the workflow's name. A Captain run is the CLI itself and never comes here.
pub fn for_run(host: &Host) -> EngineResult<Option<Loaded>> {
    if host.read(run_mode) == Some(RunMode::Captain) {
        return Ok(None);
    }
    let settings = host.settings();
    let loaded = match library(host).load(&settings.workflow) {
        Ok(loaded) => loaded,
        Err(LibraryError::Unknown(_) | LibraryError::NoSteps { .. }) => return Ok(None),
        Err(err) => return Err(EngineError::Refused(err.to_string())),
    };
    let runs = match &loaded {
        Loaded::Script(_) => true,
        Loaded::Steps(file) => !file.adapt || settings.workflows.adapt_with == AdaptWith::Script,
    };
    Ok(runs.then_some(loaded))
}

/// Whether the runner runs this workflow's steps. Its phases are the two every such run has,
/// and they aren't the main agent's to change at a phase end: the steps are the workflow's own.
pub fn runs(state: &RunState, id: &WorkflowId) -> bool {
    state
        .workflows
        .by_id
        .get(id)
        .and_then(|record| record.from.as_ref())
        .is_some_and(|from| from.as_str() == PHASES_NAME)
}

/// Another workflow a script runs as a step, by its name in the library.
fn workflow_named(host: &Host, name: &str) -> Result<Script, String> {
    match library(host).load(name) {
        Ok(Loaded::Script(script)) => Ok(script),
        Ok(Loaded::Steps(file)) => Ok(Script::from_steps(&file)),
        Err(err) => Err(err.to_string()),
    }
}

/// The run's phases when the runner runs its workflow, under the run's own name.
pub fn phases(host: &Host) -> EngineResult<(WorkflowRef, Workflow)> {
    let workflow = yaml::parse(PHASES).map_err(EngineError::Refused)?;
    let from = WorkflowRef::new(PHASES_NAME);
    Ok((from.clone(), naming::as_run(host, Some(&from), workflow)))
}

/// `script`: runs the workflow in use to its end, then merges what it accepted into the run.
pub async fn script_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    // The steps say who checks what, so the request stands as the goal.
    captain::prompt_is_the_goal(host).await?;
    let loaded = for_run(host)?.ok_or_else(|| {
        EngineError::Refused(format!(
            "the workflow in use, {}, has no steps to run",
            host.settings().workflow
        ))
    })?;
    let request = host.read(round::request)?;
    // The engine follows the crew: with the orchestrator and the workers on Claude Code, a
    // workflow the orchestrator shapes is Claude Code's own, and the runner stays out.
    if let Loaded::Steps(file) = &loaded
        && ultra::plans_and_delegates(file)
        && let Some(crew) =
            ultra::claude_codes_own(host.settings(), &host.services().workers.available())
    {
        return claude_codes::run(host, call, &request, &crew).await;
    }
    let required = required::for_run(host, &request);
    let (script, roles) = loop {
        let notes = plan::sent_back(host, call);
        let (script, roles, wrote) =
            steps_of(host, call, &loaded, &required, &request, &notes).await?;
        match plan::approval(host, call, &script, &wrote, &notes).await? {
            plan::Verdict::Approved => break (script, roles),
            plan::Verdict::SentBack => {}
            plan::Verdict::Ended(done) => return Ok(done),
        }
    };

    let stop = CancellationToken::new();
    let mut bridge = Bridge::new(host, call, stop.clone());
    // A file that fixes its own steps keeps its own counts; only steps the orchestrator wrote
    // keep to the size the request asked for.
    if roles.is_some() {
        bridge.size_asked = workflow_size::asked(&request).map(|asked| asked.said());
    }
    if let Loaded::Steps(file) = &loaded {
        bridge.max_rounds = max_rounds(file);
        bridge.own_workflow_in = file
            .steps
            .iter()
            .filter(|step| step.own_workflow)
            .map(|step| step.name.clone())
            .collect();
    }
    // What the orchestrator didn't write is the workflow's own words.
    if roles.is_none() {
        bridge.workflow_words = Some(script.meta.name.clone());
    }
    let input = RunInput {
        id: call.workflow().to_string(),
        args: args(host, &request, &required.tasks),
        limits: limits(host),
        replay: journal::read(host, call)?,
        roles,
        token_limit: None,
    };
    let ended = tokio::select! {
        ended = argonauts_runner::run(&script, input, &bridge) => ended,
        () = stop.cancelled() => return Err(bridge.take_error()),
    };
    // An engine error ends the run's stretch even when the script went on past it.
    if let Some(err) = bridge.error() {
        return Err(err);
    }
    match ended.result {
        Ok(result) => {
            // A step you require that never put its role to work in the run goes first among
            // what's left, as one Claude Code's own workflow lacked does.
            let never_ran = bridge.never_ran(&required::shared(&required).steps);
            let lacked = (!never_ran.is_empty()).then(|| {
                let names: Vec<String> =
                    never_ran.iter().map(|name| format!("\"{name}\"")).collect();
                format!("It never ran {}, which you require.", names.join(", "))
            });
            if let Some(line) = &lacked {
                claude_codes::note_left(host, call, line.clone())?;
            }
            let merged = tasks::finish(&bridge, &call.at).await?;
            let mut said = summary(&script, &result, merged);
            if let Some(line) = lacked {
                said.push_str(&format!(" · {line}"));
            }
            Ok(StepDone::done(said))
        }
        Err(ScriptEnd::Stopped) => Err(EngineError::Interrupted),
        Err(why) => Ok(StepDone {
            end: StepEnd::Failed,
            ..StepDone::done(format!("{} stopped: {why}", script.meta.name))
        }),
    }
}

/// What a script may use up in this run.
fn limits(host: &Host) -> Limits {
    let settings = host.settings();
    Limits {
        at_once: settings.concurrency.max_sessions.max(1) as usize,
        // `workflows.max_depth` counts the run's own workflow, so 1 means none inside it.
        nested: settings.workflows.max_depth > 1,
        ..Limits::default()
    }
}

/// The script the run follows, the roles its agents may name when the orchestrator wrote it,
/// and who wrote it.
async fn steps_of(
    host: &Host,
    call: &StepCall,
    loaded: &Loaded,
    required: &Required,
    request: &str,
    notes: &[String],
) -> EngineResult<(Script, Option<BTreeSet<CrewRole>>, plan::Wrote)> {
    match loaded {
        Loaded::Script(script) => {
            let missing = required.missing_from(&script.meta);
            if !missing.is_empty() {
                return Err(EngineError::Refused(format!(
                    "{} has no phase for a step you require: {}",
                    script.meta.name,
                    missing.join(", ")
                )));
            }
            Ok((script.clone(), None, plan::Wrote::File))
        }
        Loaded::Steps(file) if file.adapt => {
            let file = required.added_to(file);
            let (written, wrote) =
                write::script_for(host, call, &file, required, request, notes).await?;
            // Steps that run as written keep to themselves, so only a script is held to the
            // file's roles.
            let roles = matches!(wrote, plan::Wrote::Orchestrator(_)).then(|| file.roles());
            Ok((written, roles, wrote))
        }
        Loaded::Steps(file) => {
            let mut file = required.added_to(file);
            // A file that states no goal goes round the one the run was started with.
            if file.goal.is_none() {
                file.goal = crate::goal::until::for_run(host);
            }
            Ok((Script::from_steps(&file), None, plan::Wrote::File))
        }
    }
}

/// The most tries a file of steps allows: the whole file under `None`, each step by its name.
fn max_rounds(file: &WorkflowFile) -> BTreeMap<Option<String>, u32> {
    let steps = file
        .steps
        .iter()
        .filter_map(|step| Some((Some(step.name.clone()), step.max_rounds?)));
    file.max_rounds
        .map(|rounds| (None, rounds))
        .into_iter()
        .chain(steps)
        .collect()
}

/// What a file of steps reads: the request, how many models each role has, and a task list you
/// fixed.
fn args(host: &Host, request: &str, tasks: &[String]) -> Value {
    let settings = host.settings();
    let crew: serde_json::Map<String, Value> = CrewRole::ALL
        .into_iter()
        .map(|role| {
            let models = crate::roles::candidates(settings, role.fine_role()).len();
            (role.as_str().to_owned(), json!(models))
        })
        .collect();
    json!({"request": request, "crew": crew, "tasks": tasks})
}

fn summary(script: &Script, result: &Value, merged: usize) -> String {
    let tasks = match merged {
        0 => "nothing to merge".to_owned(),
        1 => "1 task merged into the run".to_owned(),
        n => format!("{n} tasks merged into the run"),
    };
    let notes: Vec<&str> = result
        .get("notes")
        .and_then(Value::as_array)
        .map(|notes| notes.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if notes.is_empty() {
        format!("{} ran to its end · {tasks}", script.meta.name)
    } else {
        format!(
            "{} ran to its end · {tasks} · {}",
            script.meta.name,
            notes.join(" · ")
        )
    }
}

/// Where a run keeps what belongs to its workflow's script.
fn folder(host: &Host) -> PathBuf {
    host.repo().run_dir.join("workflow")
}

/// The file's steps under its own name, for messages.
fn named(file: &WorkflowFile) -> &str {
    if file.name.is_empty() {
        "the workflow"
    } else {
        &file.name
    }
}
