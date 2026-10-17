//! What Enter on a run's panel row opens, and Ctrl+T and `/workflows`, and the short view for a
//! run that has nothing to show yet.
//!
//! Nothing of a workflow opens before its shape is known: while it is being written, by the main
//! agent's design, a script or a CLI's own workflow, the keys only say so. Once it is known they
//! open the workflow view, and Enter on the row of a run that is one worker opens that worker's
//! session. A run no agent has started in yet opens [`WaitingView`], which says what the run
//! waits for. What opens this way follows the run: on to the worker once it starts, and to the
//! workflow view once its shape is known.

use argonauts_core::RunId;
use argonauts_core::app::ViewRequest;
use argonauts_core::events::run::{RunMode, WaitReason};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::steps::StepName;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::dock::{self, Heading};
use super::panes::{self, Footer};
use super::session::SessionView;
use super::{Follow, View, ViewCtx, ViewOutcome, list};
use crate::state::{AppState, RunState, SessionState};
use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

/// What Enter on a run's row opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEntry {
    /// One agent's session, streaming as it works.
    Session(SessionKey),
    /// The run's workflow view, which the controller builds.
    Workflow,
    /// [`WaitingView`]: no agent has started and no workflow is declared.
    Waiting,
    /// Nothing: the workflow is still being written, and opens once its phases are known.
    Writing,
}

pub use argonauts_core::views::BEING_WRITTEN;

/// What Enter on a run's row opens. A run this screen doesn't hold goes to the controller,
/// which knows it from its log.
pub fn entry(state: &AppState, run: &RunId) -> RunEntry {
    let one_worker = state
        .runs
        .get(run)
        .filter(|held| declared(held) && held.shape_known())
        .and_then(single_agent);
    match one_worker {
        Some(session) => RunEntry::Session(session),
        None => workflow_entry(state, run),
    }
}

/// What Ctrl+T and `/workflows` open: the workflow view once the run's shape is known, never
/// one agent's session in its place.
pub fn workflow_entry(state: &AppState, run: &RunId) -> RunEntry {
    let Some(run) = state.runs.get(run) else {
        return RunEntry::Workflow;
    };
    if declared(run) && run.shape_known() {
        return RunEntry::Workflow;
    }
    if declared(run) {
        return RunEntry::Writing;
    }
    if designing(run).is_some() {
        return RunEntry::Writing;
    }
    // A log from before runs declared their workflows has agents and no design.
    match newest_at_work(run) {
        Some(session) => RunEntry::Session(session),
        None => RunEntry::Waiting,
    }
}

/// Where a view opened from a run's row goes once the run has moved on from `shown`, the
/// session it shows (`None` for [`WaitingView`]): the main agent's next design, or the agent that
/// started, or the workflow view once the workflow is declared.
pub(crate) fn follow(state: &AppState, run: &RunId, shown: Option<&SessionKey>) -> Option<Follow> {
    match entry(state, run) {
        RunEntry::Session(session) if shown != Some(&session) => Some(Follow::Replace(Box::new(
            SessionView::for_run(run.clone(), session),
        ))),
        RunEntry::Workflow if state.runs.contains_key(run) => {
            Some(Follow::Request(ViewRequest::Workflow {
                run: run.clone(),
                workflow: None,
            }))
        }
        _ => None,
    }
}

/// The run's workflow has started, so the controller has a workflow view to build.
fn declared(run: &RunState) -> bool {
    !run.workflows.is_empty()
}

/// The one agent a run's work is, when it is one worker: a Captain run's worker while it's the
/// only one at work in the run's one workflow, the newest when the run tries again, or the only
/// agent a run started. A run with a reviewer, or one whose phases a script or a CLI's own
/// workflow writes, is a workflow, which its view shows. A Captain run that moved to the crew has
/// a workflow of its own too. The main agent whose design became the workflow isn't the work, so
/// a declared workflow opens its view even before its first step starts.
fn single_agent(run: &RunState) -> Option<SessionKey> {
    let written = run
        .workflows
        .iter()
        .any(|workflow| run.reported.expected_from(&workflow.id));
    let workers_only = run
        .sessions_in_order()
        .all(|session| session.role == RoleName::Worker && session.workflows.is_empty());
    if written || !workers_only {
        return None;
    }
    if run.mode == Some(RunMode::Captain) && run.workflows.len() <= 1 {
        let workers: Vec<&SessionState> = run
            .sessions_in_order()
            .filter(|session| session.role == RoleName::Worker)
            .collect();
        let worker = workers
            .iter()
            .rev()
            .find(|worker| worker.is_running())
            .or(workers.last());
        if let Some(worker) = worker
            && run
                .sessions_in_order()
                .all(|session| session.key == worker.key || !session.is_running())
        {
            return Some(worker.key.clone());
        }
    }
    let mut sessions = run.sessions_in_order();
    let only = sessions.next()?;
    let designed = declared(run) && only.step == Some(StepName::DesignWorkflow);
    let one_workflow = run.workflows.len() <= 1;
    (sessions.next().is_none() && !designed && one_workflow).then(|| only.key.clone())
}

