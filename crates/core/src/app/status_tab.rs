//! What the usage panel's Status tab draws: what runs here and how it is set up, for every CLI at
//! once. Code reads it all from the settings, the CLIs' checks, the conversation and the runs;
//! no model is ever asked. It is the panel's own answer, so nothing in it reaches the conversation.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::app::ConfigEntry;
use crate::base::names::ModelRef;
use crate::models::limits::{HealthState, ModelState};
use crate::runs::api::{RunSummary, StatusSummary};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatusTabView {
    /// Argonauts Code's own version, such as `0.0.1`.
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Conversation>,
    pub folder: Folder,
    /// The session you talk to. `None` before the app knows its CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<MainSession>,
    /// How that session carries on in its CLI's own screen, once the CLI has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_cli: Option<crate::agents::cli_sessions::InItsCli>,
    pub workflow: WorkflowSetup,
    /// The approval mode in effect, such as `supervised`.
    pub mode: String,
    /// Every CLI Argonauts Code knows, installed or not, the main session's first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clis: Vec<CliStatus>,
    pub settings: SettingsInEffect,
    /// This folder's runs, the newest first. Empty is one quiet line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunSummary>,
    /// The run the screen follows, or else the newest, as `/status` sums it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<StatusSummary>,
    /// How each CLI's models stand, each on its own.
    #[serde(default)]
    pub health: HealthRows,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// Such as `conversation-014`.
    pub id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Folder {
    /// With `~` for home.
    pub shown: String,
    /// Inside a git repository. Without one, workflows keep a hidden history.
    pub git: bool,
    /// What that hidden history takes on disk, such as `18 MB`, once there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_size: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MainSession {
    pub worker: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Tokens in its context now, once it has reported them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkflowSetup {
    /// The workflow, such as `captain`.
    pub name: String,
    pub ultracode: bool,
    /// `workflow_size`, such as `medium`.
    pub size: String,
}

/// Said of a CLI that is signed in when nothing it says names the address.
pub const UNNAMED_ACCOUNT: &str = "signed in, address not shown";

/// Said of a CLI that keeps providers of its own when what it printed couldn't be read as
/// its list: the command of its own that shows them.
pub fn providers_unread(command: &str) -> String {
    format!("couldn't list its providers · {command} auth list shows them")
}

/// Said of such a CLI until it has answered.
pub fn asking_for_providers(name: &str) -> String {
    format!("asking {name}…")
}

/// One agent CLI as Doctor last found it, with who it is signed in as. The account is drawn
/// on your own screen only and is never sent anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CliStatus {
    /// Such as `claude-code`, which picks its colour.
    pub id: String,
    /// Such as `Claude Code`.
    pub name: String,
    /// The command that starts it, such as `claude`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// `None` when it isn't installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub installed: bool,
    /// The main session runs on it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub main: bool,
    /// The address it is signed in with, as the CLI itself reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Its login check says it isn't signed in. Neither this nor an account means it isn't
    /// known.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub signed_out: bool,
    /// It asked whether to trust this folder before it showed who is signed in, which only
    /// you answer, by starting it here once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untrusted_folder: bool,
    /// The plan as the CLI names it, such as `Max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// What it can use, for a CLI that keeps a list of providers in place of one account, as
    /// OpenCode does. `Some` and empty when none is set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<CliProvider>>,
    /// It keeps a list of providers in place of one account, so nothing is ever said of an
    /// address. `providers` is `None` until the list is read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lists_providers: bool,
    /// Its list of providers was asked for, and nothing it printed could be read as one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub providers_unread: bool,
    /// The model it uses when none is named, such as `GLM 5 on OpenRouter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// How its version stands against the ones Argonauts Code was checked with.
    #[serde(default)]
    pub check: CliVersionCheck,
}

/// One provider a CLI can use, and who pays for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliProvider {
    /// Such as `OpenRouter`.
    pub name: String,
    pub billing: ProviderBilling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderBilling {
    /// A key of yours, charged by use.
    ApiKey,
    /// A runtime on this computer, which costs nothing.
    Local,
    /// A plan you signed in to.
    Plan,
}

impl ProviderBilling {
    pub fn words(self) -> &'static str {
        match self {
            Self::ApiKey => "API key, billed",
            Self::Local => "local",
            Self::Plan => "plan sign-in",
        }
    }
}

