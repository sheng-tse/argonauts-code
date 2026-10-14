//! What the screens are told about one goal: its lines with each one met or not and what
//! checked it, who keeps it, every try with what it changed and what still failed after it,
//! the last check, and what the CLI itself says about its goal. The engine fills it from the
//! journal and the views only draw it.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{GoalScope, KeptBy};
use crate::events::goal::GoalStatus;
use crate::names::{ModelRef, SessionKey};
use crate::{DiffStat, TaskId};

/// One goal, as the workflow view's goal panel and the agent's Goal tab show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalReport {
    /// Whose goal it is: an agent's label such as `build:cli`, or the workflow's name.
    pub of: String,
    pub scope: GoalScope,
    /// The task it belongs to, for a task's or a step's goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    /// The session of the agent that works toward it, as its row is keyed. `None` for a
    /// workflow's goal, which every agent of the round works toward, and before the agent's
    /// first session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionKey>,
    /// The agent that works toward it, for its CLI's and its model's names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<ModelRef>,
    /// The condition in the words it was set with.
    pub condition: String,
    pub status: GoalStatus,
    /// Why it ended short of holding, such as "the same failure 3 rounds running".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_because: Option<String>,
    pub kept: Keeper,
    /// "Until all of these hold".
    pub lines: Vec<LineReport>,
    /// Oldest first. The last one is still running while the goal is pursued.
    pub tries: Vec<TryReport>,
    /// The most tries, when you set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    /// The last check: every command that ran and the judge, in the order they ran.
    pub checks: Vec<CheckReport>,
    /// Its checks are running right now because you asked for them, beside the try under way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checking: bool,
    /// The CLI's own account of its goal, when the CLI keeps it and says anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_says: Option<CliGoalReport>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub set_at: Option<OffsetDateTime>,
    /// Across every try.
    pub work_ms: u64,
    pub tokens: u64,
}

/// Who keeps the goal going, and who checks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keeper {
    pub by: KeptBy,
    /// `Codex's own goal`, or `Argonauts Code`.
    pub words: String,
    /// The CLI whose own goal keeps it, as the screen names it: `Codex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    /// Why Argonauts Code keeps a goal that was asked of the CLI, such as "Claude Code has no goal
    /// of its own".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineReport {
    /// `G1`, ...
    pub id: String,
    pub text: String,
    pub state: LineState,
    /// What checks it: `the judge`, or a command such as `cargo test due::parse`.
    pub checked_by: String,
    /// What still fails, when it isn't met: `1 still fails`, or the judge's one line.
    pub still_fails: String,
    /// The try whose check first found it met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub met_on_try: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineState {
    /// No check has run yet.
    Unchecked,
    Met,
    NotMet,
}

/// One try: the work, then its check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TryReport {
    /// From 1.
    pub n: u32,
    /// What it did, in a few words: `fixed the date comparison`.
    pub what: String,
    /// What it changed. `None` when nothing recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<DiffStat>,
    /// What still failed after it: failing tests where a command counted them, else lines not
    /// met. `None` until its check ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub still_failing: Option<Failing>,
    pub lines_met: u32,
    pub ms: u64,
    pub tokens: u64,
    pub running: bool,
    /// The try that took a different approach after the goal was stuck.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub changed_approach: bool,
}

/// What still failed after a try, for the bar beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failing {
    pub count: u32,
    pub what: FailingWhat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailingWhat {
    /// A command said how many of its tests failed.
    Tests,
    /// Nothing counted tests, so it's the goal's lines not met.
    Lines,
}

/// One thing the last check ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    /// The command as written, or `the judge`.
    pub name: String,
    pub passed: bool,
    /// `1 failed · 11 passed`, `passed · 6 tests`, or `1 of 1 line met`.
    pub summary: String,
    /// The lines of its output that name what failed, oldest first.
    pub output: Vec<String>,
    pub ms: u64,
    /// The judge, for its model's name: `the judge · 1 of 1 line met · Astra`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<ModelRef>,
}

/// What the CLI says about its own goal: its account, never Argonauts Code's check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliGoalReport {
    /// `Codex`.
    pub cli: String,
    /// `active`, `complete`, `blocked`, `paused`, as the CLI words it.
    pub status: String,
    /// Its own note on where it stands.
    pub note: String,
    /// Turns it took inside this goal, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
}

