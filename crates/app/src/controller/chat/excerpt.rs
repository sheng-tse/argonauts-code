//! The chat so far, for a session that can't resume it.

use argonauts_core::app::TurnReply;
use argonauts_core::events::session::{InputPart, InputPartKind};

use crate::controller::turn;

/// How many earlier exchanges prime a fresh session, and how long each part may be.
const EXCERPT_EXCHANGES: usize = 6;
const EXCERPT_CHARS: usize = 1500;

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

/// The chat so far, for a session that can't resume it.
pub(super) fn excerpt(history: &[Exchange]) -> Option<InputPart> {
    if history.is_empty() {
        return None;
    }
    let skipped = history.len().saturating_sub(EXCERPT_EXCHANGES);
    let mut text = String::from(
        "This chat began in another session, which this one can't resume. What was said so \
         far, oldest first:\n",
    );
    if skipped > 0 {
        text.push_str(&format!("\n({skipped} earlier exchanges left out)\n"));
    }
    for exchange in &history[skipped..] {
        text.push_str(&format!(
            "\nYou were asked:\n{}\n\nYou answered:\n{}\n",
            cut(&exchange.request, EXCERPT_CHARS),
            cut(&exchange.answer, EXCERPT_CHARS)
        ));
    }
    Some(turn::part(InputPartKind::Handoff, "chat so far", text))
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
