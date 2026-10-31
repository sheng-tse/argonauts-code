//! What you tell the main session about Argonauts Code itself, on the app's side. It sets things up
//! from what you say (`argonauts_setup`), opens the guide one topic at a time
//! (`argonauts_guide`), and answers from Argonauts Code's own records (`argonauts_lookup`). The
//! session's server lists these tools and sends each call here by its name.
//!
//! Each tool is a file of its own with the same four things: its `NAME`, the line the main
//! session's standing instructions say `ABOUT` it, whether a turn is `offered` it, and what a
//! `call` does.

mod form;
pub(crate) mod guide;
pub(crate) mod lookup;
mod question;
mod records;
pub(crate) mod run_checks;
pub(crate) mod setup;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use argonauts_core::base::names::{ChoiceId, TurnId};
use argonauts_core::runs::asking::AskReply;
use argonauts_core::runs::events::approval::AnswerAction;
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::workflows::required::{RequiredStep, same_name};

use super::ConversationState;
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
    /// Open setup questions, each with what it would write and the call waiting on it.
    questions: HashMap<ChoiceId, question::Open>,
    /// Settings a setup kept for one request, held for the workflow that request starts.
    for_request: Option<(TurnId, Vec<(String, String)>)>,
    /// Steps every workflow must contain, kept for one request.
    steps_for_request: Option<(TurnId, Vec<RequiredStep>)>,
    /// Steps every workflow must contain, kept for this conversation.
    steps_for_conversation: Vec<RequiredStep>,
    /// The checks the latest run here held for itself alone, which your words may keep.
    run_checks: Vec<CheckCommand>,
}

pub(crate) fn owns(conversation: &ConversationState, id: &ChoiceId) -> bool {
    conversation.talk.questions.contains_key(id)
}

pub(crate) fn answer(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    action: AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    question::answer(ctx, conversation, id, action)
}

pub(crate) fn set_aside(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
) -> Vec<Effect> {
    question::not_now(ctx, conversation, id)
}

/// Autopilot came on with a setup's question open: the setup holds where one proposed on
/// autopilot would.
pub(crate) fn settle_on_autopilot(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> Vec<Effect> {
    question::settle_on_autopilot(ctx, conversation)
}

/// The settings a setup kept for `turn`'s request alone, as `key=value`, for the workflow that
/// request starts. They hold for that workflow and no other.
pub(crate) fn request_settings(conversation: &mut ConversationState, turn: &TurnId) -> Vec<String> {
    // One kept for an earlier request whose workflow never started is dropped with it.
    match conversation.talk.for_request.take() {
        Some((kept_for, pairs)) if &kept_for == turn => pairs
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        _ => Vec::new(),
    }
}

/// The steps a setup kept for this conversation, which `/workflow try` and the marks on the
/// conversation's own workflow count with the ones `always.toml` lists.
pub(crate) fn conversation_steps(conversation: &ConversationState) -> &[RequiredStep] {
    &conversation.talk.steps_for_conversation
}

/// The steps a setup kept for this conversation, and for `turn`'s request alone, for the
/// workflow that request starts. They go after the ones `always.toml` lists, so a step of the
/// same name is the one kept here, and before what the request's own words say.
pub(crate) fn required_steps(
    conversation: &mut ConversationState,
    turn: &TurnId,
) -> Vec<RequiredStep> {
    let mut steps = conversation.talk.steps_for_conversation.clone();
    if let Some((kept_for, kept)) = conversation.talk.steps_for_request.take()
        && &kept_for == turn
    {
        for step in kept {
            steps.retain(|known| !same_name(&known.name, &step.name));
            steps.push(step);
        }
    }
    steps
}
