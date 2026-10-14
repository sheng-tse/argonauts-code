//! Words wrap whole where they can, and a word wider than a row breaks between graphemes. A
//! space that doesn't fit hangs past the edge of its row unseen, so the next row starts with
//! the next word. A hard line whose last row is exactly full gets an empty row after it, so the
//! cursor at its end always has a cell to sit in.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;

use crate::text::{sanitize, str_width};

/// A byte range of the text, without its newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) start: usize,
    pub(super) end: usize,
}

/// `text` as the screen draws it. The draft keeps what you pasted, tabs and joined emoji
/// included, so it reaches the agent unchanged, and only drawing lays it out for the terminal.
pub(super) fn drawn(text: &str) -> Cow<'_, str> {
    sanitize(text)
}

/// Cells `text` takes once [`drawn`].
pub(super) fn cells(text: &str) -> usize {
    str_width(&drawn(text))
}

/// Always at least one row.
pub(super) fn rows(text: &str, width: usize) -> Vec<Row> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        wrap_line(text, line_start, line_start + line.len(), width, &mut rows);
        line_start += line.len() + 1;
    }
    rows
}

fn wrap_line(text: &str, start: usize, end: usize, width: usize, rows: &mut Vec<Row>) {
    let mut row_start = start;
    let mut col = 0;
    // Where a row may break: right after a space, with the columns used up to there.
    let mut break_at: Option<(usize, usize)> = None;
    for (offset, grapheme) in text[start..end].grapheme_indices(true) {
        let at = start + offset;
        let w = cells(grapheme);
        let space = grapheme.chars().all(char::is_whitespace);
        if space && col + w > width {
            rows.push(Row {
                start: row_start,
                end: at + grapheme.len(),
            });
            row_start = at + grapheme.len();
            col = 0;
            break_at = None;
            continue;
        }
        while col > 0 && col + w > width {
            match break_at.take() {
                Some((point, used)) if point > row_start => {
                    rows.push(Row {
                        start: row_start,
                        end: point,
                    });
                    row_start = point;
                    col -= used;
                }
                _ => {
                    rows.push(Row {
                        start: row_start,
                        end: at,
                    });
                    row_start = at;
                    col = 0;
                }
            }
        }
        col += w;
        if space {
            break_at = Some((at + grapheme.len(), col));
        }
    }
    rows.push(Row {
        start: row_start,
        end,
    });
    if col >= width {
        rows.push(Row { start: end, end });
    }
}

/// As (row, column), for the cursor at byte offset `cursor`.
pub(super) fn cursor_position(
    text: &str,
    rows: &[Row],
    cursor: usize,
    width: usize,
) -> (usize, usize) {
    let width = width.max(1);
    for (index, row) in rows.iter().enumerate() {
        let next_takes_it = rows.get(index + 1).is_some_and(|next| next.start == cursor);
        let inside =
            row.start <= cursor && (cursor < row.end || (cursor == row.end && !next_takes_it));
        if inside {
            let col = cells(&text[row.start..cursor]);
            // Only a hanging space puts the cursor past the edge. It shows where typing lands:
            // at the start of the next row.
            if col >= width && index + 1 < rows.len() {
                return (index + 1, 0);
            }
            return (index, col.min(width - 1));
        }
    }
    (rows.len().saturating_sub(1), 0)
}

/// The last cursor stop at or before `col` that shows on `row`. `is_stop` rules out places
/// inside pasted blocks.
pub(super) fn offset_at(
    text: &str,
    rows: &[Row],
    row: usize,
    col: usize,
    width: usize,
    is_stop: impl Fn(usize) -> bool,
) -> usize {
    let Some(target) = rows.get(row) else {
        return text.len();
    };
    let mut best = None;
    let candidates = text[target.start..target.end]
        .grapheme_indices(true)
        .map(|(offset, _)| target.start + offset)
        .chain(std::iter::once(target.end));
    for offset in candidates {
        if !is_stop(offset) {
            continue;
        }
        let (shown_row, shown_col) = cursor_position(text, rows, offset, width);
        if shown_row != row {
            continue;
        }
        if shown_col > col && best.is_some() {
            break;
        }
        best = Some(offset);
        if shown_col >= col {
            break;
        }
    }
    best.unwrap_or(target.start)
}

