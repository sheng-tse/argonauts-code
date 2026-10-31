//! `/goal`: a condition the work that follows keeps going until it holds, on any CLI and in any
//! workflow. `/goal <condition>` sets it, `/goal` shows it, and `/goal pause`, `/goal resume`
//! and `/goal clear` do what they say.
//!
//! The goal is the setting `goal.until`, held for this conversation, so every run started from
//! here carries it whoever starts the run. A goal a settings file gives is shown the same way,
//! and pausing or clearing it holds only for this conversation.

mod keep;
#[cfg(test)]
mod tests;

use argonauts_core::NoteLevel;
use argonauts_core::RunId;
use argonauts_core::app::{AppMode, UiCommand};
use argonauts_core::runs::api::StartRun;
use argonauts_core::runs::asking::{AskReply, GoalAsk};
use argonauts_core::runs::events::goal::GoalAction;
use argonauts_core::settings::Settings;
use argonauts_core::workflows::goal::{CheckMeans, GoalFrom, GoalSpec};

pub(crate) use self::keep::{Checked, on_checked};
pub(super) use self::keep::{after_turn, brief};
use super::ConversationState;
use crate::controller::{Ctx, Effect};

/// The setting that holds the goal.
pub(crate) const UNTIL: &str = "goal.until";

/// What the conversation keeps about its goal beyond the setting.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Kept {
    /// The condition while it's paused, out of the settings so no run carries it.
    paused: Option<String>,
    /// The run started while the goal was set, which a pause or a clear reaches too.
    run: Option<RunId>,
    /// A goal the main session's tool set for the request it's working on, and no further.
    for_request: Option<String>,
    /// The main agent's own work toward the goal, when no run keeps it.
    keeping: Option<keep::Keeping>,
}

/// The goal the settings give the work that follows, with how each line is checked.
pub(crate) fn of(settings: &Settings) -> Option<GoalSpec> {
    let until = settings.goal.until.as_deref()?.trim();
    if until.is_empty() {
        return None;
    }
    spec(settings, until).ok()
}

fn spec(settings: &Settings, condition: &str) -> Result<GoalSpec, String> {
    let means = CheckMeans {
        project_checks: !settings.checks.run.is_empty(),
    };
    GoalSpec::from_words(condition, settings.goal.check, means, GoalFrom::Command)
}

pub(crate) fn goal(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let text = text.unwrap_or_default().trim().to_owned();
    match text.as_str() {
        "" | "show" => {
            show(ctx, conversation);
            Ok(Vec::new())
        }
        "pause" => pause(ctx, conversation),
        "resume" => resume(ctx, conversation),
        "clear" => clear(ctx, conversation),
        condition => set(ctx, conversation, condition),
    }
}

fn set(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    condition: &str,
) -> anyhow::Result<Vec<Effect>> {
    let goal = match spec(ctx.settings(), condition) {
        Ok(goal) => goal,
        Err(why) => {
            ctx.notice(NoteLevel::Warn, format!("That goal can't be set: {why}."));
            return Ok(Vec::new());
        }
    };
    let replaced = of(ctx.settings()).is_some() || conversation.goal.paused.is_some();
    hold(ctx, &goal.condition)?;
    conversation.goal = Kept::default();
    let set = if replaced {
        "Goal replaced"
    } else {
        "Goal set"
    };
    ctx.notice(NoteLevel::Info, format!("{set} · {}", goal.checked_by()));
    Ok(vec![Effect::Status])
}

