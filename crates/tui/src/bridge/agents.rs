//! Agent sessions in the main screen. An agent at work shows in the list under the input, and
//! lands in scrollback once, when it finishes; with `[ui] agents_in_conversation = "as_they_work"`
//! it has a live block, and its rows print as its calls finish. What prints goes by verbosity
//! (`full` shows every finished call and message named with its agent, `normal` folds quiet calls
//! into groups, `quiet` shows milestones only). A finished session prints one collapsed line,
//! except in a resumed run's history, where the condensed replay prints in its place. Argonauts
//! Code's own calls, such as the report, stay out of the block and scrollback; only Ctrl+O shows
//! them. A report shows as its one-line summary instead, whichever way the agent sent it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use argonauts_core::app::AppEvent;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::{
    CliWorkflowRun, EndReason, FileDiff, SessionEnd, SessionEvent, SubagentKind, SubagentStatus,
    SubagentUpdate, Watchdog, loaded_words,
};
use argonauts_core::events::task::TaskState;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::ui::{AgentsInConversation, Glyphs as GlyphChoice};
use argonauts_core::steps::StepName;
use argonauts_core::steps::task::assumption;
use argonauts_core::{AgentEvent, ErrorCategory, RunId, TaskId, TodoItem, TodoStatus, Verbosity};
use serde_json::Value;
use time::OffsetDateTime;

use super::{Cx, cli_workflow_run, ended, failure, plugin_output, reviewed, tools};
use crate::cell::{
    AgentsStarted, Cell, DiffView, StartedAgent, Todo, TodoState, Tone, ToolCall, ToolItem,
    ToolKind, ToolState, ToolStats, ToolTitle,
};
use crate::format;
use crate::history::CellId;
use crate::state::task_label;
use crate::theme::Glyphs;

/// Tool items a live block keeps, newest last. Quiet calls in a row count as one.
const LIVE_CALLS: usize = 3;

/// Finished items a session holds back before printing them together under its name.
const OUTBOX_MAX: usize = 6;

/// Sessions started this close together print as one `● 3 agents started` tree.
const STARTED_TOGETHER: Duration = Duration::from_secs(2);

/// Longest agent event type shown in `? unknown event (…)`.
const UNKNOWN_TYPE_MAX: usize = 60;

#[derive(Debug, Default)]
pub struct AgentBlocks {
    /// In the order they started.
    sessions: Vec<Live>,
    started: StartedGroup,
    /// Whether every check has passed so far, per task.
    checks: BTreeMap<(RunId, TaskId), bool>,
    /// The newest event time, which puts replayed sessions back on the clock.
    latest: Option<OffsetDateTime>,
    /// Each review's rounds so far.
    rounds: reviewed::Rounds,
}

#[derive(Debug, Default)]
struct StartedGroup {
    since: Option<OffsetDateTime>,
    agents: Vec<StartedAgent>,
}

/// One working session: its live block and what it has done so far.
#[derive(Debug)]
struct Live {
    /// Headed by its label, with the agent's name after it.
    labeled: bool,
    run: RunId,
    key: SessionKey,
    agent_id: String,
    task: Option<TaskId>,
    /// The agent's header with nothing under it.
    head: ToolCall,
    cell: Option<CellId>,
    started: Instant,
    started_at: OffsetDateTime,
    heard: Instant,
    heard_at: OffsetDateTime,
    /// Every call and message, for the collapsed line and the transcript overlay.
    whole: ToolCall,
    /// Keys of Argonauts Code's own calls, which `whole` keeps for the overlay alone.
    own: Vec<String>,
    /// (key, summary) of each report call still going. A report that goes through shows as
    /// its summary, the one line of it the transcript prints.
    reports: Vec<(String, String)>,
    /// The report summary shown already, so a report both called and written out shows once.
    reported: Option<String>,
    /// Finished items not printed yet, under the agent's header.
    outbox: ToolCall,
    printed: bool,
    streaming: String,
    last_message: Option<String>,
    preview: Option<DiffView>,
    todos: Vec<Todo>,
    retry: Option<Retry>,
    retries: u32,
    /// The last line a mod pushed here, so a mod that says the same on every tool call shows
    /// it once.
    last_push: Option<String>,
    /// Keys of calls that started a workflow the CLI runs in the background: each row stays
    /// live until its workflow ends, then prints once.
    workflows_running: Vec<String>,
    /// Workflow calls whose script hasn't reported yet, so nothing of them shows: the call,
    /// the agent it was made under, and what it was given.
    workflows_unseen: Vec<(String, Option<String>, serde_json::Value)>,
    /// Its role, so a reviewer or a sign-off ends with its verdict.
    role: Option<argonauts_core::names::RoleName>,
    /// What it was given to judge.
    given: reviewed::Given,
}

#[derive(Debug, Clone, Copy)]
struct Retry {
    at: Option<Instant>,
    try_number: u32,
}

impl AgentBlocks {
    pub fn on_event(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        match event {
            AppEvent::Run { event } => self.on_run_event(event, cx),
            AppEvent::Attached { summary, .. } => self.drop_run(&summary.run, cx),
            AppEvent::Detached { run } => self.drop_run(run, cx),
            AppEvent::ReplayDone { run, .. } => self.after_replay(run, cx),
            _ => {}
        }
    }

    fn on_run_event(&mut self, event: &RunEvent, cx: &mut Cx<'_>) {
        self.latest = Some(self.latest.map_or(event.ts, |latest| latest.max(event.ts)));
        if !self.started.takes(event) {
            self.print_started(cx);
        }
        match &event.kind {
            RunEventKind::Session(session) => self.on_session(event, session, cx),
            RunEventKind::Agent(agent) => self.on_agent(event, agent, cx),
            RunEventKind::Check(outcome) => {
                if let Some(task) = &event.task_id {
                    let passed = self
                        .checks
                        .entry((event.run_id.clone(), task.clone()))
                        .or_insert(true);
                    *passed &= outcome.passed;
                }
            }
            RunEventKind::Run(RunLifecycle::Finished { .. }) => self.drop_run(&event.run_id, cx),
            _ => {}
        }
    }