/// Cut to `width` columns, which drops a hanging space.
pub(super) fn visible(text: &str, row: Row, width: usize) -> &str {
    let slice = &text[row.start..row.end];
    let mut used = 0;
    for (offset, grapheme) in slice.grapheme_indices(true) {
        used += cells(grapheme);
        if used > width {
            return &slice[..offset];
        }
    }
    slice
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(text: &str, width: usize) -> Vec<&str> {
        rows(text, width)
            .into_iter()
            .map(|row| visible(text, row, width))
            .collect()
    }

    #[test]
    fn words_wrap_whole() {
        assert_eq!(shown("hello big world", 10), ["hello big ", "world"]);
        assert_eq!(shown("abcdefghijkl", 5), ["abcde", "fghij", "kl"]);
        assert_eq!(shown("one\n\ntwo", 10), ["one", "", "two"]);
        assert_eq!(shown("", 10), [""]);
    }

    #[test]
    fn a_space_that_does_not_fit_hangs() {
        let text = "abcde fgh";
        let rows = rows(text, 5);
        assert_eq!(shown(text, 5), ["abcde", "fgh"]);
        assert_eq!(cursor_position(text, &rows, 5, 5), (1, 0));
        assert_eq!(cursor_position(text, &rows, 6, 5), (1, 0));
        assert_eq!(cursor_position(text, &rows, 9, 5), (1, 3));
    }

    #[test]
    fn a_full_last_row_gets_a_row_for_the_cursor() {
        let text = "abcde";
        let rows = rows(text, 5);
        assert_eq!(rows.len(), 2);
        assert_eq!(cursor_position(text, &rows, 5, 5), (1, 0));
        assert_eq!(cursor_position(text, &rows, 4, 5), (0, 4));
    }

    #[test]
    fn the_cursor_prefers_the_start_of_a_wrapped_row() {
        let text = "hello world";
        let rows = rows(text, 8);
        assert_eq!(shown(text, 8), ["hello ", "world"]);
        assert_eq!(cursor_position(text, &rows, 6, 8), (1, 0));
        assert_eq!(cursor_position(text, &rows, 5, 8), (0, 5));
        assert_eq!(cursor_position(text, &rows, 11, 8), (1, 5));
    }

    #[test]
    fn wide_characters_wrap_before_the_edge() {
        let text = "ab你好";
        assert_eq!(shown(text, 5), ["ab你", "好"]);
        let rows = rows(text, 5);
        assert_eq!(cursor_position(text, &rows, text.len(), 5), (1, 2));
    }

    #[test]
    fn tabs_and_joined_emoji_take_the_cells_they_are_drawn_in() {
        let text = "a\tb";
        let rows = rows(text, 20);
        assert_eq!(cursor_position(text, &rows, text.len(), 20), (0, 6));
        assert_eq!(shown(text, 5), ["a\t", "b"]);
        let joined = "\u{1F468}\u{200D}\u{1F469}";
        let rows = super::rows(joined, 20);
        assert_eq!(cursor_position(joined, &rows, joined.len(), 20), (0, 4));
    }

    #[test]
    fn offsets_keep_the_column() {
        let text = "hello world\nhi";
        let rows = rows(text, 20);
        let any = |_| true;
        assert_eq!(offset_at(text, &rows, 1, 8, 20, any), text.len());
        assert_eq!(offset_at(text, &rows, 0, 1, 20, any), 1);
        assert_eq!(offset_at(text, &rows, 0, 50, 20, any), 11);

        let wrapped = "hello world";
        let wrapped_rows = super::rows(wrapped, 8);
        // The end of a wrapped row shows on the next row, so the last stop is before it.
        assert_eq!(offset_at(wrapped, &wrapped_rows, 0, 7, 8, any), 5);
    }
}
