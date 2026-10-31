//! `~/.argonauts/state/cli-versions.json`: for each agent CLI, the newer versions already
//! noticed, so each gets one notice, per version the kinds of events it sent that its adapter
//! doesn't read yet, with counts, the built-in plugins a later version loads that its adapter
//! doesn't know, and the CLI's own commands its adapter's table doesn't list. Never their content.
//!
//! It also keeps what reminding you to update needs: the newest version out where each CLI
//! was installed from, asked once a day; which newer versions the start-up line has named,
//! so each is named once; and, per version, what its start-up message lacked of the fields
//! Argonauts Code reads.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use argonauts_core::agents::adapter::StartupField;
use argonauts_core::app::status_tab::{CliVersionCheck, VersionStanding};
use argonauts_core::{Adapter, ReportedCommand, create_private_dir_all, write_private_file};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::doctor::AgentCheck;
use crate::doctor::newest::{install_source, update_command};
use crate::doctor::standing::{is_newer, oldest_supported, standing};
use crate::doctor::versions::newest_tested;

/// How long an answer about the newest version is good for.
const NEWEST_GOOD_FOR: time::Duration = time::Duration::days(1);

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
    /// The newest version out where it was installed from, as last asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest: Option<NewestAsked>,
    /// The newer versions the start-up line has named, so each is named once.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub reminded: BTreeSet<String>,
    /// By version, the fields Argonauts Code reads that its start-up message lacked, by their
    /// place in the message. A version with an empty set was compared and lacked none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub startup: BTreeMap<String, BTreeSet<String>>,
    /// The missing fields already said at start-up, as `version field`, so each is said once.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub warned: BTreeSet<String>,
}

/// What an install source last said the newest version is, and when it was asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewestAsked {
    /// `None` when it couldn't say. That still counts as asked, so a source that is down is
    /// asked once a day like any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub asked: OffsetDateTime,
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

    /// Whether `cli`'s install source is due to be asked again: never asked, or a day ago.
    pub fn newest_due(&self, cli: &str, now: OffsetDateTime) -> bool {
        self.clis
            .get(cli)
            .and_then(|entry| entry.newest.as_ref())
            .is_none_or(|newest| now - newest.asked >= NEWEST_GOOD_FOR)
    }

    /// Keeps what `cli`'s install source said at `now`.
    pub fn asked_newest(&mut self, cli: &str, version: Option<String>, now: OffsetDateTime) {
        self.clis.entry(cli.to_owned()).or_default().newest = Some(NewestAsked {
            version,
            asked: now,
        });
    }

    /// The newest version of `cli` out where it was installed from, once that has been asked.
    pub fn newest(&self, cli: &str) -> Option<&str> {
        self.clis.get(cli)?.newest.as_ref()?.version.as_deref()
    }

    /// True the first time the start-up line asks to name `version` of `cli` as out.
    pub fn first_reminder(&mut self, cli: &str, version: &str) -> bool {
        self.clis
            .entry(cli.to_owned())
            .or_default()
            .reminded
            .insert(version.to_owned())
    }

    /// Keeps which fields `version` of `cli` left out of its start-up message. The fields of
    /// every session count, since a field may come only with some of them. True when
    /// something new was learned.
    pub fn saw_startup(&mut self, cli: &str, version: &str, missing: &[&str]) -> bool {
        let versions = &mut self.clis.entry(cli.to_owned()).or_default().startup;
        let new_version = !versions.contains_key(version);
        let known = versions.entry(version.to_owned()).or_default();
        let before = known.len();
        known.extend(missing.iter().map(|field| (*field).to_owned()));
        new_version || known.len() != before
    }

    /// The fields `version` of `cli` left out of its start-up message. `None` until a session
    /// of that version has started.
    pub fn startup_missing(&self, cli: &str, version: &str) -> Option<Vec<String>> {
        let missing = self.clis.get(cli)?.startup.get(version)?;
        Some(missing.iter().cloned().collect())
    }

    /// True the first time `field` missing from `version` of `cli` asks to be said.
    pub fn first_warning(&mut self, cli: &str, version: &str, field: &str) -> bool {
        self.clis
            .entry(cli.to_owned())
            .or_default()
            .warned
            .insert(format!("{version} {field}"))
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

/// What Doctor found of an installed CLI that its version check starts from.
#[derive(Debug, Clone, Copy)]
pub struct Installed<'a> {
    pub version: Option<&'a str>,
    /// Where its program is, which says where it was installed from.
    pub path: Option<&'a Path>,
    /// Its `--help` offers every flag Argonauts Code passes.
    pub flags_whole: bool,
}

