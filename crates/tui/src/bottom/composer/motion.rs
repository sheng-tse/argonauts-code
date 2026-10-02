//! Vim's words and text objects over the draft. A word is a run of letters, digits, and `_`,
//! or a run of other visible characters; a WORD is any run without spaces. Positions never end
//! up inside a pasted block.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::edit::Draft;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Newline,
    Word,
    Punct,
}

fn class_of(grapheme: &str) -> Class {
    match grapheme.chars().next() {
        Some('\n') => Class::Newline,
        Some(c) if c.is_whitespace() => Class::Space,
        Some(c) if c.is_alphanumeric() || c == '_' => Class::Word,
        _ => Class::Punct,
    }
}

fn blank(class: Class) -> bool {
    matches!(class, Class::Space | Class::Newline)
}

/// Whether two graphemes belong to the same word, or with `big` the same WORD.
fn same(a: Class, b: Class, big: bool) -> bool {
    if big {
        !blank(a) && !blank(b)
    } else {
        a == b && !blank(a)
    }
}

/// Graphemes as (byte offset, class).
fn cells(text: &str) -> Vec<(usize, Class)> {
    text.grapheme_indices(true)
        .map(|(at, g)| (at, class_of(g)))
        .collect()
}

/// The index of the grapheme that starts at or after `pos`.
fn index_at(cells: &[(usize, Class)], pos: usize) -> usize {
    cells.partition_point(|(at, _)| *at < pos)
}

impl Draft {
    fn offset(&self, cells: &[(usize, Class)], index: usize) -> usize {
        cells.get(index).map_or(self.text.len(), |(at, _)| *at)
    }

    /// Vim's `w` and `W`: the start of the next word.
    pub(super) fn next_word_start(&self, pos: usize, big: bool) -> usize {
        let cells = cells(&self.text);
        let mut i = index_at(&cells, pos);
        if let Some(&(_, class)) = cells.get(i)
            && !blank(class)
        {
            while cells.get(i).is_some_and(|&(_, c)| same(c, class, big)) {
                i += 1;
            }
        }
        while cells.get(i).is_some_and(|&(_, c)| blank(c)) {
            i += 1;
        }
        self.out_of_element(self.offset(&cells, i), false)
    }

    /// Vim's `e` and `E`: the last character of this word, or of the next one when already
    /// there.
    pub(super) fn word_end(&self, pos: usize, big: bool) -> usize {
        let cells = cells(&self.text);
        if cells.is_empty() {
            return 0;
        }
        let mut i = index_at(&cells, pos) + 1;
        while cells.get(i).is_some_and(|&(_, c)| blank(c)) {
            i += 1;
        }
        if i >= cells.len() {
            return self.out_of_element(cells[cells.len() - 1].0, true);
        }
        let class = cells[i].1;
        while cells.get(i + 1).is_some_and(|&(_, c)| same(c, class, big)) {
            i += 1;
        }
        self.out_of_element(cells[i].0, true)
    }

    /// Vim's `b` and `B`: the start of this word, or of the one before when already there.
    pub(super) fn prev_word_start(&self, pos: usize, big: bool) -> usize {
        let cells = cells(&self.text);
        let mut i = index_at(&cells, pos);
        if i == 0 {
            return 0;
        }
        i -= 1;
        while i > 0 && blank(cells[i].1) {
            i -= 1;
        }
        let class = cells[i].1;
        if blank(class) {
            return 0;
        }
        while i > 0 && same(cells[i - 1].1, class, big) {
            i -= 1;
        }
        self.out_of_element(cells[i].0, true)
    }

    /// Vim's `^`: the line's first character that isn't a space.
    pub(super) fn first_non_blank(&self, pos: usize) -> usize {
        let start = self.line_start(pos);
        let line = &self.text[start..self.line_end(pos)];
        let indent = line.len() - line.trim_start().len();
        self.out_of_element(start + indent, true)
    }

    /// The range of a text object such as `iw`, `a"`, or `i(` around `pos`: `around` takes the
    /// quotes or brackets, and a word's or quote's spaces.
    pub(super) fn text_object(&self, pos: usize, around: bool, kind: char) -> Option<Range<usize>> {
        let range = match kind {
            'w' => self.word_object(pos, around, false),
            'W' => self.word_object(pos, around, true),
            '"' | '\'' | '`' => self.quote_object(pos, around, kind),
            '(' | ')' | 'b' => self.bracket_object(pos, around, b'(', b')'),
            '[' | ']' => self.bracket_object(pos, around, b'[', b']'),
            '{' | '}' | 'B' => self.bracket_object(pos, around, b'{', b'}'),
            '<' | '>' => self.bracket_object(pos, around, b'<', b'>'),
            _ => None,
        }?;
        Some(self.widen(range))
    }

