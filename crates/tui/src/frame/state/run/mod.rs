use std::collections::BTreeMap;
use std::time::Duration;

use argonauts_core::app::views::{is_workflow_agent, not_needed, round_stem};
use argonauts_core::base::names::{
    ApprovalId, MessageId, ModelRef, RoleName, SessionKey, WorkflowId,
};
use argonauts_core::runs::api::StatusSummary;
use argonauts_core::runs::events::agent_status::{FallbackEvent, RunSpend, SpendEvent};
use argonauts_core::runs::events::approval::{
    ApprovalEvent, ApprovalOpened, ApprovalPacket, MergePacket,
};
use argonauts_core::runs::events::run::{
    Party, PhaseChange, RunLifecycle, RunMode, RunNamed, RunOutcome, RunPhase, RunSnapshot,
    WaitReason,
};
use argonauts_core::runs::events::session::{CliWorkflowRun, QueuedMessage, SessionEvent};
use argonauts_core::runs::events::shared::BlackboardEvent;
use argonauts_core::runs::events::task::{
    CheckOutcome, TaskEvent, TaskSpec, TaskState, TaskWay, TryCause,
};
use argonauts_core::runs::events::workflow::{StepEnd, WorkflowEvent};
use argonauts_core::runs::events::{DivideEvent, MergeEvent, ReviewEvent, RunEvent, RunEventKind};
use argonauts_core::runs::fallback::QualityDebt;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::steps::judgment::ReviewVerdict;
use argonauts_core::workflows::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{DiffStat, PhaseStatus, RunId, Spend, TaskId};
use time::OffsetDateTime;

use super::decisions::{DecisionRecord, Decisions};
use super::reported::StepWorkflows;
use super::session::{SessionState, SessionStatus};

mod talk;

pub use talk::{SeatDoing, SeatNow, Talk, backs};

#[derive(Debug, Clone)]
pub struct RunState {
    pub id: RunId,
    pub title: String,
    pub mode: Option<RunMode>,
    /// The outside agent that started it (`Run::Started.outside_agent`), shown on its row.
    pub outside_agent: Option<String>,
    pub started: Option<OffsetDateTime>,
    pub phase: Option<RunPhase>,
    pub pausing: bool,
    /// Who asked for its pause, from the request until it carries on, so the pause is told
    /// as theirs: yours, the network's, or your limit's.
    pub paused_by: Option<Party>,
    pub waiting: Option<WaitReason>,
    pub finished: Option<RunOutcome>,
    /// When it finished, so its time stops there everywhere it shows.
    pub ended: Option<OffsetDateTime>,
    /// The time it spent waiting or stopped before it went on, which its clock leaves out.
    pub idle: Duration,
    /// Since when it waits or stopped, while it does.
    pub idle_since: Option<OffsetDateTime>,
    /// The agents its workflows went on without, as nothing could take them.
    pub left_out: Vec<argonauts_core::runs::events::workflow::AgentLeftOut>,
    /// What the latest merge question put up for the merge, so the line after the merge says the
    /// same.
    pub merge_totals: Option<MergeTotals>,
    /// The branch the run's work merged onto, once it did.
    pub merged_onto: Option<String>,
    /// In the order they started.
    pub sessions: BTreeMap<SessionKey, SessionState>,
    pub session_order: Vec<SessionKey>,
    pub tasks: BTreeMap<TaskId, TaskRow>,
    /// Approvals still open, oldest first.
    pub approvals: Vec<OpenApproval>,
    /// Auto decisions queued for a spot check (`Approval::SpotCheckQueued`) and not yet
    /// marked, oldest first.
    pub spot_checks: Vec<ApprovalId>,
    /// What the screen saw of recent decisions, for the spot checks that follow them.
    decisions: Decisions,
    /// The run's own workflow first, nested ones after.
    pub workflows: Vec<WorkflowState>,
    /// The workflows the runner runs as steps, as each last reported itself.
    pub reported: StepWorkflows,
    /// The workflow step that started last.
    pub last_step: Option<StepRef>,
    pub spent: Spend,
    /// Guidance and mentions not yet delivered, oldest first.
    pub queued: Vec<QueuedMessage>,
    /// The last event folded in.
    pub seq: u64,
    /// The run's own name and description, from the main agent's brief or intake, which the panel
    /// row and the views show rather than a template's.
    pub named: Option<RunNamed>,
    pub workflow: Option<String>,
    /// Set by `Run::ApprovalModeChanged`; the bottom line's mode until then.
    pub approval: Option<ApprovalMode>,
    /// A stop was asked for and the run is winding down.
    pub stopping: bool,
    /// The latest `Limit::Run`: spend against the run's limit.
    pub spend: Option<RunSpend>,
    /// What `Run::Snapshot` said about the run, for a long run seeded without its history.
    pub summary: Option<StatusSummary>,
    /// When its last event happened.
    pub updated: Option<OffsetDateTime>,
    /// The pull request it opened, as the app's newest look at GitHub found it.
    pub followed: Option<argonauts_core::app::FollowedPullRequest>,
    /// Its latest council's talk, which its row counts until it has tasks.
    pub talk: Option<Talk>,
}

