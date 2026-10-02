use std::collections::BTreeMap;
use std::time::Duration;

use argonauts_core::api::StatusSummary;
use argonauts_core::events::card::{AttemptCause, CardEvent, CardSpec, CardState, GateOutcome};
use argonauts_core::events::checkpoint::{
    CheckpointEvent, CheckpointOpened, CheckpointPacket, MergePacket,
};
use argonauts_core::events::run::{
    ParkReason, PhaseChange, RunLifecycle, RunMode, RunOutcome, RunPhase, RunSnapshot,
};
use argonauts_core::events::session::{QueuedMessage, SessionEvent};
use argonauts_core::events::shared::BlackboardEvent;
use argonauts_core::events::vendor::{BudgetEvent, HandoffEvent, RunBudget};
use argonauts_core::events::workflow::{StepEnd, WorkflowEvent};
use argonauts_core::events::{DivideEvent, IntegrationEvent, ReviewEvent, RunEvent, RunEventKind};
use argonauts_core::handoff::QualityDebt;
use argonauts_core::names::{CheckpointId, MessageId, RoleName, SessionKey, WorkerRef, WorkflowId};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{CardId, DiffStat, PhaseStatus, RunId, Spend};
use time::OffsetDateTime;

use super::decisions::{DecisionRecord, Decisions};
use super::session::{SessionState, SessionStatus};

#[derive(Debug, Clone)]
pub struct RunState {
    pub id: RunId,
    pub title: String,
    pub mode: Option<RunMode>,
    /// The chat front end that started it (`Run::Started.front_end`), shown on its row.
    pub front_end: Option<String>,
    pub started: Option<OffsetDateTime>,
    pub phase: Option<RunPhase>,
    pub pausing: bool,
    pub parked: Option<ParkReason>,
    pub finished: Option<RunOutcome>,
    /// When it finished, so its time stops there everywhere it shows.
    pub ended: Option<OffsetDateTime>,
    /// What the latest merge card put up for the merge, so the line after the merge says the same.
    pub merge_totals: Option<MergeTotals>,
    /// In the order they started.
    pub sessions: BTreeMap<SessionKey, SessionState>,
    pub session_order: Vec<SessionKey>,
    pub cards: BTreeMap<CardId, CardRow>,
    /// Checkpoints still open, oldest first.
    pub checkpoints: Vec<OpenCheckpoint>,
    /// Auto decisions queued for a spot check (`Checkpoint::SpotCheckQueued`) and not yet
    /// marked, oldest first.
    pub spot_checks: Vec<CheckpointId>,
    /// What the screen saw of recent decisions, for the spot checks that follow them.
    decisions: Decisions,
    /// The run's own workflow first, nested ones after.
    pub workflows: Vec<WorkflowState>,
    /// The workflow step that started last.
    pub last_step: Option<StepRef>,
    pub spent: Spend,
    /// Guidance and mentions not yet delivered, oldest first.
    pub queued: Vec<QueuedMessage>,
    /// The last event folded in.
    pub seq: u64,
    pub preset: Option<String>,
    /// Set by `Run::ApprovalModeChanged`; the status line's mode until then.
    pub approval: Option<ApprovalMode>,
    /// A stop was asked for and the run is winding down.
    pub stopping: bool,
    /// The latest `Budget::Run`: spend against the run's limit.
    pub budget: Option<RunBudget>,
    /// What `Run::Snapshot` said about the run, for a long run seeded without its history.
    pub summary: Option<StatusSummary>,
    /// When its last event happened.
    pub updated: Option<OffsetDateTime>,
    /// Events folded in, so the screen can tell that something arrived.
    pub events: u64,
}

impl RunState {
    pub fn new(id: RunId) -> Self {
        Self {
            id,
            title: String::new(),
            mode: None,
            front_end: None,
            started: None,
            phase: None,
            pausing: false,
            parked: None,
            finished: None,
            ended: None,
            merge_totals: None,
            sessions: BTreeMap::new(),
            session_order: Vec::new(),
            cards: BTreeMap::new(),
            checkpoints: Vec::new(),
            spot_checks: Vec::new(),
            decisions: Decisions::default(),
            workflows: Vec::new(),
            last_step: None,
            spent: Spend::default(),
            queued: Vec::new(),
            seq: 0,
            preset: None,
            approval: None,
            stopping: false,
            budget: None,
            summary: None,
            updated: None,
            events: 0,
        }
    }

