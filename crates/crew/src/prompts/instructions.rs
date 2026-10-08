//! The orchestrator's own instructions for one agent. They enter an agent's prompt here and
//! nowhere else: a task's brief and a composed step's words through [`from_orchestrator`], and
//! the words of a workflow's `agent()` call through [`of_call`], or through
//! [`brief_written_by`] when the call starts a task. So whatever plans the work has one place
//! to hand its words over, and to say when they are a workflow file's own instead.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use argonauts_core::CardId;
use argonauts_core::events::card::CardKind;
use argonauts_core::events::session::InputPart;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::prompt_parts as parts;
use argonauts_core::settings::Settings;
use argonauts_core::steps::StepName;

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::roles;
use crate::workflow::StepAt;

/// The agent about to start, as the run knows it.
#[derive(Debug, Clone, Copy)]
pub struct Agent<'a> {
    pub role: RoleName,
    pub step: Option<StepName>,
    /// Its place in the workflow, when it serves a step.
    pub at: Option<&'a StepAt>,
    /// The task it works on, when it has one.
    pub card: Option<&'a CardId>,
}

/// What the orchestrator wrote for one agent, and which model wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub text: String,
    pub by: WorkerRef,
}

/// Who wrote the words a workflow's `agent()` call carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallAuthor<'a> {
    /// The orchestrator, in the script it wrote for this request.
    Orchestrator,
    /// The workflow itself: a step's `does` in a file whose steps run as written, or a script
    /// of yours.
    Workflow {
        name: &'a str,
        /// The step or phase the call belongs to, when it has one.
        step: Option<&'a str>,
    },
}

/// The orchestrator's instructions for `agent`, when it wrote any.
///
/// They are the brief it writes for a task's worker, when it divides the plan or when a
/// workflow's `agent()` call starts a task, and what it wrote on a step of a workflow it
/// composed. A call that starts no task hands its words over through [`of_call`] instead. A
/// solo task holds your own request, which nobody wrote for the worker, so it stays where it
/// is, and a brief that is a workflow file's own words ([`brief_written_by`]) isn't the
/// orchestrator's either.
pub fn from_orchestrator(host: &Host, agent: &Agent<'_>) -> Option<Written> {
    let text = if builds_a_task(agent) {
        if brief_from_workflow(host, agent.card?).is_some() {
            return None;
        }
        brief(host, agent)
    } else {
        composed_step(host, agent)
    }?;
    Some(Written {
        text,
        by: orchestrator(host.settings()),
    })
}

/// What a task's worker leads its prompt with: the task's brief, under the name of whoever
/// wrote it. `None` for a solo task and for a brief that says nothing.
pub fn of_task(host: &Host, agent: &Agent<'_>) -> Option<InputPart> {
    if let Some(step) = agent.card.and_then(|task| brief_from_workflow(host, task)) {
        return Some(parts::from_workflow(brief(host, agent)?, step));
    }
    let written = from_orchestrator(host, agent)?;
    let model = roles::short_name(&written.by);
    Some(parts::from_orchestrator(written.text, Some(model)))
}

fn builds_a_task(agent: &Agent<'_>) -> bool {
    agent.role == RoleName::Worker && agent.step == Some(StepName::Build)
}

/// The brief of the task `agent` builds, when it has words.
fn brief(host: &Host, agent: &Agent<'_>) -> Option<String> {
    if !builds_a_task(agent) {
        return None;
    }
    let brief = host.read(|state| {
        let spec = &state.cards.get(agent.card?)?.spec;
        (spec.kind != CardKind::Solo).then(|| spec.task.clone())
    })?;
    (!brief.trim().is_empty()).then_some(brief)
}

/// Says who wrote the brief of a task that a workflow's `agent()` call starts, before the
/// task's worker starts. A brief nobody spoke for is the orchestrator's, which is what
/// dividing a plan leaves.
///
/// Kept in the run's folder, so a run that stops and picks up again still knows.
pub fn brief_written_by(host: &Host, task: &CardId, author: CallAuthor<'_>) -> EngineResult<()> {
    let workflow = match author {
        CallAuthor::Orchestrator => None,
        CallAuthor::Workflow { name, step } => Some(step_shown(name, step)),
    };
    // Workers start side by side, and each start rewrites the file.
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
    let folder = &host.repo().run_dir;
    let path = folder.join(BRIEFS_FILE);
    let mut briefs = briefs_from_workflows(&path);
    if briefs.get(task.as_str()) == workflow.as_ref() {
        return Ok(());
    }
    match workflow {
        Some(step) => briefs.insert(task.as_str().to_owned(), step),
        None => briefs.remove(task.as_str()),
    };
    let text = serde_json::to_string_pretty(&briefs).map_err(std::io::Error::other)?;
    create_private_dir_all(folder)?;
    write_private_file(&path, text.as_bytes())?;
    Ok(())
}

/// In a run's folder: each task whose brief is a workflow's own words, with the workflow and
/// the step as the screen names them.
const BRIEFS_FILE: &str = "briefs-from-workflows.json";

