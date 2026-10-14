//! The `StartWork` choice, offered when the estimate can't tell whether a request is a change.

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::events::run::RunMode;
use argonauts_core::names::{ChoiceId, TurnId};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::protocol::CaptainKind;
use argonauts_core::steps::intake::{ChangeEstimate, Instruction};

use super::{ConversationState, Queued, Turn, direct, handover, keep, run_request};
use crate::choices::{self, JUST_ASKING};
use crate::controller::captain_or_crew::Requested;
use crate::controller::instructions::{self, Command, Roster};
use crate::controller::recap::mode_name;
use crate::controller::{Ctx, Effect};

pub(super) struct Offer {
    turn: TurnId,
    request: String,
    estimate: Option<ChangeEstimate>,
    /// What the request said about how to run it, checked again against your pick.
    how_to_run: Vec<Instruction>,
    /// What starts when nobody is asked: the mode your words asked for, else the one
    /// Argonauts Code would pick.
    unasked: Option<RunMode>,
}

pub(crate) fn owns_choice(conversation: &ConversationState, id: &ChoiceId) -> bool {
    conversation.offers.contains_key(id) || direct::owns_choice(conversation, id)
}

/// The `StartWork` choice: Captain, crew, or no. A note added with Tab goes with the request.
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    action: AnswerAction,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if direct::owns_choice(conversation, &id) {
        return direct::answer_choice(ctx, conversation, id, &action);
    }
    let Some(offer) = conversation.offers.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    let picked = match &action {
        AnswerAction::Choose { value } => match value.as_str() {
            "captain" => Some(RunMode::Captain),
            "crew" => Some(RunMode::Crew),
            "no" => None,
            other => {
                conversation.offers.insert(id, offer);
                anyhow::bail!("{other:?} isn't one of this question's answers");
            }
        },
        AnswerAction::Cancel => None,
        other => {
            conversation.offers.insert(id, offer);
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
    start(ctx, conversation, Some(id), offer, mode, text)
}

/// Starts work on the offered request as `mode`. `id` is the question this answers, which
/// closes saying what started; autopilot starts with no question asked.
fn start(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: Option<ChoiceId>,
    offer: Offer,
    mode: RunMode,
    text: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let close = |ctx: &Ctx, outcome: String| {
        if let Some(id) = id.clone() {
            ctx.send(AppEvent::ChoiceClosed { id, outcome });
        }
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
    // Captain you pick here works in your checkout, as `/captain` does, unless your words asked for
    // a worktree.
    let worktree_asked = applied
        .as_ref()
        .is_some_and(|applied| applied.asked.captain == Some(CaptainKind::Checked));
    if mode == RunMode::Captain && direct::captain_is_direct(ctx.settings()) && !worktree_asked {
        close(ctx, "Working on it in your checkout.".to_owned());
        if conversation.turn.is_some() {
            conversation.queue(Queued::Captain(request));
            return Ok(Vec::new());
        }
        let main = ctx.settings().crew.main.agent.clone();
        let words = applied
            .as_ref()
            .map(|applied| applied.for_direct(&main))
            .unwrap_or_default();
        if conversation.prefs.plan_first {
            return direct::plan(ctx, conversation, request, words);
        }
        return direct::begin_with(ctx, conversation, request, None, words);
    }
    close(ctx, format!("Starting a {} run.", mode_name(mode)));
    let mut request = run_request(ctx, request, mode, offer.estimate, Some(offer.turn));
    if let Some(applied) = &applied {
        applied.apply_to(&mut request);
        keep::offer(ctx, conversation, applied);
    }
    Ok(vec![handover::start(request)])
}

fn mode_words(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Captain => "captain",
        RunMode::Crew => "the crew",
    }
}

/// Esc on the `StartWork` question goes on without you: the answer the reply gave
/// stands and nothing starts, as when Argonauts Code can't tell.
pub(crate) fn set_aside(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
) -> anyhow::Result<Vec<Effect>> {
    if direct::owns_choice(conversation, &id) {
        return direct::set_aside(ctx, conversation, id);
    }
    if conversation.offers.remove(&id).is_some() {
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: NOTHING_STARTED.to_owned(),
        });
    }
    Ok(Vec::new())
}

/// What Esc on the start-work question leaves.
const NOTHING_STARTED: &str = "No · nothing started";

/// Asks whether to start work on a request that could be a change. On autopilot nothing waits
/// for you: the work starts in the mode your words asked for, else the one Argonauts Code
/// would pick, and with neither nothing starts. One line says which.
pub(super) fn offer_to_start(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: &Turn,
    estimate: Option<ChangeEstimate>,
    would_pick: Option<RunMode>,
    requested: Requested,
) -> Vec<Effect> {
    let offer = Offer {
        turn: turn.id.clone(),
        request: turn.text.clone(),
        estimate,
        unasked: requested.applied.asked.mode.or(would_pick),
        how_to_run: requested.how_to_run,
    };
    if direct::approval(conversation) == ApprovalMode::Autopilot {
        let Some(mode) = offer.unasked else {
            ctx.notice(NoteLevel::Info, NOTHING_SAYS_HOW);
            return Vec::new();
        };
        ctx.notice(NoteLevel::Info, starts_unasked(mode));
        return start_unasked(ctx, conversation, offer, mode);
    }
    let id = ctx.next_choice_id();
    let title = match would_pick {
        Some(mode) => format!(
            "This could be a change request. Start work on it? Argonauts Code would pick {}.",
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
    conversation.offers.insert(id, offer);
    ctx.send(AppEvent::Choice { choice });
    Vec::new()
}

/// Said on autopilot when neither the request's words nor the rules say how to run it.
const NOTHING_SAYS_HOW: &str = "Decided on autopilot · this could be a change request, but \
                                nothing says how to run it, so nothing started. /captain or \
                                /crew starts it.";

fn starts_unasked(mode: RunMode) -> String {
    format!(
        "Decided on autopilot · this could be a change request, so {} starts work on it.",
        mode_words(mode)
    )
}

/// Starts the offered request with no question. A start that fails is said, and nothing waits.
fn start_unasked(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    offer: Offer,
    mode: RunMode,
) -> Vec<Effect> {
    start(ctx, conversation, None, offer, mode, None).unwrap_or_else(|err| {
        ctx.notice(NoteLevel::Warn, format!("The work didn't start: {err:#}"));
        Vec::new()
    })
}

/// Autopilot came on with "Start work on it?" open: each request starts as one that came in on
/// autopilot would, and its question closes with the line that says so.
pub(super) fn settle_on_autopilot(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> Vec<Effect> {
    let mut open: Vec<ChoiceId> = conversation.offers.keys().cloned().collect();
    open.sort();
    let mut effects = Vec::new();
    for id in open {
        let Some(offer) = conversation.offers.remove(&id) else {
            continue;
        };
        let Some(mode) = offer.unasked else {
            ctx.send(AppEvent::ChoiceClosed {
                id,
                outcome: NOTHING_SAYS_HOW.to_owned(),
            });
            continue;
        };
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: starts_unasked(mode),
        });
        effects.extend(start_unasked(ctx, conversation, offer, mode));
    }
    effects
}
