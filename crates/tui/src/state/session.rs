use argonauts_core::AgentEvent;
use argonauts_core::events::session::{
    EndReason, FileDiff, QueuedMessage, SessionEnd, SessionInput, SessionStarted, SubagentUpdate,
    VendorWorkflow,
};
use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::sessions::SessionSettings;
use argonauts_core::steps::StepName;
use argonauts_core::views::ContextUse;
use argonauts_core::workflow::StepRef;
use argonauts_core::{CardId, ErrorCategory, RetryReason, TodoItem};
use time::OffsetDateTime;

/// One agent session of a run, everything the live block, the panel, the crew view, and the
/// session view show about it.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub key: SessionKey,
    /// 1 to 9, the key that opens its session view.
    pub follow: Option<u8>,
    pub worker: WorkerRef,
    /// Its model is its CLI's default as the catalog has it, which the model it reports replaces.
    pub model_assumed: bool,
    /// Adapter or endpoint id, such as `claude-code` or `ollama`.
    pub vendor: String,
    pub role: RoleName,
    /// The step it runs, which tells an owner's forecast for its card from building it.
    pub step: Option<StepName>,
    pub card: Option<CardId>,
    pub workflow_step: Option<StepRef>,
    /// The run's name for it, such as `fix:slugify`. A log from before labels has none.
    pub label: Option<String>,
    pub status: SessionStatus,
    pub started: OffsetDateTime,
    pub ended: Option<OffsetDateTime>,
    /// When the last event from it arrived, for the stall hint.
    pub last_event: OffsetDateTime,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: Option<f64>,
    pub context: Option<ContextUse>,
    /// The model's whole window when the stream said, which can be larger than the cap.
    pub context_window: Option<u64>,
    /// Fresh sessions it has rotated to (`Card::Attempt` with cause rotation).
    pub rotations: u32,
    pub todos: Vec<TodoItem>,
    /// Every tool call, oldest first.
    pub tools: Vec<ToolRecord>,
    /// Whole messages, oldest first. Codex's arrive whole; Claude Code's are joined deltas.
    pub messages: Vec<String>,
    /// The text streaming now, since the last whole message.
    pub streaming: String,
    pub diffs: Vec<FileDiff>,
    /// Each patch an edit reported: the edit's call id, the file as the CLI names it, and its
    /// hunks with the file's line numbers.
    pub patches: Vec<(String, String, String)>,
    /// The latest update per child, in first-seen order.
    pub subagents: Vec<SubagentUpdate>,
    /// Workflows the agent's own CLI runs, the latest state of each, in first-seen order.
    pub workflows: Vec<VendorWorkflow>,
    /// What Argonauts sent it (`Session::Input`), for the session view's input side.
    pub inputs: Vec<SessionInput>,
    pub queued: Vec<QueuedMessage>,
    pub retry: Option<Retrying>,
    pub standing_in_for: Option<WorkerRef>,
    pub end: Option<Box<SessionEnd>>,
    /// Vendor lines no parser understood, drawn as `? unknown event`.
    pub unknown: Vec<serde_json::Value>,
    /// What its last event says it's doing, for the working line's words.
    pub doing: Doing,
    /// The conductor said its stream stalled, and nothing has come from it since.
    pub stalled: bool,
    /// The vendor's own session id, for resuming.
    pub vendor_session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionStatus {
    #[default]
    Starting,
    Working,
    /// Waiting on other work, such as a card it depends on.
    Waiting,
    /// Waiting on a timer, such as a usage reset.
    Sleeping,
    Done,
    Failed,
    Stopped,
}

