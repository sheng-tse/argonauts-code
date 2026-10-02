//! The atomic paste elements follow Codex's `textarea.rs` (Apache-2.0): a long paste shows as
//! a short label, the cursor can't stop inside it, deleting takes it whole, and submitting
//! swaps the label for the pasted text.

use std::ops::Range;

use unicode_segmentation::GraphemeCursor;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Draft {
    pub(super) text: String,
    /// Byte offset, always on a grapheme boundary and never inside an element.
    pub(super) cursor: usize,
    /// Sorted and never overlapping.
    pub(super) elements: Vec<Element>,
}

/// A pasted block: its label's place in the text, and what it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Element {
    pub(super) range: Range<usize>,
    pub(super) payload: String,
}

/// Text cut or copied out of a draft, with the pasted blocks in it, so putting it back keeps
/// them whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Clip {
    pub(super) text: String,
    /// Ranges into `text`.
    pub(super) elements: Vec<Element>,
}

impl Clip {
    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whole lines without the newline that joined them to the rest: the last one, or else
    /// the first. A label never holds a newline, so no element is cut.
    pub(super) fn without_line_break(mut self) -> Self {
        if self.text.ends_with('\n') {
            self.text.pop();
        } else if self.text.starts_with('\n') {
            self.text.remove(0);
            for element in &mut self.elements {
                element.range.start -= 1;
                element.range.end -= 1;
            }
        }
        self
    }
}

