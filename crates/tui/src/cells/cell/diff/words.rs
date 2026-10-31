//! A live agent block redraws every frame with all its diffs, so results are kept for the last
//! few hunks: the word diff runs once per hunk, not once per frame.

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use similar::{ChangeTag, TextDiff};

use super::{DiffKind, DiffLine};
use crate::base::text::sanitize_line;

/// Below this share of common words a pair gets no highlights, since marking every word says
/// nothing.
const MIN_SIMILARITY: f32 = 0.5;
/// Rows longer than this get no word highlights, which keeps a pathological line cheap.
const MAX_LINE_BYTES: usize = 2_000;
/// In hunks.
const CACHE_SIZE: usize = 64;

/// Changed byte ranges into each line's sanitized text, indexed like the hunk's lines.
pub(super) type HunkWords = Arc<Vec<Vec<Range<usize>>>>;

pub(super) fn changed_words(lines: &[DiffLine]) -> HunkWords {
    let key = hunk_key(lines);
    if let Some(hit) = cached(key) {
        return hit;
    }
    let words = Arc::new(compute(lines));
    remember(key, words.clone());
    words
}

fn compute(lines: &[DiffLine]) -> Vec<Vec<Range<usize>>> {
    let mut out = vec![Vec::new(); lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != DiffKind::Removed {
            i += 1;
            continue;
        }
        let removed_start = i;
        while i < lines.len() && lines[i].kind == DiffKind::Removed {
            i += 1;
        }
        let added_start = i;
        while i < lines.len() && lines[i].kind == DiffKind::Added {
            i += 1;
        }
        let pairs = (added_start - removed_start).min(i - added_start);
        for k in 0..pairs {
            let (old, new) = (removed_start + k, added_start + k);
            let old_text = sanitize_line(&lines[old].text);
            let new_text = sanitize_line(&lines[new].text);
            if let Some((old_words, new_words)) = word_diff(&old_text, &new_text) {
                out[old] = old_words;
                out[new] = new_words;
            }
        }
    }
    out
}

/// Changed byte ranges in the old line and in the new one.
type WordChanges = (Vec<Range<usize>>, Vec<Range<usize>>);

/// `None` when the lines share too little for highlights to help. Similarity counts words only,
/// since shared spaces say nothing.
fn word_diff(old: &str, new: &str) -> Option<WordChanges> {
    if old.len() > MAX_LINE_BYTES || new.len() > MAX_LINE_BYTES {
        return None;
    }
    let diff = TextDiff::from_words(old, new);
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let (mut old_at, mut new_at) = (0, 0);
    let (mut same, mut total) = (0usize, 0usize);
    for change in diff.iter_all_changes() {
        let value = change.value();
        let len = value.len();
        let word = !value.trim().is_empty();
        match change.tag() {
            ChangeTag::Equal => {
                old_at += len;
                new_at += len;
                if word {
                    same += 2;
                    total += 2;
                }
            }
            ChangeTag::Delete => {
                push_range(&mut removed, old_at..old_at + len, old);
                old_at += len;
                total += usize::from(word);
            }
            ChangeTag::Insert => {
                push_range(&mut added, new_at..new_at + len, new);
                new_at += len;
                total += usize::from(word);
            }
        }
    }
    let similarity = if total == 0 {
        1.0
    } else {
        same as f32 / total as f32
    };
    (similarity >= MIN_SIMILARITY).then_some((removed, added))
}

fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>, text: &str) {
    if text[range.clone()].trim().is_empty() {
        return;
    }
    if let Some(last) = ranges.last_mut()
        && text[last.end..range.start].trim().is_empty()
    {
        last.end = range.end;
        return;
    }
    ranges.push(range);
}

fn hunk_key(lines: &[DiffLine]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for line in lines {
        line.kind.hash(&mut hasher);
        line.text.hash(&mut hasher);
    }
    hasher.finish()
}

fn cache() -> &'static Mutex<VecDeque<(u64, HunkWords)>> {
    static CACHE: OnceLock<Mutex<VecDeque<(u64, HunkWords)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(CACHE_SIZE)))
}

fn cached(key: u64) -> Option<HunkWords> {
    let mut entries = cache().lock().ok()?;
    let index = entries.iter().position(|(k, _)| *k == key)?;
    // Move the hit to the back, so the least recently used entry goes first.
    let entry = entries.remove(index)?;
    let hit = entry.1.clone();
    entries.push_back(entry);
    Some(hit)
}

fn remember(key: u64, words: HunkWords) {
    let Ok(mut entries) = cache().lock() else {
        return;
    };
    if entries.len() >= CACHE_SIZE {
        entries.pop_front();
    }
    entries.push_back((key, words));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: DiffKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_line: None,
            new_line: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn paired_rows_mark_only_the_words_that_changed() {
        let lines = [
            line(DiffKind::Removed, "    /// Writes the rows as CSV."),
            line(DiffKind::Added, "    /// Writes the rows as CSV or JSON."),
        ];
        let words = changed_words(&lines);
        let at = |i: usize| -> Vec<&str> {
            words[i]
                .iter()
                .map(|range| &lines[i].text[range.clone()])
                .collect()
        };
        assert_eq!(at(0), ["CSV."]);
        assert_eq!(at(1), ["CSV or JSON."]);
        assert_eq!(changed_words(&lines), words);
    }

    #[test]
    fn rows_that_share_little_or_run_long_get_no_marks() {
        let lines = [
            line(DiffKind::Removed, "| Parser | In progress |"),
            line(DiffKind::Added, "| Export, CLI, Tests | Starting now |"),
        ];
        assert!(changed_words(&lines).iter().all(Vec::is_empty));
        let long = "word ".repeat(MAX_LINE_BYTES);
        let lines = [
            line(DiffKind::Removed, &long),
            line(DiffKind::Added, &format!("{long}more")),
        ];
        assert!(changed_words(&lines).iter().all(Vec::is_empty));
    }
}
