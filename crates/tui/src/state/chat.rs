use argonauts_core::app::ChatEvent;
use argonauts_core::events::context::{ContextCategory, ContextItem};
use argonauts_core::events::session::{
    SubagentKind, SubagentStatus, SubagentUpdate, VendorWorkflow,
};
use argonauts_core::names::{TurnId, WorkerRef};
use argonauts_core::screen::{Drawn, ScreenChange};
use argonauts_core::{AgentEvent, ErrorCategory, ModOutputKind};

use super::session::{Doing, ToolRecord};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatState {
    /// The turn being answered, if any.
    pub turn: Option<TurnId>,
    /// Text of the answer so far, while it streams.
    pub streamed: String,
    /// Chat messages you sent while a turn ran, oldest first, until they're sent.
    pub queued: Vec<String>,
    /// The agent answering the turn.
    pub worker: Option<WorkerRef>,
    pub tokens_out: u64,
    pub cost_usd: Option<f64>,
    pub doing: Doing,
    /// The conductor said the stream stalled, and nothing has come from the agent since.
    pub stalled: bool,
    /// A whole message closed the text so far, so more text starts a new paragraph.
    message_closed: bool,
    /// The turn's tool calls, so a result can be read with the call it answers.
    pub tools: Vec<ToolRecord>,
    /// The session's `init` listed send now, so what you send joins it.
    pub send_now: bool,
    /// What Claude Code draws above the input, kept across turns until it draws again.
    pub heads_up: Option<Drawn>,
    /// Each mod's status line, by plugin, kept until it clears it.
    pub mod_status: Vec<(String, String)>,
    /// The workflows the turn's CLI runs itself, as it reports them.
    pub workflows: Vec<VendorWorkflow>,
    /// The conversation's context by category, as its CLI last counted it. Kept across
    /// turns, since between turns there's no session to ask.
    pub context: Option<ContextBreakdown>,
    /// The subagents the turn's CLI started, the latest update of each, in first-seen order.
    /// The list under the input shows them under `main`.
    pub subagents: Vec<SubagentUpdate>,
    /// How many tasks the turn's CLI runs in the background now: its own subagents and
    /// workflows, and commands it left running.
    pub background: usize,
    /// The main agent's turn has its result, by itself or because Esc stopped it, and it has
    /// started nothing since.
    main_idle: bool,
}

/// A session's context by category, as its CLI counts it, with what fills the categories
/// that list their items.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextBreakdown {
    /// In the CLI's order, each part once.
    pub categories: Vec<ContextCategory>,
    /// Where the CLI compacts on its own, when it does.
    pub auto_compact_at: Option<u64>,
    pub window: u64,
    pub memory_files: Vec<ContextItem>,
    pub mcp_tools: Vec<ContextItem>,
    pub agents: Vec<ContextItem>,
    pub skills: Vec<ContextItem>,
}

impl ChatState {
    pub(super) fn apply(&mut self, event: &ChatEvent) {
        match event {
            ChatEvent::TurnStarted {
                turn, text, worker, ..
            } => {
                *self = Self {
                    turn: Some(turn.clone()),
                    worker: Some(worker.clone()),
                    queued: std::mem::take(&mut self.queued),
                    heads_up: self.heads_up.take(),
                    mod_status: std::mem::take(&mut self.mod_status),
                    // Another model's session holds another context.
                    context: self
                        .context
                        .take()
                        .filter(|_| self.worker.as_ref() == Some(worker)),
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
            ChatEvent::Agent { turn, event } if self.turn.as_ref() == Some(turn) => {
                self.apply_agent(event);
            }
            ChatEvent::Agent { .. } => {}
            ChatEvent::TurnFinished { turn, spend, .. } if self.turn.as_ref() == Some(turn) => {
                if spend.cost_usd > 0.0 {
                    self.cost_usd = Some(spend.cost_usd);
                }
                self.end_turn();
            }
            ChatEvent::TurnFailed { turn, .. } | ChatEvent::TurnCancelled { turn }
                if self.turn.as_ref() == Some(turn) =>
            {
                self.end_turn();
            }
            ChatEvent::HandedOff { turn, to, .. } if self.turn.as_ref() == Some(turn) => {
                self.worker = Some(to.clone());
                self.context = None;
                self.stalled = false;
            }
            // It joined the turn, so it no longer waits.
            ChatEvent::Joined { text, .. } => {
                if let Some(at) = self
                    .queued
                    .iter()
                    .position(|queued| queued.trim() == text.trim())
                {
                    self.queued.remove(at);
                }
            }
            ChatEvent::TurnFinished { .. }
            | ChatEvent::TurnFailed { .. }
            | ChatEvent::TurnCancelled { .. }
            | ChatEvent::HandedOff { .. }
            | ChatEvent::AnswerRetried { .. } => {}
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
            }
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                if let Some(tool) = self.tools.iter_mut().rev().find(|tool| tool.id == *id) {
                    tool.ok = Some(*ok);
                    tool.output.clone_from(output);
                }
                self.doing = Doing::Nothing;
            }
            AgentEvent::Result { .. } => self.doing = Doing::Nothing,
            AgentEvent::Init { capabilities, .. } => {
                self.send_now = capabilities.iter().any(|c| c == "interrupt_send_now_v1");
            }
            AgentEvent::ContextBreakdown {
                categories,
                auto_compact_at,
                window,
                memory_files,
                mcp_tools,
                agents,
                skills,
                ..
            } => {
                self.context = Some(ContextBreakdown {
                    categories: categories.clone(),
                    auto_compact_at: *auto_compact_at,
                    window: *window,
                    memory_files: memory_files.clone(),
                    mcp_tools: mcp_tools.clone(),
                    agents: agents.clone(),
                    skills: skills.clone(),
                });
            }
            AgentEvent::ScreenChanged {
                change: ScreenChange::Drawn { drawn },
            } => self.heads_up = (!drawn.is_empty()).then(|| drawn.clone()),
            AgentEvent::ScreenChanged {
                change: ScreenChange::Unreadable { .. },
            } => self.heads_up = None,
            AgentEvent::ModOutput {
                kind: ModOutputKind::Status,
                plugin,
                text,
                ..
            } => {
                let plugin = plugin.clone().unwrap_or_default();
                self.mod_status.retain(|(known, _)| *known != plugin);
                if let Some(text) = text.as_ref().filter(|text| !text.trim().is_empty()) {
                    self.mod_status.push((plugin, text.clone()));
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
    }
}