impl RunState {
    /// Its pause in a few words, as its row says it: `paused by you`, `paused for the network`.
    pub fn paused_words(&self) -> String {
        match &self.paused_by {
            Some(Party::User) | None => "paused by you".to_owned(),
            Some(Party::Network) => "paused for the network".to_owned(),
            Some(Party::Rules) => "paused at your limit".to_owned(),
            // The name is the client's own, so it's drawn as text, never as escapes.
            Some(Party::OutsideAgent { client }) => format!(
                "paused by {}",
                argonauts_core::safety::sanitize::sanitize_for_terminal(client)
            ),
            Some(_) => "paused".to_owned(),
        }
    }
    /// The run's own workflow waits for the report that names its phases, so nothing of it
    /// draws yet: no row, no view, and no agent's item in the conversation.
    pub fn awaits_its_shape(&self) -> bool {
        self.workflows
            .iter()
            .any(|workflow| workflow.parent.is_none() && self.reported.awaits_shape(&workflow.id))
    }

    pub fn new(id: RunId) -> Self {
        Self {
            id,
            title: String::new(),
            mode: None,
            outside_agent: None,
            started: None,
            phase: None,
            pausing: false,
            paused_by: None,
            waiting: None,
            finished: None,
            ended: None,
            idle: Duration::ZERO,
            idle_since: None,
            left_out: Vec::new(),
            merge_totals: None,
            merged_onto: None,
            sessions: BTreeMap::new(),
            session_order: Vec::new(),
            tasks: BTreeMap::new(),
            approvals: Vec::new(),
            spot_checks: Vec::new(),
            decisions: Decisions::default(),
            workflows: Vec::new(),
            reported: StepWorkflows::default(),
            last_step: None,
            spent: Spend::default(),
            queued: Vec::new(),
            seq: 0,
            workflow: None,
            named: None,
            approval: None,
            stopping: false,
            spend: None,
            summary: None,
            updated: None,
            followed: None,
            talk: None,
        }
    }

    /// Sessions in the order they started.
    pub fn sessions_in_order(&self) -> impl Iterator<Item = &SessionState> {
        self.session_order
            .iter()
            .filter_map(|key| self.sessions.get(key))
    }

    /// Whether the run's workflow has its shape yet, so its row, its view and its line in the
    /// conversation are drawn: from its start when its phases are fixed, else at the first
    /// report that lists phases, the script's or the CLI's own workflow a task's session runs.
    pub fn shape_known(&self) -> bool {
        let Some(own) = self
            .workflows
            .iter()
            .find(|workflow| workflow.parent.is_none())
        else {
            // A run starts its workflow right after it starts, or once the main agent has
            // designed it, so until then there's nothing of it to draw. Understand may run
            // first, under the workflow to come. A log whose agents started with no workflow
            // is from before runs declared theirs, and a run seeded without its history may
            // not hold it: neither has more to wait for.
            let begun =
                !self.workflows.is_empty() || !self.sessions.is_empty() || self.summary.is_some();
            let designing = self
                .sessions_in_order()
                .any(|session| session.step == Some(StepName::DesignWorkflow));
            return begun && !designing;
        };
        let tasks_own = self
            .sessions_in_order()
            .filter(|session| session.task.is_some())
            .flat_map(|session| session.workflows.iter());
        argonauts_core::app::views::shape_known(
            self.reported.expected_from(&own.id),
            self.reported.all().chain(tasks_own),
        )
    }

    /// The session that carried on `key`'s work once it ended, such as the main agent's design that
    /// code sent back to it: the newest later one with its label and step, or one that resumed
    /// its CLI session.
    pub fn carried_on(&self, key: &SessionKey) -> Option<&SessionState> {
        let ended = self
            .sessions
            .get(key)
            .filter(|session| !session.is_running())?;
        self.sessions_in_order()
            .skip_while(|session| session.key != *key)
            .skip(1)
            .filter(|later| {
                let same_work =
                    later.label.is_some() && later.label == ended.label && later.step == ended.step;
                let resumed = later.cli_session.is_some() && later.cli_session == ended.cli_session;
                same_work || resumed
            })
            .last()
    }

    pub fn running_sessions(&self) -> impl Iterator<Item = &SessionState> {
        self.sessions_in_order()
            .filter(|session| session.is_running())
    }

    /// Still going: not finished and not waiting.
    pub fn is_active(&self) -> bool {
        self.finished.is_none() && self.waiting.is_none()
    }

    /// What it has spent so far, the one count every line that shows the run's spend reads:
    /// the limit's, with its sessions' cost when they report more, since the limit's count
    /// can lag behind the sessions' own reports.
    pub fn spend(&self) -> Spend {
        let mut spent = self.spend.as_ref().map_or(self.spent, |limit| limit.spent);
        let sessions: f64 = self
            .sessions
            .values()
            .filter_map(|session| session.cost_usd)
            .sum();
        spent.cost_usd = spent.cost_usd.max(sessions);
        spent
    }

    pub fn cost_usd(&self) -> f64 {
        self.spend().cost_usd
    }

