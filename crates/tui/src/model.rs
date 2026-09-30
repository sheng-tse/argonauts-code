//! The run as the screen sees it, an ordered transcript of entries built from run events.
//! Agent, gate, and repo text is cleaned here on the way in, so renderers only see safe text.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use argonauts_core::{
    AgentEvent, BudgetAction, BudgetLimit, BudgetMeter, BudgetStatus, CardId, Checkpoint,
    CommitInfo, Config, DEFAULT_SOFT_RATIO, DecidedBy, DecisionRecord, ErrorCategory, GateResult,
    MergeCard, MergeDecision, Note, NoteLevel, Phase, PhaseEvent, PhaseStatus, Report,
    ReportStatus, RetryReason, RunEvent, RunEventKind, RunFinished, RunOutcome, RunStarted,
    SessionEnded, SessionRole, SessionStarted, Spend, TodoItem, Verbosity, title_from_task,
};
use time::OffsetDateTime;

use crate::format;
use crate::sanitize::{clean, clean_line};
use crate::text::truncate;
use crate::tools::ToolSummary;

/// Longest raw unknown line kept for display, in columns.
const UNKNOWN_MAX: usize = 300;

/// The state of a step, shown as a glyph, a word, and a color so no state depends on color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    NeedsYou,
    Working,
    Waiting,
    Done,
    Failed,
    Stopped,
    Warning,
    Info,
}

impl State {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::NeedsYou => "◆",
            Self::Working => "⏺",
            Self::Waiting => "…",
            Self::Done => "●",
            Self::Failed => "✗",
            Self::Stopped => "■",
            Self::Warning => "!",
            Self::Info => "○",
        }
    }
}

pub(crate) fn shows(current: Verbosity, needed: Verbosity) -> bool {
    rank(current) >= rank(needed)
}

