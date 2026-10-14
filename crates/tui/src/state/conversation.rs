use argonauts_core::app::ConversationEvent;
use argonauts_core::events::session::{
    CliWorkflowRun, SubagentKind, SubagentStatus, SubagentUpdate,
};
use argonauts_core::names::{ModelRef, TurnId};
use argonauts_core::screen::{Drawn, ScreenChange};
use argonauts_core::{AgentEvent, ErrorCategory, PluginOutputKind, TodoItem};

use super::context::{ContextCount, ContextGrowth};
use super::session::{Doing, ToolRecord};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConversationState {
    /// The turn being answered, if any.
    pub turn: Option<TurnId>,
    /// Text of the answer so far, while it streams.
    pub streamed: String,
    /// Conversation messages you sent while a turn ran, oldest first, until they're sent.
    pub queued: Vec<String>,
    /// The agent answering the turn.
    pub worker: Option<ModelRef>,
    pub tokens_out: u64,
    pub cost_usd: Option<f64>,
    pub doing: Doing,
    /// Argonauts Code said the stream stalled, and nothing has come from the agent since.
    pub stalled: bool,
    /// How many events the turn's agent has sent, so the working line can tell when it was
    /// last heard from.
    pub heard: u64,
    /// A whole message closed the text so far, so more text starts a new paragraph.
    message_closed: bool,
    /// The turn's tool calls, so a result can be read with the call it answers.
    pub tools: Vec<ToolRecord>,
    /// The CLI's own task list, as it last wrote it. Kept across turns, as the CLI keeps it,
    /// so the working line can name the task in hand from the turn's first moment.
    pub todos: Vec<TodoItem>,
    /// The session's `init` listed send now, so what you send joins it.
    pub send_now: bool,
    /// What Claude Code draws above the input, kept across turns until it draws again.
    pub notice: Option<Drawn>,
    /// Each plugin's status line, kept until the plugin clears it.
    pub plugin_status: Vec<(String, String)>,
    /// The workflows the turn's CLI runs itself, as it reports them.
    pub workflows: Vec<CliWorkflowRun>,
    /// The conversation's context as its CLI last counted it. Kept across turns, since
    /// between turns there's no session to ask.
    pub context: Option<ContextCount>,
    /// What the conversation's turns add to its context, kept across turns as the count is.
    pub growth: ContextGrowth,
    /// The subagents the turn's CLI started, the latest update of each, in first-seen order.
    /// The list under the input shows them under `main`.
    pub subagents: Vec<SubagentUpdate>,
    /// How many tasks the turn's CLI runs in the background now: its own subagents and
    /// workflows, and commands it left running.
    pub background: usize,
    /// The main agent's turn has its result, by itself or because Esc stopped it, and it has
    /// started nothing since.
    main_idle: bool,
    /// Calls the turn's agent is still writing, by id, each with its tool's name: all that is
    /// known of a call until it's whole.
    pub being_written: Vec<(String, String)>,
}

impl ConversationState {
    pub(super) fn apply(&mut self, event: &ConversationEvent) {
        match event {
            ConversationEvent::TurnStarted {
                turn, text, worker, ..
            } => {
                *self = Self {
                    turn: Some(turn.clone()),
                    worker: Some(worker.clone()),
                    queued: std::mem::take(&mut self.queued),
                    notice: self.notice.take(),
                    plugin_status: std::mem::take(&mut self.plugin_status),
                    // Another model's session holds another task list and another context.
                    todos: std::mem::take(&mut self.todos)
                        .into_iter()
                        .filter(|_| self.worker.as_ref() == Some(worker))
                        .collect(),
                    context: self
                        .context
                        .take()
                        .filter(|_| self.worker.as_ref() == Some(worker)),
                    growth: if self.worker.as_ref() == Some(worker) {
                        std::mem::take(&mut self.growth)
                    } else {
                        ContextGrowth::default()
                    },
                    ..Self::default()
                };
                // The bridge queued it while the last turn ran; now it's this turn's prompt.
                if let Some(at) = self
                    .queued
                    .iter()
                    .position(|queued| queued.trim() == text.trim())
                {
                    self.queued.remove(at);
                }
            }
            ConversationEvent::Agent { turn, event } if self.turn.as_ref() == Some(turn) => {
                self.apply_agent(event);
            }
            ConversationEvent::Agent { .. } => {}
            ConversationEvent::TurnFinished { turn, spend, .. }
                if self.turn.as_ref() == Some(turn) =>
            {
                if spend.cost_usd > 0.0 {
                    self.cost_usd = Some(spend.cost_usd);
                }
                self.end_turn();
            }
            ConversationEvent::TurnFailed { turn, .. }
            | ConversationEvent::TurnCancelled { turn }
                if self.turn.as_ref() == Some(turn) =>
            {
                self.end_turn();
            }
            ConversationEvent::FellBack { turn, to, .. } if self.turn.as_ref() == Some(turn) => {
                self.worker = Some(to.clone());
                self.context = None;
                self.growth = ContextGrowth::default();
                self.todos.clear();
                self.stalled = false;
            }
            // It joined the turn, so it no longer waits.
            ConversationEvent::Joined { text, .. } => {
                if let Some(at) = self
                    .queued
                    .iter()
                    .position(|queued| queued.trim() == text.trim())
                {
                    self.queued.remove(at);
                }
            }
            ConversationEvent::TurnFinished { .. }
            | ConversationEvent::TurnFailed { .. }
            | ConversationEvent::TurnCancelled { .. }
            | ConversationEvent::FellBack { .. }
            | ConversationEvent::AnswerRetried { .. } => {}
        }
    }

