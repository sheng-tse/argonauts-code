//! What the events say about each session, gathered once for the views that draw sessions.

use std::collections::BTreeMap;

use argonauts_core::events::checkpoint::{CheckpointEvent, CheckpointPacket};
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::{
    EndReason, SessionEnd, SessionEvent, SessionStarted, SubagentUpdate,
};
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{CheckpointId, RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::views::{AgentRow, ContextUse, RowStatus};
use argonauts_core::{AgentEvent, CardId, Spend};
use time::OffsetDateTime;

pub(crate) fn add_spend(total: &mut Spend, more: &Spend) {
    total.input_tokens += more.input_tokens;
    total.output_tokens += more.output_tokens;
    total.cached_tokens += more.cached_tokens;
    total.cost_usd += more.cost_usd;
    total.cost_partial |= more.cost_partial;
    total.cost_estimated |= more.cost_estimated;
}

pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub ok: Option<bool>,
    /// Set for a subagent's call.
    pub parent: Option<String>,
}

pub(crate) struct SessionFacts {
    pub key: SessionKey,
    pub card: Option<CardId>,
    pub started_at: OffsetDateTime,
    pub started: SessionStarted,
    /// The worker with the model the session runs: its own, else its CLI's from the run's
    /// settings, else the one it reported at its start. The screen names it by this.
    pub worker: WorkerRef,
    pub ended: Option<SessionEnd>,
    /// From `Usage` events, until `Ended` gives the final spend.
    pub usage: Spend,
    pub tools: Vec<ToolCall>,
    /// Used tokens and the window's size, from the latest `ContextFill`.
    pub context: Option<(u64, Option<u64>)>,
    /// The latest update of each child, in the order they first appeared.
    pub subagents: Vec<SubagentUpdate>,
    /// Every input part, in order: the prompt, then what each delivery added.
    pub prompt: String,
    /// The worker it replaced after a handoff.
    pub standing_in_for: Option<WorkerRef>,
}

impl SessionFacts {
    pub fn spent(&self) -> Spend {
        self.ended.as_ref().map_or(self.usage, |end| end.spend)
    }

    pub fn tool_calls(&self) -> u32 {
        let counted = u32::try_from(self.tools.len()).unwrap_or(u32::MAX);
        let reported = self
            .ended
            .as_ref()
            .map_or(0, |end| u32::try_from(end.tool_calls).unwrap_or(u32::MAX));
        counted.max(reported)
    }

    pub fn label(&self) -> String {
        match &self.card {
            Some(card) => format!("{} · {card}", self.started.role.as_str()),
            None => self.started.role.as_str().to_owned(),
        }
    }
}

/// Everything the session views need, read from the events in one pass.
pub(crate) struct RunFacts {
    pub sessions: Vec<SessionFacts>,
    /// Open checkpoints that wait for you, with their card.
    pub waiting: BTreeMap<CheckpointId, Option<CardId>>,
    /// The latest event's time: "now" as far as the log knows.
    pub last_ts: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    rotate_at: RotateAt,
}

