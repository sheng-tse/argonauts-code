//! What you tell the main session about Argonauts itself, on the app's side. It sets things up
//! from what you say (`argonauts_setup`), opens the guide one topic at a time
//! (`argonauts_guide`), and answers from Argonauts' own records (`argonauts_lookup`). The
//! session's server lists these tools and sends each call here by its name.
//!
//! Each tool is a file of its own with the same four things: its `NAME`, the line the main
//! session's standing instructions say `ABOUT` it, whether a turn is `offered` it, and what a
//! `call` does.

mod card;
mod form;
pub(crate) mod guide;
pub(crate) mod lookup;
mod records;
pub(crate) mod setup;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use argonauts_core::asking::AskReply;
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::names::{ChoiceId, TurnId};
use argonauts_core::required::{RequiredStep, same_name};

use super::ChatState;
use crate::controller::{Ctx, Effect};

use super::tools::Call;

/// Answers `call` with what the model reads as an error, to fix and call again.
fn fail(call: Call, text: impl Into<String>) {
    let _ = call.reply.send(AskReply {
        text: text.into(),
        is_error: true,
    });
}

#[derive(Default)]
pub(crate) struct TalkState {
    /// Open setup cards, each with what it would write and the call waiting on it.
    cards: HashMap<ChoiceId, card::Open>,
    /// Settings a setup kept for one request, held for the workflow that request starts.
    for_request: Option<(TurnId, Vec<(String, String)>)>,
    /// Steps every workflow must contain, kept for one request.
    steps_for_request: Option<(TurnId, Vec<RequiredStep>)>,
    /// Steps every workflow must contain, kept for this conversation.
    steps_for_conversation: Vec<RequiredStep>,
}

pub(crate) fn owns(chat: &ChatState, id: &ChoiceId) -> bool {
    chat.talk.cards.contains_key(id)
}

pub(crate) fn answer(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    action: AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    card::answer(ctx, chat, id, action)
}

pub(crate) fn set_aside(ctx: &mut Ctx, chat: &mut ChatState, id: ChoiceId) -> Vec<Effect> {
    card::not_now(ctx, chat, id)
}

/// The settings a setup kept for `turn`'s request alone, as `key=value`, for the workflow that
/// request starts. They hold for that workflow and no other.
pub(crate) fn request_settings(chat: &mut ChatState, turn: &TurnId) -> Vec<String> {
    // One kept for an earlier request whose workflow never started is dropped with it.
    match chat.talk.for_request.take() {
        Some((kept_for, pairs)) if &kept_for == turn => pairs
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        _ => Vec::new(),
    }
}

/// The steps a setup kept for this conversation, which `/workflow try` and the marks on the
/// conversation's own workflow count with the ones `always.toml` lists.
pub(crate) fn conversation_steps(chat: &ChatState) -> &[RequiredStep] {
    &chat.talk.steps_for_conversation
}

/// The steps a setup kept for this conversation, and for `turn`'s request alone, for the
/// workflow that request starts. They go after the ones `always.toml` lists, so a step of the
/// same name is the one kept here, and before what the request's own words say.
pub(crate) fn required_steps(chat: &mut ChatState, turn: &TurnId) -> Vec<RequiredStep> {
    let mut steps = chat.talk.steps_for_conversation.clone();
    if let Some((kept_for, kept)) = chat.talk.steps_for_request.take()
        && &kept_for == turn
    {
        for step in kept {
            steps.retain(|known| !same_name(&known.name, &step.name));
            steps.push(step);
        }
    }
    steps
}
