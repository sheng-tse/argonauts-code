use argonauts_core::app::ChatEvent;
use argonauts_core::events::session::VendorWorkflow;
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
            | ChatEvent::HandedOff { .. } => {}
        }
    }

    fn apply_agent(&mut self, event: &AgentEvent) {
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
        self.streamed.clear();
        self.doing = Doing::Nothing;
        self.stalled = false;
        self.message_closed = false;
    }
}