    fn on_session(&mut self, event: &RunEvent, session: &SessionEvent, cx: &mut Cx<'_>) {
        let Some(key) = &event.session else {
            return;
        };
        match session {
            SessionEvent::Started(started) => {
                // A start sent again, as a replay does, begins the block afresh.
                if let Some(index) = self.index(&event.run_id, key) {
                    self.remove(index, cx);
                }
                let task = event.task_id.as_ref();
                if writes_the_closing(started.step)
                    || asked_inside_a_cli_workflow(
                        &event.run_id,
                        started.role,
                        started.step,
                        task,
                        cx,
                    )
                {
                    return;
                }
                let live = Live::new(
                    Place {
                        run: &event.run_id,
                        key,
                        task: event.task_id.as_ref(),
                        at: event.ts,
                    },
                    &started.worker,
                    &started.agent_id.0,
                    Named {
                        label: started
                            .label
                            .as_deref()
                            .map(str::trim)
                            .filter(|label| !label.is_empty())
                            .map(str::to_owned),
                        task: task_label(started.role, started.step, event.task_id.as_ref()),
                    },
                    cx,
                );
                let mut live = live;
                // The judge settling reviewers who disagree, or deciding an approval, doesn't
                // sign off on the change, so its row keeps the usual summary.
                live.role =
                    (!matches!(started.step, Some(StepName::Settle | StepName::ModelJudge)))
                        .then_some(started.role);
                self.started.add(live.started_row(), event.ts);
                self.sessions.push(live);
            }
            SessionEvent::Input(input) => {
                if let Some(index) = self.index(&event.run_id, key) {
                    self.sessions[index].given.add(&input.parts);
                }
            }
            SessionEvent::FileDiff(diff) => {
                if let Some(index) = self.find(event, cx) {
                    self.sessions[index].file_diff(diff);
                    self.sessions[index].refresh(cx);
                }
            }
            SessionEvent::Ended(end) => {
                if let Some(index) = self.find(event, cx) {
                    self.end(index, end, cx);
                }
            }
            _ => {}
        }
    }

    fn on_agent(&mut self, event: &RunEvent, agent: &AgentEvent, cx: &mut Cx<'_>) {
        let Some(index) = self.find(event, cx) else {
            return;
        };
        let live = &mut self.sessions[index];
        live.heard = cx.now;
        live.heard_at = event.ts;
        if !matches!(agent, AgentEvent::Retry { .. }) {
            live.retry = None;
            live.retries = 0;
        }
        match agent {
            AgentEvent::TextDelta { text, parent: None } => live.streaming.push_str(text),
            AgentEvent::Message { text, parent: None } => live.message(text, cx),
            AgentEvent::Message {
                text,
                parent: Some(parent),
            } => {
                if let Some(call) = live.whole.child_mut(parent) {
                    set_message(call, text);
                }
            }
            // A call shows as the model starts writing it, and fills in where it stands. A
            // workflow's call waits for its script to exist and report.
            AgentEvent::ToolPending { id, name, parent }
                if !tools::is_own(name)
                    && !tools::is_tool_lookup(name)
                    && !cli_workflow_run::is_call(&live.agent_id, name) =>
            {
                let call = tools::writing_call(&live.agent_id, name).with_key(id.clone());
                match parent.as_deref().and_then(|p| live.whole.child_mut(p)) {
                    Some(worker) => worker.push_call(call),
                    None => live.whole.push_call(call),
                }
            }
            AgentEvent::ToolDropped { id, .. } => {
                live.whole.drop_child(id);
            }
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => {
                // Tool lookups stay out of the block too, as Claude Code leaves them out.
                if tools::is_own(name) || tools::is_tool_lookup(name) {
                    live.own.push(id.clone());
                }
                if let Some(summary) = tools::report_summary(name, input) {
                    live.reports.push((id.clone(), summary));
                }
                // A workflow's call draws nothing until its script exists and reports.
                if cli_workflow_run::is_call(&live.agent_id, name) {
                    live.workflows_unseen
                        .push((id.clone(), parent.clone(), input.clone()));
                } else {
                    let call = tools::start_call(&live.agent_id, name, input).with_key(id.clone());
                    // A shell call shares the fold line beside it only when it just looks around.
                    let alone = call.kind == ToolKind::Shell && !tools::only_looks(&call);
                    let written = live
                        .whole
                        .child_mut(id)
                        .is_some_and(|known| known.state == ToolState::Writing);
                    let unfolded = (written && alone)
                        .then(|| live.whole.take_from_fold(id))
                        .flatten();
                    if let Some(mut taken) = unfolded {
                        taken.fill_in(call);
                        live.whole.push_alone(taken);
                    } else if written {
                        if let Some(written) = live.whole.child_mut(id) {
                            written.fill_in(call);
                        }
                    } else {
                        match parent.as_deref().and_then(|p| live.whole.child_mut(p)) {
                            Some(worker) => worker.push_call(call),
                            None if alone => live.whole.push_alone(call),
                            None => live.whole.push_call(call),
                        }
                    }
                }
            }
            AgentEvent::ToolFinished { id, ok, output, .. } => {
                live.tool_finished(id, *ok, output.as_deref(), cx)
            }
            AgentEvent::FilePatch {
                id, path, hunks, ..
            } => {
                if let Some(call) = live.whole.child_mut(id) {
                    tools::take_patch(call, path, hunks);
                }
            }
            AgentEvent::Todo { items } => live.todos = items.iter().map(todo).collect(),
            AgentEvent::Retry { delay_ms, .. } => {
                live.retries += 1;
                let try_number = cx
                    .state
                    .session(&live.run, &live.key)
                    .and_then(|session| session.retry.as_ref())
                    .map_or(live.retries, |retry| retry.try_number);
                let at = delay_ms.map(|ms| cx.now + Duration::from_millis(ms));
                live.retry = Some(Retry { at, try_number });
            }
            AgentEvent::Subagent { update } => live.subagent(update, cx),
            AgentEvent::Workflow { workflow } => live.cli_workflow_run(workflow, cx),
            AgentEvent::Error {
                category, message, ..
            } => {
                // A stall may yet pass, so it reads as a note. Any other error shows in the
                // live block in words; the row the session ends with says it in scrollback, so
                // it prints once.
                if *category == ErrorCategory::Stalled {
                    let item = ToolItem::Text {
                        text: message.clone(),
                        tone: Tone::Dim,
                    };
                    live.note(item, Verbosity::Normal, cx);
                } else {
                    let cli = cx.state.names.cli(&live.agent_id);
                    live.whole.push(ToolItem::Text {
                        text: failure::plain(&cli, message),
                        tone: Tone::Error,
                    });
                }
            }
            AgentEvent::Unknown { raw } => {
                let item = ToolItem::Text {
                    text: unknown_line(raw),
                    tone: Tone::Dim,
                };
                live.note(item, Verbosity::Full, cx);
            }
            AgentEvent::Init { .. } => {
                let before = live.started_row();
                if live.rename(cx) {
                    self.started.rename(&before, live.started_row());
                }
            }
            // What a mod pushes, and the CLI's notices, go in the agent's block, a repeat once.
            AgentEvent::PluginOutput {
                kind, plugin, text, ..
            } => {
                let line = plugin_output::in_block(*kind, plugin.as_deref(), text.as_deref(), cx);
                if let Some(line) = line
                    && live.last_push.as_ref() != Some(&line.text)
                {
                    live.last_push = Some(line.text.clone());
                    let item = ToolItem::Text {
                        text: line.text,
                        tone: line.tone,
                    };
                    live.note(item, Verbosity::Normal, cx);
                }
            }
            _ => {}
        }
        live.refresh(cx);
    }

