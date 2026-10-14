//! What the crew did over a period, for the Stats tab and its card above the input: who did
//! which work, how it fared in review, and what it used, per CLI, model and role. The rows are
//! sums the screen regroups itself, so the by-model and by-role views and a model opened one
//! level deeper need no request of their own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::Date;

use crate::Spend;
use crate::api::day_format;
use crate::settings::crew::CrewRole;
use crate::settings::stats::{StatsSettings, StatsSources};

/// A role Stats counts work under: the crew's five, each finer role under one of them, and the
/// merger, which lands a run's work and is nobody's setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsRole {
    Main,
    Orchestrator,
    Worker,
    Reviewer,
    Judge,
    Merger,
}

impl From<CrewRole> for StatsRole {
    fn from(role: CrewRole) -> Self {
        match role {
            CrewRole::Main => Self::Main,
            CrewRole::Orchestrator => Self::Orchestrator,
            CrewRole::Worker => Self::Worker,
            CrewRole::Reviewer => Self::Reviewer,
            CrewRole::Judge => Self::Judge,
        }
    }
}

impl StatsRole {
    /// In the order By role lists them, from the one you talk to to the one that lands the
    /// work.
    pub const ALL: [StatsRole; 6] = [
        StatsRole::Main,
        StatsRole::Orchestrator,
        StatsRole::Worker,
        StatsRole::Reviewer,
        StatsRole::Judge,
        StatsRole::Merger,
    ];

    /// In the order the who-did-what card lists them.
    pub const BY_WORK: [StatsRole; 6] = [
        StatsRole::Orchestrator,
        StatsRole::Worker,
        StatsRole::Reviewer,
        StatsRole::Judge,
        StatsRole::Merger,
        StatsRole::Main,
    ];

    /// The name a ledger line files the merger's sessions under.
    pub const MERGER: &'static str = "merger";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Orchestrator => "orchestrator",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Judge => "judge",
            Self::Merger => Self::MERGER,
        }
    }

    /// The role's people in a heading, as in "all the workers".
    pub fn plural(self) -> &'static str {
        match self {
            Self::Worker => "workers",
            Self::Reviewer => "reviewers",
            one => one.as_str(),
        }
    }

    /// The role whose work a finer one, as a session's start names it, counts as.
    pub fn counted_as(role: &str) -> Self {
        match role {
            "main" | "narrator" => Self::Main,
            "worker" | "experiment" | "consultant" => Self::Worker,
            "reviewer" | "run-review" => Self::Reviewer,
            "judge" => Self::Judge,
            Self::MERGER => Self::Merger,
            _ => Self::Orchestrator,
        }
    }

    /// What one piece of its work is called: `task`, `review`.
    pub fn unit(self) -> &'static str {
        match self {
            Self::Main => "conversation",
            Self::Orchestrator => "workflow",
            Self::Worker => "task",
            Self::Reviewer => "review",
            Self::Judge => "sign-off",
            Self::Merger => "merge",
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
    /// What each model did and used on each local day of the period, by role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<ModelDay>,
    /// Each time a task fell back to another model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<Fallback>,
    /// Workflows the period ran, and the time they took together.
    #[serde(default)]
    pub workflows: u32,
    #[serde(default)]
    pub took_secs: u64,
    /// How those runs ended, and what the judge and the merger settled in them.
    #[serde(default, skip_serializing_if = "Outcomes::is_empty")]
    pub outcomes: Outcomes,
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

/// What the roles beside the workers and the reviewers settled over the period: the runs the
/// orchestrator planned, the judge's rulings, and the merger's merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Outcomes {
    /// Runs whose ending the ledger holds. An older run counts as a workflow, but nothing
    /// says how it ended.
    #[serde(default)]
    pub runs: u32,
    /// Of those, the runs whose work was merged.
    #[serde(default)]
    pub runs_merged: u32,
    /// Of those, the runs whose plan was made again while they ran.
    #[serde(default)]
    pub runs_replanned: u32,
    /// The judge's rulings between a worker and a reviewer that let the work stand, and those
    /// that sent it back.
    #[serde(default)]
    pub sided_with_worker: u32,
    #[serde(default)]
    pub sided_with_reviewer: u32,
    /// Merges that met no conflict, merges whose conflict the merger settled, and merges it
    /// couldn't settle.
    #[serde(default)]
    pub merges_clean: u32,
    #[serde(default)]
    pub merges_settled: u32,
    #[serde(default)]
    pub merges_failed: u32,
    /// Merges whose checks failed afterwards.
    #[serde(default)]
    pub checks_failed: u32,
}