    /// The main agent rests and the turn goes on only for the subagents and workflows its CLI
    /// runs in the background. Nothing is left for Esc to stop then, and the list's rows show
    /// the work, as on Claude Code's screen. A command left running is no
    /// agent and has no row, so a turn that waits only on one stays Esc's to stop.
    pub fn waits_on_background(&self) -> bool {
        let ended = |status| matches!(status, SubagentStatus::Done | SubagentStatus::Failed);
        let agents = self.subagents.iter().any(|agent| !ended(agent.status))
            || self.workflows.iter().any(|workflow| !workflow.ended());
        self.turn.is_some() && self.main_idle && self.background > 0 && agents
    }

    fn apply_agent(&mut self, event: &AgentEvent) {
        self.heard = self.heard.wrapping_add(1);
        match event {
            AgentEvent::BackgroundTasks { running, .. } => self.background = running.len(),
            AgentEvent::Result { .. } => self.main_idle = true,
            // Each turn of the CLI's opens with its init, and then the main agent speaks or
            // calls a tool.
            AgentEvent::Init { .. }
            | AgentEvent::TextDelta { parent: None, .. }
            | AgentEvent::ToolStarted { parent: None, .. } => self.main_idle = false,
            _ => {}
        }
        self.stalled = matches!(
            event,
            AgentEvent::Error {
                category: ErrorCategory::Stalled,
                ..
            }
        );
        match event {
            AgentEvent::TextDelta { text, parent: None } => {
                self.new_paragraph_if_closed();
                self.streamed.push_str(text);
                self.doing = Doing::Writing;
            }
            AgentEvent::Message { text, parent: None } => {
                if !self.streamed.ends_with(text.as_str()) {
                    self.new_paragraph_if_closed();
                    self.streamed.push_str(text);
                }
                self.message_closed = true;
                self.doing = Doing::Nothing;
            }
            AgentEvent::Reasoning { .. } => self.doing = Doing::Thinking,
            AgentEvent::Todo { items } => self.todos.clone_from(items),
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => {
                self.tools.push(ToolRecord {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    parent: parent.clone(),
                    ok: None,
                    output: None,
                });
                self.doing = Doing::RunningTool;
                self.being_written.retain(|(written, _)| written != id);
            }
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                if let Some(tool) = self.tools.iter_mut().rev().find(|tool| tool.id == *id) {
                    tool.ok = Some(*ok);
                    tool.output.clone_from(output);
                }
                self.doing = Doing::Nothing;
            }
            AgentEvent::ToolPending { id, name, .. }
                if !self.being_written.iter().any(|(written, _)| written == id) =>
            {
                self.being_written.push((id.clone(), name.clone()));
            }
            AgentEvent::ToolDropped { id, .. } => {
                self.being_written.retain(|(written, _)| written != id);
            }
            AgentEvent::Result { .. } => self.doing = Doing::Nothing,
            AgentEvent::Init { capabilities, .. } => {
                self.send_now = capabilities.iter().any(|c| c == "interrupt_send_now_v1");
            }
            AgentEvent::ContextBreakdown { usage } => {
                self.context = Some(ContextCount {
                    usage: (**usage).clone(),
                    counted: crate::bottom::wall_now(),
                });
            }
            AgentEvent::ContextFill { used, .. } => self.growth.filled(*used),
            AgentEvent::ScreenChanged {
                change: ScreenChange::Drawn { drawn },
            } => self.notice = (!drawn.is_empty()).then(|| drawn.clone()),
            AgentEvent::ScreenChanged {
                change: ScreenChange::Unreadable { .. },
            } => self.notice = None,
            AgentEvent::PluginOutput {
                kind: PluginOutputKind::Status,
                plugin,
                text,
                ..
            } => {
                let plugin = plugin.clone().unwrap_or_default();
                self.plugin_status.retain(|(known, _)| *known != plugin);
                if let Some(text) = text.as_ref().filter(|text| !text.trim().is_empty()) {
                    self.plugin_status.push((plugin, text.clone()));
                }
            }
            // A workflow has a row of its own.
            AgentEvent::Subagent { update } if update.kind != SubagentKind::Workflow => {
                match self
                    .subagents
                    .iter_mut()
                    .find(|known| known.id == update.id)
                {
                    Some(known) => *known = update.clone(),
                    None => self.subagents.push(update.clone()),
                }
            }
            AgentEvent::Workflow { workflow } => {
                match self
                    .workflows
                    .iter_mut()
                    .find(|known| known.id == workflow.id)
                {
                    Some(known) => {
                        let started = known.started_at_ms;
                        *known = (**workflow).clone();
                        known.started_at_ms = known.started_at_ms.or(started);
                    }
                    None => self.workflows.push((**workflow).clone()),
                }
            }
            AgentEvent::Usage {
                output_tokens,
                cost_usd,
                cumulative,
                ..
            } => {
                if *cumulative {
                    self.tokens_out = *output_tokens;
                    if cost_usd.is_some() {
                        self.cost_usd = *cost_usd;
                    }
                } else {
                    self.tokens_out += output_tokens;
                    if let Some(cost) = cost_usd {
                        self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
                    }
                }
            }
            _ => {}
        }
    }

    fn new_paragraph_if_closed(&mut self) {
        if std::mem::take(&mut self.message_closed) && !self.streamed.is_empty() {
            self.streamed.push_str("\n\n");
        }
    }

    fn end_turn(&mut self) {
        self.growth.turn_ended();
        self.turn = None;
        self.send_now = false;
        self.workflows.clear();
        self.subagents.clear();
        self.streamed.clear();
        self.doing = Doing::Nothing;
        self.stalled = false;
        self.message_closed = false;
        self.background = 0;
        self.main_idle = false;
        self.being_written.clear();
    }
}
