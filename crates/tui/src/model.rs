//! Everything the screen shows, as plain data the host changes through
//! [`Ui::model_mut`](crate::app::Ui::model_mut). The types live next to the code that renders
//! them and are gathered here.

pub use crate::bottom::verbs::{VerbPicker, VerbStyle};
pub use crate::bottom::{
    Activity, AddedPart, AgentPanel, AgentRow, AgentState, ApprovalMode, BottomLine, BottomRow,
    BottomState, Checklist, CliState, CliUse, Composer, Hint, LinePart, MenuItem, Menus,
    ModeNotice, OptionKind, PartBody, QuestionAgent, QuestionChip, QuestionNote, QuestionOption,
    QuestionPrompt, VimMode, WindowUse, WorkingLine,
};
pub use crate::cell::{
    AgentReady, AgentsStarted, AssistantMessage, Cell, CollapsedGroup, DiffCell, DiffHunk,
    DiffKind, DiffLine, DiffView, ExitNote, Notice, Recap, SessionHeader, ShellEnd, ShellRun,
    StartedAgent, Todo, TodoState, Tone, ToolCall, ToolItem, ToolKind, ToolState, ToolStats,
    ToolTitle, TurnEnd, UserMessage,
};
pub use crate::format::ClockTime;
pub use crate::history::{CellId, History, LiveBlock};
pub use crate::theme::{AgentColor, AgentColors};

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
    /// The text behind the newest failure in the conversation, for `r` until something is typed.
    pub raw_failure: Option<crate::bridge::failure::RawFailure>,
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
