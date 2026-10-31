//! What a council's agents gave, read from their answers, for the row each one ends with in
//! the conversation:
//!
//! ```text
//! ● propose:p2 · GPT 6 Luna on Codex
//!   ⎿  Done · 3 tool uses · proposal ready · 52s · 11k tokens
//!
//! ● challenge:p1 · Haiku 5.5 on Claude Code
//!   ⎿  Done · argued against p2 and p3 · 3 points · 24s · 8.1k tokens
//!
//! ● decide:plan · Haiku 5.5 on Claude Code
//!   ⎿  Chose p2, with p3's whole-second Retry-After · 1 task: per-key-rate-limit · 11s · 43.8k tokens
//! ```
//!
//! A challenge names the proposals it argues against in `against` and makes its `points`; the
//! judge that decides names the proposal it took in `chose`, says why in `summary` and hands on
//! its `tasks`. Phases are named for each request, so a proposal is told by what its agent is: a
//! worker of a council that only reads and works on no task.

use argonauts_core::base::names::RoleName;
use serde_json::Value;

/// What the agent gave, when it is one of a council's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Gave {
    Proposal,
    Challenge { against: Vec<String>, points: usize },
    Choice { said: String, tasks: Vec<String> },
}

/// Who the agent is, as its start said.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Who {
    pub role: Option<RoleName>,
    pub reads_only: bool,
    pub has_task: bool,
    pub in_council: bool,
}

/// What `answer` gave, or `None` for an agent whose row keeps the usual words.
pub(super) fn of(who: Who, answer: Option<&Value>) -> Option<Gave> {
    // A verdict says itself, on a reviewer's or a sign-off's row.
    if answer.is_some_and(|answer| answer.get("accept").is_some_and(Value::is_boolean)) {
        return None;
    }
    if let Some(chose) = answer.and_then(|answer| text(answer, "chose")) {
        let said = answer
            .and_then(|answer| text(answer, "summary"))
            .unwrap_or_else(|| format!("Chose {chose}"));
        let tasks = answer.map(task_names).unwrap_or_default();
        return Some(Gave::Choice { said, tasks });
    }
    let worker = who.role == Some(RoleName::Worker);
    if !(worker && who.reads_only && !who.has_task && who.in_council) {
        return None;
    }
    let against: Vec<String> = answer
        .and_then(|answer| answer.get("against"))
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if against.is_empty() {
        return Some(Gave::Proposal);
    }
    let points = answer
        .and_then(|answer| answer.get("points"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    Some(Gave::Challenge { against, points })
}

impl Gave {
    /// The word that takes the report's place on a proposal's Done line.
    pub fn report_word(&self) -> Option<&'static str> {
        matches!(self, Self::Proposal).then_some("proposal ready")
    }

    /// The whole ⎿ line of a challenge or a choice, given its tool uses and how long it took
    /// with what it spent, such as `24s · 8.1k tokens`. `None` for a proposal, whose Done line
    /// is the usual one with its own word.
    pub fn line(&self, uses: usize, took: &str) -> Option<String> {
        match self {
            Self::Proposal => None,
            Self::Challenge { against, points } => {
                let mut parts = vec!["Done".to_owned()];
                // A challenge reads the proposals it was given, so a count of none says nothing.
                match uses {
                    0 => {}
                    1 => parts.push("1 tool use".to_owned()),
                    uses => parts.push(format!("{uses} tool uses")),
                }
                parts.push(format!("argued against {}", and_list(against)));
                parts.push(match points {
                    1 => "1 point".to_owned(),
                    points => format!("{points} points"),
                });
                parts.push(took.to_owned());
                Some(parts.join(" · "))
            }
            Self::Choice { said, tasks } => {
                let mut parts = vec![said.trim_end_matches('.').to_owned()];
                match tasks.len() {
                    0 => {}
                    1 => parts.push(format!("1 task: {}", tasks[0])),
                    count => parts.push(format!("{count} tasks: {}", tasks.join(", "))),
                }
                parts.push(took.to_owned());
                Some(parts.join(" · "))
            }
        }
    }
}

fn text(answer: &Value, key: &str) -> Option<String> {
    let text = answer.get(key)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The names of the tasks a choice hands on: each a name, or an object with one.
fn task_names(answer: &Value) -> Vec<String> {
    let Some(tasks) = answer.get("tasks").and_then(Value::as_array) else {
        return Vec::new();
    };
    tasks
        .iter()
        .filter_map(|task| match task {
            Value::String(name) => Some(name.clone()),
            Value::Object(_) => ["task", "name", "id", "title"]
                .iter()
                .find_map(|key| text(task, key)),
            _ => None,
        })
        .collect()
}

/// `p2`, `p2 and p3`, `p1, p2 and p3`.
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn council_worker() -> Who {
        Who {
            role: Some(RoleName::Worker),
            reads_only: true,
            has_task: false,
            in_council: true,
        }
    }

    #[test]
    fn a_councils_reading_worker_proposes_or_challenges() {
        assert_eq!(of(council_worker(), None), Some(Gave::Proposal));
        let challenge = json!({"against": ["p2", "p3"], "points": ["a", "b", "c"]});
        let gave = of(council_worker(), Some(&challenge)).expect("a challenge");
        assert_eq!(
            gave.line(0, "24s · 8.1k tokens").as_deref(),
            Some("Done · argued against p2 and p3 · 3 points · 24s · 8.1k tokens")
        );
        // A worker that writes, or one of another workflow, keeps the usual words.
        let builder = Who {
            reads_only: false,
            ..council_worker()
        };
        assert_eq!(of(builder, None), None);
        let elsewhere = Who {
            in_council: false,
            ..council_worker()
        };
        assert_eq!(of(elsewhere, None), None);
    }

    #[test]
    fn a_choice_says_what_it_took_and_the_tasks_it_hands_on() {
        let judge = Who {
            role: Some(RoleName::Judge),
            ..Who::default()
        };
        let answer = json!({
            "chose": "p2",
            "summary": "Chose p2, with p3's whole-second Retry-After.",
            "tasks": [{"task": "per-key-rate-limit", "title": "Per-key buckets"}],
        });
        let gave = of(judge, Some(&answer)).expect("a choice");
        assert_eq!(
            gave.line(7, "11s · 43.8k tokens").as_deref(),
            Some(
                "Chose p2, with p3's whole-second Retry-After · 1 task: per-key-rate-limit · 11s · 43.8k tokens"
            )
        );
        // A verdict is no choice.
        assert_eq!(
            of(judge, Some(&json!({"accept": true, "chose": "p1"}))),
            None
        );
    }
}