/// How an installed CLI's version stands against the versions Argonauts Code was checked with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStanding {
    /// From the oldest version it was recorded with to the newest.
    Checked,
    /// Later than the newest one checked, in the same major version.
    Newer,
    /// A later major version, where anything may have changed.
    NewMajor,
    /// Older than the oldest version supported, so an update is needed.
    Older,
    /// A version known not to work, so an update is needed.
    Broken,
    /// Its version couldn't be read.
    #[default]
    Unknown,
}

impl VersionStanding {
    pub fn words(self) -> &'static str {
        match self {
            Self::Checked => "checked",
            Self::Newer => "newer than checked",
            Self::NewMajor => "new major version",
            Self::Older => "older than supported",
            Self::Broken => "known not to work",
            Self::Unknown => "unknown version",
        }
    }

    /// Runs with it may misread what it does until it is updated.
    pub fn needs_update(self) -> bool {
        matches!(self, Self::Older | Self::Broken)
    }
}

/// A CLI's version as Doctor and the Status tab say it: how it stands, whether what Argonauts
/// Code reads from it is all there, and what there is to update to.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CliVersionCheck {
    pub standing: VersionStanding,
    /// Its `--help` has every flag Argonauts Code passes, and its start-up message every field
    /// Argonauts Code reads, where one has been seen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub works: bool,
    /// The newest version Argonauts Code was checked with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_with: Option<String>,
    /// The oldest version supported, for one that is older.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_supported: Option<String>,
    /// The newest version out where this one was installed from, once that has been asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest: Option<String>,
    /// A version newer than the installed one is out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub newer_is_out: bool,
    /// The command that updates it, such as `claude update` or `brew upgrade codex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_command: Option<String>,
}

/// What Doctor and the Status tab say beside a version's standing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionNote {
    pub kind: VersionNoteKind,
    /// Such as `2.1.297 is out`.
    pub words: String,
    /// The command to run, drawn after the words, such as `claude update`.
    pub command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionNoteKind {
    /// An update is needed before runs can rely on it.
    Needed,
    /// A newer version is out.
    Out,
    /// A newer version than checked gives everything the checked one did.
    Matches,
    UpToDate,
    /// A new major version, compared with the checked one as it starts.
    Compared,
}

impl CliVersionCheck {
    /// `checked · works`, `newer than checked · works`, `older than supported`.
    pub fn state(&self) -> String {
        let standing = self.standing.words();
        let settled = matches!(
            self.standing,
            VersionStanding::Checked | VersionStanding::Newer
        );
        if self.works && settled {
            format!("{standing} · works")
        } else {
            standing.to_owned()
        }
    }

    /// The one thing worth saying beside the state: the update that is needed, the version
    /// that is out, or what the comparison found.
    pub fn note(&self) -> Option<VersionNote> {
        let note = |kind, words: String, command: Option<&String>| VersionNote {
            kind,
            words,
            command: command.cloned(),
        };
        let command = self.update_command.as_ref();
        match self.standing {
            VersionStanding::Older | VersionStanding::Broken => {
                let version = self.oldest_supported.as_ref().or(self.newest.as_ref())?;
                Some(note(
                    VersionNoteKind::Needed,
                    format!("{version} needed"),
                    command,
                ))
            }
            VersionStanding::Unknown => None,
            _ if self.newer_is_out => {
                let newest = self.newest.as_ref()?;
                Some(note(
                    VersionNoteKind::Out,
                    format!("{newest} is out"),
                    command,
                ))
            }
            VersionStanding::NewMajor => {
                let checked = self.checked_with.as_ref()?;
                Some(note(
                    VersionNoteKind::Compared,
                    format!("checked with {checked} · compared at start-up"),
                    None,
                ))
            }
            VersionStanding::Newer if self.works => {
                let checked = self.checked_with.as_ref()?;
                Some(note(
                    VersionNoteKind::Matches,
                    format!("its flags and start-up message match {checked}"),
                    None,
                ))
            }
            _ if self.newest.is_some() => Some(note(
                VersionNoteKind::UpToDate,
                "up to date".to_owned(),
                None,
            )),
            _ => None,
        }
    }
}

