//! Reads a mod file and says what's wrong with it in plain words, every problem at once, so
//! one look at Doctor is enough to fix it. A file with a problem isn't loaded at all: half a
//! mod would draw something its author never wrote.

use std::collections::BTreeMap;
use std::time::Duration;

use toml::{Table, Value};

use super::super::Span;
use super::{
    BuiltinSource, DEFAULT_EVERY, DEFAULT_TIMEOUT, LEAST_EVERY, MOST_TIMEOUT, ModBody, ModColor,
    ModFile, ModForm, ModLevels, ModPlace, ModSource, builtin, is_mod_name, span_words,
};
use crate::input::SLASH_COMMANDS;

const KEYS: [&str; 10] = [
    "name", "title", "body", "places", "shown", "form", "key", "data", "levels", "colors",
];
const DATA_KEYS: [&str; 4] = ["source", "command", "every", "timeout"];
const LEVEL_KEYS: [&str; 2] = ["warn", "danger"];

/// What's wrong with `text` as the file of the mod `name`, every problem at once. Empty when
/// it loads. The bottom line and stats keep settings of their own in their files, so those
/// are read as that and not as a card.
pub fn problems(name: &str, text: &str) -> Vec<String> {
    if name == builtin::BOTTOM_LINE {
        return super::bottom_line::read(text).err().into_iter().collect();
    }
    if name == builtin::STATS {
        return crate::settings::stats::StatsSettings::check(text)
            .err()
            .into_iter()
            .collect();
    }
    if builtin::draws_itself(name) {
        return match text.parse::<Table>() {
            Ok(_) => Vec::new(),
            Err(err) => vec![format!("it isn't valid TOML: {}", err.message())],
        };
    }
    read(name, text).err().unwrap_or_default()
}

/// The mod `text` describes, in the file `<stem>.toml`. `Err` lists every problem.
pub fn read(stem: &str, text: &str) -> Result<ModFile, Vec<String>> {
    let table: Table = text
        .parse()
        .map_err(|err: toml::de::Error| vec![format!("it isn't valid TOML: {}", err.message())])?;
    let mut problems = Vec::new();
    let mut reader = Reader {
        table: &table,
        problems: &mut problems,
    };
    reader.unknown_keys("", &KEYS);
    let name = reader.name(stem);
    let title = reader.text("title").unwrap_or_else(|| name.clone());
    let body = reader.body();
    let places = reader.places();
    let shown = reader.flag("shown").unwrap_or(true);
    let form = reader.word("form", "full, compact or line", ModForm::from_word);
    let key = reader.chord();
    let data = reader.data();
    let levels = reader.levels();
    let colors = reader.colors();
    match (body, data) {
        (Some(body), Some(data)) if problems.is_empty() => Ok(ModFile {
            name,
            title,
            body,
            places,
            shown,
            form: form.unwrap_or_default(),
            key,
            data,
            levels,
            colors,
        }),
        _ => Err(problems),
    }
}

struct Reader<'a> {
    table: &'a Table,
    problems: &'a mut Vec<String>,
}

