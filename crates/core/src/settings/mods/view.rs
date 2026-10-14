//! What `/mods` lists and a mod's page edits, as the app sends it to the screen, and the
//! changes a page sends back.

use serde::{Deserialize, Serialize};
use toml::Value;

use super::{ModFile, ModOrigin, ModReading, ModSource, span_words};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModsView {
    /// Every mod, the built-in cards first.
    pub rows: Vec<ModRow>,
    /// The files that didn't load, each with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<FileProblem>,
    /// The mod whose page opens at once, as `/mods checks` asks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
    /// `~/.argonauts/mods`, as the screen writes paths.
    pub yours_dir: String,
    /// `.argonauts/mods`, when there's a project to hold it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    /// What the bottom line's preview draws from.
    #[serde(default)]
    pub line: LineNumbers,
    /// Newer numbers and files for a view already open. It opens nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh: bool,
}

impl ModsView {
    pub fn row(&self, name: &str) -> Option<&ModRow> {
        self.rows.iter().find(|row| row.name == name)
    }
}

/// A file that wasn't loaded, by its path as shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileProblem {
    pub path: String,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModRow {
    pub name: String,
    pub origin: ModOrigin,
    /// Its file as shown, such as `~/.argonauts/mods/checks.toml`. For a built-in one, where a
    /// change to it would be saved.
    pub path: String,
    pub kind: ModRowKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModRowKind {
    /// A card the kit draws.
    Card {
        file: Box<ModFile>,
        /// The built-in card of this name, which a reset goes back to and each setting is
        /// compared with. `None` for a mod only you have.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        built_in: Option<Box<ModFile>>,
        /// Its numbers now. `None` before it has any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reading: Option<ModReading>,
        /// Why it has no numbers, or why the ones it has are old, in plain words.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// The project's, with a command you haven't trusted yet, so it hasn't run.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        needs_trust: bool,
        /// Its file names `bottom-line` among its places and the bottom line's file doesn't
        /// list it. `file.places` holds `bottom-line` only for a mod the line lists.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        asks_for_line: bool,
        /// It asks for the bottom line by the name of a part the line draws itself, so the
        /// line never shows this file.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hidden_by_line: bool,
    },
    /// A mod that draws itself, with a page of its own.
    Own {
        about: String,
        /// Its file's text, the built-in one when you have none.
        text: String,
        /// The built-in file's text.
        built_in: String,
    },
}

/// What the bottom line's preview draws from: the numbers the line itself shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LineNumbers {
    /// Such as `auto mode on`.
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The main model's name, at the right of the rule.
    pub model: String,
    /// One per CLI in the crew, the one in use first.
    pub clis: Vec<LineCli>,
    /// Today's spend, as it reads, such as `~$1.86`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LineCli {
    /// Such as `Claude`, `Codex`, `agy`.
    pub label: String,
    /// The adapter's id, which picks its color.
    pub id: String,
    /// The CLI you're talking to.
    pub main: bool,
    /// Used, from 0 to 1, where it's known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_days: Option<f64>,
}

/// What a page asks the app to do with a mod's file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum ModChange {
    /// Writes these keys, each with its value as TOML text, into the mod's file under
    /// `origin`: yours or the project's. A file that isn't there yet starts as a copy of the
    /// one in use.
    Save {
        name: String,
        pairs: Vec<(String, String)>,
        origin: ModOrigin,
    },
    /// Deletes your file, or the project's, so the one under it is in use again.
    Reset { name: String, origin: ModOrigin },
    /// Trusts the project's mod as its file is now, so its command may run.
    Trust { name: String },
}

