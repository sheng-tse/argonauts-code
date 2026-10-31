//! The stats mod's own keys, from `mods/stats.toml`: first `~/.argonauts/mods/`, then the
//! project's `.argonauts/mods/`, whose keys win one by one.
//!
//! ```toml
//! name    = "stats"
//! places  = ["stats-tab", "above"]   # the tab, and the card above the input
//! period  = "all"                   # today · week · month · all
//! group   = "cli"                   # cli · model · role
//! measure = "tokens"                # what the bars measure: tokens · tasks · spend
//! columns = ["work", "review", "tokens", "plan"]   # also: spend, pace, lines, fallbacks
//! total   = true                    # the "all together" bar and line
//! ```

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::runs::api::UsagePeriod;

/// The file's name in a `mods` folder.
pub const STATS_FILE: &str = "stats.toml";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatsSettings {
    pub name: String,
    /// Kept for a file that names it. Where the card shows is `/pin stats`'s, and the tab is
    /// always there.
    pub places: Vec<String>,
    pub period: StatsPeriod,
    pub group: StatsGroup,
    pub measure: StatsMeasure,
    /// What each CLI's row of the card above the input shows, in order.
    pub columns: Vec<StatsColumn>,
    /// The "all together" card on the tab, and the bar and totals of the card.
    pub total: bool,
}

impl Default for StatsSettings {
    fn default() -> Self {
        Self {
            name: "stats".to_owned(),
            places: vec!["stats-tab".to_owned(), "above".to_owned()],
            period: StatsPeriod::All,
            group: StatsGroup::Cli,
            measure: StatsMeasure::Tokens,
            columns: vec![
                StatsColumn::Work,
                StatsColumn::Review,
                StatsColumn::Tokens,
                StatsColumn::Plan,
            ],
            total: true,
        }
    }
}

/// The period Stats opens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StatsPeriod {
    #[serde(rename = "today")]
    Today,
    #[serde(rename = "week")]
    Week,
    #[serde(rename = "month")]
    Month,
    #[default]
    #[serde(rename = "all")]
    All,
}

impl StatsPeriod {
    pub fn usage(self) -> UsagePeriod {
        match self {
            Self::Today => UsagePeriod::Today,
            Self::Week => UsagePeriod::Week,
            Self::Month => UsagePeriod::Month,
            Self::All => UsagePeriod::All,
        }
    }

    /// The period `/stats` was given: `today`, `week`, `month`, `all`, or the short words
    /// `/usage` takes.
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "today" | "day" | "t" => Some(Self::Today),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "all" | "a" => Some(Self::All),
            _ => None,
        }
    }
}

/// How the tab groups the crew's work first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsGroup {
    #[default]
    Cli,
    Model,
    Role,
}

/// What the bars measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsMeasure {
    #[default]
    Tokens,
    Tasks,
    Spend,
}

impl StatsMeasure {
    /// The next one `u` gives: tokens, tasks, spend, and round again.
    pub fn next(self) -> Self {
        match self {
            Self::Tokens => Self::Tasks,
            Self::Tasks => Self::Spend,
            Self::Spend => Self::Tokens,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::Tasks => "tasks",
            Self::Spend => "spend",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsColumn {
    /// Tasks built, or reviews done.
    Work,
    /// How the tasks fared in review.
    Review,
    Tokens,
    /// The CLI's fuller plan window.
    Plan,
    Spend,
    /// Time a task.
    Pace,
    /// Lines changed. Nothing records them per task yet, so the card leaves them out.
    Lines,
    /// Tasks that fell back from another model.
    Fallbacks,
}

impl StatsColumn {
    /// Every column, in the order the file's comment lists them.
    pub const ALL: [Self; 8] = [
        Self::Work,
        Self::Review,
        Self::Tokens,
        Self::Plan,
        Self::Spend,
        Self::Pace,
        Self::Lines,
        Self::Fallbacks,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Review => "review",
            Self::Tokens => "tokens",
            Self::Plan => "plan",
            Self::Spend => "spend",
            Self::Pace => "pace",
            Self::Lines => "lines",
            Self::Fallbacks => "fallbacks",
        }
    }
}

/// Which file a key's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatsSource {
    #[default]
    BuiltIn,
    Yours,
    Project,
}

impl StatsSource {
    pub fn word(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::Yours => "yours",
            Self::Project => "project",
        }
    }
}