impl Reader<'_> {
    fn problem(&mut self, text: impl Into<String>) {
        self.problems.push(text.into());
    }

    fn unknown_keys(&mut self, section: &str, known: &[&str]) {
        let table = match section {
            "" => Some(self.table),
            section => self.table.get(section).and_then(Value::as_table),
        };
        let unknown: Vec<String> = table
            .into_iter()
            .flat_map(|table| table.keys())
            .filter(|key| !known.contains(&key.as_str()))
            .cloned()
            .collect();
        for key in unknown {
            let place = if section.is_empty() {
                "a mod".to_owned()
            } else {
                format!("[{section}]")
            };
            self.problem(format!(
                "{key} isn't a key {place} has; the keys are {}",
                known.join(", ")
            ));
        }
    }

    /// A key that must be text when it's there.
    fn text(&mut self, key: &str) -> Option<String> {
        match self.table.get(key)? {
            Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
            Value::String(_) => {
                self.problem(format!("{key} is empty"));
                None
            }
            _ => {
                self.problem(format!("{key} must be text in quotes"));
                None
            }
        }
    }

    /// The mod's own key: two keys in a row, so it can't take a key you type with.
    fn chord(&mut self) -> Option<String> {
        let key = self.text("key")?;
        if key.split_whitespace().count() != 2 {
            self.problem(format!(
                "key is {key:?}; use two keys in a row, such as \"ctrl+x k\""
            ));
            return None;
        }
        Some(key)
    }

    /// A key that must be `true` or `false` when it's there.
    fn flag(&mut self, key: &str) -> Option<bool> {
        let value = self.table.get(key)?;
        let flag = value.as_bool();
        if flag.is_none() {
            self.problem(format!("{key} is {}; use true or false", shown(value)));
        }
        flag
    }

    /// A key that must be one of a few words.
    fn word<T>(&mut self, key: &str, words: &str, from: fn(&str) -> Option<T>) -> Option<T> {
        let text = self.text(key)?;
        let found = from(&text);
        if found.is_none() {
            self.problem(format!("{key} is {text:?}; use {words}"));
        }
        found
    }

    /// The name, which the file's own name gives when the file leaves it out.
    fn name(&mut self, stem: &str) -> String {
        let name = self.text("name").unwrap_or_else(|| stem.to_owned());
        if !is_mod_name(&name) {
            self.problem(format!(
                "the name {name:?} won't do; use lowercase letters, digits, - and _, starting \
                 with a letter"
            ));
        } else if name != stem {
            self.problem(format!(
                "its name is {name}, but the file is {stem}.toml; they must match"
            ));
        } else if takes_a_command(&name) {
            self.problem(format!(
                "/{name} is already a command, so a mod can't take that name"
            ));
        }
        name
    }

    fn body(&mut self) -> Option<ModBody> {
        if !self.table.contains_key("body") {
            self.problem(format!("it needs a body: {}", body_words()));
            return None;
        }
        self.word("body", &body_words(), ModBody::from_word)
    }

    fn places(&mut self) -> Vec<ModPlace> {
        let Some(value) = self.table.get("places") else {
            return Vec::new();
        };
        let Some(list) = value.as_array() else {
            self.problem("places must be a list, such as [\"above\"]");
            return Vec::new();
        };
        let mut places = Vec::new();
        for item in list {
            match item.as_str().map(|word| (word, ModPlace::from_word(word))) {
                Some((_, Some(place))) if !places.contains(&place) => places.push(place),
                Some((_, Some(_))) => {}
                Some((word, None)) => {
                    self.problem(format!("{word} isn't a place; use {}", place_words()));
                }
                None => self.problem(format!("a place must be one of {}", place_words())),
            }
        }
        places
    }

    fn data(&mut self) -> Option<ModSource> {
        let Some(value) = self.table.get("data") else {
            self.problem("it needs [data] with a source or a command, so it has numbers to draw");
            return None;
        };
        let Some(data) = value.as_table() else {
            self.problem("data must be a table: [data]");
            return None;
        };
        self.unknown_keys("data", &DATA_KEYS);
        let every = self.span(data, "every", DEFAULT_EVERY);
        let timeout = self.span(data, "timeout", DEFAULT_TIMEOUT);
        match (data.get("source"), data.get("command")) {
            (Some(_), Some(_)) => {
                self.problem("[data] has both a source and a command; keep one");
                None
            }
            (None, None) => {
                self.problem(format!(
                    "[data] needs a source ({}) or a command",
                    source_words()
                ));
                None
            }
            (Some(source), None) => {
                let found = source.as_str().and_then(BuiltinSource::from_word);
                if found.is_none() {
                    self.problem(format!(
                        "source is {}; use {}",
                        shown(source),
                        source_words()
                    ));
                }
                found.map(|source| ModSource::Builtin { source })
            }
            (None, Some(command)) => {
                let words = command_words(command);
                if words.is_none() {
                    self.problem(
                        "command must be a list of words, such as [\"./checks.sh\"] or \
                         [\"git\", \"branch\", \"--show-current\"]",
                    );
                }
                if every.is_some_and(|every| every < LEAST_EVERY) {
                    self.problem(format!(
                        "every is too often; the least is {}",
                        span_words(LEAST_EVERY)
                    ));
                }
                if timeout.is_some_and(|timeout| timeout > MOST_TIMEOUT || timeout.is_zero()) {
                    self.problem(format!(
                        "timeout must be between 1ms and {}",
                        span_words(MOST_TIMEOUT)
                    ));
                }
                Some(ModSource::Command {
                    command: words?,
                    every: every?,
                    timeout: timeout?,
                })
            }
        }
    }

    /// A duration key of `[data]`, or its default.
    fn span(&mut self, data: &Table, key: &str, default: Duration) -> Option<Duration> {
        let Some(value) = data.get(key) else {
            return Some(default);
        };
        let parsed = value
            .as_str()
            .ok_or_else(|| format!("{} needs quotes and a unit, such as \"60s\"", shown(value)))
            .and_then(|text| Span::new(text).parse());
        match parsed {
            Ok(Some(duration)) => Some(duration),
            Ok(None) => {
                self.problem(format!(
                    "{key} can't be \"none\"; give a time, such as \"60s\""
                ));
                None
            }
            Err(why) => {
                self.problem(format!("{key}: {why}"));
                None
            }
        }
    }

    fn levels(&mut self) -> ModLevels {
        let mut levels = ModLevels::default();
        let Some(value) = self.table.get("levels") else {
            return levels;
        };
        let Some(table) = value.as_table() else {
            self.problem("levels must be a table: [levels]");
            return levels;
        };
        self.unknown_keys("levels", &LEVEL_KEYS);
        let percent = |key: &str, slot: &mut f64, problems: &mut Vec<String>| {
            let Some(value) = table.get(key) else { return };
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|n| n as f64));
            match number {
                Some(number) if (0.0..=100.0).contains(&number) => *slot = number,
                _ => problems.push(format!(
                    "levels.{key} is {}; use a percent from 0 to 100",
                    shown(value)
                )),
            }
        };
        let before = self.problems.len();
        percent("warn", &mut levels.warn, self.problems);
        percent("danger", &mut levels.danger, self.problems);
        // A line that didn't read has said so already.
        if self.problems.len() == before && levels.warn > levels.danger {
            self.problem("levels.warn is above levels.danger");
        }
        levels
    }

    fn colors(&mut self) -> BTreeMap<String, ModColor> {
        let mut colors = BTreeMap::new();
        let Some(value) = self.table.get("colors") else {
            return colors;
        };
        let Some(table) = value.as_table() else {
            self.problem("colors must be a table: [colors]");
            return colors;
        };
        for (label, value) in table {
            match value.as_str().and_then(ModColor::from_word) {
                Some(color) => {
                    colors.insert(label.clone(), color);
                }
                None => self.problem(format!(
                    "colors.{label} is {}; use {}",
                    shown(value),
                    color_words()
                )),
            }
        }
        colors
    }
}