/// The main agent designing the workflow before it's declared: the newest design still running,
/// such as `main:design-2` after `main:design` ended, else the newest one.
fn designing(run: &RunState) -> Option<SessionKey> {
    let designs: Vec<_> = run
        .sessions_in_order()
        .filter(|session| session.step == Some(StepName::DesignWorkflow))
        .collect();
    newest(&designs)
}

/// The newest agent at work, else the newest that was.
fn newest_at_work(run: &RunState) -> Option<SessionKey> {
    newest(&run.sessions_in_order().collect::<Vec<_>>())
}

fn newest(sessions: &[&SessionState]) -> Option<SessionKey> {
    sessions
        .iter()
        .rev()
        .find(|session| session.is_running())
        .or(sessions.last())
        .map(|session| session.key.clone())
}

/// A run with nothing to show yet, docked under the conversation like the workflow view: its
/// name and description, what it waits for, and Enter to open what has started since.
#[derive(Debug)]
pub struct WaitingView {
    run: RunId,
}

impl WaitingView {
    pub fn new(run: RunId) -> Self {
        Self { run }
    }

    fn heading(&self, run: Option<&RunState>) -> Heading {
        let named = run.and_then(|run| run.named.as_ref());
        let name = named
            .map(|named| named.name.trim())
            .filter(|name| !name.is_empty())
            .map_or_else(|| self.run.to_string(), str::to_owned);
        let description = named
            .map(|named| named.description.trim())
            .filter(|text| !text.is_empty())
            .or_else(|| run.map(|run| run.title.trim()))
            .unwrap_or_default()
            .to_owned();
        Heading {
            name,
            description,
            ..Heading::default()
        }
    }

    fn lines(&self, state: &AppState, theme: &Theme) -> Vec<Line<'static>> {
        let run = state.runs.get(&self.run);
        let (said, then) = match entry(state, &self.run) {
            RunEntry::Waiting => (waiting_for(run), "Enter opens its agent once one starts."),
            RunEntry::Session(session) => {
                let label = run
                    .and_then(|run| run.sessions.get(&session))
                    .map_or_else(|| session.to_string(), SessionState::task);
                (format!("{label} started."), "Enter opens its session.")
            }
            RunEntry::Workflow => (
                "Its workflow started.".to_owned(),
                "Enter opens the workflow view.",
            ),
            RunEntry::Writing => (
                "Its workflow is being written.".to_owned(),
                "Enter opens it once its phases are known.",
            ),
        };
        vec![
            Line::from(Span::styled(
                sanitize_line(&said).into_owned(),
                theme.style(Role::Text),
            )),
            Line::from(Span::styled(then, theme.style(Role::Dim))),
        ]
    }
}

/// In plain words, what a run that no agent has started in is waiting for.
pub(crate) fn waiting_for(run: Option<&RunState>) -> String {
    let Some(run) = run else {
        return "Waiting for the run to start.".to_owned();
    };
    if run.finished.is_some() {
        return "This run ended before any agent started.".to_owned();
    }
    if let Some(open) = run.approvals.first() {
        return format!(
            "Waiting for you: {}.",
            open.opened.title.trim_end_matches('.')
        );
    }
    match &run.waiting {
        Some(WaitReason::Approval) => "Waiting for you to answer its question.".to_owned(),
        Some(WaitReason::Paused) => "Paused. /resume goes on.".to_owned(),
        Some(WaitReason::AllAgentsOut { .. }) => {
            "Every agent is out, so it waits for one to come back.".to_owned()
        }
        Some(WaitReason::WaitingForAgent { agent_id, .. }) => format!("Waiting for {agent_id}."),
        Some(WaitReason::NoFallback { detail, stop, .. }) => {
            let why = stop
                .as_ref()
                .map_or(detail.as_str(), |stop| stop.why.as_str());
            format!("It stopped: {why}. /resume carries it on.")
        }
        None if run.stopping => "Stopping.".to_owned(),
        None if run.pausing => "Pausing at a safe point.".to_owned(),
        None if run.mode == Some(RunMode::Captain) => "Waiting for its agent to start.".to_owned(),
        None => "Waiting for the main agent to start designing the workflow.".to_owned(),
    }
}

