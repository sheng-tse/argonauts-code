//! The interface, the narrator, and notifications.

mod agents_in_conversation;
mod check;
mod council;
mod look;
mod mods;
mod places;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;
use crate::names::ModelRef;

pub use agents_in_conversation::AgentsInConversation;
pub use check::{BadValue, UiProblem, drop_bad_values};
pub use council::{Bubbles, CouncilView};
pub use look::{
    ClockStyle, ColorText, Crossing, DurationStyle, FormatSettings, LevelSettings, Percent,
    PlanUsageWording, ResetStyle, State, StateStyle, StatesSettings, Theme, Threshold, TokenStyle,
};
pub use mods::{
    Border, ContextCategory, ContextDetail, ContextKeys, ContextSession, HistoryKeys, LegendMarker,
    ModLook, ModName, ModShape, ModTable, ModWidth, NoKeys, Place, PlanUsageKeys, ReceiptKeys,
    RunLimitKind, RunLimitsKeys, RunLimitsRun, RunsKeys, SpendBy, SpendKeys, UiModsSettings, Units,
};
pub use places::{
    AboveInputSettings, HiddenLine, PaidByPlace, PanelContext, UsagePeriod, UsageTab, ViewOpen,
    ViewTab, ViewsSettings, WhenQuestion,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    /// Every tool call and message.
    Full,
    /// Finished steps collapse to one line.
    #[default]
    Normal,
    /// Phases and approvals only.
    Quiet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KeyScheme {
    #[default]
    Default,
    Vim,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UiSettings {
    pub verbosity: Verbosity,
    pub narrator: NarratorMode,
    /// `/vim` saves this in your global config.
    pub keys: KeyScheme,
    pub take_over: TakeOverWhere,
    /// `fullscreen` draws in the terminal's alternate screen and leaves no trace on exit;
    /// `inline` prints the conversation into the terminal's scrollback. Read at start.
    pub screen: ScreenMode,
    /// Rebinds: action name to key, such as `agent_panel_view = "ctrl+left"`.
    pub keymap: BTreeMap<String, String>,
    /// Flat labeled lines, no boxes or spinners (`--screen-reader`, `ARGONAUTS_SCREEN_READER=1`).
    pub screen_reader: bool,
    pub motion: Motion,
    pub color: ColorChoice,
    pub bell: bool,
    /// Most rows the live area (live blocks, questions, prompt) may take, as a share of the screen.
    pub live_area_max: f64,
    /// Working-line verbs: `voyage` (`Rowing…`, `Sailed for`) or `plain` (`Working…`, `Worked
    /// for`).
    pub verbs: VerbTheme,
    /// Show the turn-end line, `✻ Sailed for 3s · done 10:42 AM`.
    pub turn_duration: bool,
    /// A tip under the working line now and then, as Claude Code shows one.
    pub tips: bool,
    /// An example prompt in the empty input until your first, as Claude Code's prompt
    /// suggestions offer one.
    pub suggestions: bool,
    /// Work going on shows in the terminal's tab or title bar too, through the OSC 9;4
    /// progress sequence that Ghostty, iTerm2, and Windows Terminal draw.
    pub progress_bar: bool,
    /// Fullscreen: text you select with the mouse is copied when you let go. Off, Ctrl+Shift+C
    /// copies it.
    pub copy_on_select: bool,
    /// Fullscreen: the conversation follows new output. Off, it holds still where you are
    /// until you scroll down or send.
    pub autoscroll: bool,
    /// Start with the agent panel open, as ← on an empty prompt opens it.
    pub agents_view: bool,
    /// How answers are written: `default`, `brief`, `explanatory`, or `learning`.
    /// `/output-style` changes it until you quit.
    pub output_style: OutputStyle,
    /// A pull request a run opened shows at the bottom line's right end, as `PR #12`.
    pub pr_footer: bool,
    /// Show a `※ recap:` after long turns and finished runs.
    pub recaps: bool,
    pub recap_after: Span,
    /// `auto` asks the terminal for its background; Claude Code's six themes, or
    /// `custom:<name>` for a theme file.
    pub theme: Theme,
    /// The accent color for the working glyph and verb and its shimmer, as `#rrggbb`. Unset is
    /// the warm gold.
    pub accent: Option<String>,
    /// A role and its color, over the theme's, such as `"bar-fill" = "#b1b9f9"`.
    pub colors: BTreeMap<String, ColorText>,
    pub format: FormatSettings,
    pub levels: LevelSettings,
    pub states: StatesSettings,
    pub above_input: AboveInputSettings,
    pub views: ViewsSettings,
    pub mods: UiModsSettings,
    /// The panel under the bottom line: one row per workflow run.
    pub panel: PanelSettings,
    /// The workflow view.
    pub workflow: WorkflowViewSettings,
    /// `ascii` for fonts that draw `●`, `◇`, `✔`, `⏎`, or `⏺` at the wrong width.
    pub glyphs: Glyphs,
    /// How the welcome header draws the mode's mascot.
    pub mascot: MascotDrawing,
    /// Each CLI's and model's logo before its name: `auto` where the terminal draws pictures in
    /// its cells, the coloured dot elsewhere, or `off` for the dot everywhere.
    pub logos: Logos,
    /// Cells for East Asian ambiguous-width characters, 1 or 2, like vim's `ambiwidth`.
    pub ambiguous_width: u8,
    /// Say once at start-up when an installed agent CLI is newer than the one Argonauts Code
    /// was checked with, when a newer version of one is out, or when one stopped reporting
    /// something Argonauts Code reads. Doctor and the Status tab say it either way.
    pub newer_cli_notice: bool,
    /// What Claude Code's plugins push: a toast, a status line, a log line.
    pub plugin_output: PluginOutput,
    /// Claude Code's You should know above the input.
    pub notices: Notices,
    /// When a crew agent shows in the conversation: once it finishes, or as it works.
    pub agents_in_conversation: AgentsInConversation,
    /// How Council's talk shows: `messages` in bubbles, as a group conversation reads in
    /// Messages, or a `transcript` of one block per message.
    pub council_view: CouncilView,
    /// How the bubbles of Council's messages are drawn.
    pub bubbles: Bubbles,
    /// Where free and paid tokens show beside the plan's: `end_line`, under a run's end line,
    /// and `usage`, as a who-paid bar in Overview's Tokens. Empty shows them nowhere.
    pub paid_by: Vec<PaidByPlace>,
}

/// How the main agent writes its answers, whatever CLI it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OutputStyle {
    /// As the agent writes them.
    #[default]
    Default,
    /// The answer first, in as few words as it takes.
    Brief,
    /// Why things work as they do, and what each choice trades off.
    Explanatory,
    /// The ideas behind the answer, step by step, with something to try.
    Learning,
}

impl OutputStyle {
    pub const ALL: [OutputStyle; 4] = [
        OutputStyle::Default,
        OutputStyle::Brief,
        OutputStyle::Explanatory,
        OutputStyle::Learning,
    ];

    /// Its name in settings and in `/output-style`.
    pub fn name(self) -> &'static str {
        match self {
            OutputStyle::Default => "default",
            OutputStyle::Brief => "brief",
            OutputStyle::Explanatory => "explanatory",
            OutputStyle::Learning => "learning",
        }
    }

    /// The style named `name`, in any case.
    pub fn find(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|style| style.name().eq_ignore_ascii_case(name.trim()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginOutput {
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
pub enum Notices {
    /// Shown when you've turned it on in Claude Code, which its session then lists.
    #[default]
    WhenEnabled,
    Off,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkflowViewSettings {
    /// What each agent cost, on its row after its tokens. The view shows no cost otherwise, as
    /// Claude Code's doesn't.
    pub row_cost: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PanelSettings {
    /// What an agent's own subagents or threads show.
    pub subagents: CliInternals,
    /// Also list running agents under each workflow row.
    pub agents: bool,
    pub max_rows: u32,
    /// How a row shows its agent's context: pills, words, or off
    pub context: PanelContext,
}

impl Default for PanelSettings {
    fn default() -> Self {
        Self {
            subagents: CliInternals::Discovered,
            agents: false,
            max_rows: 5,
            context: PanelContext::Pills,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CliInternals {
    /// Whatever the agent reports: each child's name, status, and tokens.
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

/// Whether CLIs and models show their logos, `ui.logos`. The logo drawing reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Logos {
    /// The logo where the terminal draws pictures in its cells through the kitty protocol's
    /// placeholders (Ghostty, kitty), the coloured dot and the name elsewhere.
    #[default]
    Auto,
    /// The coloured dot and the name in every terminal.
    Off,
}

/// How the welcome header draws the mode's mascot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MascotDrawing {
    /// A picture where the terminal draws one (Ghostty, Kitty, WezTerm, iTerm2), block cells
    /// elsewhere.
    Auto,
    /// Block cells in every terminal.
    Blocks,
    /// No mascot.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerbTheme {
    Voyage,
    Plain,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            verbosity: Verbosity::Full,
            narrator: NarratorMode::OnDemand,
            keys: KeyScheme::Default,
            take_over: TakeOverWhere::Here,
            screen: ScreenMode::Fullscreen,
            keymap: BTreeMap::new(),
            screen_reader: false,
            motion: Motion::Full,
            color: ColorChoice::Auto,
            bell: true,
            live_area_max: 0.6,
            verbs: VerbTheme::Voyage,
            turn_duration: true,
            tips: true,
            suggestions: true,
            progress_bar: true,
            copy_on_select: true,
            autoscroll: true,
            agents_view: false,
            output_style: OutputStyle::Default,
            pr_footer: true,
            recaps: true,
            recap_after: Span::new("2m"),
            theme: Theme::Auto,
            accent: None,
            colors: BTreeMap::new(),
            format: FormatSettings::default(),
            levels: LevelSettings::default(),
            states: StatesSettings::default(),
            above_input: AboveInputSettings::default(),
            views: ViewsSettings::default(),
            mods: UiModsSettings::default(),
            panel: PanelSettings::default(),
            workflow: WorkflowViewSettings::default(),
            glyphs: Glyphs::Unicode,
            mascot: MascotDrawing::Auto,
            logos: Logos::Auto,
            ambiguous_width: 1,
            newer_cli_notice: true,
            plugin_output: PluginOutput::Show,
            notices: Notices::WhenEnabled,
            agents_in_conversation: AgentsInConversation::WhenDone,
            council_view: CouncilView::Messages,
            bubbles: Bubbles::Auto,
            paid_by: vec![PaidByPlace::EndLine, PaidByPlace::Usage],
        }
    }
}

impl UiSettings {
    /// The mods that show in `place`, in order.
    pub fn place(&self, place: Place) -> &[ModName] {
        match place {
            Place::AboveInput => &self.above_input.mods,
            Place::Status => &self.views.status.mods,
            Place::Usage => &self.views.usage.mods,
            Place::Context => &self.views.context.mods,
            Place::Limits => &self.views.limits.mods,
        }
    }

    pub fn place_mut(&mut self, place: Place) -> &mut Vec<ModName> {
        match place {
            Place::AboveInput => &mut self.above_input.mods,
            Place::Status => &mut self.views.status.mods,
            Place::Usage => &mut self.views.usage.mods,
            Place::Context => &mut self.views.context.mods,
            Place::Limits => &mut self.views.limits.mods,
        }
    }

    /// `mod`'s settings with every fallback applied.
    pub fn the_mod(&self, the_mod: ModName) -> ModLook {
        self.mods.look(the_mod, &self.levels)
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
pub enum TakeOverWhere {
    /// Suspend Argonauts Code's screen and hand over the terminal.
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

/// How much moves on screen, `ui.motion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    /// Spinners turn, text streams word by word, and the Behaviors world moves where something
    /// happened.
    Full,
    /// Spinners stand still, text arrives a line at a time, and each of the world's motions is
    /// drawn as a still sign, such as an arrowhead or a plume.
    Reduced,
    /// As reduced, and a change you make shows at once, with no frames in between.
    Off,
}

impl Motion {
    /// Reduced or off: what would move is drawn still.
    pub fn is_reduced(self) -> bool {
        self != Motion::Full
    }

    /// Off: a change jumps to its end instead of moving there.
    pub fn skips_frames(self) -> bool {
        self == Motion::Off
    }
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
    /// Any registry model; unset uses the main agent in a read-only session.
    pub model: Option<ModelRef>,
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
    /// Only your global config may set it, since a repo could otherwise make Argonauts Code run a
    /// program of its choosing.
    pub conversation: ConversationSink,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            bell: true,
            os: true,
            conversation: ConversationSink::default(),
        }
    }
}

/// A message sent to you through a command you set, such as `hermes send` or
/// `openclaw message send`. The app runs it as an argument vector with no shell. The text is a
/// fixed template with ids and a link, and never holds agent text or a one-time code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ConversationSink {
    /// An absolute program and its arguments, with `{text}` as one whole element. Empty is off.
    pub command: Vec<String>,
    pub on: Vec<ConversationTrigger>,
}

impl Default for ConversationSink {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            on: vec![
                ConversationTrigger::NeedsYou,
                ConversationTrigger::Finished,
                ConversationTrigger::AgentDown,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationTrigger {
    NeedsYou,
    Finished,
    /// An agent's CLI went down.
    AgentDown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(text: &str) -> Result<UiSettings, toml::de::Error> {
        toml::from_str(text)
    }

    #[test]
    fn motion_is_full_reduced_or_off_and_normal_no_longer_reads() {
        assert_eq!(UiSettings::default().motion, Motion::Full);
        for (word, motion, reduced, skips) in [
            ("full", Motion::Full, false, false),
            ("reduced", Motion::Reduced, true, false),
            ("off", Motion::Off, true, true),
        ] {
            let read = ui(&format!("motion = \"{word}\"")).unwrap().motion;
            assert_eq!(read, motion);
            assert_eq!(
                (read.is_reduced(), read.skips_frames()),
                (reduced, skips),
                "{word}"
            );
        }
        assert!(ui("motion = \"normal\"").is_err());
    }

    #[test]
    fn logos_council_and_paid_by_have_their_defaults() {
        let defaults = UiSettings::default();
        assert_eq!(defaults.logos, Logos::Auto);
        assert_eq!(defaults.council_view, CouncilView::Messages);
        assert_eq!(defaults.bubbles, Bubbles::Auto);
        assert_eq!(defaults.paid_by, [PaidByPlace::EndLine, PaidByPlace::Usage]);
        let set = ui("logos = \"off\"\ncouncil_view = \"transcript\"\npaid_by = []").unwrap();
        assert_eq!(set.logos, Logos::Off);
        assert_eq!(set.council_view, CouncilView::Transcript);
        assert!(set.paid_by.is_empty());
        assert_eq!(
            ui("paid_by = [\"usage\"]").unwrap().paid_by,
            [PaidByPlace::Usage]
        );
        assert!(ui("paid_by = [\"row\"]").is_err());
        assert!(ui("logos = \"letters\"").is_err());
    }

    #[test]
    fn the_panel_bar_is_no_setting() {
        let table = toml::Table::try_from(UiSettings::default()).unwrap();
        let panel = table["panel"].as_table().unwrap();
        assert!(!panel.contains_key("bar"), "{panel:?}");
    }
}
