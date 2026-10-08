//! Mod files. A mod is one drawn card, written as a TOML file in `~/.argonauts/mods/` (yours)
//! or `.argonauts/mods/` (the project's). It names a body from the kit, the places it shows,
//! its form, and where its numbers come from: a built-in source, or a command of yours that
//! prints JSON. The built-in cards are mod files too, so one loader, one checker, and one
//! drawer serve them all.
//!
//! | Part | Where |
//! |---|---|
//! | what a file may say | this file |
//! | reading a file, and what's wrong with it in plain words | [`check`] |
//! | the numbers a mod draws | [`reading`] |
//! | the built-in cards | [`builtin`] |
//! | the folders, and which file wins | [`load`] |
//! | the bottom line's own file | [`bottom_line`] |
//! | saving a whole file someone wrote for you | [`save`] |
//! | what `/mods` and a mod's page show | [`view`] |

pub mod bottom_line;
pub mod builtin;
pub mod check;
pub mod load;
pub mod reading;
pub mod save;
pub mod view;

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ui::Place;

pub use load::{LoadedMod, ModOrigin, ModProblem, ModSet};
pub use reading::{ModColor, ModLevel, ModPart, ModReading};
pub use view::{ModRow, ModsView};

/// The folder mod files live in, under `~/.argonauts/` and under a project's `.argonauts/`.
pub const MODS_DIR: &str = "mods";

/// How often a command runs when its file doesn't say.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(60);
/// How long a command may take when its file doesn't say.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
/// A command never runs more often than this, however small `every` is.
pub const LEAST_EVERY: Duration = Duration::from_secs(1);
/// A command is stopped after this, however large `timeout` is.
pub const MOST_TIMEOUT: Duration = Duration::from_secs(30);

/// `[mods]` in the settings: what mod files may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct ModsSettings {
    /// Whether a mod's command may run. Off, command mods draw nothing and say why; mods with
    /// a built-in source are untouched. A safety key.
    pub commands: bool,
}

impl Default for ModsSettings {
    fn default() -> Self {
        Self { commands: true }
    }
}

/// One mod, as its file says it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModFile {
    /// Letters, digits, `-` and `_`. Also the file's name, and the mod's command.
    pub name: String,
    /// The bold word in its header. The name when the file gives none.
    pub title: String,
    pub body: ModBody,
    /// Where it shows, in the order written. Empty shows it nowhere but `/mods`.
    pub places: Vec<ModPlace>,
    /// Off hides it everywhere and keeps its places, which is what its own command flips.
    pub shown: bool,
    pub form: ModForm,
    /// The chord that shows or hides it, such as `ctrl+x k`.
    pub key: Option<String>,
    pub data: ModSource,
    /// Where a meter turns amber and red, in percent.
    pub levels: ModLevels,
    /// A color for a part, by its label, over the one its data gives.
    pub colors: BTreeMap<String, ModColor>,
}

impl ModFile {
    /// Whether it shows in `place`.
    pub fn shows(&self, place: ModPlace) -> bool {
        self.shown && self.places.contains(&place)
    }

    /// Whether it shows anywhere.
    pub fn shows_somewhere(&self) -> bool {
        self.shown && !self.places.is_empty()
    }
}

/// The body under a mod's header, each one drawn by the kit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModBody {
    /// A stacked bar and its legend.
    Stack,
    /// One meter a row.
    Meters,
    /// Rows of a mark, a name, and a number.
    List,
    /// A small column chart.
    Columns,
    /// One row of columns, as a trend.
    Sparkline,
    /// A square per item, colored by how it went.
    Marks,
    /// One line of words.
    Text,
}

impl ModBody {
    pub const ALL: [ModBody; 7] = [
        ModBody::Stack,
        ModBody::Meters,
        ModBody::List,
        ModBody::Columns,
        ModBody::Sparkline,
        ModBody::Marks,
        ModBody::Text,
    ];

