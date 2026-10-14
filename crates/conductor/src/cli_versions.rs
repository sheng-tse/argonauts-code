//! `~/.argonauts/state/cli-versions.json`: for each agent CLI, the newer versions already
//! noticed, so each gets one notice, per version the kinds of events it sent that its adapter
//! doesn't read yet, with counts, the built-in plugins a later version loads that its adapter
//! doesn't know, and the CLI's own commands its adapter's table doesn't list. Never their content.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use argonauts_core::{Adapter, ReportedCommand, create_private_dir_all, write_private_file};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::doctor::versions::{VersionFit, newest_tested, version_fit};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliVersions {
    /// By adapter id, such as `claude-code`.
    #[serde(default)]
    pub clis: BTreeMap<String, CliEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliEntry {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub noticed: BTreeSet<String>,
    /// By version, then by kind, such as `system/turn_summary`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unread: BTreeMap<String, BTreeMap<String, u64>>,
    /// Built-in plugins an isolated session loaded that its adapter doesn't know, by name.
    /// Isolated sessions turn them off from then on.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub builtins: BTreeSet<String>,
    /// The CLI's own commands, as its last session listed them, that its adapter's table
    /// doesn't have, by name. The `/` menu offers them in every folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub commands: BTreeMap<String, ReportedCommand>,
}

impl CliVersions {
    /// What's on disk. A missing or unreadable file is an empty one: this only shapes notices.
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            create_private_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        write_private_file(&temp, &json)?;
        fs::rename(&temp, path)
    }

    /// True the first time `version` of `cli` asks for its notice.
    pub fn first_notice(&mut self, cli: &str, version: &str) -> bool {
        self.clis
            .entry(cli.to_owned())
            .or_default()
            .noticed
            .insert(version.to_owned())
    }

    pub fn add_unread(&mut self, cli: &str, version: &str, counts: &BTreeMap<String, u64>) {
        let kinds = self
            .clis
            .entry(cli.to_owned())
            .or_default()
            .unread
            .entry(version.to_owned())
            .or_default();
        for (kind, count) in counts {
            *kinds.entry(kind.clone()).or_default() += count;
        }
    }

    /// Keeps `names` among `cli`'s learned built-ins. True when one was new.
    pub fn learn_builtins(&mut self, cli: &str, names: &[String]) -> bool {
        let known = &mut self.clis.entry(cli.to_owned()).or_default().builtins;
        let before = known.len();
        known.extend(names.iter().cloned());
        known.len() != before
    }

    /// `cli`'s learned built-ins, by name.
    pub fn builtins(&self, cli: &str) -> Vec<String> {
        self.clis
            .get(cli)
            .map(|entry| entry.builtins.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Keeps `listed` as `cli`'s own commands its table lacks, in place of the last list, since
    /// a CLI can drop one too. True when they changed.
    pub fn learn_commands(&mut self, cli: &str, listed: Vec<ReportedCommand>) -> bool {
        let listed: BTreeMap<String, ReportedCommand> = listed
            .into_iter()
            .map(|command| (command.name.clone(), command))
            .collect();
        let known = &mut self.clis.entry(cli.to_owned()).or_default().commands;
        if *known == listed {
            return false;
        }
        *known = listed;
        true
    }

    /// `cli`'s own commands its table lacked when a session last listed them, by name.
    pub fn commands(&self, cli: &str) -> Vec<ReportedCommand> {
        self.clis
            .get(cli)
            .map(|entry| entry.commands.values().cloned().collect())
            .unwrap_or_default()
    }

    /// The kinds `version` of `cli` sent that weren't read, most frequent first.
    pub fn unread(&self, cli: &str, version: &str) -> Vec<(String, u64)> {
        let mut kinds: Vec<(String, u64)> = self
            .clis
            .get(cli)
            .and_then(|entry| entry.unread.get(version))
            .map(|kinds| kinds.iter().map(|(k, n)| (k.clone(), *n)).collect())
            .unwrap_or_default();
        kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        kinds
    }

    /// Records `version` and says, once per version, when it's newer than every recording.
    /// `path` is the file to keep that in.
    pub fn notice_once(path: &Path, adapter: &dyn Adapter, version: &str) -> Option<String> {
        let notice = newer_notice(adapter, version)?;
        let mut file = Self::load(path);
        if !file.first_notice(adapter.id(), version) {
            return None;
        }
        // A notice that can't be remembered would repeat at every session, so it waits.
        file.save(path).ok()?;
        Some(notice)
    }
}

/// The notice for a CLI newer than the newest version its adapter was checked with.
pub fn newer_notice(adapter: &dyn Adapter, version: &str) -> Option<String> {
    let tested = adapter.tested_versions();
    if version_fit(version, tested) != VersionFit::Newer {
        return None;
    }
    let newest = newest_tested(tested)?;
    let cli = adapter.display_name();
    Some(format!(
        "{cli} {version} is newer than {newest}, the newest version Argonauts was checked with. \
         It should work. /doctor lists anything it sends that Argonauts doesn't read yet."
    ))
}

/// Whether `command` is one of the CLI's own that `adapter`'s table doesn't list, by name or
/// alias. A name starting with `__` is the CLI's internal plumbing, never offered.
pub fn new_command(adapter: &dyn Adapter, command: &ReportedCommand) -> bool {
    command.builtin
        && !command.name.starts_with("__")
        && !adapter.builtin_commands().iter().any(|known| {
            known.name == command.name || known.aliases.contains(&command.name.as_str())
        })
}

/// `system/turn_summary` for a line with a subtype, else its type, such as `cost_update`.
/// `None` for a line that isn't a JSON object with a type.
pub fn event_kind(raw: &Value) -> Option<String> {
    let kind = raw.get("type")?.as_str()?;
    Some(match raw.get("subtype").and_then(Value::as_str) {
        Some(subtype) => format!("{kind}/{subtype}"),
        None => kind.to_owned(),
    })
}

/// `loaded built-in plugins Argonauts doesn't know yet: cc-plugin-x, which blind roles turn off`
pub fn builtins_line(names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| {
        format!(
            "loaded built-in plugins Argonauts doesn't know yet: {}, which blind roles turn off",
            names.join(", ")
        )
    })
}

