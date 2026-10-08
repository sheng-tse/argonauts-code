//! App choices as cards: the unclear request, the scope and safety questions the controller
//! asks, trust, gate commands, revert, and the guided setup's steps. Each has exactly the
//! options the controller sent, and its hint line says what Esc does for its kind.

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::events::checkpoint::FreeText;

use super::options::{card_options, first_recommended};
use crate::bottom::CardPrompt;

/// Rows of a choice's body shown on the card. The rest waits for Show the file.
const MAX_BODY_ROWS: usize = 8;
/// Rows of a dry run's table, which is all the card is for.
const MAX_TABLE_ROWS: usize = 24;

/// A setup is what its card asks about, so it may run longer.
const MAX_SETUP_ROWS: usize = 16;

pub fn choice_card(id: u64, choice: &AppChoice) -> CardPrompt {
    let options = card_options(&choice.options, false);
    let mut card = CardPrompt::new(id, choice.title.clone(), choice.title.clone(), options);
    card.selected = first_recommended(&choice.options);
    card.allow_note = choice.free_text != FreeText::None;
    card.esc_hint = esc_hint(choice.kind).to_owned();
    // Every choice asks you, so every one takes the keys. What differs is who set it off. Work
    // raised these after the main agent's or the CLI's reply, which you may rather answer by
    // typing, so Esc hands them on and gives you the input. You set off the rest, so they open
    // with the keys and Esc closes them.
    if raised_by_work(choice.kind) {
        card.needs_you = true;
    } else {
        card = card.yours();
        card.esc_answers = true;
    }

    let mut rows: Vec<String> = choice.body.clone();
    let most = match choice.kind {
        ChoiceKind::TryWorkflow => MAX_TABLE_ROWS,
        _ => MAX_BODY_ROWS,
    };
    if rows.len() > most {
        let hidden = rows.len() - (most - 1);
        rows.truncate(most - 1);
        rows.push(format!("… +{hidden} more lines"));
    }
    match choice.kind {
        // Keys and commands read best framed, the way Claude Code frames what it asks about.
        ChoiceKind::ConfirmSafetySetting
        | ChoiceKind::TrustProject
        | ChoiceKind::DetectedGates
        | ChoiceKind::ConfirmRevert
        | ChoiceKind::ConfirmConnect
        | ChoiceKind::Permission
        | ChoiceKind::TryWorkflow => card.body = rows,
        ChoiceKind::StartWork
        | ChoiceKind::RefusedCommands
        | ChoiceKind::GoAhead
        | ChoiceKind::HandOff
        | ChoiceKind::RewindTurn
        | ChoiceKind::ConfirmTakeOver
        | ChoiceKind::TermsNote
        | ChoiceKind::Init
        | ChoiceKind::SettingScope
        | ChoiceKind::KeepInstructions
        | ChoiceKind::Git => card.detail = rows,
        ChoiceKind::Setup => card.detail = setup_rows(&choice.body),
    }
    card
}

/// A setup's card shows the setup, which ends at the first blank line. What each place would
/// write follows it, for Show what would be written.
fn setup_rows(body: &[String]) -> Vec<String> {
    let mut rows: Vec<String> = body
        .iter()
        .take_while(|line| !line.trim().is_empty())
        .cloned()
        .collect();
    if rows.len() > MAX_SETUP_ROWS {
        let hidden = rows.len() - (MAX_SETUP_ROWS - 1);
        rows.truncate(MAX_SETUP_ROWS - 1);
        rows.push(format!("… +{hidden} more lines"));
    }
    rows
}

/// Work raised it after a reply in the chat, rather than a command or key of yours. A CLI's
/// permission prompt counts as yours, since Esc answers it No, as in Claude Code.
fn raised_by_work(kind: ChoiceKind) -> bool {
    match kind {
        ChoiceKind::StartWork
        | ChoiceKind::RefusedCommands
        | ChoiceKind::GoAhead
        | ChoiceKind::HandOff => true,
        ChoiceKind::Permission
        | ChoiceKind::ConfirmTakeOver
        | ChoiceKind::ConfirmSafetySetting
        | ChoiceKind::TermsNote
        | ChoiceKind::Init
        | ChoiceKind::SettingScope
        | ChoiceKind::TrustProject
        | ChoiceKind::DetectedGates
        | ChoiceKind::ConfirmRevert
        | ChoiceKind::ConfirmConnect
        | ChoiceKind::KeepInstructions
        | ChoiceKind::RewindTurn
        | ChoiceKind::TryWorkflow
        | ChoiceKind::Setup
        | ChoiceKind::Git => false,
    }
}

/// What the controller does when you press Esc, by kind.
pub fn esc_hint(kind: ChoiceKind) -> &'static str {
    match kind {
        ChoiceKind::StartWork | ChoiceKind::KeepInstructions | ChoiceKind::RefusedCommands => {
            crate::bottom::ESC_HINT
        }
        ChoiceKind::Permission | ChoiceKind::GoAhead => "Esc for No",
        ChoiceKind::HandOff => "Esc to stop",
        ChoiceKind::ConfirmTakeOver
        | ChoiceKind::SettingScope
        | ChoiceKind::ConfirmSafetySetting
        | ChoiceKind::ConfirmRevert
        | ChoiceKind::RewindTurn
        | ChoiceKind::Git => "Esc to cancel",
        ChoiceKind::TrustProject
        | ChoiceKind::DetectedGates
        | ChoiceKind::ConfirmConnect
        | ChoiceKind::Setup => "Esc for not now",
        ChoiceKind::TermsNote | ChoiceKind::Init => "Esc to take the recommended answers",
        ChoiceKind::TryWorkflow => "Esc to close",
    }
}
