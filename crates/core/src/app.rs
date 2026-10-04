//! Between the TUI and the app controller: commands up, events down.
//!
//! With `ARGONAUTS_TEST_TRACE=<file>` set, the controller appends every command and event to
//! that file as JSONL ([`TraceLine`]), which the non-UI scenario tests assert on.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::agent_sessions::AgentSession;
use crate::api::{DiffScope, DiffView, RawView, RunSummary, StatusSummary, TakeOverTicket};
use crate::crew_panel::CrewPanel;
use crate::display_names::DisplayNames;
use crate::events::RunEvent;
use crate::events::checkpoint::{AnswerAction, AnswerOption, FreeText};
use crate::events::run::{CheckedInstruction, ParkReason, Party, RunMode, TriageDecision};
use crate::input::{MentionTarget, SlashCommand, WorkflowAction};
use crate::limits::CostEstimate;
use crate::limits::{HealthState, QuotaReading};
use crate::model_catalog::display_name_from_id;
use crate::model_picker::EffortSlider;
use crate::names::{
    ChatId, CheckpointId, ChoiceId, MessageId, RoleName, SessionKey, TurnId, WorkerRef, WorkflowId,
};
use crate::policy::{ApprovalsView, NarratorLine, NarratorStatus};
use crate::registry::ModelAccess;
use crate::settings::SettingScope;
use crate::settings::approvals::ApprovalMode;
use crate::settings::budget::BudgetAmount;
use crate::settings::ui::{NarratorMode, TakeoverWhere, UiSettings};
use crate::steps::intake::ChangeEstimate;
use crate::vendor_commands::VendorCommandSet;
use crate::vendor_sessions::VendorSession;
use crate::views::{
    AgentView, CheckpointRow, ContextView, LearningArea, LearningCommand, LearningView,
    WorkflowFile, WorkflowView,
};
use crate::{AgentEvent, CardId, NoteLevel, RunId, Spend, Verbosity};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiCommand {
    /// Plain text from the prompt. The controller decides what it means.
    Submit {
        text: String,
    },
    /// Parsed with `input::parse_input`.
    Slash {
        command: SlashCommand,
    },
    Mention {
        target: MentionTarget,
        text: String,
    },
    Answer {
        to: AnswerTarget,
        action: AnswerAction,
        /// The note added with Tab on the picked option, or the Type my own answer line. Text
        /// typed at the prompt never gets here.
        text: Option<String>,
        /// From the card's `CheckpointOpened`, passed on unchanged so an answer is bound to what
        /// you saw. `None` for app choices.
        #[serde(default)]
        evidence_sha256: Option<String>,
        /// For "request changes on some cards", the cards you picked. Empty means every card.
        #[serde(default)]
        cards: Vec<CardId>,
        /// A handoff card's remember key was on, so the picked candidate is also saved as a
        /// replacement rule for the card's role and the stopped vendor. The app asks where.
        #[serde(default, skip_serializing_if = "is_false")]
        remember: bool,
    },
    /// Esc on a waiting card. A run's card moves to the crew view's "needs you" group and
    /// nothing is picked. An app choice closes as its [`ChoiceKind`] says. The next Esc, with
    /// the run's card already set aside, pauses the run.
    SetAside {
        to: AnswerTarget,
    },
    Steer {
        run: RunId,
        session: SessionKey,
        text: String,
        /// Ctrl+Enter: the model reads it now, as Claude Code's send now, where the session's
        /// CLI can. Otherwise after the running tool call.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        now: bool,
    },
    /// Sent once the take-over is confirmed.
    TakeOver {
        run: RunId,
        card: CardId,
    },
    /// The vendor's UI exited and the TUI took the terminal back.
    TakeOverEnded {
        run: RunId,
        card: CardId,
        exit_code: Option<i32>,
    },
    /// From a picker; the slash form arrives as `Slash`.
    Reassign {
        run: RunId,
        card: CardId,
        to: WorkerRef,
    },
    /// Pauses at the next safe point, or with `now` stops the agents at once and commits their
    /// work. Either way the run parks.
    Pause {
        now: bool,
    },
    CancelPause,
    Resume {
        /// The one on screen when `None`.
        run: Option<RunId>,
    },
    /// Stops the run on screen.
    Stop,
    StopAgent {
        run: RunId,
        session: SessionKey,
    },
    /// Goes back to `card`'s checkpoint `handoff-<checkpoint>` in the run on screen, after a
    /// confirm card. The `/rewind` picker sends it.
    Rewind {
        card: CardId,
        checkpoint: u32,
    },
    /// The narrator's summary of the run on screen: `s` in the status view.
    Summary,
    /// An action in a Learning picker. The app answers with the area's view again.
    Learning {
        area: LearningArea,
        command: LearningCommand,
    },
    /// `s` in the workflow view: saves the run's workflow as it is now to your own library,
    /// under the run's name, so it runs again by that name.
    SaveWorkflow {
        run: RunId,
        /// The run's own when `None`.
        workflow: Option<WorkflowId>,
    },
    /// Shows, copies, or diffs a workflow file: the Library tab of the workflow view.
    WorkflowFile {
        name: String,
        action: WorkflowAction,
    },
    /// Takes queued, undelivered messages back into the input box.
    TakeBack {
        run: RunId,
        /// Oldest first.
        messages: Vec<MessageId>,
    },
    /// For the run on screen and for new runs this session.
    ApprovalMode {
        mode: ApprovalMode,
    },
    Verbosity {
        level: Verbosity,
    },
    /// The controller saves `ui.keys` globally.
    Vim {
        on: bool,
    },
    /// A narrator mode for this app session only. The TUI doesn't send it now: the `/narrator`
    /// picker sends `SetSetting`, and the argument form arrives as `Slash`.
    Narrator {
        mode: NarratorMode,
    },
    CancelTurn,
    /// So the controller knows when an OS notification helps.
    Focus {
        focused: bool,
    },
    /// You marked an auto-approval in `/approvals` right or wrong.
    SpotCheck {
        run: RunId,
        id: CheckpointId,
        correct: bool,
    },
    /// The goal view's keys and `/goal`'s: clear the goal, turn the run into one, or change an
    /// assumption. `None` is the run on screen.
    Goal {
        run: Option<RunId>,
        action: crate::events::goal::GoalAction,
    },
    Request {
        view: ViewRequest,
    },
    /// A setting picked in a picker or `/config`, after the TUI asked the scope. The argument
    /// form (`/effort high`) arrives as `Slash`. A safety key needs the `ConfirmSafetySetting`
    /// choice and never changes inside an agent session.
    SetSetting {
        key: String,
        /// TOML text.
        value: String,
        scope: SettingScope,
    },
    /// A picker's Reset row: removes `key` from `scope` so the layer below shows through. A file
    /// scope keeps the file's comments. Same safety confirm and agent-session refusal as a set.
    ResetSetting {
        key: String,
        scope: SettingScope,
    },
    /// A `!` command, run as you in the repo folder. Its output shows in the transcript and goes
    /// to the lead with your next message, never by itself.
    Shell {
        command: String,
    },
    /// Ctrl+C while a `!` command runs.
    StopShell,
    /// Ctrl+Enter or Ctrl+X Ctrl+S: what you sent the main session while it works goes to the
    /// model now.
    SendNow,
    /// ↑ on your chat messages still waiting: back into the input, the ones the model hasn't
    /// read too.
    TakeBackChat,
    /// A button of what Claude Code draws above the input, by what its press sends back.
    PressHeadsUp {
        plugin: String,
        handle: u64,
    },
    /// Closes the screen. Runs keep going.
    Exit,
    /// Saves the pick recorded for an answered handoff card as a replacement rule for the
    /// card's role and the stopped vendor, as `Answer.remember` does at answer time. The app
    /// asks where to save it.
    RememberPick {
        run: RunId,
        id: CheckpointId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum AnswerTarget {
    Checkpoint { run: RunId, id: CheckpointId },
    Choice { id: ChoiceId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum ViewRequest {
    Runs,
    Status,
    /// Every resolved key and its layer, as a plain list. `/config` itself draws `Settings`.
    Config,
    Approvals,
    Diff {
        run: RunId,
        card: CardId,
        scope: DiffScope,
    },
    Raw {
        run: RunId,
        session: SessionKey,
    },
    /// A settings command with no argument gets its picker's rows unasked; this fetches more,
    /// such as a section of `/config`.
    Settings {
        /// A dotted key prefix, such as `budget` or `roles.reviewer.handoff`; every key when
        /// `None`.
        section: Option<String>,
    },
    Models {
        /// The default agent when `None`.
        role: Option<RoleName>,
    },
    Workflow {
        run: RunId,
        /// The run's own when `None`.
        workflow: Option<WorkflowId>,
    },
    Agent {
        run: RunId,
        session: SessionKey,
    },
    /// A goal-style run's goal: its criteria, attempts, last check, and assumptions.
    Goal {
        run: RunId,
    },
    /// Every live session's context, cap, tool calls, and spend.
    Context,
    /// The crew view's "All repos" tab: runs in every repo with a live supervisor, read from
    /// `~/.argonauts/registry.json` and each repo's journal ends.
    AllRepos,
    /// The crew panel's rows the screen can't build itself: conversations, the CLIs' own
    /// sessions, and runs in other repos.
    Crew,
    /// The Library tab of the workflow view: every workflow file and built-in.
    WorkflowLibrary,
    /// The repo's files and folders again, for the `@` list. Answered with `RepoFiles`.
    RepoFiles,
    /// A Learning picker's rows, such as after you change one of its settings.
    Learning {
        area: LearningArea,
    },
    /// The agents' sessions of one run the `/resume` picker listed without them. Answered with
    /// `AgentSessions`.
    AgentSessions {
        run: RunId,
    },
}

/// The outcome of a choice closed with nothing changed. The transcript marks it as neutral
/// rather than answered.
pub const NOT_CHANGED: &str = "Not changed.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    /// The startup header, the first history cell. It comes again when the setup saves, so
    /// the header shows the new agent and preset.
    Welcome {
        version: String,
        /// With `~` for home.
        repo: String,
        preset: String,
        agents: Vec<AgentReady>,
        /// Runs going in this repo that don't wait on you.
        runs_going: u32,
        /// Runs in this repo that wait on you. The app opens a new conversation all the same,
        /// and each one's card waits until you open its run.
        #[serde(default)]
        runs_need_you: u32,
        /// The input's placeholder until your first prompt, made from recent commit subjects,
        /// never by a model.
        #[serde(default)]
        example_prompt: Option<String>,
        /// The chat's agent, for the header's `<CLI> · <model> with <effort> effort` line. Its
        /// model is the one set, else the CLI's own default when the CLI's model list marks
        /// one, else none.
        #[serde(default)]
        default_agent: Option<WorkerRef>,
        /// Its effort level: the one set, else the one the CLI starts that model at, when known.
        #[serde(default)]
        effort: Option<String>,
    },
    /// Events for this run follow from `seq` 0, and the TUI rebuilds its state and the Ctrl+O
    /// history from them. Up to `ReplayDone` it prints none of them. A resumed run's
    /// scrollback shows the summary as its header, then [`AppEvent::CondensedReplay`].
    Attached {
        summary: RunSummary,
        /// A run this app just started. Its history prints as it comes under the prompt that
        /// started it, so it gets no header.
        #[serde(default, skip_serializing_if = "is_false")]
        started_here: bool,
    },
    Detached {
        run: RunId,
    },
    /// This conversation's runs that go on or wait on you without being on this screen, for the
    /// agent panel's rows. Earlier conversations' runs are only counted in the header. Each list
    /// replaces the last.
    BackgroundRuns {
        runs: Vec<RunSummary>,
    },
    Run {
        event: Box<RunEvent>,
    },
    Chat {
        event: ChatEvent,
    },
    Choice {
        choice: AppChoice,
    },
    /// An app-level card was answered or withdrawn.
    ChoiceClosed {
        id: ChoiceId,
        /// One line for the transcript, such as "No run started.", or [`NOT_CHANGED`].
        outcome: String,
    },
    Narrator {
        line: NarratorLine,
    },
    NarratorStatus {
        status: NarratorStatus,
    },
    Mode {
        mode: AppMode,
    },
    Status {
        status: StatusLine,
    },
    Notice {
        level: NoteLevel,
        text: String,
    },
    /// Why a key you pressed did nothing, such as asking for a view there's nothing in yet. It
    /// goes on the notice line, replacing the last one, and never into the transcript, so
    /// pressing the key again adds nothing.
    Hint {
        text: String,
    },
    View {
        data: Box<ViewData>,
    },
    /// Suspend the screen and run this in the terminal, then send `TakeOverEnded`.
    TakeOver {
        ticket: TakeOverTicket,
        place: TakeoverWhere,
        run: RunId,
    },
    /// Suspend the screen while an agent CLI's own command runs in the terminal, such as its
    /// login, then come back. Nothing is sent when it ends.
    RunInTerminal {
        argv: Vec<String>,
        cwd: PathBuf,
    },
    /// The `[ui]` settings in effect changed, whatever the cause. The TUI applies them at once.
    UiSettings {
        vim: bool,
        verbosity: Verbosity,
        /// `None` from an older controller that sent only `vim` and `verbosity`.
        #[serde(default)]
        ui: Option<Box<UiSettings>>,
    },
    /// After a long turn or a finished run. Built by code from the status summary, or written
    /// by the narrator when it's on.
    Recap {
        text: String,
        /// Set when a model wrote it.
        model: Option<WorkerRef>,
    },
    /// Put these texts back into the input box.
    TakenBack {
        /// Oldest first.
        texts: Vec<String>,
    },
    /// `/clear` started a new conversation, so what the last one's session drew above the
    /// input goes with it.
    ChatCleared,
    /// The main agent's own commands, for the `/` menu, `/help`, and the parser. Sent at start
    /// and whenever the main agent changes.
    VendorCommands {
        set: VendorCommandSet,
    },
    /// The files for the `@` list: those git tracks and those it would (untracked and not
    /// ignored) where the conversation works, relative to that folder with `/` between
    /// folders. That's the repo, or the run's copy of an agent's session the conversation
    /// carries on. Sent at start, when that folder changes, and in answer to
    /// `Request { RepoFiles }`.
    RepoFiles {
        paths: Vec<String>,
    },
    /// What the screen calls each CLI and model. Sent at start and whenever the model lists
    /// are read again.
    Names {
        names: Box<DisplayNames>,
    },
    SettingChanged {
        key: String,
        /// TOML text: the value saved, or after a reset the value now in effect from the layers
        /// below, empty when the key is now unset.
        value: String,
        scope: SettingScope,
        previous: Option<String>,
        /// Removed from `scope` rather than set.
        #[serde(default)]
        reset: bool,
    },
    /// What a resumed run prints in place of its history, sent after the replayed events and
    /// just before `ReplayDone`, and only when there was history. The controller builds it
    /// from the events at no token cost, unless the lead phrases the recap from the digest.
    CondensedReplay {
        run: RunId,
        /// The last chat turns word for word, oldest first.
        turns: Vec<ReplayTurn>,
        /// One line per milestone, oldest first, such as "Plan approved · 5 cards".
        milestones: Vec<String>,
        /// The `※ recap` line.
        recap: String,
        /// Set when the lead phrased the recap.
        recap_model: Option<WorkerRef>,
    },
    /// Every event of an attached run up to `seq` was history. Only later ones ring the bell or
    /// notify.
    ReplayDone {
        run: RunId,
        seq: u64,
    },
    /// An earlier conversation picked up again, with `/resume` or `argonauts -c`. Its turns are
    /// read from its journal at no token cost.
    ChatResumed {
        chat: ChatId,
        title: String,
        /// The agent of its last turn.
        worker: Option<WorkerRef>,
        /// Older turns, folded: they join the Ctrl+O history without printing.
        earlier: Vec<ReplayTurn>,
        /// The last turns word for word, oldest first.
        turns: Vec<ReplayTurn>,
    },
    /// What a command found, such as `/doctor`'s checks. It prints at every verbosity, since you
    /// asked for it.
    Report {
        title: String,
        lines: Vec<String>,
    },
    Shell {
        event: ShellEvent,
    },
    Exit,
}

/// A `!` command's life, numbered from 1 in each app session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shell", rename_all = "snake_case")]
pub enum ShellEvent {
    Started {
        id: u32,
        command: String,
    },
    /// Standard output and error as they arrive, in one stream as a terminal shows them.
    Output {
        id: u32,
        text: String,
    },
    Finished {
        id: u32,
        /// `None` when a signal ended it.
        exit_code: Option<i32>,
        /// Ctrl+C stopped it.
        stopped: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayTurn {
    /// You, a front end, or an agent.
    pub from: Party,
    pub text: String,
}

/// What plain text and Esc mean now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AppMode {
    /// Plain text is a new request.
    Idle,
    Answering {
        turn: TurnId,
    },
    /// Plain text is guidance.
    Working {
        run: RunId,
    },
    /// Enter cancels the pause.
    Pausing {
        run: RunId,
    },
    /// Enter or `/resume` resumes.
    Parked {
        run: RunId,
        reason: ParkReason,
    },
}

/// The header and status line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusLine {
    /// The terminal title's text: the chat's title, or the run's.
    pub title: String,
    /// With `~` for home.
    pub repo: String,
    pub preset: String,
    /// `triage.all_hands`, which the input rule's label shows beside the preset.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_hands: bool,
    pub run: Option<RunId>,
    pub default_agent: WorkerRef,
    /// One chip per vendor, in `crew` order.
    pub quotas: Vec<QuotaChip>,
    pub spent: Option<Spend>,
    pub limit: Option<BudgetAmount>,
    pub narrator: NarratorMode,
    pub approval_mode: ApprovalMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentReady {
    /// Adapter id.
    pub id: String,
    pub version: Option<String>,
    /// Installed, enabled, and a tested version.
    pub ready: bool,
    /// Why not, or a warning such as "untested version".
    pub note: Option<String>,
}

/// A vendor's chip, such as "Codex 38%" or "Claude down".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaChip {
    /// Short name, such as `Claude`, `Codex`, `agy`.
    pub label: String,
    pub reading: Option<QuotaReading>,
    pub health: HealthState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "chat", rename_all = "snake_case")]
pub enum ChatEvent {
    TurnStarted {
        turn: TurnId,
        text: String,
        worker: WorkerRef,
        /// False only for a direct turn, which may change your files.
        read_only: bool,
        commit: String,
        /// Uncommitted paths left out.
        uncommitted: Vec<String>,
        #[serde(default)]
        settings_sha256: Option<String>,
        /// The turn goes on from the one before, as a direct turn does after its estimate, so
        /// your message isn't shown again.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        continues: bool,
    },
    Agent {
        turn: TurnId,
        event: AgentEvent,
    },
    TurnFinished {
        turn: TurnId,
        reply: TurnReply,
        spend: Spend,
        elapsed_ms: u64,
    },
    /// The fallback agent may retry it.
    TurnFailed {
        turn: TurnId,
        error: String,
    },
    TurnCancelled {
        turn: TurnId,
    },
    /// A direct turn's agent ran out, and `to` carries on the same request in the same folder.
    HandedOff {
        turn: TurnId,
        from: WorkerRef,
        to: WorkerRef,
        /// Such as "hit its usage limit".
        why: String,
        /// What `to` was told, such as "your request, the transcript so far, and that
        /// invoice/money.py changed".
        told: String,
    },
    /// A message of yours that went into the running turn, once the model read it, as Claude
    /// Code's send now shows it.
    Joined {
        turn: TurnId,
        text: String,
    },
}

/// What a chat turn turned out to be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum TurnReply {
    Answer {
        markdown: String,
    },
    /// Triage picked a mode and the run starts, or for direct solo the session goes ahead in
    /// your checkout.
    Change {
        /// The agent's short plan of attack.
        markdown: String,
        estimate: ChangeEstimate,
        triage: TriageDecision,
        /// What the request said about how to run it, checked, for the Triage line of a solo
        /// that works in your checkout. A run's own Triage line has them from its start.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        instructions: Vec<CheckedInstruction>,
    },
    /// Could be either; a choice card follows.
    Unclear {
        markdown: String,
        estimate: Option<ChangeEstimate>,
        would_pick: Option<RunMode>,
    },
}

/// An app-level ◆ card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppChoice {
    pub id: ChoiceId,
    pub kind: ChoiceKind,
    pub title: String,
    pub body: Vec<String>,
    pub options: Vec<AnswerOption>,
    pub free_text: FreeText,
    pub bell: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceKind {
    /// Solo `s`, Crew run `c`, No, just asking `n`. Esc sets it aside.
    StartWork,
    /// Take over `t`. Built by the TUI; Esc cancels.
    ConfirmTakeOver,
    /// Change it `y` or Keep it as it is `n`, the body naming every key the write changes. Esc
    /// keeps it as it is.
    ConfirmSafetySetting,
    /// The plan-terms note, the guided setup's first step, shown once: Continue `c`. Esc takes
    /// the recommended answers for the rest of the setup.
    TermsNote,
    /// One step of the guided setup: one option per answer, the recommended one first so Enter
    /// takes it. Esc takes the recommended answers for this step and the rest.
    Init,
    /// Where to save a setting given as a command's argument, such as `/effort high`. Options
    /// are the [`SettingScope`] names with the command's default recommended. Esc cancels.
    SettingScope,
    /// Gate commands found in the repo, offered once: Use these `y`, Edit them `e`, or Skip
    /// `n`. Esc leaves it for the next run.
    DetectedGates,
    /// Whether to trust the project settings files, which set safety keys: Trust them `y`, Not
    /// now `n`, or Show the file `d`. Asked again when a file's hash changes. Esc means not now.
    TrustProject,
    /// `/rewind`: Revert `y`, which resets the card's branch and keeps the replaced work on a
    /// side branch, or Show the diff `d`. Esc cancels.
    ConfirmRevert,
    /// `/connect`: Write it `y`, which keeps a backup of the front end's config first, or Not
    /// now `n`. The body shows the file and the entry. Esc means not now.
    ConfirmConnect,
    /// A request said "from now on": This project, just me `m`, This project, shared `p`,
    /// Everywhere `e`, or Just this request `r`. The body says what it would keep. Esc keeps
    /// it for this request only.
    KeepInstructions,
    /// A direct turn's CLI asks before an edit, a command, or another tool: Yes, Yes and don't
    /// ask again, or No, which tells it to stop. The body shows the edit or the command. Esc
    /// means No, as in Claude Code.
    Permission,
    /// agy was refused commands it would have asked about: Yes, and continue `y`, which
    /// resumes it for one turn that runs commands without asking, or No `n`. Esc sets it aside.
    RefusedCommands,
    /// `/rewind` in a direct conversation: one option per message whose turn changed files,
    /// newest first. Esc cancels.
    RewindTurn,
    /// Plan first in direct solo: Yes, go ahead `y` resumes the session that planned as a turn
    /// that works in your checkout, or No `n`, and nothing changes. Esc means No.
    GoAhead,
    /// A direct turn's agent ran out: one option per agent that may carry it on in your checkout,
    /// `1` to `9`, then Stop here `s`. Esc stops.
    HandOff,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "data", rename_all = "snake_case")]
