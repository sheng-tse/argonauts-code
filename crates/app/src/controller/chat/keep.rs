//! "From now on": a request that says to keep doing something applies it to its own run, then
//! asks where to keep it. Code never writes your settings from words; only your answer here
//! does, through the same steps `/config` takes.

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::names::ChoiceId;

use super::ChatState;
use crate::choices::{self, JUST_THIS_REQUEST};
use crate::controller::instructions::Applied;
use crate::controller::{Ctx, Effect};

/// What a "from now on" card would keep, as `(key, TOML value)`.
pub(super) type Kept = Vec<(String, String)>;

pub(super) fn owns(chat: &ChatState, id: &ChoiceId) -> bool {
    chat.keeps.contains_key(id)
}

/// Asks where to keep what the request said to keep doing, when it said so of anything that
/// was followed.
pub(super) fn offer(ctx: &mut Ctx, chat: &mut ChatState, applied: &Applied) {
    if applied.keep.is_empty() {
        return;
    }
    let id = ctx.next_choice_id();
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::KeepInstructions,
        "Keep this for later requests?",
        vec![applied.keep_said.join(" · ")],
        choices::options(ChoiceKind::KeepInstructions),
    );
    chat.keeps.insert(id, applied.keep.clone());
    ctx.send(AppEvent::Choice { choice });
}

pub(super) fn answer(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    action: AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    let Some(kept) = chat.keeps.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    let value = match &action {
        AnswerAction::Choose { value } => value.clone(),
        AnswerAction::Cancel => JUST_THIS_REQUEST.to_owned(),
        other => {
            chat.keeps.insert(id, kept);
            anyhow::bail!("{other:?} doesn't say where to keep this");
        }
    };
    if value == JUST_THIS_REQUEST {
        just_this_request(ctx, id);
        return Ok(Vec::new());
    }
    let Some(scope) = choices::scope_from_value(&value) else {
        chat.keeps.insert(id, kept);
        anyhow::bail!("{value:?} isn't a place to keep this");
    };
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: choices::scope_label(scope).to_owned(),
    });
    Ok(vec![Effect::SaveSettings { pairs: kept, scope }])
}

/// Esc keeps it for this request only.
pub(super) fn set_aside(ctx: &mut Ctx, chat: &mut ChatState, id: ChoiceId) {
    if chat.keeps.remove(&id).is_some() {
        just_this_request(ctx, id);
    }
}

fn just_this_request(ctx: &Ctx, id: ChoiceId) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: "Just this request.".to_owned(),
    });
}