    /// Agents done out of all of them, its time, its tokens and its spend: the one count its
    /// panel row, its workflow view's header and its closing row in the conversation all show.
    /// `wall` is now, for a run that still goes.
    pub fn totals(&self, wall: OffsetDateTime) -> RunTotals {
        let (agents_done, agents_total) = self.agents();
        let spent = self.spend();
        // Whichever is ahead: the sessions' own counts while they work, or the run's spend
        // once it is told.
        let by_session: u64 = self
            .sessions
            .values()
            .map(|session| session.tokens_out)
            .sum();
        RunTotals {
            agents_done,
            agents_total,
            elapsed: self.elapsed(wall),
            tokens: by_session.max(spent.output_tokens),
            spent,
        }
    }

    /// The agents its workflow ran, done and in all, as the workflow view counts them. The
    /// agents of a workflow one of its steps runs count as that workflow reports them, so one
    /// tried again is one agent, and those still to start are counted before they do. One
    /// stopped once it wasn't needed counts as neither done nor unfinished.
    fn agents(&self) -> (u32, u32) {
        let agents: Vec<&SessionState> = self
            .sessions_in_order()
            .filter(|session| is_workflow_agent(session.step, session.workflow_step.as_ref()))
            .filter(|session| {
                !not_needed(
                    session.step,
                    session.status == SessionStatus::Stopped,
                    self.merged_onto.is_some(),
                )
            })
            .filter(|session| {
                !self
                    .reported
                    .names(session.label.as_deref().unwrap_or_default())
            })
            .filter(|session| !self.wrote_the_steps(session))
            .collect();
        let agents = steps(agents);
        let (reported_done, reported) = self.reported.agents();
        let done = reported_done
            + agents
                .iter()
                .filter(|session| session.status == SessionStatus::Done)
                .filter(|session| !self.waits_for_its_round(session))
                .count();
        (
            u32::try_from(done).unwrap_or(u32::MAX),
            u32::try_from(agents.len() + reported).unwrap_or(u32::MAX),
        )
    }

    /// Whether `session` is one of the run's own reviewers that sent its task's work back while
    /// the run goes: its next round is still to come, so it isn't done yet. A review a CLI's own
    /// workflow asked for has no task of the run's, and is done once given.
    fn waits_for_its_round(&self, session: &SessionState) -> bool {
        self.finished.is_none()
            && session.sent_back
            && session.task.is_some()
            && matches!(session.role, RoleName::Reviewer | RoleName::Judge)
    }

    /// Whether `session` wrote the steps a workflow then ran: it worked in the phase that
    /// workflow's phases took the place of, and is none of the workflow's own agents. The view
    /// shows the workflow that runs, so the session that wrote it is none of its rows.
    fn wrote_the_steps(&self, session: &SessionState) -> bool {
        session.workflow_step.as_ref().is_some_and(|step| {
            self.reported
                .in_phase(&step.workflow, step.phase)
                .is_some_and(|reported| !reported.phases.is_empty())
        })
    }

    /// The time it worked: from its start to its end, or to `wall` while it goes, less the
    /// time it waited or stopped. A run seeded without its end stops at its last session's
    /// end. `None` before its start is known.
    pub fn elapsed(&self, wall: OffsetDateTime) -> Option<Duration> {
        let started = self.started?;
        let end = match (self.finished, self.ended) {
            (_, Some(ended)) => ended,
            (Some(_), None) => self
                .sessions
                .values()
                .filter_map(|session| session.ended)
                .max()
                .unwrap_or(wall),
            (None, None) => wall,
        };
        let end = self.idle_since.filter(|since| *since < end).unwrap_or(end);
        let span = Duration::try_from(end - started).unwrap_or_default();
        Some(span.saturating_sub(self.idle))
    }

    /// The workflow the run's views open on: its own, or before the main agent picks it, the one
    /// that runs first, such as Understand.
    pub fn shown_workflow(&self) -> Option<&WorkflowState> {
        self.workflows
            .iter()
            .find(|workflow| workflow.parent.is_none())
            .or_else(|| self.workflows.first())
    }

    /// The name of the phase of [`Self::shown_workflow`] that's going on: the one of the step
    /// that started last, while it runs, else of any step still running, else of the step that
    /// started last. A nested workflow's steps count for the phase it runs in.
    pub fn workflow_phase(&self) -> Option<&str> {
        let shown = self.shown_workflow()?;
        let phase_in_shown = |step: &StepRef| {
            let mut at = step.clone();
            for _ in 0..=self.workflows.len() {
                if at.workflow == shown.id {
                    return Some(at.phase);
                }
                let workflow = self.workflows.iter().find(|w| w.id == at.workflow)?;
                at = workflow.parent.clone()?;
            }
            None
        };
        let running = |step: &StepRef| {
            self.workflows
                .iter()
                .flat_map(|workflow| &workflow.steps)
                .any(|known| known.step == *step && known.end.is_none())
        };
        let last = self.last_step.as_ref();
        let phase = last
            .filter(|step| running(step))
            .and_then(phase_in_shown)
            .or_else(|| {
                self.workflows
                    .iter()
                    .flat_map(|workflow| &workflow.steps)
                    .filter(|step| step.end.is_none())
                    .find_map(|step| phase_in_shown(&step.step))
            })
            .or_else(|| last.and_then(phase_in_shown))?;
        // A workflow that phase's step runs says which of its own phases it is in, and the
        // declared phase only held their place.
        if let Some(reported) = self.reported.in_phase(&shown.id, phase) {
            return phase_at_work(reported);
        }
        shown
            .phases
            .get(usize::try_from(phase).ok()?)
            .map(String::as_str)
    }

