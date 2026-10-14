//! The word check: every line and every file name, split into word parts and looked up in
//! the table.

use std::collections::HashMap;

use regex::Regex;

use super::report::Place;
use super::table::{self, Table};
use super::tree::File;

/// A line's word parts, lowercased. Text splits at anything that isn't a letter or a digit,
/// then where camelCase steps up, where a run of capitals ends in a word (`HTTPServer`), and
/// between letters and digits. A part is matched whole, so a retired word inside a longer
/// one never counts.
pub fn parts(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    split(text.as_bytes(), |start, end| {
        found.push(text[start..end].to_ascii_lowercase());
    });
    found
}

/// Calls `each` with where every part starts and ends.
fn split(bytes: &[u8], mut each: impl FnMut(usize, usize)) {
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_alphanumeric() {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < bytes.len() && same_part(bytes, start, end) {
            end += 1;
        }
        each(start, end);
        start = end;
    }
}

/// Whether the byte at `at` carries on the part that began at `start`.
fn same_part(bytes: &[u8], start: usize, at: usize) -> bool {
    let (before, here) = (bytes[at - 1], bytes[at]);
    if !here.is_ascii_alphanumeric() || before.is_ascii_digit() != here.is_ascii_digit() {
        return false;
    }
    if here.is_ascii_uppercase() {
        // A capital carries on only a run of capitals, and ends it when a word begins.
        let begins_a_word = bytes.get(at + 1).is_some_and(u8::is_ascii_lowercase);
        return before.is_ascii_uppercase() && !begins_a_word;
    }
    // A lowercase letter may follow one capital (`Word`) but not a run of them.
    !(before.is_ascii_uppercase() && at - 1 > start)
}

/// One spelling of a retired phrase.
struct PhraseForm<'t> {
    parts: Vec<String>,
    /// As the table spells it, which is how a report names it.
    spelled: &'t str,
    new: &'t str,
}

/// The table, indexed for the scan.
pub struct Lookup<'t> {
    table: &'t Table,
    /// Each retired word form to its entry in `table.word`.
    words: HashMap<&'t str, usize>,
    /// Each phrase's first word to the phrases that start with it.
    phrases: HashMap<String, Vec<PhraseForm<'t>>>,
    patterns: Vec<(Regex, &'t table::Pattern)>,
}

impl<'t> Lookup<'t> {
    pub fn new(table: &'t Table) -> Self {
        let mut words = HashMap::new();
        for (at, entry) in table.word.iter().enumerate() {
            for form in &entry.old {
                assert_eq!(parts(form), [form.as_str()], "{form:?} isn't one word part");
                assert!(
                    words.insert(form.as_str(), at).is_none(),
                    "{form:?} is listed twice"
                );
            }
        }
        let mut phrases: HashMap<String, Vec<PhraseForm<'t>>> = HashMap::new();
        for entry in &table.phrase {
            for form in &entry.old {
                let split = parts(form);
                assert!(
                    split.len() > 1,
                    "{form:?} is one word: list it under [[word]]"
                );
                phrases
                    .entry(split[0].clone())
                    .or_default()
                    .push(PhraseForm {
                        parts: split,
                        spelled: form.as_str(),
                        new: entry.new.as_str(),
                    });
            }
        }
        let patterns = table
            .pattern
            .iter()
            .map(|entry| {
                let regex = Regex::new(&entry.old).unwrap_or_else(|e| panic!("{}: {e}", entry.old));
                (regex, entry)
            })
            .collect();
        Self {
            table,
            words,
            phrases,
            patterns,
        }
    }

    /// Every retired name in one run of text, as (what was found, its replacement).
    pub fn in_text(&self, text: &str, path: &str) -> Vec<(String, &'t str)> {
        // Lowercased once and cut by position: this runs on every line of the workspace.
        let lower = text.to_ascii_lowercase();
        let mut cuts = Vec::new();
        split(text.as_bytes(), |start, end| cuts.push((start, end)));
        let part = |at: usize| cuts.get(at).map(|(start, end)| &lower[*start..*end]);

        let mut found = Vec::new();
        for at in 0..cuts.len() {
            let word = part(at).unwrap_or_default();
            if let Some(entry) = self.words.get(word).map(|at| &self.table.word[*at])
                && !table::fits_any(&entry.allowed_in, path)
            {
                found.push((word.to_owned(), entry.new.as_str()));
            }
            for phrase in self.phrases.get(word).into_iter().flatten() {
                // The words of a phrase sit side by side: joined by nothing (`TwoWords`), a
                // space, a dash or an underscore. `mode.line()` is two names, not a phrase.
                let joined = |next: usize| {
                    let gap = &text[cuts[at + next - 1].1..cuts[at + next].0];
                    gap.chars().all(|c| " -_".contains(c))
                };
                let whole = (0..phrase.parts.len()).all(|next| {
                    part(at + next) == Some(phrase.parts[next].as_str())
                        && (next == 0 || joined(next))
                });
                if whole {
                    found.push((phrase.spelled.to_owned(), phrase.new));
                }
            }
        }
        found
    }

    fn patterns_in(&self, text: &str, path: &str) -> Vec<(String, &'t str)> {
        self.patterns
            .iter()
            .filter(|(_, entry)| entry.only_in.is_empty() || table::fits_any(&entry.only_in, path))
            .flat_map(|(regex, entry)| {
                regex
                    .find_iter(text)
                    .map(|hit| (hit.as_str().to_owned(), entry.new.as_str()))
            })
            .collect()
    }
}

/// Which allow entries matched, so one that no longer does is reported as stale.
#[derive(Default)]
pub struct Used(pub Vec<bool>);

/// Every place a retired name is left in the given files: in a name, then in the lines of
/// each file whose words are read.
pub fn places(table: &Table, files: &[File], used: &mut Used) -> Vec<Place> {
    let lookup = Lookup::new(table);
    used.0 = vec![false; table.allow.len()];
    let mut places = Vec::new();
    for file in files {
        for (found, new) in lookup.in_text(&file.path, &file.path) {
            places.push(Place::new(&file.path, None, &found, new));
        }
        let Some(text) = file
            .text
            .as_deref()
            .filter(|_| table.reads_words_of(&file.path))
        else {
            continue;
        };
        let allowed: Vec<usize> = (0..table.allow.len())
            .filter(|at| table::fits(&table.allow[*at].path, &file.path))
            .collect();
        for (number, line) in text.lines().enumerate() {
            let line = without_allowed(line, &allowed, table, used);
            let hits = lookup
                .in_text(&line, &file.path)
                .into_iter()
                .chain(lookup.patterns_in(&line, &file.path));
            for (found, new) in hits {
                places.push(Place::new(&file.path, Some(number + 1), &found, new));
            }
        }
    }
    places
}

/// The line with each allowed text blanked, so only what the entry names is let through.
fn without_allowed<'l>(
    line: &'l str,
    allowed: &[usize],
    table: &Table,
    used: &mut Used,
) -> std::borrow::Cow<'l, str> {
    let mut line = std::borrow::Cow::Borrowed(line);
    for at in allowed {
        if let Some(without) = blanked(&line, &table.allow[*at].text) {
            used.0[*at] = true;
            line = without.into();
        }
    }
    line
}

/// The line with `text` blanked wherever it stands, or `None` when it isn't there.
pub fn blanked(line: &str, text: &str) -> Option<String> {
    line.contains(text)
        .then(|| line.replace(text, &" ".repeat(text.len())))
}
