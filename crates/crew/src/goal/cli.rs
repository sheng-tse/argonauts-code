//! A CLI's own goal. Codex has one: told to, it sets an objective with its own goal tool and
//! works until it believes the objective is achieved. Checked live on Codex 0.160.0, driven
//! the way Argonauts Code drives it (`codex exec --json`, the prompt on stdin):
//!
//! - It takes the goal, and the goal stays with the session: resumed, it still reads back as
//!   "active" with its objective.
//! - It doesn't go from one turn to the next by itself. Each `codex exec` call runs one turn
//!   and exits, goal active or not, so Argonauts Code resumes the session while Codex says its goal
//!   is still active.
//! - The stream carries no event for the goal or its tool calls. What Codex says about its
//!   goal is only in its messages, so the prompt asks for one last line, `goal: <status> ·
//!   <note>`, read back from its goal tool.
//!
//! Claude Code and Antigravity CLI have no goal of their own, so Argonauts Code keeps theirs.
//! Either way Argonauts Code runs the check itself: a CLI saying "complete" is never enough.

use argonauts_core::display_names::builtin_cli_name;
use argonauts_core::events::goal::{CliGoalSaid, Criterion};
use argonauts_core::names::ModelRef;

/// The CLIs whose own goal Argonauts Code can hand a task to.
const WITH_OWN_GOAL: &[&str] = &["codex"];

/// How often in a row a CLI may end a turn with its goal still active before the check runs
/// anyway. The check decides, so this only bounds a CLI that never says it's done.
pub const MAX_CONTINUES: u32 = 3;

pub fn has_own_goal(worker: &ModelRef) -> bool {
    WITH_OWN_GOAL.contains(&worker.agent.as_str())
}

/// `Codex`, for `Codex's own goal` and `Codex says`.
pub fn name(worker: &ModelRef) -> String {
    builtin_cli_name(&worker.agent).map_or_else(|| worker.agent.clone(), str::to_owned)
}

/// Where a CLI says how far its goal is: in the report a task ends with, or at the end of
/// its answer when it works alone in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Says {
    InReport,
    InAnswer,
}

/// What a fresh session is told when its CLI keeps the goal: set the lines as its own goal,
/// keep going, and say where the goal stands at the end.
pub fn brief(lines: &[Criterion], says: Says) -> String {
    let work = match says {
        Says::InReport => "This task is worked as a goal",
        Says::InAnswer => "This request is worked as a goal",
    };
    let mut text = format!(
        "{work}, kept by your own goal.\n\nSet this as your goal for this session with your own \
         goal tool, then keep going by yourself until it is achieved. All of these must hold:\n"
    );
    for line in lines {
        text.push_str(&format!("- {}\n", line.text));
    }
    text.push_str(&format!(
        "\nArgonauts Code runs the check itself after you say the goal is achieved, and sends back \
         what still fails.\n\n{}",
        say_where_it_stands(says)
    ));
    text
}

/// What the session is told when it ended a turn with its goal still active.
pub fn carry_on(says: Says) -> String {
    format!(
        "Continue working toward your goal until it is achieved.\n\n{}",
        say_where_it_stands(says)
    )
}

/// What follows a failed check for a CLI that keeps its own goal.
pub fn not_achieved(says: Says) -> String {
    format!(
        "Your goal isn't achieved yet, whatever your goal tool says: keep it as your goal, and \
         set it active again if you marked it complete.\n\n{}",
        say_where_it_stands(says)
    )
}

fn say_where_it_stands(says: Says) -> String {
    let place = match says {
        Says::InReport => "End the `summary` of your report",
        Says::InAnswer => "End your answer",
    };
    format!(
        "{place} with one last line in exactly this form, read back from your goal tool:\n\
         goal: <its status> · <one line on where it stands>"
    )
}

const GOAL_LINE: &str = "goal:";

