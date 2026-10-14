//! How the main agent works for you in this app session, whatever CLI it is: `/fast` answers at
//! the lowest effort its model takes, `/output-style` sets how answers are written, `/plan` has
//! runs plan first and wait for you. `/goal` has its own file, `goal`.

use argonauts_core::NoteLevel;
use argonauts_core::app::AppEvent;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::approvals::ApprovalMode;

use super::{ConversationState, direct, request_planned};
use crate::controller::turn::{self, fast_choice};
use crate::controller::{Ctx, Effect};

/// Kept across `/clear`, `/branch`, and `/resume` until the app closes.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Prefs {
    pub fast: bool,
    pub style: OutputStyle,
    /// Runs from the conversation plan first and wait for you to approve the plan.
    pub plan_first: bool,
}

impl Prefs {
    /// What every conversation turn is told after its own prompt.
    pub(super) fn notes(&self) -> Vec<InputPart> {
        self.style
            .instruction()
            .map(|text| turn::part(InputPartKind::Prompt, "output style", text.to_owned()))
            .into_iter()
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum OutputStyle {
    #[default]
    Default,
    Brief,
    Explanatory,
    Learning,
}

impl OutputStyle {
    const ALL: [OutputStyle; 4] = [
        OutputStyle::Default,
        OutputStyle::Brief,
        OutputStyle::Explanatory,
        OutputStyle::Learning,
    ];

    fn name(self) -> &'static str {
        match self {
            OutputStyle::Default => "default",
            OutputStyle::Brief => "brief",
            OutputStyle::Explanatory => "explanatory",
            OutputStyle::Learning => "learning",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            OutputStyle::Default => "Answers as the agent writes them",
            OutputStyle::Brief => "The answer first, in as few words as it takes",
            OutputStyle::Explanatory => {
                "Says why things work as they do, and what choices trade off"
            }
            OutputStyle::Learning => "Teaches the ideas behind the answer, with something to try",
        }
    }

    fn instruction(self) -> Option<&'static str> {
        match self {
            OutputStyle::Default => None,
            OutputStyle::Brief => Some(
                "Write the answer briefly: put it first, in as few words as it takes, with no \
                 preamble and no summary at the end.",
            ),
            OutputStyle::Explanatory => Some(
                "As you answer, explain why things work the way they do, and point out the \
                 choices in the code and what each one trades off.",
            ),
            OutputStyle::Learning => Some(
                "Teach as you answer: explain the ideas behind it step by step, as to someone \
                 learning them, and end with one small thing to try.",
            ),
        }
    }

    fn find(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|style| style.name().eq_ignore_ascii_case(name))
    }
}

/// `/fast [on|off]`; no word toggles it.
pub(crate) fn fast(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    on: Option<bool>,
) -> anyhow::Result<Vec<Effect>> {
    let on = on.unwrap_or(!conversation.prefs.fast);
    conversation.prefs.fast = on;
    let text = if on {
        let (worker, _) = turn::conversation_agent(ctx.settings(), &ctx.settings().crew.main);
        match fast_choice(ctx, &worker) {
            Some((fast, effort)) => {
                let name = ctx.names.worker(&fast);
                format!("Fast is on: {name} answers at {effort} effort until /fast off.")
            }
            None => {
                let name = ctx.names.worker(&worker);
                format!("Fast is on, but {name} has no faster level, so answers come as before.")
            }
        }
    } else {
        "Fast is off: answers use the effort you set again.".to_owned()
    };
    ctx.notice(NoteLevel::Info, text);
    Ok(Vec::new())
}

/// `/output-style [style]`: no style lists them.
pub(crate) fn output_style(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    style: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let Some(name) = style else {
        let mut lines: Vec<String> = OutputStyle::ALL
            .into_iter()
            .map(|style| {
                let used = if style == conversation.prefs.style {
                    " · in use"
                } else {
                    ""
                };
                format!("{} · {}{used}", style.name(), style.describe())
            })
            .collect();
        lines.push("/output-style <style> picks one.".to_owned());
        ctx.send(AppEvent::Report {
            title: "Output style".to_owned(),
            lines,
        });
        return Ok(Vec::new());
    };
    let Some(style) = OutputStyle::find(name.trim()) else {
        let names: Vec<&str> = OutputStyle::ALL
            .into_iter()
            .map(OutputStyle::name)
            .collect();
        anyhow::bail!(
            "There's no {name:?} output style. The styles: {}.",
            names.join(", ")
        );
    };
    conversation.prefs.style = style;
    let text = match style {
        OutputStyle::Default => "Answers come as the agent writes them again.".to_owned(),
        other => format!("Answers now come in the {} style.", other.name()),
    };
    ctx.notice(NoteLevel::Info, text);
    Ok(Vec::new())
}

/// `/plan <task>` sends the task with its run planned first; `/plan` alone turns planning
/// first on or off for every run from the conversation.
pub(crate) fn plan(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    task: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if let Some(task) = task
        .map(|task| task.trim().to_owned())
        .filter(|task| !task.is_empty())
    {
        return request_planned(ctx, conversation, task);
    }
    conversation.prefs.plan_first = !conversation.prefs.plan_first;
    let autopilot = direct::approval(conversation) == ApprovalMode::Autopilot;
    let text = match (conversation.prefs.plan_first, autopilot) {
        (true, false) => {
            "Plan first is on: each run from here plans, and changes nothing until you approve \
             its plan. /plan again turns it off."
        }
        // Captain's plan has no approval of its own to go by, and nothing waits on autopilot.
        (true, true) => {
            "Plan first is on: each run from here plans before it changes anything. On \
             autopilot Captain goes ahead with its plan once it is made, and a crew's plan is \
             decided as your approvals say. /plan again turns it off."
        }
        (false, _) => "Plan first is off: runs go by your approvals again.",
    };
    ctx.notice(NoteLevel::Info, text);
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_are_found_by_name_and_only_the_default_adds_nothing() {
        assert_eq!(OutputStyle::find("Brief"), Some(OutputStyle::Brief));
        assert_eq!(OutputStyle::find("terse"), None);
        let prefs = Prefs::default();
        assert!(prefs.notes().is_empty());
        let prefs = Prefs {
            style: OutputStyle::Learning,
            ..Prefs::default()
        };
        let notes = prefs.notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].text.starts_with("Teach as you answer"));
    }
}