pub enum ViewData {
    /// The `/resume` picker.
    Runs {
        /// Newest first.
        runs: Vec<RunSummary>,
        /// Earlier conversations with the lead, newest first. The picker lists them with the
        /// runs by when each was last used.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chats: Vec<ChatSummary>,
        /// The main agent CLI's own sessions for this folder, newest first, listed with the
        /// rest under a Kind column.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sessions: Vec<VendorSession>,
        /// The sessions of the listed runs' agents, each listed under its run.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agents: Vec<AgentSession>,
        /// The runs whose agents `agents` holds, the newest few. An older run's are read when
        /// you open it in the picker.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agents_read: Vec<RunId>,
    },
    /// One run's agents' sessions, for the `/resume` picker.
    AgentSessions {
        run: RunId,
        agents: Vec<AgentSession>,
    },
    /// A goal-style run's goal.
    Goal {
        view: Box<crate::views::GoalView>,
    },
    Status {
        summary: StatusSummary,
    },
    Config {
        entries: Vec<ConfigEntry>,
    },
    Approvals {
        view: ApprovalsView,
    },
    Diff {
        diff: DiffView,
    },
    Raw {
        raw: RawView,
    },
    Settings {
        /// In catalog order.
        rows: Vec<SettingRow>,
        /// The settings command whose picker these rows fill, such as `effort`; `None` when they
        /// answer a `Request { Settings }`.
        #[serde(default)]
        command: Option<String>,
        /// Echoes the requested section so the answer can be matched to it.
        #[serde(default)]
        section: Option<String>,
        /// For `/agents`: the agent CLIs this machine doesn't have, each with how to add it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not_installed: Vec<MissingCli>,
    },
    Models {
        /// Workers the role's rules allow, best fit first.
        choices: Vec<ModelChoice>,
        /// `None` is the default agent. A pick for the default agent sends `Slash { Agent }`,
        /// any other `Slash { Model }`.
        #[serde(default)]
        role: Option<RoleName>,
        /// When the CLIs' model lists were last read; `None` before the first time.
        #[serde(default, with = "time::serde::rfc3339::option")]
        refreshed: Option<OffsetDateTime>,
        /// A refresh is running, and the list it finds follows as an `update`.
        #[serde(default)]
        refreshing: bool,
        /// A newer list for a picker already showing this role's models. It opens no picker.
        #[serde(default)]
        update: bool,
    },
    /// `/effort` with no level: the slider for the default agent's or the role's model. Enter
    /// sends `Slash { Effort }` with the level.
    Effort {
        slider: EffortSlider,
    },
    /// The crew panel's rows from the controller, which the screen adds to its own sessions'.
    Crew {
        panel: CrewPanel,
    },
    Workflow {
        view: Box<WorkflowView>,
    },
    Agent {
        view: Box<AgentView>,
    },
    Context {
        view: ContextView,
    },
    AllRepos {
        /// This repo first, then the others by their newest update.
        repos: Vec<RepoRuns>,
    },
    /// The built-ins, then your files, then the repo's, a file shadowing a workflow of the same
    /// name.
    WorkflowLibrary {
        workflows: Vec<WorkflowFile>,
        /// Sent after a copy or a diff for the Library tab already open, so nothing opens for
        /// it when that tab has closed.
        #[serde(default)]
        refresh: bool,
    },
    /// `/records`, `/recipes`, `/skills`, or `/memory`: one picker with the area's tabs, each
    /// row's detail and actions, and a Settings tab.
    Learning {
        view: Box<LearningView>,
    },
    /// `/rewind`: the handoff checkpoints of `card`, or of every card, oldest first. Picking one
    /// sends `UiCommand::Rewind`.
    Checkpoints {
        run: RunId,
        card: Option<CardId>,
        checkpoints: Vec<CheckpointRow>,
    },
}