    /// The block leaves the live region, and scrollback gets the collapsed line: whole in the
    /// transcript overlay when nothing of it printed before. A replayed one is only kept for
    /// the overlay.
    fn end(&mut self, index: usize, end: &SessionEnd, cx: &mut Cx<'_>) {
        let mut live = self.remove(index, cx);
        let state = end_state(&end.reason);
        live.whole.stop();
        live.whole.state = state;
        // A workflow its session outlived no longer runs: its row prints as it stopped.
        for key in std::mem::take(&mut live.workflows_running) {
            if let Some(call) = live.whole.child_mut(&key).map(|call| call.clone()) {
                live.send_out(call, cx);
            }
        }
        let checks = live
            .task
            .as_ref()
            .and_then(|task| self.checks.get(&(live.run.clone(), task.clone())).copied())
            .or_else(|| checks_from_task(live.task_state(cx)));
        let cli = cx.state.names.cli(&live.agent_id);
        let decided = decided_alone(end, on_autopilot(cx, &live.run));
        let mut detail = end_detail(end, checks, decided.as_deref(), &cli, glyphs(cx));
        let raw = raw_error(&end.reason).and_then(|text| {
            let title = format!("{} · the raw error", live.head.title.plain());
            failure::RawFailure::behind(&detail, title, text)
        });
        // A replayed row is only kept for the overlay, where no key opens what it hints at.
        if raw.is_some() && !cx.replaying() {
            detail.push_str(" · ");
            detail.push_str(failure::RAW_ERROR_HINT);
        }
        let mut summary = live.whole.summarized(Some(&detail));
        // A verdict says what the agent read, so what it held back stays for the overlay.
        let verdict = raw
            .is_none()
            .then(|| reviewed::row(live.role, &live.given, &live.whole, end))
            .flatten();
        let held_back = verdict.is_some();
        if let Some(mut row) = verdict {
            self.rounds.count(&live.run, &mut row);
            summary = row;
        } else if state == ToolState::Failed {
            let hint = (raw.is_some() && !cx.replaying()).then_some(failure::RAW_ERROR_HINT);
            summary = ended::failed_row(
                &live.whole,
                reason_words(&end.reason, &cli),
                hint,
                Duration::from_millis(end.duration_ms),
            );
        }
        if cx.verbosity() == Verbosity::Quiet {
            return;
        }
        if cx.replaying() {
            cx.model.history.keep(summary);
            return;
        }
        if raw.is_some() {
            cx.model.raw_failure = raw;
        }
        if state != ToolState::Stopped && !held_back {
            live.flush(cx);
        } else if live.printed && !held_back {
            // A stopped agent prints one line, and what it held back stays for the overlay.
            summary.full = (!live.outbox.body.is_empty())
                .then(|| Box::new(live.outbox.clone().with_state(ToolState::Stopped)));
        }
        if live.printed && state != ToolState::Stopped {
            summary.full = None;
        }
        // What printed left out Argonauts Code's own calls, so the overlay gets them here.
        if live.printed
            && let Some(own) = live.own_calls()
        {
            cx.model.history.keep(own);
        }
        cx.model.history.push(summary);
    }

    fn print_started(&mut self, cx: &mut Cx<'_>) {
        let agents = std::mem::take(&mut self.started.agents);
        self.started.since = None;
        if agents.len() > 1 && !cx.replaying() && cx.verbosity() != Verbosity::Quiet {
            cx.model.history.push(AgentsStarted::new(agents));
        }
    }

    /// Replayed events carry old times, so each session's clock is set again from the newest.
    fn after_replay(&mut self, run: &RunId, cx: &mut Cx<'_>) {
        self.started = StartedGroup::default();
        let latest = self.latest;
        for live in self.sessions.iter_mut().filter(|live| &live.run == run) {
            if let Some(latest) = latest {
                live.started = back_from(cx.now, latest - live.started_at);
                live.heard = back_from(cx.now, latest - live.heard_at);
            }
            live.refresh(cx);
        }
    }

