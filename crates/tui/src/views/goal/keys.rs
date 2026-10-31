//! What the keys on one agent's goal do, in the workflow view and on the opened agent's Goal
//! tab: `p` holds it or picks it up again, `c` runs its checks now, and `e` rewrites its lines.
//!
//! The screen only draws goals, so each key is a word to whoever keeps them. A key is offered
//! only where it can act: on a goal that is one task's, and isn't over.

use argonauts_core::RunId;
use argonauts_core::app::UiCommand;
use argonauts_core::app::agent_goal::{AgentGoal, GoalState};
use argonauts_core::runs::events::goal::GoalAction;

/// Between the lines of a goal written on one line, which is where a condition is split too.
const BETWEEN: &str = "; ";

/// A key on a goal: the hint that offers it, and what it sends.
pub struct GoalKey {
    pub hint: &'static str,
    pub command: UiCommand,
}

pub const EDIT_HINT: &str = "e edit its lines";

fn send(run: &RunId, action: GoalAction) -> UiCommand {
    UiCommand::Goal {
        run: Some(run.clone()),
        action,
    }
}

/// `p` holds a goal that is going and picks a held one up again.
pub fn hold(run: &RunId, goal: &AgentGoal) -> Option<GoalKey> {
    let task = Some(goal.task.clone()?);
    let (hint, action) = match goal.state {
        GoalState::Running => ("p pause the goal", GoalAction::Pause { task }),
        GoalState::Paused => ("p resume the goal", GoalAction::Resume { task }),
        GoalState::Met | GoalState::Stuck | GoalState::Cleared => return None,
    };
    Some(GoalKey {
        hint,
        command: send(run, action),
    })
}

/// `c` runs the checks of a goal that is going, beside the try under way. Not while they run.
pub fn check_now(run: &RunId, goal: &AgentGoal) -> Option<GoalKey> {
    let task = Some(goal.task.clone()?);
    (goal.state == GoalState::Running && !goal.checking).then(|| GoalKey {
        hint: "c run the checks now",
        command: send(run, GoalAction::CheckNow { task }),
    })
}

/// Whether `e` can rewrite the goal's lines: while it is going or held.
pub fn can_edit(goal: &AgentGoal) -> bool {
    goal.task.is_some() && matches!(goal.state, GoalState::Running | GoalState::Paused)
}

/// The goal's lines on one line, as `e` opens them to be changed.
pub fn lines_as_typed(goal: &AgentGoal) -> String {
    let lines: Vec<&str> = goal.lines.iter().map(|line| line.text.trim()).collect();
    lines.join(BETWEEN)
}

/// What the line you typed sends: the goal's lines rewritten, one for each part between `;`.
/// A line with nothing in it is refused, in words for the notice slot.
pub fn edit(run: &RunId, goal: &AgentGoal, typed: &str) -> Result<UiCommand, String> {
    let lines: Vec<String> = typed
        .split(';')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if lines.is_empty() {
        return Err("A goal needs at least one line, so its lines stay as they were".to_owned());
    }
    let task = goal.task.clone();
    Ok(send(run, GoalAction::EditLines { task, lines }))
}

#[cfg(test)]
mod tests {
    use argonauts_core::TaskId;

    use super::*;
    use crate::views::goal::panel::fixture::build_cli;

    fn run() -> RunId {
        RunId::new("run-01").unwrap()
    }

    fn goal(state: GoalState) -> AgentGoal {
        let mut goal = build_cli("s-002");
        goal.task = Some(TaskId::new("task-002").unwrap());
        goal.state = state;
        goal
    }

    fn action(key: Option<GoalKey>) -> Option<(&'static str, GoalAction)> {
        let key = key?;
        match key.command {
            UiCommand::Goal { run: on, action } => {
                assert_eq!(on, Some(run()));
                Some((key.hint, action))
            }
            other => panic!("a goal's key sends a goal's command: {other:?}"),
        }
    }

    #[test]
    fn p_pauses_a_goal_that_is_going_and_resumes_a_held_one() {
        let task = Some(TaskId::new("task-002").unwrap());
        assert_eq!(
            action(hold(&run(), &goal(GoalState::Running))),
            Some(("p pause the goal", GoalAction::Pause { task: task.clone() }))
        );
        assert_eq!(
            action(hold(&run(), &goal(GoalState::Paused))),
            Some(("p resume the goal", GoalAction::Resume { task }))
        );
        for over in [GoalState::Met, GoalState::Stuck, GoalState::Cleared] {
            assert!(hold(&run(), &goal(over)).is_none(), "{over:?}");
        }
    }

    #[test]
    fn c_runs_the_checks_of_a_goal_that_is_going_and_not_twice_at_once() {
        let task = Some(TaskId::new("task-002").unwrap());
        assert_eq!(
            action(check_now(&run(), &goal(GoalState::Running))),
            Some(("c run the checks now", GoalAction::CheckNow { task }))
        );
        let mut checking = goal(GoalState::Running);
        checking.checking = true;
        assert!(check_now(&run(), &checking).is_none());
        assert!(check_now(&run(), &goal(GoalState::Paused)).is_none());
    }

    /// A goal that is no one task's has nothing these keys can name.
    #[test]
    fn a_goal_of_no_task_takes_no_key() {
        let mut goal = goal(GoalState::Running);
        goal.task = None;
        assert!(hold(&run(), &goal).is_none());
        assert!(check_now(&run(), &goal).is_none());
        assert!(!can_edit(&goal));
    }

    #[test]
    fn e_rewrites_the_lines_one_for_each_part_between_semicolons() {
        let goal = goal(GoalState::Running);
        assert!(can_edit(&goal) && can_edit(&self::goal(GoalState::Paused)));
        assert!(!can_edit(&self::goal(GoalState::Met)));
        assert_eq!(
            lines_as_typed(&goal),
            "tasks overdue lists tasks due before today; tasks add --due rejects a date that \
             isn't ISO; every test in tests/overdue.rs passes"
        );
        let sent = edit(&run(), &goal, " the tests pass ;; the CSV has a header; ").unwrap();
        assert_eq!(
            sent,
            UiCommand::Goal {
                run: Some(run()),
                action: GoalAction::EditLines {
                    task: goal.task.clone(),
                    lines: vec!["the tests pass".into(), "the CSV has a header".into()],
                },
            }
        );
        assert!(edit(&run(), &goal, " ; ").is_err());
    }
}
