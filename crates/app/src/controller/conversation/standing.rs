//! What the main session is told about Argonauts Code, since it is the CLI you chose and knows
//! nothing of the crew otherwise. Two things, and no more:
//!
//! - The standing lines: what its tools are and when to use them. They go at the end of the
//!   CLI's own instructions, through its own way of taking such a note, and hold for the whole
//!   conversation, because a CLI keeps its instructions from the first request on.
//! - The setup in use: the workflow, ultracode, the mode. It changes as you change it, so it
//!   travels beside your message, the first time and again whenever it is different from what the
//!   session last heard.

use argonauts_core::runs::events::run::RunMode;
use argonauts_core::runs::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::workflows::OrchestratorDesign;

use super::tools;
use super::{ConversationState, direct};
use crate::controller::Ctx;
use crate::controller::turns::captain_or_crew::convenes;
use crate::controller::turns::turn::{TurnStep, part};

/// The lines for the end of the session's instructions, naming the tools it is `offered`.
/// `None` when it is offered none, since there is nothing to say.
pub(super) fn lines(offered: &[String]) -> Option<String> {
    let about = tools::about(offered);
    if about.is_empty() {
        return None;
    }
    Some(crate::prompts::standing(&about))
}

/// What this turn's session hasn't heard about the setup yet, as a part that follows your
/// message. `fresh` is a session that starts now, which has heard nothing.
pub(super) fn setup_note(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    fresh: bool,
) -> Option<InputPart> {
    let setup = Setup::now(ctx, conversation);
    let line = setup.line();
    let heard = conversation.direct.told_setup.as_deref() == Some(line.as_str());
    if heard && !fresh {
        return None;
    }
    conversation.direct.told_setup = Some(line.clone());
    Some(part(InputPartKind::Notes, "setup", format!("\n\n{line}")))
}

/// The setup the main session works under, as far as it changes what leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Setup {
    /// The workflow in use, by its name. `None` is Captain: the work stays in the session.
    workflow: Option<String>,
    /// You chose the workflow, or forced the crew, so it takes every change.
    convenes: bool,
    ultracode: bool,
    autopilot: bool,
}

impl Setup {
    fn now(ctx: &Ctx, conversation: &ConversationState) -> Self {
        let settings = ctx.settings();
        Self {
            workflow: workflow_for(ctx, conversation),
            convenes: convenes(&ctx.settings),
            ultracode: settings.ultracode,
            autopilot: direct::approval(conversation) == ApprovalMode::Autopilot,
        }
    }

    fn line(&self) -> String {
        let mut line = match &self.workflow {
            Some(workflow) => {
                let on = if self.ultracode { "on" } else { "off" };
                let what_leaves = if self.ultracode || self.convenes {
                    "Every change goes to the workflow, small ones too. Questions are still yours \
                     to answer."
                } else {
                    "Small changes are yours to make. Real work goes to the workflow."
                };
                format!(
                    "Argonauts Code setup: the {workflow} workflow is in use, and ultracode is \
                     {on}. {what_leaves}"
                )
            }
            None => "Argonauts Code setup: Captain is in use, so the work stays with you. Start a \
                          workflow only when the user's own words ask for the crew, for other \
                          agents, for a workflow or for a checked worktree."
                .to_owned(),
        };
        if self.autopilot {
            line.push_str(
                " Autopilot is on: when you can't tell whether the user wants work done or \
                 only an answer, answer and leave things as they are.",
            );
        }
        line
    }
}

/// The workflow this conversation hands work to. An agent's session carried on from a run is
/// that agent at work, so nothing leaves it unless you ask, whatever is in use.
pub(super) fn workflow_for(ctx: &Ctx, conversation: &ConversationState) -> Option<String> {
    if conversation.place.is_some() {
        return None;
    }
    workflow_in_use(ctx.settings())
}

/// A crew workflow you chose, or the crew forced, takes every change, so code decides rather
/// than the session: the session answers read-only and ends its reply with its verdict, answer
/// or change, as with `main_session.tools` off, and a change goes to the workflow.
pub(super) fn reads_the_verdict(ctx: &Ctx, conversation: &ConversationState) -> bool {
    workflow_for(ctx, conversation).is_some() && convenes(&ctx.settings)
}

/// The workflow that takes the change a request may be, when it surely takes it: a plain request
/// when the workflow takes every change, and one sent with `/crew`. The session then gives its
/// verdict before reading anything, and leaves reading and planning a change to the workflow,
/// which does both again in its own sessions.
pub(super) fn verdict_first(
    ctx: &Ctx,
    conversation: &ConversationState,
    step: &TurnStep,
) -> Option<String> {
    let takes_it = match step {
        TurnStep::Estimate { command: None, .. } => convenes(&ctx.settings),
        TurnStep::Estimate {
            command: Some(command),
            ..
        } => command.mode == RunMode::Crew,
        _ => false,
    };
    workflow_for(ctx, conversation).filter(|_| takes_it)
}

