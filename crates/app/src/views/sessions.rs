//! What the events say about each session, gathered once for the views that draw sessions.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use argonauts_conductor::fallback::why::{category_words, stop_line};
use argonauts_core::app::agent_panel::{
    CrewDetails, CrewRow, CrewRun, CrewState, CrewTarget, LabelTry, first_label, fold_tries,
};
use argonauts_core::app::views::{
    AgentRow, ContextUse, MERGED_AT_ONCE, RowNote, RowStatus, Tone, not_needed,
};
use argonauts_core::base::names::{ApprovalId, ModelRef, RoleName, SessionKey};
use argonauts_core::models::limits::{
    network_down_in, served_on_this_machine, waiting_for_the_network,
};
use argonauts_core::runs::events::agent_status::FallbackEvent;
use argonauts_core::runs::events::approval::{ApprovalEvent, ApprovalPacket};
use argonauts_core::runs::events::context::ContextCategory;
use argonauts_core::runs::events::run::{RunLifecycle, WaitReason};
use argonauts_core::runs::events::session::{
    EndReason, InputPartKind, LimitEvent, SessionEnd, SessionEvent, SessionStarted, SubagentUpdate,
};
use argonauts_core::runs::events::task::{TaskEvent, TaskState};
use argonauts_core::runs::events::{MergeEvent, RunEvent, RunEventKind};
use argonauts_core::settings::Settings;
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::workflows::steps::{StepName, summary_line};
use argonauts_core::{AgentEvent, Spend, TaskId};
use time::OffsetDateTime;

use super::verdicts::Verdicts;

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

/// Where one part of a session's prompt starts in it.
pub(crate) struct PartAt {
    pub kind: InputPartKind,
    pub label: String,
    /// The line of the prompt it starts on, from 0.
    pub line: usize,
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
    pub task: Option<TaskId>,
    pub started_at: OffsetDateTime,
    pub started: SessionStarted,
    /// The worker with the model the session runs: its own, else its CLI's from the run's
    /// settings, else the one it reported at its start. The screen names it by this.
    pub worker: ModelRef,
    pub ended: Option<SessionEnd>,
    /// When its end was logged.
    pub ended_at: Option<OffsetDateTime>,
    /// Its latest whole message of its own, not a subagent's.
    pub last_message: Option<String>,
    /// From `Usage` events, until `Ended` gives the final spend.
    pub usage: Spend,
    /// Argonauts Code's meter, which prices what the stream leaves unpriced.
    pub metered: Option<Spend>,
    pub tools: Vec<ToolCall>,
    /// Used tokens and the window's size, from the latest `ContextFill`.
    pub context: Option<(u64, Option<u64>)>,
    /// What fills the window by its CLI's own categories, from the latest count it gave.
    pub context_rows: Vec<ContextCategory>,
    /// The latest update of each child, in the order they first appeared.
    pub subagents: Vec<SubagentUpdate>,
    /// Every input part, in order: the prompt, then what each delivery added.
    pub prompt: String,
    /// Each of those parts, where it starts.
    pub parts: Vec<PartAt>,
    /// Where it was launched, from its first input.
    pub cwd: Option<PathBuf>,
    /// The worker it replaced after a fallback.
    pub fallback_for: Option<ModelRef>,
    /// Why that worker stopped, in the one line its fallback was planned with.
    pub fallback_why: Option<String>,
    /// The task's try a worker session works on, 1-based.
    pub try_number: Option<u32>,
    /// Lines added and removed in each file it changed since the worktree's last commit.
    pub file_diffs: BTreeMap<String, (u32, u32)>,
    /// It ended as if done, but without a report, and its task failed for that.
    pub unreported: bool,
}

impl SessionFacts {
    /// What it spent: the end's count once it ends. Where the agent gave no dollars, the
    /// meter's priced count stands in, as it does in the run's limit, so a row agrees with
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
    /// the role and the task take its place: `worker · task-001`, and `worker · task-001 · try 2`
    /// for a retry, so the task's sessions read apart wherever their agents' names are cut.
    pub fn label(&self) -> String {
        if let Some(label) = &self.started.label {
            return label.clone();
        }
        let role = self.started.role.as_str();
        match (&self.task, self.try_number) {
            (Some(task), Some(try_number)) if try_number > 1 => {
                format!("{role} · {task} · try {try_number}")
            }
            (Some(task), _) => format!("{role} · {task}"),
            (None, _) => role.to_owned(),
        }
    }
}