fn briefs_from_workflows(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The workflow and the step whose own words the brief of `task` is, when it is.
fn brief_from_workflow(host: &Host, task: &CardId) -> Option<String> {
    briefs_from_workflows(&host.repo().run_dir.join(BRIEFS_FILE)).remove(task.as_str())
}

/// The part a workflow's `agent()` call leads its prompt with: the call's words, under the
/// name of whoever wrote them. Whatever runs a workflow's calls hands each one's words over
/// here, and the session's start puts the role's frame before them and your own words after
/// ([`super::assemble`]), as for every agent.
pub fn of_call(host: &Host, words: &str, author: CallAuthor<'_>) -> InputPart {
    call_part(words, author, &orchestrator(host.settings()))
}

fn call_part(words: &str, author: CallAuthor<'_>, orchestrator: &WorkerRef) -> InputPart {
    match author {
        CallAuthor::Orchestrator => {
            parts::from_orchestrator(words, Some(roles::short_name(orchestrator)))
        }
        CallAuthor::Workflow { name, step } => parts::from_workflow(words, step_shown(name, step)),
    }
}

/// A workflow's step as the screen names it: `council, step review`.
fn step_shown(workflow: &str, step: Option<&str>) -> String {
    match step {
        Some(step) => format!("{workflow}, step {step}"),
        None => workflow.to_owned(),
    }
}

/// What the orchestrator wrote on the step `agent` serves, in a workflow it composed for this
/// request rather than ran from a file.
fn composed_step(host: &Host, agent: &Agent<'_>) -> Option<String> {
    let at = agent.at?;
    host.read(|state| {
        let record = state.workflows.by_id.get(&at.step.workflow)?;
        if record.from.is_some() {
            return None;
        }
        let words = record.step_definition(&at.step)?.task.clone()?;
        (!words.trim().is_empty()).then_some(words)
    })
}

/// What a workflow's own step says its agents do, and which step said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepWords {
    pub text: String,
    /// The workflow and the step, as the screen names them: `council, step review`.
    pub step: String,
}

/// The words of the step `agent` serves, when its workflow is one that runs from a file. A
/// workflow the orchestrator composed carries the orchestrator's words instead, which
/// [`from_orchestrator`] gives.
pub fn from_step(host: &Host, agent: &Agent<'_>) -> Option<StepWords> {
    let at = agent.at?;
    host.read(|state| {
        let record = state.workflows.by_id.get(&at.step.workflow)?;
        let workflow = record.from.as_ref()?;
        let step = record.step_definition(&at.step)?;
        let text = step
            .task
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())?;
        let name = step
            .name
            .clone()
            .unwrap_or_else(|| crate::workflow::describe(&at.step));
        Some(StepWords {
            text: text.to_owned(),
            step: step_shown(workflow.as_str(), Some(&name)),
        })
    })
}

/// Who plans and delegates: the first seat of the orchestrator, `[crew] orchestrator` or
/// `roles.orchestrator.workers`, which is the main agent while the main session orchestrates.
pub fn orchestrator(settings: &Settings) -> WorkerRef {
    roles::candidates(settings, RoleName::Orchestrator)
        .into_iter()
        .next()
        .unwrap_or_else(|| settings.main_agent.clone())
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::session::InputPartKind;
    use argonauts_core::prompt_files::PromptDirs;
    use argonauts_core::prompt_parts::heading;
    use argonauts_core::settings::prompts::Writer;

    use super::*;
    use crate::prompts::assemble::{Sources, assemble};

    fn assembled(call: InputPart) -> Vec<String> {
        let dirs = PromptDirs::default();
        let prompt = assemble(
            Sources {
                dirs: &dirs,
                role: RoleName::Reviewer,
                writer: &Writer::Orchestrator,
                written: None,
                author: None,
                others_build_too: false,
                step: None,
                setting: None,
                request: vec!["focus on date parsing".to_owned()],
            },
            vec![call],
        );
        prompt.iter().filter_map(heading).collect()
    }

    /// A call's words are the agent's instructions as they are: the frame goes before them
    /// and your own words after, and no template of Argonauts' stands in for them.
    #[test]
    fn a_calls_words_are_read_under_the_name_of_who_wrote_them() {
        let orchestrator: WorkerRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let words = "Review the due-date change against docs/due.md.";

        let written = call_part(words, CallAuthor::Orchestrator, &orchestrator);
        assert_eq!(written.kind, InputPartKind::Instructions);
        assert_eq!(written.text, words);
        assert!(parts::written_for_the_agent(&written));
        assert_eq!(
            assembled(written),
            [
                "Frame · built in".to_owned(),
                format!(
                    "From the orchestrator · {}",
                    roles::short_name(&orchestrator)
                ),
                "From your request".to_owned(),
            ]
        );

        let as_written = CallAuthor::Workflow {
            name: "spec-first",
            step: Some("review"),
        };
        assert_eq!(
            assembled(call_part(words, as_written, &orchestrator)),
            [
                "Frame · built in",
                "From the workflow · spec-first, step review",
                "From your request",
            ]
        );
        let a_script = CallAuthor::Workflow {
            name: "nightly",
            step: None,
        };
        let part = call_part(words, a_script, &orchestrator);
        assert_eq!(part.source.as_deref(), Some("nightly"));
    }
}
