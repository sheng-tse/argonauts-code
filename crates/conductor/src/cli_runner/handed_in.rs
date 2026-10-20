//! A report handed in through a tool call. A model on a CLI that can't hold it to a format may
//! write its report into a tool instead of its final message: Qwen on OpenCode 2 sent it as
//! `return {…}` to the execute tool, which hands the value back, so its turn never ended and it
//! sent the same report again and again. The first call whose whole result is one JSON object in
//! the report's format, with every field the format names and no other, is the session's report.
//! An object inside longer output never counts, nor does a file the call read or wrote, so a
//! quoted example or a report on disk isn't taken, and a format that names no field takes none.

use std::collections::BTreeSet;

use argonauts_core::{AgentEvent, answer_format};
use serde_json::Value;

/// Programs whose output is a file's text, which is never a report handed in.
const PRINTS_A_FILE: &[&str] = &["cat", "head", "tail", "less", "more", "bat", "nl"];

/// Where a tool's input names the file it reads or writes.
const PATH_KEYS: &[&str] = &["file_path", "path", "filePath", "file", "notebook_path"];

#[derive(Debug, Default)]
pub struct HandedIn {
    /// The open calls that read or write a file.
    on_files: BTreeSet<String>,
}

impl HandedIn {
    /// The report `event` hands in, when it is the end of a call whose whole result has every
    /// field of `schema` and fits it, or does once the fields that ran on into one string are
    /// mended. Calls of subagents are their own business.
    pub fn report(&mut self, event: &AgentEvent, schema: &Value) -> Option<Value> {
        match event {
            AgentEvent::ToolStarted { id, input, .. } => {
                if on_a_file(input) {
                    self.on_files.insert(id.clone());
                }
                None
            }
            AgentEvent::ToolFinished {
                id,
                ok,
                output,
                parent,
            } => {
                let on_a_file = self.on_files.remove(id);
                if !ok || parent.is_some() || on_a_file {
                    return None;
                }
                let value = serde_json::from_str::<Value>(output.as_deref()?.trim()).ok()?;
                if !value.is_object() || names_no_field(schema) {
                    return None;
                }
                // Every field named, and none other, as only a report would be.
                let whole = answer_format::strict(schema);
                let report = if answer_format::fits(&whole, &value) {
                    value
                } else {
                    answer_format::mended_to_fit(&whole, &value)?
                };
                Some(answer_format::as_written(schema, report))
            }
            _ => None,
        }
    }
}

/// A format that names no field takes any object, so nothing can be told apart as a report.
fn names_no_field(schema: &Value) -> bool {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_none_or(serde_json::Map::is_empty)
}