/// One conversation with the lead, read from `chat/<chat>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSummary {
    pub chat: ChatId,
    /// From its first prompt, or its first run when it has no turn.
    pub title: String,
    /// When it last had a turn or opened a run.
    #[serde(with = "time::serde::rfc3339")]
    pub updated: time::OffsetDateTime,
    /// The agent of its last turn.
    pub worker: Option<WorkerRef>,
    pub turns: u32,
    /// The runs it started or opened, which carrying it on brings back to the agent panel.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub runs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoRuns {
    /// With `~` for home.
    pub repo: String,
    /// Newest first.
    pub runs: Vec<RunSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingRow {
    /// With real map keys (`agents.codex.model`, not `agents.*.model`).
    pub key: String,
    /// TOML text; empty when unset, such as `narrator.model` by default.
    pub value: String,
    /// `built-in`, `global`, `project`, `project-local`, `preset`, `flag`, or `session`. The
    /// scope question warns when it's above the scope you pick, since saving there changes nothing.
    pub layer: String,
    /// For a value from the preset, the settings file that picked the preset (`global`,
    /// `project`, or `project-local`), since saving there or later wins over it. `None` when the
    /// preset is the default or a flag picked it, and sits above every file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_from: Option<String>,
    /// The command's default, or in `/config` the key's catalog default.
    pub default_scope: SettingScope,
    /// Changing it asks you to confirm.
    pub safety: bool,
    /// Values the picker offers, as TOML text.
    pub allowed: Vec<String>,
    pub description: String,
}

