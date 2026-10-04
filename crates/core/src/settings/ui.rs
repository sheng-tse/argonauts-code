//! The interface, the narrator, and notifications.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;
use crate::names::WorkerRef;
use crate::{KeyScheme, Verbosity};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UiSettings {
    pub verbosity: Verbosity,
    pub narrator: NarratorMode,
    /// `/vim` saves this in your global config.
    pub keys: KeyScheme,
    pub takeover: TakeoverWhere,
    /// `fullscreen` draws in the terminal's alternate screen and leaves no trace on exit;
    /// `inline` prints the chat into the terminal's scrollback. Read at start.
    pub screen: ScreenMode,
    /// Rebinds: action name to key, such as `crew_view = "ctrl+left"`.
    pub keymap: BTreeMap<String, String>,
    /// Flat labeled lines, no boxes or spinners (`--screen-reader`, `ARGONAUTS_SCREEN_READER=1`).
    pub screen_reader: bool,
    pub motion: Motion,
    pub color: ColorChoice,
    pub bell: bool,
    /// Most rows the live area (live blocks, cards, prompt) may take, as a share of the screen.
    pub live_area_max: f64,
    /// Working-line verbs: `voyage` (`Rowing…`, `Sailed for`) or `plain` (`Working…`, `Worked for`).
    pub verbs: VerbTheme,
    /// Show the turn-end line, `✻ Sailed for 3s · done 10:42 AM`.
    pub turn_duration: bool,
    /// Show a `※ recap:` after long turns and finished runs.
    pub recaps: bool,
    pub recap_after: Span,
    /// `auto` asks the terminal for its background; `dark` or `light` forces a palette.
    pub theme: Theme,
    /// The accent color for the working glyph and verb, as `#rrggbb`. Unset is the warm gold.
    pub accent: Option<String>,
    /// The panel under the mode line: one row per workflow run.
    pub panel: PanelSettings,
    /// The workflow view.
    pub workflow: WorkflowViewSettings,
    /// `ascii` for fonts that draw `●`, `◇`, `✔`, `⏎`, or `⏺` at the wrong width.
    pub glyphs: Glyphs,
    /// Cells for East Asian ambiguous-width characters, 1 or 2, like vim's `ambiwidth`.
    pub ambiguous_width: u8,
    /// Say once when an installed agent CLI is newer than the one Argonauts was checked with.
    pub newer_cli_notice: bool,
    /// What Claude Code's mods push: a toast, a status line, a log line.
    pub mod_output: ModOutput,
    /// Claude Code's You should know above the input.
    pub heads_up: HeadsUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModOutput {
    /// Where Claude Code shows it: a toast on the notice line, a status line under the input,
    /// a log line in the transcript.
    #[default]
    Show,
    /// Each as a dim line in the transcript.
    Transcript,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeadsUp {
    /// Shown when you've turned it on in Claude Code, which its session then lists.
    #[default]
    WhenEnabled,
    Off,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkflowViewSettings {
    /// A cost column on agent rows, before the time. Cost shows in the header, the panel row,
    /// and the agent view either way.
    pub row_cost: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PanelSettings {
    /// What an agent's own subagents or threads show.
    pub vendor_internals: VendorInternals,
    /// Also list running agents under each workflow row.
    pub agents: bool,
    pub max_rows: u32,
}

impl Default for PanelSettings {
    fn default() -> Self {
        Self {
            vendor_internals: VendorInternals::Discovered,
            agents: false,
            max_rows: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VendorInternals {
    /// Whatever the vendor reports: each child's name, status, and tokens.
    Discovered,
    /// One line.
    Summary,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Glyphs {
    Unicode,
    /// `*`, `<>`, `v`, `>`, and `o` in their place.
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerbTheme {
    Voyage,
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Auto,
    Dark,
    Light,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            verbosity: Verbosity::Full,
            narrator: NarratorMode::OnDemand,
            keys: KeyScheme::Standard,
            takeover: TakeoverWhere::Here,
            screen: ScreenMode::Fullscreen,
            keymap: BTreeMap::new(),
            screen_reader: false,
            motion: Motion::Normal,
            color: ColorChoice::Auto,
            bell: true,
            live_area_max: 0.6,
            verbs: VerbTheme::Voyage,
            turn_duration: true,
            recaps: true,
            recap_after: Span::new("2m"),
            theme: Theme::Auto,
            accent: None,
            panel: PanelSettings::default(),
            workflow: WorkflowViewSettings::default(),
            glyphs: Glyphs::Unicode,
            ambiguous_width: 1,
            newer_cli_notice: true,
            mod_output: ModOutput::Show,
            heads_up: HeadsUp::WhenEnabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NarratorMode {
    Off,
    /// Only for `/ask` and `s` in `/status`.
    OnDemand,
    /// Also a short update every interval when something changed.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TakeoverWhere {
    /// Suspend Argonauts' screen and hand over the terminal.
    Here,
    TmuxSplit,
    /// A new terminal tab, where supported.
    NewTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScreenMode {
    /// The whole terminal, like Claude Code's fullscreen mode. The conversation scrolls inside
    /// the app, and the shell comes back as it was.
    Fullscreen,
    /// Under the shell prompt, with the conversation going up into the terminal's scrollback.
    Inline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    /// Spinners and word-by-word streaming.
    Normal,
    /// Static spinners, a line at a time.
    Reduced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColorChoice {
    /// Follow `NO_COLOR` and `TERM=dumb`.
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct NarratorSettings {
    /// Any registry model; unset uses the default agent in a read-only session.
    pub model: Option<WorkerRef>,
    /// At most one live update per interval, only when something changed.
    pub interval: Span,
    /// Per run: `"0.50 usd"` or `"200k tokens"`.
    pub spend_cap: String,
}

impl Default for NarratorSettings {
    fn default() -> Self {
        Self {
            model: None,
            interval: Span::new("45s"),
            spend_cap: "0.50 usd".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct NotificationSettings {
    pub bell: bool,
    /// An OS notification when something needs you and the app isn't focused.
    pub os: bool,
    /// Only your global config may set it, since a repo could otherwise make Argonauts run a
    /// program of its choosing.
    pub chat: ChatSink,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            bell: true,
            os: true,
            chat: ChatSink::default(),
        }
    }
}

/// A chat message sent through a command you set, such as `hermes send` or
/// `openclaw message send`. The app runs it as an argument vector with no shell. The text is a
/// fixed template with ids and a link, and never holds agent text or a one-time code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ChatSink {
    /// An absolute program and its arguments, with `{text}` as one whole element. Empty is off.
    pub command: Vec<String>,
    pub on: Vec<ChatTrigger>,
}

impl Default for ChatSink {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            on: vec![
                ChatTrigger::NeedsYou,
                ChatTrigger::Finished,
                ChatTrigger::VendorDown,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatTrigger {
    NeedsYou,
    Finished,
    VendorDown,
}
