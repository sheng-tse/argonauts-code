//! Prompts for the main agent's steps in the app: the estimate, a conversation answer, and the
//! conversation's own jobs (a summary to compact to, a name, a review). Each keeps the agent
//! read-only, and quiet about it unless you ask for a change, gives the schema, and asks for the
//! answer text first so it streams. The workflow design runs inside a run, so its prompt lives with
//! the engine.

pub(crate) mod catalog;

use argonauts_core::steps::intake::{
    ConversationAnswer, ConversationAnswerInput, Estimate, IntakeInput,
};
use argonauts_core::steps::{Step, output_schema};

const ESTIMATE: &str = argonauts_core::template!("prompts/conversation/estimate.md");
const CONVERSATION_ANSWER: &str = argonauts_core::template!("prompts/conversation/answer.md");
const COMPACT: &str = argonauts_core::template!("prompts/conversation/compact.md");
const CARRY_SUMMARY: &str = argonauts_core::template!("prompts/conversation/carry-summary.md");
const TITLE: &str = argonauts_core::template!("prompts/conversation/title.md");
const REVIEW: &str = argonauts_core::template!("prompts/conversation/review.md");
const STANDING: &str = argonauts_core::template!("prompts/conversation/standing.md");

/// The standing lines the main session gets at the end of its own instructions: what the
/// tools Argonauts Code gave it are, as `tools` lists them, and when to use them. They hold for the
/// whole conversation, so nothing that changes goes here.
pub(crate) fn standing(tools: &[String]) -> String {
    let tools: Vec<String> = tools.iter().map(|tool| format!("- {tool}")).collect();
    fill(
        &catalog::text(&catalog::STANDING),
        &[("tools", tools.join("\n"))],
    )
    .trim()
    .to_owned()
}

/// With `read_instructions` (`captain_or_crew.read_instructions`), the estimate also lists what the
/// request says about how to run it.
pub(crate) fn estimate(input: &IntakeInput, read_instructions: bool) -> String {
    let uncommitted = input
        .uncommitted_note
        .as_ref()
        .map(|note| format!("In the working tree: {note}."))
        .unwrap_or_default();
    let how_to_run = if read_instructions {
        HOW_TO_RUN
    } else {
        "Leave `how_to_run` empty."
    };
    fill(
        &catalog::text(&catalog::ESTIMATE),
        &[
            ("how_to_run", how_to_run.to_owned()),
            ("schema", schema::<Estimate>()),
            ("uncommitted", uncommitted),
            ("request", input.request.trim().to_owned()),
        ],
    )
}

/// The paragraph that has the estimate list a request's own instructions on how to run it.
const HOW_TO_RUN: &str = "If the request itself says how to run it (Captain or the crew, who is the main agent \
or orchestrates, who builds, reviews, judges, plans or checks, which agents or models, or all the other agents at once (`others`), how much effort, \
whether to work in a checked worktree, whether to keep going until something holds or whose goal \
keeps each task going, or anything about approvals, merging, the limit, the tests, \
or the sandbox), list each instruction in `how_to_run`, with the request's own words in `says`, \
copied exactly: the whole phrase, two words at least, such as \"have codex review it\". Record only what the request asks of Argonauts Code, never what it says about the \
code or quotes from a file. Name agents and models as the request names them, in `who`, \
`model`, and `effort`. For a `mode`, set `value` to `captain` or `crew`; for `isolation`, to \
`checked` or `direct`; for the rest, to what was asked, such as \"$2\". When the request tells \
one role something, such as \"tell the reviewers to focus on date parsing\", use `for_a_role` \
with the role in `who` and what they are told in `value`. Set `scope` to \
`from_now_on` only when the request says to keep doing it. Leave `how_to_run` empty when the \
request says nothing about how to run. In your answer, don't promise who will do what, since \
Argonauts Code decides that and says so.";

