//! What the crew did over a period, for the Stats tab and its card above the input: who did
//! which work, how it fared in review, and what it used, per CLI, model and role. The rows are
//! sums the screen regroups itself, so the by-model and by-role views and a model opened one
//! level deeper need no request of their own.

use serde::{Deserialize, Serialize};
use time::Date;

use crate::Spend;
use crate::api::day_format;
use crate::settings::stats::{StatsSettings, StatsSources};

/// The five roles every finer role follows. Stats groups by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewRole {
    /// The CLI you talk to.
    Main,
    /// Plans and delegates, with the roles that shape a plan.
    Orchestrator,
    /// Builds tasks.
    Worker,
    /// Reviews another's task.
    Reviewer,
    /// Settles what the reviewers can't, and signs off.
    Judge,
}

impl CrewRole {
    /// In the order the who-did-what card lists them.
    pub const ALL: [CrewRole; 5] = [
        CrewRole::Orchestrator,
        CrewRole::Worker,
        CrewRole::Reviewer,
        CrewRole::Judge,
        CrewRole::Main,
    ];

    /// The role a finer one, as a session's start names it, follows.
    pub fn of(role: &str) -> Self {
        match role {
            "main" | "narrator" => Self::Main,
            "worker" | "spike" | "consultant" => Self::Worker,
            "reviewer" | "retro" => Self::Reviewer,
            "judge" | "adjudicator" | "approver" => Self::Judge,
            _ => Self::Orchestrator,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Orchestrator => "orchestrator",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Judge => "judge",
        }
    }

    /// The role's people, as the who-did-what card heads a row: `workers`.
    pub fn plural(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Orchestrator => "orchestrator",
            Self::Worker => "workers",
            Self::Reviewer => "reviewers",
            Self::Judge => "judge",
        }
    }

    /// What one piece of its work is called: `task`, `review`.
    pub fn unit(self) -> &'static str {
        match self {
            Self::Main => "turn",
            Self::Orchestrator => "workflow",
            Self::Worker => "task",
            Self::Reviewer => "review",
            Self::Judge => "sign-off",
        }
    }
}

/// What the crew did over a period. Every list is in no order the screen relies on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CrewStats {
    /// One row for each CLI, model, role and kind of work, summed over the period.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work: Vec<WorkRow>,
    /// Each task the period's runs ended, with who built it and how review took it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskRecord>,
    /// Tokens each model used on each of the last seven local days, whatever the period.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<ModelDay>,
    /// Each time a model took over another's task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<Fallback>,
    /// Workflows the period ran, and the time they took together.
    #[serde(default)]
    pub workflows: u32,
    #[serde(default)]
    pub took_secs: u64,
    /// How you asked for them to be shown, in `mods/stats.toml`.
    #[serde(default, skip_serializing_if = "is_default")]
    pub settings: StatsSettings,
    /// Which file set each of those keys, for the stats mod's page.
    #[serde(default, skip_serializing_if = "StatsSources::is_empty")]
    pub sources: StatsSources,
    /// Why a stats file was left out, one line each, naming the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

fn is_default(settings: &StatsSettings) -> bool {
    *settings == StatsSettings::default()
}

/// One CLI and model in one role, and for a worker one kind of work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkRow {
    /// The CLI's id, such as `codex`, which picks its color.
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub role: CrewRole,
    /// What a worker built, such as `build`, `tests` or `docs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub spent: Spend,
    /// What it did in the role: tasks built, reviews, sign-offs, workflows planned or turns
    /// answered, as [`CrewRole::unit`] names them.
    #[serde(default)]
    pub done: u32,
    /// Of `done`, the reviews and rulings that sent the work back.
    #[serde(default)]
    pub sent_back: u32,
    /// The time its sessions took, together.
    #[serde(default)]
    pub took_secs: u64,
}

/// One task, by the model whose work it ended with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Times review sent it back.
    #[serde(default)]
    pub sent_back: u32,
    #[serde(default)]
    pub merged: bool,
    /// A reviewer or the judge looked at it.
    #[serde(default)]
    pub reviewed: bool,
}

/// One model's tokens on one local day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelDay {
    #[serde(with = "day_format")]
    pub day: Date,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
}

/// A model that took over a task, from whom, and why, such as "usage limit, resets 3:45pm".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fallback {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub from_agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_model: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// The plan window that had run out on the one it took over from, such as `five_hour`,
    /// when that CLI's last reading said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_finer_role_follows_one_of_the_five() {
        assert_eq!(CrewRole::of("worker"), CrewRole::Worker);
        assert_eq!(CrewRole::of("adjudicator"), CrewRole::Judge);
        assert_eq!(CrewRole::of("reviewer"), CrewRole::Reviewer);
        assert_eq!(CrewRole::of("drafter"), CrewRole::Orchestrator);
        assert_eq!(CrewRole::of("phase-lead"), CrewRole::Orchestrator);
        assert_eq!(CrewRole::of("main"), CrewRole::Main);
    }

    #[test]
    fn empty_stats_write_as_little_as_they_hold() {
        let text = serde_json::to_string(&CrewStats::default()).unwrap();
        assert_eq!(text, r#"{"workflows":0,"took_secs":0}"#);
        let back: CrewStats = serde_json::from_str("{}").unwrap();
        assert_eq!(back, CrewStats::default());
    }
}
