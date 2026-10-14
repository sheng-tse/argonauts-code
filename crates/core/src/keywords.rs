//! Words a CLI treats specially in a prompt, such as Claude Code's `ultrathink`. The input
//! draws them the way that CLI draws them, and the prompt keeps them as typed so they still do
//! what they do. Each adapter keeps its CLI's words in one table, so a new CLI version that adds
//! or drops one is a data change.

use std::borrow::Cow;
use std::ops::Range;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyword {
    /// Found whole and in any case, the way JavaScript's `/\bword\b/gi` finds it: no ASCII
    /// letter, digit, or `_` may touch either end.
    pub word: Cow<'static, str>,
    pub look: KeywordLook,
    /// The transcript's echo of your message draws it too, held still.
    pub echoed: bool,
}

impl Keyword {
    pub const fn new(word: &'static str, look: KeywordLook, echoed: bool) -> Self {
        Self {
            word: Cow::Borrowed(word),
            look,
            echoed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeywordLook {
    /// Letter by letter red, orange, yellow, green, blue, indigo, violet, then red again, with
    /// a lighter band sweeping across while the word sits in the input.
    Rainbow,
    /// One purple with no shimmer, as Claude Code draws `ultracode`.
    Flat,
}

/// Asks for a workflow for the one request it is typed in. Argonauts Code's own word, so it works
/// whichever CLI the main session runs on. Claude Code never draws it in the echo of your
/// message, so neither does the transcript here.
pub const ULTRACODE: Keyword = Keyword::new("ultracode", KeywordLook::Flat, false);

/// Whether `text` asks for a workflow by holding the word. A `/` command never does, so
/// `/effort ultracode on` only flips the switch.
pub fn asks_for_workflow(text: &str) -> bool {
    !text.trim_start().starts_with('/') && !find(text, std::slice::from_ref(&ULTRACODE)).is_empty()
}

/// One keyword in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    pub keyword: &'a Keyword,
    /// Byte offsets into the text.
    pub range: Range<usize>,
}

/// Every keyword in `text`, in order. Where two would overlap, the one that starts first wins,
/// or at the same start the one first in the table. The workflow word counts only where Claude
/// Code counts it ([`counts_as_the_workflow_word`]), so what's drawn purple is what asks.
pub fn find<'a>(text: &str, keywords: &'a [Keyword]) -> Vec<Found<'a>> {
    let mut found: Vec<Found<'a>> = keywords
        .iter()
        .flat_map(|keyword| {
            let workflow = keyword.word.eq_ignore_ascii_case(&ULTRACODE.word);
            whole_words(text, &keyword.word)
                .filter(move |range| !workflow || counts_as_the_workflow_word(text, range))
                .map(move |range| Found { keyword, range })
        })
        .collect();
    found.sort_by_key(|found| found.range.start);
    let mut end = 0;
    found.retain(|found| {
        let clear = found.range.start >= end;
        if clear {
            end = found.range.end;
        }
        clear
    });
    found
}

/// Claude Code skips the workflow word where it's quoted or in brackets, where it touches `/`,
/// `\` or `-` as in a path or a flag, and where `?` follows it, as in a question about the word.
fn counts_as_the_workflow_word(text: &str, range: &Range<usize>) -> bool {
    let before = text[..range.start].chars().next_back();
    let after = text[range.end..].chars().next();
    let joins = |c: Option<char>| c.is_some_and(|c| matches!(c, '/' | '\\' | '-'));
    if joins(before) || joins(after) || after == Some('?') {
        return false;
    }
    !quoted_or_bracketed(text, range)
}

/// Whether a quote or a bracket opened before `range` is still open there and closes after it.
/// An apostrophe inside a word, as in `don't`, quotes nothing.
fn quoted_or_bracketed(text: &str, range: &Range<usize>) -> bool {
    let (head, tail) = (&text[..range.start], &text[range.end..]);
    let quote = |mark: char| {
        let marks = |part: &str| {
            let chars: Vec<char> = part.chars().collect();
            (0..chars.len())
                .filter(|&at| chars[at] == mark)
                .filter(|&at| {
                    let inside_a_word = at > 0
                        && chars[at - 1].is_alphanumeric()
                        && chars.get(at + 1).is_some_and(|c| c.is_alphanumeric());
                    !inside_a_word
                })
                .count()
        };
        marks(head) % 2 == 1 && marks(tail) > 0
    };
    let bracket = |open: char, close: char| {
        let depth = head.chars().fold(0_i32, |depth, c| match c {
            c if c == open => depth + 1,
            c if c == close => (depth - 1).max(0),
            _ => depth,
        });
        depth > 0 && tail.contains(close)
    };
    ['"', '\'', '`'].into_iter().any(quote)
        || bracket('(', ')')
        || bracket('[', ']')
        || bracket('{', '}')
        || bracket('“', '”')
        || bracket('‘', '’')
}

