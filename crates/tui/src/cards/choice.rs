//! App choices as cards: the unclear request, the scope and safety questions the controller
//! asks, trust, gate commands, revert, and the guided setup's steps. Each has exactly the
//! options the controller sent, and its hint line says what Esc does for its kind.

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::events::checkpoint::FreeText;

use super::options::{card_options, first_recommended};
use crate::bottom::CardPrompt;

/// Rows of a choice's body shown on the card. The rest waits for Show the file.
const MAX_BODY_ROWS: usize = 8;

pub fn choice_card(id: u64, choice: &AppChoice) -> CardPrompt {
    let options = card_options(&choice.options, false);
    let mut card = CardPrompt::new(id, choice.title.clone(), choice.title.clone(), options);
    card.selected = first_recommended(&choice.options);
    card.allow_note = choice.free_text != FreeText::None;
    card.esc_hint = esc_hint(choice.kind).to_owned();
    // The unclear request follows the lead's reply, so it waits like a run's card and Esc sets
    // it aside. You set off the rest, so they open with the keys and Esc closes them.
    if choice.kind != ChoiceKind::StartWork {
        card = card.yours();
        card.esc_answers = true;
    }

    let mut rows: Vec<String> = choice.body.clone();
    if rows.len() > MAX_BODY_ROWS {
        let hidden = rows.len() - (MAX_BODY_ROWS - 1);
        rows.truncate(MAX_BODY_ROWS - 1);
        rows.push(format!("… +{hidden} more lines"));
    }
    match choice.kind {
        // Keys and commands read best framed, the way Claude Code frames what it asks about.
        ChoiceKind::ConfirmSafetySetting
        | ChoiceKind::TrustProject
        | ChoiceKind::DetectedGates
        | ChoiceKind::ConfirmRevert
        | ChoiceKind::ConfirmConnect => card.body = rows,
        ChoiceKind::StartWork
        | ChoiceKind::ConfirmTakeOver
        | ChoiceKind::TermsNote
        | ChoiceKind::Init
        | ChoiceKind::SettingScope => card.detail = rows,
    }
    card
}

/// What the controller does when you press Esc, by kind.
pub fn esc_hint(kind: ChoiceKind) -> &'static str {
    match kind {
        ChoiceKind::StartWork => "Esc to set aside",
        ChoiceKind::ConfirmTakeOver
        | ChoiceKind::SettingScope
        | ChoiceKind::ConfirmSafetySetting
        | ChoiceKind::ConfirmRevert => "Esc to cancel",
        ChoiceKind::TrustProject | ChoiceKind::DetectedGates | ChoiceKind::ConfirmConnect => {
            "Esc for not now"
        }
        ChoiceKind::TermsNote | ChoiceKind::Init => "Esc to take the recommended answers",
    }
}
