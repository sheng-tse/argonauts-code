//! Between the TUI and the app controller: commands up, events down.
//!
//! With `ARGONAUTS_TEST_TRACE=<file>` set, the controller appends every command and event to
//! that file as JSONL ([`TraceLine`]), which the non-UI scenario tests assert on.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::agent_panel::AgentPanelRows;
use crate::api::{DiffScope, DiffView, RawView, RunSummary, StatusSummary, TakeOverTicket};
use crate::cli_commands::CliCommandSet;
use crate::cli_sessions::CliSession;
use crate::display_names::DisplayNames;
use crate::events::RunEvent;
use crate::events::approval::{AnswerAction, AnswerOption, FreeText};
use crate::events::context::{ContextDetail, ContextUsage};
use crate::events::run::{CaptainOrCrewDecision, CheckedInstruction, Party, RunMode, WaitReason};
use crate::input::{MentionTarget, SlashCommand, WorkflowAction};
use crate::limits::CostEstimate;
use crate::limits::{HealthState, PlanUsageReading};
use crate::model_catalog::display_name_from_id;
use crate::model_picker::EffortSlider;
use crate::names::{
    ApprovalId, ChoiceId, ConversationId, MessageId, ModelRef, RoleName, SessionKey, TurnId,
    WorkflowId,
};
use crate::policy::{ApprovalsView, NarratorLine, NarratorStatus};
use crate::registry::ModelAccess;
use crate::settings::SettingScope;
use crate::settings::approvals::ApprovalMode;
use crate::settings::bottom_line::BottomLineSettings;
use crate::settings::limit::LimitAmount;
use crate::settings::ui::{NarratorMode, TakeOverWhere, UiSettings};
use crate::steps::intake::ChangeEstimate;
use crate::views::{
    AgentView, CheckpointRow, ContextView, LearningArea, LearningCommand, LearningView,
    WorkflowFile, WorkflowView,
};
use crate::{AgentEvent, NoteLevel, RunId, Spend, TaskId, Verbosity};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiCommand {
    /// Plain text from the prompt. The controller decides what it means.
    Submit {
        text: String,
        /// The text has the word `ultracode` and you didn't press Opt+W to ignore it, so this
        /// one request goes through a workflow. The screen sets it and the engine reads it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        workflow_requested: bool,
        /// The pictures whose `[Image #N]` chips the text holds, for the main session's CLI.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pictures: Vec<crate::pictures::Picture>,
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
        /// From the question's `ApprovalOpened`, passed on unchanged so an answer is bound to what
        /// you saw. `None` for app choices.
        #[serde(default)]
        evidence_sha256: Option<String>,
        /// For "request changes on some tasks", the tasks you picked. Empty means every task.
        #[serde(default)]
        tasks: Vec<TaskId>,
        /// A fallback question's remember key was on, so the picked candidate is also saved as a
        /// fallback rule for the task's role and the stopped agent. The app asks where.
        #[serde(default, skip_serializing_if = "is_false")]
        remember: bool,
    },
    /// Esc on a waiting question or approval. For a run's, it means "carry on without me": the
    /// engine takes an option in your place, as `approvals.when_declined` says, and nothing
    /// waits. An app choice closes as its [`ChoiceKind`] says.
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
        task: TaskId,
    },
    /// The agent's UI exited and the TUI took the terminal back.
    TakeOverEnded {
        run: RunId,
        task: TaskId,
        exit_code: Option<i32>,
    },
    /// The TUI took the terminal back from a command [`AppEvent::RunInTerminal`] ran, such
    /// as a CLI's login.
    TerminalReturned {
        argv: Vec<String>,
    },
    /// From a picker; the slash form arrives as `Slash`.
    Reassign {
        run: RunId,
        task: TaskId,
        to: ModelRef,
    },
    /// Pauses at the next safe point, or with `now` stops the agents at once and commits their
    /// work. Either way the run waits.
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
    /// Stops an agent that has no task of its own and starts it over with the prompt it had,
    /// as a script's own step runs one. A task's worker starts over through `Reassign`.
    RestartAgent {
        run: RunId,
        session: SessionKey,
    },
    /// Stops one subagent your conversation's own session started, by the id its `Subagent`
    /// updates carry. The session and its other subagents go on.
    StopSubagent {
        id: String,
    },
    /// Your message for a subagent of your conversation's own session, named as its row is.
    /// It has no line of its own, so the main agent passes it on, and the conversation shows only
    /// `@name` and your words.
    MessageSubagent {
        name: String,
        text: String,
    },
    /// Goes back to `task`'s checkpoint `fallback-<checkpoint>` in the run on screen, after a
    /// confirm question. The `/rewind` picker sends it.
    Rewind {
        task: TaskId,
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
    /// One change made on the page `/workflow edit <name>` opened.
    WorkflowPage {
        name: String,
        change: crate::workflow_page::WorkflowChange,
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
        id: ApprovalId,
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
    /// A mod's page saved, reset, or trusted its file.
    Mod {
        change: crate::settings::mods::view::ModChange,
    },
    /// A `!` command, run as you in the repo folder. Its output shows in the transcript and goes
    /// to the main agent with your next message, never by itself.
    Shell {
        command: String,
    },
    /// Ctrl+C while a `!` command runs.
    StopShell,
    /// Ctrl+Enter or Ctrl+X Ctrl+S: what you sent the main session while it works goes to the
    /// model now.
    SendNow,
    /// ↑ on your messages still waiting: back into the input, the ones the model hasn't
    /// read too.
    TakeBackConversation,
    /// A button of what Claude Code draws above the input, by what its press sends back.
    PressNotice {
        plugin: String,
        handle: u64,
    },
    /// Something on screen started showing the conversation's context by category, such as
    /// the context mod above the input or the Context tab, or stopped (`None`). Its session
    /// is then asked after each fill, at most every 10 seconds.
    WatchContext {
        detail: Option<ContextDetail>,
    },
    /// `/model`'s picker opened or closed. While it's open, the local model servers are asked
    /// every few seconds, so a model pulled meanwhile shows up in it.
    WatchModels {
        watching: bool,
    },
    /// Closes the screen. Runs keep going.
    Exit,
    /// A slash command as you typed it, which the conversation's record keeps where you typed
    /// it. The command itself follows as its own `Slash`, or the screen does it alone.
    CommandTyped {
        text: String,
    },
    /// A line a command you typed printed back, which the conversation's record keeps after
    /// the command.
    CommandReplied {
        reply: CommandReply,
    },
    /// Saves the pick recorded for an answered fallback question as a fallback rule for the
    /// question's role and the stopped agent, as `Answer.remember` does at answer time. The app
    /// asks where to save it.
    RememberPick {
        run: RunId,
        id: ApprovalId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum AnswerTarget {
    Approval { run: RunId, id: ApprovalId },
    Choice { id: ChoiceId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum ViewRequest {
    /// The `/resume` list: earlier conversations and the CLIs' own sessions for this folder.
    ResumeList,
    /// The resume list again, with the CLIs' sessions of every folder, not this one's alone.
    SessionsEverywhere,
    Status,
    /// The usage panel's Status tab: what runs here and how it is set up. Answered with
    /// `StatusTab`, which only the tab draws, so asking never writes into the conversation.
    StatusTab,
    /// Every resolved key and its layer, as a plain list. `/config` itself draws `Settings`.
    Config,
    Approvals,
    Diff {
        run: RunId,
        task: TaskId,
        scope: DiffScope,
    },
    Raw {
        run: RunId,
        session: SessionKey,
    },
    /// A settings command with no argument gets its picker's rows unasked; this fetches more,
    /// such as a section of `/config`.
    Settings {
        /// A dotted key prefix, such as `limits` or `fallback.reviewer`; every key when
        /// `None`.
        section: Option<String>,
    },
    /// `/mods`: every mod with its file and numbers. Answered with `Mods`.
    Mods {
        /// The mod whose page opens at once.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open: Option<String>,
        /// Newer numbers for the view already open, which opens nothing.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refresh: bool,
    },
    Models {
        /// The main agent when `None`.
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
    /// One agent of a workflow the CLI of `session` runs itself. Answered with `Agent`, whose
    /// row names the same agent.
    InnerAgent {
        run: RunId,
        session: SessionKey,
        agent: crate::views::InnerAgent,
    },
    /// A goal run's goal: its criteria, tries, last check, and assumptions.
    Goal {
        run: RunId,
    },
    /// Every live session's context, cap, tool calls, and spend.
    Context,
    /// One run session's context as its CLI last counted it, whole. Answered with
    /// `SessionContext`.
    SessionContext {
        run: RunId,
        session: SessionKey,
    },
    /// The Usage tab's numbers for a period: plan usage, spend, tokens, history and
    /// workflows. Answered with `Usage`.
    Usage {
        period: crate::api::UsagePeriod,
    },
    /// Reads every CLI's plan usage live again, as `r` on the Usage tab asks. The next
    /// `Usage` answer carries the readings as they arrive.
    ReadPlanUsage,
    /// The Overview tab's numbers for a period, from the usage ledger and the imported
    /// history. `import` answers the tab's question: yes starts the import, no remembers it.
    /// Answered with `Overview`.
    Overview {
        period: crate::overview::OverviewPeriod,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        import: Option<bool>,
    },
    /// The agent panel's "All repos" tab: runs in every repo with a live run host, read from
    /// `~/.argonauts/registry.json` and each repo's journal ends.
    AllRepos,
    /// The agent panel's rows the screen can't build itself: conversations, the CLIs' own
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
}

impl ViewRequest {
    /// The request for the agent `row` shows in `run`: its session's view, or its own when
    /// it is one of a workflow its CLI runs itself.
    pub fn agent(run: &RunId, row: &crate::views::AgentRow) -> Self {
        let (run, session) = (run.clone(), row.session.clone());
        match row.inner.clone() {
            Some(agent) => Self::InnerAgent {
                run,
                session,
                agent,
            },
            None => Self::Agent { run, session },
        }
    }
}

/// The outcome of a choice closed with nothing changed. The transcript marks it as neutral
/// rather than answered.
pub const NOT_CHANGED: &str = "Not changed.";

/// The background work of a command you typed, each kind with its own words on the working
/// line: `/update` updates the CLIs, then checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandWork {
    Updating,
    Checking,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    /// The startup header, the first history cell. It comes again when the setup saves, so
    /// the header shows the new agent and workflow.
    Welcome {
        version: String,
        /// With `~` for home.
        repo: String,
        workflow: String,
        agents: Vec<AgentReady>,
        /// Runs going in this repo that don't wait on you.
        runs_going: u32,
        /// Runs in this repo that wait on you. The app opens a new conversation all the same,
        /// and each one's task waits until you open its run.
        #[serde(default)]
        runs_need_you: u32,
        /// The input's placeholder until your first prompt, made from recent commit subjects,
        /// never by a model.
        #[serde(default)]
        example_prompt: Option<String>,
        /// The conversation's agent, for the header's `<CLI> · <model> with <effort> effort` line.
        /// Its model is the one set, else the CLI's own default when the CLI's model list marks
        /// one, else none.
        #[serde(default)]
        main_agent: Option<ModelRef>,
        /// Its effort level: the one set, else the one the CLI starts that model at, when known.
        #[serde(default)]
        effort: Option<String>,
    },
    /// Events for this run follow, from its latest snapshot or from `seq` 0, and the TUI
    /// rebuilds its state and the Ctrl+O history from them. Up to `ReplayDone` it prints none of
    /// them: what the run did before was drawn already, live or between `PastBegins` and
    /// `PastDrawn`.
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
    Conversation {
        event: ConversationEvent,
    },
    Choice {
        choice: AppChoice,
    },
    /// An app-level question was answered or withdrawn.
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
        status: HeaderAndBottomLine,
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
        place: TakeOverWhere,
        run: RunId,
    },
    /// Suspend the screen while an agent CLI's own command runs in the terminal, such as its
    /// login, then come back and send [`UiCommand::TerminalReturned`].
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
        model: Option<ModelRef>,
    },
    /// Put these texts back into the input box.
    TakenBack {
        /// Oldest first.
        texts: Vec<String>,
    },
    /// `/clear` started a new conversation: the screen starts over under its header, and what
    /// the last one's session drew above the input goes with it.
    ConversationCleared {
        /// The conversation left, when it had turns, which `/resume` picks up again.
        way_back: Option<ConversationId>,
    },
    /// The main agent's own commands, for the `/` menu, `/help`, and the parser. Sent at start
    /// and whenever the main agent changes.
    CliCommands {
        set: CliCommandSet,
    },
    /// The files for the `@` list: those git tracks and those it would (untracked and not
    /// ignored) where the conversation works, relative to that folder with `/` between
    /// folders. That's the repo, or the worktree of a run's session the conversation
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
    /// `/config key=value` left the key as it was, and why: a key that isn't a setting, or a
    /// value its picker doesn't offer. It prints under the command, as a saved change does.
    SettingRefused {
        key: String,
        why: String,
    },
    /// Every event of an attached run up to `seq` was history. Only later ones ring the bell or
    /// notify.
    ReplayDone {
        run: RunId,
        seq: u64,
    },
    /// An earlier conversation picked up again, with `/resume` or `argonauts -c`. An Argonauts
    /// Code conversation's past follows between `PastBegins` and `PastDrawn`; a CLI's own
    /// session comes with its turns, read from that session at no token cost.
    ConversationResumed {
        conversation: ConversationId,
        title: String,
        /// The agent of its last turn.
        worker: Option<ModelRef>,
        /// A CLI session's turns word for word, oldest first.
        turns: Vec<ReplayTurn>,
        /// The pages it published with Claude Code's Artifact tool, oldest first, read off its
        /// journal, so `/artifacts` and the key that opens the latest have them again.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifacts: Vec<crate::artifacts::Artifact>,
    },
    /// What follows, up to `PastDrawn`, is a conversation's or a run's record, drawn by the
    /// printers that drew it live, in order. Nothing in it happens again: no question opens, no
    /// view, no bell, no notice.
    PastBegins,
    /// The past is drawn, and what follows is live.
    PastDrawn,
    /// `/templates`: the workflows and the prompts, marked.
    Templates {
        list: crate::template_list::TemplateList,
    },
    /// A switch you flipped and what it now does, such as `/awake`: what's on, how to undo it,
    /// and what it can't do. It prints at every verbosity, since you asked for it.
    Switched {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        undo: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caveat: Option<String>,
    },
    /// What a command found, such as `/doctor`'s checks. It prints at every verbosity, since you
    /// asked for it. A report of the same title filling in takes this one's place.
    Report {
        title: String,
        lines: Vec<String>,
    },
    /// A report still filling in, such as `/update`'s with a row for each CLI done so far. It
    /// shows live until its `Report` of the same title comes.
    ReportSoFar {
        title: String,
        lines: Vec<String>,
    },
    /// What a command does in the background now, which the working line says, or `None` once
    /// it's done.
    CommandWorking {
        work: Option<CommandWork>,
    },
    /// Text shown folded, such as the summary a main model on another CLI starts from: the
    /// title, `lines` under it, and `more` after them only when Ctrl+O expands it.
    Folded {
        title: String,
        lines: Vec<String>,
        more: Vec<String>,
    },
    /// Something done or shown at your word, such as a workflow written or a workflow's plan
    /// previewed: `◆ title · tail`, then `lines` and the dim `notes` set in under it.
    Record {
        title: String,
        tail: Option<String>,
        lines: Vec<String>,
        notes: Vec<String>,
    },
    /// One dim line hung under the line before it, with no row between and no `⎿` of its own,
    /// as what a switch of the main model carried over hangs under `Set model to …`.
    Continued {
        text: String,
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
    /// You, an outside agent, or an agent.
    pub from: Party,
    pub text: String,
    /// An agent's turn of a CLI session carried on, as its transcript has it: what it wrote,
    /// thought and ran, in order. With none, `text` is all there is to print.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<crate::cli_sessions::CliStep>,
}

impl ReplayTurn {
    /// Words alone, as a conversation's journal keeps a turn.
    pub fn said(from: Party, text: impl Into<String>) -> Self {
        Self {
            from,
            text: text.into(),
            steps: Vec::new(),
        }
    }
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
    Waiting {
        run: RunId,
        reason: WaitReason,
    },
}

/// The header and bottom line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderAndBottomLine {
    /// The terminal title's text: the conversation's title, or the run's.
    pub title: String,
    /// With `~` for home.
    pub repo: String,
    pub workflow: String,
    /// `ultracode`, which the input rule's label shows beside the workflow.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ultracode: bool,
    /// The main agent's own ultra mode, while it is on, in the colour its CLI shows it in: the
    /// welcome header's mascot takes that look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultra: Option<UltraColour>,
    pub run: Option<RunId>,
    pub main_agent: ModelRef,
    /// One chip per agent, in `crew` order.
    pub plan_chips: Vec<PlanUsageChip>,
    /// What the bottom line draws beyond the chips, and how it's set up. Boxed, since every
    /// event is as large as the status it may carry.
    #[serde(default, skip_serializing_if = "LineFacts::is_plain")]
    pub line: Box<LineFacts>,
    pub spent: Option<Spend>,
    pub limit: Option<LimitAmount>,
    pub narrator: NarratorMode,
    pub approval_mode: ApprovalMode,
    /// A hold on the computer's sleep that you asked for with `/awake`, which the bottom
    /// line shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awake: Option<AwakeHold>,
}

/// The colour a CLI shows its ultra mode in: Claude Code's ultracode purple, Codex's ultra
/// effort blue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UltraColour {
    Purple,
    Blue,
}

/// How long `/awake` keeps the computer awake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "hold", rename_all = "snake_case")]
pub enum AwakeHold {
    UntilQuit,
    Until {
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
    },
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

/// An agent's chip, such as "Codex 38%" or "Claude down".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanUsageChip {
    /// Short name, such as `Claude`, `Codex`, `agy`.
    pub label: String,
    pub reading: Option<PlanUsageReading>,
    pub health: HealthState,
    /// Every plan window the CLI reported, one reading each, so the CLI in use can show both.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<PlanUsageReading>,
}

/// The bottom line's own facts: its settings, and the numbers of the parts you can add to it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LineFacts {
    /// `mods/bottom-line.toml`, your file under the repository's.
    #[serde(default, skip_serializing_if = "BottomLineSettings::is_default")]
    pub settings: BottomLineSettings,
    /// The branch checked out in the repository, for the `branch` part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The effort the main session's model runs at, which the working line names while the
    /// model thinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_effort: Option<String>,
    /// What each of the last seven days cost in dollars, oldest first and today last, for the
    /// `spend` part's sparkline. Empty when nothing was spent in them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spend_days: Vec<f64>,
    /// The artifact this conversation published last, for the `artifact` part, when the
    /// controller knows of one the screen didn't watch being published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<PublishedArtifact>,
}

impl LineFacts {
    /// The line as it ships, with nothing to add to it.
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// A page a session published on claude.ai with Claude Code's Artifact tool, as the bottom
/// line names it beside the mode: `⧉ release-notes +2 more`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedArtifact {
    /// The published file's name without its extension, else the page's title.
    pub name: String,
    /// How many more the conversation published before it, which the line adds as
    /// `+N more`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub others: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "conversation", rename_all = "snake_case")]
pub enum ConversationEvent {
    TurnStarted {
        turn: TurnId,
        text: String,
        worker: ModelRef,
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
    /// A watchdog stopped the turn, since it repeated one call or went quiet. Nothing falls
    /// back: another agent would get the same request.
    TurnStopped {
        turn: TurnId,
        /// The line under the turn: `Stopped: Qwen3 Coder 30B repeated the same execute call
        /// 12 times.`
        why: String,
    },
    /// A turn's agent ran out, and `to` carries on the same request in the same folder.
    FellBack {
        turn: TurnId,
        from: ModelRef,
        to: ModelRef,
        /// Such as "hit its usage limit".
        why: String,
        /// What `to` was told, such as "your request, the transcript so far, and that
        /// invoice/money.py changed".
        told: String,
        /// Set when `from`'s CLI turned out to be signed out, as its error said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed_out: Option<SignedOut>,
    },
    /// A message of yours that went into the running turn, once the model read it, as Claude
    /// Code's send now shows it.
    Joined {
        turn: TurnId,
        text: String,
    },
    /// The answer didn't fit the format the turn asked for, so its agent is asked for it again.
    /// What it said so far is set aside, and only the answer it ends with prints.
    AnswerRetried {
        turn: TurnId,
    },
    /// How the turn left its session's context, kept in the record as the turn ends so a
    /// resumed conversation counts it at once. The screen has it already while the turn runs.
    ContextLeft {
        turn: TurnId,
        /// When the turn ended.
        #[serde(with = "time::serde::rfc3339")]
        at: OffsetDateTime,
        /// In tokens.
        used: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<u64>,
        /// The CLI's own last count of what fills it, where its CLI counts one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Box<ContextUsage>>,
    },
    /// A slash command you typed, kept in the record where you typed it, so the conversation
    /// drawn again prints it. Live, the screen has printed it already.
    CommandTyped {
        text: String,
        /// When you typed it, so a run's record drawn again goes around it as it printed.
        /// Older records have none.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "time::serde::rfc3339::option"
        )]
        at: Option<OffsetDateTime>,
    },
    /// A line a command you typed printed back, kept after the command so the conversation
    /// drawn again prints it under it. Live, the screen has printed it already.
    CommandReplied {
        reply: CommandReply,
    },
}

/// One line a command you typed printed back under its echo: `Kept model as Opus 5.5`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReply {
    pub text: String,
    /// Drawn after `text` in the accent color: `Opus 5.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Dim words after the value, such as who the change is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// The whole line is dim, as the line a closing panel leaves is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
    /// The whole line is in the error colour, as a command's refusal is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
}

