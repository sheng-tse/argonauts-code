//! An action as the deeper map reads it: which call it was, and what a summarizer is shown of
//! it, its tool and its input, capped, with an edit's added and removed line counts so an
//! append isn't read as a rewrite.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use argonauts_core::hash::sha256_hex;

/// The most of an action's input a summarizer reads, in characters.
pub(crate) const INPUT_CAP: usize = 500;

/// Which action: its project's state folder, its run (or conversation), the session in it,
/// and the tool call's id. Runs, conversations and call ids repeat across projects, so the
/// state folder is part of every key; a key kept without one is stale.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct ActionKey {
    #[serde(default)]
    pub repo: String,
    pub run: String,
    pub session: String,
    pub call: String,
}

/// What an edit changed, in lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct PatchCounts {
    pub added: u32,
    pub removed: u32,
}

impl PatchCounts {
    /// The lines a unified patch adds and removes, its `+++` and `---` headers left out.
    pub fn of_hunks(hunks: &str) -> PatchCounts {
        let mut counts = PatchCounts::default();
        for line in hunks.lines() {
            if line.starts_with("+++") || line.starts_with("---") {
                continue;
            }
            match line.as_bytes().first() {
                Some(b'+') => counts.added += 1,
                Some(b'-') => counts.removed += 1,
                _ => {}
            }
        }
        counts
    }

    pub fn add(&mut self, more: PatchCounts) {
        self.added += more.added;
        self.removed += more.removed;
    }
}

/// One tool call an agent made, with what the map groups it by.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeepAction {
    pub key: ActionKey,
    /// As the CLI named it, such as `Bash` or `command_execution`.
    pub tool: String,
    pub input: Value,
    /// `None` while no result came back.
    pub ok: Option<bool>,
    pub patch: Option<PatchCounts>,
    pub at: OffsetDateTime,
    /// The folder's name, such as `council`.
    pub project: String,
    /// The CLI's id, such as `codex`.
    pub cli: String,
    /// As the session ran it, such as `gpt-6-luna`; `None` for the CLI's own default.
    pub model: Option<String>,
}

impl DeepAction {
    /// What the summarizer reads: `tool Bash: command: cargo test`, then the patch's counts.
    pub fn text(&self) -> String {
        let failed = if self.ok == Some(false) {
            " (failed)"
        } else {
            ""
        };
        let mut text = format!(
            "tool {}{failed}: {}",
            one_line(&self.tool),
            capped(&input_words(&self.input))
        );
        if let Some(patch) = self.patch {
            text.push_str(&format!(
                " [patch: {} lines added, {} removed]",
                patch.added, patch.removed
            ));
        }
        text
    }
}

/// A short, stable name for a text, kept beside each summary.
pub(crate) fn text_key(text: &str) -> String {
    sha256_hex(text.as_bytes())[..20].to_owned()
}

/// An input's fields as `name: value` pairs, strings unquoted, so the summarizer reads the
/// command or the path rather than JSON punctuation.
fn input_words(input: &Value) -> String {
    match input {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Object(fields) => fields
            .iter()
            .map(|(name, value)| match value {
                Value::String(text) => format!("{name}: {text}"),
                other => format!("{name}: {other}"),
            })
            .collect::<Vec<_>>()
            .join("; "),
        other => other.to_string(),
    }
}

/// Newlines read as spaces, since a request numbers one action to a line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn capped(text: &str) -> String {
    let flat = one_line(text);
    match flat.char_indices().nth(INPUT_CAP) {
        Some((cut, _)) => flat[..cut].to_owned(),
        None => flat,
    }
}

#[cfg(test)]
pub(crate) mod sample {
    use serde_json::json;

    use super::*;

    /// A Bash call in run `run`, session `s-1`, call `call`.
    pub fn shell(run: &str, call: &str, command: &str) -> DeepAction {
        DeepAction {
            key: ActionKey {
                repo: "council-3f9a1c2b4d5e".to_owned(),
                run: run.to_owned(),
                session: "s-1".to_owned(),
                call: call.to_owned(),
            },
            tool: "Bash".to_owned(),
            input: json!({ "command": command }),
            ok: Some(true),
            patch: None,
            at: OffsetDateTime::UNIX_EPOCH,
            project: "council".to_owned(),
            cli: "claude-code".to_owned(),
            model: Some("haiku".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::sample::shell;
    use super::*;

    #[test]
    fn the_text_is_the_tool_and_its_input_with_a_failure_said() {
        let mut action = shell("run-1", "c1", "cargo test\n  --quiet");
        assert_eq!(action.text(), "tool Bash: command: cargo test --quiet");
        action.ok = Some(false);
        assert_eq!(
            action.text(),
            "tool Bash (failed): command: cargo test --quiet"
        );
    }

    #[test]
    fn an_edit_carries_its_added_and_removed_lines() {
        let hunks =
            "--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1,2 +1,4 @@\n keep\n-old\n+new\n+more\n+again\n";
        let counts = PatchCounts::of_hunks(hunks);
        assert_eq!(
            counts,
            PatchCounts {
                added: 3,
                removed: 1
            }
        );
        let mut action = shell("run-1", "c2", "");
        action.tool = "Edit".to_owned();
        action.input = json!({ "file_path": "src/x.rs", "old_string": "old", "new_string": "new" });
        action.patch = Some(counts);
        assert_eq!(
            action.text(),
            "tool Edit: file_path: src/x.rs; old_string: old; new_string: new \
             [patch: 3 lines added, 1 removed]"
        );
    }

    #[test]
    fn a_long_input_is_cut_at_its_cap_on_a_character() {
        let action = shell("run-1", "c3", &"é".repeat(800));
        let text = action.text();
        let input = text.strip_prefix("tool Bash: ").unwrap();
        assert_eq!(input.chars().count(), INPUT_CAP);
    }

    #[test]
    fn the_same_text_has_the_same_key_wherever_it_ran() {
        let one = shell("run-1", "c1", "git status");
        let two = shell("run-2", "c9", "git status");
        let key = |action: &DeepAction| text_key(&action.text());
        assert_eq!(key(&one), key(&two));
        assert_ne!(key(&one), key(&shell("run-1", "c1", "git log")));
    }
}
