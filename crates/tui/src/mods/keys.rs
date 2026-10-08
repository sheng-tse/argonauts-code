//! A mod's own key: two keys in a row, such as `ctrl+x k`, that hide the mod or show it again.
//! The mods come and go with their files, so their chords are read from the mods the screen
//! was last told of and not from the keymap's fixed list.

use argonauts_core::settings::mods::ModsView;
use argonauts_core::settings::mods::view::ModRowKind;
use crossterm::event::KeyEvent;

use crate::keymap::KeySpec;

/// A mod's `key` as its two keys. `None` for one that isn't two keys the keymap reads.
fn chord_of(key: &str) -> Option<(KeySpec, KeySpec)> {
    let mut words = key.split_whitespace();
    let first = KeySpec::parse(words.next()?).ok()?;
    let second = KeySpec::parse(words.next()?).ok()?;
    words.next().is_none().then_some((first, second))
}

/// Every mod's chord, as the screen last heard of the mods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chords(Vec<(String, KeySpec, KeySpec)>);

impl Chords {
    pub fn of(view: &ModsView) -> Self {
        let chords = view
            .rows
            .iter()
            .filter_map(|row| match &row.kind {
                ModRowKind::Card { file, .. } => {
                    let (first, second) = chord_of(file.key.as_deref()?)?;
                    Some((row.name.clone(), first, second))
                }
                ModRowKind::Own { .. } => None,
            })
            .collect();
        Self(chords)
    }

    /// Whether `key` is the first key of some mod's chord.
    pub fn starts(&self, key: &KeyEvent) -> bool {
        let pressed = KeySpec::from_event(key);
        self.0.iter().any(|(_, first, _)| *first == pressed)
    }

    /// The mod whose chord is `first`, as the keymap writes a key, then `key`.
    pub fn find(&self, first: &str, key: &KeyEvent) -> Option<&str> {
        let pressed = KeySpec::from_event(key);
        self.0
            .iter()
            .find(|(_, start, end)| start.to_string() == first && *end == pressed)
            .map(|(name, _, _)| name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::mods::view::ModRow;
    use argonauts_core::settings::mods::{ModOrigin, builtin, check};
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;

    fn chords(key: &str) -> Chords {
        let (name, text) = builtin::STARTERS
            .iter()
            .find(|(name, _)| *name == "checks")
            .unwrap();
        let mut file = check::read(name, text).unwrap();
        file.key = Some(key.to_owned());
        Chords::of(&ModsView {
            rows: vec![ModRow {
                name: "checks".to_owned(),
                origin: ModOrigin::Yours,
                path: String::new(),
                kind: ModRowKind::Card {
                    file: Box::new(file),
                    built_in: None,
                    reading: None,
                    note: None,
                    needs_trust: false,
                    asks_for_line: false,
                    hidden_by_line: false,
                },
            }],
            ..ModsView::default()
        })
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn plain(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn the_starter_s_chord_finds_its_mod() {
        let chords = chords("ctrl+x k");
        assert!(chords.starts(&ctrl('x')));
        assert!(!chords.starts(&plain('x')));
        let first = KeySpec::from_event(&ctrl('x')).to_string();
        assert_eq!(chords.find(&first, &plain('k')), Some("checks"));
        assert_eq!(chords.find(&first, &plain('j')), None);
        assert_eq!(chords.find("ctrl+g", &plain('k')), None);
        assert_eq!(Chords::default().find(&first, &plain('k')), None);
    }

    #[test]
    fn a_key_that_isnt_two_keys_binds_nothing() {
        for key in ["k", "ctrl+x k j", "hyper+x k", ""] {
            let chords = chords(key);
            assert!(!chords.starts(&ctrl('x')), "{key}");
            assert!(!chords.starts(&plain('k')), "{key}");
        }
    }
}