/// `name version`, the state and the note in the columns Doctor lists the CLIs in:
/// `Claude Code 2.1.296       checked · works               2.1.297 is out · claude update`.
pub fn version_row(name: &str, version: &str, check: &CliVersionCheck) -> String {
    let mut row = format!(
        "{:<name_width$}{:<state_width$}",
        format!("{name} {version}"),
        check.state(),
        name_width = VERSION_ROW_NAMES,
        state_width = VERSION_ROW_STATES,
    );
    if let Some(note) = check.note() {
        row.push_str(&note.words);
        if let Some(command) = &note.command {
            row.push_str(" · ");
            row.push_str(command);
        }
    }
    row.trim_end().to_owned()
}

/// The columns of a [`version_row`]: the name with its version, then the state.
pub const VERSION_ROW_NAMES: usize = 26;
pub const VERSION_ROW_STATES: usize = 30;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SettingsInEffect {
    /// Each settings file that sets anything, in the order they apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SettingsFile>,
    pub mods_built_in: usize,
    /// Your own mods, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods_yours: Vec<String>,
    /// What the main session can call to act outside itself, in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// You turned the main session's tools off, with `main_session.tools = false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tools_off: bool,
    /// The MCP servers the main session's CLI brought from its own settings, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<String>>,
    /// Every key set anywhere but the built-in defaults, with the layer it came from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<ConfigEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsFile {
    /// With `~` for home, or relative to the folder.
    pub path: String,
    /// The layer it is, as [`ConfigEntry::layer`] names it.
    pub layer: String,
    pub keys: usize,
}

/// What the Status tab says of health: each CLI's models with how each stands, and the spans
/// its states are counted over.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HealthRows {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clis: Vec<CliHealth>,
    /// `health.window` in seconds: how long a degraded model must be quiet. `None` for `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_secs: Option<u64>,
    /// `health.cooldown` in seconds: how long a model that is out rests before its test task.
    #[serde(default)]
    pub cooldown_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliHealth {
    /// Such as `claude-code`.
    pub id: String,
    pub name: String,
    pub installed: bool,
    /// The main session runs on it.
    pub main: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelStanding>,
}

/// One model's row of the Status tab's health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStanding {
    /// Such as `Opus 5.5`.
    pub name: String,
    pub state: Standing,
    /// Why it stands there. Empty for a model nothing is held against.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// The job it does meanwhile for a model that is out, which a model that is fine says
    /// where another gives its reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub covers: Option<Covers>,
    /// When it's looked at again: its test task, or the end of its quiet window.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub next_check: Option<OffsetDateTime>,
}

/// A job one model does in the place of the model that was asked, drawn `↷ review:due-tests
/// for Opus 5.5`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Covers {
    /// The job's label, such as `review:due-tests`.
    pub job: String,
    /// The CLI of the model that was asked, such as `claude-code`.
    pub cli: String,
    /// That model's name, such as `Opus 5.5`.
    pub model: String,
}

/// How a model stands, as its row says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    Healthy,
    /// New work goes to another model first.
    Degraded,
    /// Out on its own: a fallback takes its work, or the work waits.
    Overloaded,
    /// Every model on its CLI is out.
    Down,
    /// One test task decides whether it's healthy or out.
    Testing,
}

impl Standing {
    pub const ALL: [Standing; 5] = [
        Self::Healthy,
        Self::Degraded,
        Self::Overloaded,
        Self::Down,
        Self::Testing,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Overloaded => "overloaded",
            Self::Down => "down",
            Self::Testing => "testing",
        }
    }

    /// How a model stands, from its CLI's state and its own: a CLI that is down or under test
    /// speaks for every model on it.
    pub fn of(cli: HealthState, model: ModelState) -> Self {
        match (cli, model) {
            (HealthState::Down, _) => Self::Down,
            (HealthState::Testing, _) | (_, ModelState::Testing) => Self::Testing,
            (_, ModelState::Out) => Self::Overloaded,
            (_, ModelState::Degraded) => Self::Degraded,
            (_, ModelState::Healthy) => Self::Healthy,
        }
    }
}
