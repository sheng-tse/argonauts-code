//! The goals the screen knows of, by run and agent. The workflow view carries its own; this is
//! where the opened agent's Goal tab and the row at the bottom find them.
//!
//! Every answer about a workflow or an agent that names goals is remembered here, so the newest
//! word on each goal is the one drawn. A run's own events say when a goal is set and when it
//! stops being pursued, so the row at the bottom counts goals before any view was opened.

use std::collections::BTreeMap;

use argonauts_core::app::agent_goal::AgentGoal;
use argonauts_core::app::views::AgentRow;
use argonauts_core::base::names::SessionKey;
use argonauts_core::runs::events::GoalEvent;
use argonauts_core::runs::events::goal::GoalStatus;
use argonauts_core::{RunId, TaskId};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoalBoard {
    runs: BTreeMap<RunId, Vec<AgentGoal>>,
    /// Where each of a run's goals stands by its own events, under the task it is the goal of.
    /// `None` is the run's own goal, which goes round its whole workflow.
    told: BTreeMap<RunId, BTreeMap<Option<TaskId>, GoalStatus>>,
}

impl GoalBoard {
    /// Takes in what was just reported about `run`'s goals. A goal already known is replaced
    /// by its newer report, and one not named this time stays as it was: an answer about one
    /// workflow says nothing of the agents in another.
    pub fn remember<'a>(&mut self, run: &RunId, goals: impl IntoIterator<Item = &'a AgentGoal>) {
        for goal in goals {
            let known = self.runs.entry(run.clone()).or_default();
            match known.iter_mut().find(|old| old.session == goal.session) {
                Some(old) => *old = goal.clone(),
                None => known.push(goal.clone()),
            }
        }
    }

    /// Takes in an answer about one agent: its goal as it stands, or that it has none now.
    /// Agents their CLI runs itself share its session, so only a goal under this agent's own
    /// label is dropped.
    pub fn remember_agent(&mut self, run: &RunId, agent: &AgentRow, goal: Option<&AgentGoal>) {
        match goal {
            Some(goal) => self.remember(run, [goal]),
            None => {
                if let Some(known) = self.runs.get_mut(run) {
                    known.retain(|goal| {
                        goal.session != agent.session || goal.label.trim() != agent.label.trim()
                    });
                }
            }
        }
    }

    pub fn of(&self, run: &RunId, session: &SessionKey) -> Option<&AgentGoal> {
        self.runs
            .get(run)?
            .iter()
            .find(|goal| goal.session == *session)
    }

    /// Takes in a goal's own event from `run`: one set, or one that stopped or went on.
    pub fn told(&mut self, run: &RunId, event: &GoalEvent) {
        let (task, status) = match event {
            GoalEvent::Set(goal) => (&goal.task, goal.status),
            GoalEvent::StatusChanged { status, task, .. } => (task, *status),
            _ => return,
        };
        self.told
            .entry(run.clone())
            .or_default()
            .insert(task.clone(), status);
    }

    /// How many of `run`'s goals are still being worked on. Its events say it for every goal
    /// it has. Where none came, as for a workflow a CLI runs by itself, the goals the views
    /// were answered with are all that is known.
    pub fn running(&self, run: &RunId) -> usize {
        match self.told.get(run) {
            Some(told) => told
                .values()
                .filter(|status| **status == GoalStatus::Pursuing)
                .count(),
            None => self.runs.get(run).map_or(0, |goals| {
                goals.iter().filter(|goal| goal.is_running()).count()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::agent_goal::{GoalKeeper, GoalState};

    use super::*;

    fn goal(session: &str, state: GoalState) -> AgentGoal {
        AgentGoal {
            session: SessionKey::parse(session).unwrap(),
            label: format!("build:{session}"),
            role: None,
            task: None,
            kept_by: GoalKeeper::Argonauts,
            kept_note: None,
            checked_by: "Argonauts Code".into(),
            state,
            stopped_because: None,
            set_ms_ago: None,
            lines: Vec::new(),
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
    fn a_newer_report_replaces_a_goal_and_leaves_the_others() {
        let run = RunId::new("run-01").unwrap();
        let mut board = GoalBoard::default();
        board.remember(
            &run,
            &[
                goal("s-001", GoalState::Running),
                goal("s-002", GoalState::Running),
            ],
        );
        assert_eq!(board.running(&run), 2);
        board.remember(&run, &[goal("s-001", GoalState::Met)]);
        assert_eq!(board.running(&run), 1);
        let key = SessionKey::parse("s-001").unwrap();
        assert_eq!(board.of(&run, &key).unwrap().state, GoalState::Met);
        assert_eq!(board.running(&RunId::new("run-02").unwrap()), 0);
    }

    fn set(task: Option<&str>) -> GoalEvent {
        use argonauts_core::runs::events::goal::{Criterion, GoalRun};
        use argonauts_core::workflows::goal::{CheckWith, GoalFrom, GoalScope, GoalSpec, KeptBy};

        let spec = GoalSpec::from_lines(
            "the export tests pass",
            vec![Criterion::new("G1", "the export tests pass")],
            CheckWith::Auto,
            GoalFrom::Command,
        );
        let mut goal = GoalRun::new(&spec, GoalScope::Task, KeptBy::Argonauts);
        goal.task = task.map(|task| TaskId::new(task).unwrap());
        GoalEvent::Set(Box::new(goal))
    }

    fn changed(task: Option<&str>, status: GoalStatus) -> GoalEvent {
        GoalEvent::StatusChanged {
            status,
            by: argonauts_core::runs::events::run::Party::ArgonautsCode,
            task: task.map(|task| TaskId::new(task).unwrap()),
            why: None,
        }
    }

    /// A run's own events count its goals, the run's and each task's, with no view opened:
    /// one set is running until its events say it was met, held, cleared or stuck.
    #[test]
    fn a_runs_events_count_its_goals_before_any_view_is_opened() {
        let run = RunId::new("run-01").unwrap();
        let mut board = GoalBoard::default();
        for event in [set(None), set(Some("task-001")), set(Some("task-002"))] {
            board.told(&run, &event);
        }
        assert_eq!(board.running(&run), 3);
        board.told(&run, &changed(Some("task-001"), GoalStatus::Met));
        board.told(&run, &changed(Some("task-002"), GoalStatus::Paused));
        assert_eq!(board.running(&run), 1);
        board.told(&run, &changed(Some("task-002"), GoalStatus::Pursuing));
        assert_eq!(board.running(&run), 2);
        // Set again, as when its worker changed, it is still the one goal.
        board.told(&run, &set(Some("task-002")));
        assert_eq!(board.running(&run), 2);
        board.told(&run, &changed(None, GoalStatus::Cleared));
        assert_eq!(board.running(&run), 1);

        // The events are the count: what a view was answered with adds nothing to it.
        board.remember(
            &run,
            &[
                goal("s-001", GoalState::Running),
                goal("s-002", GoalState::Running),
            ],
        );
        assert_eq!(board.running(&run), 1);
    }

    #[test]
    fn an_agent_said_to_have_no_goal_now_loses_the_one_it_had() {
        let run = RunId::new("run-01").unwrap();
        let (first, second) = (
            SessionKey::parse("s-001").unwrap(),
            SessionKey::parse("s-002").unwrap(),
        );
        let mut board = GoalBoard::default();
        board.remember(
            &run,
            &[
                goal("s-001", GoalState::Running),
                goal("s-002", GoalState::Running),
            ],
        );
        let row = |session: &SessionKey, label: &str| AgentRow {
            session: session.clone(),
            label: label.into(),
            worker: "codex".parse().unwrap(),
            status: argonauts_core::app::views::RowStatus::Running,
            tokens: 0,
            spent: argonauts_core::Spend::default(),
            elapsed_ms: 0,
            subagents: 0,
            context: None,
            fallback_for: None,
            answered: false,
            tries: Vec::new(),
            note: None,
            inner: None,
            left_out: false,
            not_needed: false,
            fallback_why: None,
        };
        // Another agent in the same session keeps its goal.
        board.remember_agent(&run, &row(&first, "build:other"), None);
        assert!(board.of(&run, &first).is_some());
        board.remember_agent(&run, &row(&first, "build:s-001"), None);
        assert!(board.of(&run, &first).is_none());
        assert_eq!(board.running(&run), 1);
        let met = goal("s-002", GoalState::Met);
        board.remember_agent(&run, &row(&second, "build:s-002"), Some(&met));
        assert_eq!(board.running(&run), 0);
    }
}
