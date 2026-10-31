//! The conversation so far, for a session that can't resume it: a summary when a switch to another
//! CLI made one, the latest exchanges in full, and where the whole transcript is.

use std::path::Path;

use argonauts_core::app::TurnReply;
use argonauts_core::runs::events::session::{InputPart, InputPartKind};

use crate::controller::turns::turn;

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

/// Who reads the conversation so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reader<'a> {
    /// A CLI session that can't resume the conversation's own, which can open the transcript.
    Session { transcript: &'a Path },
    /// A model on a server, which never had a session and has no tools to read a file with.
    Model,
}

/// The conversation so far, for a session that can't resume it: `summary` when there is one, the
/// last `recent` exchanges in full (every one with `None`) and any the summary doesn't cover, and
/// for a session the transcript's path.
pub(super) fn excerpt(
    history: &[Exchange],
    summary: Option<&Summary>,
    recent: Option<usize>,
    reader: Reader<'_>,
) -> Option<InputPart> {
    if history.is_empty() {
        return None;
    }
    let summary = summary.filter(|summary| summary.covers > 0);
    let mut full_from = recent.map_or(0, |recent| history.len().saturating_sub(recent));
    if let Some(summary) = summary {
        full_from = full_from.min(summary.covers);
    }
    let mut text = String::from(match reader {
        Reader::Session { .. } => {
            "This conversation began in another session, which this one can't resume. What was \
             said so far:\n"
        }
        Reader::Model => "What was said in this conversation so far:\n",
    });
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
    if let Reader::Session { transcript } = reader {
        text.push_str(&format!(
            "\nThe whole conversation is recorded in {}, one JSON event per line. Read it when \
             you need more than this.\n",
            transcript.display()
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn history() -> Vec<Exchange> {
        let answer = |text: &str| TurnReply::Answer {
            markdown: text.to_owned(),
        };
        vec![
            Exchange::new("What's the code word?".to_owned(), &answer("PELICAN")),
            Exchange::new("And again?".to_owned(), &answer("Still PELICAN")),
        ]
    }

    #[test]
    fn a_session_hears_it_began_elsewhere_and_where_the_transcript_is() {
        let transcript = Path::new("/home/me/.argonauts/state/c-1.jsonl");
        let part = excerpt(&history(), None, None, Reader::Session { transcript }).unwrap();
        assert!(
            part.text
                .starts_with("This conversation began in another session")
        );
        assert!(part.text.contains("/home/me/.argonauts/state/c-1.jsonl"));
        assert!(part.text.contains("You answered:\nStill PELICAN"));
    }

    #[test]
    fn a_model_on_a_server_hears_the_exchanges_alone() {
        let part = excerpt(&history(), None, Some(1), Reader::Model).unwrap();
        assert!(
            part.text
                .starts_with("What was said in this conversation so far:\n")
        );
        assert!(part.text.contains("You were asked:\nAnd again?"));
        for never in ["session", "resume", "recorded in", "Read it", ".jsonl"] {
            assert!(!part.text.contains(never), "{never}: {}", part.text);
        }
        assert_eq!(part.label, "conversation so far");
    }
}