impl View for WaitingView {
    fn docked(&self) -> bool {
        true
    }

    fn follow(&mut self, ctx: &ViewCtx<'_>) -> Option<Follow> {
        follow(ctx.state, &self.run, None)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let heading = self.heading(ctx.state.runs.get(&self.run));
        let frame = dock::split(area, 0, 0);
        dock::draw_rule(buf, &frame, theme);
        dock::draw_heading(buf, &frame, &heading, theme);
        dock::draw_box(buf, &frame, ["", "Starting"], "", theme);
        panes::put_lines(buf, frame.panes.right, &self.lines(ctx.state, theme));
        dock::draw_footer(
            buf,
            &frame,
            Footer::hints(&[("⏎ open", 1), (panes::ESC_BACK, 0)]),
            theme,
        );
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ViewOutcome::Ignored;
        }
        match key.code {
            KeyCode::Esc => ViewOutcome::Close,
            KeyCode::Enter => match entry(ctx.state, &self.run) {
                RunEntry::Session(session) => {
                    ViewOutcome::Replace(Box::new(SessionView::for_run(self.run.clone(), session)))
                }
                RunEntry::Workflow => ViewOutcome::Request(ViewRequest::Workflow {
                    run: self.run.clone(),
                    workflow: None,
                }),
                RunEntry::Waiting => ViewOutcome::Notice("Nothing has started yet".to_owned()),
                RunEntry::Writing => ViewOutcome::Notice(BEING_WRITTEN.to_owned()),
            },
            _ => ViewOutcome::Consumed,
        }
    }
}

/// A crew run as the engine reports it while its main agent designs the workflow, built from the
/// log's own JSON, for tests here and of the keys that open it.
#[cfg(test)]
pub(crate) mod fixture {
    use argonauts_core::RunId;
    use argonauts_core::app::AppEvent;
    use serde_json::{Value, json};

    use crate::state::AppState;

    pub(crate) const RUN: &str = "run-20261004-165413-8d01";

    pub(crate) fn run_id() -> RunId {
        RunId::new(RUN).unwrap()
    }

    pub(crate) struct RunLog {
        seq: u64,
        pub state: AppState,
        /// Every event sent, in order, for another reader of the log.
        pub events: Vec<AppEvent>,
    }

    impl RunLog {
        /// A crew run that has started and taken its name, with no agent yet.
        pub(crate) fn started() -> Self {
            Self::started_as("crew")
        }

        /// The same, in `mode`, such as `captain`.
        pub(crate) fn started_as(mode: &str) -> Self {
            let mut log = Self {
                seq: 0,
                state: AppState::default(),
                events: Vec::new(),
            };
            let request = "Make the rate limiter per API key";
            log.send(
                None,
                json!({"type": "run", "data": {"event": "started", "request": request,
                    "title": request, "mode": mode, "forced": false, "workflow": "council",
                    "estimate": null, "captain_or_crew": null, "conversation_turn": null, "continues": null,
                    "repo_root": "/repo", "base_branch": "main", "base_commit": "8b41d07",
                    "uncommitted": [], "uncommitted_included": false, "settings_sha256": "",
                    "settings": {}, "overrides": [], "cli_versions": {},
                    "version": "0.1.0"}}),
            );
            log.send(
                None,
                json!({"type": "run", "data": {"event": "named", "name": "add-per-api-key",
                    "description": "Add per-API-key rate limiting with Retry-After headers",
                    "style": "workflow"}}),
            );
            log
        }

        pub(crate) fn send(&mut self, session: Option<&str>, kind: Value) {
            self.send_as(session, None, kind);
        }

        /// The same, an event of the task `task`.
        pub(crate) fn send_for_task(&mut self, session: Option<&str>, task: &str, kind: Value) {
            self.send_as(session, Some(task), kind);
        }