fn rank(v: Verbosity) -> u8 {
    match v {
        Verbosity::Quiet => 0,
        Verbosity::Normal => 1,
        Verbosity::Full => 2,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunInfo {
    pub task: String,
    pub card_title: String,
    pub mode: String,
    pub agent: String,
    pub model: Option<String>,
    pub repo: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Step {
    pub state: State,
    pub text: String,
    pub details: Vec<String>,
    /// The least verbose level that still shows this step.
    pub shown_from: Verbosity,
}

#[derive(Debug)]
pub(crate) enum Entry {
    Step(Step),
    /// An index into [`RunView::sessions`].
    Session(usize),
    Gates(GatesView),
    Merge(Box<MergeView>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolStatus {
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolCall {
    pub summary: ToolSummary,
    pub status: ToolStatus,
    pub output: Option<String>,
    pub parent: Option<String>,
    /// Tool calls made by a subagent this call started.
    pub children: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    Tool(ToolCall),
    Message {
        text: String,
        parent: Option<String>,
    },
    Reasoning(String),
    File {
        path: String,
        kind: String,
    },
    Todo(Vec<TodoItem>),
    Error(String),
    Retry(String),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionResult {
    pub is_error: bool,
    pub text: Option<String>,
}

#[derive(Debug)]
pub(crate) struct SessionView {
    pub agent: String,
    pub model: Option<String>,
    pub role: SessionRole,
    pub card_id: Option<CardId>,
    pub card_title: Option<String>,
    cwd: Option<PathBuf>,
    pub command: Option<String>,
    pub started: OffsetDateTime,
    pub ended: Option<SessionEnded>,
    pub items: Vec<Item>,
    tool_index: HashMap<String, usize>,
    pub stream: String,
    pub todos: Vec<TodoItem>,
    /// Index in `items` of the latest tool call that carries a diff preview.
    pub last_diff: Option<usize>,
    meter: BudgetMeter,
    pub result: Option<SessionResult>,
    pub report: Option<Report>,
    pub last_error: Option<String>,
    /// The latest retry notice, until the next event replaces it.
    pub retry: Option<String>,
    pub unknown: usize,
    pub files_changed: BTreeSet<String>,
}

impl SessionView {
    fn new(
        agent: &str,
        model: Option<&str>,
        role: SessionRole,
        card_id: Option<CardId>,
        started: OffsetDateTime,
    ) -> Self {
        Self {
            agent: clean_line(agent),
            model: model.map(clean_line),
            role,
            card_id,
            card_title: None,
            cwd: None,
            command: None,
            started,
            ended: None,
            items: Vec::new(),
            tool_index: HashMap::new(),
            stream: String::new(),
            todos: Vec::new(),
            last_diff: None,
            meter: BudgetMeter::new(BudgetLimit::None, DEFAULT_SOFT_RATIO),
            result: None,
            report: None,
            last_error: None,
            retry: None,
            unknown: 0,
            files_changed: BTreeSet::new(),
        }
    }

    /// `claude-code/haiku`, or just the agent id when the model is unknown.
    pub(crate) fn agent_label(&self) -> String {
        match &self.model {
            Some(model) => format!("{}/{}", self.agent, model),
            None => self.agent.clone(),
        }
    }

    pub(crate) fn role_label(&self) -> &'static str {
        role_word(self.role)
    }

    pub(crate) fn is_running(&self) -> bool {
        self.ended.is_none()
    }

    pub(crate) fn state(&self) -> State {
        let Some(end) = &self.ended else {
            return State::Working;
        };
        if end.stopped_by.is_some() {
            return State::Stopped;
        }
        let errored = self.result.as_ref().is_some_and(|r| r.is_error);
        let report_failed = self
            .report
            .as_ref()
            .is_some_and(|r| matches!(r.status, ReportStatus::Failed | ReportStatus::Blocked));
        if end.exit_code != Some(0) || errored || report_failed {
            State::Failed
        } else {
            State::Done
        }
    }

    /// Milliseconds since the session started, or its full length once it ended.
    pub(crate) fn elapsed_ms(&self, now: OffsetDateTime) -> u64 {
        if let Some(end) = &self.ended {
            return end.duration_ms;
        }
        let ms = (now - self.started).whole_milliseconds();
        u64::try_from(ms).unwrap_or(0)
    }

    pub(crate) fn spend(&self) -> Spend {
        self.meter.spend()
    }

    pub(crate) fn last_message(&self) -> Option<&str> {
        self.items.iter().rev().find_map(|item| match item {
            Item::Message { text, parent: None } => Some(text.as_str()),
            _ => None,
        })
    }

    fn apply(&mut self, event: &AgentEvent) {
        if !matches!(event, AgentEvent::Retry { .. } | AgentEvent::Usage { .. }) {
            self.retry = None;
        }
        match event {
            AgentEvent::Init { model, .. } => {
                if self.model.is_none() {
                    self.model = model.as_deref().map(clean_line);
                }
            }
            AgentEvent::TextDelta { text, parent: None } => self.stream.push_str(&clean(text)),
            // Subagent text only shows as whole messages, so it never mixes into the main stream.
            AgentEvent::TextDelta { .. } => {}
            AgentEvent::Message { text, parent } => {
                if parent.is_none() {
                    self.stream.clear();
                }
                self.push_message(clean(text), parent.clone());
            }
            AgentEvent::Reasoning { text } => self.items.push(Item::Reasoning(clean(text))),
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => self.tool_started(id, name, input, parent.as_deref()),
            AgentEvent::ToolFinished {
                id,
                ok,
                output,
                parent,
            } => self.tool_finished(id, *ok, output.as_deref(), parent.as_deref()),
            AgentEvent::FileChanged { path, kind } => {
                let path = clean_line(path);
                self.files_changed.insert(path.clone());
                self.items.push(Item::File {
                    path,
                    kind: clean_line(kind),
                });
            }
            AgentEvent::Todo { items } => {
                self.todos = items
                    .iter()
                    .map(|item| TodoItem {
                        text: clean_line(&item.text),
                        status: item.status,
                    })
                    .collect();
                self.items.push(Item::Todo(self.todos.clone()));
            }
            AgentEvent::Usage { .. } => {
                self.meter.observe(event);
            }
            AgentEvent::Retry { reason, delay_ms } => {
                let text = retry_text(reason, *delay_ms);
                self.retry = Some(text.clone());
                self.items.push(Item::Retry(text));
            }
            // No adapter emits these yet, and only the interactive screen will draw them.
            AgentEvent::QuotaSignal { .. }
            | AgentEvent::Subagent { .. }
            | AgentEvent::Quota { .. }
            | AgentEvent::ContextFill { .. } => {}
            AgentEvent::Error {
                category, message, ..
            } => {
                let text = format!(
                    "{} error · {}",
                    category_word(*category),
                    clean_line(message)
                );
                self.last_error = Some(text.clone());
                self.items.push(Item::Error(text));
            }
            AgentEvent::Result { text, is_error, .. } => {
                self.flush_stream();
                self.result = Some(SessionResult {
                    is_error: *is_error,
                    text: text.as_deref().map(clean),
                });
            }
            AgentEvent::Unknown { raw } => {
                self.unknown += 1;
                let line = truncate(&clean_line(&raw.to_string()), UNKNOWN_MAX);
                self.items.push(Item::Unknown(line));
            }
        }
    }

    fn tool_started(
        &mut self,
        id: &str,
        name: &str,
        input: &serde_json::Value,
        parent: Option<&str>,
    ) {
        if parent.is_none() {
            self.flush_stream();
        }
        if let Some(parent) = parent
            && let Some(Item::Tool(call)) = self.tool_index.get(parent).map(|&i| &mut self.items[i])
        {
            call.children += 1;
        }
        let summary = ToolSummary::from_call(name, input, self.cwd.as_deref());
        if !summary.diff.is_empty() && parent.is_none() {
            self.last_diff = Some(self.items.len());
        }
        self.tool_index.insert(id.to_owned(), self.items.len());
        self.items.push(Item::Tool(ToolCall {
            summary,
            status: ToolStatus::Running,
            output: None,
            parent: parent.map(str::to_owned),
            children: 0,
        }));
    }

    fn tool_finished(&mut self, id: &str, ok: bool, output: Option<&str>, parent: Option<&str>) {
        let status = if ok {
            ToolStatus::Ok
        } else {
            ToolStatus::Failed
        };
        let output = output.map(clean);
        if let Some(Item::Tool(call)) = self.tool_index.get(id).map(|&i| &mut self.items[i]) {
            call.status = status;
            call.output = output;
            return;
        }
        let summary =
            ToolSummary::from_call("tool", &serde_json::Value::String(id.to_owned()), None);
        self.tool_index.insert(id.to_owned(), self.items.len());
        self.items.push(Item::Tool(ToolCall {
            summary,
            status,
            output,
            parent: parent.map(str::to_owned),
            children: 0,
        }));
    }

    fn push_message(&mut self, text: String, parent: Option<String>) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        // A streamed message may have been flushed already; the whole message repeats it.
        let repeated = self.items.iter().rev().take(3).any(|item| {
            matches!(item, Item::Message { text: earlier, parent: p } if p == &parent && earlier.trim() == trimmed)
        });
        if !repeated {
            self.items.push(Item::Message {
                text: trimmed.to_owned(),
                parent,
            });
        }
    }

    fn flush_stream(&mut self) {
        if !self.stream.is_empty() {
            let text = std::mem::take(&mut self.stream);
            self.push_message(text, None);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GatesView {
    pub running: bool,
    pub results: Vec<GateResult>,
}

impl GatesView {
    pub(crate) fn passed(&self) -> usize {
        self.results.iter().filter(|g| g.passed).count()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MergeView {
    pub card_id: Option<CardId>,
    pub card: MergeCard,
    pub diff: Option<String>,
    pub awaiting: bool,
    /// What the user picked, before the conductor logs it.
    pub chosen: Option<MergeDecision>,
    pub decided: Option<DecisionRecord>,
}

#[derive(Debug)]
pub(crate) struct RunView {
    home: Option<PathBuf>,
    pub run_id: Option<String>,
    pub run: Option<RunInfo>,
    pub entries: Vec<Entry>,
    pub sessions: Vec<SessionView>,
    pub active_phase: Option<Phase>,
    pub finished: Option<RunFinished>,
    meter: BudgetMeter,
    sessions_started: usize,
    pub budget: Option<BudgetStatus>,
    config_limit: BudgetLimit,
    budget_level: BudgetAction,
    branch: Option<String>,
}

impl RunView {
    pub(crate) fn new(home: Option<PathBuf>) -> Self {
        Self {
            home,
            run_id: None,
            run: None,
            entries: Vec::new(),
            sessions: Vec::new(),
            active_phase: None,
            finished: None,
            meter: BudgetMeter::new(BudgetLimit::None, DEFAULT_SOFT_RATIO),
            sessions_started: 0,
            budget: None,
            config_limit: BudgetLimit::None,
            budget_level: BudgetAction::Continue,
            branch: None,
        }
    }

    pub(crate) fn apply(&mut self, event: &RunEvent) {
        if self.run_id.is_none() {
            self.run_id = Some(event.run_id.to_string());
        }
        match &event.kind {
            RunEventKind::RunStarted(started) => self.run_started(started),
            RunEventKind::Agent(agent) => {
                if matches!(agent, AgentEvent::Usage { .. }) {
                    self.meter.observe(agent);
                }
                self.session_for(event).apply(agent);
            }
            RunEventKind::Phase(phase) => self.phase(phase),
            RunEventKind::SessionStarted(started) => {
                self.session_started(started, event.card_id.clone(), event.ts);
            }
            RunEventKind::SessionEnded(ended) => {
                if let Some(session) = self.running_session_mut(event.card_id.as_ref()) {
                    session.flush_stream();
                    session.ended = Some(ended.clone());
                }
            }
            RunEventKind::GateResult(gate) => self.gate_result(gate),
            RunEventKind::Commit(commit) => self.commit(commit),
            RunEventKind::Report(report) => self.report(report, event.card_id.as_ref()),
            RunEventKind::Checkpoint(Checkpoint::Merge(card)) => {
                self.upsert_merge(event.card_id.clone(), card, None, false);
            }
            RunEventKind::Decision(record) => self.decision(record),
            RunEventKind::Budget(status) => self.budget(status),
            RunEventKind::Note(note) => self.note(note),
            RunEventKind::RunFinished(finished) => self.run_finished(finished),
        }
    }

    pub(crate) fn show_merge_prompt(
        &mut self,
        card_id: &CardId,
        card: &MergeCard,
        diff: Option<&str>,
    ) {
        self.upsert_merge(Some(card_id.clone()), card, diff, true);
    }

    pub(crate) fn mark_chosen(&mut self, decision: MergeDecision) {
        if let Some(merge) = self.open_merge_mut() {
            merge.awaiting = false;
            merge.chosen = Some(decision);
        }
    }

    pub(crate) fn pending_merge(&self) -> Option<&MergeView> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Merge(merge) if merge.awaiting => Some(merge.as_ref()),
            _ => None,
        })
    }

    pub(crate) fn merge_diff(&self) -> Option<&str> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Merge(merge) => merge.diff.as_deref(),
            _ => None,
        })
    }

    /// Whether anything on screen animates or counts time.
    pub(crate) fn is_live(&self) -> bool {
        self.finished.is_none()
            && (self.active_phase.is_some()
                || self.sessions.iter().any(SessionView::is_running)
                || self
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, Entry::Gates(gates) if gates.running)))
    }

    /// Spend so far. The conductor's numbers win unless ours are newer.
    pub(crate) fn spend(&self) -> Spend {
        let own = self.meter.spend();
        match &self.budget {
            Some(status) if status.spend.billable_tokens() >= own.billable_tokens() => status.spend,
            _ => own,
        }
    }

    pub(crate) fn limit(&self) -> BudgetLimit {
        self.budget.map_or(self.config_limit, |status| status.limit)
    }

    fn run_started(&mut self, started: &RunStarted) {
        let repo = format::display_path(&started.repo_root, self.home.as_deref());
        let task = clean(&started.task);
        if let Ok(config) = serde_json::from_value::<Config>(started.config.clone()) {
            self.config_limit = config.budget.per_card_limit();
        }
        self.run = Some(RunInfo {
            card_title: title_from_task(&task),
            task,
            mode: clean_line(&started.mode),
            agent: clean_line(&started.agent),
            model: started.model.as_deref().map(clean_line),
            repo: clean_line(&repo),
        });
        let mut text = format!(
            "Recorded · commit {}",
            format::short_sha(&clean_line(&started.base_commit))
        );
        if let Some(branch) = &started.base_branch {
            text.push_str(&format!(" on {}", clean_line(branch)));
        }
        self.push_step(State::Done, text, Vec::new(), Verbosity::Normal);
    }

    fn session_started(
        &mut self,
        started: &SessionStarted,
        card_id: Option<CardId>,
        ts: OffsetDateTime,
    ) {
        self.sessions_started += 1;
        if self.sessions_started > 1 {
            self.meter.start_new_session();
        }
        let mut session = SessionView::new(
            &started.agent,
            started.model.as_deref(),
            started.role,
            card_id,
            ts,
        );
        session.cwd = Some(started.cwd.clone());
        let command = format::command_line(&started.program, &started.args);
        session.command = Some(truncate(&clean_line(&command), UNKNOWN_MAX));
        session.card_title = self.run.as_ref().map(|run| run.card_title.clone());
        self.push_session(session);
    }

    fn push_session(&mut self, session: SessionView) -> usize {
        self.sessions.push(session);
        let index = self.sessions.len() - 1;
        self.entries.push(Entry::Session(index));
        index
    }

    fn session_for(&mut self, event: &RunEvent) -> &mut SessionView {
        let found = self.sessions.iter().rposition(|s| {
            s.is_running() && (event.card_id.is_none() || s.card_id == event.card_id)
        });
        let index = match found {
            Some(index) => index,
            None => {
                let mut session = SessionView::new(
                    &event.source,
                    None,
                    SessionRole::Build,
                    event.card_id.clone(),
                    event.ts,
                );
                session.card_title = self.run.as_ref().map(|run| run.card_title.clone());
                self.push_session(session)
            }
        };
        &mut self.sessions[index]
    }

    fn running_session_mut(&mut self, card_id: Option<&CardId>) -> Option<&mut SessionView> {
        self.sessions
            .iter_mut()
            .rev()
            .find(|s| s.is_running() && (card_id.is_none() || s.card_id.as_ref() == card_id))
    }

    fn phase(&mut self, event: &PhaseEvent) {
        let detail = event.detail.as_deref().map(clean_line);
        match event.status {
            PhaseStatus::Started => self.phase_started(event.phase),
            PhaseStatus::Finished => self.phase_finished(event.phase, detail),
            PhaseStatus::Failed => self.phase_failed(event.phase, detail),
            PhaseStatus::Skipped => self.phase_skipped(event.phase, detail),
        }
    }

    fn phase_started(&mut self, phase: Phase) {
        self.active_phase = Some(phase);
        if phase == Phase::Gates {
            self.entries.push(Entry::Gates(GatesView {
                running: true,
                results: Vec::new(),
            }));
        }
    }

    fn phase_finished(&mut self, phase: Phase, detail: Option<String>) {
        self.end_phase(phase);
        let shown_from = match phase {
            Phase::Goal | Phase::Worktree => Verbosity::Normal,
            Phase::Intake | Phase::Merge | Phase::Cleanup => Verbosity::Full,
            // Their own entries already show how these went.
            Phase::Agent | Phase::Commit | Phase::Gates | Phase::Decision => return,
        };
        let name = phase_name(phase);
        let text = match (detail, phase) {
            (Some(detail), _) => format!("{name} · {detail}"),
            (None, Phase::Worktree) => "Worktree ready".to_owned(),
            (None, _) => format!("{name} done"),
        };
        self.push_step(State::Done, text, Vec::new(), shown_from);
    }

    fn phase_failed(&mut self, phase: Phase, detail: Option<String>) {
        self.end_phase(phase);
        if phase == Phase::Gates && self.last_gates_has_results() {
            return;
        }
        let name = phase_name(phase);
        let text = match detail {
            Some(detail) => format!("{name} failed · {detail}"),
            None => format!("{name} failed"),
        };
        self.push_step(State::Failed, text, Vec::new(), Verbosity::Quiet);
    }

    fn phase_skipped(&mut self, phase: Phase, detail: Option<String>) {
        if phase == Phase::Gates
            && matches!(self.entries.last(), Some(Entry::Gates(g)) if g.results.is_empty())
        {
            self.entries.pop();
        }
        self.end_phase(phase);
        let name = phase_name(phase);
        let text = match detail {
            Some(detail) => format!("{name} skipped · {detail}"),
            None => format!("{name} skipped"),
        };
        self.push_step(State::Stopped, text, Vec::new(), Verbosity::Normal);
    }

    fn end_phase(&mut self, phase: Phase) {
        if self.active_phase == Some(phase) {
            self.active_phase = None;
        }
        if phase == Phase::Gates
            && let Some(gates) = self.open_gates_mut()
        {
            gates.running = false;
        }
    }

    /// The gates entry still collecting results, even if a note landed after it.
    fn open_gates_mut(&mut self) -> Option<&mut GatesView> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Gates(gates) if gates.running => Some(gates),
            _ => None,
        })
    }

    fn last_gates_has_results(&self) -> bool {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Gates(gates) => Some(!gates.results.is_empty()),
            _ => None,
        }) == Some(true)
    }

    fn gate_result(&mut self, gate: &GateResult) {
        let gate = GateResult {
            command: gate.command.iter().map(|c| clean_line(c)).collect(),
            output: clean(&gate.output),
            ..gate.clone()
        };
        if let Some(gates) = self.open_gates_mut() {
            gates.results.push(gate);
            return;
        }
        let running = self.active_phase == Some(Phase::Gates);
        self.entries.push(Entry::Gates(GatesView {
            running,
            results: vec![gate],
        }));
    }

    fn commit(&mut self, commit: &CommitInfo) {
        let branch = clean_line(&commit.branch);
        self.branch = Some(branch.clone());
        let (text, shown_from) = match &commit.sha {
            Some(sha) => (
                format!(
                    "Committed {} on {} · {}",
                    format::short_sha(&clean_line(sha)),
                    branch,
                    format::diff_stat(&commit.stat)
                ),
                Verbosity::Normal,
            ),
            None => (
                "Nothing to commit · the agent changed no files".to_owned(),
                Verbosity::Quiet,
            ),
        };
        self.push_step(State::Done, text, Vec::new(), shown_from);
    }

    fn report(&mut self, report: &Report, card_id: Option<&CardId>) {
        let report = clean_report(report);
        let target = self
            .sessions
            .iter_mut()
            .rev()
            .find(|s| card_id.is_none() || s.card_id.as_ref() == card_id);
        match target {
            Some(session) => session.report = Some(report),
            None => {
                let state = report_state(report.status);
                let text = format!("Report · {} · {}", report.status.as_str(), report.summary);
                self.push_step(state, text, Vec::new(), Verbosity::Quiet);
            }
        }
    }

    fn upsert_merge(
        &mut self,
        card_id: Option<CardId>,
        card: &MergeCard,
        diff: Option<&str>,
        awaiting: bool,
    ) {
        let card = clean_card(card);
        self.branch = Some(card.branch.clone());
        let diff = diff.map(clean);
        let existing = self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Merge(merge) if merge.decided.is_none() && merge.card_id == card_id => {
                Some(merge)
            }
            _ => None,
        });
        if let Some(merge) = existing {
            merge.card = card;
            if diff.is_some() {
                merge.diff = diff;
            }
            merge.awaiting |= awaiting;
            return;
        }
        self.entries.push(Entry::Merge(Box::new(MergeView {
            card_id,
            card,
            diff,
            awaiting,
            chosen: None,
            decided: None,
        })));
    }

    fn open_merge_mut(&mut self) -> Option<&mut MergeView> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Merge(merge) if merge.decided.is_none() => Some(merge.as_mut()),
            _ => None,
        })
    }

    fn decision(&mut self, record: &DecisionRecord) {
        let record = DecisionRecord {
            decision: clean_decision(&record.decision),
            ..record.clone()
        };
        match self.open_merge_mut() {
            Some(merge) => {
                merge.awaiting = false;
                merge.decided = Some(record);
            }
            None => {
                let text = format!("Merge · {}", decision_text(&record.decision));
                self.push_step(State::Done, text, Vec::new(), Verbosity::Quiet);
            }
        }
    }

    fn budget(&mut self, status: &BudgetStatus) {
        self.budget = Some(*status);
        if status.level == self.budget_level {
            return;
        }
        self.budget_level = status.level;
        // What the conductor does about it arrives as a note.
        let limit = format::budget_limit(status);
        match status.level {
            BudgetAction::Continue => {}
            BudgetAction::WrapUp => {
                let used = status
                    .fraction_used
                    .map_or_else(|| "most".to_owned(), |f| format!("{:.0}%", f * 100.0));
                self.push_step(
                    State::Warning,
                    format!("Budget {used} used of {limit}"),
                    Vec::new(),
                    Verbosity::Quiet,
                );
            }
            BudgetAction::Stop => self.push_step(
                State::Stopped,
                format!("Budget used up · {limit}"),
                Vec::new(),
                Verbosity::Quiet,
            ),
        }
    }

    fn note(&mut self, note: &Note) {
        let (state, shown_from) = match note.level {
            NoteLevel::Info => (State::Info, Verbosity::Normal),
            NoteLevel::Warn => (State::Warning, Verbosity::Quiet),
            NoteLevel::Error => (State::Failed, Verbosity::Quiet),
        };
        let text = clean(&note.text);
        let mut lines = text.lines().map(str::to_owned);
        let first = lines.next().unwrap_or_default();
        self.push_step(state, first, lines.collect(), shown_from);
    }

    fn run_finished(&mut self, finished: &RunFinished) {
        self.active_phase = None;
        let finished = RunFinished {
            detail: finished.detail.as_deref().map(clean),
            ..finished.clone()
        };
        let (state, mut text) = match finished.outcome {
            RunOutcome::Merged => (State::Done, "Merged".to_owned()),
            RunOutcome::BranchLeft => (State::Done, "Branch left".to_owned()),
            RunOutcome::Failed => (State::Failed, "Run failed".to_owned()),
            RunOutcome::Cancelled => (State::Stopped, "Run cancelled".to_owned()),
        };
        if let Some(branch) = &self.branch {
            text.push_str(&format!(" · {branch}"));
        }
        let details = finished
            .detail
            .as_deref()
            .map(|d| d.lines().map(str::to_owned).collect())
            .unwrap_or_default();
        self.push_step(state, text, details, Verbosity::Quiet);
        self.finished = Some(finished);
    }

    fn push_step(
        &mut self,
        state: State,
        text: String,
        details: Vec<String>,
        shown_from: Verbosity,
    ) {
        self.entries.push(Entry::Step(Step {
            state,
            text,
            details,
            shown_from,
        }));
    }
}