/// An agent CLI that isn't installed, as `/agents` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingCli {
    /// Adapter id.
    pub id: String,
    /// What to run to install it, when its adapter says.
    #[serde(default)]
    pub install: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub worker: WorkerRef,
    pub family: String,
    pub access: ModelAccess,
    /// For a typical card.
    pub cost: CostEstimate,
    pub quota: Option<QuotaReading>,
    /// From the registry entry; every level when unknown.
    pub efforts: Vec<String>,
    pub strengths: Vec<String>,
    /// The role's current worker.
    #[serde(default)]
    pub current: bool,
    /// What the name alone doesn't say: an alias's model ("newest Opus"), the model a CLI's
    /// default runs, or that the model is retired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Superseded, deprecated, or retired, so `/model` lists it under "Older".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub older: bool,
    /// What the picker calls it, such as "Opus 5.5". Empty for the CLI's default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub recommended: bool,
    /// Neither the catalog nor the CLI knows it, so the picker marks it "new".
    #[serde(default, skip_serializing_if = "is_false")]
    pub new: bool,
    /// The level the CLI runs it at when none is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    /// The level set for it now, for the default agent or the role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// A short name the CLI resolves itself, such as `opus`. The models level lists aliases on
    /// one line under Default.
    #[serde(default, skip_serializing_if = "is_false")]
    pub alias: bool,
    /// For the CLI's default or an alias: the display name of the model it runs now, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<String>,
}

impl ModelChoice {
    /// `name`, else one derived from the model id, else "Default".
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        match &self.worker.model {
            Some(model) => display_name_from_id(model),
            None => "Default".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEntry {
    pub key: String,
    /// TOML text.
    pub value: String,
    /// `built-in`, `global`, `project`, `project-local`, `preset`, or `flag`.
    pub layer: String,
    /// Changing it asks you to confirm.
    pub safety: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "dir", rename_all = "snake_case")]
pub enum TraceLine {
    In {
        /// Milliseconds since the app started.
        t_ms: u64,
        command: UiCommand,
    },
    Out {
        /// Milliseconds since the app started.
        t_ms: u64,
        event: AppEvent,
    },
}

fn is_false(value: &bool) -> bool {
    !value
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}