impl Outcomes {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn rulings(&self) -> u32 {
        self.sided_with_worker + self.sided_with_reviewer
    }

    pub fn merges(&self) -> u32 {
        self.merges_clean + self.merges_settled + self.merges_failed
    }
}

/// One CLI and model in one role, and for a worker one kind of work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkRow {
    /// The CLI's id, such as `codex`, which picks its color.
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub role: StatsRole,
    /// What a worker built, such as `build`, `tests` or `docs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub spent: Spend,
    /// What it did in the role: tasks built, reviews, sign-offs, workflows planned, merges or
    /// conversations held, as [`StatsRole::unit`] names them.
    #[serde(default)]
    pub done: u32,
    /// Of `done`, the reviews that sent the work back and the sign-offs that refused it.
    #[serde(default)]
    pub sent_back: u32,
    /// The time its sessions took, together.
    #[serde(default)]
    pub took_secs: u64,
    /// The sessions it ran in the role, and the tools they called.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sessions: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tool_calls: u32,
    /// For the main agent: the turns of its conversations.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub turns: u32,
    /// For a reviewer or judge: what its reviews were given and sent work back for.
    #[serde(default, skip_serializing_if = "ReviewTally::is_empty")]
    pub review: ReviewTally,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What a reviewer's or a judge's reviews were given, and why they sent work back.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReviewTally {
    /// Reviews that sent work back, by why: `a test missing`, `a check failed`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasons: BTreeMap<String, u32>,
    /// The check commands the reviews were given with their results, such as `pytest`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    /// Reviews whose checks all passed, and reviews given a check that failed.
    #[serde(default)]
    pub passed: u32,
    #[serde(default)]
    pub failed: u32,
    /// Reviews that ran commands of their own beside the checks.
    #[serde(default)]
    pub ran_own: u32,
}

impl ReviewTally {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Adds `more` in: counts summed, checks named once.
    pub fn add(&mut self, more: &ReviewTally) {
        for (why, n) in &more.reasons {
            *self.reasons.entry(why.clone()).or_default() += n;
        }
        for check in &more.checks {
            if !self.checks.contains(check) {
                self.checks.push(check.clone());
            }
        }
        self.passed += more.passed;
        self.failed += more.failed;
        self.ran_own += more.ran_own;
    }
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
    /// Why, each time, in the words of the review that did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub merged: bool,
    /// A reviewer or the judge looked at it.
    #[serde(default)]
    pub reviewed: bool,
}

/// One model's work in one role on one local day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelDay {
    #[serde(with = "day_format")]
    pub day: Date,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `None` for tokens no role is known for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<StatsRole>,
    pub tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    /// Pieces of work it finished that day, as [`StatsRole::unit`] names them.
    #[serde(default)]
    pub done: u32,
}

/// A model a task fell back to, from whom, and why, such as "usage limit, resets 3:45pm".
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
    /// The plan window that had run out on the one it fell back from, such as `five_hour`,
    /// when that CLI's last reading said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_finer_role_follows_one_of_the_five() {
        assert_eq!(StatsRole::counted_as("worker"), StatsRole::Worker);
        assert_eq!(StatsRole::counted_as("judge"), StatsRole::Judge);
        assert_eq!(StatsRole::counted_as("reviewer"), StatsRole::Reviewer);
        assert_eq!(StatsRole::counted_as("draft"), StatsRole::Orchestrator);
        assert_eq!(
            StatsRole::counted_as("tie-breaker"),
            StatsRole::Orchestrator
        );
        assert_eq!(StatsRole::counted_as("main"), StatsRole::Main);
    }

    #[test]
    fn the_merger_is_a_role_of_its_own_beside_the_crews_five() {
        assert_eq!(StatsRole::counted_as("merger"), StatsRole::Merger);
        assert_eq!(StatsRole::ALL.len(), CrewRole::ALL.len() + 1);
        for role in CrewRole::ALL {
            assert_eq!(StatsRole::from(role).as_str(), role.as_str());
            assert_eq!(StatsRole::from(role).plural(), role.plural());
        }
        let text = serde_json::to_string(&StatsRole::Merger).unwrap();
        assert_eq!(text, r#""merger""#);
    }

    #[test]
    fn empty_stats_write_as_little_as_they_hold() {
        let text = serde_json::to_string(&CrewStats::default()).unwrap();
        assert_eq!(text, r#"{"workflows":0,"took_secs":0}"#);
        let back: CrewStats = serde_json::from_str("{}").unwrap();
        assert_eq!(back, CrewStats::default());
    }
}