impl Draft {
    pub(super) fn from_text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            cursor: text.len(),
            elements: Vec::new(),
        }
    }

    pub(super) fn insert(&mut self, text: &str) {
        let at = self.cursor;
        self.text.insert_str(at, text);
        for element in &mut self.elements {
            if element.range.start >= at {
                element.range.start += text.len();
                element.range.end += text.len();
            }
        }
        self.cursor = at + text.len();
    }

    pub(super) fn insert_element(&mut self, label: &str, payload: String) {
        let start = self.cursor;
        self.insert(label);
        let element = Element {
            range: start..start + label.len(),
            payload,
        };
        let index = self
            .elements
            .partition_point(|e| e.range.start < element.range.start);
        self.elements.insert(index, element);
    }

    /// Puts `clip` in at the cursor with its pasted blocks still whole.
    pub(super) fn insert_clip(&mut self, clip: &Clip) {
        let start = self.cursor;
        self.insert(&clip.text);
        let index = self.elements.partition_point(|e| e.range.start < start);
        let pasted = clip.elements.iter().map(|element| Element {
            range: element.range.start + start..element.range.end + start,
            payload: element.payload.clone(),
        });
        self.elements.splice(index..index, pasted);
    }

    /// `range` grown to whole elements, with the pasted blocks in it.
    pub(super) fn copy(&self, range: Range<usize>) -> Clip {
        let range = self.widen(range);
        let elements = self
            .elements
            .iter()
            .filter(|e| e.range.start >= range.start && e.range.end <= range.end)
            .map(|element| Element {
                range: element.range.start - range.start..element.range.end - range.start,
                payload: element.payload.clone(),
            })
            .collect();
        Clip {
            text: self.text[range].to_owned(),
            elements,
        }
    }

    /// Grows `range` to whole elements and returns what it removed.
    pub(super) fn delete(&mut self, range: Range<usize>) -> Clip {
        let range = self.widen(range);
        if range.is_empty() {
            return Clip::default();
        }
        let removed = self.copy(range.clone());
        self.text.drain(range.clone());
        let len = range.len();
        self.elements
            .retain(|e| e.range.end <= range.start || e.range.start >= range.end);
        for element in &mut self.elements {
            if element.range.start >= range.end {
                element.range.start -= len;
                element.range.end -= len;
            }
        }
        self.cursor = if self.cursor >= range.end {
            self.cursor - len
        } else {
            self.cursor.min(range.start)
        };
        removed
    }

    pub(super) fn expanded(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        let mut at = 0;
        for element in &self.elements {
            out.push_str(&self.text[at..element.range.start]);
            out.push_str(&element.payload);
            at = element.range.end;
        }
        out.push_str(&self.text[at..]);
        out
    }

    pub(super) fn is_stop(&self, pos: usize) -> bool {
        self.text.is_char_boundary(pos) && self.element_around(pos).is_none()
    }

    pub(super) fn prev_stop(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        if let Some(element) = self
            .elements
            .iter()
            .find(|e| pos > e.range.start && pos <= e.range.end)
        {
            return element.range.start;
        }
        let mut cursor = GraphemeCursor::new(pos, self.text.len(), true);
        let back = cursor
            .prev_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(0);
        self.out_of_element(back, true)
    }

    pub(super) fn next_stop(&self, pos: usize) -> usize {
        if pos >= self.text.len() {
            return self.text.len();
        }
        if let Some(element) = self
            .elements
            .iter()
            .find(|e| pos >= e.range.start && pos < e.range.end)
        {
            return element.range.end;
        }
        let mut cursor = GraphemeCursor::new(pos, self.text.len(), true);
        let on = cursor
            .next_boundary(&self.text, 0)
            .ok()
            .flatten()
            .unwrap_or(self.text.len());
        self.out_of_element(on, false)
    }

    pub(super) fn word_start_before(&self, pos: usize) -> usize {
        let before = &self.text[..pos];
        let trimmed = before.trim_end();
        let start = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        self.out_of_element(start, true)
    }

    pub(super) fn word_end_after(&self, pos: usize) -> usize {
        let after = &self.text[pos..];
        let skipped = after.len() - after.trim_start().len();
        let rest = &after[skipped..];
        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        self.out_of_element(pos + skipped + word, false)
    }

    pub(super) fn line_start(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    pub(super) fn line_end(&self, pos: usize) -> usize {
        self.text[pos..]
            .find('\n')
            .map_or(self.text.len(), |i| pos + i)
    }

    /// Strictly inside, so an element's edges are still stops.
    fn element_around(&self, pos: usize) -> Option<&Element> {
        self.elements
            .iter()
            .find(|e| pos > e.range.start && pos < e.range.end)
    }

    pub(super) fn out_of_element(&self, pos: usize, to_start: bool) -> usize {
        match self.element_around(pos) {
            Some(element) if to_start => element.range.start,
            Some(element) => element.range.end,
            None => pos,
        }
    }

    pub(super) fn widen(&self, range: Range<usize>) -> Range<usize> {
        let mut start = range.start.min(self.text.len());
        let mut end = range.end.min(self.text.len()).max(start);
        for element in &self.elements {
            if element.range.start < end && element.range.end > start {
                start = start.min(element.range.start);
                end = end.max(element.range.end);
            }
        }
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_with_paste() -> Draft {
        let mut draft = Draft::from_text("ab ");
        draft.insert_element("[paste]", "one\ntwo".into());
        draft.insert(" cd");
        draft
    }

    #[test]
    fn inserting_before_an_element_moves_it() {
        let mut draft = draft_with_paste();
        draft.cursor = 0;
        draft.insert("xy");
        assert_eq!(draft.text, "xyab [paste] cd");
        assert_eq!(draft.elements[0].range, 5..12);
        assert_eq!(draft.expanded(), "xyab one\ntwo cd");
    }

    #[test]
    fn the_cursor_steps_over_elements_whole() {
        let draft = draft_with_paste();
        assert_eq!(draft.prev_stop(13), 12);
        assert_eq!(draft.prev_stop(11), 10);
        assert_eq!(draft.prev_stop(10), 3);
        assert_eq!(draft.next_stop(3), 10);
        assert!(!draft.is_stop(5));
        assert!(draft.is_stop(3));
    }

    #[test]
    fn deleting_into_an_element_takes_it_whole() {
        let mut draft = draft_with_paste();
        draft.cursor = 10;
        let removed = draft.delete(9..10);
        assert_eq!(removed.text, "[paste]");
        assert_eq!(removed.elements[0].range, 0..7);
        assert_eq!(draft.text, "ab  cd");
        assert!(draft.elements.is_empty());
        assert_eq!(draft.cursor, 3);
    }

    #[test]
    fn a_clip_puts_its_pasted_blocks_back_whole() {
        let mut draft = draft_with_paste();
        let clip = draft.copy(1..10);
        assert_eq!(clip.text, "b [paste]");
        draft.cursor = draft.text.len();
        draft.insert_clip(&clip);
        assert_eq!(draft.text, "ab [paste] cdb [paste]");
        assert_eq!(draft.elements.len(), 2);
        assert_eq!(draft.elements[1].range, 15..22);
        assert_eq!(draft.expanded(), "ab one\ntwo cdb one\ntwo");
        assert!(!draft.is_stop(18));
    }

    #[test]
    fn graphemes_move_whole() {
        let mut draft = Draft::from_text("e\u{301}🇯🇵x");
        assert_eq!(draft.prev_stop(draft.text.len()), draft.text.len() - 1);
        draft.cursor = draft.text.len() - 1;
        assert_eq!(draft.prev_stop(draft.cursor), 3);
        assert_eq!(draft.prev_stop(3), 0);
        assert_eq!(draft.next_stop(0), 3);
    }

    #[test]
    fn words_skip_spaces_then_letters() {
        let draft = Draft::from_text("  hello   big world");
        assert_eq!(draft.word_start_before(draft.text.len()), 14);
        assert_eq!(draft.word_start_before(14), 10);
        assert_eq!(draft.word_start_before(10), 2);
        assert_eq!(draft.word_end_after(0), 7);
        assert_eq!(draft.word_end_after(7), 13);
    }

    #[test]
    fn lines_have_starts_and_ends() {
        let draft = Draft::from_text("one\ntwo\nthree");
        assert_eq!(draft.line_start(6), 4);
        assert_eq!(draft.line_end(5), 7);
        assert_eq!(draft.line_start(2), 0);
        assert_eq!(draft.line_end(9), 13);
    }
}
