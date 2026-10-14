//! The conversation so far, for a session that can't resume it: a summary when a switch to another
//! CLI made one, the latest exchanges in full, and where the whole transcript is.

use std::path::Path;

use argonauts_core::app::TurnReply;
use argonauts_core::events::session::{InputPart, InputPartKind};

use crate::controller::turn;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Exchange {
    request: String,
    answer: String,
}

impl Exchange {
    pub(super) fn new(request: String, reply: &TurnReply) -> Self {
        Self {
            request,
            answer: reply_text(reply).to_owned(),
        }
    }

    pub(super) fn request(&self) -> &str {
        &self.request
    }

    pub(super) fn answer(&self) -> &str {
        &self.answer
    }
}

pub(super) fn reply_text(reply: &TurnReply) -> &str {
    match reply {
        TurnReply::Answer { markdown }
        | TurnReply::Change { markdown, .. }
        | TurnReply::Unclear { markdown, .. } => markdown,
    }
}

/// A summary of the conversation's first `covers` exchanges, which a fresh session starts from.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Summary {
    pub text: String,
    /// The model that wrote it, by name.
    pub writer: String,
    pub covers: usize,
}

/// The conversation so far, for a session that can't resume it: `summary` when there is one, the
/// last `recent` exchanges in full (every one with `None`) and any the summary doesn't cover, and
/// the transcript's path.
pub(super) fn excerpt(
    history: &[Exchange],
    summary: Option<&Summary>,
    recent: Option<usize>,
    transcript: &Path,
) -> Option<InputPart> {
    if history.is_empty() {
        return None;
    }
    let summary = summary.filter(|summary| summary.covers > 0);
    let mut full_from = recent.map_or(0, |recent| history.len().saturating_sub(recent));
    if let Some(summary) = summary {
        full_from = full_from.min(summary.covers);
    }
    let mut text = String::from(
        "This conversation began in another session, which this one can't resume. What was said so \
         far:\n",
    );
    if let Some(summary) = summary {
        text.push_str(&format!(
            "\nA summary of its first {}, written by {}:\n\n{}\n",
            exchanges(summary.covers),
            summary.writer,
            summary.text.trim()
        ));
    }
    let full = &history[full_from..];
    match (full_from, summary) {
        (0, _) => text.push_str(&format!("\n{} in full, oldest first:\n", every(full.len()))),
        (skipped, Some(_)) => text.push_str(&format!(
            "\nThe last {} in full, oldest first. The summary covers the {} before them.\n",
            exchanges(full.len()),
            exchanges(skipped)
        )),
        (skipped, None) => text.push_str(&format!(
            "\nThe last {} in full, oldest first. Left out before them: {}.\n",
            exchanges(full.len()),
            exchanges(skipped)
        )),
    }
    for exchange in full {
        text.push_str(&format!(
            "\nYou were asked:\n{}\n\nYou answered:\n{}\n",
            exchange.request.trim(),
            exchange.answer.trim()
        ));
    }
    text.push_str(&format!(
        "\nThe whole conversation is recorded in {}, one JSON event per line. Read it when you \
         need more than this.\n",
        transcript.display()
    ));
    Some(turn::part(
        InputPartKind::Fallback,
        "conversation so far",
        text,
    ))
}

/// `1 exchange`, `3 exchanges`.
pub(super) fn exchanges(count: usize) -> String {
    match count {
        1 => "1 exchange".to_owned(),
        count => format!("{count} exchanges"),
    }
}

/// `The 1 exchange`, `All 3 exchanges`.
fn every(count: usize) -> String {
    match count {
        1 => "The 1 exchange".to_owned(),
        count => format!("All {count} exchanges"),
    }
}

pub(super) fn cut(text: &str, chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(chars).collect();
    short.push('…');
    short
}