/// A command as a list of words, the program first.
fn command_words(value: &Value) -> Option<Vec<String>> {
    let words: Option<Vec<String>> = value
        .as_array()?
        .iter()
        .map(|word| word.as_str().map(str::to_owned))
        .collect();
    words.filter(|words| {
        words
            .first()
            .is_some_and(|program| !program.trim().is_empty())
    })
}

/// Whether `/name` is a command that isn't a mod's own, such as `/model`.
fn takes_a_command(name: &str) -> bool {
    SLASH_COMMANDS.iter().any(|spec| spec.name == name) && !super::builtin::is_builtin(name)
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => format!("{text:?}"),
        other => other.to_string(),
    }
}

fn joined(words: impl Iterator<Item = &'static str>) -> String {
    let words: Vec<&str> = words.collect();
    match words.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => words.join(""),
    }
}

pub fn body_words() -> String {
    joined(ModBody::ALL.into_iter().map(ModBody::word))
}

pub fn place_words() -> String {
    joined(ModPlace::ALL.into_iter().map(ModPlace::word))
}

pub fn source_words() -> String {
    joined(BuiltinSource::ALL.into_iter().map(BuiltinSource::word))
}

pub fn color_words() -> String {
    joined(ModColor::ALL.into_iter().map(ModColor::word))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKS: &str = r#"
# A mod is one card. /mods lists it beside the built-in ones.
name   = "checks"
title  = "checks"
body   = "stack"
places = ["above"]
form   = "full"
key    = "ctrl+x k"

[data]
command = ["./checks.sh"]
every   = "60s"
timeout = "2s"
"#;

    #[test]
    fn the_starter_file_reads_as_written() {
        let file = read("checks", CHECKS).unwrap();
        assert_eq!(file.name, "checks");
        assert_eq!(file.title, "checks");
        assert_eq!(file.body, ModBody::Stack);
        assert_eq!(file.places, [ModPlace::Above]);
        assert_eq!(file.form, ModForm::Full);
        assert_eq!(file.key.as_deref(), Some("ctrl+x k"));
        assert_eq!(
            file.data,
            ModSource::Command {
                command: vec!["./checks.sh".to_owned()],
                every: Duration::from_secs(60),
                timeout: Duration::from_secs(2),
            }
        );
        assert_eq!(file.levels, ModLevels::default());
    }

    #[test]
    fn what_a_file_leaves_out_takes_its_default() {
        let file = read(
            "ctx",
            "body = \"meters\"\n[data]\nsource = \"plan_usage\"\n",
        )
        .unwrap();
        assert_eq!(file.name, "ctx");
        assert_eq!(file.title, "ctx");
        assert!(file.places.is_empty());
        assert!(file.shown);
        assert_eq!(file.form, ModForm::Full);
        assert_eq!(file.key, None);
        assert_eq!(
            file.data,
            ModSource::Builtin {
                source: BuiltinSource::PlanUsage
            }
        );
    }

    fn problems(stem: &str, text: &str) -> Vec<String> {
        read(stem, text).unwrap_err()
    }

    #[test]
    fn every_problem_is_named_at_once_in_plain_words() {
        let text = r#"
name = "Checks"
titel = "x"
body = "pie"
places = ["above", "sidebar"]
shown = "no"
form = "huge"
[data]
source = "weather"
[levels]
warn = 95
danger = 120
[colors]
passed = "teal"
"#;
        let found = problems("checks", text);
        let has = |needle: &str| found.iter().any(|problem| problem.contains(needle));
        assert!(has("titel isn't a key a mod has"), "{found:?}");
        assert!(has("shown is \"no\"; use true or false"), "{found:?}");
        assert!(has("the name \"Checks\" won't do"), "{found:?}");
        assert!(has("body is \"pie\"; use stack, meters"), "{found:?}");
        assert!(
            has("sidebar isn't a place; use above, mode-line"),
            "{found:?}"
        );
        assert!(
            has("form is \"huge\"; use full, compact or line"),
            "{found:?}"
        );
        assert!(
            has("source is \"weather\"; use context, plan_usage"),
            "{found:?}"
        );
        assert!(
            has("levels.danger is 120; use a percent from 0 to 100"),
            "{found:?}"
        );
        assert!(
            has("colors.passed is \"teal\"; use green, amber"),
            "{found:?}"
        );
        assert_eq!(found.len(), 9, "{found:?}");
    }

    #[test]
    fn a_mod_s_key_is_two_keys_in_a_row() {
        let with =
            |key: &str| format!("body = \"text\"\nkey = \"{key}\"\n[data]\nsource = \"spend\"\n");
        assert_eq!(
            read("x", &with("ctrl+x k")).unwrap().key.as_deref(),
            Some("ctrl+x k")
        );
        assert_eq!(
            problems("x", &with("k")),
            ["key is \"k\"; use two keys in a row, such as \"ctrl+x k\""]
        );
    }

    #[test]
    fn a_hidden_mod_keeps_its_places_and_shows_nowhere() {
        let text =
            "body = \"text\"\nplaces = [\"above\"]\nshown = false\n[data]\nsource = \"spend\"\n";
        let file = read("x", text).unwrap();
        assert_eq!(file.places, [ModPlace::Above]);
        assert!(!file.shows(ModPlace::Above));
        assert!(!file.shows_somewhere());
    }

    #[test]
    fn a_file_needs_a_body_and_its_data() {
        assert_eq!(
            problems("x", "title = \"x\"\n"),
            [
                format!("it needs a body: {}", body_words()),
                "it needs [data] with a source or a command, so it has numbers to draw".to_owned()
            ]
        );
        assert_eq!(
            problems("x", "body = \"text\"\n[data]\n"),
            [format!(
                "[data] needs a source ({}) or a command",
                source_words()
            )]
        );
        assert_eq!(
            problems(
                "x",
                "body = \"text\"\n[data]\nsource = \"spend\"\ncommand = [\"ls\"]\n"
            ),
            ["[data] has both a source and a command; keep one"]
        );
    }

    #[test]
    fn a_name_must_match_its_file_and_leave_commands_alone() {
        let body = "body = \"text\"\n[data]\nsource = \"spend\"\n";
        assert_eq!(
            problems("b", &format!("name = \"a\"\n{body}")),
            ["its name is a, but the file is b.toml; they must match"]
        );
        assert_eq!(
            problems("model", body),
            ["/model is already a command, so a mod can't take that name"]
        );
        // A built-in mod's own name is its file's to take.
        assert!(read("context", body).is_ok());
    }

    #[test]
    fn a_command_keeps_within_its_times() {
        let with = |extra: &str| format!("body = \"text\"\n[data]\ncommand = [\"date\"]\n{extra}");
        assert_eq!(
            problems("x", &with("every = \"100ms\"\n")),
            ["every is too often; the least is 1s"]
        );
        assert_eq!(
            problems("x", &with("timeout = \"5m\"\n")),
            ["timeout must be between 1ms and 30s"]
        );
        assert_eq!(
            problems("x", &with("every = 60\n")),
            ["every: 60 needs quotes and a unit, such as \"60s\""]
        );
        assert_eq!(
            problems("x", "body = \"text\"\n[data]\ncommand = \"date\"\n").len(),
            1
        );
        assert!(read("x", &with("every = \"5m\"\ntimeout = \"10s\"\n")).is_ok());
    }

    #[test]
    fn bad_toml_is_one_problem() {
        let found = problems("x", "body = \n");
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("it isn't valid TOML:"), "{found:?}");
    }
}