/// What an agent is busy with, as far as its stream says. The screen times each spell itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Doing {
    #[default]
    Nothing,
    Thinking,
    RunningTool,
    /// Answer text is streaming.
    Writing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Retrying {
    pub reason: RetryReason,
    pub delay_ms: Option<u64>,
    pub attempt: u32,
    pub since: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRecord {
    pub id: String,
    pub name: String,
    /// `Null` while the model is still writing the call.
    pub input: serde_json::Value,
    /// A subagent's id when the call is inside one.
    pub parent: Option<String>,
    /// `None` while it runs.
    pub ok: Option<bool>,
    pub output: Option<String>,
}

impl ToolRecord {
    /// The model is still writing this call, so only its name is known.
    pub fn is_pending(&self) -> bool {
        self.input.is_null() && self.ok.is_none()
    }
}

/// What a session does, as the live block, the started tree and the panel name it:
/// `owner · card-002`. An owner forecasting its card runs before you approve the plan, so it's
/// an `estimator` there; `owner` would read as if building had begun.
pub fn task_label(role: RoleName, step: Option<StepName>, card: Option<&CardId>) -> String {
    let role = match (role, step) {
        (RoleName::Worker, Some(StepName::Predict)) => "estimator",
        _ => role.as_str(),
    };
    match card {
        Some(card) => format!("{role} · {card}"),
        None => role.to_owned(),
    }
}

impl SessionState {
    pub fn new(
        key: SessionKey,
        worker: WorkerRef,
        vendor: impl Into<String>,
        role: RoleName,
        started: OffsetDateTime,
    ) -> Self {
        Self {
            key,
            follow: None,
            worker,
            model_assumed: false,
            vendor: vendor.into(),
            role,
            step: None,
            card: None,
            workflow_step: None,
            label: None,
            status: SessionStatus::Starting,
            started,
            ended: None,
            last_event: started,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            context: None,
            context_window: None,
            rotations: 0,
            todos: Vec::new(),
            tools: Vec::new(),
            messages: Vec::new(),
            streaming: String::new(),
            diffs: Vec::new(),
            patches: Vec::new(),
            subagents: Vec::new(),
            workflows: Vec::new(),
            inputs: Vec::new(),
            queued: Vec::new(),
            retry: None,
            standing_in_for: None,
            end: None,
            unknown: Vec::new(),
            doing: Doing::Nothing,
            stalled: false,
            vendor_session: None,
        }
    }

    pub(super) fn started(
        key: SessionKey,
        started: &SessionStarted,
        card: Option<CardId>,
        at: OffsetDateTime,
    ) -> Self {
        let mut session = Self::new(
            key,
            started.worker.clone(),
            started.vendor.0.clone(),
            started.role,
            at,
        );
        session.follow = started.follow_key;
        session.step = started.step;
        session.card = card;
        session.workflow_step = started.workflow_step.clone();
        session.label = started
            .label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned);
        session.vendor_session = started.resumed_from.clone();
        session
    }

    /// Its label, such as `fix:slugify`, as its rows name what it does, or for a log from
    /// before labels, `owner · card-002`.
    pub fn task(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| task_label(self.role, self.step, self.card.as_ref()))
    }

    pub fn tokens(&self) -> u64 {
        self.tokens_in + self.tokens_out
    }

    /// Whether it's the session of `worker`, which an event may name without the model the
    /// session filled in from its settings or its start.
    pub fn is_worker(&self, worker: &WorkerRef) -> bool {
        self.worker.worker == worker.worker
            && (worker.model.is_none() || self.worker.model == worker.model)
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.status,
            SessionStatus::Starting
                | SessionStatus::Working
                | SessionStatus::Waiting
                | SessionStatus::Sleeping
        )
    }

    /// Folds one event from its stream in.
    pub(super) fn apply_agent(&mut self, event: &AgentEvent, at: OffsetDateTime) {
        self.last_event = at;
        self.stalled = matches!(
            event,
            AgentEvent::Error {
                category: ErrorCategory::Stalled,
                ..
            }
        );
        if self.status == SessionStatus::Starting {
            self.status = SessionStatus::Working;
        }
        if !matches!(
            event,
            AgentEvent::Retry { .. }
                | AgentEvent::Usage { .. }
                | AgentEvent::ContextFill { .. }
                | AgentEvent::ContextBreakdown { .. }
                | AgentEvent::Plan { .. }
                | AgentEvent::QuotaSignal { .. }
                | AgentEvent::Quota { .. }
                | AgentEvent::ModOutput { .. }
                | AgentEvent::ModelsReported { .. }
                | AgentEvent::IsolationLeak { .. }
                | AgentEvent::CommandsChanged { .. }
                | AgentEvent::MessageState { .. }
                | AgentEvent::ScreenChanged { .. }
                | AgentEvent::BackgroundTasks { .. }
        ) {
            self.retry = None;
        }
        match event {
            AgentEvent::Init {
                session_id, model, ..
            } => {
                if session_id.is_some() {
                    self.vendor_session.clone_from(session_id);
                }
                if self.worker.model.is_none() || (self.model_assumed && model.is_some()) {
                    self.worker.model.clone_from(model);
                    self.model_assumed = false;
                }
            }
            AgentEvent::TextDelta { text, parent: None } => {
                self.streaming.push_str(text);
                self.doing = Doing::Writing;
            }
            AgentEvent::Message { text, parent: None } => {
                self.streaming.clear();
                self.messages.push(text.clone());
                self.doing = Doing::Nothing;
            }
            AgentEvent::TextDelta { .. } | AgentEvent::Message { .. } => {}
            AgentEvent::Reasoning { .. } => self.doing = Doing::Thinking,
            AgentEvent::ToolPending { id, name, parent } => {
                self.finish_streaming();
                if !self.tools.iter().any(|tool| tool.id == *id) {
                    self.tools.push(ToolRecord {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::Value::Null,
                        parent: parent.clone(),
                        ok: None,
                        output: None,
                    });
                }
            }
            AgentEvent::ToolDropped { id, .. } => {
                self.tools
                    .retain(|tool| !(tool.id == *id && tool.is_pending()));
            }
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => {
                self.finish_streaming();
                // A call shows once by its id, however many copies of it the stream carries.
                // One the model was still writing is filled in where it stands.
                let known = !id.is_empty() && self.tools.iter().any(|tool| tool.id == *id);
                if let Some(written) = self
                    .tools
                    .iter_mut()
                    .find(|tool| tool.id == *id && tool.is_pending())
                {
                    written.name.clone_from(name);
                    written.input = input.clone();
                } else if !known {
                    self.tools.push(ToolRecord {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                        parent: parent.clone(),
                        ok: None,
                        output: None,
                    });
                }
                self.doing = Doing::RunningTool;
            }
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                if let Some(tool) = self.tools.iter_mut().rev().find(|tool| tool.id == *id) {
                    tool.ok = Some(*ok);
                    tool.output.clone_from(output);
                }
                let still_running = self.tools.iter().any(|tool| tool.ok.is_none());
                if !still_running {
                    self.doing = Doing::Nothing;
                }
            }
            AgentEvent::FileChanged { .. } => {}
            AgentEvent::FilePatch {
                id, path, hunks, ..
            } => self.patches.push((id.clone(), path.clone(), hunks.clone())),
            AgentEvent::Todo { items } => self.todos.clone_from(items),
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cost_usd,
                cumulative,
                ..
            } => {
                if *cumulative {
                    self.tokens_in = *input_tokens;
                    self.tokens_out = *output_tokens;
                    if cost_usd.is_some() {
                        self.cost_usd = *cost_usd;
                    }
                } else {
                    self.tokens_in += input_tokens;
                    self.tokens_out += output_tokens;
                    if let Some(cost) = cost_usd {
                        self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
                    }
                }
            }
            AgentEvent::ContextFill { used, size, .. } => {
                if size.is_some() {
                    self.context_window = *size;
                }
                self.context = Some(ContextUse {
                    used: *used,
                    cap: context_cap(self.context_window),
                });
            }
            AgentEvent::Retry { reason, delay_ms } => {
                let attempt = self.retry.as_ref().map_or(1, |retry| retry.attempt + 1);
                self.retry = Some(Retrying {
                    reason: reason.clone(),
                    delay_ms: *delay_ms,
                    attempt,
                    since: at,
                });
            }
            AgentEvent::QuotaSignal { .. }
            | AgentEvent::ContextBreakdown { .. }
            | AgentEvent::Plan { .. }
            | AgentEvent::Quota { .. }
            | AgentEvent::ModOutput { .. }
            | AgentEvent::ModelsReported { .. }
            | AgentEvent::IsolationLeak { .. }
            | AgentEvent::CommandsChanged { .. }
            | AgentEvent::MessageState { .. }
            | AgentEvent::ScreenChanged { .. }
            | AgentEvent::BackgroundTasks { .. } => {}
            AgentEvent::Subagent { update } => {
                match self.subagents.iter_mut().find(|sub| sub.id == update.id) {
                    Some(known) => *known = update.clone(),
                    None => self.subagents.push(update.clone()),
                }
            }
            AgentEvent::Workflow { workflow } => {
                let mut workflow = (**workflow).clone();
                // Its time counts from when it was first seen, on this clock.
                let seen =
                    u64::try_from((at - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok();
                match self
                    .workflows
                    .iter_mut()
                    .find(|known| known.id == workflow.id)
                {
                    Some(known) => {
                        workflow.started_at_ms = known.started_at_ms.or(seen);
                        *known = workflow;
                    }
                    None => {
                        workflow.started_at_ms = seen;
                        self.workflows.push(workflow);
                    }
                }
            }
            AgentEvent::Error { .. } => {}
            AgentEvent::Result { .. } => {
                self.finish_streaming();
                self.doing = Doing::Nothing;
            }
            AgentEvent::Unknown { raw } => self.unknown.push(raw.clone()),
        }
    }

    pub(super) fn apply_diff(&mut self, diff: &FileDiff) {
        match self.diffs.iter_mut().find(|known| known.path == diff.path) {
            Some(known) => *known = diff.clone(),
            None => self.diffs.push(diff.clone()),
        }
    }

    pub(super) fn end(&mut self, end: &SessionEnd, at: OffsetDateTime) {
        self.finish_streaming();
        self.status = match end.reason {
            EndReason::Completed => SessionStatus::Done,
            EndReason::Paused | EndReason::Stopped | EndReason::BudgetHit { .. } => {
                SessionStatus::Stopped
            }
            _ => SessionStatus::Failed,
        };
        self.ended = Some(at);
        self.last_event = at;
        self.retry = None;
        self.doing = Doing::Nothing;
        self.stalled = false;
        // A workflow its CLI ran ends with it, whether or not the stream said so.
        for workflow in &mut self.workflows {
            workflow.end_with_session("its session ended before the workflow did");
        }
        // The metered count is the session's own. The vendor's totals for a session that
        // resumed another's include what the earlier one spent, which its own row shows.
        let spend = &end.spend;
        if spend.cost_usd > 0.0 {
            self.cost_usd = Some(spend.cost_usd);
        }
        if spend.input_tokens + spend.output_tokens > 0 {
            self.tokens_in = spend.input_tokens;
            self.tokens_out = spend.output_tokens;
        }
        self.end = Some(Box::new(end.clone()));
    }

    /// Text that stops streaming without a whole message still counts as said.
    fn finish_streaming(&mut self) {
        if !self.streaming.trim().is_empty() {
            self.messages.push(std::mem::take(&mut self.streaming));
        }
        self.streaming.clear();
    }
}

