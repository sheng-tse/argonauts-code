//! An event's options as the question draws them: exactly the options it carries, in its order and
//! numbered. The event's letters stay in the protocol; on screen the numbers alone pick. What an
//! option does decides how it acts on the question.

use argonauts_core::events::approval::{AnswerAction, AnswerOption};

use crate::bottom::{OptionKind, QuestionOption};

/// `cancel_is_final` is set where Cancel drops a task's work, as on the stuck question.
pub fn question_option(option: &AnswerOption, cancel_is_final: bool) -> QuestionOption {
    let kind = match option.action {
        AnswerAction::ShowDiff => OptionKind::View,
        AnswerAction::Answer => OptionKind::FreeText,
        AnswerAction::Merge | AnswerAction::Abandon | AnswerAction::AskYou => {
            OptionKind::Irreversible
        }
        AnswerAction::Cancel if cancel_is_final => OptionKind::Irreversible,
        _ => OptionKind::Answer,
    };
    let mut shown = QuestionOption::new(option.label.clone()).with_kind(kind);
    shown.description = option
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .map(str::to_owned);
    shown.recommended = option.recommended;
    shown
}

pub fn question_options(options: &[AnswerOption], cancel_is_final: bool) -> Vec<QuestionOption> {
    options
        .iter()
        .map(|option| question_option(option, cancel_is_final))
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
