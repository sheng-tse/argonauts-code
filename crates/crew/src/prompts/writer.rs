//! A model you named writes an agent's instructions (`[prompts] writer = "codex/gpt-5.6"`).
//! It is asked before the agent starts, with what the agent would have been told without it,
//! and what it writes takes that text's place. Agents of one role at one step of one task
//! read the same text.

use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::RoleName;
use argonauts_core::prompts::prompt_parts as parts;
use argonauts_core::runs::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::prompts::Writer;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::steps::instructions::{WriteInstructions, WriteInstructionsInput};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};

use super::assemble;
use super::instructions::Agent;

pub const WRITE_INSTRUCTIONS: &str =
    argonauts_core::template!("prompts/writer/write-instructions.md");

/// `task` with the writer model's instructions in place of the ones it led with, when the
/// agent's role has a writer model. Otherwise `task` as it was: the writer never writes for
/// itself, and a writer that fails leaves the agent what it would have read anyway, since an
/// agent that doesn't start helps nobody.
pub fn written<'a>(
    host: &'a Host,
    agent: Agent<'a>,
    task: Vec<InputPart>,
) -> BoxFuture<'a, EngineResult<Vec<InputPart>>> {
    Box::pin(async move {
        let Writer::Model(model) = host.settings().prompts.writer_for(agent.role).clone() else {
            return Ok(task);
        };
        if agent.step == Some(StepName::WriteInstructions) {
            return Ok(task);
        }
        let Some(draft) = assemble::draft(host, &agent, &task) else {
            return Ok(task);
        };
        let input = WriteInstructionsInput {
            role: CrewRole::read_by(agent.role).as_str().to_owned(),
            frame: assemble::frame_text(host, &agent),
            draft,
        };
        let mut ask = Ask::new(
            agent.at.cloned(),
            RoleName::Orchestrator,
            model.clone(),
            WRITE_INSTRUCTIONS,
            input,
        );
        ask.task = agent.task.cloned();
        ask.access = Access::NoTools;
        ask.slot = slot(&agent, &ask.input.draft);
        let instructions = match host.ask::<WriteInstructions>(ask).await {
            Ok(answered) => answered.output.instructions,
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(err) => {
                tracing::warn!(
                    "prompts: {model} couldn't write the {} instructions ({err}), so the agent \
                     reads the ones it would have had",
                    agent.role.as_str()
                );
                return Ok(task);
            }
        };
        if instructions.trim().is_empty() {
            return Ok(task);
        }
        let leading = task.iter().take_while(|part| part.kind.instructs()).count();
        let mut prompt = vec![parts::from_writer(instructions, host.names().short(&model))];
        prompt.extend(task.into_iter().skip(leading));
        Ok(prompt)
    })
}

/// Tells the writer's sessions apart by the role, the step and what there was to write from,
/// so a resumed run finds each one's answer again. A workflow's calls share a role and have no
/// step of their own, and each carries its own words.
fn slot(agent: &Agent<'_>, draft: &str) -> u32 {
    let name = format!("{}/{:?}/{draft}", agent.role.as_str(), agent.step);
    // FNV-1a: small, stable across runs, and spread enough for a handful of sessions.
    name.bytes().fold(2_166_136_261, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    })
}

/// Whether the task's leading parts are instructions someone still has to write: nothing
/// framed yet, nothing a writer wrote already, and not the words of your own workflow's step,
/// which its agents read as you wrote them.
pub(super) fn open_to_a_writer(task: &[InputPart]) -> bool {
    let settled = [parts::FROM_THE_WRITER, parts::FROM_THE_WORKFLOW];
    !task.iter().any(|part| {
        part.kind == InputPartKind::Frame
            || (part.kind == InputPartKind::Instructions && settled.contains(&part.label.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_writer_leaves_your_workflows_words_and_its_own_alone() {
        let orchestrators = parts::from_orchestrator("Review the change.", None);
        assert!(open_to_a_writer(&[orchestrators]));
        let yours = parts::from_workflow("Review the change.", "spec-first, step review".into());
        assert!(!open_to_a_writer(&[yours]));
        let written = parts::from_writer("Review the change.", "GPT 5.6".into());
        assert!(!open_to_a_writer(&[written]));
    }

    /// Two reviewers of one change read the same text, and two calls of a workflow that ask
    /// different things each get their own.
    #[test]
    fn a_writers_session_is_told_apart_by_what_it_writes_from() {
        let agent = Agent {
            role: RoleName::Reviewer,
            step: None,
            at: None,
            task: None,
        };
        assert_eq!(
            slot(&agent, "Check the dates."),
            slot(&agent, "Check the dates.")
        );
        assert_ne!(
            slot(&agent, "Check the dates."),
            slot(&agent, "Check the tests.")
        );
    }
}
