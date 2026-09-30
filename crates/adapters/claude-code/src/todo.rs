//! Rebuilds the agent's to-do list from `TodoWrite` and the Task tools.

use argonauts_core::{AgentEvent, TodoItem, TodoStatus};
use serde_json::Value;

use crate::json::{str_at, string_at};

/// Task tool entries keep their id so later updates can find them.
#[derive(Debug, Default)]
pub(crate) struct TodoTracker {
    entries: Vec<Entry>,
}

#[derive(Debug)]
struct Entry {
    task_id: Option<String>,
    item: TodoItem,
}

impl TodoTracker {
    /// `output` is the tool's structured result, which `TaskCreate` needs for the new task's id.
    pub(crate) fn apply(
        &mut self,
        tool: &str,
        input: &Value,
        output: Option<&Value>,
    ) -> Option<AgentEvent> {
        let changed = match tool {
            "TodoWrite" => self.replace(input),
            "TaskCreate" => self.create(input, output),
            "TaskUpdate" => self.update(input),
            _ => false,
        };
        changed.then(|| AgentEvent::Todo {
            items: self.entries.iter().map(|e| e.item.clone()).collect(),
        })
    }

    fn replace(&mut self, input: &Value) -> bool {
        let Some(todos) = input.get("todos").and_then(Value::as_array) else {
            return false;
        };
        self.entries = todos
            .iter()
            .filter_map(|todo| {
                Some(Entry {
                    task_id: None,
                    item: TodoItem {
                        text: string_at(todo, "content")?,
                        status: status_or_pending(str_at(todo, "status")),
                    },
                })
            })
            .collect();
        true
    }

    fn create(&mut self, input: &Value, output: Option<&Value>) -> bool {
        let Some(subject) = string_at(input, "subject") else {
            return false;
        };
        let task_id = output
            .and_then(|o| o.get("task"))
            .and_then(|task| string_at(task, "id"));
        self.entries.push(Entry {
            task_id,
            item: TodoItem {
                text: subject,
                status: TodoStatus::Pending,
            },
        });
        true
    }

    fn update(&mut self, input: &Value) -> bool {
        let Some(id) = str_at(input, "taskId") else {
            return false;
        };
        let Some(index) = self
            .entries
            .iter()
            .position(|e| e.task_id.as_deref() == Some(id))
        else {
            return false;
        };
        let status = str_at(input, "status");
        if status == Some("deleted") {
            self.entries.remove(index);
            return true;
        }
        let item = &mut self.entries[index].item;
        if let Some(status) = status.and_then(parse_status) {
            item.status = status;
        }
        if let Some(subject) = string_at(input, "subject") {
            item.text = subject;
        }
        true
    }
}

fn parse_status(status: &str) -> Option<TodoStatus> {
    match status {
        "pending" => Some(TodoStatus::Pending),
        "in_progress" => Some(TodoStatus::InProgress),
        "completed" => Some(TodoStatus::Completed),
        _ => None,
    }
}

fn status_or_pending(status: Option<&str>) -> TodoStatus {
    status.and_then(parse_status).unwrap_or(TodoStatus::Pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn items(event: Option<AgentEvent>) -> Vec<(String, TodoStatus)> {
        match event {
            Some(AgentEvent::Todo { items }) => {
                items.into_iter().map(|i| (i.text, i.status)).collect()
            }
            other => panic!("expected a todo event, got {other:?}"),
        }
    }

    #[test]
    fn todo_write_replaces_the_list() {
        let mut todos = TodoTracker::default();
        let input = json!({"todos": [
            {"content": "Write tests", "status": "in_progress", "activeForm": "Writing tests"},
            {"content": "Ship", "status": "odd"},
            {"status": "pending"}
        ]});
        assert_eq!(
            items(todos.apply("TodoWrite", &input, None)),
            [
                ("Write tests".to_owned(), TodoStatus::InProgress),
                ("Ship".to_owned(), TodoStatus::Pending)
            ]
        );
        assert!(todos.apply("TodoWrite", &json!({}), None).is_none());
    }

    #[test]
    fn task_tools_create_update_and_delete() {
        let mut todos = TodoTracker::default();
        let created = todos.apply(
            "TaskCreate",
            &json!({"subject": "Fix bug", "description": "..."}),
            Some(&json!({"task": {"id": "1", "subject": "Fix bug"}})),
        );
        assert_eq!(
            items(created),
            [("Fix bug".to_owned(), TodoStatus::Pending)]
        );
        todos.apply(
            "TaskCreate",
            &json!({"subject": "Add test"}),
            Some(&json!({"task": {"id": "2"}})),
        );
        let updated = todos.apply(
            "TaskUpdate",
            &json!({"taskId": "1", "status": "completed", "subject": "Fix the bug"}),
            None,
        );
        assert_eq!(
            items(updated),
            [
                ("Fix the bug".to_owned(), TodoStatus::Completed),
                ("Add test".to_owned(), TodoStatus::Pending)
            ]
        );
        let deleted = todos.apply(
            "TaskUpdate",
            &json!({"taskId": "2", "status": "deleted"}),
            None,
        );
        assert_eq!(
            items(deleted),
            [("Fix the bug".to_owned(), TodoStatus::Completed)]
        );
    }

    #[test]
    fn unrelated_tools_and_unknown_tasks_change_nothing() {
        let mut todos = TodoTracker::default();
        assert!(
            todos
                .apply("Bash", &json!({"command": "ls"}), None)
                .is_none()
        );
        assert!(
            todos
                .apply(
                    "TaskUpdate",
                    &json!({"taskId": "9", "status": "completed"}),
                    None
                )
                .is_none()
        );
    }
}