impl RunFacts {
    pub fn of(events: &[RunEvent]) -> Self {
        let mut sessions: Vec<SessionFacts> = Vec::new();
        let mut index: BTreeMap<SessionKey, usize> = BTreeMap::new();
        let mut waiting = BTreeMap::new();
        // The worker a card's handoff stopped, and the role it worked in.
        let mut handoffs: BTreeMap<CardId, (WorkerRef, RoleName)> = BTreeMap::new();
        let mut rotate_at = Settings::default().sessions.rotate_at;
        let mut finished_at = None;
        // Each CLI's model from the run's settings (`agents.<cli>.model`).
        let mut configured: BTreeMap<String, String> = BTreeMap::new();
        for event in events {
            match &event.kind {
                RunEventKind::Run(RunLifecycle::Started(started)) => {
                    if let Some(value) = started.settings.pointer("/sessions/rotate_at")
                        && let Ok(parsed) = serde_json::from_value::<RotateAt>(value.clone())
                    {
                        rotate_at = parsed;
                    }
                    if let Some(agents) = started.settings.get("agents").and_then(|a| a.as_object())
                    {
                        configured = agents
                            .iter()
                            .filter_map(|(id, agent)| {
                                let model = agent.get("model")?.as_str()?.trim();
                                (!model.is_empty()).then(|| (id.clone(), model.to_owned()))
                            })
                            .collect();
                    }
                }
                RunEventKind::Run(RunLifecycle::Finished { .. }) => finished_at = Some(event.ts),
                RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                    if opened.needs_you {
                        let card = opened.card.clone().or_else(|| match &opened.packet {
                            CheckpointPacket::Question(question) => Some(question.card.clone()),
                            _ => None,
                        });
                        waiting.insert(opened.id.clone(), card);
                    }
                }
                RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                    waiting.remove(&decided.id);
                }
                RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => {
                    waiting.remove(id);
                }
                RunEventKind::Handoff(HandoffEvent::Needed(plan)) => {
                    let stopped_role = sessions.iter().rev().find_map(|session| {
                        (session.card.as_ref() == Some(&plan.card)
                            && session.started.worker == plan.stopped)
                            .then_some(session.started.role)
                    });
                    if let Some(role) = stopped_role {
                        handoffs.insert(plan.card.clone(), (plan.stopped.clone(), role));
                    }
                }
                RunEventKind::Session(SessionEvent::Started(started)) => {
                    let Some(key) = &event.session else { continue };
                    let standing_in_for = event
                        .card_id
                        .as_ref()
                        .and_then(|card| handoffs.get(card))
                        .filter(|(stopped, role)| {
                            *role == started.role && *stopped != started.worker
                        })
                        .map(|(stopped, _)| stopped.clone());
                    let worker = match &started.worker.model {
                        Some(_) => started.worker.clone(),
                        None => WorkerRef::new(
                            started.worker.worker.clone(),
                            configured.get(&started.worker.worker).cloned(),
                        ),
                    };
                    index.insert(key.clone(), sessions.len());
                    sessions.push(SessionFacts {
                        key: key.clone(),
                        card: event.card_id.clone(),
                        started_at: event.ts,
                        started: (**started).clone(),
                        worker,
                        ended: None,
                        usage: Spend::default(),
                        tools: Vec::new(),
                        context: None,
                        subagents: Vec::new(),
                        prompt: String::new(),
                        standing_in_for,
                    });
                }
                _ => {
                    let Some(facts) = event
                        .session
                        .as_ref()
                        .and_then(|key| index.get(key))
                        .and_then(|at| sessions.get_mut(*at))
                    else {
                        continue;
                    };
                    facts.note(&event.kind);
                }
            }
        }
        Self {
            sessions,
            waiting,
            last_ts: events.last().map(|event| event.ts),
            finished_at,
            rotate_at,
        }
    }

    pub fn session(&self, key: &SessionKey) -> Option<&SessionFacts> {
        self.sessions.iter().find(|facts| &facts.key == key)
    }

    pub fn status(&self, facts: &SessionFacts) -> RowStatus {
        match &facts.ended {
            Some(end) => match end.reason {
                EndReason::Completed
                | EndReason::Paused
                | EndReason::Stopped
                | EndReason::ContextFull => RowStatus::Done,
                _ => RowStatus::Failed,
            },
            None if facts.card.is_some()
                && self.waiting.values().any(|card| card == &facts.card) =>
            {
                RowStatus::NeedsYou
            }
            None => RowStatus::Running,
        }
    }

    pub fn elapsed_ms(&self, facts: &SessionFacts) -> u64 {
        match &facts.ended {
            Some(end) => end.duration_ms,
            None => self.since(facts.started_at),
        }
    }

    /// Milliseconds from `from` to the end of the log.
    pub fn since(&self, from: OffsetDateTime) -> u64 {
        let until = self.finished_at.or(self.last_ts).unwrap_or(from);
        u64::try_from((until - from).whole_milliseconds()).unwrap_or(0)
    }

    /// Where a session with this window rotates, from the run's `sessions.rotate_at`.
    pub fn rotate_cap(&self, window: Option<u64>) -> Option<u64> {
        self.rotate_at.tokens(window).ok().flatten()
    }

    pub fn agent_row(&self, facts: &SessionFacts) -> AgentRow {
        let spent = facts.spent();
        AgentRow {
            session: facts.key.clone(),
            label: facts.label(),
            worker: facts.worker.clone(),
            status: self.status(facts),
            tokens: spent.input_tokens + spent.output_tokens,
            spent,
            elapsed_ms: self.elapsed_ms(facts),
            subagents: u32::try_from(facts.subagents.len()).unwrap_or(u32::MAX),
            context: facts.context.and_then(|(used, window)| {
                let cap = self.rotate_cap(window).or(window)?;
                Some(ContextUse { used, cap })
            }),
            standing_in_for: facts.standing_in_for.clone(),
        }
    }

    /// Earlier sessions of the same card and role that ended because their context filled.
    pub fn rotations(&self, facts: &SessionFacts) -> u32 {
        let count = self
            .sessions
            .iter()
            .filter(|other| {
                other.started_at <= facts.started_at
                    && other.key != facts.key
                    && other.card == facts.card
                    && other.started.role == facts.started.role
                    && other
                        .ended
                        .as_ref()
                        .is_some_and(|end| matches!(end.reason, EndReason::ContextFull))
            })
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

impl SessionFacts {
    fn note(&mut self, kind: &RunEventKind) {
        match kind {
            RunEventKind::Session(SessionEvent::Input(input)) => {
                for part in &input.parts {
                    self.prompt.push_str(&part.text);
                }
            }
            RunEventKind::Session(SessionEvent::Ended(end)) => self.ended = Some((**end).clone()),
            RunEventKind::Agent(agent) => self.note_agent(agent),
            _ => {}
        }
    }

    fn note_agent(&mut self, agent: &AgentEvent) {
        match agent {
            AgentEvent::Init { model, .. } if self.worker.model.is_none() => {
                self.worker.model.clone_from(model);
            }
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => self.tools.push(ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                ok: None,
                parent: parent.clone(),
            }),
            AgentEvent::ToolFinished { id, ok, .. } => {
                if let Some(call) = self.tools.iter_mut().rev().find(|call| &call.id == id) {
                    call.ok = Some(*ok);
                }
            }
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cached_tokens,
                cost_usd,
                cumulative,
            } => {
                let reading = Spend {
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    cached_tokens: *cached_tokens,
                    cost_usd: cost_usd.unwrap_or(0.0),
                    cost_partial: cost_usd.is_none(),
                    cost_estimated: false,
                };
                if *cumulative {
                    self.usage = reading;
                } else {
                    add_spend(&mut self.usage, &reading);
                }
            }
            AgentEvent::ContextFill { used, size } => {
                let window = size.or(self.context.and_then(|(_, window)| window));
                self.context = Some((*used, window));
            }
            AgentEvent::Subagent { update } => {
                match self
                    .subagents
                    .iter_mut()
                    .find(|child| child.id == update.id)
                {
                    Some(child) => *child = update.clone(),
                    None => self.subagents.push(update.clone()),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{event, session_started};
    use argonauts_core::DiffStat;
    use argonauts_core::events::vendor::HandoffPlan;
    use argonauts_core::settings::handoff::{HandoffMode, HandoffStrategy};

    #[test]
    fn a_replacement_stands_in_for_the_stopped_worker_in_its_role() {
        let card = CardId::new("card-001").unwrap();
        let claude = WorkerRef::new("claude-code", None);
        let plan = HandoffPlan {
            card: card.clone(),
            title: "Export".to_owned(),
            stopped: claude.clone(),
            reason: "usage limit".to_owned(),
            resets_at: None,
            work: DiffStat::default(),
            gates_ran: false,
            candidates: Vec::new(),
            mode: HandoffMode::Auto,
            strategy: HandoffStrategy::First,
            race_count: 1,
            also_moving: Vec::new(),
        };
        let (s1, s2, s3) = (
            SessionKey::numbered(1),
            SessionKey::numbered(2),
            SessionKey::numbered(3),
        );
        let events = vec![
            session_started(0, &s1, Some(&card), RoleName::Owner, "claude-code"),
            event(1, RunEventKind::Handoff(HandoffEvent::Needed(plan))),
            session_started(2, &s2, Some(&card), RoleName::Owner, "codex"),
            session_started(3, &s3, Some(&card), RoleName::Reviewer, "antigravity-cli"),
        ];
        let facts = RunFacts::of(&events);
        let stands_in = |key| facts.session(key).unwrap().standing_in_for.clone();
        assert_eq!(stands_in(&s1), None);
        assert_eq!(stands_in(&s2), Some(claude));
        assert_eq!(stands_in(&s3), None, "a reviewer replaced no one");
    }

    /// The views name a session by the model it runs: its CLI's from the run's settings when
    /// it names none, else the one it reported at its start.
    #[test]
    fn a_session_is_named_by_the_model_it_runs() {
        use crate::client::test_support::{agent, run_started};
        let mut started = run_started(0, "add a limit", None);
        if let RunEventKind::Run(RunLifecycle::Started(run)) = &mut started.kind {
            run.settings = serde_json::json!({"agents": {"codex": {"model": "gpt-5.6-luna"}}});
        }
        let (codex, claude) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let init = |model: &str| AgentEvent::Init {
            session_id: None,
            model: Some(model.to_owned()),
            tools: Vec::new(),
        };
        let events = vec![
            started,
            session_started(1, &codex, None, RoleName::Owner, "codex"),
            session_started(2, &claude, None, RoleName::Owner, "claude-code"),
            agent(3, &codex, init("gpt-5.6")),
            agent(4, &claude, init("claude-haiku-4-5")),
        ];
        let facts = RunFacts::of(&events);
        let worker = |key: &SessionKey| {
            let session = facts.session(key).unwrap();
            facts.agent_row(session).worker.to_string()
        };
        assert_eq!(worker(&codex), "codex/gpt-5.6-luna");
        assert_eq!(worker(&claude), "claude-code/claude-haiku-4-5");
    }
}
