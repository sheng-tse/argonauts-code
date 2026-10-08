//! The `StartWork` choice, offered when triage can't tell whether a request is a change.

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::RunMode;
use argonauts_core::names::{ChoiceId, TurnId};
use argonauts_core::settings::protocol::SoloStyle;
use argonauts_core::steps::intake::{ChangeEstimate, Instruction};

use super::{ChatState, Queued, Turn, direct, handover, keep, run_request};
use crate::choices::{self, JUST_ASKING};
use crate::controller::instructions::{self, Command, Roster};
use crate::controller::recap::mode_name;
use crate::controller::triage::Requested;
use crate::controller::{Ctx, Effect};

pub(super) struct Offer {
    turn: TurnId,
    request: String,
    estimate: Option<ChangeEstimate>,
    /// What the request said about how to run it, checked again against your pick.
    how_to_run: Vec<Instruction>,
}

pub(crate) fn owns_choice(chat: &ChatState, id: &ChoiceId) -> bool {
    chat.offers.contains_key(id) || direct::owns_choice(chat, id)
}

/// The `StartWork` choice: solo, crew, or no. A note added with Tab goes with the request.
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    action: AnswerAction,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if direct::owns_choice(chat, &id) {
        return direct::answer_choice(ctx, chat, id, &action);
    }
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
    let mut request = offer.request;
    // Your pick is a command, so words that ask for the other mode aren't followed.
    let applied = (!offer.how_to_run.is_empty()).then(|| {
        let picked = Command {
            mode,
            said: format!("you chose {}", mode_words(mode)),
            style: None,
        };
        instructions::check(
            &offer.how_to_run,
            &request,
            ctx.settings(),
            &Roster::new(ctx),
            Some(&picked),
        )
    });
    if let Some(note) = text.map(|note| note.trim().to_owned())
        && !note.is_empty()
    {
        request = format!("{request}\n\n{note}");
    }
    // Solo you pick here works in your checkout, as `/solo` does, unless your words asked for
    // a copy.
    let copy_asked = applied
        .as_ref()
        .is_some_and(|applied| applied.asked.solo == Some(SoloStyle::Checked));
    if mode == RunMode::Solo && direct::solo_is_direct(ctx.settings()) && !copy_asked {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: "Working on it in your checkout.".to_owned(),
        });
        if chat.turn.is_some() {
            chat.queue(Queued::Solo(request));
            return Ok(Vec::new());
        }
        let main = ctx.settings().main_agent.worker.clone();
        let words = applied
            .as_ref()
            .map(|applied| applied.for_direct(&main))
            .unwrap_or_default();
        if chat.prefs.plan_first {
            return direct::plan(ctx, chat, request, words);
        }
        return direct::begin_with(ctx, chat, request, None, words);
    }
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: format!("Starting a {} run.", mode_name(mode)),
    });
    let mut request = run_request(ctx, request, mode, offer.estimate, Some(offer.turn));
    if let Some(applied) = &applied {
        applied.apply_to(&mut request);
        keep::offer(ctx, chat, applied);
    }
    Ok(vec![handover::start(request)])
}

fn mode_words(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Solo => "solo",
        RunMode::Crew => "the crew",
    }
}

/// Esc on the `StartWork` card goes on without you: the answer the reply gave
/// stands and nothing starts, as when Argonauts can't tell.
pub(crate) fn set_aside(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
) -> anyhow::Result<Vec<Effect>> {
    if direct::owns_choice(chat, &id) {
        return direct::set_aside(ctx, chat, id);
    }
    if chat.offers.remove(&id).is_some() {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: NOTHING_STARTED.to_owned(),
        });
    }
    Ok(Vec::new())
}

/// What Esc on the start-work card leaves.
const NOTHING_STARTED: &str = "No · nothing started";

pub(super) fn offer_to_start(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: &Turn,
    estimate: Option<ChangeEstimate>,
    would_pick: Option<RunMode>,
    requested: Requested,
) {
    let id = ctx.next_choice_id();
    let title = match would_pick {
        Some(mode) => format!(
            "This could be a change request. Start work on it? Argonauts would pick {}.",
            mode_name(mode)
        ),
        None => "This could be a change request. Start work on it?".to_owned(),
    };
    // The mode your words ask for comes first, so Enter takes it.
    let mut options = choices::options(ChoiceKind::StartWork);
    if requested.applied.asked.mode == Some(RunMode::Crew) {
        options.swap(0, 1);
    }
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::StartWork,
        title,
        Vec::new(),
        options,
    );
    chat.offers.insert(
        id,
        Offer {
            turn: turn.id.clone(),
            request: turn.text.clone(),
            estimate,
            how_to_run: requested.how_to_run,
        },
    );
    ctx.send(AppEvent::Choice { choice });
}
