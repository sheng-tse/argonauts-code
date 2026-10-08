//! What whoever writes the workflow is told of what you required: the orchestrator writing
//! one, and Claude Code planning its own, which gets it as part of the request.

use super::{Required, RequiredStep, label};

/// Lines for the orchestrator, or anyone else who writes the workflow. `None` when nothing is
/// required.
pub fn for_writer(required: &Required) -> Option<String> {
    let mut parts = Vec::new();
    if !required.steps.is_empty() {
        let mut text = String::from(
            "The user requires these steps in every workflow. Give each a step of its own, by \
             this name, where it says:\n",
        );
        for step in &required.steps {
            text.push_str(&format!("- {}\n", step_line(step)));
        }
        parts.push(text);
    }
    if !required.tasks.is_empty() {
        let mut text = String::from(
            "The user fixed the task list. Plan exactly these tasks, by these names and in this \
             order, with none merged, split or added. You still decide who takes each and how \
             it is reviewed. If you believe a task is missing, say so and ask the user; don't \
             add it.\n",
        );
        for (at, task) in required.tasks.iter().enumerate() {
            text.push_str(&format!("{}. {task}\n", at + 1));
        }
        parts.push(text);
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Lines added to the request when Claude Code runs its own workflow. What it reports is then
/// checked against the same list, by its phases' titles and its agents' labels.
pub fn for_claude_code(required: &Required) -> Option<String> {
    let mut parts = Vec::new();
    if !required.steps.is_empty() {
        let mut text = String::from(
            "Your workflow must have a phase for each of these steps, titled with this name, \
             where it says:\n",
        );
        for step in &required.steps {
            text.push_str(&format!("- {}\n", step_line(step)));
        }
        parts.push(text);
    }
    if !required.tasks.is_empty() {
        let mut text = String::from(
            "Do exactly these tasks, in this order, one agent each, labelled as given. Keep \
             them as they are: don't merge, split or add any. If you believe a task is missing, \
             ask the user instead of adding it.\n",
        );
        for (at, task) in required.tasks.iter().enumerate() {
            text.push_str(&format!("{}. {task} (label: {})\n", at + 1, label(task)));
        }
        parts.push(text);
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// `Benchmark, before sign-off, by the workers: Run the benchmarks.`
pub fn step_line(step: &RequiredStep) -> String {
    let when = step.when.as_deref().unwrap_or("before the sign-off");
    let mut line = format!("{}, {when}, by {}", step.name, step.role.the_agents());
    if let Some(does) = &step.does {
        line.push_str(": ");
        line.push_str(does);
    }
    line
}