    /// Takes in what the runner reports of the workflow `step` runs.
    pub fn workflow_reported(&mut self, step: &StepRef, workflow: &CliWorkflowRun) {
        self.reported.report(step, workflow);
    }

    pub fn tokens_out(&self) -> u64 {
        self.sessions
            .values()
            .map(|session| session.tokens_out)
            .sum()
    }

    /// Tasks done out of all of them, skipping cancelled ones.
    pub fn tasks_done(&self) -> (usize, usize) {
        let counted = self
            .tasks
            .values()
            .filter(|task| task.state != Some(TaskState::Cancelled));
        let (done, total) = counted.fold((0, 0), |(done, total), task| {
            let merged = task.state == Some(TaskState::Merged);
            (done + usize::from(merged), total + 1)
        });
        (done, total)
    }

    pub fn approval(&self, id: &ApprovalId) -> Option<&OpenApproval> {
        self.approvals.iter().find(|open| open.opened.id == *id)
    }

    /// The call a spot check asks about, when this screen saw it made.
    pub fn decision(&self, id: &ApprovalId) -> Option<&DecisionRecord> {
        self.decisions.get(id)
    }

    /// You marked this spot check, from its question or `/approvals`, so it no longer waits.
    pub fn spot_checked(&mut self, id: &ApprovalId) {
        self.spot_checks.retain(|waiting| waiting != id);
        self.decisions.forget(id);
    }

    /// Folds one event of this run in. Returns an approval that just opened.
    pub(super) fn apply(&mut self, event: &RunEvent, replaying: bool) -> Option<ApprovalId> {
        self.seq = event.seq;
        self.updated = Some(event.ts);
        let at = event.ts;
        if let Some(key) = &event.session
            && let Some(session) = self.sessions.get_mut(key)
        {
            session.last_event = at;
        }
        match &event.kind {
            RunEventKind::Run(lifecycle) => self.apply_lifecycle(lifecycle, at, replaying),
            RunEventKind::Phase(change) => self.apply_phase(change),
            RunEventKind::Goal(_)
            | RunEventKind::Plan(_)
            | RunEventKind::RunReview(_)
            | RunEventKind::Note(_)
            | RunEventKind::AgentStatus(_) => None,
            RunEventKind::Council(council) => {
                Talk::follow(&mut self.talk, council);
                None
            }
            RunEventKind::Divide(divide) => {
                self.apply_divide(divide);
                None
            }
            RunEventKind::Task(task) => {
                if let Some(id) = &event.task_id {
                    self.apply_task(id, task);
                }
                None
            }
            RunEventKind::Session(session) => {
                if let Some(key) = &event.session {
                    self.apply_session(key, event.task_id.as_ref(), session, at);
                }
                None
            }
            RunEventKind::Agent(agent) => {
                if let Some(session) = event
                    .session
                    .as_ref()
                    .and_then(|key| self.sessions.get_mut(key))
                {
                    session.apply_agent(agent, at);
                }
                None
            }
            RunEventKind::Check(check) => {
                if let Some(task) = event.task_id.as_ref().and_then(|id| self.tasks.get_mut(id)) {
                    task.last_check = Some(CheckBrief::from(check));
                }
                None
            }
            RunEventKind::Review(review) => {
                if let ReviewEvent::Reviewed { review, .. } = review {
                    if let Some(task) = event.task_id.as_ref().and_then(|id| self.tasks.get_mut(id))
                    {
                        task.reviews += 1;
                    }
                    if let Some(session) = event
                        .session
                        .as_ref()
                        .and_then(|key| self.sessions.get_mut(key))
                    {
                        session.sent_back = review.verdict == ReviewVerdict::RequestChanges;
                    }
                }
                None
            }
            RunEventKind::Merge(merge) => {
                match merge {
                    MergeEvent::Merged { task, .. } => {
                        if let Some(task) = self.tasks.get_mut(task) {
                            task.state = Some(TaskState::Merged);
                        }
                    }
                    MergeEvent::RunMerged { target, .. } => {
                        self.merged_onto = Some(target.clone());
                    }
                    _ => {}
                }
                None
            }
            RunEventKind::Approval(approval) => self.apply_approval(approval, replaying),
            RunEventKind::Fallback(fallback) => {
                self.apply_fallback(fallback);
                None
            }
            RunEventKind::Spend(limit) => {
                if let SpendEvent::Run(limit) = limit {
                    self.spent = limit.spent;
                    self.spend = Some(limit.clone());
                }
                None
            }
            RunEventKind::Blackboard(board) => {
                if let BlackboardEvent::Delivered { message, .. } = board {
                    self.delivered(message);
                }
                None
            }
            RunEventKind::Workflow(workflow) => {
                self.apply_workflow(workflow);
                None
            }
        }
    }