    pub fn word(self) -> &'static str {
        match self {
            ModBody::Stack => "stack",
            ModBody::Meters => "meters",
            ModBody::List => "list",
            ModBody::Columns => "columns",
            ModBody::Sparkline => "sparkline",
            ModBody::Marks => "marks",
            ModBody::Text => "text",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|body| body.word() == word)
    }

    /// What it draws, for `/mods` and the README.
    pub fn describe(self) -> &'static str {
        match self {
            ModBody::Stack => "a stacked bar with its legend",
            ModBody::Meters => "one meter a row",
            ModBody::List => "rows of a mark, a name and a number",
            ModBody::Columns => "a column chart",
            ModBody::Sparkline => "one row of columns, as a trend",
            ModBody::Marks => "a square per item",
            ModBody::Text => "one line of words",
        }
    }
}

/// Where a mod shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModPlace {
    /// Above the input.
    Above,
    /// On the line under the input, as a few words.
    ModeLine,
    Overview,
    Status,
    Usage,
    Context,
    Limits,
    Stats,
}

impl ModPlace {
    pub const ALL: [ModPlace; 8] = [
        ModPlace::Above,
        ModPlace::ModeLine,
        ModPlace::Overview,
        ModPlace::Status,
        ModPlace::Usage,
        ModPlace::Context,
        ModPlace::Limits,
        ModPlace::Stats,
    ];

    pub fn word(self) -> &'static str {
        match self {
            ModPlace::Above => "above",
            ModPlace::ModeLine => "mode-line",
            ModPlace::Overview => "overview",
            ModPlace::Status => "status",
            ModPlace::Usage => "usage",
            ModPlace::Context => "context",
            ModPlace::Limits => "limits",
            ModPlace::Stats => "stats",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|place| place.word() == word)
    }

    /// The place list in your settings that records which built-in cards show here. `None`
    /// where no list does: the bottom line keeps its own file, and the rest is a mod's own
    /// `places`.
    pub fn list(self) -> Option<Place> {
        match self {
            ModPlace::Above => Some(Place::AboveInput),
            ModPlace::Status => Some(Place::Status),
            ModPlace::Usage => Some(Place::Usage),
            ModPlace::Context => Some(Place::Context),
            ModPlace::Limits => Some(Place::Budget),
            ModPlace::ModeLine | ModPlace::Overview | ModPlace::Stats => None,
        }
    }

    /// As a sentence says it: "above the input", "on the Usage tab".
    pub fn phrase(self) -> &'static str {
        match self {
            ModPlace::Above => "above the input",
            ModPlace::ModeLine => "on the bottom line",
            ModPlace::Overview => "on the Overview tab",
            ModPlace::Status => "on the Status tab",
            ModPlace::Usage => "on the Usage tab",
            ModPlace::Context => "on the Context tab",
            ModPlace::Limits => "on the Limits tab",
            ModPlace::Stats => "on the Stats tab",
        }
    }
}

/// How much of a mod shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModForm {
    /// The frame, the header, the body and its legend.
    #[default]
    Full,
    /// The frame, the header and the bar.
    Compact,
    /// One row, no frame.
    Line,
}

impl ModForm {
    pub const ALL: [ModForm; 3] = [ModForm::Full, ModForm::Compact, ModForm::Line];

    pub fn word(self) -> &'static str {
        match self {
            ModForm::Full => "full",
            ModForm::Compact => "compact",
            ModForm::Line => "line",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|form| form.word() == word)
    }
}

/// Where a mod's numbers come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum ModSource {
    /// Numbers Argonauts already keeps.
    Builtin { source: BuiltinSource },
    /// A program of yours that prints the JSON [`ModReading`] reads.
    Command {
        /// The program and its arguments. A path starting with `./` is beside the mod file.
        command: Vec<String>,
        every: Duration,
        timeout: Duration,
    },
}

