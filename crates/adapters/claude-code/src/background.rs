//! The tasks Claude Code runs in the background: a command send now moved there, a subagent or
//! a workflow that runs on its own. They outlive the turn, and Claude Code stops them once its
//! input closes, so the session keeps its input open until each one has ended.

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
            "task_started" if bool_at(line, "is_backgrounded") => self.insert(line),
            "task_updated"
                if line
                    .get("patch")
                    .is_some_and(|patch| bool_at(patch, "is_backgrounded")) =>
            {
                self.insert(line)
            }
            "task_notification" => {
                let ended = str_at(line, "task_id").is_some_and(|task| self.running.remove(task));
                return ended.then(|| self.event(true));
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
}
