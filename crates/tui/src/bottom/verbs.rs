//! The voyage set fits Argonauts Code; the plain set says what happens without the theme. The
//! words live in `data/spinner-words.txt`.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum VerbStyle {
    #[default]
    Voyage,
    Plain,
}

const WORDS_FILE: &str = include_str!("../../data/spinner-words.txt");

struct Lists {
    voyage_working: Vec<&'static str>,
    voyage_done: Vec<&'static str>,
    plain_working: Vec<&'static str>,
    plain_done: Vec<&'static str>,
}

static LISTS: LazyLock<Lists> = LazyLock::new(|| {
    let mut lists = Lists {
        voyage_working: Vec::new(),
        voyage_done: Vec::new(),
        plain_working: Vec::new(),
        plain_done: Vec::new(),
    };
    let mut current: Option<&mut Vec<&'static str>> = None;
    for line in WORDS_FILE.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(heading) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = match heading {
                "voyage working" => Some(&mut lists.voyage_working),
                "voyage done" => Some(&mut lists.voyage_done),
                "plain working" => Some(&mut lists.plain_working),
                "plain done" => Some(&mut lists.plain_done),
                _ => None,
            };
            continue;
        }
        if let Some(list) = current.as_mut() {
            list.push(line);
        }
    }
    lists
});

/// The gerunds for the working line, such as `Rowing`.
pub fn working_words(style: VerbStyle) -> &'static [&'static str] {
    match style {
        VerbStyle::Voyage => &LISTS.voyage_working,
        VerbStyle::Plain => &LISTS.plain_working,
    }
}

/// The past-tense verbs for the turn end, as in `Sailed for 11m 3s`.
pub fn done_words(style: VerbStyle) -> &'static [&'static str] {
    match style {
        VerbStyle::Voyage => &LISTS.voyage_done,
        VerbStyle::Plain => &LISTS.plain_done,
    }
}

/// Hands out a fresh word for each working stretch and each turn end, never the one it gave
/// last time. The default walks each list in order so tests and snapshots stay put; the app
/// starts with [`VerbPicker::random`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerbPicker {
    rng: Option<u64>,
    last_working: Option<&'static str>,
    last_done: Option<&'static str>,
}

impl VerbPicker {
    pub fn random() -> Self {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(u64::from(std::process::id()));
        Self::seeded(hasher.finish())
    }

    pub fn seeded(seed: u64) -> Self {
        Self {
            rng: Some(seed),
            ..Self::default()
        }
    }

    pub fn working(&mut self, style: VerbStyle) -> &'static str {
        let mut last = self.last_working;
        let word = self.pick(working_words(style), &mut last);
        self.last_working = last;
        word
    }

    pub fn done(&mut self, style: VerbStyle) -> &'static str {
        let mut last = self.last_done;
        let word = self.pick(done_words(style), &mut last);
        self.last_done = last;
        word
    }

    fn pick(
        &mut self,
        words: &'static [&'static str],
        last: &mut Option<&'static str>,
    ) -> &'static str {
        let after = last.and_then(|word| words.iter().position(|w| *w == word));
        let index = match (self.rng.as_mut(), after) {
            _ if words.len() < 2 => 0,
            (None, after) => after.map_or(0, |i| (i + 1) % words.len()),
            // Drawing from one fewer and stepping over the last word keeps the odds even.
            (Some(state), Some(after)) => {
                let draw = next(state) as usize % (words.len() - 1);
                if draw >= after { draw + 1 } else { draw }
            }
            (Some(state), None) => next(state) as usize % words.len(),
        };
        let word = words.get(index).copied().unwrap_or("Working");
        *last = Some(word);
        word
    }
}

/// SplitMix64: small, fast, and plenty for picking a word.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn the_lists_are_the_sizes_the_design_asks_for() {
        let working = working_words(VerbStyle::Voyage);
        let done = done_words(VerbStyle::Voyage);
        assert!((180..=200).contains(&working.len()), "{}", working.len());
        assert!((8..=12).contains(&done.len()), "{}", done.len());
        assert!(!working_words(VerbStyle::Plain).is_empty());
        assert_eq!(done_words(VerbStyle::Plain), ["Worked"]);
    }

    #[test]
    fn every_word_is_one_short_unique_word() {
        for style in [VerbStyle::Voyage, VerbStyle::Plain] {
            for words in [working_words(style), done_words(style)] {
                let unique: HashSet<_> = words.iter().map(|w| w.to_lowercase()).collect();
                assert_eq!(unique.len(), words.len(), "a word is listed twice");
                for word in words {
                    assert!(!word.contains(char::is_whitespace), "{word:?}");
                    assert!(word.width() <= 18, "{word:?} crowds the working line");
                    assert!(word.starts_with(char::is_uppercase), "{word:?}");
                }
            }
            for word in working_words(style) {
                assert!(word.ends_with("ing"), "{word:?}");
            }
            for word in done_words(style) {
                assert!(word.ends_with("ed"), "{word:?} reads oddly before `for`");
            }
        }
    }

    #[test]
    fn a_random_picker_never_gives_the_same_word_twice_in_a_row() {
        let mut picker = VerbPicker::seeded(7);
        let mut seen = HashSet::new();
        let mut last = picker.working(VerbStyle::Voyage);
        for _ in 0..2_000 {
            let word = picker.working(VerbStyle::Voyage);
            assert_ne!(word, last);
            seen.insert(word);
            last = word;
        }
        assert!(seen.len() > 150, "only {} words came up", seen.len());

        let mut last = picker.done(VerbStyle::Voyage);
        for _ in 0..200 {
            let word = picker.done(VerbStyle::Voyage);
            assert_ne!(word, last);
            last = word;
        }
    }

    #[test]
    fn random_pickers_start_on_different_words() {
        let firsts: HashSet<_> = (0..40)
            .map(|_| VerbPicker::random().working(VerbStyle::Voyage))
            .collect();
        assert!(firsts.len() > 1);
    }

    #[test]
    fn the_default_picker_walks_the_lists_in_order() {
        let mut picker = VerbPicker::default();
        let words = working_words(VerbStyle::Voyage);
        assert_eq!(picker.working(VerbStyle::Voyage), words[0]);
        assert_eq!(picker.working(VerbStyle::Voyage), words[1]);
        assert_eq!(picker.done(VerbStyle::Voyage), "Sailed");
        assert_eq!(picker.done(VerbStyle::Voyage), "Charted");
        assert_eq!(picker.done(VerbStyle::Plain), "Worked");
        assert_eq!(picker.done(VerbStyle::Plain), "Worked");
    }
}