    fn apply_lifecycle(
        &mut self,
        lifecycle: &RunLifecycle,
        at: OffsetDateTime,
        replaying: bool,
    ) -> Option<ApprovalId> {
        match lifecycle {
            RunLifecycle::Started(started) => {
                self.title.clone_from(&started.title);
                self.mode = Some(started.mode);
                self.outside_agent.clone_from(&started.outside_agent);
                self.workflow = Some(started.workflow.clone());
                self.started = Some(at);
            }
            RunLifecycle::Named(named) => self.named = Some(named.clone()),
            RunLifecycle::Resumed(_) => {
                // A run counts the time it worked, never the hours it waited or Argonauts
                // Code was closed.
                if let Some(since) = self.idle_since.take().or(self.ended) {
                    self.idle += Duration::try_from(at - since).unwrap_or_default();
                }
                self.waiting = None;
                self.pausing = false;
                self.paused_by = None;
                self.stopping = false;
                self.finished = None;
                self.ended = None;
            }
            RunLifecycle::PauseRequested { by, .. } => {
                self.pausing = true;
                self.paused_by = Some(by.clone());
            }
            RunLifecycle::PauseCancelled => {
                self.pausing = false;
                self.paused_by = None;
            }
            RunLifecycle::Paused { .. } => self.pausing = false,
            RunLifecycle::ApprovalModeChanged { mode, .. } => self.approval = Some(*mode),
            RunLifecycle::StopRequested { .. } => self.stopping = true,
            RunLifecycle::Waiting { reason, .. } => {
                self.waiting = Some(reason.clone());
                self.pausing = false;
                self.idle_since.get_or_insert(at);
            }
            RunLifecycle::Snapshot(snapshot) => return self.seed(snapshot, at, replaying),
            // The transcript prints it with the run's end.
            RunLifecycle::ClosingAnswer(_) => {}
            RunLifecycle::Finished { outcome, .. } => {
                self.finished = Some(*outcome);
                self.ended = Some(at);
                self.pausing = false;
                self.stopping = false;
                for session in self.sessions.values_mut() {
                    if session.is_running() {
                        session.status = SessionStatus::Stopped;
                        session.ended = Some(at);
                    }
                }
            }
        }
        None
    }

    /// A long run's snapshot stands in for the history before `upto`.
    fn seed(
        &mut self,
        snapshot: &RunSnapshot,
        at: OffsetDateTime,
        replaying: bool,
    ) -> Option<ApprovalId> {
        let summary = &snapshot.summary;
        for brief in &summary.tasks {
            let task = self
                .tasks
                .entry(brief.task.clone())
                .or_insert_with(|| TaskRow::new(brief.task.clone(), &brief.title));
            task.state = Some(brief.state);
            task.worker.clone_from(&brief.worker);
        }
        if let Some(limit) = &summary.spend {
            self.spent = limit.spent;
            self.spend = Some(limit.clone());
        }
        let mut opened = None;
        for approval in &snapshot.open_approvals {
            if self.approval(&approval.id).is_none() {
                opened = Some(approval.id.clone());
                self.approvals.push(OpenApproval {
                    opened: Box::new(approval.clone()),
                    set_aside: false,
                    replayed: replaying,
                });
            }
        }
        for key in &snapshot.running {
            if !self.sessions.contains_key(key) {
                // Only the key survives a snapshot; its next events fill in the rest.
                let worker = ModelRef::new("agent", None);
                let mut session =
                    SessionState::new(key.clone(), worker, String::new(), RoleName::Main, at);
                session.status = SessionStatus::Working;
                self.sessions.insert(key.clone(), session);
                self.session_order.push(key.clone());
            }
        }
        self.summary = Some(summary.clone());
        opened
    }

    fn apply_phase(&mut self, change: &PhaseChange) -> Option<ApprovalId> {
        if change.status == PhaseStatus::Started {
            self.phase = Some(change.phase);
        }
        None
    }

    fn apply_divide(&mut self, divide: &DivideEvent) {
        match divide {
            DivideEvent::Graph(graph) => {
                for spec in &graph.tasks {
                    self.create_task(spec);
                }
            }
            DivideEvent::Worker { task, worker, .. } => {
                if let Some(task) = self.tasks.get_mut(task) {
                    task.worker = Some(worker.clone());
                }
            }
            DivideEvent::Reviewers { .. } | DivideEvent::Predicted { .. } => {}
            DivideEvent::Limit(_) => {}
        }
    }

    fn create_task(&mut self, spec: &TaskSpec) {
        let task = self
            .tasks
            .entry(spec.id.clone())
            .or_insert_with(|| TaskRow::new(spec.id.clone(), &spec.title));
        task.title.clone_from(&spec.title);
        task.item.clone_from(&spec.item);
        if spec.way.is_some() {
            task.way.clone_from(&spec.way);
        }
        if spec.worker.is_some() {
            task.worker.clone_from(&spec.worker);
        }
        task.reviewers = u32::try_from(spec.reviewers.len()).unwrap_or(u32::MAX);
    }

    /// What a task goes by in the conversation and the panel: its worker's label, such as
    /// `build:flag`, else `build:` and its item. Its id is for Ctrl+O, the agent view, and
    /// commands, and shows only for a log from before labels.
    pub fn task_label(&self, task: &TaskId) -> String {
        let worker = self
            .sessions_in_order()
            .filter(|session| session.task.as_ref() == Some(task))
            .find(|session| session.step == Some(StepName::Build))
            .and_then(|session| session.label.clone());
        worker
            .or_else(|| {
                let item = self.tasks.get(task)?.item.as_deref()?;
                Some(format!("build:{item}"))
            })
            .unwrap_or_else(|| task.to_string())
    }