/// The keys your file and the project's set. A key neither sets is built in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsSources {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub yours: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project: Vec<String>,
}

impl StatsSources {
    /// The project's file wins key by key, as it does when the files are read.
    pub fn of(&self, key: &str) -> StatsSource {
        let has = |keys: &[String]| keys.iter().any(|set| set == key);
        if has(&self.project) {
            StatsSource::Project
        } else if has(&self.yours) {
            StatsSource::Yours
        } else {
            StatsSource::BuiltIn
        }
    }

    pub fn is_empty(&self) -> bool {
        self.yours.is_empty() && self.project.is_empty()
    }
}

impl StatsSettings {
    /// The keys of `~/.argonauts/mods/stats.toml` under the project's own, each falling back
    /// to its default. A file that doesn't read is left out, and why is given back.
    pub fn read(home: &Path, project: Option<&Path>) -> (Self, Vec<String>) {
        let (settings, _, problems) = Self::read_with_sources(home, project);
        (settings, problems)
    }

    /// As [`Self::read`], with the keys each file set, for a page that says where a value
    /// comes from.
    pub fn read_with_sources(
        home: &Path,
        project: Option<&Path>,
    ) -> (Self, StatsSources, Vec<String>) {
        let mut problems = Vec::new();
        let mut keys = toml::Table::new();
        let mut sources = StatsSources::default();
        let global = home.join("mods").join(STATS_FILE);
        let local = project.map(|root| root.join(".argonauts").join("mods").join(STATS_FILE));
        for (path, project) in [(Some(global), false), (local, true)] {
            let Some(text) = path.as_ref().and_then(|path| fs::read_to_string(path).ok()) else {
                continue;
            };
            // Each file reads on its own, so a bad one is left out alone and named, and the
            // other still counts.
            match text
                .parse::<toml::Table>()
                .map_err(|err| err.message().to_owned())
                .and_then(|table| {
                    Self::deserialize(toml::Value::Table(table.clone()))
                        .map(|_| table)
                        .map_err(|err| err.message().to_owned())
                }) {
                Ok(table) => {
                    let set = if project {
                        &mut sources.project
                    } else {
                        &mut sources.yours
                    };
                    // `name` and `places` say what the mod is, not how it shows.
                    set.extend(table.keys().filter(|key| !is_about(key)).cloned());
                    keys.extend(table);
                }
                Err(why) => problems.push(format!("{}: {why}", path.unwrap_or_default().display())),
            }
        }
        match Self::deserialize(toml::Value::Table(keys)) {
            Ok(settings) => (settings, sources, problems),
            Err(err) => {
                problems.push(format!("mods/{STATS_FILE}: {}", err.message()));
                (Self::default(), StatsSources::default(), problems)
            }
        }
    }

    /// Why `text` isn't a stats file that reads, for Doctor and `/mods`.
    pub fn check(text: &str) -> Result<(), String> {
        toml::from_str::<Self>(text)
            .map(|_| ())
            .map_err(|err| err.message().to_owned())
    }

    /// The keys whose values are different from `from`, each with its value as TOML text, in the
    /// order the file lists them: what a page saves.
    pub fn changes_from(&self, from: &Self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        let mut add = |changed: bool, key: &str, text: String| {
            if changed {
                pairs.push((key.to_owned(), text));
            }
        };
        add(
            self.period != from.period,
            "period",
            toml_text(&self.period),
        );
        add(self.group != from.group, "group", toml_text(&self.group));
        add(
            self.measure != from.measure,
            "measure",
            toml_text(&self.measure),
        );
        add(
            self.columns != from.columns,
            "columns",
            toml_text(&self.columns),
        );
        add(self.total != from.total, "total", toml_text(&self.total));
        pairs
    }
}

fn is_about(key: &str) -> bool {
    matches!(key, "name" | "places")
}

