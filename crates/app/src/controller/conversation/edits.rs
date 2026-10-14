//! Codex reports which files an edit added, changed or deleted, but not the change itself. The
//! conversation reads each file as the edit starts and again as it ends, and sends the difference
//! as the patch Claude Code's edit results carry, so both CLIs' edits are drawn with the file's
//! line numbers and the lines around each change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use argonauts_core::event::AgentEvent;
use serde_json::Value;
use similar::TextDiff;

/// What Codex's adapter names an edit.
const FILE_CHANGE: &str = "file_change";
/// Unchanged lines shown around each change, as many as Claude Code shows.
const CONTEXT: usize = 3;
/// Codex reports an edit as it applies it, so a file written this shortly before the report
/// may already hold the edit, or half of it.
const SETTLED: Duration = Duration::from_secs(2);
/// Larger files are left without a patch rather than read and diffed on every edit.
const MAX_BYTES: u64 = 1 << 20;
/// Past this, the diff settles for correct but less tidy hunks.
const DIFF_TIME_LIMIT: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
pub(super) struct Edits {
    /// The files of each edit still running, as they were before it.
    open: HashMap<String, Vec<Before>>,
    /// Each file as an earlier edit left it: what the next edit of it starts from when the
    /// file itself was written too recently to tell.
    left: HashMap<PathBuf, String>,
}

#[derive(Debug)]
struct Before {
    /// As Codex names the file.
    path: String,
    full: PathBuf,
    deleted: bool,
    /// `None` when it can't be told, and the edit is drawn without its patch.
    text: Option<String>,
}

impl Edits {
    /// The patches to send ahead of `event`: one per file an edit changed, as it finishes.
    pub(super) fn on_agent(&mut self, folder: &Path, event: &AgentEvent) -> Vec<AgentEvent> {
        match event {
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent: None,
            } if name == FILE_CHANGE => {
                self.started(folder, id, input, SystemTime::now());
                Vec::new()
            }
            AgentEvent::ToolFinished {
                id,
                ok,
                parent: None,
                ..
            } => self.finished(id, *ok),
            _ => Vec::new(),
        }
    }

    fn started(&mut self, folder: &Path, id: &str, input: &Value, began: SystemTime) {
        let changes = input.get("changes").and_then(Value::as_array);
        let befores = changes
            .into_iter()
            .flatten()
            .filter_map(|change| {
                let path = change.get("path").and_then(Value::as_str)?;
                let kind = change
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("update");
                let full = folder.join(path);
                let text = if kind == "add" {
                    Some(String::new())
                } else {
                    settled_text(&full, began).or_else(|| self.left.get(&full).cloned())
                };
                Some(Before {
                    path: path.to_owned(),
                    full,
                    deleted: kind == "delete",
                    text,
                })
            })
            .collect();
        self.open.insert(id.to_owned(), befores);
    }

    fn finished(&mut self, id: &str, ok: bool) -> Vec<AgentEvent> {
        let Some(befores) = self.open.remove(id) else {
            return Vec::new();
        };
        if !ok {
            return Vec::new();
        }
        let mut patches = Vec::new();
        for before in befores {
            let after = if before.deleted {
                self.left.remove(&before.full);
                String::new()
            } else {
                let Some(text) = small_text(&before.full) else {
                    self.left.remove(&before.full);
                    continue;
                };
                self.left.insert(before.full.clone(), text.clone());
                text
            };
            if let Some(hunks) = before.text.and_then(|text| hunks(&text, &after)) {
                patches.push(AgentEvent::FilePatch {
                    id: id.to_owned(),
                    path: before.path,
                    hunks,
                    parent: None,
                });
            }
        }
        patches
    }
}

/// The file as it was before an edit that `began` then, or `None` when it was written too
/// close to then, or while it was read, to be sure the edit isn't in it already.
fn settled_text(path: &Path, began: SystemTime) -> Option<String> {
    let stamp = |path: &Path| {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    };
    let (modified, len) = stamp(path)?;
    let settled = began
        .duration_since(modified)
        .is_ok_and(|age| age >= SETTLED);
    if !settled || len > MAX_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    (stamp(path)? == (modified, len)).then_some(text)
}

