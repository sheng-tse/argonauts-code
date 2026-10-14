//! What the Overview tab draws: every CLI's work on this computer, from the usage ledger and
//! the records the import took from the CLIs' own session files. Numbers only, never a
//! model's words, and nothing of what was said.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::api::day_format;
use crate::settings::overview::YearColors;

/// The Overview's periods, as its row under the tabs names them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverviewPeriod {
    #[default]
    All,
    Month,
    Week,
}

impl OverviewPeriod {
    /// In the order `r` cycles them, as Claude Code's /stats does.
    pub const ALL: [OverviewPeriod; 3] = [
        OverviewPeriod::All,
        OverviewPeriod::Week,
        OverviewPeriod::Month,
    ];

    pub fn title(self) -> &'static str {
        match self {
            OverviewPeriod::All => "All time",
            OverviewPeriod::Month => "Last 30 days",
            OverviewPeriod::Week => "Last 7 days",
        }
    }

    /// The days it spans, today among them, or `None` for every day.
    pub fn days(self) -> Option<i64> {
        match self {
            OverviewPeriod::All => None,
            OverviewPeriod::Month => Some(30),
            OverviewPeriod::Week => Some(7),
        }
    }

    /// Its first day when today is `today`.
    pub fn first_day(self, today: Date) -> Option<Date> {
        self.days()
            .map(|days| today.saturating_sub(time::Duration::days(days - 1)))
    }
}

/// Where the import of the CLIs' own history stands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ImportStatus {
    /// Nobody was asked yet: the tab asks first.
    #[default]
    Unasked,
    /// You said not now. `i` on the tab imports after all.
    Declined,
    /// Reading session files in the background.
    Running { done: u32, of: u32 },
    /// Imported, and kept up with what's new each time the tab opens.
    Done,
    /// It stopped, and why.
    Failed { why: String },
}

/// Tokens split the way the CLIs count them. `input` is new input only, never what came from
/// the cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// All input, the cache's included.
    pub fn all_input(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, more: &Tokens) {
        self.input += more.input;
        self.output += more.output;
        self.cache_read += more.cache_read;
        self.cache_write += more.cache_write;
    }
}

/// One CLI whose work the Overview counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliSource {
    /// Such as `claude-code`.
    pub agent: String,
    /// The first day it has work, in any period.
    #[serde(default, with = "day_format::option")]
    pub since: Option<Date>,
    /// Its history came from its own session files. Otherwise only what ran through
    /// Argonauts counts.
    pub imported: bool,
    /// Its sessions in the period.
    pub sessions: u32,
}

/// One local day of the year chart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewDay {
    #[serde(with = "day_format")]
    pub day: Date,
    pub tokens: u64,
    /// The CLI that ran the most tokens that day.
    pub agent: String,
    /// Of that CLI, the model that ran the most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One week of the weekly bars, Monday first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewWeek {
    #[serde(with = "day_format")]
    pub start: Date,
    /// Tokens by CLI.
    pub by_cli: Vec<(String, u64)>,
}

impl OverviewWeek {
    pub fn tokens(&self) -> u64 {
        self.by_cli.iter().map(|(_, tokens)| tokens).sum()
    }
}

/// One model in the period, whichever CLI ran it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverviewModel {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub tokens: Tokens,
    /// What its tokens cost at list prices, when the model has a price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_value: Option<f64>,
    pub sessions: u32,
}

/// One day of the Models chart: the tokens of each of the period's top three models that day,
/// in the order `models` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDay {
    #[serde(with = "day_format")]
    pub day: Date,
    pub tokens: Vec<u64>,
}

/// A month's tokens by CLI, for how the share moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewMonth {
    /// Its first day.
    #[serde(with = "day_format")]
    pub month: Date,
    pub by_cli: Vec<(String, u64)>,
}

