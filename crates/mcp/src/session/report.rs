//! `argonauts_report`: each call is checked against the step's schema, and its errors go back
//! so the model can fix them, at most three times. The last valid report is written to
//! `report.json`. Argonauts Code validates it again, since the agent could write the file itself.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};

use jsonschema::Validator;
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::leaked;
use crate::ReportTool;

/// Invalid calls answered with their errors. Later invalid calls are refused, but a valid
/// report is still saved.
pub const MAX_INVALID: u32 = 3;
/// Schema errors listed back per call, at most.
const MAX_ERRORS: usize = 20;
const ERROR_MAX: usize = 200;

pub struct ReportState {
    tool: ReportTool,
    validator: Result<Validator, String>,
    invalid: AtomicU32,
    /// The report is written in place, so two calls at once must not interleave.
    writing: Mutex<()>,
}

impl ReportState {
    pub fn new(tool: ReportTool) -> Self {
        let validator = jsonschema::validator_for(&tool.schema)
            .map_err(|err| format!("the step's output schema is invalid: {err}"));
        Self {
            tool,
            validator,
            invalid: AtomicU32::new(0),
            writing: Mutex::new(()),
        }
    }

    pub fn tool(&self) -> &ReportTool {
        &self.tool
    }

    /// The tool's input schema: the step's schema, or an open object when it isn't a valid
    /// object schema, since MCP needs one there and a broken one could break the tool list.
    /// Validation always uses the step's own schema.
    pub fn input_schema(&self) -> JsonObject {
        match &self.tool.schema {
            Value::Object(schema) if self.validator.is_ok() => schema.clone(),
            _ => match json!({ "type": "object" }) {
                Value::Object(schema) => schema,
                _ => JsonObject::new(),
            },
        }
    }

    pub fn invalid_calls(&self) -> u32 {
        self.invalid.load(Ordering::SeqCst)
    }

    /// Checks one report and writes it when valid, or when it is once mended: its leaked
    /// parameters made fields again and any field the schema forbids dropped. The error lists
    /// what failed, for the model.
    pub fn submit(&self, report: &Value) -> Result<(), String> {
        let problems = self.problems(report);
        if problems.is_empty() {
            return self.write(report);
        }
        if let Some(mended) = leaked::mended(report, &self.tool.schema)
            && self.problems(&mended).is_empty()
        {
            return self.write(&mended);
        }
        let earlier = self.invalid.fetch_add(1, Ordering::SeqCst);
        if earlier >= MAX_INVALID {
            return Err(format!(
                "argonauts_report refused: it already had {MAX_INVALID} invalid reports. It \
                 still saves a valid one. If you can't send one, stop calling it and say what \
                 went wrong in your final message."
            ));
        }
        let left = MAX_INVALID.saturating_sub(earlier.saturating_add(1));
        Err(format!(
            "The report doesn't match the step's schema:\n{}\nFix it and call argonauts_report \
             again ({left} {} left).",
            problems.join("\n"),
            if left == 1 { "try" } else { "tries" },
        ))
    }

    fn write(&self, report: &Value) -> Result<(), String> {
        let _writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        write_report(&self.tool.report_file, report)
    }

    /// What in `report` fails the step's schema, one line each.
    fn problems(&self, report: &Value) -> Vec<String> {
        match &self.validator {
            Ok(validator) => validator
                .iter_errors(report)
                .take(MAX_ERRORS)
                .map(|err| {
                    let place = err.instance_path().to_string();
                    let place = if place.is_empty() {
                        "/".to_owned()
                    } else {
                        place
                    };
                    let what: String = err.masked().to_string().chars().take(ERROR_MAX).collect();
                    format!("- at {place}: {what}")
                })
                .collect::<Vec<_>>(),
            Err(err) => vec![format!("- {err}")],
        }
    }
}

