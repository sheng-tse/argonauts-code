//! An event's options as the card draws them: exactly the options it carries, in its order and
//! numbered. The event's letters stay in the protocol; on screen the numbers alone pick. What an
//! option does decides how it acts on the card.

use argonauts_core::events::checkpoint::{AnswerAction, AnswerOption};

use crate::bottom::{CardOption, OptionKind};

/// `cancel_is_final` is set where Cancel drops a card's work, as on the escalation card.
pub fn card_option(option: &AnswerOption, cancel_is_final: bool) -> CardOption {
    let kind = match option.action {
        AnswerAction::ShowDiff => OptionKind::View,
        AnswerAction::Answer => OptionKind::FreeText,
        AnswerAction::Merge | AnswerAction::Abandon | AnswerAction::Escalate => {
            OptionKind::Irreversible
        }
        AnswerAction::Cancel if cancel_is_final => OptionKind::Irreversible,
        _ => OptionKind::Answer,
    };
    let mut card = CardOption::new(option.label.clone()).with_kind(kind);
    card.description = option
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .map(str::to_owned);
    card.recommended = option.recommended;
    card
}

pub fn card_options(options: &[AnswerOption], cancel_is_final: bool) -> Vec<CardOption> {
    options
        .iter()
        .map(|option| card_option(option, cancel_is_final))
        .collect()
}

/// The pointer starts on the recommended option, so Enter takes it.
pub fn first_recommended(options: &[AnswerOption]) -> usize {
    options
        .iter()
        .position(|option| option.recommended)
        .unwrap_or(0)
}

pub fn answer(key: char, label: &str, action: AnswerAction) -> AnswerOption {
    AnswerOption {
        key,
        label: label.to_owned(),
        action,
        recommended: false,
        detail: None,
    }
}
