//! Cards (units of work) and the report an agent returns for one.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::budget::BudgetLimit;
use crate::ids::CardId;

const TITLE_MAX_CHARS: usize = 60;

/// How many `{` positions [`Report::from_text`] tries, counting from the end, so a long
/// transcript full of braces stays cheap.
const MAX_BRACES_TRIED: usize = 200;

/// One unit of work, owned by one agent session in one worktree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    /// Unique within its run.
    pub id: CardId,
    pub title: String,
    pub task: String,
    /// Paths the agent may edit, relative to the worktree. Empty means the whole worktree.
    pub owned_paths: Vec<String>,
    pub budget: BudgetLimit,
}

impl Card {
    /// The single card of a solo run, `card-001`, titled from the task's first line.
    pub fn solo(task: impl Into<String>, budget: BudgetLimit) -> Self {
        let task = task.into();
        Self {
            id: CardId::numbered(1),
            title: title_from_task(&task),
            task,
            owned_paths: Vec::new(),
            budget,
        }
    }
}

/// The first non-empty line of `task`, cut to `TITLE_MAX_CHARS` characters.
pub fn title_from_task(task: &str) -> String {
    let line = task
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= TITLE_MAX_CHARS {
        return line.to_owned();
    }
    let cut: String = line.chars().take(TITLE_MAX_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Done,
    /// The agent can't continue without something it lacks.
    Blocked,
    Failed,
    NeedsDecision,
}

impl ReportStatus {
    /// In schema order.
    pub const ALL: [ReportStatus; 4] = [
        ReportStatus::Done,
        ReportStatus::Blocked,
        ReportStatus::Failed,
        ReportStatus::NeedsDecision,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::NeedsDecision => "needs_decision",
        }
    }
}

/// The structured report that ends every card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub status: ReportStatus,
    pub summary: String,
    /// Relative to the worktree.
    #[serde(default)]
    pub files_changed: Vec<String>,
    /// Test commands with their outcomes.
    #[serde(default)]
    pub tests_run: Vec<String>,
    #[serde(default)]
    pub concerns: Vec<String>,
}

impl Report {
    pub fn from_value(value: &Value) -> Result<Self, serde_json::Error> {
        Self::deserialize(value)
    }

    /// The last valid report in `text`, which may wrap it in a code fence or prose. Used when the
    /// harness couldn't enforce the schema and the report sits in the final message.
    pub fn from_text(text: &str) -> Option<Self> {
        if let Ok(report) = serde_json::from_str::<Report>(text.trim()) {
            return Some(report);
        }
        text.rmatch_indices('{')
            .take(MAX_BRACES_TRIED)
            .find_map(|(start, _)| Self::from_leading_json(&text[start..]))
    }

    /// Parses the JSON value that starts `text` as a report, ignoring whatever follows it.
    fn from_leading_json(text: &str) -> Option<Self> {
        let mut values = serde_json::Deserializer::from_str(text).into_iter::<Value>();
        match values.next() {
            Some(Ok(value)) if value.is_object() => Self::from_value(&value).ok(),
            _ => None,
        }
    }
}

