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

use argonauts_core::agents::session_runner::Access;
use argonauts_core::app::AppEvent;
use argonauts_core::app::views::writes_the_message;
use argonauts_core::base::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::look::logos::logo_for;
use argonauts_core::runs::events::run::RunLifecycle;
use argonauts_core::runs::events::session::{
    CliWorkflowRun, EndReason, FileDiff, SessionEnd, SessionEvent, SubagentKind, SubagentStatus,
    SubagentUpdate, Watchdog, loaded_words,
};
use argonauts_core::runs::events::task::TaskState;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::ui::{AgentsInConversation, Glyphs as GlyphChoice};
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::steps::task::assumption;
use argonauts_core::{AgentEvent, ErrorCategory, RunId, TaskId, TodoItem, TodoStatus, Verbosity};
use serde_json::Value;
use time::OffsetDateTime;

use super::{Cx, cli_workflow_run, ended, failure, gave, plugin_output, reviewed, tools};
use crate::base::format;
use crate::base::theme::Glyphs;
use crate::base::theme::shades::ModelColors;
use crate::cells::cell::history::CellId;
use crate::cells::cell::{
    AgentsStarted, Cell, DiffView, ModelShade, StartedAgent, Todo, TodoState, Tone, ToolCall,
    ToolItem, ToolKind, ToolState, ToolStats, ToolTitle,
};
use crate::frame::state::{AppState, RunState, task_label};

