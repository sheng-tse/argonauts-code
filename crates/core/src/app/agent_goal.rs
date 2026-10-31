//! One agent's goal as the screens draw it: the lines that must hold, every try with what still
//! failed after it, the checks last run, and what the agent's own CLI says about its goal.
//!
//! Plain data with no behaviour of its own, so whatever keeps a goal can fill it. The workflow
//! view's rows and goal panel, the opened agent's Goal tab, and the row at the bottom all draw
//! from this one shape.

use serde::{Deserialize, Serialize};

use crate::base::names::SessionKey;
use crate::{DiffStat, TaskId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentGoal {
    /// The agent working toward it, as its row is keyed.
    pub session: SessionKey,
    /// The agent's label in its workflow, such as `build:cli`.
    pub label: String,
    /// The role it fills, as the settings name it, such as `worker`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The task it is the goal of, when it is one task's. A key that holds the goal names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    pub kept_by: GoalKeeper,
    /// Why Argonauts Code keeps a goal that was asked of the CLI, such as "Claude Code has no goal
    /// of its own".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_note: Option<String>,
    /// Who runs the checks, such as `Argonauts Code`.
    pub checked_by: String,
    pub state: GoalState,
    /// Why it stopped short of holding, such as "the same failure 3 rounds running".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_because: Option<String>,
    /// How long ago it was set, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_ms_ago: Option<u64>,
    pub lines: Vec<GoalLine>,
    /// Oldest first.
    #[serde(default)]
    pub tries: Vec<GoalTry>,
    /// What a try's count counts, such as `failing tests`. Empty when it's the lines not met.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub counting: String,
    /// The most tries it may take, when you set one. No limit otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tries: Option<u32>,
    /// Across every try.
    #[serde(default)]
    pub work_ms: u64,
    #[serde(default)]
    pub tokens: u64,
    /// The checks as last run, in the order they ran.
    #[serde(default)]
    pub checks: Vec<GoalCheck>,
    /// The checks are running right now because you asked for them, beside the try under way.
    #[serde(default, skip_serializing_if = "is_false")]
    pub checking: bool,
    /// `None` where the CLI has no goal of its own, or reports nothing about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_says: Option<CliGoal>,
}

/// Who keeps the agent going until the goal holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum GoalKeeper {
    /// The CLI's own goal, such as Codex's.
    Cli {
        /// As the screen names the CLI, such as `Codex`.
        name: String,
    },
    /// Argonauts Code runs the check after each turn and sends back what still fails.
    Argonauts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalState {
    Running,
    /// Every line holds.
    Met,
    Paused,
    /// The same failure too many rounds running.
    Stuck,
    Cleared,
}

/// One condition of the goal, such as a done-when line of the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalLine {
    pub text: String,
    /// `None` before its first check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub met: Option<bool>,
    /// What checks it: `the judge`, or a command such as `cargo test due::parse`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checked_by: String,
    /// The try whose check first found it met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub met_on_try: Option<u32>,
    /// What still fails, such as `1 of 12 fails`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalTry {
    /// From 1.
    pub number: u32,
    /// What it did, in a few words.
    pub did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<DiffStat>,
    /// What still failed after it. `None` until its check has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing: Option<u32>,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCheck {
    /// The command, or `the judge`.
    pub name: String,
    /// `None` while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,
    /// The outcome first, then what else it reported, joined by ` · `: such as
    /// `1 failed · 11 passed · 4.1s` or `passed · 6 tests`.
    pub result: String,
    /// The lines of its output worth reading, such as the failing test and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
}

/// The CLI's own account of its goal, which Argonauts Code shows and never takes as the check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliGoal {
    /// As the screen names the CLI, such as `Codex`.
    pub cli: String,
    /// The goal's state in the CLI's own word, such as `active`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl AgentGoal {
    pub fn lines_met(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.met == Some(true))
            .count()
    }

    /// The try going on now, or the last one made. 0 before the first.
    pub fn current_try(&self) -> u32 {
        self.tries.last().map_or(0, |last| last.number)
    }

    /// Still being worked on, which is what the row at the bottom counts.
    pub fn is_running(&self) -> bool {
        self.state == GoalState::Running
    }

    /// The check that says most about where the goal stands: the first that failed, and the
    /// last one run when all passed.
    pub fn telling_check(&self) -> Option<&GoalCheck> {
        self.checks
            .iter()
            .find(|check| check.passed == Some(false))
            .or_else(|| self.checks.last())
    }
}

impl GoalKeeper {
    /// `Codex's own goal`, or `Argonauts Code`, as it reads after "kept by".
    pub fn words(&self) -> String {
        match self {
            Self::Cli { name } => format!("{name}'s own goal"),
            Self::Argonauts => "Argonauts Code".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(met: Option<bool>) -> GoalLine {
        GoalLine {
            text: "tasks overdue lists tasks due before today".into(),
            met,
            checked_by: "the judge".into(),
            met_on_try: None,
            failing: None,
        }
    }

    fn check(name: &str, passed: Option<bool>) -> GoalCheck {
        GoalCheck {
            name: name.into(),
            passed,
            result: String::new(),
            detail: Vec::new(),
        }
    }

    fn goal() -> AgentGoal {
        AgentGoal {
            session: SessionKey::parse("s-002").unwrap(),
            label: "build:cli".into(),
            role: None,
            task: None,
            kept_by: GoalKeeper::Cli {
                name: "Codex".into(),
            },
            kept_note: None,
            checked_by: "Argonauts Code".into(),
            state: GoalState::Running,
            stopped_because: None,
            set_ms_ago: None,
            lines: vec![line(Some(true)), line(Some(false)), line(None)],
            tries: Vec::new(),
            counting: String::new(),
            max_tries: None,
            work_ms: 0,
            tokens: 0,
            checks: Vec::new(),
            checking: false,
            cli_says: None,
        }
    }

    #[test]
    fn only_a_line_found_met_counts_as_met() {
        assert_eq!(goal().lines_met(), 1);
    }

    #[test]
    fn the_first_failed_check_tells_most_and_else_the_last() {
        let mut goal = goal();
        assert!(goal.telling_check().is_none());
        goal.checks = vec![
            check("cargo test due::parse", Some(true)),
            check("cargo test overdue", Some(false)),
            check("the judge", Some(true)),
        ];
        assert_eq!(goal.telling_check().unwrap().name, "cargo test overdue");
        goal.checks[1].passed = Some(true);
        assert_eq!(goal.telling_check().unwrap().name, "the judge");
    }

    #[test]
    fn a_goal_written_with_only_what_is_known_still_reads() {
        let json = serde_json::json!({
            "session": "s-002",
            "label": "build:docs",
            "kept_by": {"by": "argonauts"},
            "checked_by": "Argonauts Code",
            "state": "running",
            "lines": [{"text": "the README names the command"}]
        });
        let goal: AgentGoal = serde_json::from_value(json).unwrap();
        assert_eq!(goal.kept_by.words(), "Argonauts Code");
        assert_eq!(goal.current_try(), 0);
        assert_eq!(goal.lines[0].met, None);
    }
}
