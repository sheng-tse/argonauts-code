//! "From now on": a request that says to keep doing something applies it to its own run, then
//! asks where to keep it. Code never writes your settings from words; only your answer here
//! does, through the same steps `/config` takes. Words that set the crew show the crew
//! understood, and without a "from now on" ask once where to keep it.

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::names::ChoiceId;
use argonauts_core::settings::SettingScope;

use super::ChatState;
use crate::choices::{self, JUST_THIS_REQUEST};
use crate::controller::instructions::Applied;
use crate::controller::{Ctx, Effect, crew_lines};

/// What a "from now on" card or "Keep this crew?" would keep.
pub(super) struct Kept {
    /// As `(key, TOML value)`.
    pairs: Vec<(String, String)>,
    /// "Keep this crew?", whose answers read as its options do.
    crew: bool,
}

pub(super) fn owns(chat: &ChatState, id: &ChoiceId) -> bool {
    chat.keeps.contains_key(id)
}

/// Asks where to keep what the request said to keep doing, when it said so of anything that
/// was followed.
pub(super) fn offer(ctx: &mut Ctx, chat: &mut ChatState, applied: &Applied) {
    offer_in_run(ctx, chat, applied, None);
}

/// [`offer`] for a run that adds seats of its own around your words: `run` is its whole
/// request layer, so the crew shown is the one that runs.
pub(super) fn offer_in_run(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    applied: &Applied,
    run: Option<&[String]>,
) {
    let crew = crew_lines::understood(ctx.settings(), applied, run);
    if let Some(lines) = &crew {
        ctx.notice(NoteLevel::Info, crew_lines::text(lines));
    }
    if applied.keep.is_empty() {
        if let Some(lines) = crew {
            keep_the_crew(ctx, chat, &lines);
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
    chat.keeps.insert(id, kept);
    ctx.send(AppEvent::Choice { choice });
}

/// Your words set the crew for this request, so one question asks where to keep it. Just
/// this request and this conversation write nothing to a file.
fn keep_the_crew(ctx: &mut Ctx, chat: &mut ChatState, lines: &[crew_lines::CrewLine]) {
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
    chat.keeps.insert(id, Kept { pairs, crew: true });
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