/// `value` as it's written in the file: `"week"`, `["work", "review"]`, `true`.
fn toml_text<T: Serialize>(value: &T) -> String {
    toml::Value::try_from(value)
        .map(|value| value.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) {
        let mods = dir.join("mods");
        fs::create_dir_all(&mods).unwrap();
        fs::write(mods.join(STATS_FILE), text).unwrap();
    }

    #[test]
    fn no_file_opens_on_every_day_so_far() {
        let home = tempfile::tempdir().unwrap();
        let (settings, problems) = StatsSettings::read(home.path(), None);
        assert_eq!(settings, StatsSettings::default());
        assert!(problems.is_empty());
        assert_eq!(settings.period.usage(), UsagePeriod::All);
    }

    /// The file `/setup` writes opens Stats on every day so far too, as no file does.
    #[test]
    fn the_starter_file_opens_on_every_day_so_far() {
        let home = tempfile::tempdir().unwrap();
        let starter = crate::settings::mods::builtin::BUILTIN
            .iter()
            .find(|builtin| builtin.name == crate::settings::mods::builtin::STATS)
            .unwrap();
        write(home.path(), starter.text);
        let (settings, problems) = StatsSettings::read(home.path(), None);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(settings.period, StatsPeriod::All);
    }

    #[test]
    fn the_projects_keys_win_one_by_one() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write(home.path(), "period = \"week\"\nmeasure = \"spend\"\n");
        write(
            &repo.path().join(".argonauts"),
            "measure = \"tasks\"\ngroup = \"model\"\n",
        );
        let (settings, problems) = StatsSettings::read(home.path(), Some(repo.path()));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(settings.period, StatsPeriod::Week);
        assert_eq!(settings.measure, StatsMeasure::Tasks);
        assert_eq!(settings.group, StatsGroup::Model);
        assert!(settings.total);
    }

    /// A bad project file is left out alone: yours still counts, and the bad one is named.
    #[test]
    fn a_bad_file_leaves_only_itself_out() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write(home.path(), "period = \"week\"\n");
        write(&repo.path().join(".argonauts"), "period = \"fortnight\"\n");
        let (settings, problems) = StatsSettings::read(home.path(), Some(repo.path()));
        assert_eq!(settings.period, StatsPeriod::Week);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains(".argonauts"), "{problems:?}");
        assert!(StatsSettings::check("period = \"fortnight\"\n").is_err());
        assert!(
            crate::settings::mods::check::problems("stats", "period = \"fortnight\"\n").len() == 1
        );
    }

    #[test]
    fn a_wrong_key_is_named_and_the_defaults_stand() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), "measure = \"lines\"\n");
        let (settings, problems) = StatsSettings::read(home.path(), None);
        assert_eq!(settings, StatsSettings::default());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("stats.toml"), "{problems:?}");
    }

    #[test]
    fn each_key_says_which_file_set_it() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write(
            home.path(),
            "name = \"stats\"\nperiod = \"week\"\nmeasure = \"spend\"\n",
        );
        write(&repo.path().join(".argonauts"), "measure = \"tasks\"\n");
        let (_, sources, problems) =
            StatsSettings::read_with_sources(home.path(), Some(repo.path()));
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(sources.of("period"), StatsSource::Yours);
        assert_eq!(sources.of("measure"), StatsSource::Project);
        assert_eq!(sources.of("columns"), StatsSource::BuiltIn);
        assert!(!sources.yours.contains(&"name".to_owned()));
    }

    #[test]
    fn a_page_saves_only_the_keys_it_changed_as_the_file_writes_them() {
        let from = StatsSettings::default();
        let mut changed = from.clone();
        changed.period = StatsPeriod::Week;
        changed.columns = vec![StatsColumn::Tokens, StatsColumn::Work];
        changed.total = false;
        assert_eq!(
            changed.changes_from(&from),
            vec![
                ("period".to_owned(), "\"week\"".to_owned()),
                ("columns".to_owned(), "[\"tokens\", \"work\"]".to_owned()),
                ("total".to_owned(), "false".to_owned()),
            ]
        );
        assert!(from.changes_from(&from).is_empty());
    }

    #[test]
    fn the_period_takes_the_files_words_and_the_usage_words() {
        assert_eq!(StatsPeriod::parse("week"), Some(StatsPeriod::Week));
        assert_eq!(StatsPeriod::parse("t"), Some(StatsPeriod::Today));
        assert_eq!(StatsPeriod::parse("month"), Some(StatsPeriod::Month));
        assert_eq!(StatsPeriod::parse("soon"), None);
        assert_eq!(StatsMeasure::Spend.next(), StatsMeasure::Tokens);
    }
}
