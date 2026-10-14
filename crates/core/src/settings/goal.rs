//! Goals, `[goal]`, and `[roles.<role>] goal`. A goal is a condition: "keep going until this is
//! true", and how to check it. You set one on any CLI, around a workflow, a step, a task, or
//! the main agent alone, and it ends when it holds, when you pause or clear it, or when it's
//! stuck.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GoalSettings {
    /// The goal itself: keep going until this is true. `/goal` sets it for the conversation, a
    /// request's own words for that request, and a settings file for every request in its
    /// scope. Unset, there's no goal unless `default` gives each request its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// `off`, or `from-request`: the main agent says what done means for every request, and
    /// the work goes on until it holds.
    pub default: GoalDefault,
    /// What checks a goal's lines.
    pub check: CheckWith,
    /// Rounds in a row that end on the same failure before the goal changes approach once,
    /// then stops and says why.
    pub stuck_after: u32,
    /// Most rounds a goal goes. Unset, only the goal holding, you, or being stuck ends it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    /// Who the judge is: `other_provider`, or an agent by name.
    pub checker: String,
}

impl Default for GoalSettings {
    fn default() -> Self {
        Self {
            until: None,
            default: GoalDefault::Off,
            check: CheckWith::Auto,
            stuck_after: 3,
            max_rounds: None,
            checker: "other_provider".to_owned(),
        }
    }
}

/// `goal.default`: whether a request gets a goal you didn't set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum GoalDefault {
    #[default]
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "from-request")]
    FromRequest,
}

/// `goal.check`: what decides that a line holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckWith {
    /// A command when the line names one, else the judge.
    #[default]
    Auto,
    /// Commands only, by their exit code.
    Command,
    /// The judge only: a model from another provider than the work reads the result against
    /// each line.
    Judge,
    /// The commands, then the judge on every line.
    Both,
}

impl CheckWith {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckWith::Auto => "auto",
            CheckWith::Command => "command",
            CheckWith::Judge => "judge",
            CheckWith::Both => "both",
        }
    }
}

/// `[roles.<role>] goal`, and a step's `kept_by`: whether the role works each task as a goal,
/// and who keeps it going.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleGoal {
    /// A task is done when its agent reports done and its checks pass, as without a goal.
    #[default]
    Off,
    /// The CLI's own goal, which Codex has: the task's done-when lines are set as its goal and
    /// it keeps going by itself. Argonauts Code still runs the check. A CLI with no goal of its own
    /// is kept by Argonauts Code.
    Cli,
    /// Argonauts Code keeps it: after each try it runs the check and sends back what still fails.
    Argonauts,
}

impl RoleGoal {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleGoal::Off => "off",
            RoleGoal::Cli => "cli",
            RoleGoal::Argonauts => "argonauts",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Serialize)]
    struct File {
        goal: GoalSettings,
    }

    #[test]
    fn a_goal_is_off_until_you_set_one_and_has_no_fixed_number_of_tries() {
        let settings = GoalSettings::default();
        assert_eq!(settings.until, None);
        assert_eq!(settings.default, GoalDefault::Off);
        assert_eq!(settings.check, CheckWith::Auto);
        assert_eq!(settings.stuck_after, 3);
        assert_eq!(settings.max_rounds, None);
    }

    #[test]
    fn the_section_reads_as_the_settings_page_writes_it() {
        let file: File = toml::from_str(
            "[goal]\ndefault = \"from-request\"\ncheck = \"both\"\nstuck_after = 2\nmax_rounds = 5\n",
        )
        .expect("the section reads");
        assert_eq!(file.goal.default, GoalDefault::FromRequest);
        assert_eq!(file.goal.check, CheckWith::Both);
        assert_eq!(file.goal.stuck_after, 2);
        assert_eq!(file.goal.max_rounds, Some(5));
        let written = toml::to_string(&file).expect("written");
        assert!(written.contains("default = \"from-request\""), "{written}");
        // A fixed number of tries is yours to set, so none is written unless you did.
        let unset = toml::to_string(&File {
            goal: GoalSettings::default(),
        })
        .expect("written");
        assert!(!unset.contains("max_rounds"), "{unset}");
    }

    #[test]
    fn a_check_is_one_of_the_four() {
        let file: File = toml::from_str("[goal]\ncheck = \"judge\"\n").expect("reads");
        assert_eq!(file.goal.check, CheckWith::Judge);
        assert!(toml::from_str::<File>("[goal]\ncheck = \"vibes\"\n").is_err());
    }
}
