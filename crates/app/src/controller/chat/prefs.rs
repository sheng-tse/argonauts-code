//! How the main agent works for you in this app session, whatever CLI it is: `/fast` answers at
//! the lowest effort its model takes, `/output-style` sets how answers are written, `/plan` has
//! runs plan first and wait for you, and `/goal` sets a goal a crew run works toward.

use argonauts_core::NoteLevel;
use argonauts_core::RunId;
use argonauts_core::app::{AppEvent, AppMode, UiCommand};
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::{InputPart, InputPartKind};

use super::{ChatState, planned, request_planned, run_request};
use crate::controller::turn::{self, fastest_effort};
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
    /// What every chat turn is told after its own prompt.
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
                "Write the answer briefly: lead with it, in as few words as it takes, with no \
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
    chat: &mut ChatState,
    on: Option<bool>,
) -> anyhow::Result<Vec<Effect>> {
    let on = on.unwrap_or(!chat.prefs.fast);
    chat.prefs.fast = on;
    let text = if on {
        let (worker, _) = turn::chat_agent(ctx.settings(), &ctx.settings().default_agent);
        let effort = fastest_effort(ctx, &worker);
        let name = ctx.names.worker(&worker);
        format!("Fast is on: {name} answers at {effort} effort until /fast off.")
    } else {
        "Fast is off: answers use the effort you set again.".to_owned()
    };
    ctx.notice(NoteLevel::Info, text);
    Ok(Vec::new())
}

/// `/output-style [style]`: no style lists them.
pub(crate) fn output_style(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    style: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let Some(name) = style else {
        let mut lines: Vec<String> = OutputStyle::ALL
            .into_iter()
            .map(|style| {
                let used = if style == chat.prefs.style {
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
    chat.prefs.style = style;
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
    chat: &mut ChatState,
    task: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if let Some(task) = task
        .map(|task| task.trim().to_owned())
        .filter(|task| !task.is_empty())
    {
        return request_planned(ctx, chat, task);
    }
    chat.prefs.plan_first = !chat.prefs.plan_first;
    let text = if chat.prefs.plan_first {
        "Plan first is on: each run from here plans, and changes nothing until you approve its \
         plan. /plan again turns it off."
    } else {
        "Plan first is off: runs go by your approvals again."
    };
    ctx.notice(NoteLevel::Info, text);
    Ok(Vec::new())
}

/// What `/goal` set, and the run working toward it once it started.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Goal {
    text: String,
    run: Option<RunId>,
}

/// `/goal <goal>` starts a crew run that works toward it until it's met. `/goal` shows it,
/// `/goal pause` and `/goal resume` hold and release its run, and `/goal clear` drops it.
pub(crate) fn goal(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let text = text.unwrap_or_default().trim().to_owned();
    match text.as_str() {
        "" => {
            let shown = match &chat.goal {
                None => "No goal is set. /goal <goal> sets one, and a crew run works toward it."
                    .to_owned(),
                Some(Goal {
                    text,
                    run: Some(run),
                }) => format!("The goal: {text} ({run} works toward it)"),
                Some(Goal { text, run: None }) => format!("The goal: {text}"),
            };
            ctx.notice(NoteLevel::Info, shown);
            Ok(Vec::new())
        }
        "clear" => {
            let text = match chat.goal.take() {
                Some(Goal { run: Some(run), .. }) => {
                    format!("Dropped the goal. {run} keeps going; /stop {run} stops it.")
                }
                Some(_) => "Dropped the goal.".to_owned(),
                None => "No goal is set.".to_owned(),
            };
            ctx.notice(NoteLevel::Info, text);
            Ok(Vec::new())
        }
        "pause" | "resume" => Ok(hold(ctx, chat, text == "pause")),
        _ => {
            let replaced = chat.goal.is_some();
            chat.goal = Some(Goal {
                text: text.clone(),
                run: None,
            });
            let mut request = run_request(ctx, text, RunMode::Crew, None, None);
            if chat.prefs.plan_first {
                request = planned(request);
            }
            let set = if replaced {
                "Replaced the goal"
            } else {
                "Goal set"
            };
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "{set}. A crew run works toward it until it's met; /goal shows it, /goal \
                     pause and /goal resume hold it, and /goal clear drops it."
                ),
            );
            Ok(vec![Effect::StartRun(Box::new(request))])
        }
    }
}

/// Pauses or resumes the goal's run, which must be the one on screen to pause.
fn hold(ctx: &Ctx, chat: &ChatState, pause: bool) -> Vec<Effect> {
    let Some(goal) = &chat.goal else {
        ctx.notice(NoteLevel::Info, "No goal is set. /goal <goal> sets one.");
        return Vec::new();
    };
    let Some(run) = goal.run.clone() else {
        ctx.notice(NoteLevel::Info, "The goal's run hasn't started.");
        return Vec::new();
    };
    let on_screen = match &ctx.mode {
        AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Parked { run, .. } => {
            Some(run)
        }
        AppMode::Idle | AppMode::Answering { .. } => None,
    };
    let command = if pause {
        if on_screen != Some(&run) {
            ctx.notice(
                NoteLevel::Info,
                format!("{run} isn't on screen. /resume {run} opens it, then /goal pause."),
            );
            return Vec::new();
        }
        UiCommand::Pause { now: false }
    } else {
        UiCommand::Resume { run: Some(run) }
    };
    vec![Effect::Input(Box::new(command))]
}

/// A run just started; the goal waiting for one takes it.
pub(crate) fn goal_run_started(chat: &mut ChatState, request: &str, run: Option<&RunId>) {
    if let Some(goal) = chat.goal.as_mut()
        && goal.run.is_none()
        && goal.text == request
    {
        goal.run = run.cloned();
    }
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