/// A CLI whose error said its sign-in is gone, and how to sign it in again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOut {
    /// What became of its sign-in, such as "its sign-in was revoked". Empty when its CLI only
    /// asked to log in again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// The command that signs it in again, such as `claude /login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
}

/// What a conversation turn turned out to be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum TurnReply {
    Answer {
        markdown: String,
    },
    /// The Captain-or-crew rules picked a mode and the run starts, or for direct Captain the
    /// session goes ahead in your checkout.
    Change {
        /// The agent's short plan of attack.
        markdown: String,
        estimate: ChangeEstimate,
        captain_or_crew: CaptainOrCrewDecision,
        /// What the request said about how to run it, checked, for the start line of a Captain
        /// that works in your checkout. A run's own start line has them from its start.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        instructions: Vec<CheckedInstruction>,
    },
    /// Could be either; a choice question follows.
    Unclear {
        markdown: String,
        estimate: Option<ChangeEstimate>,
        would_pick: Option<RunMode>,
    },
}

/// An app-level ◆ question.
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
    /// Captain `s`, Crew run `c`, No, just asking `n`. Esc sets it aside.
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
    /// Check commands found in the repo, offered once: Use these `y`, Edit them `e`, or Skip
    /// `n`. Esc leaves it for the next run.
    DetectedChecks,
    /// Whether to trust the project settings files, which set safety keys: Trust them `y`, Not
    /// now `n`, or Show the file `d`. Asked again when a file's hash changes. Esc means not now.
    TrustProject,
    /// `/rewind`: Rewind `y`, which resets the task's branch and keeps the replaced work on a
    /// side branch, or Show the diff `d`. Esc cancels.
    ConfirmRewind,
    /// `/connect`: Write it `y`, which keeps a backup of the outside agent's config first, or Not
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
    /// Plan first in direct Captain: Yes, go ahead `y` resumes the session that planned as a turn
    /// that works in your checkout, or No `n`, and nothing changes. Esc means No.
    GoAhead,
    /// A direct turn's agent ran out: one option per agent that may carry it on in your checkout,
    /// `1` to `9`, then Stop here `s`. Esc stops.
    Fallback,
    /// `/workflow try`: what a workflow would run for a request, in the body. Run it `y` runs
    /// that one request with it, and Edit it `e` opens its file. Esc closes, and nothing ran.
    TryWorkflow,
    /// The main session's proposed setup: where it applies (this request, this conversation,
    /// this repository, all your repositories) or Not now `n`, and Show what would be written
    /// `d`. The body is the setup, then after a blank line what each place would write. Esc
    /// means not now. Each row of the setup is split by tabs into parts, plain then dim in
    /// turn, so its notes show dim.
    Setup,
    /// `/git`: how this folder keeps history, one option per way with the one in use marked,
    /// then, for making it a repository, whether to go ahead once the body has shown what
    /// the first commit would hold. Esc cancels, and nothing changes.
    Git,
    /// `/templates`: one option per workflow and per role's prompts, each with its marks as
    /// its detail, then where the picked one's copy goes: This folder `t` or Every folder
    /// `e`. Esc copies nothing.
    Templates,
    /// A settings file still uses names that changed: Rename them `r`, which keeps a copy of
    /// the file, or Leave them `l`, asked again only when the file changes. The body lists each
    /// old name beside its new one, each row split by tabs into parts, plain then dim in turn.
    /// Esc leaves them.
    RenamedSettings,
    /// A CLI the run about to start would use must be updated first: Update it now `u`, Leave
    /// it out of this run `l`, offered where another CLI can take its roles, or Run with it
    /// anyway `r`. Each option's detail is drawn beside it. Esc starts nothing.
    NeededUpdate,
}