    /// Sessions in the order they started.
    pub fn sessions_in_order(&self) -> impl Iterator<Item = &SessionState> {
        self.session_order
            .iter()
            .filter_map(|key| self.sessions.get(key))
    }

    pub fn running_sessions(&self) -> impl Iterator<Item = &SessionState> {
        self.sessions_in_order()
            .filter(|session| session.is_running())
    }

    /// Still going: not finished and not parked.
    pub fn is_active(&self) -> bool {
        self.finished.is_none() && self.parked.is_none()
    }

    /// What it has spent so far, the one count every line that shows the run's spend reads:
    /// the budget's, with its sessions' cost when they report more, since the budget's count
    /// can lag behind the sessions' own reports.
    pub fn spend(&self) -> Spend {
        let mut spent = self
            .budget
            .as_ref()
            .map_or(self.spent, |budget| budget.spent);
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

    /// Agents done out of all of them, its time, and its spend: what the panel row and the
    /// workflow view's header both show. `wall` is now, for a run that still goes.
    pub fn totals(&self, wall: OffsetDateTime) -> RunTotals {
        let done = self
            .sessions
            .values()
            .filter(|session| session.status == SessionStatus::Done)
            .count();
        RunTotals {
            agents_done: u32::try_from(done).unwrap_or(u32::MAX),
            agents_total: u32::try_from(self.sessions.len()).unwrap_or(u32::MAX),
            elapsed: self.elapsed(wall),
            spent: self.spend(),
        }
    }

    /// From its start to its end, or to `wall` while it goes. A run seeded without its end
    /// stops at its last session's end. `None` before its start is known.
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
        Some(Duration::try_from(end - started).unwrap_or_default())
    }

