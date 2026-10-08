//! What the main session is told about Argonauts, since it is the CLI you chose and knows
//! nothing of the crew otherwise. Two things, and no more:
//!
//! - The standing lines: what its tools are and when to use them. They go at the end of the
//!   CLI's own instructions, through its own way of taking such a note, and hold for the whole
//!   conversation, because a CLI keeps its instructions from the first request on.
//! - The setup in use: the workflow, ultracode, the mode. It changes as you change it, so it
//!   travels beside your message, the first time and again whenever it differs from what the
//!   session last heard.

use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;

use super::tools;
use super::{ChatState, direct};
use crate::controller::Ctx;
use crate::controller::turn::part;

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
pub(super) fn setup_note(ctx: &Ctx, chat: &mut ChatState, fresh: bool) -> Option<InputPart> {
    let setup = Setup::now(ctx, chat);
    let line = setup.line();
    let heard = chat.direct.told_setup.as_deref() == Some(line.as_str());
    if heard && !fresh {
        return None;
    }
    chat.direct.told_setup = Some(line.clone());
    Some(part(InputPartKind::Notes, "setup", format!("\n\n{line}")))
}

/// The setup the main session works under, as far as it changes what leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Setup {
    /// The workflow in use, by its name. `None` is solo: the work stays in the session.
    workflow: Option<String>,
    ultracode: bool,
    autopilot: bool,
}

impl Setup {
    fn now(ctx: &Ctx, chat: &ChatState) -> Self {
        let settings = ctx.settings();
        Self {
            workflow: workflow_for(ctx, chat),
            ultracode: settings.ultracode,
            autopilot: direct::approval(chat) == ApprovalMode::Autopilot,
        }
    }

    fn line(&self) -> String {
        let mut line = match (&self.workflow, self.ultracode) {
            (Some(workflow), false) => format!(
                "Argonauts setup: the {workflow} workflow is in use, and ultracode is off. Small \
                 changes are yours to make. Real work goes to the workflow."
            ),
            (Some(workflow), true) => format!(
                "Argonauts setup: the {workflow} workflow is in use, and ultracode is on. Every \
                 change goes to the workflow, small ones too. Questions are still yours to \
                 answer."
            ),
            (None, _) => "Argonauts setup: solo is in use, so the work stays with you. Start a \
                          workflow only when the user's own words ask for the crew, for other \
                          agents, for a workflow or for a checked copy."
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
pub(super) fn workflow_for(ctx: &Ctx, chat: &ChatState) -> Option<String> {
    if chat.place.is_some() {
        return None;
    }
    workflow_in_use(ctx.settings())
}

/// The workflow work is handed to, by its name. `None` while the workflow is the main session
/// itself: solo in your checkout, where the work stays unless you ask.
pub(super) fn workflow_in_use(settings: &Settings) -> Option<String> {
    if direct::preset_is_direct(settings) {
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
            ultracode: false,
            autopilot: false,
        }
    }

    #[test]
    fn the_setup_line_says_what_leaves_the_session() {
        let council = setup(Some("council")).line();
        assert!(council.starts_with("Argonauts setup: the council workflow is in use"));
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

        let solo = setup(None).line();
        assert!(solo.contains("solo is in use"), "{solo}");
        assert!(
            solo.contains("only when the user's own words ask"),
            "{solo}"
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
}
