//! The tasks Claude Code runs in the background: a command send now moved there, a subagent or
//! a workflow that runs on its own. They outlive the turn. Once its input closes, Claude Code
//! gives them a while longer and then stops what still runs, so the session keeps its input
//! open until each one has ended.

use std::collections::BTreeSet;

use argonauts_core::AgentEvent;
use serde_json::Value;

use crate::json::{array_at, bool_at, str_at};

#[derive(Debug, Default)]
pub struct BackgroundTracker {
    /// In the background, and not yet reported to the model as ended.
    running: BTreeSet<String>,
}

impl BackgroundTracker {
    /// Reads a `system` line, and reports the list when it changed.
    pub fn observe(&mut self, subtype: &str, line: &Value) -> Option<AgentEvent> {
        let changed = match subtype {
            // A task leaves this list before its notification, and the notification is what
            // starts the turn that tells the model. So a task stays until that comes.
            "background_tasks_changed" => {
                let listed = array_at(line, "tasks")
                    .iter()
                    .filter_map(|task| str_at(task, "task_id"))
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let before = self.running.len();
                self.running.extend(listed);
                self.running.len() != before
            }
            "task_started" if bool_at(line, "is_backgrounded") || is_workflow(line) => {
                self.insert(line)
            }
            "task_updated"
                if line
                    .get("patch")
                    .is_some_and(|patch| bool_at(patch, "is_backgrounded")) =>
            {
                self.insert(line)
            }
            // A paused workflow is marked as told and never gets a notification, so it leaves
            // here, and no turn follows it either.
            "task_updated"
                if line.get("patch").and_then(|patch| str_at(patch, "status")) == Some(PAUSED) =>
            {
                let paused = str_at(line, "task_id").is_some_and(|task| self.running.remove(task));
                return paused.then(|| self.event(false));
            }
            "task_notification" => {
                let ended = str_at(line, "task_id").is_some_and(|task| self.running.remove(task));
                // Claude Code marks a workflow or an agent it stopped as already told, so no
                // turn follows it. A command it stops at its deadline or for memory is still
                // told, in a turn that comes after the session's input closed, and the
                // session's end takes that turn's result as it comes.
                let told = str_at(line, "status") != Some(STOPPED);
                return ended.then(|| self.event(told));
            }
            _ => false,
        };
        changed.then(|| self.event(false))
    }

    fn insert(&mut self, line: &Value) -> bool {
        str_at(line, "task_id").is_some_and(|task| self.running.insert(task.to_owned()))
    }

    fn event(&self, ended: bool) -> AgentEvent {
        AgentEvent::BackgroundTasks {
            running: self.running.iter().cloned().collect(),
            ended,
        }
    }
}

/// A workflow always runs in the background ("Workflow launched in background"), and its
/// `task_started` carries no `is_backgrounded` (2.1.295).
fn is_workflow(line: &Value) -> bool {
    str_at(line, "task_type") == Some(WORKFLOW_TASK)
}

const WORKFLOW_TASK: &str = "local_workflow";
/// A task stopped by its session, the user, a spend limit, or Claude Code's own deadline or
/// memory watch, rather than one that ended.
const STOPPED: &str = "stopped";
const PAUSED: &str = "paused";

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn running(event: Option<AgentEvent>) -> (Vec<String>, bool) {
        match event {
            Some(AgentEvent::BackgroundTasks { running, ended }) => (running, ended),
            other => panic!("expected the list, got {other:?}"),
        }
    }

    #[test]
    fn a_task_stays_listed_until_its_notification() {
        let mut tracker = BackgroundTracker::default();
        let listed = json!({"tasks": [{"task_id": "b1", "task_type": "local_bash"}]});
        assert_eq!(
            running(tracker.observe("background_tasks_changed", &listed)),
            (vec!["b1".to_owned()], false)
        );
        // Moved there: already listed, so nothing changed.
        let moved = json!({"task_id": "b1", "patch": {"is_backgrounded": true}});
        assert_eq!(tracker.observe("task_updated", &moved), None);
        // Claude Code empties its list first, and the notification comes after.
        let emptied = json!({"tasks": []});
        assert_eq!(tracker.observe("background_tasks_changed", &emptied), None);
        let done = json!({"task_id": "b1", "patch": {"status": "completed"}});
        assert_eq!(tracker.observe("task_updated", &done), None);
        let notified = json!({"task_id": "b1", "status": "completed"});
        assert_eq!(
            running(tracker.observe("task_notification", &notified)),
            (Vec::new(), true)
        );
    }

    #[test]
    fn a_foreground_task_and_one_from_an_earlier_process_arent_listed() {
        let mut tracker = BackgroundTracker::default();
        let foreground = json!({"task_id": "b2", "is_backgrounded": false});
        assert_eq!(tracker.observe("task_started", &foreground), None);
        let earlier = json!({"task_id": "gone", "status": "stopped"});
        assert_eq!(tracker.observe("task_notification", &earlier), None);
        let agent = json!({"task_id": "a1", "is_backgrounded": true});
        assert_eq!(
            running(tracker.observe("task_started", &agent)),
            (vec!["a1".to_owned()], false)
        );
    }

    /// Until a workflow reports, the session's input stays open for the turn its report
    /// starts, so the answer that counts is the one given after it.
    #[test]
    fn a_workflow_stays_listed_until_it_reports() {
        let mut tracker = BackgroundTracker::default();
        let started = json!({
            "task_id": "w1",
            "task_type": "local_workflow",
            "workflow_name": "add-due-dates",
        });
        assert_eq!(
            running(tracker.observe("task_started", &started)),
            (vec!["w1".to_owned()], false)
        );
        let notified = json!({"task_id": "w1", "status": "completed"});
        assert_eq!(
            running(tracker.observe("task_notification", &notified)),
            (Vec::new(), true)
        );
    }

    /// A workflow Claude Code stops, as its spend limit does, is never told to the model, so the
    /// session waits for no turn after it.
    #[test]
    fn a_stopped_workflow_starts_no_turn() {
        let mut tracker = BackgroundTracker::default();
        let started = json!({
            "task_id": "w1",
            "task_type": "local_workflow",
            "workflow_name": "fix-money-rounding",
        });
        assert_eq!(
            running(tracker.observe("task_started", &started)),
            (vec!["w1".to_owned()], false)
        );
        let killed = json!({"task_id": "w1", "patch": {"status": "killed", "end_time": 1}});
        assert_eq!(tracker.observe("task_updated", &killed), None);
        let notified = json!({"task_id": "w1", "status": "stopped"});
        assert_eq!(
            running(tracker.observe("task_notification", &notified)),
            (Vec::new(), false)
        );
    }

    #[test]
    fn a_paused_workflow_leaves_the_list_and_starts_no_turn() {
        let mut tracker = BackgroundTracker::default();
        let started = json!({"task_id": "w1", "task_type": "local_workflow"});
        assert_eq!(
            running(tracker.observe("task_started", &started)),
            (vec!["w1".to_owned()], false)
        );
        let paused = json!({"task_id": "w1", "patch": {"status": "paused"}});
        assert_eq!(
            running(tracker.observe("task_updated", &paused)),
            (Vec::new(), false)
        );
        // Resumed, it is a workflow started again.
        assert_eq!(
            running(tracker.observe("task_started", &started)),
            (vec!["w1".to_owned()], false)
        );
    }
}