/// `argonauts_set_goal`: the main session sets, shows, pauses, resumes or clears the goal
/// because you said so in your own words. The answer states the goal back in one line, with
/// what checks it, and the screen says the same. The arguments are a call's as
/// `argonauts_mcp::session::goal::parse` reads them.
pub(crate) fn asked(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    action: GoalAsk,
    condition: &str,
    from_now_on: bool,
) -> (AskReply, Vec<Effect>) {
    let done = |result: anyhow::Result<Vec<Effect>>, said: &str| match result {
        Ok(effects) => (AskReply::text(said), effects),
        Err(err) => (
            refusal(&format!("Argonauts Code couldn't do that: {err}")),
            Vec::new(),
        ),
    };
    match action {
        GoalAsk::Show => {
            let said = match (&conversation.goal.paused, shown(ctx, conversation)) {
                (Some(paused), _) => format!("The goal is paused: {paused}"),
                (None, Some(goal)) => {
                    format!("The goal: {} · {}", goal.condition, goal.checked_by())
                }
                (None, None) => "No goal is set.".to_owned(),
            };
            (AskReply::text(said), Vec::new())
        }
        GoalAsk::Pause => done(
            pause(ctx, conversation),
            "The goal is paused, or none was set.",
        ),
        GoalAsk::Resume => done(
            resume(ctx, conversation),
            "The goal is resumed, or none was paused.",
        ),
        GoalAsk::Clear => {
            conversation.goal.for_request = None;
            conversation.goal.keeping = None;
            done(
                clear(ctx, conversation),
                "The goal is cleared, or none was set.",
            )
        }
        GoalAsk::Set => {
            let goal = match spec(ctx.settings(), condition) {
                Ok(goal) => goal,
                Err(why) => {
                    return (
                        refusal(&format!("That goal can't be set: {why}.")),
                        Vec::new(),
                    );
                }
            };
            let covers = if from_now_on {
                if let Err(err) = hold(ctx, &goal.condition) {
                    return (
                        refusal(&format!("Argonauts Code couldn't keep it: {err}")),
                        Vec::new(),
                    );
                }
                conversation.goal = Kept::default();
                "the work that follows"
            } else {
                conversation.goal.for_request = Some(goal.condition.clone());
                conversation.goal.keeping = None;
                "this request"
            };
            let stated = format!("Goal set for {covers} · {}", goal.checked_by());
            ctx.notice(NoteLevel::Info, stated.clone());
            let said = format!(
                "{stated}. Tell the user that line, then do the work. Argonauts Code runs the \
                 check itself when you finish and sends back what still fails, with no fixed \
                 number of tries, so don't stop to ask whether to go on."
            );
            (AskReply::text(said), vec![Effect::Status])
        }
    }
}

fn refusal(text: &str) -> AskReply {
    AskReply {
        text: text.to_owned(),
        is_error: true,
    }
}

/// The goal the work in hand has: this request's own, else the conversation's.
fn shown(ctx: &Ctx, conversation: &ConversationState) -> Option<GoalSpec> {
    conversation
        .goal
        .for_request
        .as_deref()
        .and_then(|condition| spec(ctx.settings(), condition).ok())
        .or_else(|| of(ctx.settings()))
}

/// A run the main session starts for a request whose goal it just set carries that goal, for
/// that run alone. The conversation's own goal reaches every run through the settings.
pub(super) fn carry(conversation: &mut ConversationState, start: &mut StartRun) {
    if let Some(condition) = conversation.goal.for_request.take() {
        let value = toml::Value::String(condition).to_string();
        start.request_settings.push(format!("{UNTIL}={value}"));
    }
}

fn show(ctx: &Ctx, conversation: &ConversationState) {
    if let Some(paused) = &conversation.goal.paused {
        ctx.notice(
            NoteLevel::Info,
            format!("Goal paused: {paused}\n/goal resume picks it up, /goal clear drops it."),
        );
        return;
    }
    let Some(goal) = of(ctx.settings()) else {
        ctx.notice(
            NoteLevel::Info,
            "No goal is set. /goal <condition> sets one: the work that follows keeps going \
             until it holds, whatever the workflow and the CLI.",
        );
        return;
    };
    let mut text = format!("Goal · {}", goal.checked_by());
    for line in &goal.lines {
        text.push_str(&format!(
            "\n  {} · {}",
            line.text,
            goal.line_checked_by(line)
        ));
    }
    let limit = match ctx.settings().goal.max_rounds {
        Some(1) => "It stops after 1 round".to_owned(),
        Some(max) => format!("It stops after {max} rounds"),
        None => "No limit on rounds".to_owned(),
    };
    let from = if ctx.settings.session_sets(UNTIL) {
        "set for this conversation"
    } else {
        "from your settings"
    };
    text.push_str(&format!(
        "\n{limit} · {from} · /goal pause holds it, /goal clear drops it."
    ));
    ctx.notice(NoteLevel::Info, text);
}

