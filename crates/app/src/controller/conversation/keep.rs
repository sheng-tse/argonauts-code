//! "From now on": a request that says to keep doing something applies it to its own run, then
//! asks where to keep it. Code never writes your settings from words; only your answer here
//! does, through the same steps `/config` takes. Words that set the crew show the crew
//! understood, and without a "from now on" ask once where to keep it.

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::names::ChoiceId;
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::approvals::ApprovalMode;

use super::ConversationState;
use crate::choices::{self, JUST_THIS_REQUEST};
use crate::controller::instructions::Applied;
use crate::controller::{Ctx, Effect, crew_lines};

/// Said on autopilot in place of asking where to keep what the request set.
pub(super) const KEPT_ON_AUTOPILOT: &str =
    "Decided on autopilot · this applies to this request only. /config keeps it for later ones.";

/// What a "from now on" question or "Keep this crew?" would keep.
pub(super) struct Kept {
    /// As `(key, TOML value)`.
    pairs: Vec<(String, String)>,
    /// "Keep this crew?", whose answers read as its options do.
    crew: bool,
}

pub(super) fn owns(conversation: &ConversationState, id: &ChoiceId) -> bool {
    conversation.keeps.contains_key(id)
}

/// Asks where to keep what the request said to keep doing, when it said so of anything that
/// was followed.
pub(super) fn offer(ctx: &mut Ctx, conversation: &mut ConversationState, applied: &Applied) {
    offer_in_run(ctx, conversation, applied, None);
}

/// [`offer`] for a run that adds models of its own around your words: `run` is its whole
/// request layer, so the crew shown is the one that runs.
pub(super) fn offer_in_run(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    applied: &Applied,
    run: Option<&[String]>,
) {
    let crew = crew_lines::understood(ctx.settings(), applied, run);
    if let Some(lines) = &crew {
        ctx.notice(NoteLevel::Info, crew_lines::text(lines));
    }
    // Nothing waits for you on autopilot: what you said applies to this request, nothing is
    // written, and one line says how to keep it.
    if super::direct::approval(conversation) == ApprovalMode::Autopilot {
        if crew.is_some() || !applied.keep.is_empty() {
            ctx.notice(NoteLevel::Info, KEPT_ON_AUTOPILOT);
        }
        return;
    }
    if applied.keep.is_empty() {
        if let Some(lines) = crew {
            keep_the_crew(ctx, conversation, &lines);
        }
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
    let kept = Kept {
        pairs: applied.keep.clone(),
        crew: false,
    };
    conversation.keeps.insert(id, kept);
    ctx.send(AppEvent::Choice { choice });
}

/// Your words set the crew for this request, so one question asks where to keep it. Just
/// this request and this conversation write nothing to a file.
fn keep_the_crew(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    lines: &[crew_lines::CrewLine],
) {
    let pairs = crew_lines::to_keep(ctx.settings(), lines);
    if pairs.is_empty() {
        return;
    }
    let id = ctx.next_choice_id();
    let choice = choices::app_choice(
        id.clone(),
        ChoiceKind::KeepInstructions,
        "Keep this crew?",
        Vec::new(),
        choices::keep_crew_options(),
    );
    conversation.keeps.insert(id, Kept { pairs, crew: true });
    ctx.send(AppEvent::Choice { choice });
}

pub(super) fn answer(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    id: ChoiceId,
    action: AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    let Some(kept) = conversation.keeps.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    let value = match &action {
        AnswerAction::Choose { value } => value.clone(),
        AnswerAction::Cancel => JUST_THIS_REQUEST.to_owned(),
        other => {
            conversation.keeps.insert(id, kept);
            anyhow::bail!("{other:?} doesn't say where to keep this");
        }
    };
    if value == JUST_THIS_REQUEST {
        just_this_request(ctx, id);
        return Ok(Vec::new());
    }
    let Some(scope) = choices::scope_from_value(&value) else {
        conversation.keeps.insert(id, kept);
        anyhow::bail!("{value:?} isn't a place to keep this");
    };
    let outcome = match scope {
        SettingScope::Session if kept.crew => "This conversation.",
        SettingScope::Project if kept.crew => "Saved for this repository.",
        scope => choices::scope_label(scope),
    };
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.to_owned(),
    });
    Ok(vec![Effect::SaveSettings {
        pairs: kept.pairs,
        scope,
    }])
}

/// Autopilot came on with "Keep this for later requests?" or "Keep this crew?" open: it
/// applies to its request alone, as one asked on autopilot does, and nothing is written.
pub(super) fn settle_on_autopilot(ctx: &Ctx, conversation: &mut ConversationState) {
    let mut open: Vec<ChoiceId> = conversation.keeps.keys().cloned().collect();
    open.sort();
    for id in open {
        conversation.keeps.remove(&id);
        ctx.send(AppEvent::ChoiceClosed {
            id,
            outcome: KEPT_ON_AUTOPILOT.to_owned(),
        });
    }
}

/// Esc keeps it for this request only.
pub(super) fn set_aside(ctx: &mut Ctx, conversation: &mut ConversationState, id: ChoiceId) {
    if conversation.keeps.remove(&id).is_some() {
        just_this_request(ctx, id);
    }
}

fn just_this_request(ctx: &Ctx, id: ChoiceId) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: "Just this request.".to_owned(),
    });
}