    fn drop_run(&mut self, run: &RunId, cx: &mut Cx<'_>) {
        self.started = StartedGroup::default();
        while let Some(index) = self.sessions.iter().position(|live| &live.run == run) {
            self.remove(index, cx);
        }
    }

    fn index(&self, run: &RunId, key: &SessionKey) -> Option<usize> {
        self.sessions
            .iter()
            .position(|live| &live.run == run && &live.key == key)
    }

    /// The event's session, taken up from the app's state when its start never came, as after
    /// attaching to a long run.
    fn find(&mut self, event: &RunEvent, cx: &mut Cx<'_>) -> Option<usize> {
        let key = event.session.as_ref()?;
        if let Some(index) = self.index(&event.run_id, key) {
            return Some(index);
        }
        let session = cx
            .state
            .session(&event.run_id, key)
            .filter(|session| session.is_running() && !writes_the_closing(session.step))
            .filter(|session| {
                let task = session.task.as_ref().or(event.task_id.as_ref());
                !asked_inside_a_cli_workflow(&event.run_id, session.role, session.step, task, cx)
            })?;
        let (worker, agent_id) = (session.worker.clone(), session.agent_id.clone());
        let task = session.task.clone().or_else(|| event.task_id.clone());
        let named = Named {
            label: session.label.clone(),
            task: task_label(session.role, session.step, task.as_ref()),
        };
        let live = Live::new(
            Place {
                run: &event.run_id,
                key,
                task: task.as_ref(),
                at: event.ts,
            },
            &worker,
            &agent_id,
            named,
            cx,
        );
        self.sessions.push(live);
        Some(self.sessions.len() - 1)
    }

    fn remove(&mut self, index: usize, cx: &mut Cx<'_>) -> Live {
        let live = self.sessions.remove(index);
        if let Some(id) = live.cell {
            cx.model.history.remove_active(id);
        }
        live
    }
}

impl StartedGroup {
    /// Whether the event belongs with the starts gathered so far rather than ending them. What
    /// Argonauts Code does between starts, such as making the next task, passes through; the
    /// first thing an agent prints ends them.
    fn takes(&mut self, event: &RunEvent) -> bool {
        let close = self
            .since
            .is_none_or(|since| event.ts - since <= STARTED_TOGETHER);
        close && !prints(&event.kind)
    }

    fn add(&mut self, agent: StartedAgent, at: OffsetDateTime) {
        self.since.get_or_insert(at);
        self.agents.push(agent);
    }

    /// An agent not printed yet takes the name it got since it started.
    fn rename(&mut self, before: &StartedAgent, after: StartedAgent) {
        if let Some(agent) = self.agents.iter_mut().find(|agent| **agent == *before) {
            *agent = after;
        }
    }
}

/// The session that writes a crew run's closing answer prints nothing of its own: the answer
/// itself ends the run.
fn writes_the_closing(step: Option<StepName>) -> bool {
    step == Some(StepName::CloseRun)
}

/// A reviewer or judge that a CLI's own workflow asked through Argonauts Code while it runs: its
/// answer belongs to the workflow's row and view, not to the conversation. It is tied to no task
/// and no step of its own.
fn asked_inside_a_cli_workflow(
    run: &RunId,
    role: RoleName,
    step: Option<StepName>,
    task: Option<&TaskId>,
    cx: &Cx<'_>,
) -> bool {
    let asked = matches!(role, RoleName::Reviewer | RoleName::Judge) && step.is_none();
    asked
        && task.is_none()
        && cx.state.runs.get(run).is_some_and(|run| {
            run.sessions_in_order()
                .any(|session| session.workflows.iter().any(|workflow| !workflow.ended()))
        })
}

/// Agent output and a session's end go to scrollback, so the started tree goes before them.
fn prints(kind: &RunEventKind) -> bool {
    match kind {
        RunEventKind::Agent(agent) => matches!(
            agent,
            AgentEvent::TextDelta { .. }
                | AgentEvent::Message { .. }
                | AgentEvent::ToolStarted { .. }
                | AgentEvent::ToolFinished { .. }
                | AgentEvent::Subagent { .. }
                | AgentEvent::Error { .. }
                | AgentEvent::Unknown { .. }
        ),
        RunEventKind::Session(session) => matches!(session, SessionEvent::Ended(_)),
        _ => false,
    }
}

/// Where a session sits: its run, key, task, and when it began.
/// What an agent's block is headed by: its label, `● fix:slugify · Haiku 4.5 on Claude Code`,
/// or for a log from before labels, its role and task after its name.
struct Named {
    label: Option<String>,
    task: String,
}

struct Place<'a> {
    run: &'a RunId,
    key: &'a SessionKey,
    task: Option<&'a TaskId>,
    at: OffsetDateTime,
}

