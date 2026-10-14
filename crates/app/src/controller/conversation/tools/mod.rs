//! The main session's tools, on the app's side. The session's own server lists them
//! (`argonauts_mcp::session::main_tools`) and sends each call here, under the tool's name, over
//! the turn's listener. Nothing outside the main session happens until one of them is called.
//!
//! This is the one place they are registered. A tool has a file of its own beside this one,
//! which says when the session is offered it, what the standing lines say about it, and what a
//! call does, and one line in [`all`].

mod set_goal;
pub(super) mod start_workflow;

use argonauts_core::asking::AskReply;
use argonauts_core::names::TurnId;
use serde_json::Value;
use tokio::sync::oneshot;

use super::{ConversationState, talk};
use crate::controller::{Ctx, Effect};

/// One call, from the session of `turn`. The session waits on `reply`, however long it takes.
pub(super) struct Call {
    pub turn: TurnId,
    pub arguments: Value,
    pub reply: oneshot::Sender<AskReply>,
}

impl Call {
    /// Answers the call. A session that already ended hears nothing, and nothing waits on it.
    pub(super) fn answer(self, text: impl Into<String>) {
        let _ = self.reply.send(AskReply::text(text));
    }
}

pub(super) struct MainTool {
    /// The name the session's server lists it under.
    pub name: &'static str,
    /// What the standing lines say about it: the name in backticks, then what it does.
    pub about: &'static str,
    /// Whether a turn that starts now gets it.
    pub offered: fn(&Ctx, &ConversationState) -> bool,
    pub call: fn(&mut Ctx, &mut ConversationState, Call) -> Vec<Effect>,
}

fn all() -> Vec<MainTool> {
    vec![
        start_workflow::tool(),
        set_goal::tool(),
        MainTool {
            name: talk::setup::NAME,
            about: talk::setup::ABOUT,
            offered: talk::setup::offered,
            call: talk::setup::call,
        },
        MainTool {
            name: talk::guide::NAME,
            about: talk::guide::ABOUT,
            offered: talk::guide::offered,
            call: talk::guide::call,
        },
        MainTool {
            name: talk::lookup::NAME,
            about: talk::lookup::ABOUT,
            offered: talk::lookup::offered,
            call: talk::lookup::call,
        },
    ]
}

/// The tools a turn that starts now is offered, by name. None for a CLI that can't take
/// tools when Argonauts Code drives it.
pub(super) fn offered(ctx: &Ctx, conversation: &ConversationState) -> Vec<String> {
    if !super::takes_tools(ctx, conversation) {
        return Vec::new();
    }
    all()
        .into_iter()
        .filter(|tool| (tool.offered)(ctx, conversation))
        .map(|tool| tool.name.to_owned())
        .collect()
}

/// A line about each of the `offered` tools, for the standing lines.
pub(super) fn about(offered: &[String]) -> Vec<String> {
    all()
        .into_iter()
        .filter(|tool| offered.iter().any(|name| name == tool.name))
        .map(|tool| tool.about.to_owned())
        .collect()
}

/// Runs the tool `name` for the turn that called it. A call from a turn that is over, or for a
/// tool that turn's session wasn't offered, is answered and starts nothing.
pub(super) fn call(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    name: &str,
    call: Call,
) -> Vec<Effect> {
    let Some(turn) = conversation
        .turn
        .as_ref()
        .filter(|turn| turn.id == call.turn)
    else {
        call.answer("That turn is over, so nothing starts for it.");
        return Vec::new();
    };
    let offered = turn.step.offers_tool(name);
    match all().into_iter().find(|tool| tool.name == name) {
        Some(tool) if offered => (tool.call)(ctx, conversation, call),
        _ => {
            let _ = call.reply.send(AskReply {
                text: format!("Argonauts Code gave this session no tool named {name:?}."),
                is_error: true,
            });
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use argonauts_mcp::session::main_tools;

    use super::*;

    /// A tool the session lists and the app can't run, or the other way round, would fail
    /// only when a model first called it.
    #[test]
    fn the_session_lists_exactly_the_tools_the_app_runs() {
        let listed: BTreeSet<&str> = main_tools::all().iter().map(|tool| tool.name).collect();
        let run: BTreeSet<&str> = all().iter().map(|tool| tool.name).collect();
        assert_eq!(listed, run);
        for tool in all() {
            assert!(
                tool.about.starts_with(&format!("`{}`", tool.name)),
                "{}",
                tool.about
            );
        }
    }
}