/// A match starts on an ASCII byte, so it's always on a char boundary.
fn whole_words<'t>(text: &'t str, word: &'t str) -> impl Iterator<Item = Range<usize>> + 't {
    let bytes = text.as_bytes();
    let len = word.len();
    let mut at = 0;
    std::iter::from_fn(move || {
        if len == 0 || !word.is_ascii() {
            return None;
        }
        while at + len <= bytes.len() {
            let start = at;
            let end = at + len;
            let whole = bytes[start..end].eq_ignore_ascii_case(word.as_bytes())
                && !start.checked_sub(1).is_some_and(|i| is_word_byte(bytes[i]))
                && !bytes.get(end).is_some_and(|&b| is_word_byte(b));
            if whole {
                at = end;
                return Some(start..end);
            }
            at += 1;
        }
        None
    })
}

/// What JavaScript's `\b` counts as part of a word, without the `u` flag.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    const ULTRATHINK: Keyword = Keyword::new("ultrathink", KeywordLook::Rainbow, true);

    fn words(text: &str, keywords: &[Keyword]) -> Vec<String> {
        find(text, keywords)
            .into_iter()
            .map(|found| text[found.range].to_owned())
            .collect()
    }

    #[test]
    fn the_workflow_word_counts_only_whole() {
        assert!(asks_for_workflow("ultracode fix the flaky export test"));
        assert!(asks_for_workflow("fix it, Ultracode."));
        assert!(!asks_for_workflow("ultracoded"));
        assert!(!asks_for_workflow("my_ultracode"));
        assert!(!asks_for_workflow("fix the flaky export test"));
        assert!(!asks_for_workflow("/effort ultracode on"));
        // Where Claude Code skips it: a question about the word, a quote, a path, a flag,
        // brackets, and a word joined by `-`.
        for text in [
            "what is ultracode?",
            "explain \"ultracode\" to me",
            "rename src/ultracode.rs",
            "add a --ultracode flag",
            "the (ultracode) setting",
            "ultracode-mode is broken",
            "see `ultracode` in the docs",
            "the “ultracode” word",
        ] {
            assert!(!asks_for_workflow(text), "{text}");
        }
        assert!(asks_for_workflow("don't stop, ultracode it"));
        assert!(asks_for_workflow("(first) ultracode the export"));
    }

    #[test]
    fn finds_every_whole_word_in_any_case() {
        assert_eq!(
            words("ultrathink, then UltraThink. (ULTRATHINK)", &[ULTRATHINK]),
            ["ultrathink", "UltraThink", "ULTRATHINK"]
        );
    }

    #[test]
    fn letters_digits_and_underscores_join_a_word() {
        for text in [
            "ultrathinking",
            "reultrathink",
            "ultrathink_now",
            "ultrathink2",
        ] {
            assert!(words(text, &[ULTRATHINK]).is_empty(), "{text}");
        }
    }

    #[test]
    fn anything_else_ends_a_word_as_in_javascript() {
        // Without the `u` flag, `\b` only knows ASCII, so an accented letter ends the word too.
        for text in [
            "-ultrathink-",
            "/ultrathink",
            "éultrathinké",
            "「ultrathink」",
            "ultrathink?",
        ] {
            assert_eq!(words(text, &[ULTRATHINK]), ["ultrathink"], "{text}");
        }
    }

    #[test]
    fn offsets_are_bytes_after_wide_text() {
        let text = "日本語 ultrathink";
        let table = [ULTRATHINK];
        let found = find(text, &table);
        assert_eq!(found[0].range, 10..20);
        assert_eq!(found[0].keyword, &ULTRATHINK);
    }

    #[test]
    fn the_earlier_of_two_overlapping_words_wins() {
        let think = Keyword::new("think", KeywordLook::Rainbow, false);
        let think_hard = Keyword::new("think hard", KeywordLook::Rainbow, false);
        assert_eq!(
            words("think hard, think", &[think, think_hard]),
            ["think", "think"]
        );
    }

    #[test]
    fn an_empty_or_non_ascii_word_finds_nothing() {
        let empty = Keyword::new("", KeywordLook::Rainbow, false);
        let wide = Keyword::new("考える", KeywordLook::Rainbow, false);
        assert!(find("anything 考える", &[empty, wide]).is_empty());
    }

    #[test]
    fn a_table_crosses_the_wire_and_back() {
        let json = serde_json::to_string(&[ULTRATHINK]).unwrap();
        let back: Vec<Keyword> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, [ULTRATHINK]);
    }
}