/// Writes the report in place. The session sandbox lets the agent write only `report.json`, so
/// a temp file beside it can't be made. Argonauts Code validates the file again, which catches
/// a torn write.
fn write_report(path: &Path, report: &Value) -> Result<(), String> {
    let failed = |err: &dyn std::fmt::Display| format!("writing {}: {err}", path.display());
    let mut text = serde_json::to_string_pretty(report).map_err(|err| failed(&err))?;
    text.push('\n');
    File::create(path)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .map_err(|err| failed(&err))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["done", "blocked"] },
                "tests": { "type": "integer", "minimum": 0 }
            },
            "required": ["status"],
            "additionalProperties": false
        })
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("read")).expect("json")
    }

    #[test]
    fn a_valid_report_replaces_the_last_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let report_file = dir.path().join("report.json");
        let state = ReportState::new(ReportTool {
            schema: schema(),
            report_file: report_file.clone(),
        });
        state
            .submit(&json!({ "status": "done", "tests": 4 }))
            .expect("first");
        state
            .submit(&json!({ "status": "blocked" }))
            .expect("second");
        // The shorter report must not keep the tail of the longer one.
        assert_eq!(read(&report_file), json!({ "status": "blocked" }));
    }

    /// A call whose later parameters leaked into its first string is saved as the fields it
    /// meant, without spending a try.
    #[test]
    fn a_report_whose_parameters_leaked_into_a_string_is_saved_mended() {
        let dir = tempfile::tempdir().expect("temp dir");
        let report_file = dir.path().join("report.json");
        let state = ReportState::new(ReportTool {
            schema: schema(),
            report_file: report_file.clone(),
        });
        state
            .submit(&json!({ "status": "done</status>\n<tests>4</tests>" }))
            .expect("mended");
        assert_eq!(read(&report_file), json!({ "status": "done", "tests": 4 }));
        assert_eq!(state.invalid_calls(), 0);
    }

    #[test]
    fn a_valid_report_is_saved_after_the_invalid_limit() {
        let dir = tempfile::tempdir().expect("temp dir");
        let report_file = dir.path().join("report.json");
        let state = ReportState::new(ReportTool {
            schema: schema(),
            report_file: report_file.clone(),
        });
        for left in (0..MAX_INVALID).rev() {
            let err = state.submit(&json!({ "status": 3 })).unwrap_err();
            let tries = if left == 1 { "try" } else { "tries" };
            assert!(err.contains(&format!("{left} {tries} left")), "{err}");
        }
        let err = state.submit(&json!({ "status": 3 })).unwrap_err();
        assert!(err.contains("refused"), "{err}");
        assert!(!report_file.exists());

        state
            .submit(&json!({ "status": "done" }))
            .expect("a valid report after the limit");
        assert_eq!(read(&report_file), json!({ "status": "done" }));
    }

    /// The session sandbox lets the agent write only `report.json` in its session dir. This
    /// test runs itself again under that rule and submits from inside it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_report_is_saved_when_only_the_report_file_is_writable() {
        const SANDBOXED_REPORT: &str = "ARGONAUTS_TEST_SANDBOXED_REPORT";
        let report = json!({ "status": "done", "tests": 2 });
        if let Some(report_file) = std::env::var_os(SANDBOXED_REPORT) {
            let report_file = std::path::PathBuf::from(report_file);
            let beside = report_file.with_file_name("beside.json");
            assert!(File::create(&beside).is_err(), "the sandbox isn't on");
            let state = ReportState::new(ReportTool {
                schema: schema(),
                report_file,
            });
            state.submit(&report).expect("submit in the sandbox");
            return;
        }

        let dir = tempfile::tempdir().expect("temp dir");
        // Seatbelt matches real paths, and the temp dir sits behind the /var symlink.
        let session = dir.path().canonicalize().expect("real path");
        let report_file = session.join("report.json");
        let profile = format!(
            "(version 1)\n(allow default)\n(deny file-write*)\n\
             (allow file-write* (subpath \"{}\"))\n",
            report_file.display()
        );
        let test = concat!(
            module_path!(),
            "::a_report_is_saved_when_only_the_report_file_is_writable"
        );
        let test = test.split_once("::").map_or(test, |(_, path)| path);
        let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile])
            .arg(std::env::current_exe().expect("test binary"))
            .args(["--exact", test, "--nocapture"])
            .env(SANDBOXED_REPORT, &report_file)
            .output()
            .expect("run sandbox-exec");
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // Also fails when the filter matched no test, since nothing was written then.
        assert_eq!(read(&report_file), report);
        let names: Vec<_> = fs::read_dir(&session)
            .expect("list")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(names, vec!["report.json"]);
    }

    #[test]
    fn errors_name_each_problem_and_count_down() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = ReportState::new(ReportTool {
            schema: schema(),
            report_file: dir.path().join("report.json"),
        });
        let err = state
            .submit(&json!({ "status": "maybe", "tests": -1, "extra": true }))
            .unwrap_err();
        assert!(err.contains("/status"), "{err}");
        assert!(err.contains("/tests"), "{err}");
        assert!(err.contains("2 tries left"), "{err}");
        assert_eq!(state.invalid_calls(), 1);
    }

    #[test]
    fn a_broken_schema_is_reported_and_counted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = ReportState::new(ReportTool {
            schema: json!({ "type": 12 }),
            report_file: dir.path().join("report.json"),
        });
        let err = state.submit(&json!({})).unwrap_err();
        assert!(err.contains("schema is invalid"), "{err}");
        assert_eq!(state.invalid_calls(), 1);
        assert_eq!(state.input_schema().get("type"), Some(&json!("object")));
        assert!(!dir.path().join("report.json").exists());
    }

    #[test]
    fn a_non_object_schema_still_lists_an_object() {
        let state = ReportState::new(ReportTool {
            schema: json!(true),
            report_file: "report.json".into(),
        });
        assert_eq!(state.input_schema().get("type"), Some(&json!("object")));
    }
}
