//! The `StartWork` choice, offered when triage can't tell whether a request is a change.

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::RunMode;
use argonauts_core::names::{ChoiceId, TurnId};
use argonauts_core::steps::intake::ChangeEstimate;

use super::{ChatState, Turn, run_request};
use crate::choices::{self, JUST_ASKING};
use crate::controller::recap::mode_name;
use crate::controller::{Ctx, Effect};

pub(super) struct Offer {
    turn: TurnId,
    request: String,
    estimate: Option<ChangeEstimate>,
}

pub(crate) fn owns_choice(chat: &ChatState, id: &ChoiceId) -> bool {
    chat.offers.contains_key(id)
}

/// The `StartWork` choice: solo, crew, or no. A note added with Tab goes with the request.
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    action: AnswerAction,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let Some(offer) = chat.offers.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    let picked = match &action {
        AnswerAction::Choose { value } => match value.as_str() {
            "solo" => Some(RunMode::Solo),
            "crew" => Some(RunMode::Crew),
            "no" => None,
            other => {
                chat.offers.insert(id, offer);
                anyhow::bail!("{other:?} isn't one of this question's answers");
            }
        },
        AnswerAction::Cancel => None,
        other => {
            chat.offers.insert(id, offer);
            anyhow::bail!("{other:?} doesn't answer whether to start work");
        }
    };
    let Some(mode) = picked else {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: JUST_ASKING.to_owned(),
        });
        return Ok(Vec::new());
    };
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: format!("Starting a {} run.", mode_name(mode)),
    });
    let mut request = offer.request;
    if let Some(note) = text.map(|note| note.trim().to_owned())
        && !note.is_empty()
    {
        request = format!("{request}\n\n{note}");
    }
    let request = run_request(ctx, request, mode, offer.estimate, Some(offer.turn));
    Ok(vec![Effect::StartRun(Box::new(request))])
}

/// Esc on the `StartWork` card sets it aside, like a run's card: it stays answerable.
pub(crate) fn set_aside(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
) -> anyhow::Result<Vec<Effect>> {
    let _ = (ctx, chat, id);
    Ok(Vec::new())
}

pub(super) fn offer_to_start(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: &Turn,
    estimate: Option<ChangeEstimate>,
    would_pick: Option<RunMode>,
) {
    let id = ctx.next_choice_id();
    let title = match would_pick {
        Some(mode) => format!(
            "This could be a change request. Start work on it? Triage would pick {}.",
            mode_name(mode)
        ),
        None => "This could be a change request. Start work on it?".to_owned(),
    };
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::StartWork,
        title,
        Vec::new(),
        choices::options(ChoiceKind::StartWork),
    );
    chat.offers.insert(
        id,
        Offer {
            turn: turn.id.clone(),
            request: turn.text.clone(),
            estimate,
        },
    );
    ctx.send(AppEvent::Choice { choice });
}