fn small_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Unified hunks, `@@ -a,b +c,d @@` and the marked lines, or `None` when nothing changed.
fn hunks(before: &str, after: &str) -> Option<String> {
    if before == after {
        return None;
    }
    let diff = TextDiff::configure()
        .timeout(DIFF_TIME_LIMIT)
        .diff_lines(before, after);
    let hunks = diff.unified_diff().context_radius(CONTEXT).to_string();
    (!hunks.is_empty()).then_some(hunks)
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use serde_json::json;

    use super::*;

    fn started(id: &str, changes: Value) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            name: FILE_CHANGE.into(),
            input: json!({ "changes": changes }),
            parent: None,
        }
    }

    fn finished(id: &str, ok: bool) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok,
            output: None,
            parent: None,
        }
    }

    /// Written a minute ago, long enough before an edit to be read as it was.
    fn write_settled(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(60))
            .unwrap();
    }

    fn patch(event: &AgentEvent) -> (&str, &str) {
        match event {
            AgentEvent::FilePatch { path, hunks, .. } => (path.as_str(), hunks.as_str()),
            other => panic!("not a patch: {other:?}"),
        }
    }

    #[test]
    fn an_update_sends_the_change_with_three_lines_around_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        let lines: Vec<String> = (1..=10).map(|n| format!("line {n}\n")).collect();
        write_settled(&file, &lines.concat());
        let mut edits = Edits::default();
        let path = file.to_str().unwrap();
        let change = json!([{ "path": path, "kind": "update" }]);
        assert!(
            edits
                .on_agent(dir.path(), &started("i1", change))
                .is_empty()
        );
        let mut changed = lines.clone();
        changed[5] = "line six\n".into();
        std::fs::write(&file, changed.concat()).unwrap();

        let sent = edits.on_agent(dir.path(), &finished("i1", true));
        assert_eq!(sent.len(), 1);
        let (named, hunks) = patch(&sent[0]);
        assert_eq!(named, path);
        assert_eq!(
            hunks,
            "@@ -3,7 +3,7 @@\n line 3\n line 4\n line 5\n-line 6\n+line six\n line 7\n line 8\n line 9\n"
        );
    }

    #[test]
    fn an_added_file_is_all_new_lines_and_a_deleted_one_all_gone() {
        let dir = tempfile::tempdir().unwrap();
        let mut edits = Edits::default();
        let changes = json!([
            { "path": "new.txt", "kind": "add" },
            { "path": "old.txt", "kind": "delete" },
        ]);
        write_settled(&dir.path().join("old.txt"), "bye\n");
        edits.on_agent(dir.path(), &started("i2", changes));
        std::fs::write(dir.path().join("new.txt"), "a\nb\n").unwrap();
        std::fs::remove_file(dir.path().join("old.txt")).unwrap();

        let sent = edits.on_agent(dir.path(), &finished("i2", true));
        let patches: Vec<_> = sent.iter().map(patch).collect();
        assert_eq!(
            patches,
            [
                ("new.txt", "@@ -0,0 +1,2 @@\n+a\n+b\n"),
                ("old.txt", "@@ -1 +0,0 @@\n-bye\n"),
            ]
        );
    }

    #[test]
    fn a_file_written_just_before_the_edit_starts_from_what_the_last_edit_left() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        let mut edits = Edits::default();
        let change = json!([{ "path": "a.txt", "kind": "update" }]);

        // Written a moment ago and never edited here: there is nothing to trust.
        std::fs::write(&file, "one\n").unwrap();
        edits.on_agent(dir.path(), &started("i3", change.clone()));
        std::fs::write(&file, "two\n").unwrap();
        assert!(edits.on_agent(dir.path(), &finished("i3", true)).is_empty());

        // The next edit starts from what that one left, though the file is fresh again.
        std::fs::write(&file, "three\n").unwrap();
        edits.on_agent(dir.path(), &started("i4", change));
        let sent = edits.on_agent(dir.path(), &finished("i4", true));
        assert_eq!(patch(&sent[0]).1, "@@ -1 +1 @@\n-two\n+three\n");
    }

    #[test]
    fn a_failed_edit_sends_nothing_and_other_tools_pass_by() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        write_settled(&file, "one\n");
        let mut edits = Edits::default();
        let change = json!([{ "path": "a.txt", "kind": "update" }]);
        edits.on_agent(dir.path(), &started("i5", change));
        std::fs::write(&file, "two\n").unwrap();
        assert!(
            edits
                .on_agent(dir.path(), &finished("i5", false))
                .is_empty()
        );

        let shell = AgentEvent::ToolStarted {
            id: "i6".into(),
            name: "command_execution".into(),
            input: json!({ "command": "ls" }),
            parent: None,
        };
        assert!(edits.on_agent(dir.path(), &shell).is_empty());
        assert!(edits.on_agent(dir.path(), &finished("i6", true)).is_empty());
    }

    #[tokio::test]
    async fn the_conversation_sends_a_codex_edits_patch_just_before_the_edit_ends() {
        use argonauts_core::app::{AppEvent, ConversationEvent};
        use argonauts_core::session_runner::RunnerEvent;
        use argonauts_testkit::FakeSession;

        use crate::controller::test_support::TestDriver;

        let mut driver = TestDriver::start().await;
        std::fs::write(driver.repo.root().join("hello.txt"), "hello\n").unwrap();
        let change = json!([{ "path": "hello.txt", "kind": "add" }]);
        let session = [started("i1", change), finished("i1", true)]
            .into_iter()
            .fold(
                FakeSession::structured(
                    json!({"kind": "answer", "answer_markdown": "Done.", "estimate": null}),
                ),
                |session, event| session.with_event(RunnerEvent::Agent(event)),
            );
        driver.agent.push(session);
        driver.submit("write hello.txt").await;
        driver
            .until(|event| {
                matches!(
                    event,
                    AppEvent::Conversation {
                        event: ConversationEvent::TurnFinished { .. }
                    }
                )
            })
            .await;

        let agent: Vec<&AgentEvent> = driver
            .seen()
            .iter()
            .filter_map(|event| match event {
                AppEvent::Conversation {
                    event: ConversationEvent::Agent { event, .. },
                } => Some(event),
                _ => None,
            })
            .filter(|event| {
                matches!(
                    event,
                    AgentEvent::FilePatch { .. } | AgentEvent::ToolFinished { .. }
                )
            })
            .collect();
        assert_eq!(agent.len(), 2, "{agent:?}");
        assert_eq!(patch(agent[0]), ("hello.txt", "@@ -0,0 +1 @@\n+hello\n"));
        assert!(matches!(agent[1], AgentEvent::ToolFinished { .. }));
    }
}