impl<'a> Installed<'a> {
    pub fn of(check: &'a AgentCheck) -> Self {
        Self {
            version: check.version.as_deref(),
            path: check.path.as_deref(),
            flags_whole: check.problem.is_none(),
        }
    }
}

/// How an installed CLI's version stands, whether everything Argonauts Code reads from it is
/// there, and what there is to update to, from what `seen` holds of it.
pub fn version_check(
    adapter: &dyn Adapter,
    installed: Installed<'_>,
    seen: &CliVersions,
) -> CliVersionCheck {
    let tested = adapter.tested_versions();
    let version = installed.version;
    let standing = standing(version, tested, adapter.broken_versions());
    let newest = seen.newest(adapter.id()).map(str::to_owned);
    let source = installed
        .path
        .and_then(|path| install_source(path, &adapter.install_sources()));
    // A CLI with a start-up message is only known to give everything once one was read.
    let startup_whole = adapter.startup_message().is_none_or(|_| {
        version
            .and_then(|version| seen.startup_missing(adapter.id(), version))
            .is_some_and(|missing| missing.is_empty())
    });
    CliVersionCheck {
        standing,
        works: installed.flags_whole && startup_whole,
        checked_with: newest_tested(tested).map(str::to_owned),
        oldest_supported: standing
            .needs_update()
            .then(|| oldest_supported(tested).map(str::to_owned))
            .flatten(),
        newer_is_out: newest
            .as_deref()
            .zip(version)
            .is_some_and(|(newest, installed)| is_newer(newest, installed)),
        newest,
        update_command: update_command(adapter, source.as_ref()),
    }
}

/// What a CLI's start-up message lacks of the fields its adapter reads, when `line` is that
/// message. `None` for any other line.
pub fn startup_lacks(adapter: &dyn Adapter, line: &str) -> Option<Vec<&'static StartupField>> {
    let startup = adapter.startup_message()?;
    if !line.contains(startup.marker) {
        return None;
    }
    let message: Value = serde_json::from_str(line).ok()?;
    let is_startup = startup
        .is
        .iter()
        .all(|(pointer, value)| message.pointer(pointer).and_then(Value::as_str) == Some(*value));
    is_startup.then(|| {
        startup
            .fields
            .iter()
            .filter(|field| message.pointer(field.pointer).is_none_or(Value::is_null))
            .collect()
    })
}

/// `Claude Code 2.1.297 no longer reports its context, so /context shows less until Argonauts
/// Code is updated`, for each field of `missing` its adapter reads, with the field's place.
pub fn startup_warnings(
    adapter: &dyn Adapter,
    version: &str,
    missing: &[String],
) -> Vec<(&'static str, String)> {
    let Some(startup) = adapter.startup_message() else {
        return Vec::new();
    };
    let cli = adapter.display_name();
    startup
        .fields
        .iter()
        .filter(|field| missing.iter().any(|missing| missing == field.pointer))
        .map(|field| {
            let warning = format!(
                "{cli} {version} no longer reports {}, so {} until Argonauts Code is updated",
                field.what, field.without
            );
            (field.pointer, warning)
        })
        .collect()
}

/// `Claude Code 2.1.297 is out · claude update · shown once for this version`: the one line
/// start-up says of a newer version, the first time it is known.
pub fn out_line(name: &str, check: &CliVersionCheck) -> Option<String> {
    let newest = check.newest.as_deref().filter(|_| check.newer_is_out)?;
    let mut line = format!("{name} {newest} is out");
    if let Some(command) = &check.update_command {
        line.push_str(" · ");
        line.push_str(command);
    }
    line.push_str(" · shown once for this version");
    Some(line)
}