        fn send_as(&mut self, session: Option<&str>, task: Option<&str>, kind: Value) {
            let event = json!({"type": "run", "event": {"seq": self.seq,
                "ts": "2026-10-04T16:54:13Z", "run_id": RUN, "task_id": task,
                "session": session, "source": "argonauts-code", "kind": kind}});
            self.seq += 1;
            let event: AppEvent = serde_json::from_value(event).expect("a run event");
            self.state.apply(&event);
            self.events.push(event);
        }

        /// The main agent starts a design of the workflow, such as `main:design-2` after one that
        /// didn't hold.
        pub(crate) fn design(&mut self, session: &str, label: &str) {
            self.design_from(session, label, None);
        }

        /// The same, resuming the CLI session `from` when code sent its brief back.
        pub(crate) fn design_from(&mut self, session: &str, label: &str, from: Option<&str>) {
            self.send(
                Some(session),
                json!({"type": "session", "data": {"event": "started",
                    "worker": "claude-code/haiku", "agent_id": "claude-code", "provider": "anthropic",
                    "kind": "cli", "role": "tie-breaker", "step": "design_workflow",
                    "access": "read_only", "lifetime": "task", "resumed_from": from,
                    "follow_key": null, "label": label}}),
            );
        }

        /// A task's worker, such as a Captain run's.
        pub(crate) fn worker(&mut self, session: &str) {
            self.send(
                Some(session),
                json!({"type": "session", "data": {"event": "started",
                    "worker": "claude-code/haiku", "agent_id": "claude-code", "provider": "anthropic",
                    "kind": "cli", "role": "worker", "step": null, "access": "build",
                    "lifetime": "task", "resumed_from": null, "follow_key": 1,
                    "label": "build:limiter"}}),
            );
        }

        pub(crate) fn ended(&mut self, session: &str) {
            self.send(
                Some(session),
                json!({"type": "session", "data": {"event": "ended", "cli_session": null,
                    "exit_code": 0, "signal": null, "duration_ms": 82_711,
                    "reason": {"why": "completed"},
                    "spend": {"input_tokens": 12_611, "output_tokens": 8_142,
                              "cached_tokens": 0, "cost_usd": 0.07, "cost_partial": false,
                              "cost_estimated": false},
                    "tool_calls": 1, "result_text": null, "structured": null,
                    "last_message": null, "raw_log": null}}),
            );
        }

        /// The main agent's brief held, so the run's workflow starts.
        pub(crate) fn declare(&mut self) {
            self.workflow("wf-001");
        }

        /// A workflow starts, such as the crew's when a Captain run moves to it.
        pub(crate) fn workflow(&mut self, id: &str) {
            self.send(
                None,
                json!({"type": "workflow", "data": {"event": "started", "id": id,
                    "from": "council", "version": "1", "parent": null,
                    "workflow": {"name": "add-per-api-key",
                        "description": "Add per-API-key rate limiting", "phases": []}}}),
            );
        }

