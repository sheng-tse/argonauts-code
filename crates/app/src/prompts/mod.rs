//! Prompts for the lead's steps in the app: the triage estimate, a chat answer, and the chat's
//! own jobs (a summary to compact to, a name, a review). Each keeps
//! the agent read-only, and quiet about it unless you ask for a change, gives the schema, and
//! asks for the answer text first so it streams. The workflow design runs inside a run, so its prompt lives with the engine.

use argonauts_core::steps::intake::{ChatAnswer, ChatAnswerInput, IntakeInput, TriageEstimate};
use argonauts_core::steps::{Step, output_schema};

const TRIAGE_ESTIMATE: &str = include_str!("triage_estimate.md");
const CHAT_ANSWER: &str = include_str!("chat_answer.md");
const COMPACT: &str = include_str!("compact.md");
const TITLE: &str = include_str!("title.md");
const REVIEW: &str = include_str!("review.md");

pub(crate) fn triage_estimate(input: &IntakeInput) -> String {
    let uncommitted = input
        .uncommitted_note
        .as_ref()
        .map(|note| format!("In the working tree: {note}."))
        .unwrap_or_default();
    fill(
        TRIAGE_ESTIMATE,
        &[
            ("schema", schema::<TriageEstimate>()),
            ("uncommitted", uncommitted),
            ("request", input.request.trim().to_owned()),
        ],
    )
}

pub(crate) fn chat_answer(input: &ChatAnswerInput) -> String {
    let run_status = input
        .run_status
        .as_ref()
        .map(|status| format!("A run is going. Its status now:\n\n{}", status.trim()))
        .unwrap_or_default();
    fill(
        CHAT_ANSWER,
        &[
            ("run_status", run_status),
            ("schema", schema::<ChatAnswer>()),
            ("question", input.question.trim().to_owned()),
        ],
    )
}

/// `/compact`: a summary the next session starts from.
pub(crate) fn compact(instructions: Option<&str>) -> String {
    let instructions = instructions
        .map(|text| format!("Also, as you were asked: {}", text.trim()))
        .unwrap_or_default();
    fill(
        COMPACT,
        &[
            ("instructions", instructions),
            ("schema", schema::<ChatAnswer>()),
        ],
    )
}

/// `/rename` with no name: the lead names the conversation.
pub(crate) fn title() -> String {
    fill(TITLE, &[("schema", schema::<ChatAnswer>())])
}

/// What `/review` and `/security-review` look at.
pub(crate) struct ReviewInput<'a> {
    pub security: bool,
    /// Such as "the uncommitted changes" or "the branch feature-x".
    pub target: &'a str,
    /// The diff, or how to read it, such as for a pull request.
    pub changes: &'a str,
    /// The reviewer comes from another company than the main agent.
    pub other_company: bool,
}

pub(crate) fn review(input: &ReviewInput<'_>) -> String {
    let focus = if input.security {
        "Look for security problems only: injection, untrusted input handled unsafely, secrets \
         in code or logs, broken access checks, unsafe file and process handling, and risky \
         dependencies. Rate each one high, medium, or low."
    } else {
        "Look for bugs first: wrong behavior, missed edge cases, races, and broken error \
         handling. Then missing tests, and code that is harder to read than it needs to be."
    };
    let reviewer = if input.other_company {
        ", as an agent from another company than the one that wrote them"
    } else {
        ""
    };
    fill(
        REVIEW,
        &[
            ("reviewer", reviewer.to_owned()),
            ("focus", focus.to_owned()),
            ("schema", schema::<ChatAnswer>()),
            ("target", input.target.to_owned()),
            ("changes", input.changes.trim().to_owned()),
        ],
    )
}

fn schema<S: Step>() -> String {
    let schema = output_schema::<S>();
    serde_json::to_string_pretty(&schema).unwrap_or_else(|_| schema.to_string())
}

