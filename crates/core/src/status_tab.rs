//! What the Overview's Status tab draws: what runs here and how it is set up, for every CLI at
//! once. Code reads it all from the settings, the CLIs' checks, the conversation and the runs;
//! no model is ever asked. It is the panel's own answer, so nothing in it reaches the chat.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::api::{RunSummary, StatusSummary};
use crate::app::ConfigEntry;
use crate::names::WorkerRef;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StatusTabView {
    /// Argonauts' own version, such as `0.0.1`.
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Conversation>,
    pub folder: Folder,
    /// The session you talk to. `None` before the app knows its CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<MainSession>,
    pub workflow: WorkflowSetup,
    /// The approval mode in effect, such as `supervised`.
    pub mode: String,
    /// Every CLI Argonauts knows, installed or not, the main session's first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clis: Vec<CliStatus>,
    pub settings: SettingsInEffect,
    /// This folder's runs, the newest first. Empty is one quiet line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunSummary>,
    /// The run the screen follows, or else the newest, as `/status` sums it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<StatusSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// Such as `chat-014`.
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MainSession {
    pub worker: WorkerRef,
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
    /// The preset, such as `solo`.
    pub name: String,
    pub ultracode: bool,
    /// `workflow_size`, such as `medium`.
    pub size: String,
}

/// Said of a CLI that is signed in when nothing it says names the address.
pub const UNNAMED_ACCOUNT: &str = "signed in, address not shown";

/// One agent CLI as Doctor last found it, with who it is signed in as. The account is drawn
/// on your own screen only and is never sent anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CliStatus {
    /// Such as `claude-code`, which picks its colour.
    pub id: String,
    /// Such as `Claude Code`.
    pub name: String,
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
    /// The plan as the CLI names it, such as `Max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// How its version stands against the ones Argonauts was checked with, such as `checked`
    /// or `newer than checked, works`; for one that isn't installed, how to get it.
    pub note: String,
}

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