pub(crate) fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Intake => "Intake",
        Phase::Goal => "Goal",
        Phase::Worktree => "Worktree",
        Phase::Agent => "Agent",
        Phase::Commit => "Commit",
        Phase::Gates => "Gates",
        Phase::Decision => "Decision",
        Phase::Merge => "Merge",
        Phase::Cleanup => "Cleanup",
    }
}

pub(crate) fn phase_activity(phase: Phase) -> &'static str {
    match phase {
        Phase::Intake => "Recording the run",
        Phase::Goal => "Recording the goal",
        Phase::Worktree => "Creating the worktree",
        Phase::Agent => "Starting the agent",
        Phase::Commit => "Committing the agent's changes",
        Phase::Gates => "Running gates",
        Phase::Decision => "Preparing the merge card",
        Phase::Merge => "Merging",
        Phase::Cleanup => "Cleaning up",
    }
}

pub(crate) fn report_state(status: ReportStatus) -> State {
    match status {
        ReportStatus::Done => State::Done,
        ReportStatus::Blocked | ReportStatus::Failed => State::Failed,
        ReportStatus::NeedsDecision => State::NeedsYou,
    }
}

pub(crate) fn decision_text(decision: &MergeDecision) -> String {
    match decision {
        MergeDecision::Approve => "approve and merge".to_owned(),
        MergeDecision::RequestChanges { note: Some(note) } => {
            format!("request changes, \"{note}\"")
        }
        MergeDecision::RequestChanges { note: None } => "request changes".to_owned(),
        MergeDecision::OpenPr => "open a pull request".to_owned(),
        MergeDecision::Leave => "leave the branch".to_owned(),
    }
}