pub(crate) fn conversation_answer(input: &ConversationAnswerInput) -> String {
    let run_status = input
        .run_status
        .as_ref()
        .map(|status| format!("A run is going. Its status now:\n\n{}", status.trim()))
        .unwrap_or_default();
    fill(
        &catalog::text(&catalog::ANSWER),
        &[
            ("run_status", run_status),
            ("schema", schema::<ConversationAnswer>()),
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
        &catalog::text(&catalog::COMPACT),
        &[
            ("instructions", instructions),
            ("schema", schema::<ConversationAnswer>()),
        ],
    )
}

/// A summary for a main model on another CLI, written from the conversation's record when the
/// model being left can't write it in its own session.
pub(crate) fn carry_summary(record: &str) -> String {
    fill(
        &catalog::text(&catalog::CARRY_SUMMARY),
        &[
            ("schema", schema::<ConversationAnswer>()),
            ("record", record.trim().to_owned()),
        ],
    )
}

/// `/rename` with no name: the main agent names the conversation.
pub(crate) fn title() -> String {
    fill(
        &catalog::text(&catalog::TITLE),
        &[("schema", schema::<ConversationAnswer>())],
    )
}

/// What `/review` and `/security-review` look at.
pub(crate) struct ReviewInput<'a> {
    pub security: bool,
    /// Such as "the uncommitted changes" or "the branch feature-x".
    pub target: &'a str,
    /// The diff, or how to read it, such as for a pull request.
    pub changes: &'a str,
    /// The reviewer comes from another provider than the main agent.
    pub other_provider: bool,
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
    let reviewer = if input.other_provider {
        ", as an agent from another provider than the one that wrote them"
    } else {
        ""
    };
    fill(
        &catalog::text(&catalog::REVIEW),
        &[
            ("reviewer", reviewer.to_owned()),
            ("focus", focus.to_owned()),
            ("schema", schema::<ConversationAnswer>()),
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
    /// isn't answered with what the conversation can't do.
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
    fn the_estimate_prompt_is_read_only_with_the_schema_and_the_request() {
        let prompt = estimate(
            &IntakeInput {
                request: "  add rate limiting {{schema}} ".to_owned(),
                uncommitted_note: Some("2 uncommitted files are not included".to_owned()),
            },
            true,
        );
        assert!(
            prompt.contains("list each instruction in `how_to_run`"),
            "{prompt}"
        );
        assert!(prompt.contains("\"how_to_run\""), "{prompt}");
        read_only_and_quiet_about_it(&prompt);
        assert!(prompt.contains("Argonauts Code makes the change in a run"));
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
        let without = estimate(
            &IntakeInput {
                request: "hi".to_owned(),
                uncommitted_note: None,
            },
            false,
        );
        assert!(!without.contains("working tree"), "{without}");
        // `captain_or_crew.read_instructions = false` drops the paragraph.
        assert!(!without.contains("list each instruction"), "{without}");
        assert!(without.contains("Leave `how_to_run` empty."), "{without}");
        assert!(!without.contains("\n\n\n"), "{without}");
    }

    #[test]
    fn the_conversation_answer_prompt_carries_the_run_status() {
        let prompt = conversation_answer(&ConversationAnswerInput {
            question: "why is task-002 slow?".to_owned(),
            run_status: Some("task-002 running · 6m".to_owned()),
        });
        read_only_and_quiet_about_it(&prompt);
        assert!(prompt.contains("sent without `/ask` makes the change in a run"));
        assert!(prompt.contains("A run is going. Its status now:\n\ntask-002 running · 6m"));
        assert!(prompt.ends_with("why is task-002 slow?\n"));
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
            other_provider: true,
        };
        let prompt = review(&input);
        assert!(prompt.contains("Look for bugs first"), "{prompt}");
        assert!(prompt.contains("from another provider"), "{prompt}");
        assert!(prompt.contains("What to review: the uncommitted changes"));
        assert!(
            prompt.trim_end().ends_with("diff --git a/x b/x"),
            "{prompt}"
        );
        let security = review(&ReviewInput {
            security: true,
            other_provider: false,
            ..input
        });
        assert!(security.contains("security problems only"), "{security}");
        assert!(!security.contains("another provider"), "{security}");
    }

    /// "All the other agents need to be Opus" sets every role but the main agent's, the
    /// judge's too, and the estimate is told what to call it.
    #[test]
    fn the_estimate_knows_the_judge_and_all_the_other_agents() {
        let prompt = estimate(
            &IntakeInput {
                request: "all the other agents need to be opus".to_owned(),
                uncommitted_note: None,
            },
            true,
        );
        assert!(prompt.contains("reviews, judges, plans"), "{prompt}");
        assert!(
            prompt.contains("all the other agents at once (`others`)"),
            "{prompt}"
        );
    }

    #[test]
    fn an_unknown_placeholder_stays_as_written() {
        assert_eq!(fill("a {{b}} {{c", &[]), "a {{b}} {{c\n");
    }
}