        /// One of the workflow's own agents.
        pub(crate) fn step_agent(&mut self, session: &str, label: &str) {
            let follow: u8 = session.trim_start_matches("s-").parse().unwrap_or(1);
            self.send(
                Some(session),
                json!({"type": "session", "data": {"event": "started",
                    "worker": "codex/gpt-5.6-luna", "agent_id": "codex", "provider": "openai",
                    "kind": "cli", "role": "reader", "step": "read",
                    "access": "read_only", "lifetime": "task", "resumed_from": null,
                    "follow_key": follow, "label": label,
                    "workflow_step": {"workflow": "wf-001", "phase": 0, "path": [0]}}}),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use serde_json::{Value, json};

    use super::fixture::{RunLog, run_id};
    use super::*;
    use crate::state::AppState;

    fn session(key: &str) -> RunEntry {
        RunEntry::Session(SessionKey::parse(key).unwrap())
    }

    /// While the main agent designs the workflow, nothing of it opens, and never the main
    /// agent's page in its place: Enter, Ctrl+T and `/workflows` say it is being written.
    #[test]
    fn nothing_opens_while_the_workflow_is_being_written() {
        let mut log = RunLog::started();
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Waiting);
        log.design("s-001", "main:design");
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Writing);
        assert_eq!(workflow_entry(&log.state, &run_id()), RunEntry::Writing);
        // The first brief didn't hold, so a second design starts over.
        log.ended("s-001");
        log.design("s-002", "main:design-2");
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Writing);
        log.ended("s-002");
        log.declare();
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Workflow);
        log.step_agent("s-003", "read:openai");
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Workflow);
    }

    /// A workflow a script or a CLI's own workflow writes opens once the first report lists its
    /// phases. Its one worker at work is no shortcut: the run is a workflow.
    #[test]
    fn a_written_workflow_opens_once_its_phases_are_known() {
        let mut log = RunLog::started();
        log.send(
            None,
            json!({"type": "workflow", "data": {"event": "started", "id": "wf-001",
            "from": "script", "version": "1", "parent": null,
            "workflow": {"name": "fix-money-rounding", "description": "",
                "phases": [
                    {"name": "Work", "standard": "do", "steps": [{"kind": "script"}]},
                    {"name": "Merge", "standard": "decide", "steps": [{"kind": "merge"}]}
                ]}}}),
        );
        let started = json!({"type": "session", "data": {"event": "started",
            "worker": "claude-code/haiku", "agent_id": "claude-code", "provider": "anthropic",
            "kind": "cli", "role": "worker", "step": "build", "access": "build",
            "lifetime": "task", "resumed_from": null, "follow_key": 1, "label": "build:fix-money",
            "workflow_step": {"workflow": "wf-001", "phase": 0, "path": [0]}}});
        log.send_for_task(Some("s-001"), "task-fix-money", started);
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Writing);
        assert_eq!(workflow_entry(&log.state, &run_id()), RunEntry::Writing);
        let workflow = |phases: Value| {
            json!({"type": "agent", "data": {"type": "workflow", "workflow": {
                "id": "wf_1", "name": "fix-money-rounding", "description": "",
                "phases": phases, "agents": [], "status": "running", "tokens": 0,
                "tool_calls": 0, "duration_ms": 0}}})
        };
        log.send_for_task(Some("s-001"), "task-fix-money", workflow(json!([])));
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Writing);
        log.send_for_task(
            Some("s-001"),
            "task-fix-money",
            workflow(json!([{"index": 1, "title": "Build"}, {"index": 2, "title": "Review"}])),
        );
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Workflow);
        assert_eq!(workflow_entry(&log.state, &run_id()), RunEntry::Workflow);
    }

    /// A Captain run is its worker's while the worker is the only one at work in its one workflow:
    /// the newest try's worker, not the first. Once a reviewer works, or the crew takes the
    /// run over in a workflow of its own, the row opens the workflow view.
    #[test]
    fn a_captain_run_opens_its_worker_while_that_is_the_work() {
        let mut log = RunLog::started_as("captain");
        log.workflow("wf-001");
        log.worker("s-001");
        assert_eq!(entry(&log.state, &run_id()), session("s-001"));
        log.ended("s-001");
        assert_eq!(entry(&log.state, &run_id()), session("s-001"));
        log.worker("s-002");
        assert_eq!(entry(&log.state, &run_id()), session("s-002"));
        log.step_agent("s-003", "review:limiter");
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Workflow);

        let mut moved = RunLog::started_as("captain");
        moved.workflow("wf-001");
        moved.worker("s-001");
        moved.ended("s-001");
        moved.workflow("wf-002");
        assert_eq!(entry(&moved.state, &run_id()), RunEntry::Workflow);
    }

    /// A log from before runs declared their workflows has agents and no design, and its row
    /// opens the newest at work rather than asking for a view there's nothing in.
    #[test]
    fn agents_with_no_design_and_no_workflow_open_the_newest_at_work() {
        let mut log = RunLog::started();
        log.step_agent("s-001", "read:openai");
        log.step_agent("s-002", "read:anthropic");
        assert_eq!(entry(&log.state, &run_id()), session("s-002"));
        log.ended("s-002");
        assert_eq!(entry(&log.state, &run_id()), session("s-001"));
        log.ended("s-001");
        assert_eq!(entry(&log.state, &run_id()), session("s-002"));
    }

    fn followed(view: &mut dyn View, state: &AppState) -> String {
        let theme = Theme::default();
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: std::time::Instant::now(),
        };
        view.follow(&ctx)
            .map_or_else(String::new, |next| format!("{next:?}"))
    }

    /// What opens from a run's row moves on with the run: nowhere while the workflow is being
    /// written, and to the workflow view once it's declared.
    #[test]
    fn a_view_from_the_row_follows_the_run() {
        let mut log = RunLog::started();
        let mut waiting = WaitingView::new(run_id());
        assert_eq!(followed(&mut waiting, &log.state), "");
        log.design("s-001", "main:design");
        assert_eq!(followed(&mut waiting, &log.state), "");
        log.ended("s-001");
        log.design_from("s-002", "main:design", Some("cli-7"));
        assert_eq!(followed(&mut waiting, &log.state), "");

        let mut fixing = SessionView::for_run(run_id(), SessionKey::parse("s-002").unwrap());
        log.ended("s-002");
        log.declare();
        let next = followed(&mut fixing, &log.state);
        assert!(next.starts_with("Request(Workflow"), "{next}");
        // A session opened by itself, such as from the agent panel, follows only its agent:
        // into the session that fixed its brief, and no further.
        let mut chosen = SessionView::new(run_id(), SessionKey::parse("s-001").unwrap());
        let next = followed(&mut chosen, &log.state);
        assert!(
            next.contains("s-002") && next.contains("for_run: false"),
            "{next}"
        );
        let mut chosen = SessionView::new(run_id(), SessionKey::parse("s-002").unwrap());
        assert_eq!(followed(&mut chosen, &log.state), "");
    }

    /// A view stays on an agent while it works, even opened from the run's row, so a run with
    /// several agents and no workflow doesn't pull it from one to the next.
    #[test]
    fn a_view_stays_on_an_agent_while_it_works() {
        let mut log = RunLog::started();
        log.step_agent("s-001", "read:openai");
        let mut reading = SessionView::for_run(run_id(), SessionKey::parse("s-001").unwrap());
        log.step_agent("s-002", "read:anthropic");
        assert_eq!(followed(&mut reading, &log.state), "");
        log.ended("s-001");
        let next = followed(&mut reading, &log.state);
        assert!(next.contains("s-002"), "{next}");
    }

    #[test]
    fn a_declared_workflow_opens_its_view_before_its_first_agent() {
        let mut log = RunLog::started();
        log.design("s-001", "main:design");
        log.declare();
        assert_eq!(entry(&log.state, &run_id()), RunEntry::Workflow);
    }

    #[test]
    fn a_run_this_screen_does_not_hold_is_the_controllers_to_show() {
        assert_eq!(entry(&AppState::default(), &run_id()), RunEntry::Workflow);
    }

    fn text(view: &mut WaitingView, state: &AppState) -> String {
        let theme = Theme::default();
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: std::time::Instant::now(),
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal
            .draw(|frame| view.render(frame.area(), frame.buffer_mut(), &ctx))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        dock::lines(&buf)
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(view: &mut WaitingView, code: KeyCode, state: &AppState) -> ViewOutcome {
        let theme = Theme::default();
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: std::time::Instant::now(),
        };
        view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
    }

    #[test]
    fn the_waiting_view_says_what_it_waits_for_and_opens_what_starts() {
        let mut log = RunLog::started();
        let mut view = WaitingView::new(run_id());
        let shown = text(&mut view, &log.state);
        assert!(shown.contains("add-per-api-key"), "{shown}");
        assert!(
            shown.contains("Waiting for the main agent to start designing the workflow."),
            "{shown}"
        );
        assert!(
            matches!(
                press(&mut view, KeyCode::Enter, &log.state),
                ViewOutcome::Notice(_)
            ),
            "nothing to open yet, said on the notice line"
        );

        // The main agent's design is the workflow being written: nothing opens until it is.
        log.design("s-001", "main:design");
        let shown = text(&mut view, &log.state);
        assert!(shown.contains("Its workflow is being written."), "{shown}");
        assert!(
            matches!(
                press(&mut view, KeyCode::Enter, &log.state),
                ViewOutcome::Notice(said) if said == BEING_WRITTEN
            ),
            "the main agent's page never opens in the workflow's place"
        );

        log.declare();
        assert!(matches!(
            press(&mut view, KeyCode::Enter, &log.state),
            ViewOutcome::Request(ViewRequest::Workflow { workflow: None, .. })
        ));
        assert!(matches!(
            press(&mut view, KeyCode::Esc, &log.state),
            ViewOutcome::Close
        ));
    }

    #[test]
    fn a_paused_run_says_so() {
        let mut log = RunLog::started();
        log.send(
            None,
            serde_json::json!({"type": "run", "data": {"event": "waiting",
                "reason": {"why": "paused"}, "waiting_on": []}}),
        );
        assert_eq!(
            waiting_for(log.state.runs.get(&run_id())),
            "Paused. /resume goes on."
        );
    }
}
