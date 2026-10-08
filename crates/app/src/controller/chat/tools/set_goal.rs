//! `argonauts_set_goal`: the main session sets, shows, pauses, resumes or clears the goal
//! because you said so in your own words. The goal itself is `chat::goal`'s.

use argonauts_mcp::session::goal::{self, ABOUT, SET_GOAL_TOOL};

use super::{Call, MainTool};
use crate::controller::chat::ChatState;
use crate::controller::{Ctx, Effect};

pub(super) fn tool() -> MainTool {
    MainTool {
        name: SET_GOAL_TOOL,
        about: ABOUT,
        offered,
        call,
    }
}

/// The main session's alone: a conversation carried on from a run's agent is that agent at
/// work, and its goal is its task's.
fn offered(_ctx: &Ctx, chat: &ChatState) -> bool {
    chat.place.is_none()
}

fn call(ctx: &mut Ctx, chat: &mut ChatState, call: Call) -> Vec<Effect> {
    let asked = goal::parse(&call.arguments);
    let (reply, effects) =
        super::super::goal::asked(ctx, chat, asked.action, &asked.condition, asked.from_now_on);
    let _ = call.reply.send(reply);
    effects
}