    fn apply_task(&mut self, id: &TaskId, event: &TaskEvent) {
        if let TaskEvent::Created(spec) = event {
            self.create_task(spec);
            return;
        }
        let Some(task) = self.tasks.get_mut(id) else {
            return;
        };
        match event {
            TaskEvent::Created(_) => {}
            TaskEvent::State { to, .. } => task.state = Some(*to),
            TaskEvent::Try {
                try_number,
                worker,
                cause,
                ..
            } => {
                task.try_number = *try_number;
                task.worker = Some(worker.clone());
                if *cause == TryCause::Rotation {
                    task.rotations += 1;
                    let rotations = task.rotations;
                    for session in self.sessions.values_mut() {
                        if session.task.as_ref() == Some(id) && session.is_running() {
                            session.rotations = rotations;
                        }
                    }
                }
            }
            TaskEvent::Diff { stat, .. } => task.diff = Some(*stat),
            TaskEvent::TakeOverEnded { changed, .. } => task.diff = Some(*changed),
            TaskEvent::Reassigned { from, to, .. } => task.move_to(from.clone(), to),
            TaskEvent::CheckpointTagged { n, .. } => task.checkpoints = task.checkpoints.max(*n),
            TaskEvent::QualityDebt(debt) => task.debt.push(debt.clone()),
            TaskEvent::DebtCleared { .. } => task.debt.clear(),
            TaskEvent::Todos { done, total } => task.todos = Some((*done, *total)),
            TaskEvent::Worktree { .. }
            | TaskEvent::Committed(_)
            | TaskEvent::Reported(_)
            | TaskEvent::TakenOver { .. }
            | TaskEvent::Rewound { .. } => {}
        }
    }

    fn apply_session(
        &mut self,
        key: &SessionKey,
        task: Option<&TaskId>,
        event: &SessionEvent,
        at: OffsetDateTime,
    ) {
        if let SessionEvent::Started(started) = event {
            let mut session = SessionState::started(key.clone(), started, task.cloned(), at);
            if let Some(row) = task.and_then(|id| self.tasks.get(id)) {
                session.fallback_for.clone_from(&row.fallback_for);
                session.rotations = row.rotations;
            }
            // A job another model does in the asked one's place says so itself.
            if session.fallback_for.is_none() {
                session.fallback_for.clone_from(&started.asked);
            }
            if !self.sessions.contains_key(key) {
                self.session_order.push(key.clone());
            }
            self.sessions.insert(key.clone(), session);
            return;
        }
        if let SessionEvent::Queued(message) = event {
            if !self.queued.iter().any(|known| known.id == message.id) {
                self.queued.push(message.clone());
            }
            if let Some(session) = self.sessions.get_mut(key)
                && !session.queued.iter().any(|known| known.id == message.id)
            {
                session.queued.push(message.clone());
            }
            return;
        }
        if let SessionEvent::Delivered { message, .. } = event {
            self.delivered(message);
            return;
        }
        let Some(session) = self.sessions.get_mut(key) else {
            return;
        };
        match event {
            SessionEvent::Input(input) => {
                session.launched(input.launch.model());
                session.inputs.push((**input).clone());
            }
            SessionEvent::CliSession { id, .. } => session.cli_session = Some(id.clone()),
            SessionEvent::FileDiff(diff) => session.apply_diff(diff),
            SessionEvent::Ended(end) => session.end(end, at),
            SessionEvent::Limit(_) | SessionEvent::CarryOn(_) => {}
            SessionEvent::Started(_) | SessionEvent::Queued(_) | SessionEvent::Delivered { .. } => {
            }
        }
    }

    fn delivered(&mut self, message: &MessageId) {
        self.queued.retain(|queued| queued.id != *message);
        for session in self.sessions.values_mut() {
            session.queued.retain(|queued| queued.id != *message);
        }
    }

    fn apply_approval(&mut self, event: &ApprovalEvent, replaying: bool) -> Option<ApprovalId> {
        match event {
            ApprovalEvent::Opened(opened) => {
                if let ApprovalPacket::Merge(merge) = &opened.packet {
                    self.merge_totals = Some(MergeTotals::of(merge));
                }
                self.approvals.retain(|open| open.opened.id != opened.id);
                self.approvals.push(OpenApproval {
                    opened: opened.clone(),
                    set_aside: false,
                    replayed: replaying,
                });
                return Some(opened.id.clone());
            }
            ApprovalEvent::Checked(check) => {
                let opened = opened_in(&self.approvals, &check.id);
                self.decisions.checked(opened, check, &self.spot_checks);
            }
            ApprovalEvent::Decided(decided) => {
                let opened = opened_in(&self.approvals, &decided.id);
                self.decisions.decided(opened, decided, &self.spot_checks);
                self.close_approval(&decided.id);
            }
            ApprovalEvent::Withdrawn { id, .. } => self.close_approval(id),
            ApprovalEvent::SpotCheckQueued { id } => {
                if !self.spot_checks.contains(id) {
                    self.spot_checks.push(id.clone());
                }
            }
        }
        None
    }

