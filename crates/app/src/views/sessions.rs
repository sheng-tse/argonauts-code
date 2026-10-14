//! What the events say about each session, gathered once for the views that draw sessions.

use std::collections::BTreeMap;
use std::path::PathBuf;

use argonauts_core::crew_panel::{
    CrewDetails, CrewRow, CrewRun, CrewState, CrewTarget, SeatTry, fold_tries, seat_of,
};
use argonauts_core::events::card::{CardEvent, CardState};
use argonauts_core::events::checkpoint::{CheckpointEvent, CheckpointPacket};
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::{
    EndReason, LimitEvent, SessionEnd, SessionEvent, SessionStarted, SubagentUpdate,
};
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{CheckpointId, RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::steps::{StepName, summary_line};
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

/// The token counts of whichever reading saw more.
fn larger_tokens(a: Spend, b: Spend) -> Spend {
    if b.total_tokens() > a.total_tokens() {
        b
    } else {
        a
    }
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
    /// When its end was logged.
    pub ended_at: Option<OffsetDateTime>,
    /// Its latest whole message of its own, not a subagent's.
    pub last_message: Option<String>,
    /// From `Usage` events, until `Ended` gives the final spend.
    pub usage: Spend,
    /// The conductor's meter, which prices what the stream leaves unpriced.
    pub metered: Option<Spend>,
    pub tools: Vec<ToolCall>,
    /// Used tokens and the window's size, from the latest `ContextFill`.
    pub context: Option<(u64, Option<u64>)>,
    /// The latest update of each child, in the order they first appeared.
    pub subagents: Vec<SubagentUpdate>,
    /// Every input part, in order: the prompt, then what each delivery added.
    pub prompt: String,
    /// Where it was launched, from its first input.
    pub cwd: Option<PathBuf>,
    /// The worker it replaced after a handoff.
    pub standing_in_for: Option<WorkerRef>,
    /// The card's attempt an owner session works on, 1-based.
    pub attempt: Option<u32>,
    /// Lines added and removed in each file it changed since the worktree's last commit.
    pub file_diffs: BTreeMap<String, (u32, u32)>,
    /// It ended as if done, but without a report, and its card failed for that.
    pub unreported: bool,
}

impl SessionFacts {
    /// What it spent: the end's count once it ends. Where the vendor gave no dollars, the
    /// meter's priced count stands in, as it does in the run's budget, so a row agrees with
    /// the header.
    pub fn spent(&self) -> Spend {
        let reported = self.ended.as_ref().map_or(self.usage, |end| end.spend);
        match self.metered {
            Some(metered) if reported.cost_partial && !metered.cost_partial => Spend {
                cost_usd: metered.cost_usd,
                cost_partial: false,
                cost_estimated: metered.cost_estimated,
                ..larger_tokens(reported, metered)
            },
            _ => reported,
        }
    }

    pub fn tool_calls(&self) -> u32 {
        let counted = u32::try_from(self.tools.len()).unwrap_or(u32::MAX);
        let reported = self
            .ended
            .as_ref()
            .map_or(0, |end| u32::try_from(end.tool_calls).unwrap_or(u32::MAX));
        counted.max(reported)
    }

    /// The label the run gave it, such as `fix:slugify`. A log from before labels has none, so
    /// the role and the card stand in: `owner · card-001`, and `owner · card-001 · attempt 2`
    /// for a retry, so the card's sessions read apart wherever their agents' names are cut.
    pub fn label(&self) -> String {
        if let Some(label) = &self.started.label {
            return label.clone();
        }
        let role = self.started.role.as_str();
        match (&self.card, self.attempt) {
            (Some(card), Some(attempt)) if attempt > 1 => {
                format!("{role} · {card} · attempt {attempt}")
            }
            (Some(card), _) => format!("{role} · {card}"),
            (None, _) => role.to_owned(),
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
        // Each card's current attempt, where its latest report is in the log, and its owner
        // session that ended with no report yet.
        let mut attempts: BTreeMap<CardId, u32> = BTreeMap::new();
        let mut reported_at: BTreeMap<CardId, usize> = BTreeMap::new();
        let mut unreported: BTreeMap<CardId, usize> = BTreeMap::new();
        // Where each session starts in the log.
        let mut starts: Vec<usize> = Vec::new();
        for (at_event, event) in events.iter().enumerate() {
            match &event.kind {
                RunEventKind::Card(card_event) => {
                    let Some(card) = &event.card_id else { continue };
                    match card_event {
                        CardEvent::Attempt { attempt, .. } => {
                            attempts.insert(card.clone(), *attempt);
                        }
                        CardEvent::Reported(_) => {
                            reported_at.insert(card.clone(), at_event);
                            unreported.remove(card);
                        }
                        CardEvent::State {
                            to: CardState::Failed | CardState::Escalated,
                            ..
                        } => {
                            if let Some(at) = unreported.remove(card)
                                && let Some(facts) = sessions.get_mut(at)
                            {
                                facts.unreported = true;
                            }
                        }
                        _ => {}
                    }
                }
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
                    let owns = started.step == Some(StepName::Build);
                    let attempt = event
                        .card_id
                        .as_ref()
                        .filter(|_| owns)
                        .and_then(|card| attempts.get(card).copied());
                    index.insert(key.clone(), sessions.len());
                    starts.push(at_event);
                    sessions.push(SessionFacts {
                        key: key.clone(),
                        card: event.card_id.clone(),
                        started_at: event.ts,
                        started: (**started).clone(),
                        worker,
                        ended: None,
                        ended_at: None,
                        last_message: None,
                        usage: Spend::default(),
                        metered: None,
                        tools: Vec::new(),
                        context: None,
                        subagents: Vec::new(),
                        prompt: String::new(),
                        cwd: None,
                        standing_in_for,
                        attempt,
                        file_diffs: BTreeMap::new(),
                        unreported: false,
                    });
                }
                RunEventKind::Session(SessionEvent::Ended(end)) => {
                    let Some(at) = event.session.as_ref().and_then(|key| index.get(key)) else {
                        continue;
                    };
                    let Some(facts) = sessions.get_mut(*at) else {
                        continue;
                    };
                    facts.ended = Some((**end).clone());
                    facts.ended_at = Some(event.ts);
                    // A report sent with a tool came before the end; one parsed from the last
                    // answer comes after it, before the card moves on.
                    if let Some(card) = &facts.card
                        && facts.started.step == Some(StepName::Build)
                        && end.reason == EndReason::Completed
                        && reported_at
                            .get(card)
                            .is_none_or(|reported| starts.get(*at).is_some_and(|s| reported < s))
                    {
                        unreported.insert(card.clone(), *at);
                    }
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

    /// The session as it stands: its newest start. A session that goes again after a failure
    /// starts once more under its key, and the row that stands for its step is that start.
    pub fn session(&self, key: &SessionKey) -> Option<&SessionFacts> {
        self.sessions.iter().rev().find(|facts| &facts.key == key)
    }

    pub fn status(&self, facts: &SessionFacts) -> RowStatus {
        match &facts.ended {
            Some(_) if facts.unreported => RowStatus::Failed,
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
            answered: None,
            tries: Vec::new(),
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

/// A run's agents as the list of everything running shows them: each by its
/// label and last line, with a seat's retries folded into one row. `live` says the run's
/// supervisor is up, so an agent with no end is working. Without one it waits to be resumed.
pub(crate) fn crew_rows(
    events: &[RunEvent],
    run: &CrewRun,
    repo: &str,
    live: bool,
) -> Vec<CrewRow> {
    let facts = RunFacts::of(events);
    let tries = facts
        .sessions
        .iter()
        .map(|session| {
            let label = session.label();
            let state = crew_state(&facts, session, live);
            let last_message = session.last_said();
            let row = CrewRow {
                target: CrewTarget::Agent {
                    run: run.id.clone(),
                    session: session.key.clone(),
                },
                state,
                title: run.title.clone(),
                label: Some(label.clone()),
                tries: 0,
                repo: repo.to_owned(),
                run: Some(run.clone()),
                worker: Some(session.worker.clone()),
                latest: last_message
                    .as_deref()
                    .and_then(last_line)
                    .unwrap_or_else(|| state_word(state).to_owned()),
                updated: session
                    .ended_at
                    .or(facts.last_ts)
                    .unwrap_or(session.started_at),
                details: CrewDetails {
                    last_message,
                    card: session.card.clone(),
                    spent: Some(session.spent()),
                    context: facts.agent_row(session).context,
                    branch: None,
                    prompt: Some(session.prompt.trim().to_owned()).filter(|text| !text.is_empty()),
                },
            };
            SeatTry {
                seat: match &session.card {
                    Some(card) => format!("{card}/{label}"),
                    None => format!("{}/{}", session.started.role.as_str(), seat_of(&label)),
                },
                started: session.started_at,
                ended: session.ended_at,
                row,
            }
        })
        .collect();
    fold_tries(tries)
}

fn crew_state(facts: &RunFacts, session: &SessionFacts, live: bool) -> CrewState {
    let stopped = session
        .ended
        .as_ref()
        .is_some_and(|end| matches!(end.reason, EndReason::Stopped | EndReason::Paused));
    match facts.status(session) {
        RowStatus::NeedsYou => CrewState::NeedsYou,
        RowStatus::Failed => CrewState::Failed,
        RowStatus::Done if stopped => CrewState::Stopped,
        RowStatus::Done | RowStatus::Skipped => CrewState::Done,
        RowStatus::Running | RowStatus::Queued | RowStatus::Waiting if live => CrewState::Working,
        RowStatus::Running | RowStatus::Queued | RowStatus::Waiting => CrewState::Waiting,
    }
}

/// What a row says when its agent has said nothing yet.
fn state_word(state: CrewState) -> &'static str {
    match state {
        CrewState::NeedsYou => "needs you",
        CrewState::Working => "working",
        CrewState::Waiting => "waiting",
        CrewState::Ready => "ready",
        CrewState::Done => "done",
        CrewState::Stopped => "stopped",
        CrewState::Failed => "failed",
    }
}

/// The last line of `text` with anything in it.
fn last_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_owned)
}

impl SessionFacts {
    /// What it said last: its report's summary, else its final result, else its latest whole
    /// message. A structured result never shows as JSON.
    fn last_said(&self) -> Option<String> {
        let ended = self.ended.as_ref().and_then(|end| {
            let summary = end.structured.as_ref().and_then(summary_line);
            let result = end.result_text.clone().filter(|text| {
                !serde_json::from_str::<serde_json::Value>(text.trim())
                    .is_ok_and(|value| value.is_object())
            });
            summary.or(result).or_else(|| end.last_message.clone())
        });
        ended
            .or_else(|| self.last_message.clone())
            .filter(|text| !text.trim().is_empty())
    }

    fn note(&mut self, kind: &RunEventKind) {
        match kind {
            RunEventKind::Session(SessionEvent::Input(input)) => {
                self.cwd.get_or_insert_with(|| input.launch.cwd.clone());
                for part in &input.parts {
                    self.prompt.push_str(&part.text);
                }
            }
            RunEventKind::Session(SessionEvent::Limit(LimitEvent::Budget(status))) => {
                self.metered = Some(status.spend);
            }
            RunEventKind::Session(SessionEvent::FileDiff(diff)) => {
                self.file_diffs
                    .insert(diff.path.clone(), (diff.added, diff.removed));
            }
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
            AgentEvent::Message { text, parent: None } if !text.trim().is_empty() => {
                self.last_message = Some(text.clone());
            }
            AgentEvent::ContextFill { used, size, .. } => {
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
        let card = CardId::new("task-001").unwrap();
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
            session_started(0, &s1, Some(&card), RoleName::Worker, "claude-code"),
            event(1, RunEventKind::Handoff(HandoffEvent::Needed(plan))),
            session_started(2, &s2, Some(&card), RoleName::Worker, "codex"),
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
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        };
        let events = vec![
            started,
            session_started(1, &codex, None, RoleName::Worker, "codex"),
            session_started(2, &claude, None, RoleName::Worker, "claude-code"),
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

    /// The list of everything running names a run's agents by their labels, says each one's
    /// last line, and folds a seat's retries into one row.
    #[test]
    fn a_run_s_agents_list_by_label_with_retries_folded() {
        use crate::client::test_support::{agent, base_ts, event_at, session_ended};
        use argonauts_core::RunId;
        let labelled = |seq: u64, key: &SessionKey, role, label: &str, minute: i64| {
            let mut started = session_started(seq, key, None, role, "codex");
            if let RunEventKind::Session(SessionEvent::Started(start)) = &mut started.kind {
                start.label = Some(label.to_owned());
            }
            started.ts = base_ts() + time::Duration::minutes(minute);
            started
        };
        let ended = |seq: u64, key: &SessionKey, reason, minute: i64| {
            let mut end = session_ended(seq, key, reason, 60_000);
            end.ts = base_ts() + time::Duration::minutes(minute);
            end
        };
        let said = |seq: u64, key: &SessionKey, text: &str| {
            agent(
                seq,
                key,
                AgentEvent::Message {
                    text: text.to_owned(),
                    parent: None,
                },
            )
        };
        let key = SessionKey::numbered;
        let failed = EndReason::AdapterBroken {
            detail: "the plan had no tasks".to_owned(),
        };
        let mut events = vec![
            labelled(0, &key(1), RoleName::Orchestrator, "plan:orchestrator", 0),
            ended(1, &key(1), failed.clone(), 1),
            labelled(2, &key(2), RoleName::Orchestrator, "plan:orchestrator-2", 1),
            ended(3, &key(2), failed, 2),
            labelled(4, &key(3), RoleName::Orchestrator, "plan:orchestrator-3", 2),
            ended(5, &key(3), EndReason::Completed, 3),
            labelled(6, &key(4), RoleName::Worker, "build:add-due", 3),
            said(
                7,
                &key(4),
                "Reading the store.\n\nImplemented optional due dates.\n",
            ),
        ];
        events.push(event_at(
            8,
            base_ts() + time::Duration::minutes(5),
            RunEventKind::Agent(AgentEvent::Reasoning {
                text: String::new(),
            }),
        ));
        let run = CrewRun::new(RunId::new("run-01").unwrap(), "Add due dates");
        let rows = crew_rows(&events, &run, "~/app", true);
        let shown: Vec<(&str, u32, CrewState, &str)> = rows
            .iter()
            .map(|row| {
                (
                    row.label.as_deref().unwrap_or_default(),
                    row.tries,
                    row.state,
                    row.latest.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "plan:orchestrator",
                    3,
                    CrewState::Done,
                    "accepted on its 3rd try"
                ),
                (
                    "build:add-due",
                    0,
                    CrewState::Working,
                    "Implemented optional due dates."
                ),
            ]
        );
        // Enter on the folded row opens the try that was accepted.
        assert!(matches!(
            &rows[0].target,
            CrewTarget::Agent { session, .. } if *session == key(3)
        ));
        // With no supervisor an agent with no end isn't working: it waits to be resumed.
        let parked = crew_rows(&events, &run, "~/app", false);
        assert_eq!(parked[1].state, CrewState::Waiting);
    }
}