impl Live {
    fn new(
        place: Place<'_>,
        worker: &ModelRef,
        agent_id: &str,
        named: Named,
        cx: &mut Cx<'_>,
    ) -> Self {
        let worker = &cx.state.names.running(worker);
        let color = cx.model.agent_colors.color_for(&worker.to_string());
        let title = place.task.and_then(|task| {
            cx.state
                .runs
                .get(place.run)
                .and_then(|run| run.tasks.get(task))
                .map(|row| row.title.clone())
        });
        // The transcript overlay is the detail view, so its stamp keeps the id.
        let started = format::ClockTime::from(place.at.to_offset(cx.state.clock_offset()));
        let agent = cx.state.names.worker(worker);
        let head = match &named.label {
            Some(label) => ToolCall::agent(label.clone(), Some(color), agent, String::new()),
            None => ToolCall::agent(agent, Some(color), named.task, title.unwrap_or_default()),
        }
        .with_key(place.key.to_string())
        .with_stamp(format!("{started} {worker}"));
        let mut live = Self {
            labeled: named.label.is_some(),
            run: place.run.clone(),
            key: place.key.clone(),
            agent_id: agent_id.to_owned(),
            task: place.task.cloned(),
            outbox: head.clone().with_state(ToolState::Done),
            whole: head.clone(),
            own: Vec::new(),
            workflows_running: Vec::new(),
            workflows_unseen: Vec::new(),
            reports: Vec::new(),
            reported: None,
            head,
            cell: None,
            started: cx.now,
            started_at: place.at,
            heard: cx.now,
            heard_at: place.at,
            printed: false,
            streaming: String::new(),
            last_message: None,
            preview: None,
            todos: Vec::new(),
            retry: None,
            last_push: None,
            retries: 0,
            role: None,
            given: reviewed::Given::default(),
        };
        // By default an agent at work shows in the list under the input with what it's doing
        // now, and the conversation gets it once, when it finishes.
        if as_they_work(cx) {
            let block = live.block(cx);
            live.cell = Some(cx.model.history.push_active(block));
        }
        live
    }

    /// Names the agent as its session now reads, such as by the model it reported at its
    /// start when its settings named none, so it has one name everywhere. Whether it changed.
    fn rename(&mut self, cx: &Cx<'_>) -> bool {
        let Some(session) = cx.state.session(&self.run, &self.key) else {
            return false;
        };
        let name = cx
            .state
            .names
            .worker(&cx.state.names.running(&session.worker));
        let mut changed = false;
        for call in [&mut self.head, &mut self.whole, &mut self.outbox] {
            // A labeled block names its agent after the label.
            let shown = match &mut call.title {
                ToolTitle::Agent { role, .. } if self.labeled => role,
                ToolTitle::Agent { name, .. } => name,
                _ => continue,
            };
            if *shown != name {
                shown.clone_from(&name);
                changed = true;
            }
        }
        changed
    }

    /// `├ GPT-6.1 Sol on Codex · worker · task-002 Redis store`
    fn started_row(&self) -> StartedAgent {
        let ToolTitle::Agent {
            name,
            color,
            role,
            task,
        } = &self.head.title
        else {
            return StartedAgent::new(self.head.title.plain(), None, "");
        };
        let detail = if task.is_empty() {
            role.clone()
        } else {
            format!("{role} {task}")
        };
        StartedAgent::new(name.clone(), *color, detail)
    }

    fn message(&mut self, text: &str, cx: &mut Cx<'_>) {
        self.streaming.clear();
        self.whole.push(ToolItem::Markdown(text.to_owned()));
        let shown = self.without_report(text);
        if shown.is_empty() {
            return;
        }
        self.last_message = Some(shown.clone());
        if self.prints(Verbosity::Normal, cx) {
            self.outbox.push(ToolItem::Markdown(shown));
            if as_they_work(cx) {
                self.flush(cx);
            }
        }
    }

    /// The message as the transcript shows it. A report or result the agent writes out at the
    /// end reads as the words before it and the report's summary; its JSON stays in Ctrl+O.
    fn without_report(&mut self, text: &str) -> String {
        let Some((mut shown, summary)) = tools::written_report(text) else {
            return text.to_owned();
        };
        if let Some(summary) = summary.filter(|summary| !self.said(summary)) {
            if !shown.is_empty() {
                shown.push_str("\n\n");
            }
            shown.push_str(&summary);
            self.reported = Some(summary);
        }
        shown
    }

    fn said(&self, summary: &str) -> bool {
        self.reported.as_deref() == Some(summary)
    }

    /// Argonauts Code's own calls under the agent's header, or `None` when it made none.
    fn own_calls(&self) -> Option<ToolCall> {
        let calls: Vec<ToolItem> = self
            .whole
            .body
            .iter()
            .filter(|item| {
                matches!(item, ToolItem::Call(call)
                    if call.key.as_ref().is_some_and(|key| self.own.contains(key)))
            })
            .cloned()
            .collect();
        if calls.is_empty() {
            return None;
        }
        let mut batch = self.head.clone().with_state(ToolState::Done);
        batch.body = calls;
        Some(batch)
    }

    fn tool_finished(&mut self, id: &str, ok: bool, output: Option<&str>, cx: &mut Cx<'_>) {
        if let Some(at) = self.workflows_unseen.iter().position(|(key, ..)| key == id) {
            // The call returns once the workflow is launched, and its row comes with its first
            // report. One that failed to start has no report to wait for, and says why now.
            if ok {
                if !self.workflows_running.iter().any(|key| key == id) {
                    self.workflows_running.push(id.to_owned());
                }
                return;
            }
            let (key, parent, input) = self.workflows_unseen.remove(at);
            let failed = cli_workflow_run::failed_start(&input, output).with_key(key);
            match parent.as_deref().and_then(|p| self.whole.child_mut(p)) {
                Some(worker) => worker.push_call(failed),
                None => self.send_out(failed, cx),
            }
            return;
        }
        let top = is_top_level(&self.whole, id);
        let Some(call) = self.whole.child_mut(id) else {
            return;
        };
        if !call.is_running() {
            return;
        }
        let starts_a_workflow = matches!(&call.title, ToolTitle::Call { name, .. }
            if cli_workflow_run::is_call(&self.agent_id, name));
        // The call returns once the workflow is launched; the row waits for its end.
        if ok && starts_a_workflow {
            if !self.workflows_running.iter().any(|key| key == id) {
                self.workflows_running.push(id.to_owned());
            }
            return;
        }
        tools::finish_call(call, ok, output);
        let done = call.clone();
        if let Some(at) = self.reports.iter().position(|(key, _)| key == id) {
            let (_, summary) = self.reports.remove(at);
            if ok && !self.said(&summary) {
                self.reported = Some(summary.clone());
                self.message(&summary, cx);
            }
        } else if top && !self.own.iter().any(|own| own == id) {
            self.send_out(done, cx);
        }
    }