impl GoalReport {
    pub fn lines_met(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.state == LineState::Met)
            .count()
    }

    /// Still being worked: the bottom row counts these.
    pub fn running(&self) -> bool {
        self.status == GoalStatus::Pursuing
    }

    /// `kept by Codex's own goal · checked by Argonauts Code`.
    pub fn kept_words(&self) -> String {
        match self.kept.by {
            KeptBy::Cli => format!("kept by {} · checked by Argonauts Code", self.kept.words),
            KeptBy::Argonauts => "kept by Argonauts Code".to_owned(),
        }
    }

    /// The agent row's words: `goal met in 2 tries`, `try 3 · 2 of 3 lines met`.
    pub fn row_words(&self) -> String {
        let tries = self.tries.len().max(1);
        let total = self.lines.len();
        let lines = if total == 1 { "line" } else { "lines" };
        match self.status {
            GoalStatus::Met => match tries {
                1 => "goal met in 1 try".to_owned(),
                n => format!("goal met in {n} tries"),
            },
            GoalStatus::Pursuing => {
                format!("try {tries} · {} of {total} {lines} met", self.lines_met())
            }
            GoalStatus::Paused => {
                format!("goal paused · {} of {total} {lines} met", self.lines_met())
            }
            GoalStatus::Cleared => {
                format!("goal cleared · {} of {total} {lines} met", self.lines_met())
            }
            GoalStatus::Stuck | GoalStatus::OverLimit => {
                format!("goal stuck · {} of {total} {lines} met", self.lines_met())
            }
        }
    }

    /// The role its agent fills, as the settings name it: `worker` for a task's or a step's
    /// goal, `main` for the main agent's alone. None for a workflow's goal,
    /// which every role of the round works toward.
    pub fn role(&self) -> Option<&'static str> {
        match self.scope {
            GoalScope::Task | GoalScope::Step => Some("worker"),
            GoalScope::Captain => Some("main"),
            GoalScope::Workflow => None,
        }
    }

    /// What the number beside each try counts: `failing tests` where a command counted them.
    /// Empty when it's the goal's lines not met.
    pub fn counting(&self) -> &'static str {
        let tests = self.tries.iter().any(|a_try| {
            a_try
                .still_failing
                .is_some_and(|failing| failing.what == FailingWhat::Tests)
        });
        if tests { "failing tests" } else { "" }
    }

    /// The tries panel's own line: `3 so far · no limit set`.
    pub fn tries_words(&self) -> String {
        let limit = match self.max_rounds {
            Some(max) => format!("{max} at most"),
            None => "no limit set".to_owned(),
        };
        format!("{} so far · {limit}", self.tries.len())
    }
}

/// `2 goals running`, for the row at the bottom. Empty when none is.
pub fn running_words(reports: &[GoalReport]) -> String {
    match reports.iter().filter(|report| report.running()).count() {
        0 => String::new(),
        1 => "1 goal running".to_owned(),
        n => format!("{n} goals running"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, state: LineState) -> LineReport {
        LineReport {
            id: id.to_owned(),
            text: format!("line {id}"),
            state,
            checked_by: "the judge".to_owned(),
            still_fails: String::new(),
            met_on_try: None,
        }
    }

    fn a_try(n: u32) -> TryReport {
        TryReport {
            n,
            what: "worked".to_owned(),
            changed: None,
            still_failing: None,
            lines_met: 0,
            ms: 0,
            tokens: 0,
            running: false,
            changed_approach: false,
        }
    }

    fn report(status: GoalStatus, by: KeptBy) -> GoalReport {
        GoalReport {
            of: "build:cli".to_owned(),
            scope: GoalScope::Task,
            task: None,
            session: None,
            worker: None,
            condition: String::new(),
            status,
            ended_because: None,
            kept: Keeper {
                by,
                words: match by {
                    KeptBy::Cli => "Codex's own goal".to_owned(),
                    KeptBy::Argonauts => "Argonauts Code".to_owned(),
                },
                cli: (by == KeptBy::Cli).then(|| "Codex".to_owned()),
                note: None,
            },
            lines: vec![
                line("G1", LineState::Met),
                line("G2", LineState::Met),
                line("G3", LineState::NotMet),
            ],
            tries: vec![a_try(1), a_try(2), a_try(3)],
            max_rounds: None,
            checks: Vec::new(),
            checking: false,
            cli_says: None,
            set_at: None,
            work_ms: 0,
            tokens: 0,
        }
    }

    #[test]
    fn the_row_says_the_try_and_the_lines_met_as_the_picture_does() {
        let pursuing = report(GoalStatus::Pursuing, KeptBy::Cli);
        assert_eq!(pursuing.row_words(), "try 3 · 2 of 3 lines met");
        assert_eq!(
            pursuing.kept_words(),
            "kept by Codex's own goal · checked by Argonauts Code"
        );
        assert_eq!(pursuing.tries_words(), "3 so far · no limit set");

        let mut met = report(GoalStatus::Met, KeptBy::Argonauts);
        met.tries.truncate(2);
        assert_eq!(met.row_words(), "goal met in 2 tries");
        assert_eq!(met.kept_words(), "kept by Argonauts Code");
    }

    #[test]
    fn a_report_says_its_agents_role_and_what_its_tries_count() {
        let mut task = report(GoalStatus::Pursuing, KeptBy::Cli);
        assert_eq!(task.role(), Some("worker"));
        // Nothing counted tests yet, so the number is the lines not met.
        assert_eq!(task.counting(), "");
        task.tries[0].still_failing = Some(Failing {
            count: 4,
            what: FailingWhat::Tests,
        });
        assert_eq!(task.counting(), "failing tests");

        task.scope = GoalScope::Captain;
        assert_eq!(task.role(), Some("main"));
        task.scope = GoalScope::Workflow;
        assert_eq!(task.role(), None);
    }

    #[test]
    fn the_bottom_row_counts_the_goals_still_running() {
        let reports = [
            report(GoalStatus::Pursuing, KeptBy::Cli),
            report(GoalStatus::Pursuing, KeptBy::Argonauts),
            report(GoalStatus::Met, KeptBy::Cli),
        ];
        assert_eq!(running_words(&reports), "2 goals running");
        assert_eq!(running_words(&reports[2..]), "");
    }
}
