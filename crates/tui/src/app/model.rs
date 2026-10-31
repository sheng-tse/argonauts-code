//! Everything the screen shows, as plain data the host changes through
//! [`Ui::model_mut`](crate::app::Ui::model_mut). The types live next to the code that renders
//! them and are gathered here.

pub use crate::base::format::ClockTime;
pub use crate::base::theme::{AgentColor, AgentColors};
pub use crate::bottom::working::verbs::{VerbPicker, VerbStyle};
pub use crate::bottom::{
    Activity, AddedPart, AgentPanel, AgentRow, AgentState, ApprovalMode, BottomLine, BottomRow,
    BottomState, Checklist, CliState, CliUse, Composer, Hint, LinePart, MenuItem, Menus,
    ModeNotice, OptionKind, PartBody, QuestionAgent, QuestionChip, QuestionNote, QuestionOption,
    QuestionPrompt, VimMode, WindowUse, WorkingLine,
};
pub use crate::cells::cell::history::{CellId, History, LiveBlock};
pub use crate::cells::cell::{
    AgentReady, AgentsStarted, AssistantMessage, Cell, CollapsedGroup, DiffCell, DiffHunk,
    DiffKind, DiffLine, DiffView, ExitNote, Notice, Recap, SessionHeader, ShellEnd, ShellRun,
    StartedAgent, Todo, TodoState, Tone, ToolCall, ToolItem, ToolKind, ToolState, ToolStats,
    ToolTitle, TurnEnd, UserMessage,
};

#[derive(Debug, Default)]
pub struct ViewModel {
    /// The session name, shown in the terminal title after the status glyph.
    pub title: String,
    pub history: History,
    pub bottom: BottomState,
    pub verbs: VerbStyle,
    /// Picks each working stretch's word and each turn end's.
    pub verb_picker: VerbPicker,
    /// Each agent's name color for the whole run, handed out as agents first appear.
    pub agent_colors: AgentColors,
    /// Each mod's own chord, from the mods the controller last sent.
    pub mod_chords: crate::mods::keys::Chords,
    /// Settings a view saved and sums up itself when it closes, such as Config's rows, so their
    /// change prints nothing as it happens.
    pub said_by_view: std::collections::BTreeSet<String>,
    /// The key of the `/config key=value` typed last, and the command as typed, whose change
    /// prints under it as `⎿  Set ui.tips to false (in ~/.argonauts/config.toml)`.
    pub typed_config: Option<(String, String)>,
    /// A workflow's command as typed last, such as `/council`, which the line it leaves prints
    /// under: `⎿  Switched to Council · everywhere`.
    pub typed_workflow: Option<String>,
    /// The text behind the newest failure in the conversation, for `r` until something is typed.
    pub raw_failure: Option<crate::bridge::failure::RawFailure>,
    /// What `[ui]` asks of the terminal while the screen runs.
    pub screen: ScreenHabits,
}

/// The terminal's progress bar, copying a selection on release, and following new output in
/// fullscreen, each as its `[ui]` key says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenHabits {
    pub progress_bar: bool,
    pub copy_on_select: bool,
    pub autoscroll: bool,
}

impl Default for ScreenHabits {
    fn default() -> Self {
        Self {
            progress_bar: true,
            copy_on_select: true,
            autoscroll: true,
        }
    }
}

impl ScreenHabits {
    pub fn from_settings(ui: &argonauts_core::settings::ui::UiSettings) -> Self {
        Self {
            progress_bar: ui.progress_bar,
            copy_on_select: ui.copy_on_select,
            autoscroll: ui.autoscroll,
        }
    }
}

impl ViewModel {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    pub fn is_working(&self) -> bool {
        self.bottom.working.is_some()
    }
}