    /// A workflow the CLI runs itself, on the row of the call that started it: its name and
    /// steps rather than the call's answer, which only the raw log keeps.
    fn cli_workflow_run(&mut self, workflow: &CliWorkflowRun, cx: &mut Cx<'_>) {
        let row = cli_workflow_run::row(workflow);
        // Its first report is where its row first shows, where its call was made.
        if let Some(key) = workflow.call.as_deref()
            && let Some(at) = self.workflows_unseen.iter().position(|(id, ..)| id == key)
        {
            let (_, parent, _) = self.workflows_unseen.remove(at);
            match parent.as_deref().and_then(|p| self.whole.child_mut(p)) {
                Some(worker) => worker.push_call(row.clone()),
                None => self.whole.push_call(row.clone()),
            }
        }
        let Some(call) = workflow
            .call
            .as_deref()
            .and_then(|key| self.whole.child_mut(key))
        else {
            return;
        };
        call.title = row.title;
        call.tail = row.tail;
        call.kind = row.kind;
        call.body = row.body;
        if workflow.ended() {
            self.workflow_ended(workflow, cx);
        }
    }

    /// Its workflow ended: the row that stayed live prints once, with its final phases.
    fn workflow_ended(&mut self, workflow: &CliWorkflowRun, cx: &mut Cx<'_>) {
        let Some(key) = workflow.call.as_deref() else {
            return;
        };
        let Some(at) = self.workflows_running.iter().position(|held| held == key) else {
            return;
        };
        self.workflows_running.remove(at);
        let top = is_top_level(&self.whole, key);
        let Some(call) = self.whole.child_mut(key) else {
            return;
        };
        call.finish(!matches!(workflow.status, SubagentStatus::Failed));
        let done = call.clone();
        if top {
            self.send_out(done, cx);
        }
    }

    fn subagent(&mut self, update: &SubagentUpdate, cx: &mut Cx<'_>) {
        if self.whole.child_mut(&update.id).is_none() {
            let name = match update.kind {
                SubagentKind::Subagent => "Task",
                SubagentKind::Workflow => "Workflow",
                SubagentKind::Thread => "Thread",
            };
            let args = update
                .label
                .clone()
                .or_else(|| update.prompt.as_deref().map(first_line))
                .unwrap_or_default();
            let call = ToolCall::call(ToolKind::Agent, name, args).with_key(update.id.clone());
            match update
                .parent
                .as_deref()
                .and_then(|p| self.whole.child_mut(p))
            {
                Some(worker) => worker.push_call(call),
                None => self.whole.push_call(call),
            }
        }
        let top = is_top_level(&self.whole, &update.id);
        let Some(call) = self.whole.child_mut(&update.id) else {
            return;
        };
        // Its call's row draws its steps from the workflow's own reports.
        let drawn = update.kind == SubagentKind::Workflow
            && matches!(&call.title, ToolTitle::Call { name, .. }
                if cli_workflow_run::is_call(&self.agent_id, name));
        if !drawn && update.steps_visible {
            if let Some(message) = &update.last_message {
                set_message(call, message);
            }
        } else if !drawn {
            let mut row = status_word(update.status).to_owned();
            if let Some(message) = update.last_message.as_deref().map(first_line) {
                row.push_str(" · ");
                row.push_str(&message);
            }
            call.body
                .retain(|item| !matches!(item, ToolItem::Text { .. } | ToolItem::Markdown(_)));
            call.push(ToolItem::text(row));
        }
        let ok = match update.status {
            SubagentStatus::Done => true,
            SubagentStatus::Failed => false,
            _ => return,
        };
        if call.is_running() {
            call.finish(ok);
            let done = call.clone();
            if top {
                self.send_out(done, cx);
            }
        }
    }

    /// At `full` each call goes out whole; at `normal` quiet ones fold into one line.
    fn send_out(&mut self, call: ToolCall, cx: &mut Cx<'_>) {
        if cx.replaying() {
            return;
        }
        match cx.verbosity() {
            Verbosity::Full => self.outbox.body.push(ToolItem::Call(call)),
            Verbosity::Normal => self.outbox.push_call(call),
            Verbosity::Quiet => return,
        }
        if self.outbox.body.len() >= OUTBOX_MAX && as_they_work(cx) {
            self.flush(cx);
        }
    }

    /// Something the agent said that isn't a call, kept at `least` verbosity and above.
    fn note(&mut self, item: ToolItem, least: Verbosity, cx: &mut Cx<'_>) {
        self.whole.push(item.clone());
        if self.prints(least, cx) {
            self.outbox.push(item);
        }
    }

    fn prints(&self, least: Verbosity, cx: &Cx<'_>) -> bool {
        let level = |verbosity| match verbosity {
            Verbosity::Quiet => 0,
            Verbosity::Normal => 1,
            Verbosity::Full => 2,
        };
        !cx.replaying() && level(cx.verbosity()) >= level(least)
    }

    fn flush(&mut self, cx: &mut Cx<'_>) {
        if self.outbox.body.is_empty() {
            return;
        }
        let batch = self.outbox.clone();
        self.outbox.body.clear();
        cx.model.history.push(batch);
        self.printed = true;
    }

    /// The live preview, and the diff a call that changed the file was missing, as Codex's
    /// `file_change` is.
    fn file_diff(&mut self, diff: &FileDiff) {
        let Ok(view) = DiffView::from_unified(&diff.hunk) else {
            return;
        };
        let view = view.updated(
            diff.path.clone(),
            usize::try_from(diff.added).unwrap_or(usize::MAX),
            usize::try_from(diff.removed).unwrap_or(usize::MAX),
        );
        if !attach_diff(&mut self.outbox, &diff.path, &view) {
            attach_diff(&mut self.whole, &diff.path, &view);
        }
        self.preview = Some(view);
    }