/// A change this turn finds opens a workflow's talk, which starts from your message: the change
/// surely goes to the crew, by the rules or a command, and the workflow in use talks. A main
/// agent that may pick any workflow may send it elsewhere.
pub(super) fn opens_a_talk(ctx: &Ctx, conversation: &ConversationState, step: &TurnStep) -> bool {
    let to_the_crew = match step {
        TurnStep::Estimate { command: None, .. } => reads_the_verdict(ctx, conversation),
        TurnStep::Estimate {
            command: Some(command),
            ..
        } => command.mode == RunMode::Crew,
        _ => false,
    };
    to_the_crew
        && ctx.settings().orchestrator_designs() != OrchestratorDesign::Any
        && workflow_for(ctx, conversation).is_some_and(|name| ctx.talking.talks(ctx, &name))
}

/// The workflow work is passed to, by its name. `None` while the workflow is the main session
/// itself: Captain in your checkout, where the work stays unless you ask.
pub(super) fn workflow_in_use(settings: &Settings) -> Option<String> {
    if direct::workflow_is_direct(settings) {
        return None;
    }
    let name = settings.workflow.trim();
    Some(if name.is_empty() { "crew" } else { name }.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(workflow: Option<&str>) -> Setup {
        Setup {
            workflow: workflow.map(str::to_owned),
            convenes: false,
            ultracode: false,
            autopilot: false,
        }
    }

    #[test]
    fn the_setup_line_says_what_leaves_the_session() {
        // Council only as the default: a small change may stay with the session.
        let council = setup(Some("council")).line();
        assert!(council.starts_with("Argonauts Code setup: the council workflow is in use"));
        assert!(council.contains("ultracode is off"), "{council}");
        assert!(council.contains("Small changes are yours"), "{council}");

        let ultra = Setup {
            ultracode: true,
            ..setup(Some("orchestrator"))
        }
        .line();
        assert!(ultra.contains("ultracode is on"), "{ultra}");
        assert!(ultra.contains("small ones too"), "{ultra}");
        assert!(ultra.contains("Questions are still yours"), "{ultra}");

        let chosen = Setup {
            convenes: true,
            ..setup(Some("council"))
        }
        .line();
        assert!(chosen.contains("ultracode is off"), "{chosen}");
        assert!(chosen.contains("small ones too"), "{chosen}");
        assert!(chosen.contains("Questions are still yours"), "{chosen}");
        assert!(!chosen.contains("Small changes are yours"), "{chosen}");

        let captain = setup(None).line();
        assert!(captain.contains("Captain is in use"), "{captain}");
        assert!(
            captain.contains("only when the user's own words ask"),
            "{captain}"
        );
    }

    #[test]
    fn autopilot_adds_its_sentence() {
        let line = Setup {
            autopilot: true,
            ..setup(Some("council"))
        }
        .line();
        assert!(line.contains("Autopilot is on"), "{line}");
        assert!(
            line.contains("answer and leave things as they are"),
            "{line}"
        );
    }

    #[test]
    fn no_tools_means_no_standing_lines() {
        assert_eq!(lines(&[]), None);
        let offered = vec![tools::start_workflow::NAME.to_owned()];
        let lines = lines(&offered).expect("lines");
        assert!(lines.contains("`argonauts_start_workflow`"), "{lines}");
        assert!(lines.contains("nothing outside this session happens until you call one"));
        assert!(lines.contains("say that nothing is changed"), "{lines}");
        assert!(!lines.contains("{{"), "{lines}");
    }

    /// On the council you chose, a plain request and one sent with `/crew` whose words are read
    /// both get the verdict-first estimate, since the council takes the change either way; on the default
    /// workflow a plain request gets the estimate that plans it.
    #[tokio::test]
    async fn a_request_the_chosen_council_takes_gets_the_verdict_first() {
        use crate::controller::base::test_support::{
            CliOptions, TestDriver, on_the_default_workflow,
        };

        let estimate = |options: CliOptions, text: &'static str| async move {
            let mut driver = TestDriver::start_with(options).await;
            driver.submit(text).await;
            driver
                .until(|event| {
                    matches!(
                        event,
                        argonauts_core::app::AppEvent::Conversation {
                            event: argonauts_core::app::ConversationEvent::TurnStarted { .. }
                        }
                    )
                })
                .await;
            let prompt: String = driver.agent.specs()[0]
                .prompt
                .iter()
                .map(|part| part.text.as_str())
                .collect();
            driver.exit().await;
            prompt
        };
        let first = "Decide this first, before you read or search anything";
        for text in [
            "Make the rate limiter per API key.",
            "/crew Make the rate limiter per API key, and keep going until its tests pass.",
        ] {
            let prompt = estimate(CliOptions::default(), text).await;
            assert!(prompt.contains(first), "{text:?}:\n{prompt}");
            assert!(
                prompt.contains("size up a change from its words alone"),
                "{text:?}:\n{prompt}"
            );
        }
        let planned = estimate(
            on_the_default_workflow(CliOptions::default()),
            "Make the rate limiter per API key.",
        )
        .await;
        assert!(!planned.contains(first), "{planned}");
        assert!(planned.contains("how you would go about it"), "{planned}");
    }
}
