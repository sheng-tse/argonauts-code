//! A session that runs its CLI's own workflow reports through Argonauts Code's report tool, never
//! through the CLI's answer format. Claude Code demands an answer in that format at the end of
//! every turn, and the first turn ends as soon as the workflow starts in the background, so the
//! only answer it could give then is that nothing is finished yet. A report sent while the
//! session's background work still runs is held back: it never reaches the record or the
//! screen, and a session that ends without a later one is asked for it once.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::AgentEvent;
use argonauts_core::agents::session_runner::SessionSpec;
use argonauts_core::base::layout::session_files;
use serde_json::Value;

/// Sent once to a session whose work has ended without a report it could keep.
pub const ASK_FOR_IT: &str = "Your workflow has ended and Argonauts Code has no report from you \
yet. Call argonauts_report now with your final report, once. It is the only way your report \
reaches Argonauts Code.";

/// The session owes a report and works as its CLI's own workflow, which runs on in the
/// background after its turn ends.
pub fn reports_by_tool(spec: &SessionSpec) -> bool {
    spec.cli_workflow.ultracode && spec.json_schema.is_some()
}

pub fn report_file(spec: &SessionSpec) -> PathBuf {
    spec.session_dir.join(session_files::REPORT)
}

pub fn schema_file(spec: &SessionSpec) -> PathBuf {
    spec.session_dir.join(session_files::SCHEMA)
}

/// Writes the step's schema, which the tool checks each report against, and clears a report
/// an earlier process of the session left.
pub fn prepare(spec: &SessionSpec) -> io::Result<()> {
    let Some(schema) = &spec.json_schema else {
        return Ok(());
    };
    let text = serde_json::to_vec_pretty(schema).map_err(io::Error::other)?;
    argonauts_core::base::paths::create_private_dir_all(&spec.session_dir)?;
    argonauts_core::base::paths::write_private_file(&schema_file(spec), &text)?;
    match std::fs::remove_file(report_file(spec)) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// The last report the tool took. A missing or broken file is no report.
pub fn read(spec: &SessionSpec) -> Option<Value> {
    let text = std::fs::read_to_string(report_file(spec)).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
}

/// Report calls made while the session's background work still ran.
#[derive(Debug, Default)]
pub struct Held {
    calls: BTreeSet<String>,
}

impl Held {
    /// True for a report call made while `background` tasks run, and for its end. When it ends
    /// what it wrote is removed, so only a report sent after the work is done counts.
    pub fn holds(&mut self, event: &AgentEvent, background: usize, report: &Path) -> bool {
        match event {
            AgentEvent::ToolStarted { id, name, .. } if background > 0 && is_report(name) => {
                self.calls.insert(id.clone());
                true
            }
            AgentEvent::ToolFinished { id, .. } if self.calls.remove(id) => {
                let _ = std::fs::remove_file(report);
                true
            }
            _ => false,
        }
    }
}

/// `argonauts_report` under whatever prefix the CLI gives its MCP servers' tools.
fn is_report(name: &str) -> bool {
    name == "argonauts_report" || name.ends_with("__argonauts_report")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn started(id: &str, name: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
            input: json!({}),
            parent: None,
        }
    }

    fn finished(id: &str) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: None,
            parent: None,
        }
    }

    #[test]
    fn a_report_sent_while_the_workflow_runs_is_held_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report.json");
        let mut held = Held::default();
        let early = started("r1", "mcp__argonauts_status__argonauts_report");
        assert!(held.holds(&early, 1, &report));
        std::fs::write(&report, r#"{"status":"blocked"}"#).unwrap();
        assert!(held.holds(&finished("r1"), 1, &report));
        assert!(!report.exists());
        // Its other tools, and a report once the work has ended, go through.
        assert!(!held.holds(&started("b1", "Bash"), 1, &report));
        let late = started("r2", "mcp__argonauts_status__argonauts_report");
        assert!(!held.holds(&late, 0, &report));
        assert!(!held.holds(&finished("r2"), 0, &report));
    }

    #[test]
    fn only_a_session_in_its_own_workflow_that_owes_a_report_uses_the_tool() {
        let mut spec = argonauts_testkit::engine::runner::session_spec(
            "claude-code",
            argonauts_core::agents::session_runner::Access::Build,
            Path::new("/tmp/work"),
        );
        spec.json_schema = Some(json!({"type": "object"}));
        assert!(!reports_by_tool(&spec));
        spec.cli_workflow.ultracode = true;
        assert!(reports_by_tool(&spec));
        spec.json_schema = None;
        assert!(!reports_by_tool(&spec));
    }

    #[test]
    fn the_schema_is_written_and_an_old_report_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = argonauts_testkit::engine::runner::session_spec(
            "claude-code",
            argonauts_core::agents::session_runner::Access::Build,
            Path::new("/tmp/work"),
        );
        spec.session_dir = dir.path().join("session");
        spec.json_schema = Some(json!({"type": "object"}));
        std::fs::create_dir_all(&spec.session_dir).unwrap();
        std::fs::write(report_file(&spec), r#"{"status":"done"}"#).unwrap();
        prepare(&spec).unwrap();
        assert!(read(&spec).is_none());
        assert!(schema_file(&spec).exists());
        std::fs::write(report_file(&spec), r#"{"status":"done"}"#).unwrap();
        assert_eq!(read(&spec), Some(json!({"status": "done"})));
    }
}