    fn word_object(&self, pos: usize, around: bool, big: bool) -> Option<Range<usize>> {
        let cells = cells(&self.text);
        if cells.is_empty() {
            return None;
        }
        let at = index_at(&cells, pos).min(cells.len() - 1);
        let class = cells[at].1;
        if class == Class::Newline {
            return None;
        }
        let joins = |c: Class| {
            if blank(class) {
                c == Class::Space
            } else {
                same(c, class, big)
            }
        };
        let mut start = at;
        while start > 0 && joins(cells[start - 1].1) {
            start -= 1;
        }
        let mut end = at + 1;
        while cells.get(end).is_some_and(|&(_, c)| joins(c)) {
            end += 1;
        }
        if around {
            if blank(class) {
                // Spaces, then the word after them.
                if let Some(&(_, next)) = cells.get(end)
                    && !blank(next)
                {
                    while cells.get(end).is_some_and(|&(_, c)| same(c, next, big)) {
                        end += 1;
                    }
                }
            } else if cells.get(end).is_some_and(|&(_, c)| c == Class::Space) {
                while cells.get(end).is_some_and(|&(_, c)| c == Class::Space) {
                    end += 1;
                }
            } else {
                while start > 0 && cells[start - 1].1 == Class::Space {
                    start -= 1;
                }
            }
        }
        Some(self.offset(&cells, start)..self.offset(&cells, end))
    }

    /// Quotes pair up from the start of the line, skipping escaped ones, as vim reads them.
    fn quote_object(&self, pos: usize, around: bool, quote: char) -> Option<Range<usize>> {
        let start = self.line_start(pos);
        let line = &self.text[start..self.line_end(pos)];
        let mut quotes = Vec::new();
        let mut escaped = false;
        for (at, c) in line.char_indices() {
            if c == quote && !escaped {
                quotes.push(start + at);
            }
            escaped = c == '\\' && !escaped;
        }
        let (open, close) = quotes
            .chunks_exact(2)
            .map(|pair| (pair[0], pair[1]))
            .find(|&(open, close)| open <= pos && pos <= close)
            .or_else(|| {
                quotes
                    .chunks_exact(2)
                    .map(|pair| (pair[0], pair[1]))
                    .find(|&(open, _)| open > pos)
            })?;
        let width = quote.len_utf8();
        if !around {
            return Some(open + width..close);
        }
        let mut end = close + width;
        let after = &self.text[end..self.line_end(end)];
        let trailing = after.len() - after.trim_start_matches(' ').len();
        let mut begin = open;
        if trailing > 0 {
            end += trailing;
        } else {
            let before = &self.text[start..open];
            begin -= before.len() - before.trim_end_matches(' ').len();
        }
        Some(begin..end)
    }

    /// Brackets nest and may span lines. Bracket bytes are ASCII, so they never sit inside a
    /// longer character.
    fn bracket_object(
        &self,
        pos: usize,
        around: bool,
        open: u8,
        close: u8,
    ) -> Option<Range<usize>> {
        let bytes = self.text.as_bytes();
        let pos = pos.min(bytes.len());
        let open_at = if bytes.get(pos) == Some(&open) {
            pos
        } else {
            let mut depth = 0usize;
            let mut found = None;
            for i in (0..pos).rev() {
                if bytes[i] == close {
                    depth += 1;
                } else if bytes[i] == open {
                    if depth == 0 {
                        found = Some(i);
                        break;
                    }
                    depth -= 1;
                }
            }
            found?
        };
        let mut depth = 0usize;
        let mut close_at = None;
        for (i, &byte) in bytes.iter().enumerate().skip(open_at + 1) {
            if byte == open {
                depth += 1;
            } else if byte == close {
                if depth == 0 {
                    close_at = Some(i);
                    break;
                }
                depth -= 1;
            }
        }
        let close_at = close_at?;
        Some(if around {
            open_at..close_at + 1
        } else {
            open_at + 1..close_at
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(text: &str, pos: usize, around: bool, kind: char) -> Option<&str> {
        let draft = Draft::from_text(text);
        let range = draft.text_object(pos, around, kind)?;
        Some(&text[range])
    }

    #[test]
    fn words_move_like_vim() {
        let draft = Draft::from_text("fix(the) big_test  now");
        assert_eq!(draft.next_word_start(0, false), 3);
        assert_eq!(draft.next_word_start(3, false), 4);
        assert_eq!(draft.next_word_start(0, true), 9);
        assert_eq!(draft.word_end(0, false), 2);
        assert_eq!(draft.word_end(2, false), 3);
        assert_eq!(draft.word_end(9, false), 16);
        assert_eq!(draft.prev_word_start(19, false), 9);
        assert_eq!(draft.prev_word_start(9, false), 7);
        assert_eq!(draft.prev_word_start(9, true), 0);
        assert_eq!(draft.next_word_start(19, false), draft.text.len());
    }

    #[test]
    fn text_objects_find_words_quotes_and_brackets() {
        let text = r#"say "hi there" to f(a, (b)) now"#;
        assert_eq!(object(text, 1, false, 'w'), Some("say"));
        assert_eq!(object(text, 1, true, 'w'), Some("say "));
        assert_eq!(object(text, 7, false, '"'), Some("hi there"));
        assert_eq!(object(text, 7, true, '"'), Some("\"hi there\" "));
        assert_eq!(object(text, 0, false, '"'), Some("hi there"));
        assert_eq!(object(text, 21, false, '('), Some("a, (b)"));
        assert_eq!(object(text, 25, false, ')'), Some("b"));
        assert_eq!(object(text, 21, true, 'b'), Some("(a, (b))"));
        assert_eq!(object(text, 0, false, '('), None);
        assert_eq!(object("end word", 8, true, 'w'), Some(" word"));
    }
}