pub(crate) fn decider_word(by: DecidedBy) -> &'static str {
    match by {
        DecidedBy::User => "you",
        DecidedBy::Config => "config",
    }
}

pub(crate) fn role_word(role: SessionRole) -> &'static str {
    match role {
        SessionRole::Build => "owner",
        SessionRole::ReadOnly => "read-only",
    }
}

pub(crate) fn category_word(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::Auth => "auth",
        ErrorCategory::UsageLimit => "usage limit",
        ErrorCategory::RateLimit => "rate limit",
        ErrorCategory::Billing => "billing",
        ErrorCategory::Overloaded => "overloaded",
        ErrorCategory::Server => "server",
        ErrorCategory::Network => "network",
        ErrorCategory::ContextFull => "context full",
        ErrorCategory::ModelUnavailable => "model unavailable",
        ErrorCategory::InvalidRequest => "invalid request",
        ErrorCategory::Permission => "permission",
        ErrorCategory::Budget => "budget",
        ErrorCategory::Timeout => "timeout",
        ErrorCategory::Interrupted => "interrupted",
        ErrorCategory::Cli => "cli",
        ErrorCategory::Unknown => "unknown",
    }
}

pub(crate) fn retry_word(reason: &RetryReason) -> String {
    match reason {
        RetryReason::RateLimit => "rate limit".to_owned(),
        RetryReason::Overloaded => "overloaded".to_owned(),
        RetryReason::Network => "network".to_owned(),
        RetryReason::ServerError => "server error".to_owned(),
        RetryReason::Other(other) => clean_line(other),
    }
}