/// A run session's last whole count and when its CLI gave it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCounted {
    pub usage: Box<ContextUsage>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "data", rename_all = "snake_case")]
pub enum ViewData {
    /// The `/resume` picker. A run is never listed on its own: it comes back with the
    /// conversation that started it.
    ResumeList {
        /// Earlier conversations with the main agent, newest first.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        conversations: Vec<ConversationSummary>,
        /// The sessions of the main agent's CLI for this folder, newest first, listed with the
        /// rest under a Kind column.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sessions: Vec<CliSession>,
    },
    /// A goal run's goal.
    Goal {
        view: Box<crate::views::GoalView>,
    },
    Status {
        summary: StatusSummary,
    },
    StatusTab {
        view: Box<crate::status_tab::StatusTabView>,
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
    /// `/mods`: every mod, with what its page edits and what its preview draws.
    Mods {
        view: Box<crate::settings::mods::ModsView>,
    },
    Models {
        /// Models the role's rules allow, best fit first.
        choices: Vec<ModelChoice>,
        /// `None` is the main agent. A pick for the main agent sends `Slash { Agent }`,
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
        /// Each list the choices come from, with when it was read and whether it's being
        /// asked again, so each level of the picker says how fresh its own list is.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sources: Vec<crate::model_picker::ModelSource>,
        /// What this machine gives models, when it can be read, for the level of a CLI that
        /// runs models here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memory: Option<crate::model_picker::MachineMemory>,
    },
    /// `/effort` with no level: the slider for the main agent's or the role's model. Enter
    /// sends `Slash { Effort }` with the level.
    Effort {
        slider: EffortSlider,
    },
    /// The agent panel's rows from the controller, which the screen adds to its own sessions'.
    Crew {
        panel: AgentPanelRows,
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
    /// `/context print`: the same sessions, to go into the conversation as rows.
    ContextPrint {
        view: ContextView,
    },
    /// One run session's last whole count, as its folder keeps it, and when it was counted.
    /// `None` for a session never counted.
    SessionContext {
        run: RunId,
        session: SessionKey,
        counted: Option<SessionCounted>,
    },
    /// What `/usage` shows, and what the cards pinned above the input draw from.
    Usage {
        summary: Box<crate::api::UsageSummary>,
    },
    /// What the Overview tab draws.
    Overview {
        overview: Box<crate::overview::OverviewData>,
    },
    AllRepos {
        /// This repo first, then the others by their newest update.
        repos: Vec<RepoRuns>,
    },
    /// The built-ins, then your files, then the repo's, a file replacing a workflow of the same
    /// name.
    WorkflowLibrary {
        workflows: Vec<WorkflowFile>,
        /// Sent after a copy or a diff for the Library tab already open, so nothing opens for
        /// it when that tab has closed.
        #[serde(default)]
        refresh: bool,
    },
    /// `/workflow edit <name>`: the file's steps in order, each with its keys and the check.
    WorkflowPage {
        page: Box<crate::workflow_page::WorkflowPage>,
    },
    /// `/records`, `/learned`, `/skills`, or `/memory`: one picker with the area's tabs, each
    /// row's detail and actions, and a Settings tab.
    Learning {
        view: Box<LearningView>,
    },
    /// `/prompts`: every role's parts, who writes each, and which you changed.
    Prompts {
        view: Box<crate::prompt_list::PromptList>,
    },
    /// `/rewind`: the fallback checkpoints of `task`, or of every task, oldest first. Picking one
    /// sends `UiCommand::Rewind`.
    Checkpoints {
        run: RunId,
        task: Option<TaskId>,
        checkpoints: Vec<CheckpointRow>,
    },
}

/// One conversation with the main agent, read from `conversation/<conversation>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub conversation: ConversationId,
    /// From its first prompt, or its first run when it has no turn.
    pub title: String,
    /// When it last had a turn or opened a run.
    #[serde(with = "time::serde::rfc3339")]
    pub updated: time::OffsetDateTime,
    /// The agent of its last turn.
    pub worker: Option<ModelRef>,
    pub turns: u32,
    /// The runs it started or opened, which carrying it on brings back to the agent panel.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub runs: u32,
    /// One of the runs it started waits on you.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_you: bool,
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
    /// `built-in`, `global`, `project`, `project-local`, `workflow`, `flag`, or `session`. The
    /// scope question warns when it's above the scope you pick, since saving there changes nothing.
    pub layer: String,
    /// For a value from the workflow, the settings file that picked the workflow (`global`,
    /// `project`, or `project-local`), since saving there or later wins over it. `None` when the
    /// workflow is the default or a flag picked it, and sits above every file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_from: Option<String>,
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
    pub worker: ModelRef,
    pub provider: String,
    pub access: ModelAccess,
    /// For a typical task.
    pub cost: CostEstimate,
    pub plan_usage: Option<PlanUsageReading>,
    /// From the registry entry; every level when unknown.
    pub efforts: Vec<String>,
    pub strengths: Vec<String>,
    /// The role's current model.
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
    /// The level set for it now, for the main agent or the role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// A short name the CLI resolves itself, such as `opus`. The models level lists aliases on
    /// one line under Default.
    #[serde(default, skip_serializing_if = "is_false")]
    pub alias: bool,
    /// For the CLI's default or an alias: the display name of the model it runs now, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<String>,
    /// What a CLI that lists its models by provider, as OpenCode does, says of this one: its
    /// group, its window, whether it can use tools, and where it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed: Option<crate::model_picker::ListedFacts>,
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
    /// One of the names in [`crate::settings::layer`].
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