/// The built-in workflow whose agents propose, challenge and decide.
const COUNCIL: &str = "council";

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
    /// What agents of a run printed before its workflow's shape was known, which prints once
    /// its Workflow line has, as each cell and whether it prints or is only kept.
    unshaped: Vec<(RunId, Vec<(Cell, bool)>)>,
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
    /// The merge's commit message writer, stopped once your answer to the merge comes first.
    writes_the_message: bool,
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
    role: Option<argonauts_core::base::names::RoleName>,
    /// It may only read, as a council's proposals and challenges do.
    reads_only: bool,
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
            // A run whose past the screen drew already keeps the blocks it drew, at work or not:
            // the stream that follows repeats none of what made them.
            AppEvent::Attached { summary, .. } if !cx.state.seen.contains_key(&summary.run) => {
                self.drop_run(&summary.run, cx)
            }
            AppEvent::Detached { run } => self.drop_run(run, cx),
            AppEvent::ReplayDone { run, .. } => self.after_replay(run, cx),
            _ => {}
        }
    }

    fn on_run_event(&mut self, event: &RunEvent, cx: &mut Cx<'_>) {
        self.latest = Some(self.latest.map_or(event.ts, |latest| latest.max(event.ts)));
        self.print_unshaped(event, cx);
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
                // A seat's work shows in its bubble and on its row under the request, so the
                // conversation prints no agent rows for it; the run's own view keeps them.
                if started.seat.is_some()
                    || judges_a_council(&event.run_id, started.role, cx)
                    || writes_the_closing(started.step)
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
                        step: started.step,
                    },
                    cx,
                );
                let mut live = live;
                // The judge settling reviewers who disagree, or deciding an approval, doesn't
                // sign off on the change, so its row keeps the usual summary.
                live.role =
                    (!matches!(started.step, Some(StepName::Settle | StepName::ModelJudge)))
                        .then_some(started.role);
                live.reads_only = started.access == Access::ReadOnly;
                self.started.add(live.started_row(), event.ts);
                self.sessions.push(live);
            }
            SessionEvent::Input(input) => {
                if let Some(index) = self.index(&event.run_id, key) {
                    let live = &mut self.sessions[index];
                    live.given.add(&input.parts);
                    // Named now by the model its command launched.
                    let before = live.started_row();
                    if live.rename(cx) {
                        self.started.rename(&before, live.started_row());
                    }
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
        // Once stopped, nothing waits for the writer's message, and the change lands with the
        // one plain code writes, so it leaves no row.
        if live.writes_the_message && end.reason == EndReason::Stopped && !live.printed {
            let row = live.started_row();
            self.started.agents.retain(|agent| *agent != row);
            return;
        }
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
        let decided = decided_alone(end, on_autopilot(cx, &live.run));
        let who = gave::Who {
            role: live.role,
            reads_only: live.reads_only,
            has_task: live.task.is_some(),
            in_council: cx
                .state
                .runs
                .get(&live.run)
                .is_some_and(|run| run.workflow.as_deref() == Some(COUNCIL)),
        };
        let gave = (end.reason == EndReason::Completed)
            .then(|| gave::of(who, end.structured.as_ref()))
            .flatten();
        let mut detail = end_detail(
            end,
            checks,
            decided.as_deref(),
            gave.as_ref().and_then(gave::Gave::report_word),
            (&live.agent_id, &cx.state.names),
            glyphs(cx),
        );
        let raw = raw_error(&end.reason).and_then(|text| {
            let title = format!("{} · the raw error", live.head.title.plain());
            failure::RawFailure::behind(&detail, title, text)
        });
        // A replayed row is only kept for the overlay, where no key opens what it hints at.
        if raw.is_some() && !cx.replaying() {
            detail.push_str(" · ");
            detail.push_str(failure::RAW_ERROR_HINT);
        }
        let mut summary = live
            .whole
            .summarized_with(&spent_and_took(end), Some(&detail));
        if let Some(line) = gave
            .as_ref()
            .and_then(|gave| gave.line(live.whole.tool_uses(), &took(end)))
        {
            summary.body = vec![ToolItem::text(line)];
        }
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
                reason_words(&end.reason, &live.agent_id, &cx.state.names),
                hint,
                Duration::from_millis(end.duration_ms),
            );
        }
        if cx.verbosity() == Verbosity::Quiet {
            return;
        }
        if cx.history_only() {
            cx.model.history.keep(summary);
            return;
        }
        // `r` opens the newest raw failure, which a past drawn again isn't.
        if raw.is_some() && !cx.replaying() {
            cx.model.raw_failure = raw;
        }
        let mut out: Vec<(Cell, bool)> = Vec::new();
        if state != ToolState::Stopped && !held_back {
            // One row for the agent, as Claude Code draws a finished subagent: the tool lines
            // it held back above its Done line, under its one header.
            if let Some(batch) = live.take_batch() {
                summary.body.splice(0..0, batch.body);
            }
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
            out.push((own.into(), false));
        }
        out.push((summary.into(), true));
        // Nothing of a workflow draws before its shape is known, so an agent that ends sooner,
        // as the one that writes it does, prints after the Workflow line.
        if cx
            .state
            .runs
            .get(&live.run)
            .is_some_and(RunState::awaits_its_shape)
        {
            self.unshaped.push((live.run.clone(), out));
            return;
        }
        print_all(out, cx);
    }

    /// What the run's agents printed before its shape was known, once it is, or once the run
    /// waits or ends without one.
    fn print_unshaped(&mut self, event: &RunEvent, cx: &mut Cx<'_>) {
        if self.unshaped.is_empty() {
            return;
        }
        let ends = matches!(
            &event.kind,
            RunEventKind::Run(
                RunLifecycle::Finished { .. }
                    | RunLifecycle::Waiting { .. }
                    | RunLifecycle::Paused { .. }
            )
        );
        let shaped = !cx
            .state
            .runs
            .get(&event.run_id)
            .is_some_and(RunState::awaits_its_shape);
        if !(ends || shaped) {
            return;
        }
        let (now, later) = std::mem::take(&mut self.unshaped)
            .into_iter()
            .partition(|(run, _)| *run == event.run_id);
        self.unshaped = later;
        for (_, out) in now {
            print_all(out, cx);
        }
    }

    fn print_started(&mut self, cx: &mut Cx<'_>) {
        let agents = std::mem::take(&mut self.started.agents);
        self.started.since = None;
        if agents.len() > 1 && !cx.history_only() && cx.verbosity() != Verbosity::Quiet {
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
    /// attaching to a long run. A council seat's session is never taken up: its start left it
    /// out on purpose, since its work shows in its bubble and its row under the request.
    fn find(&mut self, event: &RunEvent, cx: &mut Cx<'_>) -> Option<usize> {
        let key = event.session.as_ref()?;
        if let Some(index) = self.index(&event.run_id, key) {
            return Some(index);
        }
        let session = cx
            .state
            .session(&event.run_id, key)
            .filter(|session| session.seat.is_none())
            .filter(|session| !judges_a_council(&event.run_id, session.role, cx))
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
            step: session.step,
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
/// The judge that settles a talking council's question or plan for you in auto mode. The
/// question's row says who decided, so the talk prints no agent rows for it, as for a seat; a
/// council that prints its agents as rows keeps its judges' rows.
fn judges_a_council(run: &RunId, role: RoleName, cx: &Cx<'_>) -> bool {
    role == RoleName::Judge && cx.state.runs.get(run).is_some_and(|run| run.talk.is_some())
}

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

/// What an agent's block is headed by: its label and the task it works, `● fix:slugify(Slug
/// the titles) · Haiku 4.5 on Claude Code`, or for a log from before labels, its role and task
/// after its name.
struct Named {
    label: Option<String>,
    task: String,
    /// The step it works, which says whether it writes the commit message.
    step: Option<StepName>,
}

/// What an agent's parentheses say where its step isn't the task's own work: a planner plans
/// the work, and a reviewer or judge reviews the task its id names last, `review the flag`
/// for `due-flag`. `None` keeps the task's title.
fn doing(step: Option<StepName>, task: Option<&TaskId>) -> Option<String> {
    match step? {
        StepName::Orchestrate => Some("plan the work".to_owned()),
        StepName::Review | StepName::Judge => {
            let last = task?.as_str().rsplit('-').next().unwrap_or_default();
            // A numbered id such as `task-002` names nothing to review.
            let named = !last.is_empty() && !last.chars().all(|c| c.is_ascii_digit());
            Some(format!("review the {}", if named { last } else { "task" }))
        }
        _ => None,
    }
}

/// Where a session sits: its run, key, task, and when it began.
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
        let title = doing(named.step, place.task).or(title);
        // The transcript overlay is the detail view, so its stamp keeps the id.
        let started = format::ClockTime::from(place.at.to_offset(cx.state.clock_offset()));
        let agent = cx.state.names.worker(worker);
        let head = match &named.label {
            // A label heads it as Claude Code heads its agents, with the task it works in
            // parentheses and no colour of its own.
            Some(label) => ToolCall::agent(label.clone(), None, agent, title.unwrap_or_default())
                .with_model(shaded(worker, cx.state)),
            None => ToolCall::agent(agent, Some(color), named.task, title.unwrap_or_default()),
        }
        .with_key(place.key.to_string())
        .with_stamp(format!("{started} {worker}"));
        let mut live = Self {
            labeled: named.label.is_some(),
            writes_the_message: writes_the_message(named.step),
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
            reads_only: false,
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
        let running = cx.state.names.running(&session.worker);
        let name = cx.state.names.worker(&running);
        let mut changed = false;
        for call in [&mut self.head, &mut self.whole, &mut self.outbox] {
            // A labeled block names its agent after the label, its model in its shade.
            let shown = match &mut call.title {
                ToolTitle::Agent { role, model, .. } if self.labeled => {
                    *model = Some(Box::new(shaded(&running, cx.state)));
                    role
                }
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
            ..
        } = &self.head.title
        else {
            return StartedAgent::new(self.head.title.plain(), None, "");
        };
        // Under a label the tree names who runs it alone, as the label says what it works.
        let detail = if task.is_empty() || self.labeled {
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
        if cx.history_only() {
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
        if let Some(batch) = self.take_batch() {
            cx.model.history.push(batch);
        }
    }

    /// The calls waiting to print, which count as printed from here on.
    fn take_batch(&mut self) -> Option<ToolCall> {
        if self.outbox.body.is_empty() {
            return None;
        }
        let batch = self.outbox.clone();
        self.outbox.body.clear();
        self.printed = true;
        Some(batch)
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

/// Each cell to scrollback, or only to the overlay where it was only kept.
fn print_all(out: Vec<(Cell, bool)>, cx: &mut Cx<'_>) {
    for (cell, prints) in out {
        if prints {
            cx.model.history.push(cell);
        } else {
            cx.model.history.keep(cell);
        }
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
        GlyphChoice::Unicode => Glyphs::unicode(),
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

/// Who runs `worker` as its header draws them: the logo of whoever serves its model, and the
/// model in its colour, its place among its CLI's models as every tab ranks them.
fn shaded(worker: &ModelRef, state: &AppState) -> ModelShade {
    let names = &state.names;
    let (cli, name) = names.shown_as(&worker.agent, worker.model.as_deref());
    let ranked = state
        .usage
        .as_ref()
        .map_or(&[][..], |usage| usage.all_time_models.as_slice());
    let place = ModelColors::new(ranked, names)
        .with_named([(cli.as_str(), name.as_str())])
        .place_named(&cli, &name);
    ModelShade {
        cli: crate::base::theme::Role::for_cli(&cli),
        place: u8::try_from(place.min(crate::base::theme::shades::OWN_TEXTURES))
            .unwrap_or_default(),
        logo: logo_for(&worker.agent, worker.model.as_deref()),
        model: names.model(&worker.agent, worker.model.as_deref()),
        on: names.cli(&worker.agent),
    }
}

/// `report ✓ · checks ✓`, or why it ended first, then what the agent decided in your place.
fn end_detail(
    end: &SessionEnd,
    checks: Option<bool>,
    decided: Option<&str>,
    gave: Option<&str>,
    (agent, names): (&str, &DisplayNames),
    glyphs: &Glyphs,
) -> String {
    let mut parts = Vec::new();
    if let Some(reason) = reason_words(&end.reason, agent, names) {
        parts.push(reason);
    }
    let reported = end.structured.is_some() || end.result_text.is_some();
    if end.reason == EndReason::Completed && reported {
        parts.push(gave.map_or_else(|| format!("report {}", glyphs.ready), str::to_owned));
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
    parts.join(" · ")
}

/// What the session spent and how long it took, in the order Claude Code's own agents' rows
/// put them in their parentheses: `61.0k tokens · 9m 40s`.
fn spent_and_took(end: &SessionEnd) -> String {
    let mut parts: Vec<String> = spent(end).into_iter().collect();
    parts.push(format::duration(Duration::from_millis(end.duration_ms)));
    parts.join(" · ")
}

/// How long the session took and what it spent, as a council's challenge and choice rows end:
/// `9m 40s · 61.0k tokens`.
fn took(end: &SessionEnd) -> String {
    let mut parts = vec![format::duration(Duration::from_millis(end.duration_ms))];
    parts.extend(spent(end));
    parts.join(" · ")
}

/// `61.0k tokens`, or nothing for a session that spent none.
fn spent(end: &SessionEnd) -> Option<String> {
    let tokens = end.spend.input_tokens + end.spend.output_tokens;
    (tokens > 0).then(|| format!("{} tokens", format::tokens_as_claude_code(tokens)))
}

/// Why it ended, in words. A failure the CLI explained is one plain line; its own text stays
/// in the raw log.
/// Why a session ended, in words, the model a CLI refused by its own name.
pub(crate) fn reason_words(
    reason: &EndReason,
    agent: &str,
    names: &DisplayNames,
) -> Option<String> {
    let cli = names.cli(agent);
    Some(match reason {
        EndReason::Completed | EndReason::Stopped => return None,
        EndReason::Paused => "paused".to_owned(),
        EndReason::LimitHit { .. } => "limit reached".to_owned(),
        EndReason::Stuck {
            watchdog: Watchdog::Inactivity,
            ..
        } => "stuck, no activity".to_owned(),
        EndReason::Stuck {
            watchdog: Watchdog::Repeat,
            ..
        } => "stuck, repeating a call".to_owned(),
        EndReason::Stuck {
            watchdog: Watchdog::NoProgress,
            ..
        } => "stuck, changing nothing".to_owned(),
        EndReason::UsageLimit { .. } => "usage limit".to_owned(),
        EndReason::RateLimited => "rate limited".to_owned(),
        EndReason::ContextFull => "context full".to_owned(),
        EndReason::ModelUnavailable { model } => format!(
            "{} unavailable",
            names
                .model(agent, Some(model))
                .unwrap_or_else(|| model.clone())
        ),
        EndReason::CliError { message, .. }
        | EndReason::Auth { message }
        | EndReason::Billing { message }
        | EndReason::StartFailed { message }
        | EndReason::Crashed { message } => failure::plain(&cli, message),
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

#[cfg(test)]
mod rows_tests;

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::app::AppEvent;
    use argonauts_core::runs::events::RunEvent;
    use argonauts_core::settings::ui::UiSettings;
    use serde_json::json;

    use super::AgentBlocks;
    use crate::app::model::ViewModel;
    use crate::base::draw::render::{Clock, RenderCtx};
    use crate::base::text::plain_text;
    use crate::base::theme::{Glyphs, Theme};
    use crate::bridge::{Cx, Effects};
    use crate::frame::state::AppState;

    /// An agent named for the commit message, as a run's log records its start and its end:
    /// the merger's writer where it works the `commit_message` step, else one a workflow's
    /// script only gave that name.
    fn named_for_the_message(session: &str, step: Option<&str>, why: &str) -> Vec<RunEvent> {
        let on = |seq: u64, data: serde_json::Value| -> RunEvent {
            serde_json::from_value(json!({
                "seq": seq,
                "ts": "2026-10-15T00:35:00Z",
                "run_id": "run-01",
                "session": session,
                "source": "argonauts-code",
                "kind": { "type": "session", "data": data },
            }))
            .expect("a session event")
        };
        vec![
            on(
                1,
                json!({
                    "event": "started",
                    "worker": "claude-code/haiku",
                    "agent_id": "claude-code",
                    "provider": "anthropic",
                    "kind": "cli",
                    "role": "worker",
                    "step": step,
                    "access": "no_tools",
                    "lifetime": "task",
                    "resumed_from": null,
                    "follow_key": null,
                    "label": argonauts_core::app::views::COMMIT_MESSAGE,
                }),
            ),
            on(
                2,
                json!({
                    "event": "ended",
                    "cli_session": null,
                    "exit_code": null,
                    "signal": null,
                    "duration_ms": 12_000,
                    "reason": { "why": why },
                    "spend": argonauts_core::Spend::default(),
                    "tool_calls": 0,
                    "result_text": null,
                    "structured": null,
                    "last_message": null,
                }),
            ),
        ]
    }

    /// A council seat's turn as a run's log records it: its start, a tool it ran, and its end
    /// with a large count, as real CLIs report.
    fn a_seats_turn() -> Vec<RunEvent> {
        let on = |seq: u64, kind: serde_json::Value| -> RunEvent {
            serde_json::from_value(json!({
                "seq": seq,
                "ts": "2026-10-15T00:35:00Z",
                "run_id": "run-01",
                "session": "talk-1",
                "source": "argonauts-code",
                "kind": kind,
            }))
            .expect("a run event")
        };
        vec![
            on(
                1,
                json!({ "type": "session", "data": {
                    "event": "started",
                    "worker": "claude-code/haiku",
                    "agent_id": "claude-code",
                    "provider": "anthropic",
                    "kind": "cli",
                    "role": "draft",
                    "step": null,
                    "access": "read_only",
                    "lifetime": "seat",
                    "resumed_from": null,
                    "follow_key": null,
                    "label": "talk:anthropic",
                    "seat": "seat-001",
                }}),
            ),
            on(
                2,
                json!({ "type": "agent", "data": {
                    "type": "tool_started",
                    "id": "toolu-1",
                    "name": "Read",
                    "input": { "file_path": "src/server.ts" },
                    "parent": null,
                }}),
            ),
            on(
                3,
                json!({ "type": "session", "data": {
                    "event": "ended",
                    "cli_session": null,
                    "exit_code": 0,
                    "signal": null,
                    "duration_ms": 6_000,
                    "reason": { "why": "completed" },
                    "spend": argonauts_core::Spend {
                        input_tokens: 373_000,
                        output_tokens: 900,
                        ..argonauts_core::Spend::default()
                    },
                    "tool_calls": 1,
                    "result_text": null,
                    "structured": null,
                    "last_message": null,
                }}),
            ),
        ]
    }

    /// A seat's turn shows in its bubble and its row under the request, so the conversation
    /// prints no agent row for it, not even once its tools run and it ends.
    #[test]
    fn a_seats_turn_leaves_no_agent_row() {
        let printed = printed(a_seats_turn());
        assert!(
            !printed.contains("Done") && !printed.contains("Read") && !printed.contains("talk:"),
            "{printed}"
        );
    }

    /// The judge auto mode asks to settle a talking council's question reads files and ends
    /// like a seat's turn, and the question's row says who decided, so the talk prints no agent
    /// row for it either.
    #[test]
    fn a_councils_judge_leaves_no_agent_row() {
        use std::path::PathBuf;

        use argonauts_core::runs::events::RunEventKind;
        use argonauts_core::runs::events::council;
        use argonauts_core::runs::events::run::{RunLifecycle, RunMode, RunStarted};

        let turn = a_seats_turn();
        let started = RunEvent {
            seq: 0,
            ts: turn[0].ts,
            run_id: turn[0].run_id.clone(),
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind: RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
                request: "make the rate limiter per api key".into(),
                title: "Make the rate limiter per API key".into(),
                mode: RunMode::Crew,
                forced: false,
                workflow: "council".into(),
                estimate: None,
                captain_or_crew: None,
                conversation_turn: None,
                continues: None,
                repo_root: PathBuf::from("/Users/you/code/example"),
                base_branch: Some("main".into()),
                base_commit: "8b41d07aa".into(),
                uncommitted: vec![],
                uncommitted_included: false,
                settings_sha256: String::new(),
                settings: serde_json::Value::Null,
                overrides: vec![],
                cli_versions: Default::default(),
                version: "0.2.1".into(),
                outside_agent: None,
                instructions: Vec::new(),
                required_steps: Vec::new(),
                starts_from: None,
            }))),
        };
        let seated = RunEvent {
            seq: 1,
            ts: turn[0].ts,
            run_id: turn[0].run_id.clone(),
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind: RunEventKind::Council(council::CouncilEvent {
                talk: argonauts_core::workflows::workflow::StepRef {
                    workflow: argonauts_core::base::names::WorkflowId::numbered(1),
                    phase: 1,
                    path: vec![0],
                },
                kind: council::CouncilEventKind::Seated(council::Seating {
                    seats: vec![council::Seat {
                        id: argonauts_core::base::names::SeatId::numbered(1),
                        model: "claude-code/haiku".parse().expect("a model"),
                        role: council::SeatRole::Proposer,
                        number: None,
                    }],
                    notes: None,
                }),
            }),
        };
        let mut events = vec![started, seated];
        for mut event in turn {
            if let RunEventKind::Session(
                argonauts_core::runs::events::session::SessionEvent::Started(started),
            ) = &mut event.kind
            {
                started.seat = None;
                started.role = argonauts_core::base::names::RoleName::Judge;
                started.label = Some("talk:1".into());
                started.worker = "claude-code/opus".parse().expect("a model");
            }
            events.push(event);
        }
        let printed = printed(events);
        assert!(
            !printed.contains("Done") && !printed.contains("Read") && !printed.contains("talk:"),
            "{printed}"
        );
    }

    /// What the conversation prints once `events` are in.
    fn printed(events: Vec<RunEvent>) -> String {
        plain_text(&printed_lines(events))
    }

    /// [`printed`] as the screen's lines, colours and all.
    fn printed_lines(events: Vec<RunEvent>) -> Vec<ratatui::text::Line<'static>> {
        let mut model = ViewModel::default();
        let mut state = AppState::default();
        let settings = UiSettings::default();
        let mut agents = AgentBlocks::default();
        for event in events {
            let event = AppEvent::Run {
                event: Box::new(event),
            };
            state.apply(&event);
            let mut out = Effects::default();
            let mut cx = Cx {
                model: &mut model,
                state: &state,
                settings: &settings,
                now: Instant::now(),
                out: &mut out,
            };
            agents.on_event(&event, &mut cx);
        }
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        model.history.drain_printable(&ctx)
    }

    /// Your answer to the merge came before the commit message, so the writer was stopped and
    /// the change lands with the message plain code writes: the writer leaves no row. One that
    /// wrote its message ends on its row as any agent does, and so does an agent a script only
    /// named for the message once you stop it.
    #[test]
    fn a_message_writer_the_merge_didnt_wait_for_leaves_no_row() {
        let writer = Some("commit_message");
        assert_eq!(
            printed(named_for_the_message("s-001", writer, "stopped")),
            ""
        );
        assert_eq!(
            printed(named_for_the_message("s-002", writer, "completed")),
            "● commit message · Haiku on Claude Code\n  ⎿  Done (0 tool uses · 12s)"
        );
        let stopped = printed(named_for_the_message("s-003", None, "stopped"));
        assert!(
            stopped.starts_with("● commit message · Haiku on Claude Code"),
            "{stopped}"
        );
    }

    /// A run's rows once it merged without waiting for the message: the commit message's
    /// writer it stopped leaves none, and an agent a workflow's script only named for the
    /// message, stopped too, keeps its row as any stopped agent does.
    #[test]
    fn the_rows_leave_out_only_the_writer_the_merge_didnt_wait_for() {
        use crate::testing::pictures::picture_check::assert_matches;

        let mut events = named_for_the_message("s-001", Some("commit_message"), "stopped");
        events.extend(named_for_the_message("s-002", None, "stopped"));
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        assert_matches(
            "writer_not_waited_for",
            &printed_lines(events),
            include_str!("../../../tests/golden/transcript/writer_not_waited_for.txt"),
            include_str!("../../../tests/golden/transcript/writer_not_waited_for.html"),
            &theme,
        );
    }
}