fn retry_text(reason: &RetryReason, delay_ms: Option<u64>) -> String {
    let why = retry_word(reason);
    match delay_ms {
        Some(ms) => format!("Retrying · {why} · in {}", format::seconds(ms)),
        None => format!("Retrying · {why}"),
    }
}

fn clean_report(report: &Report) -> Report {
    let list = |items: &[String]| items.iter().map(|s| clean_line(s)).collect();
    Report {
        status: report.status,
        summary: clean_line(&report.summary),
        files_changed: list(&report.files_changed),
        tests_run: list(&report.tests_run),
        concerns: list(&report.concerns),
    }
}

fn clean_card(card: &MergeCard) -> MergeCard {
    MergeCard {
        branch: clean_line(&card.branch),
        target_branch: card.target_branch.as_deref().map(clean_line),
        stat: card.stat,
        gates: card
            .gates
            .iter()
            .map(|g| GateResult {
                command: g.command.iter().map(|c| clean_line(c)).collect(),
                output: clean(&g.output),
                ..g.clone()
            })
            .collect(),
        report: card.report.as_ref().map(clean_report),
        budget: card.budget,
        stopped_by: card.stopped_by,
    }
}

fn clean_decision(decision: &MergeDecision) -> MergeDecision {
    match decision {
        MergeDecision::RequestChanges { note } => MergeDecision::RequestChanges {
            note: note.as_deref().map(clean_line),
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::{DecidedBy, TodoStatus};
    use serde_json::json;

    use super::*;
    use crate::testing::{Script, WORKTREE, solo_run_events};

    fn view_of(events: &[RunEvent]) -> RunView {
        let mut view = RunView::new(Some(PathBuf::from("/home/you")));
        for event in events {
            view.apply(event);
        }
        view
    }

    fn sessions(view: &RunView) -> Vec<&SessionView> {
        view.sessions.iter().collect()
    }

    #[test]
    fn solo_run_builds_the_expected_transcript() {
        let view = view_of(&solo_run_events());
        let run = view.run.as_ref().unwrap();
        assert_eq!(run.repo, "~/code/api");
        assert_eq!(run.agent, "claude-code");
        assert_eq!(view.limit(), BudgetLimit::Usd(3.0));

        let kinds: Vec<&str> = view
            .entries
            .iter()
            .map(|e| match e {
                Entry::Step(_) => "step",
                Entry::Session(_) => "session",
                Entry::Gates(_) => "gates",
                Entry::Merge(_) => "merge",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "step", "step", "step", "session", "step", "gates", "merge", "step"
            ]
        );

        let session = sessions(&view)[0];
        assert_eq!(session.state(), State::Done);
        assert_eq!(session.agent_label(), "claude-code/haiku");
        assert_eq!(session.report.as_ref().unwrap().status, ReportStatus::Done);
        assert!(session.stream.is_empty());
        assert_eq!(session.spend().billable_tokens(), 9_400);
        assert!(view.finished.is_some());
        assert!(!view.is_live());
    }

    #[test]
    fn streamed_text_then_whole_message_appears_once() {
        let mut s = Script::new();
        s.session_started();
        s.agent(AgentEvent::TextDelta {
            text: "Creating ".into(),
            parent: None,
        });
        s.agent(AgentEvent::TextDelta {
            text: "the file.".into(),
            parent: None,
        });
        let view = view_of(&s.events);
        assert_eq!(sessions(&view)[0].stream, "Creating the file.");

        s.agent(AgentEvent::Message {
            text: "Creating the file.".into(),
            parent: None,
        });
        let view = view_of(&s.events);
        let session = sessions(&view)[0];
        assert!(session.stream.is_empty());
        assert_eq!(session.last_message(), Some("Creating the file."));
        assert_eq!(session.items.len(), 1);
    }

    #[test]
    fn stream_flushed_by_a_tool_is_not_repeated_by_the_message() {
        let mut s = Script::new();
        s.session_started();
        s.agent(AgentEvent::TextDelta {
            text: "Looking.".into(),
            parent: None,
        });
        s.tool("t1", "Read", json!({"file_path": "a"}));
        s.agent(AgentEvent::Message {
            text: "Looking.".into(),
            parent: None,
        });
        let view = view_of(&s.events);
        let messages = sessions(&view)[0]
            .items
            .iter()
            .filter(|i| matches!(i, Item::Message { .. }))
            .count();
        assert_eq!(messages, 1);
    }

    #[test]
    fn tool_calls_track_status_output_and_diff() {
        let mut s = Script::new();
        s.session_started();
        s.tool(
            "e1",
            "Edit",
            json!({"file_path": format!("{WORKTREE}/README.md"), "old_string": "a", "new_string": "b"}),
        );
        s.finish_tool("e1", false, Some("permission denied"));
        let view = view_of(&s.events);
        let session = sessions(&view)[0];
        let Item::Tool(call) = &session.items[0] else {
            panic!("expected a tool call");
        };
        assert_eq!(call.summary.describe(), "Edit README.md  (+1 −1)");
        assert_eq!(call.status, ToolStatus::Failed);
        assert_eq!(call.output.as_deref(), Some("permission denied"));
        assert_eq!(session.last_diff, Some(0));
    }

    #[test]
    fn subagent_tools_count_under_their_parent() {
        let mut s = Script::new();
        s.session_started();
        s.tool("task", "Task", json!({"description": "explore"}));
        s.agent(AgentEvent::ToolStarted {
            id: "child".into(),
            name: "Read".into(),
            input: json!({"file_path": "x"}),
            parent: Some("task".into()),
        });
        s.agent(AgentEvent::TextDelta {
            text: "sub text".into(),
            parent: Some("task".into()),
        });
        let view = view_of(&s.events);
        let session = sessions(&view)[0];
        let Item::Tool(parent) = &session.items[0] else {
            panic!("expected the parent tool first");
        };
        assert_eq!(parent.children, 1);
        assert!(session.stream.is_empty());
    }

    #[test]
    fn agent_text_is_cleaned_on_the_way_in() {
        let mut s = Script::new();
        s.session_started();
        s.agent(AgentEvent::TextDelta {
            text: "\x1b[2J\x1b]0;title\x07hi".into(),
            parent: None,
        });
        s.agent(AgentEvent::Todo {
            items: vec![TodoItem {
                text: "\x1b[31mred".into(),
                status: TodoStatus::Pending,
            }],
        });
        let view = view_of(&s.events);
        let session = sessions(&view)[0];
        assert_eq!(session.stream, "hi");
        assert_eq!(session.todos[0].text, "red");
    }

    #[test]
    fn events_without_a_session_start_get_one() {
        let mut s = Script::new();
        s.agent(AgentEvent::Message {
            text: "hello".into(),
            parent: None,
        });
        let view = view_of(&s.events);
        let session = sessions(&view)[0];
        assert_eq!(session.agent, "claude-code");
        assert!(session.is_running());
    }

    #[test]
    fn budget_levels_add_one_step_each() {
        let mut s = Script::new();
        s.session_started();
        s.budget(BudgetAction::Continue, 0.5);
        s.budget(BudgetAction::WrapUp, 0.86);
        s.budget(BudgetAction::WrapUp, 0.9);
        s.budget(BudgetAction::Stop, 1.01);
        let view = view_of(&s.events);
        let steps: Vec<&str> = view
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Step(step) => Some(step.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            steps,
            vec!["Budget 86% used of $3.00", "Budget used up · $3.00"]
        );
    }

    #[test]
    fn stopped_and_failed_sessions() {
        let mut s = Script::new();
        s.session_started();
        s.session_ended(Some(130), Some(argonauts_core::StopCause::Budget));
        let view = view_of(&s.events);
        assert_eq!(sessions(&view)[0].state(), State::Stopped);

        let mut s = Script::new();
        s.session_started();
        s.agent(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        });
        s.session_ended(Some(0), None);
        let view = view_of(&s.events);
        assert_eq!(sessions(&view)[0].state(), State::Failed);
    }

    #[test]
    fn merge_prompt_and_checkpoint_share_one_card() {
        let mut s = Script::new();
        s.checkpoint();
        let mut view = view_of(&s.events);
        let card = match &s.events[0].kind {
            RunEventKind::Checkpoint(Checkpoint::Merge(card)) => card.clone(),
            _ => unreachable!(),
        };
        view.show_merge_prompt(&CardId::numbered(1), &card, Some("+hi\n"));
        let merges = view
            .entries
            .iter()
            .filter(|e| matches!(e, Entry::Merge(_)))
            .count();
        assert_eq!(merges, 1);
        assert!(view.pending_merge().is_some());
        assert_eq!(view.merge_diff(), Some("+hi\n"));

        view.mark_chosen(MergeDecision::Leave);
        assert!(view.pending_merge().is_none());

        let decision = RunEvent::now(
            s.events[0].run_id.clone(),
            Some(CardId::numbered(1)),
            "conductor",
            RunEventKind::Decision(DecisionRecord {
                checkpoint: argonauts_core::CheckpointKind::Merge,
                decision: MergeDecision::Leave,
                by: DecidedBy::User,
                commit: None,
            }),
        );
        view.apply(&decision);
        let Some(Entry::Merge(merge)) = view.entries.last() else {
            panic!("expected the merge card");
        };
        assert!(merge.decided.is_some());
    }

    #[test]
    fn phases_drive_the_spinner_and_failures() {
        let mut s = Script::new();
        s.phase(Phase::Worktree, PhaseStatus::Started, None);
        let view = view_of(&s.events);
        assert_eq!(view.active_phase, Some(Phase::Worktree));
        assert!(view.is_live());

        s.phase(
            Phase::Worktree,
            PhaseStatus::Failed,
            Some("git worktree add failed"),
        );
        let view = view_of(&s.events);
        assert_eq!(view.active_phase, None);
        let Some(Entry::Step(step)) = view.entries.last() else {
            panic!("expected a step");
        };
        assert_eq!(step.state, State::Failed);
        assert_eq!(step.text, "Worktree failed · git worktree add failed");
    }

    #[test]
    fn gate_results_find_their_entry_past_a_note() {
        let mut s = Script::new();
        s.phase(Phase::Gates, PhaseStatus::Started, None);
        s.gate(&["a"], true, "");
        s.push_note("slow gate");
        s.gate(&["b"], false, "boom");
        s.phase(Phase::Gates, PhaseStatus::Failed, Some("1 gate failed"));
        let view = view_of(&s.events);
        let gates: Vec<&GatesView> = view
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Gates(g) => Some(g),
                _ => None,
            })
            .collect();
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].results.len(), 2);
        assert!(!gates[0].running);
        assert!(
            matches!(view.entries.last(), Some(Entry::Step(step)) if step.state == State::Info)
        );
    }

    #[test]
    fn skipped_gates_leave_one_line() {
        let mut s = Script::new();
        s.phase(Phase::Gates, PhaseStatus::Started, None);
        s.phase(Phase::Gates, PhaseStatus::Skipped, Some("no gate commands"));
        let view = view_of(&s.events);
        assert_eq!(view.entries.len(), 1);
        let Some(Entry::Step(step)) = view.entries.first() else {
            panic!("expected a step");
        };
        assert_eq!(step.text, "Gates skipped · no gate commands");
    }

    #[test]
    fn verbosity_ranks() {
        assert!(shows(Verbosity::Full, Verbosity::Normal));
        assert!(shows(Verbosity::Normal, Verbosity::Quiet));
        assert!(!shows(Verbosity::Quiet, Verbosity::Normal));
        assert!(!shows(Verbosity::Normal, Verbosity::Full));
    }
}