impl ModSource {
    /// `plan_usage`, or the command as you'd type it.
    pub fn words(&self) -> String {
        match self {
            ModSource::Builtin { source } => source.word().to_owned(),
            ModSource::Command { command, .. } => command.join(" "),
        }
    }

    pub fn is_command(&self) -> bool {
        matches!(self, ModSource::Command { .. })
    }
}

/// The numbers Argonauts keeps that a mod can draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinSource {
    /// The main session's context: used and free.
    Context,
    /// Each CLI's plan windows.
    PlanUsage,
    /// Today's spend, by CLI.
    Spend,
    /// Today's tokens: from cache, new input, and output.
    Tokens,
    /// This repository's workflows and how each ended.
    Runs,
    /// What waits for you.
    Waiting,
}

impl BuiltinSource {
    pub const ALL: [BuiltinSource; 6] = [
        BuiltinSource::Context,
        BuiltinSource::PlanUsage,
        BuiltinSource::Spend,
        BuiltinSource::Tokens,
        BuiltinSource::Runs,
        BuiltinSource::Waiting,
    ];

    pub fn word(self) -> &'static str {
        match self {
            BuiltinSource::Context => "context",
            BuiltinSource::PlanUsage => "plan_usage",
            BuiltinSource::Spend => "spend",
            BuiltinSource::Tokens => "tokens",
            BuiltinSource::Runs => "runs",
            BuiltinSource::Waiting => "waiting",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.word() == word)
    }

    pub fn describe(self) -> &'static str {
        match self {
            BuiltinSource::Context => "the main session's context, used and free",
            BuiltinSource::PlanUsage => "each CLI's plan windows",
            BuiltinSource::Spend => "today's spend, by CLI",
            BuiltinSource::Tokens => "today's tokens: from cache, new input and output",
            BuiltinSource::Runs => "this repository's workflows and how each ended",
            BuiltinSource::Waiting => "what waits for you",
        }
    }
}

/// Where a meter turns amber and red, in percent of what it's measured against.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModLevels {
    pub warn: f64,
    pub danger: f64,
}

impl Default for ModLevels {
    fn default() -> Self {
        Self {
            warn: 70.0,
            danger: 90.0,
        }
    }
}

impl ModLevels {
    pub fn level(&self, percent: f64) -> ModLevel {
        if percent >= self.danger {
            ModLevel::Danger
        } else if percent >= self.warn {
            ModLevel::Warn
        } else {
            ModLevel::Ok
        }
    }
}

/// Whether `name` can name a mod: letters, digits, `-` and `_`, starting with a letter.
pub fn is_mod_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && name.len() <= 32
}

/// `60s`, `5m`, `2h`: a duration as a mod file writes it.
pub fn span_words(duration: Duration) -> String {
    let secs = duration.as_secs();
    if duration.subsec_millis() > 0 {
        format!("{}ms", duration.as_millis())
    } else if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

impl fmt::Display for ModBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

impl fmt::Display for ModPlace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

impl fmt::Display for ModForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mod_name_is_lowercase_words_joined_by_dashes() {
        for good in ["checks", "plan-usage", "ci_2", "a"] {
            assert!(is_mod_name(good), "{good}");
        }
        for bad in ["", "Checks", "2fast", "my mod", "../x", "a.b", "-x"] {
            assert!(!is_mod_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_duration_reads_back_as_it_was_written() {
        for (secs, words) in [
            (60, "1m"),
            (2, "2s"),
            (90, "90s"),
            (7200, "2h"),
            (300, "5m"),
        ] {
            assert_eq!(span_words(Duration::from_secs(secs)), words);
        }
        assert_eq!(span_words(Duration::from_millis(500)), "500ms");
    }

    #[test]
    fn levels_turn_at_their_lines() {
        let levels = ModLevels::default();
        assert_eq!(levels.level(10.0), ModLevel::Ok);
        assert_eq!(levels.level(70.0), ModLevel::Warn);
        assert_eq!(levels.level(95.0), ModLevel::Danger);
    }
}