/// What ran through Argonauts itself, from the work ledger.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThroughArgonauts {
    /// The first day the work ledger holds.
    #[serde(default, with = "day_format::option")]
    pub since: Option<Date>,
    pub workflows: u32,
    /// Workflows by name, the most first.
    pub by_workflow: Vec<(String, u32)>,
    pub merged: u32,
    /// Of the reviewed tasks, the share that passed first time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_pass: Option<f64>,
    pub fallbacks: u32,
    /// Of those, the ones taken because a plan window ran out.
    pub for_limit: u32,
    /// Questions settled on autopilot.
    pub answered: u32,
}

/// Everything the Overview tab draws for a period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverviewData {
    pub period: OverviewPeriod,
    #[serde(with = "day_format")]
    pub today: Date,
    pub import: ImportStatus,
    /// What the import found, as it says it once: `Claude Code since Oct 2025, 812 sessions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<String>,
    /// Every CLI with work, the most tokens first.
    pub sources: Vec<CliSource>,
    pub total: Tokens,
    /// Tokens in the last 30 days (7 for the week) and the as many days before them, for the
    /// trend.
    pub recent: u64,
    pub before: u64,
    /// What the period's tokens cost at list prices, and whether some had no price.
    pub api_value: f64,
    pub api_partial: bool,
    /// Days with any tokens in the period, and the days the period spans so far.
    pub active_days: u32,
    pub span_days: u32,
    /// Days in a row up to today (or yesterday), and the longest run of them.
    pub streak: u32,
    pub longest_streak: u32,
    /// The last 53 weeks' days that have tokens, oldest first, whatever the period.
    pub days: Vec<OverviewDay>,
    /// Tokens by local hour the answers came, midnight first.
    pub hours: Vec<u64>,
    /// The last 26 weeks, oldest first, whatever the period.
    pub weeks: Vec<OverviewWeek>,
    /// Every model, the most tokens first.
    pub models: Vec<OverviewModel>,
    /// Tokens by folder, the most first.
    pub folders: Vec<(String, u64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argonauts: Option<ThroughArgonauts>,
    /// Tokens subagents used, where the CLI says.
    pub subagent_tokens: u64,
    /// The longest session, first answer to last, in seconds.
    pub longest_session_secs: u64,
    /// Each month's tokens by CLI, oldest first.
    pub months: Vec<OverviewMonth>,
    /// How the year of days is coloured.
    pub year_colors: YearColors,
    /// The period's day with the most tokens.
    #[serde(default, with = "day_format::option")]
    pub peak_day: Option<Date>,
    /// The period's days with work, oldest first, for the Models chart.
    #[serde(default)]
    pub model_days: Vec<ModelDay>,
}

impl OverviewData {
    /// Nothing recorded yet.
    pub fn empty(period: OverviewPeriod, today: Date) -> Self {
        Self {
            period,
            today,
            import: ImportStatus::default(),
            found: None,
            sources: Vec::new(),
            total: Tokens::default(),
            recent: 0,
            before: 0,
            api_value: 0.0,
            api_partial: false,
            active_days: 0,
            span_days: 0,
            streak: 0,
            longest_streak: 0,
            days: Vec::new(),
            hours: vec![0; 24],
            weeks: Vec::new(),
            models: Vec::new(),
            folders: Vec::new(),
            argonauts: None,
            subagent_tokens: 0,
            longest_session_secs: 0,
            months: Vec::new(),
            year_colors: YearColors::default(),
            peak_day: None,
            model_days: Vec::new(),
        }
    }

    /// Tokens by CLI, the most first.
    pub fn by_cli(&self) -> Vec<(String, u64)> {
        let mut clis: Vec<(String, u64)> = Vec::new();
        for model in &self.models {
            match clis.iter_mut().find(|(agent, _)| *agent == model.agent) {
                Some((_, tokens)) => *tokens += model.tokens.total(),
                None => clis.push((model.agent.clone(), model.tokens.total())),
            }
        }
        clis.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        clis
    }

    pub fn sessions(&self) -> u32 {
        self.sources.iter().map(|source| source.sessions).sum()
    }
}
