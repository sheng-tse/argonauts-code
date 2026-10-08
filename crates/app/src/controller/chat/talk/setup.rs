//! `argonauts_setup`: the main session sets Argonauts up from what you say. The form it fills
//! is checked the way a settings file is, a wrong value goes back to it, and a valid one is
//! one card for you. Nothing is written until you pick where it applies, or said so yourself.

use super::{Call, ChatState, card, fail, form};
use crate::controller::{Ctx, Effect};

pub(crate) const NAME: &str = argonauts_mcp::session::setup::NAME;

pub(crate) const ABOUT: &str = "`argonauts_setup` proposes a change to the user's Argonauts \
    setup when they ask for one: the crew and each role's model, the workflow, a role's \
    instructions, any prompt, goals, limits, fallbacks and mods. The user sees it as a card \
    and nothing changes until they pick where it applies. When the user only talks about \
    their setup, such as wondering whether another model would review better, answer from \
    `argonauts_lookup`, say that nothing changed, and say in one line what they could ask \
    you to set. You never edit the files in .argonauts/ yourself.";

/// Only the main session has it. A conversation carried on from a run's agent is that agent
/// at work, and no agent of a crew changes your setup.
pub(crate) fn offered(_ctx: &Ctx, chat: &ChatState) -> bool {
    chat.place.is_none()
}

pub(crate) fn call(ctx: &mut Ctx, chat: &mut ChatState, call: Call) -> Vec<Effect> {
    let said = chat
        .turn
        .as_ref()
        .filter(|turn| turn.id == call.turn)
        .map(|turn| turn.text.clone());
    match said {
        Some(said) => propose(ctx, chat, call, &said),
        None => {
            call.answer("That turn is over, so nothing was shown.");
            Vec::new()
        }
    }
}

/// Checks the form against your message `said`, then shows the card, or keeps the setup
/// where your words said to.
pub(super) fn propose(ctx: &mut Ctx, chat: &mut ChatState, call: Call, said: &str) -> Vec<Effect> {
    match form::check(ctx, &call.arguments, said) {
        Err(problems) => {
            fail(call, problems);
            Vec::new()
        }
        Ok(proposal) => match proposal.keep {
            Some(place) => card::keep_as_said(ctx, chat, call, proposal, place),
            None => {
                card::open(ctx, chat, call, proposal);
                Vec::new()
            }
        },
    }
}