/// The notice for a CLI newer than the newest version its adapter was checked with, by the
/// rule /doctor uses: any later version of the same major is newer than checked, and a later
/// major is said as one.
pub fn newer_notice(adapter: &dyn Adapter, version: &str) -> Option<String> {
    let tested = adapter.tested_versions();
    let newest = newest_tested(tested)?;
    let cli = adapter.display_name();
    match standing(Some(version), tested, adapter.broken_versions()) {
        VersionStanding::Newer => Some(format!(
            "{cli} {version} is newer than {newest}, the newest version Argonauts Code was checked \
             with. It should work. /doctor lists anything it sends that Argonauts Code doesn't \
             read yet."
        )),
        VersionStanding::NewMajor => Some(format!(
            "{cli} {version} is a new major version, after {newest}, the newest version Argonauts \
             Code was checked with. /doctor says what it found."
        )),
        _ => None,
    }
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

/// `loaded built-in plugins Argonauts Code doesn't know yet: cc-plugin-x, which blind roles turn
/// off`
pub fn builtins_line(names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| {
        format!(
            "loaded built-in plugins Argonauts Code doesn't know yet: {}, which blind roles turn off",
            names.join(", ")
        )
    })
}

/// `sent 2 kinds of events Argonauts Code doesn't read yet: system/turn_summary, cost_update`
pub fn unread_line(kinds: &[(String, u64)]) -> Option<String> {
    let names: Vec<&str> = kinds.iter().map(|(kind, _)| kind.as_str()).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some(format!(
            "sent 1 kind of event Argonauts Code doesn't read yet: {one}"
        )),
        many => Some(format!(
            "sent {} kinds of events Argonauts Code doesn't read yet: {}",
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
            Some("sent 2 kinds of events Argonauts Code doesn't read yet: system/x, cost_update")
        );
        assert!(unread_line(&[]).is_none());
        // Nothing but kinds and counts.
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("content"), "{text}");
    }

    /// With recordings up to 2.1.295: that version is checked, and a later patch or minor is
    /// newer, with one notice per version that holds across restarts.
    #[test]
    fn a_newer_patch_is_noticed_once_per_version() {
        use argonauts_adapter_claude_code::ClaudeCodeAdapter;

        use crate::doctor::versions::{VersionFit, version_fit};

        let adapter = ClaudeCodeAdapter;
        let tested = adapter.tested_versions();
        // The newest recording's version, and patches after it.
        let newest = *tested
            .last()
            .expect("Claude Code was recorded with a version");
        let (minor, patch) = newest.rsplit_once('.').expect("a three-part version");
        let patch: u32 = patch.parse().expect("a patch number");
        let later = |by: u32| format!("{minor}.{}", patch + by);
        assert_eq!(version_fit(newest, tested), VersionFit::Tested);
        assert_eq!(version_fit(&later(2), tested), VersionFit::Newer);
        assert_eq!(version_fit("2.99.0", tested), VersionFit::Newer);
        assert_eq!(version_fit("3.0.0", tested), VersionFit::Untested);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/cli-versions.json");
        assert!(CliVersions::notice_once(&path, &adapter, newest).is_none());
        // A later minor is newer too, as /doctor says.
        assert!(CliVersions::notice_once(&path, &adapter, "2.99.0").is_some());
        let notice = CliVersions::notice_once(&path, &adapter, &later(2)).unwrap();
        assert_eq!(
            notice,
            format!(
                "Claude Code {} is newer than {newest}, the newest version Argonauts Code was \
                 checked with. It should work. /doctor lists anything it sends that Argonauts \
                 Code doesn't read yet.",
                later(2)
            )
        );
        // The file is read again each time, as after a restart.
        assert!(CliVersions::notice_once(&path, &adapter, &later(2)).is_none());
        assert!(CliVersions::notice_once(&path, &adapter, &later(3)).is_some());
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