/// Written by hand so it stays inside what all three vendors' structured output modes accept:
/// every property required, no extra properties, only basic types.
pub fn report_json_schema() -> Value {
    let statuses: Vec<&str> = ReportStatus::ALL.iter().map(ReportStatus::as_str).collect();
    let string_list = |description: &str| {
        json!({
            "type": "array",
            "items": { "type": "string" },
            "description": description,
        })
    };
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "summary", "files_changed", "tests_run", "concerns"],
        "properties": {
            "status": {
                "type": "string",
                "enum": statuses,
                "description": "done if the task is complete, blocked if you can't continue, failed if you tried and couldn't, needs_decision if the user must choose something.",
            },
            "summary": {
                "type": "string",
                "description": "What you did, in a few sentences.",
            },
            "files_changed": string_list("Paths you changed, relative to the working directory."),
            "tests_run": string_list("Test commands you ran and whether they passed."),
            "concerns": string_list("Risks, open questions, or follow-ups. Empty if none."),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Report {
        Report {
            status: ReportStatus::Done,
            summary: "Created hello.txt.".into(),
            files_changed: vec!["hello.txt".into()],
            tests_run: vec!["test -f hello.txt: passed".into()],
            concerns: vec![],
        }
    }

    #[test]
    fn solo_card_defaults() {
        let card = Card::solo("Fix the README\n\nMore detail here.", BudgetLimit::Usd(3.0));
        assert_eq!(card.id.as_str(), "card-001");
        assert_eq!(card.title, "Fix the README");
        assert!(card.owned_paths.is_empty());
        assert!(card.task.contains("More detail"));
    }

    #[test]
    fn long_titles_are_cut_on_char_boundaries() {
        let task = "é".repeat(100);
        let title = title_from_task(&task);
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
        assert_eq!(title_from_task("  \n  hello  \n"), "hello");
        assert_eq!(title_from_task(""), "");
    }

    #[test]
    fn schema_matches_the_report_type() {
        let schema = report_json_schema();
        let serialized = serde_json::to_value(sample()).unwrap();
        let mut report_keys: Vec<&String> = serialized.as_object().unwrap().keys().collect();
        let mut schema_keys: Vec<&String> =
            schema["properties"].as_object().unwrap().keys().collect();
        let mut required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        report_keys.sort();
        schema_keys.sort();
        required.sort_unstable();
        assert_eq!(report_keys, schema_keys);
        assert_eq!(
            required,
            schema_keys.iter().map(|k| k.as_str()).collect::<Vec<_>>()
        );
        for status in ReportStatus::ALL {
            assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
        }
        assert_eq!(
            schema["properties"]["status"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn schema_accepts_reports_and_rejects_bad_ones() {
        let validator = jsonschema::validator_for(&report_json_schema()).unwrap();
        assert!(validator.is_valid(&serde_json::to_value(sample()).unwrap()));
        let bad_status = json!({"status": "maybe", "summary": "", "files_changed": [], "tests_run": [], "concerns": []});
        assert!(!validator.is_valid(&bad_status));
        let extra = json!({"status": "done", "summary": "", "files_changed": [], "tests_run": [], "concerns": [], "x": 1});
        assert!(!validator.is_valid(&extra));
        assert!(!validator.is_valid(&json!({"status": "done", "summary": "s"})));
    }

    #[test]
    fn report_round_trips() {
        let value = serde_json::to_value(sample()).unwrap();
        assert_eq!(Report::from_value(&value).unwrap(), sample());
        assert_eq!(value["status"], "done");
    }

    #[test]
    fn from_value_fills_missing_lists() {
        let report =
            Report::from_value(&json!({"status": "blocked", "summary": "no access"})).unwrap();
        assert_eq!(report.status, ReportStatus::Blocked);
        assert!(report.files_changed.is_empty());
        assert!(Report::from_value(&json!({"summary": "no status"})).is_err());
    }

    #[test]
    fn from_text_finds_plain_json() {
        let text = serde_json::to_string(&sample()).unwrap();
        assert_eq!(Report::from_text(&text), Some(sample()));
    }

    #[test]
    fn from_text_finds_the_last_fenced_report() {
        let text = format!(
            "I made the change. Example config: {{\"a\": 1}}\n\n```json\n{}\n```\nThanks!",
            serde_json::to_string_pretty(&sample()).unwrap()
        );
        assert_eq!(Report::from_text(&text), Some(sample()));
    }

    #[test]
    fn from_text_prefers_the_last_report() {
        let first = json!({"status": "failed", "summary": "first try"});
        let second = json!({"status": "done", "summary": "second try"});
        let text = format!("{first}\nthen\n{second}");
        assert_eq!(Report::from_text(&text).unwrap().summary, "second try");
    }

    #[test]
    fn from_text_returns_none_without_a_report() {
        assert_eq!(Report::from_text("done, no json here"), None);
        assert_eq!(Report::from_text("{\"status\": \"done\""), None);
        assert_eq!(Report::from_text("{\"other\": true}"), None);
    }
}