    fn task_state(&self, cx: &Cx<'_>) -> Option<TaskState> {
        let task = self.task.as_ref()?;
        cx.state.runs.get(&self.run)?.tasks.get(task)?.state
    }

    fn refresh(&self, cx: &mut Cx<'_>) {
        let Some(id) = self.cell else {
            return;
        };
        let block = self.block(cx);
        if let Some(Cell::Tool(cell)) = cx.model.history.active_mut(id) {
            *cell = block;
        }
    }

    /// Its header with time and tokens, the last few tool lines, a diff preview, one line of
    /// text, and the to-do line.
    fn block(&self, cx: &Cx<'_>) -> ToolCall {
        let tokens = cx
            .state
            .session(&self.run, &self.key)
            .map_or(0, |session| session.tokens());
        let mut block = self.head.clone().with_stats(ToolStats {
            started_at: Some(self.started),
            ended_at: None,
            tokens_down: tokens,
        });
        match self.retry {
            Some(retry) => block.retrying(retry.at, retry.try_number),
            None if retries_silently(&self.agent_id) => block.quiet_since(self.heard),
            None => {}
        }
        let calls: Vec<&ToolItem> = self
            .whole
            .body
            .iter()
            .filter(|item| match item {
                ToolItem::Markdown(_) => false,
                ToolItem::Call(call) => call.key.as_ref().is_none_or(|key| !self.own.contains(key)),
                _ => true,
            })
            .collect();
        let recent = &calls[calls.len().saturating_sub(LIVE_CALLS)..];
        block.body.extend(recent.iter().map(|item| (*item).clone()));
        // A call above that drew this file's change, from its own patch, shows it already.
        if let Some(preview) = self.preview.as_ref().filter(|preview| {
            !recent
                .iter()
                .any(|item| draws_diff_of(item, preview.file()))
        }) {
            block.body.push(ToolItem::Diff(preview.clone()));
        }
        let streaming = last_line(&self.streaming);
        if !streaming.is_empty() {
            block.body.push(ToolItem::Streaming(streaming.to_owned()));
        } else if let Some(message) = self.last_message.as_deref().map(last_line) {
            block.body.push(ToolItem::Text {
                text: message.to_owned(),
                tone: Tone::Normal,
            });
        }
        if !self.todos.is_empty() {
            block.body.push(ToolItem::TodoLine(self.todos.clone()));
        }
        block
    }
}

/// Whether agents show in the conversation as they work, in a live block that prints their rows as
/// they finish, rather than once each is done.
fn as_they_work(cx: &Cx<'_>) -> bool {
    cx.settings.agents_in_conversation == AgentsInConversation::AsTheyWork
}

/// agy waits out a server's retry delay and `opencode run` retries, both without a word, so
/// their headers say how long they've been quiet instead.
fn retries_silently(agent_id: &str) -> bool {
    matches!(agent_id, "antigravity-cli" | "opencode")
}

fn is_top_level(whole: &ToolCall, id: &str) -> bool {
    whole.body.iter().any(|item| match item {
        ToolItem::Call(call) => call.key.as_deref() == Some(id),
        ToolItem::Group(group) => group
            .calls
            .iter()
            .any(|call| call.key.as_deref() == Some(id)),
        _ => false,
    })
}

/// A subagent shows only its newest words.
fn set_message(call: &mut ToolCall, text: &str) {
    call.body
        .retain(|item| !matches!(item, ToolItem::Markdown(_)));
    call.push(ToolItem::Markdown(text.to_owned()));
}

/// Into the newest edit or write of `path` that has no diff yet.
/// A call that draws a diff of `file`, a path in the repo.
fn draws_diff_of(item: &ToolItem, file: Option<&str>) -> bool {
    let (ToolItem::Call(call), Some(file)) = (item, file) else {
        return false;
    };
    call.body.iter().any(|item| match item {
        ToolItem::Diff(diff) => diff
            .file()
            .is_some_and(|drawn| tools::same_file(drawn, file)),
        _ => false,
    })
}

fn attach_diff(worker: &mut ToolCall, path: &str, view: &DiffView) -> bool {
    let target = worker.body.iter_mut().rev().find_map(|item| match item {
        ToolItem::Call(call)
            if matches!(call.kind, ToolKind::Edit | ToolKind::Write)
                && matches!(&call.title, ToolTitle::Call { args, .. } if args == path)
                && !call
                    .body
                    .iter()
                    .any(|item| matches!(item, ToolItem::Diff(_))) =>
        {
            Some(call)
        }
        _ => None,
    });
    let Some(call) = target else {
        return false;
    };
    call.body
        .retain(|item| !matches!(item, ToolItem::Text { .. }));
    call.body.push(ToolItem::Diff(view.clone()));
    true
}

fn todo(item: &TodoItem) -> Todo {
    Todo {
        text: item.text.clone(),
        state: match item.status {
            TodoStatus::Pending => TodoState::Pending,
            TodoStatus::InProgress => TodoState::InProgress,
            TodoStatus::Completed => TodoState::Done,
        },
    }
}

fn status_word(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Started | SubagentStatus::Running | SubagentStatus::Unknown => "Running",
        SubagentStatus::Done => "Done",
        SubagentStatus::Failed => "Failed",
    }
}

/// `? unknown event (system/hook_response)`, with the CLI's own type.
fn unknown_line(raw: &Value) -> String {
    let field = |key: &str| raw.get(key).and_then(Value::as_str);
    let kind = match (raw, field("type")) {
        (Value::String(_), _) => "not JSON".to_owned(),
        (_, Some(kind)) => match field("subtype") {
            Some(subtype) => format!("{kind}/{subtype}"),
            None => kind.to_owned(),
        },
        _ => "no type".to_owned(),
    };
    let kind: String = kind.chars().take(UNKNOWN_TYPE_MAX).collect();
    format!("? unknown event ({kind})")
}