/// The call names a file, or runs a command that prints one.
fn on_a_file(input: &Value) -> bool {
    if PATH_KEYS.iter().any(|key| input.get(*key).is_some()) {
        return true;
    }
    // A command given as words may hold a whole script in one of them, as `bash -lc "…"`.
    let commands: Vec<&str> = match input.get("command").or_else(|| input.get("cmd")) {
        Some(Value::String(command)) => vec![command],
        Some(Value::Array(words)) => words.iter().filter_map(Value::as_str).collect(),
        _ => return false,
    };
    commands
        .into_iter()
        .flat_map(|command| command.split(['&', ';', '|', '\n']))
        .filter_map(|part| part.split_whitespace().next())
        .map(|program| program.rsplit('/').next().unwrap_or(program))
        .any(|program| PRINTS_A_FILE.contains(&program))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["done", "blocked"]},
                "summary": {"type": "string"},
            },
            "required": ["status", "summary"],
        })
    }

    fn started(id: &str, name: &str, input: Value) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
            input,
            parent: None,
        }
    }

    fn finished(id: &str, ok: bool, output: &str) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok,
            output: Some(output.into()),
            parent: None,
        }
    }

    const REPORT: &str = "{\n  \"status\": \"done\",\n  \"summary\": \"Fixed slugify.\"\n}\n";

    #[test]
    fn a_call_whose_whole_result_is_a_report_hands_it_in() {
        let mut handed = HandedIn::default();
        let code =
            json!({"code": "return {\"status\": \"done\", \"summary\": \"Fixed slugify.\"}"});
        assert_eq!(
            handed.report(&started("c1", "execute", code), &schema()),
            None
        );
        assert_eq!(
            handed.report(&finished("c1", true, REPORT), &schema()),
            Some(json!({"status": "done", "summary": "Fixed slugify."}))
        );
    }

    #[test]
    fn a_report_whose_fields_ran_on_is_mended_to_fit() {
        let schema = json!({
            "type": "object",
            "properties": {
                "status": {"type": "string"},
                "summary": {"type": "string"},
                "files_changed": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["status", "summary", "files_changed"],
        });
        let ran_on = json!({
            "status": "done",
            "summary": "Fixed slugify.</summary>\n<files_changed>[\"slug.py\"]</files_changed>",
        });
        let mut handed = HandedIn::default();
        assert_eq!(
            handed.report(&finished("c1", true, &ran_on.to_string()), &schema),
            Some(json!({
                "status": "done",
                "summary": "Fixed slugify.",
                "files_changed": ["slug.py"],
            }))
        );
    }

    #[test]
    fn only_a_whole_result_in_the_format_counts() {
        let mut handed = HandedIn::default();
        let longer = format!("Here is the report:\n{REPORT}");
        let off_format = r#"{"status": "finished", "summary": "Fixed slugify."}"#;
        for output in [longer.as_str(), off_format, "[1, 2]", "done", ""] {
            assert_eq!(
                handed.report(&finished("c1", true, output), &schema()),
                None,
                "{output}"
            );
        }
        assert_eq!(
            handed.report(&finished("c1", false, REPORT), &schema()),
            None
        );
        let of_a_subagent = AgentEvent::ToolFinished {
            id: "c2".into(),
            ok: true,
            output: Some(REPORT.into()),
            parent: Some("task-1".into()),
        };
        assert_eq!(handed.report(&of_a_subagent, &schema()), None);
    }

    /// `gh api …`, `jq . x.json` or a tool of your own may print any object, which a format
    /// that names no field would take.
    #[test]
    fn a_format_that_names_no_field_takes_nothing() {
        let mut handed = HandedIn::default();
        for schema in [
            json!({"type": "object"}),
            json!({"type": "object", "properties": {}}),
        ] {
            assert_eq!(
                handed.report(&finished("c1", true, r#"{"name":"x"}"#), &schema),
                None
            );
        }
    }

    #[test]
    fn an_object_short_of_a_field_is_no_report() {
        let schema = json!({
            "type": "object",
            "properties": {
                "status": {"type": "string"},
                "summary": {"type": "string"},
                "concerns": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["status"],
        });
        let mut handed = HandedIn::default();
        let short = r#"{"status": "done", "summary": "x"}"#;
        assert_eq!(handed.report(&finished("c1", true, short), &schema), None);
        // Every field it names: a report, read as the format is written, without a field of
        // its own the format doesn't know.
        let whole = r#"{"status": "done", "summary": "x", "concerns": null, "mood": "fine"}"#;
        assert_eq!(
            handed.report(&finished("c1", true, whole), &schema),
            Some(json!({"status": "done", "summary": "x"}))
        );
    }

    #[test]
    fn a_file_read_or_printed_is_never_a_report() {
        let calls = [
            ("read", json!({"filePath": "report.json"})),
            ("Read", json!({"file_path": "/work/report.json"})),
            ("write", json!({"path": "report.json", "content": REPORT})),
            ("bash", json!({"command": "cd /work && cat report.json"})),
            (
                "shell",
                json!({"command": ["bash", "-lc", "/bin/cat report.json"]}),
            ),
            ("bash", json!({"command": "jq . out.json | head -n 40"})),
        ];
        for (n, (name, input)) in calls.into_iter().enumerate() {
            let mut handed = HandedIn::default();
            let id = format!("c{n}");
            handed.report(&started(&id, name, input.clone()), &schema());
            assert_eq!(
                handed.report(&finished(&id, true, REPORT), &schema()),
                None,
                "{name} {input}"
            );
        }
        // A command that writes the report out itself hands it in.
        let mut handed = HandedIn::default();
        let echo =
            json!({"command": "echo '{\"status\":\"done\",\"summary\":\"Fixed slugify.\"}'"});
        handed.report(&started("e1", "bash", echo), &schema());
        assert!(
            handed
                .report(&finished("e1", true, REPORT), &schema())
                .is_some()
        );
    }
}

/// The run that showed it: Qwen on OpenCode 2, as its session's agent events were recorded.
#[cfg(test)]
mod replay {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Instant;

    use argonauts_core::AgentEvent;
    use argonauts_core::events::session::EndReason;
    use argonauts_core::session_runner::{SessionLimits, SessionRunner};
    use argonauts_core::steps::output_schema;
    use argonauts_core::steps::task::{Build, ReportStatus, TaskReport};

    use crate::cli_runner::context::Context;
    use crate::cli_runner::observe::{Action, Observer};
    use crate::cli_runner::outbox;
    use crate::cli_runner::stop::StopCause;
    use crate::cli_runner::testing::{TestAdapter, TestDriver, collect, spec_in};

    const FIRST_EXECUTE: &str = "call_bvplblm9";

    fn recording() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/opencode/report_in_execute.jsonl")
    }

    fn events() -> Vec<AgentEvent> {
        std::fs::read_to_string(recording())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The watchdogs as the run had them: a nudge at 10 identical calls, the stop at 12.
    fn limits() -> SessionLimits {
        SessionLimits {
            repeat_limit: Some(10),
            inactivity_min: Some(15.0),
            ..SessionLimits::default()
        }
    }

    #[tokio::test]
    async fn the_report_ends_the_session_at_its_first_execute_call() {
        let driver = TestDriver::new(TestAdapter::new("exec sleep 30"));
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "Fix slugify");
        spec.json_schema = Some(output_schema::<Build>());
        spec.limits = limits();
        let (_handle, out, _control) = outbox::open(spec.key.clone());
        let home = driver.worker.home().unwrap();
        let ctx = Arc::new(Context::new(&driver.worker, spec, home));
        let mut observer = Observer::new(ctx, out, Instant::now());

        let mut actions = Vec::new();
        let mut at = None;
        for (n, event) in events().into_iter().enumerate() {
            let seen = observer.event(event.clone());
            if at.is_none() && seen.contains(&Action::Stop(StopCause::Reported)) {
                at = Some((n, event));
            }
            actions.extend(seen);
        }
        let (n, event) = at.expect("the report was handed in");
        assert!(
            matches!(&event, AgentEvent::ToolFinished { id, .. } if id == FIRST_EXECUTE),
            "{event:?}"
        );
        // Nothing came before it to nudge or stop the session.
        let before: Vec<&Action> = actions
            .iter()
            .take_while(|action| **action != Action::Stop(StopCause::Reported))
            .collect();
        assert!(before.is_empty(), "{before:?} before event {n}");
        let report: TaskReport =
            serde_json::from_value(observer.facts.structured.clone().unwrap()).unwrap();
        assert_eq!(report.status, ReportStatus::Done);
        assert_eq!(report.files_changed, vec!["slug.py".to_owned()]);
    }

    /// A later try that only looked around, then handed its report in through a tool after the
    /// wrap-up: the report counts, and the watchdog's stop never comes.
    #[tokio::test]
    async fn a_report_handed_in_after_the_wrap_up_ends_the_later_try() {
        let report = events()
            .into_iter()
            .find_map(|event| match event {
                AgentEvent::ToolFinished { id, output, .. } if id == FIRST_EXECUTE => output,
                _ => None,
            })
            .expect("the recorded report");
        let driver = TestDriver::new(TestAdapter::new("exec sleep 30"));
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "Fix slugify");
        spec.json_schema = Some(output_schema::<Build>());
        spec.limits = SessionLimits {
            no_progress_calls: Some(8),
            ..limits()
        };
        let (_handle, out, _control) = outbox::open(spec.key.clone());
        let home = driver.worker.home().unwrap();
        let ctx = Arc::new(Context::new(&driver.worker, spec, home));
        let mut observer = Observer::new(ctx, out, Instant::now());

        let mut actions = Vec::new();
        let call = |n: u32, name: &str, input: serde_json::Value, output: &str| {
            let id = format!("c{n}");
            [
                AgentEvent::ToolStarted {
                    id: id.clone(),
                    name: name.into(),
                    input,
                    parent: None,
                },
                AgentEvent::ToolFinished {
                    id,
                    ok: true,
                    output: Some(output.into()),
                    parent: None,
                },
            ]
        };
        for n in 1..=9 {
            let looked = serde_json::json!({"command": format!("ls dir-{n}")});
            for event in call(n, "shell", looked, "slug.py") {
                actions.extend(observer.event(event));
            }
        }
        let code = serde_json::json!({"code": "return report"});
        for event in call(10, "execute", code, &report) {
            actions.extend(observer.event(event));
        }
        assert_eq!(
            actions,
            [Action::CheckAndReport, Action::Stop(StopCause::Reported)]
        );
    }

    /// A CLI that holds its sessions to the format has the report come as its answer, so a
    /// tool's result is never taken for one there.
    #[tokio::test]
    async fn a_cli_that_holds_the_format_is_never_handed_a_report() {
        let adapter = TestAdapter::new("exec sleep 30").holding_its_answer_format();
        let driver = TestDriver::new(adapter);
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "Fix slugify");
        spec.json_schema = Some(output_schema::<Build>());
        spec.limits = limits();
        let (_handle, out, _control) = outbox::open(spec.key.clone());
        let home = driver.worker.home().unwrap();
        let ctx = Arc::new(Context::new(&driver.worker, spec, home));
        let mut observer = Observer::new(ctx, out, Instant::now());
        for event in events() {
            let seen = observer.event(event);
            assert!(!seen.contains(&Action::Stop(StopCause::Reported)));
        }
        assert_eq!(observer.facts.structured, None);
    }

    #[tokio::test]
    async fn replayed_it_ends_done_with_its_report() {
        let script = format!("cat '{}'; exec sleep 30", recording().display());
        let driver = TestDriver::new(TestAdapter::new(&script));
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), dir.path(), "Fix slugify");
        spec.json_schema = Some(output_schema::<Build>());
        spec.limits = limits();
        let (_, end) = collect(driver.worker.start(spec).await.unwrap()).await;
        assert_eq!(end.reason, EndReason::Completed);
        let report: TaskReport = serde_json::from_value(end.structured.unwrap()).unwrap();
        assert_eq!(report.status, ReportStatus::Done);
    }
}