/// Replaces each `{{name}}` in a single pass, so text inside a value is never read as a
/// placeholder. A placeholder alone on its line with an empty value takes the line with it.
fn fill(template: &str, values: &[(&str, String)]) -> String {
    let value = |name: &str| {
        values
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    };
    let kept: Vec<&str> = template
        .lines()
        .filter(|line| {
            let name = line
                .trim()
                .strip_prefix("{{")
                .and_then(|rest| rest.strip_suffix("}}"));
            !name.and_then(value).is_some_and(str::is_empty)
        })
        .collect();
    let template = squeeze_blank_lines(&kept);
    let mut out = String::with_capacity(template.len() + 1024);
    let mut rest = template.as_str();
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        match value(&after[..end]) {
            Some(filled) => out.push_str(filled),
            None => out.push_str(&rest[start..start + end + 4]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    let mut out = out.trim_end().to_owned();
    out.push('\n');
    out
}

fn squeeze_blank_lines(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut after_blank = false;
    for line in lines {
        let blank = line.trim().is_empty();
        if blank && after_blank {
            continue;
        }
        after_blank = blank;
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent keeps the rule, and brings it up only when asked for a change, so a greeting
    /// isn't answered with what the chat can't do.
    fn read_only_and_quiet_about_it(prompt: &str) {
        assert!(
            prompt.contains("change nothing and run nothing that writes"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Keep the read-only rule to yourself"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Bring it up only when the")
                && prompt.contains("asks you to change something"),
            "{prompt}"
        );
        assert!(!prompt.contains("This session is read-only"), "{prompt}");
    }

    #[test]
    fn the_triage_prompt_is_read_only_with_the_schema_and_the_request() {
        let prompt = triage_estimate(&IntakeInput {
            request: "  add rate limiting {{schema}} ".to_owned(),
            uncommitted_note: Some("2 uncommitted files are not included".to_owned()),
        });
        read_only_and_quiet_about_it(&prompt);
        assert!(prompt.contains("Argonauts makes the change in a run"));
        assert!(prompt.contains("\"answer_markdown\""), "{prompt}");
        assert!(prompt.contains("\"file_count\""), "{prompt}");
        assert!(prompt.contains("the run's description"), "{prompt}");
        assert!(prompt.contains("In the working tree: 2 uncommitted files are not included."));
        // The request comes last and is never read as a placeholder.
        assert!(
            prompt.ends_with("add rate limiting {{schema}}\n"),
            "{prompt}"
        );
        assert!(prompt.find("First write your answer") < prompt.find("```json"));
        assert!(!prompt.contains("\n\n\n"), "{prompt}");
        let without = triage_estimate(&IntakeInput {
            request: "hi".to_owned(),
            uncommitted_note: None,
        });
        assert!(!without.contains("working tree"), "{without}");
    }

    #[test]
    fn the_chat_answer_prompt_carries_the_run_status() {
        let prompt = chat_answer(&ChatAnswerInput {
            question: "why is card-002 slow?".to_owned(),
            run_status: Some("card-002 running · 6m".to_owned()),
        });
        read_only_and_quiet_about_it(&prompt);
        assert!(prompt.contains("sent without `/ask` makes the change in a run"));
        assert!(prompt.contains("A run is going. Its status now:\n\ncard-002 running · 6m"));
        assert!(prompt.ends_with("why is card-002 slow?\n"));
        assert!(prompt.contains("\"answer_markdown\""));
    }

    #[test]
    fn the_compact_prompt_asks_for_a_summary_and_carries_your_instructions() {
        let prompt = compact(Some(" keep the test plan "));
        assert!(prompt.contains("Summarize the conversation"), "{prompt}");
        assert!(
            prompt.contains("as you were asked: keep the test plan\n"),
            "{prompt}"
        );
        assert!(prompt.contains("answer_markdown"), "{prompt}");
        assert!(!compact(None).contains("as you were asked"));
    }

    #[test]
    fn the_title_prompt_wants_the_name_alone() {
        let prompt = title();
        assert!(prompt.contains("2 to 6 words") && prompt.contains("answer_markdown"));
    }

    #[test]
    fn the_review_prompt_names_its_focus_and_the_changes() {
        let input = ReviewInput {
            security: false,
            target: "the uncommitted changes",
            changes: "diff --git a/x b/x\n",
            other_company: true,
        };
        let prompt = review(&input);
        assert!(prompt.contains("Look for bugs first"), "{prompt}");
        assert!(prompt.contains("from another company"), "{prompt}");
        assert!(prompt.contains("What to review: the uncommitted changes"));
        assert!(
            prompt.trim_end().ends_with("diff --git a/x b/x"),
            "{prompt}"
        );
        let security = review(&ReviewInput {
            security: true,
            other_company: false,
            ..input
        });
        assert!(security.contains("security problems only"), "{security}");
        assert!(!security.contains("another company"), "{security}");
    }

    #[test]
    fn an_unknown_placeholder_stays_as_written() {
        assert_eq!(fill("a {{b}} {{c", &[]), "a {{b}} {{c\n");
    }
}