    fn close_approval(&mut self, id: &ApprovalId) {
        self.approvals.retain(|open| open.opened.id != *id);
    }

    fn apply_fallback(&mut self, fallback: &FallbackEvent) {
        match fallback {
            FallbackEvent::Needed(plan) => {
                if let Some(task) = self.tasks.get_mut(&plan.task) {
                    task.waiting_on_agent = None;
                }
            }
            FallbackEvent::FellBack { task, to, .. } => {
                if let Some(task) = self.tasks.get_mut(task) {
                    let from = task.worker.clone();
                    task.move_to(from, to);
                    task.waiting_on_agent = None;
                }
            }
            FallbackEvent::Waiting {
                task,
                agent_id,
                until,
                ..
            } => {
                if let Some(task) = self.tasks.get_mut(task) {
                    task.waiting_on_agent = Some(AgentWait {
                        agent_id: agent_id.0.clone(),
                        until: *until,
                    });
                }
            }
            FallbackEvent::Note { .. }
            | FallbackEvent::JobWaiting(_)
            | FallbackEvent::JobFreed { .. } => {}
        }
    }

    fn apply_workflow(&mut self, event: &WorkflowEvent) {
        match event {
            WorkflowEvent::Started(started) => {
                let state = WorkflowState {
                    id: started.id.clone(),
                    name: started.workflow.name.clone(),
                    from: started.from.as_ref().map(|from| from.as_str().to_owned()),
                    description: started.workflow.description.clone(),
                    phases: phase_names(&started.workflow),
                    parent: started.parent.clone(),
                    steps: Vec::new(),
                };
                match self.workflow_mut(&started.id) {
                    Some(known) => *known = state,
                    None => self.workflows.push(state),
                }
                if super::reported::hands_over(&started.workflow) {
                    self.reported.expect_from(&started.id);
                }
            }
            WorkflowEvent::StepStarted(step) => {
                self.last_step = Some(step.step.clone());
                let Some(workflow) = self.workflow_mut(&step.step.workflow) else {
                    return;
                };
                let state = StepState {
                    step: step.step.clone(),
                    kind: step.kind,
                    standard: step.standard,
                    round: step.round,
                    end: None,
                    summary: None,
                };
                match workflow
                    .steps
                    .iter_mut()
                    .find(|known| known.step == step.step)
                {
                    Some(known) => *known = state,
                    None => workflow.steps.push(state),
                }
            }
            WorkflowEvent::StepFinished(step) => {
                let Some(workflow) = self.workflow_mut(&step.step.workflow) else {
                    return;
                };
                let finished = StepState {
                    step: step.step.clone(),
                    kind: step.kind,
                    standard: step.standard,
                    round: step.round,
                    end: Some(step.end),
                    summary: step.summary.clone(),
                };
                match workflow
                    .steps
                    .iter_mut()
                    .find(|known| known.step == step.step)
                {
                    Some(known) => *known = finished,
                    None => workflow.steps.push(finished),
                }
            }
            WorkflowEvent::Changed(changed) => {
                if let Some(workflow) = self.workflow_mut(&changed.id) {
                    workflow.name.clone_from(&changed.workflow.name);
                    workflow.phases = phase_names(&changed.workflow);
                    workflow
                        .description
                        .clone_from(&changed.workflow.description);
                }
            }
            WorkflowEvent::Reported(report) => {
                self.workflow_reported(&report.step, &report.workflow);
            }
            WorkflowEvent::Declared(_)
            | WorkflowEvent::PhaseStarted(_)
            | WorkflowEvent::PhaseEnded { .. }
            | WorkflowEvent::PhaseAdded { .. }
            | WorkflowEvent::PhaseSkipped { .. }
            | WorkflowEvent::Note(_) => {}
            WorkflowEvent::LeftOut(left) => self.left_out.push(left.clone()),
        }
    }

    fn workflow_mut(&mut self, id: &WorkflowId) -> Option<&mut WorkflowState> {
        self.workflows
            .iter_mut()
            .find(|workflow| workflow.id == *id)
    }
}

/// One session per step, oldest first: an earlier try at a step, one that failed or one a
/// later session with the same label followed, counts in the newer one.
fn steps(sessions: Vec<&SessionState>) -> Vec<&SessionState> {
    let tried_again = |at: usize, before: &SessionState| {
        let label = before.label.as_deref().map(str::trim).unwrap_or_default();
        !label.is_empty()
            && sessions[at + 1..].iter().any(|after| {
                let after_label = after.label.as_deref().map(str::trim).unwrap_or_default();
                let same_step = after_label == label;
                (same_step
                    && (before.status == SessionStatus::Failed
                        || after.fallback_for.as_ref() == Some(&before.worker)))
                    || (asked_again(before, after)
                        && (same_step || round_stem(after_label) == Some(label)))
            })
    };
    sessions
        .iter()
        .enumerate()
        .filter(|(at, before)| !tried_again(*at, before))
        .map(|(_, session)| *session)
        .collect()
}