/// Where a session rotates: `sessions.rotate_at`, near the end of its window by default, or
/// the window when that's smaller. With no window reported yet the bar is drawn against
/// 200k tokens. `ViewData::Context` brings the configured caps when `/context` opens.
fn context_cap(window: Option<u64>) -> u64 {
    const FALLBACK: u64 = 200_000;
    let cap = SessionSettings::default()
        .rotate_at
        .tokens(window)
        .ok()
        .flatten()
        .unwrap_or(FALLBACK);
    window.map_or(cap, |window| cap.min(window))
}

#[cfg(test)]
mod writing_tests {
    use super::*;

    fn session() -> SessionState {
        SessionState::new(
            SessionKey::numbered(1),
            WorkerRef::new("claude-code", Some("haiku".into())),
            "claude-code",
            RoleName::Worker,
            OffsetDateTime::UNIX_EPOCH,
        )
    }

    /// An agent inside a run lists a call as its model starts writing it, fills it in where
    /// it stands, and forgets one that never arrived.
    #[test]
    fn a_call_being_written_is_listed_filled_in_or_forgotten() {
        let mut session = session();
        let at = OffsetDateTime::UNIX_EPOCH;
        let pending = |id: &str, name: &str| AgentEvent::ToolPending {
            id: id.into(),
            name: name.into(),
            parent: None,
        };
        session.apply_agent(&pending("e1", "Edit"), at);
        session.apply_agent(&pending("w1", "Write"), at);
        assert!(session.tools.iter().all(ToolRecord::is_pending));
        session.apply_agent(
            &AgentEvent::ToolStarted {
                id: "e1".into(),
                name: "Edit".into(),
                input: serde_json::json!({"file_path": "a.py"}),
                parent: None,
            },
            at,
        );
        session.apply_agent(
            &AgentEvent::ToolDropped {
                id: "w1".into(),
                parent: None,
            },
            at,
        );
        assert_eq!(session.tools.len(), 1);
        let edit = &session.tools[0];
        assert_eq!(edit.id, "e1");
        assert!(!edit.is_pending());
        assert_eq!(edit.input["file_path"], "a.py");
    }
}