/// The goal steps aside: runs started from here stop carrying it, and the run working toward
/// it finishes its round without another.
fn pause(ctx: &mut Ctx, conversation: &mut ConversationState) -> anyhow::Result<Vec<Effect>> {
    if conversation.goal.paused.is_some() {
        ctx.notice(NoteLevel::Info, "The goal is already paused.");
        return Ok(Vec::new());
    }
    let Some(goal) = of(ctx.settings()) else {
        ctx.notice(
            NoteLevel::Info,
            "No goal is set. /goal <condition> sets one.",
        );
        return Ok(Vec::new());
    };
    // An empty condition is no goal, and it hides one a settings file gives.
    hold(ctx, "")?;
    conversation.goal.paused = Some(goal.condition);
    ctx.notice(
        NoteLevel::Info,
        "Goal paused. Work in progress finishes its round and isn't sent round again, and \
         the work that follows doesn't carry it. /goal resume picks it up.",
    );
    let mut effects = vec![Effect::Status];
    effects.extend(tell_run(
        ctx,
        conversation,
        GoalAction::Pause { task: None },
    ));
    Ok(effects)
}

fn resume(ctx: &mut Ctx, conversation: &mut ConversationState) -> anyhow::Result<Vec<Effect>> {
    let Some(condition) = conversation.goal.paused.take() else {
        ctx.notice(NoteLevel::Info, "No goal is paused.");
        return Ok(Vec::new());
    };
    hold(ctx, &condition)?;
    ctx.notice(NoteLevel::Info, "Goal resumed.");
    let mut effects = vec![Effect::Status];
    effects.extend(tell_run(
        ctx,
        conversation,
        GoalAction::Resume { task: None },
    ));
    Ok(effects)
}

fn clear(ctx: &mut Ctx, conversation: &mut ConversationState) -> anyhow::Result<Vec<Effect>> {
    let had = of(ctx.settings()).is_some() || conversation.goal.paused.is_some();
    if !had {
        ctx.notice(NoteLevel::Info, "No goal is set.");
        return Ok(Vec::new());
    }
    let mut effects = vec![Effect::Status];
    // Its run stops going round, and Merge takes what holds once every check passes.
    effects.extend(tell_run(
        ctx,
        conversation,
        GoalAction::Clear { land: true },
    ));
    forget(ctx)?;
    // A goal a settings file gives would come back, so it's hidden for this conversation.
    if of(ctx.settings()).is_some() {
        hold(ctx, "")?;
    }
    conversation.goal = Kept::default();
    ctx.notice(NoteLevel::Info, "Goal cleared.");
    Ok(effects)
}

/// Holds `condition` as the goal for this conversation.
fn hold(ctx: &mut Ctx, condition: &str) -> anyhow::Result<()> {
    let env = ctx.env.clone();
    let value = toml::Value::String(condition.to_owned()).to_string();
    ctx.settings.set_session(&env, UNTIL, &value)
}

/// A new conversation starts without the last one's goal.
pub(super) fn forget(ctx: &mut Ctx) -> anyhow::Result<()> {
    let env = ctx.env.clone();
    if ctx.settings.session_sets(UNTIL) {
        ctx.settings.remove_session(&env, UNTIL)?;
    }
    Ok(())
}

/// The action for the goal's own run, when it's the one on screen and still going.
fn tell_run(ctx: &Ctx, conversation: &ConversationState, action: GoalAction) -> Option<Effect> {
    let run = conversation.goal.run.as_ref()?;
    let on_screen = match &ctx.mode {
        AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Waiting { run, .. } => {
            Some(run)
        }
        AppMode::Idle | AppMode::Answering { .. } => None,
    };
    (on_screen == Some(run)).then(|| {
        Effect::Input(Box::new(UiCommand::Goal {
            run: Some(run.clone()),
            action,
        }))
    })
}

/// A run just started. With a goal set, it's the run a pause or a clear reaches.
pub(crate) fn goal_run_started(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    run: Option<&RunId>,
) {
    if of(ctx.settings()).is_some() {
        conversation.goal.run = run.cloned();
    }
}