/// What the CLI says about its goal, from the last `goal: <status> · <note>` line of `text`.
pub fn said(worker: &ModelRef, text: &str) -> Option<CliGoalSaid> {
    let line = text.lines().rev().map(str::trim).find(|line| {
        line.get(..GOAL_LINE.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(GOAL_LINE))
    })?;
    let rest = line[GOAL_LINE.len()..].trim();
    let (status, note) = [" · ", " - ", " — ", " – ", ": ", ". "]
        .iter()
        .filter_map(|separator| rest.split_once(separator))
        .min_by_key(|(status, _)| status.len())
        .unwrap_or((rest, ""));
    let status = status.trim().trim_matches('`').to_lowercase();
    if status.is_empty() {
        return None;
    }
    Some(CliGoalSaid {
        cli: worker.agent.clone(),
        status,
        note: note.trim().trim_matches('"').to_owned(),
        turns: None,
    })
}

/// Whether the CLI says it's still working toward the goal, so it's resumed rather than
/// checked.
pub fn still_going(said: &CliGoalSaid) -> bool {
    matches!(
        said.status.as_str(),
        "active" | "pursuing" | "in progress" | "in_progress"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex() -> ModelRef {
        "codex/gpt-5.6-luna".parse().expect("a model")
    }

    #[test]
    fn only_codex_has_a_goal_of_its_own() {
        assert!(has_own_goal(&codex()));
        for other in ["claude-code/haiku", "antigravity-cli", "opencode"] {
            let worker: ModelRef = other.parse().expect("a model");
            assert!(!has_own_goal(&worker), "{other}");
        }
        assert_eq!(name(&codex()), "Codex");
    }

    /// The last message of the live check's working turn, word for word (Codex 0.160.0,
    /// gpt-5.6-luna, 2026-10-07).
    #[test]
    fn what_codex_says_is_read_from_its_last_goal_line() {
        let said = said(
            &codex(),
            "goal: complete · Fixed `add` and empty-list `mean`; all 3 tests pass.",
        )
        .expect("a goal line");
        assert_eq!(said.cli, "codex");
        assert_eq!(said.status, "complete");
        assert_eq!(
            said.note,
            "Fixed `add` and empty-list `mean`; all 3 tests pass."
        );
        assert!(!still_going(&said));
    }

    #[test]
    fn a_goal_still_active_is_carried_on() {
        let summary = "Wrote the command and its tests.\nGoal: Active · one test left: tasks \
                       without a date";
        let said = said(&codex(), summary).expect("a goal line");
        assert_eq!(said.status, "active");
        assert_eq!(said.note, "one test left: tasks without a date");
        assert!(still_going(&said));
        // A report that says nothing about the goal says nothing.
        assert_eq!(super::said(&codex(), "Fixed the date comparison."), None);
        assert_eq!(super::said(&codex(), "goal:"), None);
    }

    #[test]
    fn the_brief_hands_over_every_line_and_asks_where_the_goal_stands() {
        let lines = vec![
            Criterion::new("G1", "tasks overdue lists tasks due before today"),
            Criterion::new("G2", "every test in tests/overdue.rs passes"),
        ];
        let text = brief(&lines, Says::InReport);
        assert!(text.contains("with your own goal tool"));
        assert!(text.contains("- tasks overdue lists tasks due before today\n"));
        assert!(text.contains("- every test in tests/overdue.rs passes\n"));
        assert!(text.contains("End the `summary` of your report with one last line"));
        assert!(text.contains("goal: <its status> · <one line on where it stands>"));
        // Alone in the conversation there's no report, so the line ends the answer.
        let alone = brief(&lines, Says::InAnswer);
        assert!(alone.starts_with("This request is worked as a goal"));
        assert!(alone.contains("End your answer with one last line"));
        assert!(carry_on(Says::InAnswer).starts_with("Continue working toward your goal"));
        assert!(not_achieved(Says::InReport).contains("set it active again"));
    }
}
