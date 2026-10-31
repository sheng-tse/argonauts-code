//! The title says `◆ needs you` while a question waits, since a crew can wait on you for hours in a
//! tab you aren't looking at.
//!
//! Title text comes from session names and agent output, so it is sanitized the way Codex's
//! `terminal_title.rs` (Apache-2.0) does: no control characters that could end or reshape the
//! OSC sequence, no invisible or bidirectional formatting, whitespace collapsed, length capped.

use std::fmt;

use crossterm::Command;

use crate::base::text::sanitize;
use crate::base::theme::Glyphs;

/// Longest title written, in characters. Terminals cut long titles anyway.
pub const MAX_TITLE_CHARS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitleStatus {
    Working,
    Idle,
    /// Wins over working.
    NeedsYou,
}

/// While working, `frame` picks the half circle.
pub fn title_text(status: TitleStatus, session: &str, frame: usize, glyphs: &Glyphs) -> String {
    let session = sanitize_title(session);
    let glyph = match status {
        TitleStatus::NeedsYou => {
            return if session.is_empty() {
                format!("{} needs you", glyphs.question)
            } else {
                format!("{} needs you · {session}", glyphs.question)
            };
        }
        TitleStatus::Working if !glyphs.title_busy.is_empty() => {
            glyphs.title_busy[frame % glyphs.title_busy.len()]
        }
        _ => glyphs.title_idle,
    };
    if session.is_empty() {
        glyph.to_owned()
    } else {
        format!("{glyph} {session}")
    }
}

/// Untrusted text made safe for an OSC title, on one line and at most [`MAX_TITLE_CHARS`] long.
pub fn sanitize_title(title: &str) -> String {
    // Whole escape sequences go first, so their parameters don't survive as visible text.
    let title = sanitize(title);
    let mut out = String::new();
    let mut written = 0;
    let mut pending_space = false;
    for c in title.chars() {
        if c.is_whitespace() {
            // Only after content, which also drops leading whitespace.
            pending_space = !out.is_empty();
            continue;
        }
        if is_hidden_in_title(c) {
            continue;
        }
        if pending_space && MAX_TITLE_CHARS.saturating_sub(written) > 1 {
            out.push(' ');
            written += 1;
            pending_space = false;
        }
        if written >= MAX_TITLE_CHARS {
            break;
        }
        out.push(c);
        written += 1;
    }
    out
}

/// Control characters, plus invisible and bidirectional formatting that could make a title
/// read differently from its text.
fn is_hidden_in_title(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{E0100}'..='\u{E01EF}'
        )
}

/// OSC 0, terminated with BEL, which more terminals accept than ST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTitle(pub String);

impl Command for SetTitle {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b]0;{}\x07", self.0)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::other("titles need ANSI support"))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_titles_alternate_and_idle_titles_rest() {
        let g = Glyphs::UNICODE;
        assert_eq!(
            title_text(TitleStatus::Working, "README.md review", 1, &g),
            "◑ README.md review"
        );
        // Two frames only, as Claude Code's title.
        assert_eq!(title_text(TitleStatus::Working, "x", 2, &g), "◐ x");
        assert_eq!(
            title_text(TitleStatus::Idle, "Homework tutoring setup", 3, &g),
            "✳ Homework tutoring setup"
        );
        assert_eq!(title_text(TitleStatus::Idle, " \x07 ", 0, &g), "✳");
    }

    #[test]
    fn a_waiting_question_says_so_in_the_title() {
        let g = Glyphs::UNICODE;
        assert_eq!(
            title_text(TitleStatus::NeedsYou, "Add --json to export", 1, &g),
            "◆ needs you · Add --json to export"
        );
        assert_eq!(title_text(TitleStatus::NeedsYou, "", 0, &g), "◆ needs you");
    }

    #[test]
    fn sanitizes_controls_and_whitespace() {
        assert_eq!(
            sanitize_title("  Project\t|\nWorking\x1b\x07\u{009D}\u{009C} |  Thread  "),
            "Project | Working | Thread"
        );
    }

    #[test]
    fn escape_sequences_leave_nothing_behind() {
        assert_eq!(sanitize_title("\x1b]0;evil\x07ok \x1b[31mred"), "ok red");
    }

    #[test]
    fn strips_invisible_and_bidi_formatting() {
        assert_eq!(
            sanitize_title("Pro\u{202E}j\u{2066}e\u{200F}c\u{061C}t\u{200B} \u{FEFF}T\u{2060}itle"),
            "Project Title"
        );
    }

    #[test]
    fn caps_the_length() {
        let long = "a".repeat(MAX_TITLE_CHARS + 10);
        assert_eq!(sanitize_title(&long).chars().count(), MAX_TITLE_CHARS);

        let with_space = format!("{} b", "a".repeat(MAX_TITLE_CHARS - 1));
        let cut = sanitize_title(&with_space);
        assert_eq!(cut.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(cut.chars().last(), Some('b'));
    }

    #[test]
    fn writes_osc_zero_with_bel() {
        let mut out = String::new();
        SetTitle("hello".to_owned()).write_ansi(&mut out).unwrap();
        assert_eq!(out, "\x1b]0;hello\x07");
    }
}