/// A stop, a pause, or a limit keeps the work so far, so it reads as stopped, not failed.
fn end_state(reason: &EndReason) -> ToolState {
    match reason {
        EndReason::Completed => ToolState::Done,
        EndReason::Stopped
        | EndReason::Paused
        | EndReason::LimitHit { .. }
        | EndReason::UsageLimit { .. }
        | EndReason::RateLimited
        | EndReason::ContextFull => ToolState::Stopped,
        _ => ToolState::Failed,
    }
}

fn checks_from_task(state: Option<TaskState>) -> Option<bool> {
    match state? {
        TaskState::InReview | TaskState::Merging | TaskState::Merged => Some(true),
        _ => None,
    }
}

fn glyphs(cx: &Cx<'_>) -> &'static Glyphs {
    match cx.settings.glyphs {
        GlyphChoice::Ascii => &Glyphs::ASCII,
        GlyphChoice::Unicode => &Glyphs::UNICODE,
    }
}

/// Whether the run decides everything in your place: its own mode once Shift+Tab gave it one,
/// else what your settings say.
fn on_autopilot(cx: &Cx<'_>, run: &RunId) -> bool {
    let own = cx.state.runs.get(run).and_then(|run| run.approval);
    let mode = own.or_else(|| cx.state.status.as_ref().map(|status| status.approval_mode));
    mode == Some(ApprovalMode::Autopilot)
}

/// `decided 1 thing on autopilot`: how many things the agent's report says it decided where it
/// would have asked, which its concerns mark `Assumed:`. Off autopilot they read as assumed,
/// since the report comes to you.
fn decided_alone(end: &SessionEnd, on_autopilot: bool) -> Option<String> {
    let concerns = end.structured.as_ref()?.get("concerns")?.as_array()?;
    let count = concerns
        .iter()
        .filter_map(Value::as_str)
        .flat_map(str::lines)
        .filter_map(assumption)
        .count();
    let things = match count {
        0 => return None,
        1 => "1 thing".to_owned(),
        more => format!("{more} things"),
    };
    Some(if on_autopilot {
        format!("decided {things} on autopilot")
    } else {
        format!("assumed {things}")
    })
}

/// `report ✓ · checks ✓ · 9m 40s · 61k tokens`, or why it ended first. What the agent decided
/// in your place comes right before how long it took.
fn end_detail(
    end: &SessionEnd,
    checks: Option<bool>,
    decided: Option<&str>,
    cli: &str,
    glyphs: &Glyphs,
) -> String {
    let mut parts = Vec::new();
    if let Some(reason) = reason_words(&end.reason, cli) {
        parts.push(reason);
    }
    let reported = end.structured.is_some() || end.result_text.is_some();
    if end.reason == EndReason::Completed && reported {
        parts.push(format!("report {}", glyphs.ready));
    }
    if let Some(passed) = checks {
        let mark = if passed {
            glyphs.ready
        } else {
            glyphs.not_ready
        };
        parts.push(format!("checks {mark}"));
    }
    parts.extend(decided.map(str::to_owned));
    parts.push(format::duration(Duration::from_millis(end.duration_ms)));
    let tokens = end.spend.input_tokens + end.spend.output_tokens;
    if tokens > 0 {
        parts.push(format!("{} tokens", format::tokens(tokens)));
    }
    parts.join(" · ")
}

/// Why it ended, in words. A failure the CLI explained is one plain line; its own text stays
/// in the raw log.
pub(crate) fn reason_words(reason: &EndReason, cli: &str) -> Option<String> {
    Some(match reason {
        EndReason::Completed | EndReason::Stopped => return None,
        EndReason::Paused => "paused".to_owned(),
        EndReason::LimitHit { .. } => "limit reached".to_owned(),
        EndReason::Stuck {
            watchdog: Watchdog::Inactivity,
        } => "stuck, no activity".to_owned(),
        EndReason::Stuck {
            watchdog: Watchdog::Repeat,
        } => "stuck, repeating a call".to_owned(),
        EndReason::UsageLimit { .. } => "usage limit".to_owned(),
        EndReason::RateLimited => "rate limited".to_owned(),
        EndReason::ContextFull => "context full".to_owned(),
        EndReason::ModelUnavailable { model } => format!("{model} unavailable"),
        EndReason::CliError { message, .. }
        | EndReason::Auth { message }
        | EndReason::Billing { message }
        | EndReason::StartFailed { message }
        | EndReason::Crashed { message } => failure::plain(cli, message),
        EndReason::AdapterBroken { detail } => first_line(detail),
        EndReason::IsolationBroken { plugins, skills } => {
            format!("loaded {}", loaded_words(plugins, skills))
        }
    })
}

/// The CLI's own text behind a failure [`reason_words`] says in words.
fn raw_error(reason: &EndReason) -> Option<&str> {
    match reason {
        EndReason::CliError { message, .. }
        | EndReason::Auth { message }
        | EndReason::Billing { message }
        | EndReason::StartFailed { message }
        | EndReason::Crashed { message } => Some(message),
        EndReason::AdapterBroken { detail } => Some(detail),
        _ => None,
    }
}

fn first_line(text: &str) -> String {
    lines_of(text).next().unwrap_or("").to_owned()
}

fn last_line(text: &str) -> &str {
    lines_of(text).next_back().unwrap_or("")
}

fn lines_of(text: &str) -> impl DoubleEndedIterator<Item = &str> {
    text.lines().map(str::trim).filter(|line| !line.is_empty())
}

/// `now` less `offset`, or `now` for a negative offset or one past the clock's start.
fn back_from(now: Instant, offset: time::Duration) -> Instant {
    Duration::try_from(offset)
        .ok()
        .and_then(|offset| now.checked_sub(offset))
        .unwrap_or(now)
}