impl ModFile {
    /// The keys whose values are different from `from`, each with its value as TOML text, as a page
    /// saves them.
    pub fn changes_from(&self, from: &ModFile) -> Vec<(String, String)> {
        let word = |text: &str| Value::String(text.to_owned()).to_string();
        let words = |list: Vec<String>| {
            Value::Array(list.into_iter().map(Value::String).collect()).to_string()
        };
        let mut pairs = Vec::new();
        let mut add = |changed: bool, key: &str, value: String| {
            if changed {
                pairs.push((key.to_owned(), value));
            }
        };
        add(self.title != from.title, "title", word(&self.title));
        add(self.body != from.body, "body", word(self.body.word()));
        add(
            self.places != from.places,
            "places",
            words(
                self.places
                    .iter()
                    .map(|place| place.word().to_owned())
                    .collect(),
            ),
        );
        add(self.shown != from.shown, "shown", self.shown.to_string());
        add(self.form != from.form, "form", word(self.form.word()));
        if let Some(key) = &self.key {
            add(self.key != from.key, "key", word(key));
        }
        match (&self.data, &from.data) {
            (ModSource::Builtin { source }, was) => add(
                !matches!(was, ModSource::Builtin { source: old } if old == source),
                "data.source",
                word(source.word()),
            ),
            (
                ModSource::Command {
                    command,
                    every,
                    timeout,
                },
                was,
            ) => {
                let (old_command, old_every, old_timeout) = match was {
                    ModSource::Command {
                        command,
                        every,
                        timeout,
                    } => (Some(command), Some(every), Some(timeout)),
                    ModSource::Builtin { .. } => (None, None, None),
                };
                add(
                    old_command != Some(command),
                    "data.command",
                    words(command.clone()),
                );
                add(
                    old_every != Some(every),
                    "data.every",
                    word(&span_words(*every)),
                );
                add(
                    old_timeout != Some(timeout),
                    "data.timeout",
                    word(&span_words(*timeout)),
                );
            }
        }
        let number = |value: f64| Value::Float(value).to_string();
        add(
            self.levels.warn != from.levels.warn,
            "levels.warn",
            number(self.levels.warn),
        );
        add(
            self.levels.danger != from.levels.danger,
            "levels.danger",
            number(self.levels.danger),
        );
        pairs
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::settings::mods::{BuiltinSource, ModBody, ModForm, ModPlace, builtin, check};

    fn context() -> ModFile {
        let builtin = builtin::builtin("context").unwrap();
        check::read(builtin.name, builtin.text).unwrap()
    }

    #[test]
    fn nothing_changed_writes_nothing() {
        assert!(context().changes_from(&context()).is_empty());
    }

    #[test]
    fn each_changed_key_is_written_as_toml() {
        let before = context();
        let mut after = before.clone();
        after.body = ModBody::Meters;
        after.form = ModForm::Line;
        after.places = vec![ModPlace::Above, ModPlace::Context];
        after.levels.warn = 60.0;
        assert_eq!(
            after.changes_from(&before),
            [
                ("body".to_owned(), "\"meters\"".to_owned()),
                ("places".to_owned(), "[\"above\", \"context\"]".to_owned()),
                ("form".to_owned(), "\"line\"".to_owned()),
                ("levels.warn".to_owned(), "60.0".to_owned()),
            ]
        );
    }

    #[test]
    fn a_new_command_writes_its_times_too() {
        let before = context();
        let mut after = before.clone();
        after.data = ModSource::Command {
            command: vec!["./ctx.sh".to_owned(), "--json".to_owned()],
            every: Duration::from_secs(300),
            timeout: Duration::from_secs(2),
        };
        assert_eq!(
            after.changes_from(&before),
            [
                (
                    "data.command".to_owned(),
                    "[\"./ctx.sh\", \"--json\"]".to_owned()
                ),
                ("data.every".to_owned(), "\"5m\"".to_owned()),
                ("data.timeout".to_owned(), "\"2s\"".to_owned()),
            ]
        );
        // And back to a built-in source.
        let mut back = after.clone();
        back.data = ModSource::Builtin {
            source: BuiltinSource::Spend,
        };
        assert_eq!(
            back.changes_from(&after),
            [("data.source".to_owned(), "\"spend\"".to_owned())]
        );
    }

    #[test]
    fn a_change_survives_the_wire() {
        let change = ModChange::Save {
            name: "checks".to_owned(),
            pairs: vec![("form".to_owned(), "\"line\"".to_owned())],
            origin: ModOrigin::Yours,
        };
        let text = serde_json::to_string(&change).unwrap();
        assert_eq!(serde_json::from_str::<ModChange>(&text).unwrap(), change);
    }
}