    /// The workflow the run's views open on: its own, or before the lead picks it, the one that
    /// runs first, such as the goal's alignment.
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
        shown
            .phases
            .get(usize::try_from(phase).ok()?)
            .map(String::as_str)
    }

    pub fn tokens_out(&self) -> u64 {
        self.sessions
            .values()
            .map(|session| session.tokens_out)
            .sum()
    }

    /// Cards done out of all of them, skipping cancelled ones.
    pub fn cards_done(&self) -> (usize, usize) {
        let counted = self
            .cards
            .values()
            .filter(|card| card.state != Some(CardState::Cancelled));
        let (done, total) = counted.fold((0, 0), |(done, total), card| {
            let integrated = card.state == Some(CardState::Integrated);
            (done + usize::from(integrated), total + 1)
        });
        (done, total)
    }

    pub fn checkpoint(&self, id: &CheckpointId) -> Option<&OpenCheckpoint> {
        self.checkpoints.iter().find(|open| open.opened.id == *id)
    }

    /// The call a spot check asks about, when this screen saw it made.
    pub fn decision(&self, id: &CheckpointId) -> Option<&DecisionRecord> {
        self.decisions.get(id)
    }

    /// You marked this spot check, from its card or `/approvals`, so it no longer waits.
    pub fn spot_checked(&mut self, id: &CheckpointId) {
        self.spot_checks.retain(|waiting| waiting != id);
        self.decisions.forget(id);
    }

    /// Folds one event of this run in. Returns a checkpoint that just opened.
    pub(super) fn apply(&mut self, event: &RunEvent, replaying: bool) -> Option<CheckpointId> {
        self.seq = event.seq;
        self.events += 1;
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
            | RunEventKind::Retro(_)
            | RunEventKind::Note(_)
            | RunEventKind::Vendor(_) => None,
            RunEventKind::Divide(divide) => {
                self.apply_divide(divide);
                None
            }
            RunEventKind::Card(card) => {
                if let Some(id) = &event.card_id {
                    self.apply_card(id, card);
                }
                None
            }
            RunEventKind::Session(session) => {
                if let Some(key) = &event.session {
                    self.apply_session(key, event.card_id.as_ref(), session, at);
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
            RunEventKind::Gate(gate) => {
                if let Some(card) = event.card_id.as_ref().and_then(|id| self.cards.get_mut(id)) {
                    card.last_gate = Some(GateBrief::from(gate));
                }
                None
            }
            RunEventKind::Review(review) => {
                if let ReviewEvent::Reviewed { .. } = review
                    && let Some(card) = event.card_id.as_ref().and_then(|id| self.cards.get_mut(id))
                {
                    card.reviews += 1;
                }
                None
            }
            RunEventKind::Integration(integration) => {
                if let IntegrationEvent::Merged { card, .. } = integration
                    && let Some(card) = self.cards.get_mut(card)
                {
                    card.state = Some(CardState::Integrated);
                }
                None
            }
            RunEventKind::Checkpoint(checkpoint) => self.apply_checkpoint(checkpoint, replaying),
            RunEventKind::Handoff(handoff) => {
                self.apply_handoff(handoff);
                None
            }
            RunEventKind::Budget(budget) => {
                if let BudgetEvent::Run(budget) = budget {
                    self.spent = budget.spent;
                    self.budget = Some(budget.clone());
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
    ) -> Option<CheckpointId> {
        match lifecycle {
            RunLifecycle::Started(started) => {
                self.title.clone_from(&started.title);
                self.mode = Some(started.mode);
                self.front_end.clone_from(&started.front_end);
                self.preset = Some(started.preset.clone());
                self.started = Some(at);
            }
            RunLifecycle::Resumed(_) => {
                self.parked = None;
                self.pausing = false;
                self.stopping = false;
                self.finished = None;
                self.ended = None;
            }
            RunLifecycle::PauseRequested { .. } => self.pausing = true,
            RunLifecycle::PauseCancelled => self.pausing = false,
            RunLifecycle::Paused { .. } => self.pausing = false,
            RunLifecycle::ApprovalModeChanged { mode, .. } => self.approval = Some(*mode),
            RunLifecycle::StopRequested { .. } => self.stopping = true,
            RunLifecycle::Parked { reason, .. } => {
                self.parked = Some(reason.clone());
                self.pausing = false;
            }
            RunLifecycle::Snapshot(snapshot) => return self.seed(snapshot, at, replaying),
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
    ) -> Option<CheckpointId> {
        let summary = &snapshot.summary;
        for brief in &summary.cards {
            let card = self
                .cards
                .entry(brief.card.clone())
                .or_insert_with(|| CardRow::new(brief.card.clone(), &brief.title));
            card.state = Some(brief.state);
            card.owner.clone_from(&brief.owner);
        }
        if let Some(budget) = &summary.budget {
            self.spent = budget.spent;
            self.budget = Some(budget.clone());
        }
        let mut opened = None;
        for checkpoint in &snapshot.open_checkpoints {
            if self.checkpoint(&checkpoint.id).is_none() {
                opened = Some(checkpoint.id.clone());
                self.checkpoints.push(OpenCheckpoint {
                    opened: Box::new(checkpoint.clone()),
                    set_aside: false,
                    replayed: replaying,
                });
            }
        }
        for key in &snapshot.running {
            if !self.sessions.contains_key(key) {
                // Only the key survives a snapshot; its next events fill in the rest.
                let worker = WorkerRef::new("agent", None);
                let mut session =
                    SessionState::new(key.clone(), worker, String::new(), RoleName::Default, at);
                session.status = SessionStatus::Working;
                self.sessions.insert(key.clone(), session);
                self.session_order.push(key.clone());
            }
        }
        self.summary = Some(summary.clone());
        opened
    }

    fn apply_phase(&mut self, change: &PhaseChange) -> Option<CheckpointId> {
        if change.status == PhaseStatus::Started {
            self.phase = Some(change.phase);
        }
        None
    }

    fn apply_divide(&mut self, divide: &DivideEvent) {
        match divide {
            DivideEvent::Graph(graph) => {
                for spec in &graph.cards {
                    self.create_card(spec);
                }
            }
            DivideEvent::Owner { card, worker, .. } => {
                if let Some(card) = self.cards.get_mut(card) {
                    card.owner = Some(worker.clone());
                }
            }
            DivideEvent::Reviewers { .. } | DivideEvent::Predicted { .. } => {}
            DivideEvent::Budget(_) => {}
        }
    }

    fn create_card(&mut self, spec: &CardSpec) {
        let card = self
            .cards
            .entry(spec.id.clone())
            .or_insert_with(|| CardRow::new(spec.id.clone(), &spec.title));
        card.title.clone_from(&spec.title);
        if spec.owner.is_some() {
            card.owner.clone_from(&spec.owner);
        }
    }

    fn apply_card(&mut self, id: &CardId, event: &CardEvent) {
        if let CardEvent::Created(spec) = event {
            self.create_card(spec);
            return;
        }
        let Some(card) = self.cards.get_mut(id) else {
            return;
        };
        match event {
            CardEvent::Created(_) => {}
            CardEvent::State { to, .. } => card.state = Some(*to),
            CardEvent::Attempt {
                attempt,
                worker,
                cause,
            } => {
                card.attempt = *attempt;
                card.owner = Some(worker.clone());
                if *cause == AttemptCause::Rotation {
                    card.rotations += 1;
                    let rotations = card.rotations;
                    for session in self.sessions.values_mut() {
                        if session.card.as_ref() == Some(id) && session.is_running() {
                            session.rotations = rotations;
                        }
                    }
                }
            }
            CardEvent::Diff { stat, .. } => card.diff = Some(*stat),
            CardEvent::TakeOverEnded { changed, .. } => card.diff = Some(*changed),
            CardEvent::Reassigned { from, to, .. } => card.hand_to(from.clone(), to),
            CardEvent::CheckpointTagged { n, .. } => card.checkpoints = card.checkpoints.max(*n),
            CardEvent::QualityDebt(debt) => card.debt.push(debt.clone()),
            CardEvent::DebtCleared { .. } => card.debt.clear(),
            CardEvent::Worktree { .. }
            | CardEvent::Committed(_)
            | CardEvent::Reported(_)
            | CardEvent::TakenOver { .. }
            | CardEvent::Reverted { .. } => {}
        }
    }

    fn apply_session(
        &mut self,
        key: &SessionKey,
        card: Option<&CardId>,
        event: &SessionEvent,
        at: OffsetDateTime,
    ) {
        if let SessionEvent::Started(started) = event {
            let mut session = SessionState::started(key.clone(), started, card.cloned(), at);
            if let Some(row) = card.and_then(|id| self.cards.get(id)) {
                session.standing_in_for.clone_from(&row.standing_in_for);
                session.rotations = row.rotations;
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
            SessionEvent::Input(input) => session.inputs.push((**input).clone()),
            SessionEvent::VendorSession { id, .. } => session.vendor_session = Some(id.clone()),
            SessionEvent::FileDiff(diff) => session.apply_diff(diff),
            SessionEvent::Ended(end) => session.end(end, at),
            SessionEvent::Limit(_) => {}
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

    fn apply_checkpoint(
        &mut self,
        event: &CheckpointEvent,
        replaying: bool,
    ) -> Option<CheckpointId> {
        match event {
            CheckpointEvent::Opened(opened) => {
                if let CheckpointPacket::Merge(merge) = &opened.packet {
                    self.merge_totals = Some(MergeTotals::of(merge));
                }
                self.checkpoints.retain(|open| open.opened.id != opened.id);
                self.checkpoints.push(OpenCheckpoint {
                    opened: opened.clone(),
                    set_aside: false,
                    replayed: replaying,
                });
                return Some(opened.id.clone());
            }
            CheckpointEvent::Checked(check) => {
                let opened = opened_in(&self.checkpoints, &check.id);
                self.decisions.checked(opened, check, &self.spot_checks);
            }
            CheckpointEvent::Decided(decided) => {
                let opened = opened_in(&self.checkpoints, &decided.id);
                self.decisions.decided(opened, decided, &self.spot_checks);
                self.close_checkpoint(&decided.id);
            }
            CheckpointEvent::Withdrawn { id, .. } => self.close_checkpoint(id),
            CheckpointEvent::SpotCheckQueued { id } => {
                if !self.spot_checks.contains(id) {
                    self.spot_checks.push(id.clone());
                }
            }
        }
        None
    }

    fn close_checkpoint(&mut self, id: &CheckpointId) {
        self.checkpoints.retain(|open| open.opened.id != *id);
    }

    fn apply_handoff(&mut self, handoff: &HandoffEvent) {
        match handoff {
            HandoffEvent::Needed(plan) => {
                if let Some(card) = self.cards.get_mut(&plan.card) {
                    card.waiting_on_vendor = None;
                }
            }
            HandoffEvent::HandedOff { card, to, .. } => {
                if let Some(card) = self.cards.get_mut(card) {
                    let from = card.owner.clone();
                    card.hand_to(from, to);
                    card.waiting_on_vendor = None;
                }
            }
            HandoffEvent::Waiting {
                card,
                vendor,
                until,
            } => {
                if let Some(card) = self.cards.get_mut(card) {
                    card.waiting_on_vendor = Some(VendorWait {
                        vendor: vendor.0.clone(),
                        until: *until,
                    });
                }
            }
            HandoffEvent::Packet { .. } => {}
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
        }
    }

    fn workflow_mut(&mut self, id: &WorkflowId) -> Option<&mut WorkflowState> {
        self.workflows
            .iter_mut()
            .find(|workflow| workflow.id == *id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardRow {
    pub id: CardId,
    pub title: String,
    pub state: Option<CardState>,
    pub owner: Option<WorkerRef>,
    pub attempt: u32,
    pub diff: Option<DiffStat>,
    /// The worker it stands in for after a handoff, for `↺ for claude-code`.
    pub standing_in_for: Option<WorkerRef>,
    /// Work a weaker replacement did that still needs a review, for `◌ needs review`.
    pub debt: Vec<QualityDebt>,
    /// Checkpoint tags so far (`Card::CheckpointTagged`).
    pub checkpoints: u32,
    /// Fresh sessions its owner rotated to.
    pub rotations: u32,
    pub last_gate: Option<GateBrief>,
    pub reviews: u32,
    /// Its vendor is out and the card waits for it (`Handoff::Waiting`).
    pub waiting_on_vendor: Option<VendorWait>,
}

impl CardRow {
    pub fn new(id: CardId, title: &str) -> Self {
        Self {
            id,
            title: title.to_owned(),
            state: None,
            owner: None,
            attempt: 0,
            diff: None,
            standing_in_for: None,
            debt: Vec::new(),
            checkpoints: 0,
            rotations: 0,
            last_gate: None,
            reviews: 0,
            waiting_on_vendor: None,
        }
    }

    /// A handoff or reassignment. The first owner stays the one it stands in for until work
    /// goes back to that owner.
    fn hand_to(&mut self, from: Option<WorkerRef>, to: &WorkerRef) {
        if self.standing_in_for.as_ref() == Some(to) {
            self.standing_in_for = None;
        } else if self.standing_in_for.is_none() {
            self.standing_in_for = from.filter(|from| from != to);
        }
        self.owner = Some(to.clone());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateBrief {
    pub name: String,
    pub passed: bool,
}

impl From<&GateOutcome> for GateBrief {
    fn from(gate: &GateOutcome) -> Self {
        Self {
            name: gate.name.clone(),
            passed: gate.passed,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VendorWait {
    pub vendor: String,
    pub until: Option<OffsetDateTime>,
}

/// Borrows only the list, so the run's other fields can change while it's held.
fn opened_in<'a>(open: &'a [OpenCheckpoint], id: &CheckpointId) -> Option<&'a CheckpointOpened> {
    open.iter()
        .find(|open| open.opened.id == *id)
        .map(|open| &*open.opened)
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenCheckpoint {
    pub opened: Box<CheckpointOpened>,
    /// Esc folded it to its pinned line; it's listed under "needs you".
    pub set_aside: bool,
    /// Opened before `ReplayDone`, so it rang no bell.
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowState {
    pub id: WorkflowId,
    pub name: String,
    /// The template it came from, such as the preset's workflow a goal alignment runs ahead of.
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
    pub spent: Spend,
}

/// What a merge card put up: its cards and the combined diff that lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeTotals {
    pub cards: u32,
    pub combined: DiffStat,
}

impl MergeTotals {
    pub fn of(merge: &MergePacket) -> Self {
        Self {
            cards: u32::try_from(merge.rows.len()).unwrap_or(u32::MAX),
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