/// `sent 2 kinds of events Argonauts doesn't read yet: system/turn_summary, cost_update`
pub fn unread_line(kinds: &[(String, u64)]) -> Option<String> {
    let names: Vec<&str> = kinds.iter().map(|(kind, _)| kind.as_str()).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some(format!(
            "sent 1 kind of event Argonauts doesn't read yet: {one}"
        )),
        many => Some(format!(
            "sent {} kinds of events Argonauts doesn't read yet: {}",
            many.len(),
            many.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn kinds_and_counts_are_kept_per_version_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/cli-versions.json");
        let mut file = CliVersions::load(&path);
        assert_eq!(file, CliVersions::default());
        let counts = BTreeMap::from([("cost_update".to_owned(), 1), ("system/x".to_owned(), 3)]);
        file.add_unread("claude-code", "2.1.291", &counts);
        file.add_unread("claude-code", "2.1.291", &counts);
        assert!(file.first_notice("claude-code", "2.1.291"));
        assert!(!file.first_notice("claude-code", "2.1.291"));
        file.save(&path).unwrap();

        let back = CliVersions::load(&path);
        assert_eq!(
            back.unread("claude-code", "2.1.291"),
            [("system/x".to_owned(), 6), ("cost_update".to_owned(), 2)]
        );
        assert!(back.unread("claude-code", "2.1.289").is_empty());
        assert!(back.unread("codex", "2.1.291").is_empty());
        assert_eq!(
            unread_line(&back.unread("claude-code", "2.1.291")).as_deref(),
            Some("sent 2 kinds of events Argonauts doesn't read yet: system/x, cost_update")
        );
        assert!(unread_line(&[]).is_none());
        // Nothing but kinds and counts.
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("content"), "{text}");
    }

    /// With recordings up to 2.1.295: that version is checked, a later patch newer with one
    /// notice that holds across restarts, and the next minor not checked.
    #[test]
    fn a_newer_patch_is_noticed_once_per_version() {
        use argonauts_adapter_claude_code::ClaudeCodeAdapter;

        let adapter = ClaudeCodeAdapter;
        let tested = adapter.tested_versions();
        assert_eq!(version_fit("2.1.295", tested), VersionFit::Tested);
        assert_eq!(version_fit("2.1.297", tested), VersionFit::Newer);
        assert_eq!(version_fit("2.2.0", tested), VersionFit::Untested);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/cli-versions.json");
        assert!(CliVersions::notice_once(&path, &adapter, "2.1.295").is_none());
        assert!(CliVersions::notice_once(&path, &adapter, "2.2.0").is_none());
        let notice = CliVersions::notice_once(&path, &adapter, "2.1.297").unwrap();
        assert_eq!(
            notice,
            "Claude Code 2.1.297 is newer than 2.1.295, the newest version Argonauts was \
             checked with. It should work. /doctor lists anything it sends that Argonauts \
             doesn't read yet."
        );
        // The file is read again each time, as after a restart.
        assert!(CliVersions::notice_once(&path, &adapter, "2.1.297").is_none());
        assert!(CliVersions::notice_once(&path, &adapter, "2.1.298").is_some());
    }

    /// Claude Code lists `/heapdump` as its own, which its table doesn't have; `/compact` and
    /// the alias `/reset` are in the table, and `__remote-workflow` is plumbing.
    #[test]
    fn the_clis_own_commands_its_table_lacks_are_kept_as_last_listed() {
        use argonauts_adapter_claude_code::ClaudeCodeAdapter;

        let own = |name: &str| ReportedCommand {
            name: name.to_owned(),
            description: format!("{name} it"),
            argument_hint: String::new(),
            aliases: Vec::new(),
            builtin: true,
        };
        let adapter = ClaudeCodeAdapter;
        let plugin = ReportedCommand {
            builtin: false,
            ..own("pr-tools:review")
        };
        let listed = [
            own("compact"),
            own("reset"),
            own("heapdump"),
            own("__remote-workflow"),
            plugin,
        ];
        let new: Vec<ReportedCommand> = listed
            .iter()
            .filter(|command| new_command(&adapter, command))
            .cloned()
            .collect();
        assert_eq!(new, [own("heapdump")]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/cli-versions.json");
        let mut file = CliVersions::load(&path);
        assert!(file.learn_commands("claude-code", new.clone()));
        assert!(!file.learn_commands("claude-code", new));
        file.save(&path).unwrap();
        assert_eq!(
            CliVersions::load(&path).commands("claude-code"),
            [own("heapdump")]
        );
        // A later version that drops it drops it here too.
        assert!(file.learn_commands("claude-code", Vec::new()));
        assert!(file.commands("claude-code").is_empty());
    }

    #[test]
    fn kinds_name_the_type_and_subtype() {
        assert_eq!(
            event_kind(&json!({"type": "system", "subtype": "turn_summary", "text": "x"})),
            Some("system/turn_summary".to_owned())
        );
        assert_eq!(
            event_kind(&json!({"type": "cost_update"})),
            Some("cost_update".to_owned())
        );
        assert_eq!(event_kind(&json!("not json")), None);
        assert_eq!(event_kind(&json!({"subtype": "x"})), None);
    }
}