/// Whether `after` is the same reviewer asked again once `before`, its round, finished: one
/// step across its rounds. Two reviewers of one round are two workers, and stay two.
fn asked_again(before: &SessionState, after: &SessionState) -> bool {
    matches!(before.role, RoleName::Reviewer | RoleName::Judge)
        && before.status == SessionStatus::Done
        && before.worker == after.worker
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: TaskId,
    pub title: String,
    pub state: Option<TaskState>,
    pub worker: Option<ModelRef>,
    pub try_number: u32,
    pub diff: Option<DiffStat>,
    /// The worker it is the fallback for, for `↺ for claude-code`.
    pub fallback_for: Option<ModelRef>,
    /// Work a weaker fallback did that still needs a review, for `◌ needs review`.
    pub debt: Vec<QualityDebt>,
    /// Checkpoint tags so far (`Task::CheckpointTagged`).
    pub checkpoints: u32,
    /// Fresh sessions its worker rotated to.
    pub rotations: u32,
    pub last_check: Option<CheckBrief>,
    pub reviews: u32,
    /// Its agent is out and the task waits for it (`Fallback::Waiting`).
    pub waiting_on_agent: Option<AgentWait>,
    /// The one-token name its labels use, such as `flag` in `build:flag`.
    pub item: Option<String>,
    /// How it is done, as its plan says. `None` in a record from before ways were kept.
    pub way: Option<TaskWay>,
    /// Its session's to-do list, done of total, as the crew last counted it.
    pub todos: Option<(u32, u32)>,
    /// The reviewers its plan gave it, each a session of the review that follows its build.
    pub reviewers: u32,
}

impl TaskRow {
    pub fn new(id: TaskId, title: &str) -> Self {
        Self {
            item: None,
            id,
            title: title.to_owned(),
            state: None,
            worker: None,
            try_number: 0,
            diff: None,
            fallback_for: None,
            debt: Vec::new(),
            checkpoints: 0,
            rotations: 0,
            last_check: None,
            reviews: 0,
            waiting_on_agent: None,
            way: None,
            todos: None,
            reviewers: 0,
        }
    }

    /// A fallback or reassignment. The first worker stays the one it is the fallback for until work
    /// goes back to that worker.
    fn move_to(&mut self, from: Option<ModelRef>, to: &ModelRef) {
        if self.fallback_for.as_ref() == Some(to) {
            self.fallback_for = None;
        } else if self.fallback_for.is_none() {
            self.fallback_for = from.filter(|from| from != to);
        }
        self.worker = Some(to.clone());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckBrief {
    pub name: String,
    pub passed: bool,
}

impl From<&CheckOutcome> for CheckBrief {
    fn from(check: &CheckOutcome) -> Self {
        Self {
            name: check.name.clone(),
            passed: check.passed,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentWait {
    pub agent_id: String,
    pub until: Option<OffsetDateTime>,
}

/// Borrows only the list, so the run's other fields can change while it's held.
fn opened_in<'a>(open: &'a [OpenApproval], id: &ApprovalId) -> Option<&'a ApprovalOpened> {
    open.iter()
        .find(|open| open.opened.id == *id)
        .map(|open| &*open.opened)
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenApproval {
    pub opened: Box<ApprovalOpened>,
    /// Esc passed it on; until it's settled it's listed under "needs you".
    pub set_aside: bool,
    /// Opened before `ReplayDone`, so it rang no bell.
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowState {
    pub id: WorkflowId,
    pub name: String,
    /// The template it came from, such as the chosen workflow Understand runs ahead of.
    pub from: Option<String>,
    pub description: String,
    /// Its phases' names, in order.
    pub phases: Vec<String>,
    /// The `workflow` step that started it, for a nested one.
    pub parent: Option<StepRef>,
    pub steps: Vec<StepState>,
}

fn phase_names(workflow: &Workflow) -> Vec<String> {
    workflow
        .phases
        .iter()
        .map(|phase| phase.name.clone())
        .collect()
}

/// A run's counts as the panel and the workflow view show them, from [`RunState::totals`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunTotals {
    pub agents_done: u32,
    pub agents_total: u32,
    pub elapsed: Option<Duration>,
    /// Tokens its agents wrote, as `↓ 177k tokens` counts them.
    pub tokens: u64,
    pub spent: Spend,
}

/// What a merge question put up: its tasks and the combined diff that lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeTotals {
    pub tasks: u32,
    pub combined: DiffStat,
}

impl MergeTotals {
    pub fn of(merge: &MergePacket) -> Self {
        Self {
            tasks: u32::try_from(merge.rows.len()).unwrap_or(u32::MAX),
            combined: merge.combined,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StepState {
    pub step: StepRef,
    pub kind: StepKind,
    pub standard: StandardPhase,
    pub round: u32,
    /// `None` while it runs.
    pub end: Option<StepEnd>,
    pub summary: Option<String>,
}

/// The phase of a workflow its CLI or a script runs that is at work, by its own name: the one
/// of the agent at work, else of the last that started, else its first, where none has yet.
pub fn phase_at_work(workflow: &CliWorkflowRun) -> Option<&str> {
    super::reported::current_phase(workflow)
        .or_else(|| workflow.phases.first().map(|phase| phase.title.as_str()))
}
