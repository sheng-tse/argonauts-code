//! Words a vendor CLI treats specially in a prompt, such as Claude Code's `ultrathink`. The input
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
}

/// One keyword in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    pub keyword: &'a Keyword,
    /// Byte offsets into the text.
    pub range: Range<usize>,
}

/// Every keyword in `text`, in order. Where two would overlap, the one that starts first wins,
/// or at the same start the one first in the table.
pub fn find<'a>(text: &str, keywords: &'a [Keyword]) -> Vec<Found<'a>> {
    let mut found: Vec<Found<'a>> = keywords
        .iter()
        .flat_map(|keyword| {
            whole_words(text, &keyword.word).map(move |range| Found { keyword, range })
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
