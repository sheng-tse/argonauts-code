//! `argonauts_setup`: the main session sets Argonauts Code up from what you say. The form it fills
//! is checked the way a settings file is, a wrong value goes back to it, and a valid one is
//! one question for you. Nothing is written until you pick where it applies, or said so yourself.
//! On autopilot it holds for the request or the conversation, with no question and nothing saved.

use argonauts_core::settings::approvals::ApprovalMode;

use super::{Call, ConversationState, fail, form, question};
use crate::controller::conversation::direct;
use crate::controller::{Ctx, Effect};

pub(crate) const NAME: &str = argonauts_mcp::session::setup::NAME;

pub(crate) const ABOUT: &str = "`argonauts_setup` proposes a change to the user's Argonauts Code \
    setup when they ask for one: the crew and each role's model, the workflow, a role's \
    instructions, any prompt, goals, limits, fallbacks and mods. The user sees it as a question \
    and nothing changes until they pick where it applies. When the user only talks about \
    their setup, such as wondering whether another model would review better, answer from \
    `argonauts_lookup`, say that nothing changed, and say in one line what they could ask \
    you to set. When their words only ask to keep a request cheap, its agents already run \
    their cheapest models; you may also propose a crew of one provider, such as all Claude, \
    or a cheaper mix, for just this request, and the crew stays as it is unless they pick it. \
    You never edit the files in .argonauts/ yourself.";

/// Only the main session has it. A conversation carried on from a run's agent is that agent
/// at work, and no agent of a crew changes your setup.
pub(crate) fn offered(_ctx: &Ctx, conversation: &ConversationState) -> bool {
    conversation.place.is_none()
}

pub(crate) fn call(ctx: &mut Ctx, conversation: &mut ConversationState, call: Call) -> Vec<Effect> {
    let said = conversation
        .turn
        .as_ref()
        .filter(|turn| turn.id == call.turn)
        .map(|turn| turn.text.clone());
    match said {
        Some(said) => propose(ctx, conversation, call, &said),
        None => {
            call.answer("That turn is over, so nothing was shown.");
            Vec::new()
        }
    }
}

/// Checks the form against your message `said`, then shows the question, or keeps the setup
/// where your words said to.
pub(super) fn propose(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    call: Call,
    said: &str,
) -> Vec<Effect> {
    match form::check(ctx, &call.arguments, said) {
        Err(problems) => {
            fail(call, problems);
            Vec::new()
        }
        Ok(proposal) => {
            let autopilot = direct::approval(conversation) == ApprovalMode::Autopilot;
            match proposal.keep {
                Some(place) => {
                    question::keep_as_said(ctx, conversation, call, proposal, place, autopilot)
                }
                // Nothing waits for you on autopilot.
                None if autopilot => question::keep_on_autopilot(ctx, conversation, call, proposal),
                None => {
                    question::open(ctx, conversation, call, proposal);
                    Vec::new()
                }
            }
        }
    }
}
