//! A task's worker: the report it ends with, and fixing a merge conflict. What it may touch is
//! its frame, `frames/worker.md`.

pub const BUILD: &str = argonauts_core::template!("prompts/task/report.md");
pub const RESOLVE_CONFLICT: &str = argonauts_core::template!("prompts/task/resolve-conflict.md");

/// What the report's `status` line says on autopilot, where nobody answers for the person.
const STATUS_ALONE: &str = "- `status`: `done` when the task is complete and its tests pass; \
                            `blocked` only when you can't do the work at all without an answer, \
                            which the main agent gives before you carry on; `needs_decision` when \
                            a choice outside your task is needed; `failed` when you couldn't do it.";
/// And its `questions` line.
const QUESTIONS_ALONE: &str = "- `questions`: for another task's worker, or the one thing that \
                               blocks you. Nobody else is asked on autopilot: where you would \
                               ask, decide, carry on, and list what you assumed in `concerns`, \
                               each on a line starting with \"Assumed:\".";

/// The report's instructions on autopilot: the two lines that offer a question say to decide
/// and write down what was assumed. A template of yours without those lines reads as written.
pub fn report_alone(template: &str) -> String {
    let mut text: String = template
        .lines()
        .map(|line| {
            if line.starts_with("- `status`:") {
                STATUS_ALONE
            } else if line.starts_with("- `questions`:") {
                QUESTIONS_ALONE
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if template.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_written() {
        for template in [BUILD, RESOLVE_CONFLICT] {
            assert!(!template.contains("Not written yet"));
            assert!(!template.contains('\u{2014}'), "no em dashes");
        }
    }

    #[test]
    fn on_autopilot_the_report_offers_no_question_for_the_person() {
        assert!(BUILD.contains("or for the user"));
        assert!(BUILD.contains("when you need an answer first"));
        let alone = report_alone(BUILD);
        assert!(!alone.contains("for the user"), "{alone}");
        assert!(!alone.contains("when you need an answer first"), "{alone}");
        assert!(alone.contains("list what you assumed in `concerns`"));
        // Every other line reads as it did.
        let kept = |text: &str| -> Vec<String> {
            text.lines()
                .filter(|line| {
                    !line.starts_with("- `status`") && !line.starts_with("- `questions`")
                })
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(kept(&alone), kept(BUILD));
        assert_eq!(alone.lines().count(), BUILD.lines().count());
    }
}