/// Everything the session views need, read from the events in one pass.
pub(crate) struct RunFacts {
    pub sessions: Vec<SessionFacts>,
    /// Open approvals that wait for you, with their task.
    pub waiting: BTreeMap<ApprovalId, Option<TaskId>>,
    /// The latest event's time: "now" as far as the log knows.
    pub last_ts: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    rotate_at: RotateAt,
    /// What the run's reviews and checks decided, for the words after a row's counts.
    pub verdicts: Verdicts,
    /// What the run waits for, while its log ends on a wait. A session the wait cut short ends
    /// `Paused` whatever the run waits for, so its row says this instead.
    pub held: Option<WaitReason>,
    /// When the internet went, while the log says it's still down.
    pub network_down: Option<OffsetDateTime>,
    /// Whether the run's work merged, so a session stopped because the merge didn't wait for
    /// it wasn't needed.
    merged: bool,
}

impl RunFacts {
    pub fn of(events: &[RunEvent]) -> Self {
        let mut sessions: Vec<SessionFacts> = Vec::new();
        let mut index: BTreeMap<SessionKey, usize> = BTreeMap::new();
        let mut waiting = BTreeMap::new();
        // The worker that stopped when a task fell back, and the role it worked in.
        let mut fallbacks: BTreeMap<TaskId, (ModelRef, RoleName, String)> = BTreeMap::new();
        let mut rotate_at = Settings::default().sessions.rotate_at;
        let mut finished_at = None;
        let mut held = None;
        let mut network_down = None;
        let mut merged = false;
        // Each CLI's model from the run's settings (`agents.<cli>.model`).
        let mut configured: BTreeMap<String, String> = BTreeMap::new();
        // Each task's current try, where its latest report is in the log, and its worker
        // session that ended with no report yet.
        let mut tries: BTreeMap<TaskId, u32> = BTreeMap::new();
        let mut reported_at: BTreeMap<TaskId, usize> = BTreeMap::new();
        let mut unreported: BTreeMap<TaskId, usize> = BTreeMap::new();
        // Where each session starts in the log.
        let mut starts: Vec<usize> = Vec::new();
        for (at_event, event) in events.iter().enumerate() {
            if let Some(down) = network_down_in(&event.kind) {
                network_down = down.then_some(event.ts);
            }
            match &event.kind {
                RunEventKind::Task(task_event) => {
                    let Some(task) = &event.task_id else { continue };
                    match task_event {
                        TaskEvent::Try { try_number, .. } => {
                            tries.insert(task.clone(), *try_number);
                        }
                        TaskEvent::Reported(_) => {
                            reported_at.insert(task.clone(), at_event);
                            unreported.remove(task);
                        }
                        TaskEvent::State {
                            to: TaskState::Failed | TaskState::NeedsYou,
                            ..
                        } => {
                            if let Some(at) = unreported.remove(task)
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
                RunEventKind::Run(RunLifecycle::Finished { .. }) => {
                    finished_at = Some(event.ts);
                    held = None;
                }
                RunEventKind::Run(RunLifecycle::Waiting { reason, .. }) => {
                    held = Some(reason.clone());
                }
                RunEventKind::Run(RunLifecycle::Resumed(_)) => held = None,
                RunEventKind::Merge(MergeEvent::RunMerged { .. }) => merged = true,
                RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                    if opened.needs_you {
                        let task = opened.task.clone().or_else(|| match &opened.packet {
                            ApprovalPacket::Question(question) => Some(question.task.clone()),
                            _ => None,
                        });
                        waiting.insert(opened.id.clone(), task);
                    }
                }
                RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                    waiting.remove(&decided.id);
                }
                RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                    waiting.remove(id);
                }
                RunEventKind::Fallback(FallbackEvent::Needed(plan)) => {
                    let stopped_role = sessions.iter().rev().find_map(|session| {
                        (session.task.as_ref() == Some(&plan.task)
                            && session.started.worker == plan.stopped)
                            .then_some(session.started.role)
                    });
                    if let Some(role) = stopped_role {
                        let stopped = (plan.stopped.clone(), role, plan.reason.clone());
                        fallbacks.insert(plan.task.clone(), stopped);
                    }
                }
                RunEventKind::Session(SessionEvent::Started(started)) => {
                    let Some(key) = &event.session else { continue };
                    let fallback = event
                        .task_id
                        .as_ref()
                        .and_then(|task| fallbacks.get(task))
                        .filter(|(stopped, role, _)| {
                            *role == started.role && *stopped != started.worker
                        });
                    // A step's own fallback names the model the step asked, which another
                    // runs in place of: a review that stays on its CLI with the next model.
                    let asked = started
                        .asked
                        .as_ref()
                        .filter(|asked| **asked != started.worker);
                    let fallback_for = fallback
                        .map(|(stopped, ..)| stopped.clone())
                        .or_else(|| asked.cloned());
                    let fallback_why = fallback
                        .map(|(.., why)| why.trim().to_owned())
                        .filter(|why| !why.is_empty())
                        .or_else(|| asked.and(started.asked_why.clone()))
                        .or_else(|| {
                            why_it_stopped(&sessions, event.task_id.as_ref(), started, asked?)
                        });
                    let worker = match &started.worker.model {
                        Some(_) => started.worker.clone(),
                        None => ModelRef::new(
                            started.worker.agent.clone(),
                            configured.get(&started.worker.agent).cloned(),
                        ),
                    };
                    let owns = started.step == Some(StepName::Build);
                    let try_number = event
                        .task_id
                        .as_ref()
                        .filter(|_| owns)
                        .and_then(|task| tries.get(task).copied());
                    index.insert(key.clone(), sessions.len());
                    starts.push(at_event);
                    sessions.push(SessionFacts {
                        key: key.clone(),
                        task: event.task_id.clone(),
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
                        context_rows: Vec::new(),
                        subagents: Vec::new(),
                        prompt: String::new(),
                        parts: Vec::new(),
                        cwd: None,
                        fallback_for,
                        fallback_why,
                        try_number,
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
                    // answer comes after it, before the question moves on.
                    if let Some(task) = &facts.task
                        && facts.started.step == Some(StepName::Build)
                        && end.reason == EndReason::Completed
                        && reported_at
                            .get(task)
                            .is_none_or(|reported| starts.get(*at).is_some_and(|s| reported < s))
                    {
                        unreported.insert(task.clone(), *at);
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
            verdicts: Verdicts::of(events),
            held,
            network_down,
            merged,
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
                EndReason::Completed | EndReason::ContextFull => RowStatus::Done,
                // Held where it was, its work so far kept, until the run goes on.
                EndReason::Paused => RowStatus::Paused,
                // Stopped by you: it neither finished nor failed.
                EndReason::Stopped => RowStatus::Skipped,
                _ => RowStatus::Failed,
            },
            None if facts.task.is_some()
                && self.waiting.values().any(|task| task == &facts.task) =>
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
        let not_needed = self.not_needed(facts);
        AgentRow {
            session: facts.key.clone(),
            label: facts.label(),
            worker: facts.worker.clone(),
            status: self.status(facts),
            tokens: spent.input_tokens + spent.output_tokens,
            spent,
            elapsed_ms: self.elapsed_ms(facts),
            subagents: u32::try_from(facts.subagents.len()).unwrap_or(u32::MAX),
            context: facts.context.map(|(used, window)| ContextUse {
                used,
                window,
                cap: self.rotate_cap(window),
            }),
            fallback_for: facts.fallback_for.clone(),
            answered: false,
            tries: Vec::new(),
            // A fallback's row says why it runs until it has something of its own to say. Why
            // a step chose another before the start reads on from the asked model's name, which
            // only the agent view has room for.
            note: not_needed
                .then(|| RowNote::new(MERGED_AT_ONCE, Tone::Plain))
                .or_else(|| self.held_by_the_network(facts))
                .or_else(|| self.verdicts.note(self, facts))
                .or_else(|| {
                    let why = facts.fallback_why.as_deref()?;
                    let chosen = facts.started.asked_why.is_some();
                    (!chosen).then(|| RowNote::new(why, Tone::Warn))
                }),
            inner: None,
            left_out: false,
            not_needed,
            fallback_why: facts
                .fallback_why
                .clone()
                .filter(|_| facts.fallback_for.is_some()),
        }
    }

    /// Whether `facts` is the commit-message writer that was stopped once the run merged
    /// without its message.
    fn not_needed(&self, facts: &SessionFacts) -> bool {
        let stopped = facts
            .ended
            .as_ref()
            .is_some_and(|end| end.reason == EndReason::Stopped);
        not_needed(facts.started.step, stopped, self.merged)
    }

    /// What a running session's row says while the internet is down: how long it has waited.
    /// A session served on this machine needs no internet, so it says nothing of it.
    fn held_by_the_network(&self, facts: &SessionFacts) -> Option<RowNote> {
        let since = self.network_down?;
        let worker = &facts.worker;
        if self.status(facts) != RowStatus::Running
            || served_on_this_machine(&worker.agent, worker.model.as_deref())
        {
            return None;
        }
        let held = Duration::from_millis(self.since(since));
        Some(RowNote::new(waiting_for_the_network(held), Tone::Plain))
    }

    /// Earlier sessions of the same task and role that ended because their context filled.
    pub fn rotations(&self, facts: &SessionFacts) -> u32 {
        let count = self
            .sessions
            .iter()
            .filter(|other| {
                other.started_at <= facts.started_at
                    && other.key != facts.key
                    && other.task == facts.task
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

/// Why the model a step asked stopped, from its last session in the same job, in the few
/// words a fallback's row has room for: a CLI's error by its kind, such as `overloaded`. The
/// job is the step, the role and the task, since only a task's sessions keep one label. A
/// session that finished, or that you or a pause ended, is no reason.
fn why_it_stopped(
    sessions: &[SessionFacts],
    task: Option<&TaskId>,
    started: &SessionStarted,
    asked: &ModelRef,
) -> Option<String> {
    let tried = sessions.iter().rev().find(|earlier| {
        let before = &earlier.started;
        before.worker == *asked
            && earlier.task.as_ref() == task
            && before.workflow_step == started.workflow_step
            && before.role == started.role
            && before.step == started.step
    })?;
    match &tried.ended.as_ref()?.reason {
        EndReason::Completed | EndReason::Paused | EndReason::Stopped => None,
        EndReason::CliError { category, .. } => Some(category_words(*category).to_owned()),
        reason => Some(stop_line(reason)),
    }
}

/// A run's agents as the list of everything running shows them: each by its
/// label and last line, with an agent's retries folded into one row. `live` says the run's
/// run host is up, so an agent with no end is working. Without one it waits to be resumed.
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
            // A writer the merge didn't wait for says why, not what it was writing.
            let latest = match state {
                CrewState::Skipped => Some(MERGED_AT_ONCE.to_owned()),
                _ => last_message.as_deref().and_then(last_line),
            };
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
                latest: latest.unwrap_or_else(|| state_word(state).to_owned()),
                updated: session
                    .ended_at
                    .or(facts.last_ts)
                    .unwrap_or(session.started_at),
                details: CrewDetails {
                    last_message,
                    task: session.task.clone(),
                    spent: Some(session.spent()),
                    context: facts.agent_row(session).context,
                    branch: None,
                    prompt: Some(session.prompt.trim().to_owned()).filter(|text| !text.is_empty()),
                },
            };
            LabelTry {
                key: match &session.task {
                    Some(task) => format!("{task}/{label}"),
                    None => format!("{}/{}", session.started.role.as_str(), first_label(&label)),
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
    if facts.not_needed(session) {
        return CrewState::Skipped;
    }
    let stopped = session
        .ended
        .as_ref()
        .is_some_and(|end| matches!(end.reason, EndReason::Stopped | EndReason::Paused));
    match facts.status(session) {
        RowStatus::NeedsYou => CrewState::NeedsYou,
        RowStatus::Failed => CrewState::Failed,
        RowStatus::Paused => CrewState::Stopped,
        RowStatus::Done | RowStatus::Skipped if stopped => CrewState::Stopped,
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
        CrewState::Skipped => "skipped",
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
                if self.worker.model.is_none() {
                    self.worker.model = input.launch.model().map(str::to_owned);
                }
                let mut line = self.prompt.matches('\n').count();
                for part in &input.parts {
                    self.parts.push(PartAt {
                        kind: part.kind,
                        label: part.label.clone(),
                        line,
                    });
                    self.prompt.push_str(&part.text);
                    line += part.text.matches('\n').count();
                }
            }
            RunEventKind::Session(SessionEvent::Limit(LimitEvent::Spend(status))) => {
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
            // How full it is stays the fill's to say, which is what it rotates by.
            AgentEvent::ContextBreakdown { usage } => {
                self.context_rows.clone_from(&usage.categories);
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
    use argonauts_core::runs::events::agent_status::FallbackPlan;
    use argonauts_core::settings::fallback::{FallbackMode, FallbackStrategy};

    #[test]
    fn a_fallback_takes_the_place_of_the_stopped_worker_in_its_role() {
        let task = TaskId::new("task-001").unwrap();
        let claude = ModelRef::new("claude-code", None);
        let plan = FallbackPlan {
            task: task.clone(),
            title: "Export".to_owned(),
            stopped: claude.clone(),
            reason: "usage limit".to_owned(),
            resets_at: None,
            work: DiffStat::default(),
            checks_ran: false,
            candidates: Vec::new(),
            mode: FallbackMode::Auto,
            strategy: FallbackStrategy::First,
            race_count: 1,
            also_moving: Vec::new(),
        };
        let (s1, s2, s3) = (
            SessionKey::numbered(1),
            SessionKey::numbered(2),
            SessionKey::numbered(3),
        );
        let events = vec![
            session_started(0, &s1, Some(&task), RoleName::Worker, "claude-code"),
            event(1, RunEventKind::Fallback(FallbackEvent::Needed(plan))),
            session_started(2, &s2, Some(&task), RoleName::Worker, "codex"),
            session_started(3, &s3, Some(&task), RoleName::Reviewer, "antigravity-cli"),
        ];
        let facts = RunFacts::of(&events);
        let fallback_for = |key| facts.session(key).unwrap().fallback_for.clone();
        assert_eq!(fallback_for(&s1), None);
        assert_eq!(fallback_for(&s2), Some(claude));
        assert_eq!(fallback_for(&s3), None, "a reviewer replaced no one");

        // The fallback's row says why the first worker stopped, and no other row does.
        let note = |key| {
            let row = facts.agent_row(facts.session(key).unwrap());
            row.note.map(|note| (note.text, note.tone))
        };
        assert_eq!(note(&s1), None);
        assert_eq!(note(&s2), Some(("usage limit".to_owned(), Tone::Warn)));
        assert_eq!(note(&s3), None);
    }

    /// A step that stays on its CLI starts its fallback under the model it asked. The
    /// fallback's row names that model and says why it stopped, in the few words its error
    /// has, as a task's fallback does.
    #[test]
    fn a_steps_own_fallback_is_named_for_the_model_it_asked() {
        use crate::client::test_support::session_ended;
        use argonauts_core::ErrorCategory;

        let task = TaskId::new("task-001").unwrap();
        let (first, second) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let opus: ModelRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let sonnet: ModelRef = "claude-code/claude-sonnet-5-5".parse().unwrap();
        let review = |seq, key: &SessionKey, worker: &ModelRef, asked: Option<&ModelRef>| {
            let mut started =
                session_started(seq, key, Some(&task), RoleName::Reviewer, "claude-code");
            if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
                session.worker = worker.clone();
                session.label = Some("review:due-tests".to_owned());
                session.asked = asked.cloned();
            }
            started
        };
        let overloaded = EndReason::CliError {
            category: ErrorCategory::Overloaded,
            message: "Overloaded".to_owned(),
        };
        let events = vec![
            review(0, &first, &opus, None),
            session_ended(1, &first, overloaded, 2_000),
            review(2, &second, &sonnet, Some(&opus)),
        ];
        let facts = RunFacts::of(&events);
        let row = |key| facts.agent_row(facts.session(key).unwrap());
        assert_eq!(row(&first).fallback_for, None);
        let fallback = row(&second);
        assert_eq!(fallback.fallback_for, Some(opus));
        assert_eq!(
            fallback.note.map(|note| (note.text, note.tone)),
            Some(("overloaded".to_owned(), Tone::Warn))
        );
    }

    /// A job without a task gives its fallback a label of its own, so the reason comes from
    /// the asked model's session in the same step and role. One you stopped is no reason.
    #[test]
    fn a_fallback_without_a_task_still_says_why_the_asked_model_stopped() {
        use crate::client::test_support::session_ended;
        use argonauts_core::ErrorCategory;
        use argonauts_core::base::names::WorkflowId;
        use argonauts_core::workflows::workflow::StepRef;

        let opus: ModelRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let sonnet: ModelRef = "claude-code/claude-sonnet-5-5".parse().unwrap();
        let step = |index| StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![index],
        };
        let reading = |seq, key: u32, label: &str, at: u32, asked: Option<&ModelRef>| {
            let key = SessionKey::numbered(key);
            let mut started = session_started(seq, &key, None, RoleName::Reader, "claude-code");
            if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
                session.worker = if asked.is_some() { &sonnet } else { &opus }.clone();
                session.label = Some(label.to_owned());
                session.workflow_step = Some(step(at));
                session.asked = asked.cloned();
            }
            started
        };
        let ended =
            |seq, key: u32, reason| session_ended(seq, &SessionKey::numbered(key), reason, 2_000);
        let overloaded = EndReason::CliError {
            category: ErrorCategory::Overloaded,
            message: "Overloaded".to_owned(),
        };
        let events = vec![
            reading(0, 1, "read:anthropic", 0, None),
            ended(1, 1, overloaded),
            reading(2, 2, "read:anthropic-2", 0, Some(&opus)),
            // Another step's reader, which you stopped before its fallback started.
            reading(3, 3, "read:anthropic-3", 1, None),
            ended(4, 3, EndReason::Stopped),
            reading(5, 4, "read:anthropic-4", 1, Some(&opus)),
        ];
        let facts = RunFacts::of(&events);
        let said = |key: u32| {
            let row = facts.agent_row(facts.session(&SessionKey::numbered(key)).unwrap());
            (row.fallback_for, row.note.map(|note| note.text))
        };
        assert_eq!(said(2), (Some(opus.clone()), Some("overloaded".to_owned())));
        assert_eq!(said(4), (Some(opus), None));
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
    /// last line, and folds an agent's retries into one row.
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
        // With no run host an agent with no end isn't working: it waits to be resumed.
        let waiting = crew_rows(&events, &run, "~/app", false);
        assert_eq!(waiting[1].state, CrewState::Waiting);
    }

    /// While the internet is down, a running session's row says how long it has waited for
    /// it. A model served on this machine works on, and once the internet is back no row
    /// waits.
    #[test]
    fn a_running_row_waits_for_the_network_while_it_is_down() {
        use argonauts_core::models::limits::{NETWORK_BACK, NETWORK_DOWN};
        use argonauts_core::runs::events::workflow::{RunNote, WorkflowEvent};

        let (cloud, local) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let note = |seq, text: &str| {
            event(
                seq,
                RunEventKind::Workflow(WorkflowEvent::Note(RunNote {
                    text: text.to_owned(),
                    workflow: None,
                    left: false,
                })),
            )
        };
        let mut served_here = session_started(1, &local, None, RoleName::Reviewer, "opencode");
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut served_here.kind {
            started.worker = "opencode/ollama/qwen3-coder:30b".parse().unwrap();
        }
        let mut events = vec![
            session_started(0, &cloud, None, RoleName::Worker, "claude-code"),
            served_here,
            note(10, NETWORK_DOWN),
            note(140, "Proposals are in"),
        ];
        let said = |events: &[RunEvent], key| {
            let facts = RunFacts::of(events);
            let row = facts.agent_row(facts.session(key).unwrap());
            row.note.map(|note| (note.text, note.tone))
        };
        assert_eq!(
            said(&events, &cloud),
            Some(("waiting for the network · 2m 10s".to_owned(), Tone::Plain))
        );
        assert_eq!(
            said(&events, &local),
            None,
            "a local model needs no internet"
        );

        events.push(note(150, NETWORK_BACK));
        assert_eq!(said(&events, &cloud), None);
    }

    /// A step that gives a session to another model before it starts says why, and the
    /// fallback's agent view reads it on from the asked model's name. Its row says only
    /// whose place it took.
    #[test]
    fn a_fallback_chosen_before_the_start_says_why_in_its_agent_view() {
        let key = SessionKey::numbered(1);
        let qwen: ModelRef = "ollama/qwen3-coder:30b-64k".parse().unwrap();
        let mut started = session_started(0, &key, None, RoleName::Reviewer, "codex");
        if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
            session.label = Some("review:per-api-key".to_owned());
            session.asked = Some(qwen.clone());
            session.asked_why = Some("whose 64k window can't hold this change".to_owned());
        }
        let facts = RunFacts::of(&[started]);
        let row = facts.agent_row(facts.session(&key).unwrap());
        assert_eq!(row.fallback_for, Some(qwen));
        assert_eq!(
            row.fallback_why.as_deref(),
            Some("whose 64k window can't hold this change")
        );
        assert_eq!(row.note, None);
    }
}
