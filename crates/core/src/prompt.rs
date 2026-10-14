//! Prompt text Argonauts Code sends to agents.

use serde_json::Value;

use crate::task::Task;

/// The fixed message Argonauts Code sends when a task's limit is nearly spent.
pub const WRAP_UP_MESSAGE: &str = "Your limit for this task is almost used up. Don't start anything new. \
Finish the step you are on, then end with your final report as one JSON object that matches the report schema.";

/// Renders a Captain task's prompt from the task, the rules, and the report schema.
pub fn render_captain_prompt(task: &Task, report_schema: &Value) -> String {
    let schema =
        serde_json::to_string_pretty(report_schema).unwrap_or_else(|_| report_schema.to_string());
    let scope = if task.owned_paths.is_empty() {
        "You may edit any file in the working directory.".to_owned()
    } else {
        format!(
            "Edit only these paths: {}.",
            task.owned_paths
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    format!(
        "You are working on one task in a git worktree that Argonauts Code created for you.

# Task

{task}

# Rules

- The current working directory is your worktree. Don't change files outside it.
- {scope}
- Don't change anything under `.argonauts/`.
- Don't run git commands that change history or branches, such as commit, push, checkout, reset, rebase, merge, or stash. Argonauts Code commits your changes when you finish.
- Keep the change focused on the task. Run the project's tests if they are quick.
- End your final message with your report: one JSON object that matches the schema below. Use status \"blocked\" or \"needs_decision\" instead of guessing when you can't continue.

# Report schema

```json
{schema}
```
",
        task = task.task.trim(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meter::MeterLimit;
    use crate::task::report_json_schema;

    #[test]
    fn captain_prompt_has_task_rules_and_schema() {
        let task = Task::captain("  Create hello.txt containing hi  ", MeterLimit::None);
        let prompt = render_captain_prompt(&task, &report_json_schema());
        assert!(prompt.contains("# Task\n\nCreate hello.txt containing hi\n"));
        assert!(prompt.contains("any file in the working directory"));
        assert!(prompt.contains("such as commit, push"));
        assert!(prompt.contains("\"needs_decision\""));
        let fenced = prompt
            .split("```json\n")
            .nth(1)
            .and_then(|rest| rest.split("\n```").next())
            .unwrap();
        let schema: Value = serde_json::from_str(fenced).unwrap();
        assert_eq!(schema, report_json_schema());
    }

    #[test]
    fn owned_paths_narrow_the_scope() {
        let mut task = Task::captain("task", MeterLimit::None);
        task.owned_paths = vec!["src/a.rs".into(), "docs/".into()];
        let prompt = render_captain_prompt(&task, &report_json_schema());
        assert!(prompt.contains("Edit only these paths: `src/a.rs`, `docs/`."));
        assert!(!prompt.contains("any file in the working directory"));
    }

    #[test]
    fn wrap_up_message_mentions_the_report() {
        assert!(WRAP_UP_MESSAGE.contains("report"));
        assert!(!WRAP_UP_MESSAGE.contains('\n'));
    }
}
