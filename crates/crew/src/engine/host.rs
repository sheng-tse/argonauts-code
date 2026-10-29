//! The API every step kind uses to act on the run: read the state, record events, ask a model
//! a schema-checked question, start and steer sessions, and run checks. Every call is
//! idempotent against the log, so a re-entered step after a crash never repeats a side effect.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::mpsc::OwnedPermit;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::engine::{RepoContext, Services};
use argonauts_core::events::agent_status::SpendEvent;
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent, ApprovalKind};
use argonauts_core::events::run::{ForkFrom, Party};
use argonauts_core::events::run::{RunLifecycle, RunMode};
use argonauts_core::events::session::{
    AskedBy, Billing, EndReason, InputPart, InputPartKind, SessionEnd, SessionEvent, SessionStarted,
};
use argonauts_core::events::task::{CheckOutcome, TaskKind};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::{self, session_files};
use argonauts_core::limits::CheckRequest;
use argonauts_core::main_sessions::{MainSession, MainSessionLease};
use argonauts_core::meter::who_pays;
use argonauts_core::names::{ModelRef, PlanLabel, RoleName, SeatId, SessionKey};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::session_runner::{
    Access, RunnerEvent, SessionControl, SessionLimits, SessionRunner, SessionSpec,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit, OnHit};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::{
    Step, StepError, StepName, StepObserver, StepOutcome, StepRequest, decode, output_schema,
};
use argonauts_core::{LimitDimension, Note, NoteLevel};
use argonauts_core::{RunId, Spend, TaskId};

use crate::blackboard::delivery::{self, StartDeliveries};
use crate::engine::commands;
use crate::engine::job_fallback::{self, Job};
use crate::engine::live::{LiveSession, pump};
use crate::engine::run_meter::NoRoom;
use crate::engine::shared::{Admission, Interrupt, Shared, StatusTarget, lock};
use crate::engine::snapshot;
use crate::error::{EngineError, EngineResult};
use crate::git::Git;
use crate::lifetime::{self, Reuse};
use crate::prompts;
use crate::prompts::instructions::Agent;
use crate::spec::{self, SpecInput};
use crate::state::RunState;
use crate::state::ids::IdKind;
use crate::state::sessions::SessionRecord;
use crate::status::folder::Folder;
use crate::status::{self, Viewer};
use crate::workflow::StepAt;

mod network;

const CONDUCTOR: &str = "argonauts-code";

/// What a session resumed after a pause or a crash is told, instead of its task again.
const CONTINUE: &str = "Argonauts Code was interrupted and has resumed this session. Carry on where \
                        you stopped and finish the task.";

#[derive(Clone)]
pub struct Host {
    shared: Arc<Shared>,
}

/// One schema-checked question to one worker: read, compare, draft, map, critique, judge,
/// review, predict, and the other LLM steps.
#[derive(Debug, Clone)]
pub struct Ask<I> {
    /// The workflow step it serves, which places the session in the workflow view.
    pub at: Option<StepAt>,
    pub role: RoleName,
    pub worker: ModelRef,
    pub task: Option<TaskId>,
    /// `ReadOnly` or `NoTools`. Until the goal (and in crew mode the plan) is approved, nothing
    /// else may start.
    pub access: Access,
    /// `None` works in the run's base worktree, never the main checkout.
    pub workdir: Option<PathBuf>,
    /// The role's prompt template, from `prompts`.
    pub template: &'static str,
    pub input: I,
    /// Extra parts after the template and the input, such as the status summary or guidance.
    pub parts: Vec<InputPart>,
    /// Continue this earlier session, such as a drafting agent revising its own plan. Blind roles
    /// are always fresh.
    pub resume: Option<SessionKey>,
    /// Start as a copy of this main session, made by its CLI, when the ask's worker runs on
    /// that CLI.
    pub fork_of: Option<ForkFrom>,
    /// Tells apart two asks of the same worker in the same step and round, for replay. Slots
    /// count up from 0 in the order the asks start, which is how the log matches them.
    pub slot: u32,
    /// Drafting agents, judges, and blind reviewers: always fresh, no native fan-out, and a mention
    /// reaches them as your text only.
    pub blind: bool,
    /// A drafting agent's own label while drafts are sealed, for what its status summary may show.
    pub label: Option<PlanLabel>,
    /// The task's item for its label, when the task has no record yet.
    pub item: Option<String>,
    /// Agents whose provider a fallback for this ask may not share: the worker of the task a
    /// review judges, or the authors of the plans a judge scores.
    pub independent_of: Vec<ModelRef>,
    /// Work the run can do without, such as the run review's proposal: its agent gets one try,
    /// and a failure of the agent's comes back at once, with no second try and no fallback,
    /// so it never holds up what comes after.
    pub optional: bool,
    /// How long the ask's session may run. Its meter stops it there, as it stops a task at its
    /// time limit, and the ask comes back with that limit hit.
    pub time_limit: Option<Duration>,
    /// The council seat it speaks for. A seat carries on its own session, found by the seat
    /// alone, so two seats of one model never share one. Seats are never blind: that would
    /// refuse every CLI that doesn't isolate.
    pub seat: Option<SeatId>,
    /// A council's check, which must never learn who made the change: no status part and no
    /// status tool, since the status names each task's model. A seat's session goes without
    /// them too, whatever this says.
    pub without_status: bool,
}

impl<I> Ask<I> {
    /// A fresh read-only ask in the base worktree.
    pub fn new(
        at: Option<StepAt>,
        role: RoleName,
        worker: ModelRef,
        template: &'static str,
        input: I,
    ) -> Self {
        Self {
            at,
            role,
            worker,
            task: None,
            access: Access::ReadOnly,
            workdir: None,
            template,
            input,
            parts: Vec::new(),
            resume: None,
            fork_of: None,
            slot: 0,
            blind: false,
            label: None,
            item: None,
            independent_of: Vec::new(),
            optional: false,
            time_limit: None,
            seat: None,
            without_status: false,
        }
    }
}

/// A decoded answer and the session that gave it.
#[derive(Debug, Clone, PartialEq)]
pub struct Answered<O> {
    pub output: O,
    pub session: SessionKey,
    pub worker: ModelRef,
    pub end: SessionEnd,
}

/// A long-running session with tools: a task's worker, an experiment, a conflict fix, or the
/// consultant.
#[derive(Debug, Clone)]
pub struct SessionPlan {
    pub at: Option<StepAt>,
    pub role: RoleName,
    pub worker: ModelRef,
    pub task: Option<TaskId>,
    pub access: Access,
    pub step: Option<StepName>,
    pub workdir: PathBuf,
    pub parts: Vec<InputPart>,
    /// The report schema the session must end with, when there is one.
    pub json_schema: Option<Value>,
    pub resume: Option<SessionKey>,
    /// As [`Ask::fork_of`].
    pub fork_of: Option<ForkFrom>,
    /// Counts up from 0 in start order, like [`Ask::slot`].
    pub slot: u32,
    /// What its row is called when its step can't say, such as the label a workflow's script
    /// gave the agent. Made unique in the run like any other.
    pub label: Option<String>,
    /// It reads a change to judge it, so it may run the checks as a reviewer does, whatever
    /// its role.
    pub reviews: bool,
    /// Starts a session of its own, never carrying one on: the merger's work is no
    /// part of the session a role keeps for the project.
    pub fresh: bool,
    /// Who built the work a reviewer or a judge reads, whose provider a fallback for this
    /// session may not share. Empty for every other job.
    pub independent_of: Vec<ModelRef>,
    /// The model the step asked, when it gave the session to `worker` before the start.
    pub in_place_of: Option<InPlaceOf>,
    /// As [`Ask::without_status`].
    pub without_status: bool,
}

/// The model a step asked for a session, and why it gave the session to another.
#[derive(Debug, Clone)]
pub struct InPlaceOf {
    pub asked: ModelRef,
    /// Read on from the asked model's name: "whose 64k window can't hold this change".
    pub why: String,
}

/// A started session. Steer or stop it with [`Host::control`]; its events are recorded as
/// they arrive.
#[derive(Debug)]
pub struct Running {
    pub key: SessionKey,
    pub worker: ModelRef,
    ended: oneshot::Receiver<EngineResult<SessionEnd>>,
}

impl Running {
    pub fn new(
        key: SessionKey,
        worker: ModelRef,
        ended: oneshot::Receiver<EngineResult<SessionEnd>>,
    ) -> Self {
        Self { key, worker, ended }
    }

    /// Waits for the end, already recorded as `Session::Ended` when this returns. A session
    /// the run paused returns `Interrupted`.
    pub async fn ended(self) -> EngineResult<SessionEnd> {
        self.ended.await.unwrap_or(Err(EngineError::Interrupted))
    }
}

/// A session that ran to its end, and who ran it last.
#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    pub key: SessionKey,
    pub worker: ModelRef,
    pub end: SessionEnd,
}

/// Whether another agent may take a long session's job over after an agent failure, or only the
/// same agent may go again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallbacks {
    /// The next agent by the job's fallback list, as asks have.
    Allowed,
    /// Only the same agent again, as for a task's worker, whose task falls back by its own
    /// recovery.
    None,
}

/// A session to start over after an agent failure, and who starts it.
struct Again {
    record: SessionRecord,
    worker: ModelRef,
    /// The internet took it: the same worker carries on in the same conversation, where the
    /// CLI gave it one, with the short note a resumed session gets. A resume that fails
    /// starts fresh, as any session its CLI lost does.
    carry_on: bool,
}

impl Again {
    fn fresh(record: SessionRecord, worker: ModelRef) -> Self {
        Self {
            record,
            worker,
            carry_on: false,
        }
    }

    /// Whether it goes on in the conversation its record holds.
    fn carries_on(&self) -> bool {
        self.carry_on
            && self.worker == self.record.started.worker
            && self.record.cli_session.is_some()
    }
}

impl Job {
    /// The job of a planned session, which is never blind, so it loads your plugins as its
    /// CLI does.
    fn of(plan: &SessionPlan) -> Self {
        Self {
            at: plan.at.clone(),
            role: plan.role,
            step: plan.step,
            task: plan.task.clone(),
            independent_of: plan.independent_of.clone(),
            isolated: false,
        }
    }
}

/// What makes two sessions the same one for replay: the step and its round, the role, the
/// task, the worker, the step name, and the slot.
#[derive(Debug, Clone, PartialEq)]
struct Identity {
    at: Option<StepAt>,
    role: RoleName,
    task: Option<TaskId>,
    worker: ModelRef,
    step: Option<StepName>,
    slot: u32,
    /// Only for the label, when the task has no record yet.
    item: Option<String>,
    /// The label its plan gave it, which wins over the step's pattern.
    given_label: Option<String>,
    /// The council seat it speaks for, whose own session it carries on.
    seat: Option<SeatId>,
}

impl Identity {
    fn of(plan: &SessionPlan) -> Self {
        Self {
            at: plan.at.clone(),
            role: plan.role,
            task: plan.task.clone(),
            worker: plan.worker.clone(),
            step: plan.step,
            slot: plan.slot,
            item: None,
            given_label: plan.label.clone(),
            seat: None,
        }
    }

    /// The earlier session with this identity, if the log holds one.
    fn find(&self, state: &RunState) -> Option<SessionRecord> {
        self.alike(state).nth(self.slot as usize).cloned()
    }

    /// Sessions with this identity in any slot, in start order.
    fn alike<'s>(&'s self, state: &'s RunState) -> impl Iterator<Item = &'s SessionRecord> + 's {
        let (from, until) = round_window(state, self.at.as_ref());
        let step_ref = self.at.as_ref().map(|at| &at.step);
        state
            .sessions
            .in_order()
            .into_iter()
            .filter(move |session| {
                session.since >= from
                    && until.is_none_or(|until| session.since < until)
                    && (session.started.workflow_step.as_ref() == step_ref
                        // A review a run recorded before reviews had their place.
                        || (self.role == RoleName::Reviewer
                            && session.started.workflow_step.is_none()))
                    && session.started.role == self.role
                    && session.task == self.task
                    && *session.asked_worker() == self.worker
                    && session.started.step == self.step
                    && session.started.seat == self.seat
            })
    }
}

/// The seqs a step's round covers: from its first `StepStarted` to the next round's.
pub(crate) fn round_window(state: &RunState, at: Option<&StepAt>) -> (u64, Option<u64>) {
    let Some(at) = at else {
        return (0, None);
    };
    let Some(workflow) = state.workflows.by_id.get(&at.step.workflow) else {
        return (0, None);
    };
    let rounds = || workflow.steps.iter().filter(|step| step.step == at.step);
    let from = rounds()
        .filter(|step| step.round == at.round)
        .map(|step| step.since)
        .min()
        .unwrap_or(0);
    let until = rounds()
        .filter(|step| step.round > at.round)
        .map(|step| step.since)
        .min();
    (from, until)
}

/// The starting agent as the prompt's assembly knows it.
fn agent_of(identity: &Identity) -> Agent<'_> {
    Agent {
        role: identity.role,
        step: identity.step,
        at: identity.at.as_ref(),
        task: identity.task.as_ref(),
    }
}

/// Everything a session start needs, from an ask or a session plan.
struct Launch {
    identity: Identity,
    /// The worker the step asked, when another runs the session in its place.
    asked: Option<ModelRef>,
    /// As [`InPlaceOf::why`], when the step chose the other before the start.
    asked_why: Option<String>,
    /// The session and the agent of a task's CLI workflow that asked for it, when one did.
    asked_by: Option<AskedBy>,
    earlier: Option<SessionRecord>,
    access: Access,
    workdir: Option<PathBuf>,
    parts: Vec<InputPart>,
    json_schema: Option<Value>,
    resume: Option<SessionKey>,
    blind: bool,
    label: Option<PlanLabel>,
    /// Asks always carry the status summary; a long session gets it only where its worker
    /// has no status tool to read it from.
    status_part: bool,
    /// As [`SessionPlan::reviews`].
    reviews: bool,
    /// As [`SessionPlan::fresh`].
    fresh: bool,
    /// As [`Ask::time_limit`].
    time_limit: Option<Duration>,
    /// What becomes of it where the run pauses because its limit has no room for it.
    refused: Refused,
    /// The main session it starts as a copy of, on the CLI that runs it.
    fork_of: Option<ForkFrom>,
    /// As [`Ask::without_status`].
    without_status: bool,
}

/// `fork` when `worker` runs on the CLI whose session it copies. A fallback on another CLI
/// can't read that session, so it starts from the brief alone.
fn on_its_cli(fork: Option<ForkFrom>, worker: &ModelRef) -> Option<ForkFrom> {
    fork.filter(|fork| fork.worker.agent == worker.agent)
}

/// What becomes of a session the run's limit has no room for, where the run pauses for that
/// rather than ending. Either way, once you raise the limit and resume, its step starts it as
/// it first would have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    /// An ask's ends paused in its own slot with no CLI session, and its step starts it afresh,
    /// with its whole prompt.
    EndsPaused,
    /// A planned session never starts and leaves nothing in the log: its step unwinds as for a
    /// pause, and a task's worker goes on with the try it had begun.
    LeftOut,
}

struct Launched {
    key: SessionKey,
    worker: Arc<dyn SessionRunner>,
    spec: SessionSpec,
    live: LiveSession,
    status: Option<StatusTarget>,
    deliveries: StartDeliveries,
    /// What's left of the run's limit can't hold it, so it ends before its process starts.
    no_room: Option<NoRoom>,
}

impl Host {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    pub fn shared(&self) -> &Shared {
        &self.shared
    }

    pub fn run_id(&self) -> &RunId {
        &self.shared.run
    }

    pub fn repo(&self) -> &RepoContext {
        &self.shared.repo
    }

    pub fn settings(&self) -> &Settings {
        &self.shared.settings
    }

    /// Who pays for the tokens of a session `worker` runs, kept on the session's start so a
    /// run's tokens split into plan, free and paid however your settings change later.
    fn who_pays(&self, worker: &dyn SessionRunner) -> Billing {
        let model = worker.model_run().model;
        who_pays(
            &self.shared.settings,
            &worker.agent_id().0,
            model.as_deref(),
        )
    }

    pub fn services(&self) -> &Services {
        &self.shared.services
    }

    pub fn git(&self) -> &Git {
        &self.shared.git
    }

    pub fn now(&self) -> OffsetDateTime {
        self.shared.services.clock.now()
    }

    /// Reads the folded state under its lock. Don't await inside `read`.
    pub fn read<R>(&self, read: impl FnOnce(&RunState) -> R) -> R {
        read(&lock(&self.shared.state))
    }

    /// Appends through the sink, folds the event into the state and the status summary, records
    /// a snapshot when one is due, and writes running sessions' status files when due.
    pub fn record(
        &self,
        task: Option<&TaskId>,
        session: Option<&SessionKey>,
        kind: RunEventKind,
    ) -> EngineResult<RunEvent> {
        self.record_as(CONDUCTOR, task, session, kind)
    }

    /// Like [`Host::record`], with the event's source, such as a worker's adapter id.
    pub fn record_as(
        &self,
        source: &str,
        task: Option<&TaskId>,
        session: Option<&SessionKey>,
        kind: RunEventKind,
    ) -> EngineResult<RunEvent> {
        // A replay's tasks never check out or name its hidden tests, whoever planned them.
        let kind = match &self.shared.replay {
            Some(terms) => terms.hide(kind),
            None => kind,
        };
        let (event, status_writes) = {
            // One lock across the append and the folds keeps them in log order.
            let mut state = lock(&self.shared.state);
            let event = self.shared.sink.record(task, session, source, kind)?;
            state.apply(&event);
            let mut folder = lock(&self.shared.status);
            folder.push(&event);
            // Right after the event that made it due, so the snapshot holds every event before it.
            self.snapshot_if_due(&mut state, &mut folder);
            let writes: Vec<_> = lock(&self.shared.status_targets)
                .values()
                .filter(|target| folder.changed_for(&target.viewer))
                .map(|target| (target.path.clone(), folder.summary(&target.viewer)))
                .collect();
            (event, writes)
        };
        for (path, summary) in status_writes {
            self.write_status(&path, &summary);
        }
        if let RunEventKind::Approval(ApprovalEvent::Decided(decided)) = &event.kind {
            self.shared.desk.wake(decided);
        }
        self.shared.changed.notify_one();
        self.meter_run(&event)?;
        Ok(event)
    }

    /// Adds a session's spend to the run's, records the run's limit when it's due, and pauses
    /// the run once its limit is spent. The pause runs beside the caller, which may hold locks
    /// the pause needs.
    fn meter_run(&self, event: &RunEvent) -> EngineResult<()> {
        let due = lock(&self.shared.run_meter).observe(event);
        let Some(limit) = due else {
            return Ok(());
        };
        let which = limit_words(limit.limit.as_ref());
        self.record(None, None, RunEventKind::Spend(SpendEvent::Run(limit)))?;
        let settled = self.read(|state| {
            state.run.paused || state.run.pause_requested.is_some() || state.run.finished.is_some()
        });
        if settled || !lock(&self.shared.run_meter).start_holding() {
            return Ok(());
        }
        self.settle_for_limit(format!("The run reached {which}"));
        Ok(())
    }

    /// Ends the run or pauses it for its spend limit, saying `why`. On autopilot a reached
    /// limit never waits for you, and `stop` says to end there; otherwise the run pauses until
    /// you raise the limit. The pause runs beside the caller, which may hold locks it needs.
    fn settle_for_limit(&self, why: String) {
        let stops = self.stops_at_limit();
        let host = self.clone();
        self.shared.tasks.spawn(async move {
            let result = match host.limit_note(&why, stops) {
                Err(err) => Err(err),
                Ok(()) if stops => commands::stop_by(&host, Party::Rules).await,
                Ok(()) => match commands::begin_pause_by(&host, false, Party::Rules) {
                    Ok(true) => commands::finish_pause(&host).await,
                    Ok(false) => Ok(()),
                    Err(err) => Err(err),
                },
            };
            if let Err(err) = result {
                tracing::error!(%err, "the run's limit is spent, and settling the run failed");
            }
        });
    }

    /// On autopilot a reached limit never waits for you, and `stop` says to end there.
    fn stops_at_limit(&self) -> bool {
        self.approval_mode() == ApprovalMode::Autopilot
            || self.settings().limits.spend.when_reached == OnHit::Stop
    }

    /// Says why the run ends or pauses for its limit, and how to carry on.
    fn limit_note(&self, why: &str, stops: bool) -> EngineResult<()> {
        let text = if stops {
            format!(
                "{why}, so it ends here with its work kept. Raise the limit with /limits and ask \
                 again for what's left."
            )
        } else {
            format!(
                "{why}, so it pauses here with its work kept. Raise the limit with /limits and \
                 resume it to carry on."
            )
        };
        let note = Note {
            level: NoteLevel::Warn,
            text,
        };
        self.record(None, None, RunEventKind::Note(note))
            .map(|_| ())
    }

    /// Ends a session the run's limit has no room for before its process starts, and ends or
    /// pauses the run as a spent limit does, once. Where the run pauses, an ask's ends paused
    /// as [`Refused::EndsPaused`] says; otherwise it ends at its limit.
    fn no_room(
        &self,
        key: &SessionKey,
        task: Option<&TaskId>,
        spec: &SessionSpec,
        no_room: NoRoom,
        refused: Refused,
    ) -> EngineResult<SessionEnd> {
        let stops = self.stops_at_limit();
        let waits = refused == Refused::EndsPaused && self.pauses_for_no_room();
        let why = self.settles_for_no_room(&spec.model, no_room);
        let paused_here = match why.as_deref() {
            Some(why) if !stops => self.begin_pause_for_no_room(why)?,
            _ => false,
        };
        let end = SessionEnd {
            reason: if waits {
                EndReason::Paused
            } else {
                EndReason::LimitHit {
                    dimension: LimitDimension::Spend,
                }
            },
            cli_session: spec
                .resume
                .clone()
                .filter(|_| refused != Refused::EndsPaused),
            ..crashed_end("")
        };
        let end = self.finish_session(key, task, end)?;
        if paused_here {
            self.finish_pause_beside();
        } else if let Some(why) = why
            && stops
        {
            self.settle_for_limit(why);
        }
        Ok(end)
    }

    /// Pauses the run for a planned session its limit has no room for. That session never
    /// started, so the pause waits only for the others.
    fn pause_for_no_room(&self, worker: &ModelRef, no_room: NoRoom) -> EngineResult<()> {
        if let Some(why) = self.settles_for_no_room(worker, no_room)
            && self.begin_pause_for_no_room(&why)?
        {
            self.finish_pause_beside();
        }
        Ok(())
    }

    /// Where the run's limit has no room for a session, whether the run pauses for that: it
    /// doesn't end there, and isn't over already.
    fn pauses_for_no_room(&self) -> bool {
        !self.stops_at_limit() && self.read(|state| state.run.finished.is_none())
    }

    /// Why a session of `worker` can't start, where this refusal is the one that settles the
    /// run: no pause is under way, none began for the limit since the run last resumed, and
    /// the run isn't over.
    fn settles_for_no_room(&self, worker: &ModelRef, no_room: NoRoom) -> Option<String> {
        let settled = self.read(|state| {
            state.run.paused || state.run.pause_requested.is_some() || state.run.finished.is_some()
        });
        (!settled && lock(&self.shared.run_meter).hold())
            .then(|| self.no_room_words(worker, no_room))
    }

    /// Says why and begins the pause before the step unwinds, so the workflow it re-enters
    /// waits for the resume instead of finding no room again at once. True where this began it.
    fn begin_pause_for_no_room(&self, why: &str) -> EngineResult<bool> {
        self.limit_note(why, false)?;
        commands::begin_pause_by(self, false, Party::Rules)
    }

    /// Records `Paused` beside the caller, once the sessions the pause stopped have ended.
    fn finish_pause_beside(&self) {
        let host = self.clone();
        self.shared.tasks.spawn(async move {
            if let Err(err) = commands::finish_pause(&host).await {
                tracing::error!(%err, "the run's limit has no room, and pausing it failed");
            }
        });
    }

    /// "The run can't start another session under its $0.60 spend limit: a session of Haiku 5.5
    /// on Claude Code costs about $0.36 to start here and $0.23 is left".
    fn no_room_words(&self, worker: &ModelRef, no_room: NoRoom) -> String {
        let limit = lock(&self.shared.run_meter).limit().limit;
        let cli = DisplayNames::default().cli(&worker.agent);
        let session = match job_fallback::model_name(self, worker) {
            Some(model) => format!("{model} on {cli}"),
            None => cli,
        };
        format!(
            "The run can't start another session under {}: a session of {session} costs about \
             {} to start here and {} {} left",
            limit_words(limit.as_ref()),
            amount_words(no_room.opening, no_room.left.unit),
            amount_words(no_room.left.amount, no_room.left.unit),
            if no_room.left.unit == LimitUnit::Tokens {
                "are"
            } else {
                "is"
            },
        )
    }

    /// The next number for `kind`, never one the log or this process has used.
    pub fn allocate(&self, kind: IdKind) -> EngineResult<u32> {
        let seen = self.read(|state| state.ids.last(kind));
        let mut allocated = lock(&self.shared.allocated);
        let next = allocated
            .last(kind)
            .max(seen)
            .checked_add(1)
            .ok_or_else(|| EngineError::Refused(format!("out of {kind:?} ids")))?;
        allocated.raise(kind, next);
        Ok(next)
    }

    /// Runs one LLM step. A finished ask with the same identity (step, round, role, task,
    /// worker, slot) returns its recorded output without running again; an unfinished one
    /// resumes by its CLI session id. A session that ends for an agent reason never ends
    /// the run: [`job_fallback`] tries the same agent once more, then gives its job to the
    /// next agent, and has the run wait only when nobody can take it.
    pub async fn ask<S: Step>(&self, ask: Ask<S::Input>) -> EngineResult<Answered<S::Output>> {
        let Ask {
            at,
            role,
            worker,
            task,
            access,
            workdir,
            template,
            input,
            parts,
            resume,
            fork_of,
            slot,
            blind,
            label,
            item,
            independent_of,
            optional,
            time_limit,
            seat,
            without_status,
        } = ask;
        let ask = Ask {
            input: serde_json::to_string_pretty(&input)?,
            at,
            role,
            worker: job_fallback::listed(self, &worker),
            task,
            access,
            workdir,
            template,
            parts,
            resume,
            fork_of,
            slot,
            blind,
            label,
            item,
            independent_of,
            optional,
            time_limit,
            seat,
            without_status,
        };
        let identity = Identity {
            at: ask.at.clone(),
            role: ask.role,
            task: ask.task.clone(),
            worker: ask.worker.clone(),
            step: Some(S::NAME),
            slot: ask.slot,
            item: ask.item.clone(),
            given_label: None,
            seat: ask.seat.clone(),
        };
        let job = Job {
            at: ask.at.clone(),
            role: ask.role,
            step: Some(S::NAME),
            task: ask.task.clone(),
            independent_of: ask.independent_of.clone(),
            isolated: ask.blind,
        };
        let mut again = None;
        loop {
            let mut ran = None;
            match self
                .ask_once::<S>(&ask, &identity, &job, again.take(), &mut ran)
                .await
            {
                Err(EngineError::Step(err)) if agent_failure(&err) && !ask.optional => {
                    // The session that just ended, by its key: finding it by its identity could
                    // find another round's.
                    let record =
                        ran.and_then(|key| self.read(|state| state.sessions.get(&key).cloned()));
                    let Some(record) = record else {
                        return Err(EngineError::Step(err));
                    };
                    again = Some(if self.lost_the_network_in(&record).await {
                        job_fallback::hold_against_nobody(self, &record);
                        Again {
                            worker: record.started.worker.clone(),
                            record,
                            carry_on: true,
                        }
                    } else {
                        let worker = job_fallback::next(self, &record, &job).await?;
                        Again::fresh(record, worker)
                    });
                }
                answered => return answered,
            }
        }
    }

    /// One try of an ask, saying in `ran` which session it ran or replayed. `again` starts that
    /// session over, fresh, with its worker, after an agent failure; otherwise a finished session
    /// replays, an unfinished one resumes, and an agent known to be out has its job taken at
    /// once.
    async fn ask_once<S: Step>(
        &self,
        ask: &Ask<String>,
        identity: &Identity,
        job: &Job,
        again: Option<Again>,
        ran: &mut Option<SessionKey>,
    ) -> EngineResult<Answered<S::Output>> {
        let mut identity = identity.clone();
        let earlier = match again {
            // A fresh start on the same session, so the step finds it again on a replay. One
            // the internet took carries on in its conversation.
            Some(again) => {
                let carries_on = again.carries_on();
                let Again {
                    mut record, worker, ..
                } = again;
                if !carries_on {
                    record.cli_session = None;
                }
                identity.worker = worker;
                Some(record)
            }
            None => match self.earlier(&identity).await? {
                Some(record) if !record.resumable() => {
                    *ran = Some(record.key.clone());
                    return replay::<S>(&record);
                }
                Some(record) => {
                    identity.worker = record.started.worker.clone();
                    Some(record)
                }
                None => None,
            },
        };
        self.check_access(ask.access, ask.task.as_ref())?;
        if earlier.is_none() {
            if let Some(answered) = self.reuse::<S>(&identity, ask.access)? {
                return Ok(answered);
            }
            if let Some(taker) = job_fallback::instead_of(self, &identity.worker, job).await? {
                identity.worker = taker;
            }
        }
        self.wait_for_the_network(&identity.worker).await?;
        self.admitted().await?;
        let mut parts = vec![
            part(InputPartKind::Prompt, "prompt", ask.template),
            part(InputPartKind::StepInput, "input", &ask.input),
        ];
        parts.extend(ask.parts.iter().cloned());
        if earlier.is_none() {
            parts = prompts::writer::written(self, agent_of(&identity), parts).await?;
        }
        let launched = self.launch(Launch {
            asked: Some(ask.worker.clone()).filter(|asked| *asked != identity.worker),
            asked_by: None,
            asked_why: None,
            identity: identity.clone(),
            earlier,
            access: ask.access,
            workdir: ask.workdir.clone(),
            parts,
            json_schema: Some(output_schema::<S>()),
            resume: ask.resume.clone(),
            blind: ask.blind,
            fresh: false,
            label: ask.label,
            status_part: true,
            reviews: false,
            time_limit: ask.time_limit,
            refused: Refused::EndsPaused,
            fork_of: on_its_cli(ask.fork_of.clone(), &identity.worker),
            without_status: ask.without_status,
        })?;
        let Launched {
            key,
            worker,
            spec,
            live,
            status,
            deliveries,
            no_room,
        } = launched;
        *ran = Some(key.clone());
        let task = live.task.clone();
        if let Some(no_room) = no_room {
            let end = self.no_room(&key, task.as_ref(), &spec, no_room, Refused::EndsPaused)?;
            if self.cut_short(&end) {
                return Err(EngineError::Interrupted);
            }
            return Err(EngineError::Step(StepError::Session(Box::new(end))));
        }
        let (control, steering) = mpsc::channel(16);
        let cancel = live.cancel.clone();
        self.go_live(&key, LiveSession { control, ..live }, status);
        let request = StepRequest {
            worker,
            spec,
            retries_on_mismatch: 1,
            ask_limit: self.ask_limit(),
            control: Some(steering),
            cancel,
            normalize: S::normalize,
        };

        // The session runs in a task of its own, so a step dropped mid-ask still records its end.
        let host = self.clone();
        let session = key.clone();
        let halt = self.shared.halt.clone();
        let spawned = self.shared.tasks.spawn(async move {
            tokio::select! {
                ran = host.run_step(&session, task.as_ref(), request, deliveries) => ran,
                () = halt.cancelled() => Err(EngineError::Interrupted),
            }
        });
        let (end, result) = spawned
            .await
            .map_err(|err| EngineError::Refused(format!("session {key} failed: {err}")))??;
        if self.cut_short(&end) {
            return Err(EngineError::Interrupted);
        }
        let outcome = result?;
        Ok(Answered {
            output: decode::<S>(&outcome)?,
            session: key,
            worker: identity.worker,
            end,
        })
    }

    /// A replay's ask in a phase before the one its learned workflow change touches takes the
    /// answer the run its task came from recorded at the same place, when the replay's workflow
    /// starts with the same phases. Asks of one step, round, role, and task take the answers in the
    /// order they start, whichever worker asks. The ask is logged as a session that ended at once,
    /// at no cost, so a resumed step finds it like any other. An answer this step's schema no
    /// longer takes is left, and the ask goes to its worker.
    fn reuse<S: Step>(
        &self,
        identity: &Identity,
        access: Access,
    ) -> EngineResult<Option<Answered<S::Output>>> {
        let Some(recorded) = self
            .shared
            .replay
            .as_ref()
            .and_then(|terms| terms.reuse.as_ref())
        else {
            return Ok(None);
        };
        let Some(at) = identity.at.as_ref() else {
            return Ok(None);
        };
        let _one_at_a_time = lock(&self.shared.reusing);
        let answer = self.read(|state| {
            let root = state.workflows.root.as_ref()?;
            let workflow = state.workflows.by_id.get(root)?;
            let before = crate::replay_terms::phases_before(&workflow.workflow, recorded.from);
            if at.step.workflow != *root
                || at.step.phase as usize >= before.len()
                || workflow.changed_from_phase.is_some()
                || !recorded.fits(&workflow.workflow)
            {
                return None;
            }
            let (from, until) = round_window(state, Some(at));
            let taken = state
                .sessions
                .in_order()
                .into_iter()
                .filter(|session| {
                    session.since >= from
                        && until.is_none_or(|until| session.since < until)
                        && session.started.workflow_step.as_ref() == Some(&at.step)
                        && session.started.role == identity.role
                        && session.task == identity.task
                        && session.started.step == identity.step
                })
                .count();
            recorded
                .answers
                .iter()
                .filter(|answer| {
                    answer.phase == at.step.phase
                        && answer.path == at.step.path
                        && answer.round == at.round
                        && answer.role == identity.role
                        && answer.task == identity.task
                        && answer.step == identity.step
                })
                .nth(taken)
                .cloned()
        });
        let Some(answer) = answer else {
            return Ok(None);
        };
        let end = SessionEnd {
            reason: EndReason::Completed,
            structured: Some(answer.structured.clone()),
            result_text: answer.result_text,
            ..crashed_end("")
        };
        let outcome = StepOutcome {
            value: answer.structured,
            end: end.clone(),
            tries: 1,
        };
        let Ok(output) = decode::<S>(&outcome) else {
            return Ok(None);
        };
        let worker = self.shared.services.workers.get(&identity.worker)?;
        let key = SessionKey::numbered(self.allocate(IdKind::Session)?);
        let provider = worker.provider();
        let label = self.agent_label(&key, identity, &provider);
        let started = SessionStarted {
            worker: identity.worker.clone(),
            agent_id: worker.agent_id(),
            provider,
            kind: worker.kind(),
            role: identity.role,
            step: identity.step,
            access,
            lifetime: match identity.seat {
                Some(_) => SessionLifetime::Seat,
                None => lifetime::lifetime(&self.shared.settings, identity.role),
            },
            resumed_from: None,
            follow_key: None,
            workflow_step: Some(at.step.clone()),
            label: Some(label),
            asked: None,
            asked_by: None,
            asked_why: None,
            forked: false,
            seat: identity.seat.clone(),
            billing: Some(self.who_pays(worker.as_ref())),
        };
        let task = identity.task.as_ref();
        self.record(
            task,
            Some(&key),
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        )?;
        self.record(
            task,
            Some(&key),
            RunEventKind::Session(SessionEvent::Ended(Box::new(end.clone()))),
        )?;
        Ok(Some(Answered {
            output,
            session: key,
            worker: identity.worker.clone(),
            end,
        }))
    }

    /// Runs an ask's session to its end, recording its deliveries, its events and then its end.
    async fn run_step(
        &self,
        key: &SessionKey,
        task: Option<&TaskId>,
        request: StepRequest,
        deliveries: StartDeliveries,
    ) -> EngineResult<(SessionEnd, Result<StepOutcome, StepError>)> {
        let source = request.worker.model_ref().agent.clone();
        let (sender, mut events) = mpsc::unbounded_channel();
        // The observer runs on the runner's task and must not block, so it only forwards.
        let observer: StepObserver = Box::new(move |event: &RunnerEvent| {
            let _ = sender.send(event.clone());
        });
        let recorder = async {
            let mut failed = None;
            let mut deliveries = Some(deliveries);
            while let Some(event) = events.recv().await {
                if failed.is_some() {
                    continue;
                }
                // A worker's input record comes first, so by its first event the session has
                // started with its messages, and is owed what was posted while it started. A
                // start that fails sends no event.
                let recorded = deliveries
                    .take()
                    .map_or(Ok(()), |start| {
                        start.record(self)?;
                        delivery::catch_up(self, key).map(drop)
                    })
                    .and_then(|()| self.record_worker_event(&source, key, task, event));
                if let Err(err) = recorded {
                    failed = Some(err);
                }
            }
            failed.map_or(Ok(()), Err)
        };
        let steps = self.shared.services.steps.clone();
        let (result, recorded) = tokio::join!(steps.run(request, observer), recorder);

        let cli_session = self.read(|state| {
            state
                .sessions
                .get(key)
                .and_then(|session| session.cli_session.clone())
        });
        let end = match &result {
            Ok(outcome) => SessionEnd {
                // What passed the schema, so a replay decodes exactly the same answer.
                structured: Some(outcome.value.clone()),
                ..outcome.end.clone()
            },
            Err(StepError::Session(end)) => (**end).clone(),
            Err(StepError::Runner(err)) => SessionEnd {
                reason: EndReason::StartFailed {
                    message: err.to_string(),
                },
                cli_session,
                ..crashed_end("")
            },
            Err(StepError::SchemaMismatch { raw, .. }) => SessionEnd {
                reason: EndReason::Completed,
                cli_session,
                result_text: raw.clone(),
                ..crashed_end("")
            },
        };
        let end = self.finish_session(key, task, end)?;
        recorded?;
        Ok((end, result))
    }

    /// The earlier session with this identity, once it no longer runs. A step re-entered while
    /// its session from before is still stopping waits for that end, so it never starts the
    /// same session twice.
    async fn earlier(&self, identity: &Identity) -> EngineResult<Option<SessionRecord>> {
        loop {
            let earlier = self.read(|state| identity.find(state));
            let Some(key) = earlier
                .as_ref()
                .map(|record| record.key.clone())
                .filter(|key| self.shared.live.contains(key))
            else {
                return Ok(earlier);
            };
            tokio::select! {
                () = self.shared.live.wait_gone(&key) => {}
                () = self.interrupted().cancelled() => return Err(EngineError::Interrupted),
            }
        }
    }

    /// The slot for a session with this plan's identity, whatever `plan.slot` says: the
    /// newest earlier one's while it can still resume, so a restart or a pause continues it
    /// under the same key, else the next free one.
    pub fn slot_for(&self, plan: &SessionPlan) -> u32 {
        let mut identity = Identity::of(plan);
        identity.worker = job_fallback::listed(self, &identity.worker);
        self.read(|state| {
            let mut taken = 0u32;
            let mut newest_resumable = false;
            for session in identity.alike(state) {
                taken = taken.saturating_add(1);
                newest_resumable = session.resumable();
            }
            if newest_resumable { taken - 1 } else { taken }
        })
    }

    /// Starts a session and pumps its events into the log. Waits while the run is paused, and
    /// unwinds with `Interrupted` where the run pauses because its limit has no room for it.
    /// An earlier session with the same identity that never ended is resumed, not restarted.
    /// A worker that couldn't start ends the session `StartFailed`.
    pub async fn start(&self, mut plan: SessionPlan) -> EngineResult<Running> {
        plan.worker = job_fallback::listed(self, &plan.worker);
        self.start_with(plan, None, None).await
    }

    /// Runs a session to its end. One that ends for an agent reason goes again on the same
    /// session: the same agent once more after a short wait, then, when
    /// `fallbacks` allows, the next agent that may take its job, and an agent known to be out
    /// has its job taken at once. With `Fallbacks::None` the last agent end comes back, for the
    /// task's own recovery to fall back. `Err(Stalled)` when nobody can take the job.
    pub async fn run_session(
        &self,
        mut plan: SessionPlan,
        fallbacks: Fallbacks,
    ) -> EngineResult<Ran> {
        plan.worker = job_fallback::listed(self, &plan.worker);
        let job = Job::of(&plan);
        let allowed = fallbacks == Fallbacks::Allowed;
        let running = self
            .start_with(plan.clone(), None, allowed.then_some(&job))
            .await?;
        self.see_through(plan, job, allowed, running).await
    }

    /// [`Self::run_session`] for sessions that start side by side under one step, as a workflow
    /// script's agents do, where no count kept by the caller could tell them apart in the log.
    /// The session takes the next free slot among those like it, and two that start together
    /// each get their own. `known` is the session this very call started before the run
    /// stopped, which carries on in place of a new one, and `started` hears the session's key
    /// as soon as it has one, so the caller can keep it for that.
    pub async fn run_session_in_turn(
        &self,
        mut plan: SessionPlan,
        known: Option<&SessionKey>,
        fallbacks: Fallbacks,
        started: &(dyn Fn(&SessionKey) -> EngineResult<()> + Sync),
    ) -> EngineResult<Ran> {
        plan.worker = job_fallback::listed(self, &plan.worker);
        let job = Job::of(&plan);
        let allowed = fallbacks == Fallbacks::Allowed;
        // Before the turn, so a session waiting for the internet holds up no local model's.
        self.wait_for_the_network(&plan.worker).await?;
        let running = {
            let _turn = self.shared.start_turn.lock().await;
            let identity = Identity::of(&plan);
            plan.slot = self.read(|state| {
                let alike: Vec<&SessionRecord> = identity.alike(state).collect();
                let place =
                    known.and_then(|key| alike.iter().position(|session| session.key == *key));
                u32::try_from(place.unwrap_or(alike.len())).unwrap_or(u32::MAX)
            });
            self.start_with(plan.clone(), None, allowed.then_some(&job))
                .await?
        };
        started(&running.key)?;
        self.see_through(plan, job, allowed, running).await
    }

    /// Waits for a started session's end, and has it go again while it ends for an agent
    /// reason and someone can take it.
    async fn see_through(
        &self,
        plan: SessionPlan,
        job: Job,
        allowed: bool,
        mut running: Running,
    ) -> EngineResult<Ran> {
        loop {
            let (key, worker) = (running.key.clone(), running.worker.clone());
            let end = running.ended().await?;
            // You asked for it to start over, and the stop ended it: the same worker goes
            // again with a fresh conversation. One that finished first keeps its answer.
            let start_over =
                lock(&self.shared.starting_over).remove(&key) && end.reason == EndReason::Stopped;
            if !start_over && !end.reason.is_agent_failure() {
                return Ok(Ran { key, worker, end });
            }
            let Some(record) = self.read(|state| state.sessions.get(&key).cloned()) else {
                return Ok(Ran { key, worker, end });
            };
            if start_over {
                let again = Again::fresh(record, worker);
                running = self.start_with(plan.clone(), Some(again), None).await?;
                continue;
            }
            if self.lost_the_network(&worker, &end).await {
                job_fallback::hold_against_nobody(self, &record);
                let again = Again {
                    record,
                    worker,
                    carry_on: true,
                };
                running = self.start_with(plan.clone(), Some(again), None).await?;
                continue;
            }
            let next = if allowed {
                job_fallback::next(self, &record, &job).await?
            } else {
                match job_fallback::again(self, &record).await? {
                    Some(next) => next,
                    None => return Ok(Ran { key, worker, end }),
                }
            };
            let again = Again::fresh(record, next);
            running = self.start_with(plan.clone(), Some(again), None).await?;
        }
    }

    /// [`Self::start`], or with `again`, that session started over by its worker with the same
    /// prompt: in the same conversation when the plan continues one and the same agent goes
    /// again, else fresh. With `job`, an agent known to be out has the job taken at once.
    async fn start_with(
        &self,
        plan: SessionPlan,
        again: Option<Again>,
        job: Option<&Job>,
    ) -> EngineResult<Running> {
        let mut identity = Identity::of(&plan);
        let earlier = match again {
            Some(again) => {
                let carries_on = again.carries_on();
                let Again {
                    mut record, worker, ..
                } = again;
                if !carries_on && (plan.resume.is_none() || worker != record.started.worker) {
                    record.cli_session = None;
                }
                identity.worker = worker;
                Some(record)
            }
            None => {
                let earlier = self.earlier(&identity).await?;
                if let Some(record) = &earlier {
                    if let Some(end) = record.end.clone().filter(|_| !record.resumable()) {
                        let (sender, ended) = oneshot::channel();
                        let _ = sender.send(Ok(end));
                        return Ok(Running::new(
                            record.key.clone(),
                            record.started.worker.clone(),
                            ended,
                        ));
                    }
                    identity.worker = record.started.worker.clone();
                }
                earlier
            }
        };
        self.check_access(plan.access, plan.task.as_ref())?;
        if earlier.is_none()
            && let Some(job) = job
            && let Some(taker) = job_fallback::instead_of(self, &identity.worker, job).await?
        {
            identity.worker = taker;
        }
        self.wait_for_the_network(&identity.worker).await?;
        self.admitted().await?;
        let mut plan = plan;
        if earlier.is_none() {
            plan.parts = prompts::writer::written(self, agent_of(&identity), plan.parts).await?;
        }
        let running = identity.worker.clone();
        let asked_by = crate::claude_codes::asked_by(self, &plan);
        let (asked, asked_why) = match plan.in_place_of {
            Some(InPlaceOf { asked, why }) => (asked, Some(why)),
            None => (plan.worker, None),
        };
        let Launched {
            key,
            worker,
            spec,
            live,
            status,
            deliveries,
            no_room,
        } = self.launch(Launch {
            asked_by,
            asked: Some(asked).filter(|asked| *asked != identity.worker),
            asked_why,
            identity,
            earlier,
            access: plan.access,
            workdir: Some(plan.workdir),
            parts: plan.parts,
            json_schema: plan.json_schema,
            resume: plan.resume,
            blind: false,
            label: None,
            status_part: false,
            reviews: plan.reviews,
            fresh: plan.fresh,
            time_limit: None,
            refused: Refused::LeftOut,
            fork_of: on_its_cli(plan.fork_of, &running),
            without_status: plan.without_status,
        })?;
        let task = live.task.clone();
        let (sender, ended) = oneshot::channel();
        if let Some(no_room) = no_room {
            let end = self.no_room(&key, task.as_ref(), &spec, no_room, Refused::LeftOut)?;
            let _ = sender.send(Ok(end));
            return Ok(Running::new(key, running, ended));
        }
        let handle = match worker.start(spec).await {
            Ok(handle) => handle,
            Err(err) => {
                let end = SessionEnd {
                    reason: EndReason::StartFailed {
                        message: err.to_string(),
                    },
                    ..crashed_end("")
                };
                self.finish_session(&key, task.as_ref(), end.clone())?;
                let _ = sender.send(Ok(end));
                return Ok(Running::new(key, running, ended));
            }
        };
        deliveries.record(self)?;
        self.go_live(
            &key,
            LiveSession {
                control: handle.control,
                ..live
            },
            status,
        );
        let host = self.clone();
        let pumped = key.clone();
        let halt = self.shared.halt.clone();
        // Once the loop halts, the pump ends without recording more and the step gets
        // `Interrupted`.
        self.shared.tasks.spawn(async move {
            tokio::select! {
                result = pump(&host, pumped, task, handle.events, handle.ended) => {
                    let _ = sender.send(result);
                }
                () = halt.cancelled() => {}
            }
        });
        // What was posted for it while it started, which no poster could hand it until now.
        delivery::catch_up(self, &key)?;
        Ok(Running::new(key, running, ended))
    }

    /// Sends a control to a running session: a delivery, the wrap-up, stop after the tool
    /// call, or stop now.
    pub fn control(&self, session: &SessionKey, control: SessionControl) -> Result<(), String> {
        self.reserve_control(session)?.send(control);
        Ok(())
    }

    /// Holds a place in a running session's control queue. Sending into it can't fail, so a
    /// caller can record what it sends first and send after.
    pub fn reserve_control(
        &self,
        session: &SessionKey,
    ) -> Result<OwnedPermit<SessionControl>, String> {
        let live = self
            .shared
            .live
            .get(session)
            .ok_or_else(|| format!("{session} isn't running"))?;
        live.control.try_reserve_owned().map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => {
                format!("{session} has too many controls waiting; try again")
            }
            mpsc::error::TrySendError::Closed(_) => format!("{session} has ended"),
        })
    }

    /// Runs a check through the `CheckRunner` and records its outcome.
    pub async fn check(&self, request: CheckRequest) -> EngineResult<CheckOutcome> {
        let task = request.task.clone();
        let outcome = self.shared.services.checks.run(request).await;
        self.record(Some(&task), None, RunEventKind::Check(outcome.clone()))?;
        Ok(outcome)
    }

    /// Returns once new sessions may start; `Err(Interrupted)` when the run is stopping.
    pub async fn admitted(&self) -> EngineResult<()> {
        let mut admission = self.shared.admission.subscribe();
        loop {
            match *admission.borrow_and_update() {
                Admission::Open => return Ok(()),
                Admission::Closed => return Err(EngineError::Interrupted),
                Admission::Paused => {}
            }
            if admission.changed().await.is_err() {
                return Err(EngineError::Interrupted);
            }
        }
    }

    /// Cancelled on a `now` pause, a stop, or shutdown. Long waits select on it, reading it
    /// when they start.
    pub fn interrupted(&self) -> Interrupt {
        self.shared.interrupt()
    }

    /// The status summary for this viewer, rendered for a prompt part.
    pub fn status_text(&self, viewer: &Viewer) -> String {
        status::render(&lock(&self.shared.status).summary(viewer))
    }

    /// Records one worker event: deltas through the journal buffer, anything else after the
    /// session's held deltas. The engine records a session's start and end itself.
    pub fn record_worker_event(
        &self,
        source: &str,
        key: &SessionKey,
        task: Option<&TaskId>,
        mut event: RunnerEvent,
    ) -> EngineResult<()> {
        if let RunnerEvent::Agent(argonauts_core::AgentEvent::Workflow { workflow }) = &mut event {
            workflow.name = self.read(|state| {
                crate::workflow::naming::inner_workflow_name(state, &workflow.id, &workflow.name)
            });
        }
        let todos = match &event {
            RunnerEvent::Agent(argonauts_core::AgentEvent::Todo { items }) => {
                crate::task::todos::count(items)
            }
            _ => None,
        };
        let outside_note = match &event {
            RunnerEvent::Agent(agent) => self.outside_note(key, agent),
            RunnerEvent::Session(SessionEvent::Ended(_)) => {
                lock(&self.shared.outside_writes).ended(key);
                None
            }
            _ => None,
        };
        let kinds: Vec<RunEventKind> = match event {
            RunnerEvent::Agent(agent) => {
                let now = self.now();
                lock(&self.shared.deltas)
                    .push(key, agent, now)
                    .into_iter()
                    .map(RunEventKind::Agent)
                    .collect()
            }
            RunnerEvent::Session(SessionEvent::Started(_) | SessionEvent::Ended(_)) => Vec::new(),
            RunnerEvent::Session(session) => self.held_then(key, RunEventKind::Session(session)),
            RunnerEvent::AgentStatus(agent_id) => {
                self.held_then(key, RunEventKind::AgentStatus(agent_id))
            }
        };
        for kind in kinds {
            self.record_as(source, task, Some(key), kind)?;
        }
        if let (Some(task), Some(todos)) = (task, todos) {
            self.record_todos(source, key, task, todos)?;
        }
        if let Some(text) = outside_note {
            let note = RunEventKind::Note(argonauts_core::Note {
                level: argonauts_core::NoteLevel::Quiet,
                text,
            });
            self.record_as(source, task, Some(key), note)?;
        }
        Ok(())
    }

    /// Records a task's to-do count when its worker's own list changed it. A reviewer's or a
    /// checker's list is its own, and counts nothing for the task.
    fn record_todos(
        &self,
        source: &str,
        key: &SessionKey,
        task: &TaskId,
        (done, total): (u32, u32),
    ) -> EngineResult<()> {
        let changed = self.read(|state| {
            let worker = state
                .sessions
                .get(key)
                .is_some_and(|session| session.started.role == RoleName::Worker);
            let before = state.tasks.progress.get(task).and_then(|task| task.todos);
            worker && before != Some((done, total))
        });
        if !changed {
            return Ok(());
        }
        let todos =
            RunEventKind::Task(argonauts_core::events::task::TaskEvent::Todos { done, total });
        self.record_as(source, Some(task), Some(key), todos)
            .map(drop)
    }

    /// The line a worker's write outside the project leaves once it's done. Its own folder,
    /// your checkout, the run's worktrees, and Argonauts Code's state are the project's.
    fn outside_note(&self, key: &SessionKey, event: &argonauts_core::AgentEvent) -> Option<String> {
        let repo = &self.shared.repo;
        let state = repo.run_dir.ancestors().nth(3);
        let project: Vec<&Path> = [
            Some(repo.root.as_path()),
            Some(repo.worktrees.as_path()),
            state,
        ]
        .into_iter()
        .flatten()
        .collect();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        lock(&self.shared.outside_writes).follow(key, event, &project, home.as_deref())
    }

    /// The session's held deltas, then `kind`, so the log keeps the order they happened in.
    fn held_then(&self, key: &SessionKey, kind: RunEventKind) -> Vec<RunEventKind> {
        let mut kinds: Vec<RunEventKind> = lock(&self.shared.deltas)
            .flush(key)
            .into_iter()
            .map(RunEventKind::Agent)
            .collect();
        kinds.push(kind);
        kinds
    }

    /// Records held deltas that are due by now, for the loop's timer.
    pub fn flush_due_deltas(&self) -> EngineResult<()> {
        let due = lock(&self.shared.deltas).due(self.now());
        for (key, event) in due {
            let (source, task) = self.read(|state| {
                let session = state.sessions.get(&key);
                (
                    session.map(|s| s.started.worker.agent.clone()),
                    session.and_then(|s| s.task.clone()),
                )
            });
            let source = source.unwrap_or_else(|| CONDUCTOR.to_owned());
            self.record_as(
                &source,
                task.as_ref(),
                Some(&key),
                RunEventKind::Agent(event),
            )?;
        }
        Ok(())
    }

    /// The source for a session's worker events: its adapter id.
    pub fn session_source(&self, key: &SessionKey) -> String {
        self.read(|state| {
            state
                .sessions
                .get(key)
                .map(|session| session.started.worker.agent.clone())
        })
        .unwrap_or_else(|| CONDUCTOR.to_owned())
    }

    /// Records a session's end after its held deltas, and lets it go: its main-session lease
    /// released, its CLI session kept, and out of the live set. A session a pause stopped
    /// ends `Paused`, so its step resumes it.
    pub fn finish_session(
        &self,
        key: &SessionKey,
        task: Option<&TaskId>,
        mut end: SessionEnd,
    ) -> EngineResult<SessionEnd> {
        lock(&self.shared.status_targets).remove(key);
        let paused = lock(&self.shared.pausing).remove(key);
        if paused && matches!(end.reason, EndReason::Stopped | EndReason::Paused) {
            end.reason = EndReason::Paused;
        }
        let recorded = self.record_end(key, task, &end);
        // It leaves the live set only once its end is in the log, so a step re-entered meanwhile
        // finds it ended. Dropping its control sender stops it if it still runs.
        drop(self.shared.live.remove(key));
        // With one session fewer, the loop looks again at whether to wait.
        self.shared.changed.notify_one();
        recorded.map(|()| end)
    }

    fn record_end(
        &self,
        key: &SessionKey,
        task: Option<&TaskId>,
        end: &SessionEnd,
    ) -> EngineResult<()> {
        let held = lock(&self.shared.deltas).flush(key);
        let source = self.session_source(key);
        for event in held {
            self.record_as(&source, task, Some(key), RunEventKind::Agent(event))?;
        }
        lock(&self.shared.deltas).forget(key);
        self.record(
            task,
            Some(key),
            RunEventKind::Session(SessionEvent::Ended(Box::new(end.clone()))),
        )?;
        let lease = lock(&self.shared.leases).remove(key);
        if let (Some(lease), Some(cli_session)) = (lease, &end.cli_session) {
            self.keep_main_session(&lease, cli_session)?;
        }
        Ok(())
    }

    /// Whether a pause or a stop is under way.
    pub fn pausing(&self) -> bool {
        self.shared.admission() != Admission::Open
    }

    /// Whether the session ended because the run paused or is stopping, so its step unwinds
    /// with `Interrupted` instead of judging the end.
    pub fn cut_short(&self, end: &SessionEnd) -> bool {
        end.reason == EndReason::Paused
            || (end.reason == EndReason::Stopped && self.shared.admission() == Admission::Closed)
    }

    /// Goal first: until the goal is approved only read-only and no-tools sessions start. Building
    /// the Captain task (the one a `build` with `input: goal` makes) needs only the goal, even in a
    /// crew run that switched to Captain at the plan approval, and so does every build of a Captain
    /// run; other builds wait for the approved plan. Experiments settle plan disputes, so they may
    /// run once the goal is approved.
    fn check_access(&self, access: Access, task: Option<&TaskId>) -> EngineResult<()> {
        if matches!(access, Access::ReadOnly | Access::NoTools) {
            return Ok(());
        }
        let (goal, plan, captain_task, own_steps) = self.read(|state| {
            let plan = state
                .approvals
                .last_decided(ApprovalKind::Plan, None)
                .and_then(|record| record.decided.as_ref())
                .is_some_and(|decided| {
                    matches!(
                        decided.action,
                        AnswerAction::Approve | AnswerAction::ApproveWithEdits
                    )
                });
            let captain_task = task
                .and_then(|task| state.tasks.get(task))
                .is_some_and(|record| record.spec.kind == TaskKind::Captain);
            // A workflow run by its own steps carries its plan in its file or script, so no
            // plan approval of the engine's stands before its workers.
            let own_steps = state
                .workflows
                .root
                .as_ref()
                .and_then(|root| state.workflows.by_id.get(root))
                .and_then(|record| record.from.as_ref())
                .is_some_and(|from| from.as_str() == crate::script::PHASES_NAME);
            (state.goal.approved.is_some(), plan, captain_task, own_steps)
        });
        if !goal {
            return Err(EngineError::Refused(
                "build and experiment sessions wait for the approved goal".to_owned(),
            ));
        }
        let captain_run = self.shared.mode == RunMode::Captain;
        if access == Access::Build && !captain_task && !captain_run && !plan && !own_steps {
            return Err(EngineError::Refused(
                "build sessions wait for the approved plan".to_owned(),
            ));
        }
        Ok(())
    }

    /// The session's label in the conversation, the panel, and the views (`crate::labels`).
    fn agent_label(&self, key: &SessionKey, identity: &Identity, provider: &str) -> String {
        if let Some(given) = &identity.given_label {
            return self.read(|state| crate::labels::given_label(state, key, given));
        }
        self.read(|state| {
            crate::labels::session_label(
                state,
                &crate::labels::Asking {
                    key,
                    step: identity.at.as_ref().map(|at| &at.step),
                    round: identity.at.as_ref().map_or(1, |at| at.round),
                    role: identity.role,
                    step_name: identity.step,
                    task: identity.task.as_ref(),
                    item: identity.item.as_deref(),
                    worker: &identity.worker,
                    provider,
                },
            )
        })
    }

    /// Picks the key and the CLI session, builds the prompt and the spec, and records
    /// `Session::Started`.
    fn launch(&self, launch: Launch) -> EngineResult<Launched> {
        let Launch {
            identity,
            asked,
            asked_why,
            asked_by,
            earlier,
            access,
            workdir,
            parts,
            json_schema,
            resume,
            blind,
            label,
            status_part,
            reviews,
            fresh,
            time_limit,
            refused,
            fork_of,
            without_status,
        } = launch;
        // A session that carries on a seat's own speaks for that seat too, as the task a seat
        // does in the session it talked in.
        let seat = identity.seat.clone().or_else(|| {
            let carried = earlier
                .as_ref()
                .map(|record| record.key.clone())
                .or_else(|| resume.clone())?;
            self.read(|state| state.sessions.get(&carried)?.started.seat.clone())
        });
        // A seat never learns which model sits in which seat, and the status names them.
        let without_status = without_status || seat.is_some();
        // A copy of the main session is a session of its own: nothing earlier carries on.
        let fork = fork_of.filter(|_| earlier.is_none());
        let worker = self.shared.services.workers.get(&identity.worker)?;
        let settings = self.shared.settings.clone();
        let mut lease = None;
        let mut kept_label = None;
        let mut carries_on_in_role = false;
        let (earlier_key, resume, continuing) = match (&earlier, &fork) {
            (Some(record), _) => (
                Some(record.key.clone()),
                record.cli_session.clone(),
                record.cli_session.is_some(),
            ),
            (None, Some(fork)) => (None, Some(fork.cli_session.clone()), false),
            (None, None) => {
                let fresh_by_role = lifetime::always_fresh(&settings, identity.role);
                let asked = resume
                    .filter(|_| !blind && !fresh_by_role)
                    .and_then(|earlier| {
                        self.read(|state| {
                            let session = state.sessions.get(&earlier)?;
                            // Carrying on the same step's work, it keeps that row's name.
                            let same = session.started.step == identity.step
                                && session.started.role == identity.role;
                            if same && session.cli_session.is_some() {
                                kept_label.clone_from(&session.started.label);
                            }
                            let in_role = session.started.role == identity.role;
                            session
                                .cli_session
                                .clone()
                                .map(|agent_id| (agent_id, in_role))
                        })
                    });
                // Asked to carry on in its own role, the session already read that role's frame.
                carries_on_in_role = asked.as_ref().is_some_and(|(_, in_role)| *in_role);
                let asked = asked.map(|(agent_id, _)| agent_id);
                let cli_session = match asked {
                    Some(cli_session) => Some(cli_session),
                    None => {
                        // Where the session will work, as the launch settles it below.
                        let next_folder = match (&workdir, access) {
                            (Some(workdir), _) => Some(workdir.as_path()),
                            (None, Access::NoTools) => None,
                            (None, _) => Some(self.shared.repo.base_worktree.as_path()),
                        };
                        let reuse = self.read(|state| match &identity.seat {
                            Some(_) if blind || fresh => Ok(Reuse::Fresh),
                            Some(seat) => {
                                Ok(lifetime::of_seat(state, &settings, seat, next_folder))
                            }
                            None => lifetime::choose(
                                state,
                                &settings,
                                &self.shared.mains,
                                identity.role,
                                &identity.worker,
                                identity.task.as_ref(),
                                next_folder,
                                blind || fresh,
                            ),
                        })?;
                        match reuse {
                            Reuse::Fresh => None,
                            Reuse::Resume { cli_session, .. } => Some(cli_session),
                            Reuse::Main {
                                lease: held,
                                cli_session,
                            } => {
                                lease = Some(held);
                                cli_session
                            }
                        }
                    }
                };
                (None, cli_session, false)
            }
        };
        // Asked before it counts as started, when it would hold what it costs to open itself.
        let no_room =
            lock(&self.shared.run_meter).no_room_for(&identity.worker, access, resume.is_some());
        if let Some(no_room) = no_room
            && refused == Refused::LeftOut
            && self.pauses_for_no_room()
        {
            self.pause_for_no_room(&identity.worker, no_room)?;
            return Err(EngineError::Interrupted);
        }
        let key = match earlier_key {
            Some(key) => key,
            None => SessionKey::numbered(self.allocate(IdKind::Session)?),
        };

        let session_dir = layout::session_dir(&self.shared.repo.run_dir, &key);
        create_private_dir_all(&session_dir)?;
        // Its CLI finds the session it copies in the folder that session ran in.
        let workdir = fork.as_ref().map(|fork| fork.folder.clone()).or(workdir);
        let workdir = match (workdir, access) {
            (Some(workdir), _) => workdir,
            (None, Access::NoTools) => {
                let scratch = session_dir.join("work");
                create_private_dir_all(&scratch)?;
                crate::git::init_scratch_repo(&scratch)?;
                scratch
            }
            (None, _) => self.shared.repo.base_worktree.clone(),
        };
        let status_file = (worker.capabilities().status_tool && !without_status)
            .then(|| session_dir.join(session_files::STATUS));
        let task_spec = identity.task.as_ref().and_then(|task| {
            self.read(|state| state.tasks.get(task).map(|record| record.spec.clone()))
        });
        let (goal_sha256, lifetime) = self.read(|state| {
            (
                state.goal.approved_sha256().map(str::to_owned),
                if seat.is_some() {
                    SessionLifetime::Seat
                } else {
                    lifetime::lifetime(&settings, identity.role)
                },
            )
        });

        let provider = worker.provider();
        let agent_label = match kept_label {
            Some(label) => label,
            None => self.agent_label(&key, &identity, &provider),
        };
        if resume.is_none() || fork.is_some() {
            job_fallback::say_listed(self, &agent_label, &identity.worker, identity.task.as_ref())?;
        }
        let started = SessionStarted {
            worker: identity.worker.clone(),
            agent_id: worker.agent_id(),
            provider,
            kind: worker.kind(),
            role: identity.role,
            step: identity.step,
            access,
            lifetime,
            // An ask the limit has no room for never reaches the CLI session it would have
            // carried on, so its step later starts it afresh, with its whole prompt.
            resumed_from: resume
                .clone()
                .filter(|_| fork.is_none())
                .filter(|_| no_room.is_none() || refused != Refused::EndsPaused),
            follow_key: None,
            workflow_step: identity.at.as_ref().map(|at| at.step.clone()),
            label: Some(agent_label),
            asked,
            forked: fork.is_some(),
            asked_why,
            asked_by,
            seat,
            billing: Some(self.who_pays(worker.as_ref())),
        };
        self.record(
            identity.task.as_ref(),
            Some(&key),
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        )?;
        if identity.role == RoleName::Reviewer
            && let Some(task) = &identity.task
        {
            // A task's review runs beside its build, which may show it in a later phase.
            let step = self.read(|state| crate::workflow::runner::task_step(state, task));
            if let Some(step) = step {
                crate::workflow::runner::review_started(self, &step)?;
            }
        }
        if let Some(lease) = lease {
            lock(&self.shared.leases).insert(key.clone(), lease);
        }

        let live = LiveSession {
            role: identity.role,
            task: identity.task.clone(),
            blind,
            control: mpsc::channel(1).0,
            cancel: CancellationToken::new(),
        };
        // Live from its start on, before the worker starts, so a pause that begins meanwhile
        // waits for it too.
        self.shared.live.insert(key.clone(), live.clone());
        let viewer = Viewer::Session {
            role: identity.role,
            task: identity.task.clone(),
            label,
        };
        let status = status_file.clone().map(|path| StatusTarget {
            path,
            viewer: viewer.clone(),
        });
        let status_part = !without_status && (status_part || status.is_none());
        // A session carrying on already read its frame and instructions.
        let mut parts = if continuing || carries_on_in_role {
            parts
        } else {
            prompts::assemble::head(self, &agent_of(&identity), parts)
        };
        // A copy of the main session reads first that it now orchestrates the run.
        if fork.is_some() {
            parts.insert(0, crate::orchestrated::forked_opening());
        }
        let built = self
            .prompt(
                &key,
                &viewer,
                status_part,
                continuing,
                parts,
                json_schema.as_ref(),
            )
            .and_then(|(prompt, deliveries)| {
                let spec = spec::build(
                    &settings,
                    SpecInput {
                        run: self.shared.run.clone(),
                        key: key.clone(),
                        role: identity.role,
                        worker: identity.worker.clone(),
                        kind: worker.kind(),
                        task: identity.task.clone(),
                        step: identity.step,
                        access,
                        deny_read: self.deny_read(&workdir),
                        workdir,
                        session_dir,
                        prompt,
                        json_schema,
                        resume,
                        fork: fork.is_some(),
                        lifetime,
                        limits: within(
                            self.capped_by_run(
                                self.watched_for_progress(
                                    &identity,
                                    access,
                                    self.shared
                                        .services
                                        .limits
                                        .session_limits(task_spec.as_ref(), identity.role),
                                ),
                            ),
                            time_limit,
                        ),
                        goal_sha256,
                        status_file,
                        blind,
                        own_workflow: identity
                            .task
                            .as_ref()
                            .is_some_and(|task| self.shared.may_run_its_own_workflow(task)),
                        autopilot: self.approval_mode() == ApprovalMode::Autopilot,
                        reviews,
                        outside: self.outside(),
                    },
                )?;
                let mut spec = spec;
                // The never-list holds only where the session may work outside the project, as
                // its mode and its sandbox let it: a container shows it nothing outside. Only
                // there does a refusal of a path on it come from the list.
                let never =
                    argonauts_conductor::sandbox::profile::outside_reach(&spec.sandbox).never;
                lock(&self.shared.outside_writes).began(&key, &spec.workdir, never);
                let own = identity
                    .task
                    .as_ref()
                    .and_then(|task| self.shared.claude_codes_own(task));
                if let Some(own) = own.filter(|_| spec.access == Access::Build) {
                    own.apply(&mut spec);
                }
                Ok((spec, deliveries))
            });
        match built {
            Ok((spec, deliveries)) => Ok(Launched {
                key,
                worker,
                spec,
                live,
                status,
                deliveries,
                no_room,
            }),
            Err(err) => {
                let end = SessionEnd {
                    reason: EndReason::StartFailed {
                        message: err.to_string(),
                    },
                    ..crashed_end("")
                };
                self.finish_session(&key, identity.task.as_ref(), end)?;
                Err(err)
            }
        }
    }

    /// Only a worker's later try on work an earlier session committed is watched for calls
    /// that change nothing, since its work may already be done.
    fn watched_for_progress(
        &self,
        identity: &Identity,
        access: Access,
        mut limits: SessionLimits,
    ) -> SessionLimits {
        let later_try = identity.role == RoleName::Worker
            && access == Access::Build
            && identity.task.as_ref().is_some_and(|task| {
                self.read(|state| crate::task::later_try::on_committed_work(state, task))
            });
        limits.no_progress_calls = limits.no_progress_calls.filter(|_| later_try);
        limits
    }

    /// A session may spend no more than what's left of the run's limit, since the run's
    /// ceiling is as hard as a task's. It goes beside the task's own limit, since only the
    /// session's own spend counts against it. The narrator answers to its own cap.
    fn capped_by_run(&self, mut limits: SessionLimits) -> SessionLimits {
        let left = lock(&self.shared.run_meter).left();
        // A task limit in another unit stands alone.
        limits.run_left = left.filter(|left| limits.limit.is_none_or(|own| own.unit == left.unit));
        limits
    }

    /// The task's parts, the word on questions while the run decides without you, the status
    /// summary when asked for, and the messages waiting for this session. A resumed session
    /// gets a short note instead of its task again.
    fn prompt(
        &self,
        key: &SessionKey,
        viewer: &Viewer,
        status_part: bool,
        continuing: bool,
        task: Vec<InputPart>,
        answer: Option<&Value>,
    ) -> EngineResult<(Vec<InputPart>, StartDeliveries)> {
        let mut parts = if continuing {
            vec![part(InputPartKind::Message, "resumed", CONTINUE)]
        } else {
            task
        };
        // Told up front, right after its instructions, so an agent decides where it would have
        // ended its turn with a question. One that asks anyway is told again when it does
        // (`ask_limit`), and a session carried on, which starts with no instructions, was told
        // when it began.
        let after_instructions = parts
            .iter()
            .take_while(|part| part.kind.instructs())
            .count();
        if !continuing && after_instructions > 0 && self.ask_limit().is_some() {
            let template = crate::prompts::files::text(self, crate::prompts::board::DECIDE_FIRST);
            let text = crate::prompts::board::decide_first(template, answer);
            parts.insert(
                after_instructions,
                part(InputPartKind::Rules, "decide, don't ask", &text),
            );
        }
        // The request reads once: a goal that repeats what an earlier part said starts
        // after it, and the status leaves out a goal the prompt already gave.
        once_each(&mut parts);
        if status_part {
            let mut summary = lock(&self.shared.status).summary(viewer);
            if said_already(&parts, summary.goal.first().map(String::as_str)) {
                summary.goal.clear();
            }
            let text = crate::status::render(&summary);
            parts.push(part(InputPartKind::Status, "status", &text));
        }
        // A resumed session gets what was sent while it was stopped, such as guidance given to
        // a waiting run.
        let (messages, deliveries) = delivery::for_session_start(self, key)?;
        parts.extend(messages);
        Ok((parts, deliveries))
    }

    /// How many times an agent that ends its turn with a question is told to decide and carry
    /// on: `approvals.ask_limit` while the run decides without you, in auto and on autopilot,
    /// where 0 means take the recommended option at once. In supervised mode nobody replies
    /// for you, so there is none.
    pub(crate) fn ask_limit(&self) -> Option<u8> {
        self.approval_mode()
            .goes_ahead()
            .then(|| u8::try_from(self.settings().approvals.ask_limit).unwrap_or(u8::MAX))
    }

    /// The run's own mode once Shift+Tab gave it one, else what your settings say.
    fn approval_mode(&self) -> ApprovalMode {
        self.read(|state| state.run.approval_mode)
            .unwrap_or_else(|| self.settings().approvals.mode())
    }

    /// Gives the live session its control channel and writes its first status file. A session
    /// that starts while a pause or a stop is under way is told to stop too: at once when the
    /// pause cancelled its step already, otherwise after its tool call.
    fn go_live(&self, key: &SessionKey, live: LiveSession, status: Option<StatusTarget>) {
        if let Some(status) = status {
            let summary = lock(&self.shared.status).summary(&status.viewer);
            self.write_status(&status.path, &summary);
            lock(&self.shared.status_targets).insert(key.clone(), status);
        }
        let control = live.control.clone();
        let task = live.task.clone();
        let stop = if live.cancel.is_cancelled() {
            SessionControl::Stop
        } else {
            SessionControl::StopAfterTool
        };
        self.shared.live.insert(key.clone(), live);
        let admission = self.shared.admission();
        if admission == Admission::Open {
            return;
        }
        if admission == Admission::Paused {
            lock(&self.shared.pausing).insert(key.clone());
        }
        lock(&self.shared.stopped).insert(key.clone(), task);
        let _ = control.try_send(stop);
    }

    /// What the run's sessions read and write outside the project, as `[outside]` says for the
    /// run's mode. They can't ask, so where it opens outside only once asked they read there
    /// and write nothing: no question would come before such a write. Inside the project they
    /// still write only their worktrees.
    fn outside(&self) -> argonauts_core::outside::Outside {
        let Some(home) = std::env::var_os("HOME") else {
            return argonauts_core::outside::Outside::default();
        };
        argonauts_core::outside::Outside::for_mode(
            &self.settings().outside,
            self.approval_mode(),
            Some(&self.shared.repo.root),
            Path::new(&home),
        )
        .without_asking()
    }

    /// The state directory, `~/.ssh`, and every other worktree of the run, and your checkout
    /// for a session working in neither, as a no-tools one does in its own folder.
    fn deny_read(&self, workdir: &Path) -> Vec<PathBuf> {
        let repo = &self.shared.repo;
        let mut deny: Vec<PathBuf> = repo
            .run_dir
            .ancestors()
            .nth(3)
            .map(Path::to_path_buf)
            .into_iter()
            .collect();
        if let Some(home) = std::env::var_os("HOME") {
            deny.push(PathBuf::from(home).join(".ssh"));
        }
        // The sandbox keeps your checkout read-only for a session in one of its worktrees, but
        // knows it only through that worktree's git, so a checkout under a temp folder would be
        // writable for a session anywhere else.
        if !workdir.starts_with(&repo.root) && !workdir.starts_with(&repo.worktrees) {
            deny.push(repo.root.clone());
        }
        if let Ok(entries) = std::fs::read_dir(&repo.worktrees) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && !workdir.starts_with(&path) {
                    deny.push(path);
                }
            }
        }
        deny
    }

    /// A status file is advisory, so a failed write is logged and the run carries on.
    fn write_status(&self, path: &Path, summary: &argonauts_core::api::StatusSummary) {
        let now = self.now();
        if let Err(err) = lock(&self.shared.status_files).write(path, summary, now) {
            tracing::warn!(path = %path.display(), %err, "status file not written");
        }
    }

    /// Called with the state and status locks held, so nothing is appended between the event
    /// that made the snapshot due and the snapshot.
    fn snapshot_if_due(&self, state: &mut RunState, folder: &mut Folder) {
        let every = u64::from(self.shared.settings.run_host.snapshot_every);
        if !snapshot::due(state, every) {
            return;
        }
        let summary = folder.summary(&Viewer::User);
        let recorded = snapshot::build(state, summary, self.shared.live.keys()).and_then(|built| {
            let kind = RunEventKind::Run(RunLifecycle::Snapshot(Box::new(built)));
            Ok(self.shared.sink.record(None, None, CONDUCTOR, kind)?)
        });
        match recorded {
            Ok(event) => {
                state.apply(&event);
                folder.push(&event);
            }
            // A missed snapshot only makes the next resume replay more of the log.
            Err(err) => tracing::warn!(%err, "snapshot not recorded"),
        }
    }

    fn keep_main_session(&self, lease: &MainSessionLease, cli_session: &str) -> EngineResult<()> {
        let mains = &self.shared.mains;
        let earlier = mains.get(lease.role(), lease.worker());
        let main = match earlier {
            Some(main) if main.cli_session == cli_session => MainSession {
                runs: main.runs + 1,
                ..main
            },
            _ => MainSession {
                role: lease.role(),
                worker: lease.worker().clone(),
                cli_session: cli_session.to_owned(),
                started: self.now(),
                runs: 1,
            },
        };
        mains.put(&main)?;
        Ok(())
    }
}

/// A session's limits with its time capped at `limit`, where that is the tighter of the two.
fn within(mut limits: SessionLimits, limit: Option<Duration>) -> SessionLimits {
    if let Some(limit) = limit {
        let minutes = limit.as_secs_f64() / 60.0;
        limits.max_minutes = Some(limits.max_minutes.map_or(minutes, |own| own.min(minutes)));
    }
    limits
}

/// The session ended for an agent reason, or its agent couldn't start at all.
fn agent_failure(err: &StepError) -> bool {
    match err {
        StepError::Session(end) => end.reason.is_agent_failure(),
        StepError::Runner(_) => true,
        StepError::SchemaMismatch { .. } => false,
    }
}

/// A finished ask, from its recorded end: the same answer, or the same failure.
fn replay<S: Step>(record: &SessionRecord) -> EngineResult<Answered<S::Output>> {
    let end = record.end.clone().ok_or(EngineError::Interrupted)?;
    match (&end.reason, &end.structured) {
        (EndReason::Completed, Some(value)) => {
            let outcome = StepOutcome {
                value: value.clone(),
                end: end.clone(),
                tries: 1,
            };
            Ok(Answered {
                output: decode::<S>(&outcome)?,
                session: record.key.clone(),
                worker: record.started.worker.clone(),
                end,
            })
        }
        (EndReason::Completed, None) => Err(EngineError::Step(StepError::SchemaMismatch {
            raw: end.result_text.clone(),
            errors: vec!["the recorded answer didn't match the schema".to_owned()],
        })),
        _ => Err(EngineError::Step(StepError::Session(Box::new(end)))),
    }
}

/// A goal part whose first line an earlier part already holds starts after that line, and
/// goes when nothing is left of it.
fn once_each(parts: &mut Vec<InputPart>) {
    for at in 0..parts.len() {
        if parts[at].kind != InputPartKind::Goal {
            continue;
        }
        let Some(first) = parts[at]
            .text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
        else {
            continue;
        };
        if !said_already(&parts[..at], Some(&first)) {
            continue;
        }
        let text = &parts[at].text;
        let start = text.find(&first).map_or(0, |found| found + first.len());
        let rest = text[start..].trim_start_matches(['\n', '\r']).to_owned();
        parts[at].approx_tokens = u32::try_from(rest.len() / 4).unwrap_or(u32::MAX);
        parts[at].text = rest;
    }
    parts.retain(|part| part.kind != InputPartKind::Goal || !part.text.trim().is_empty());
}

/// Whether one of `parts` already says `line`, as a request given in the instructions is.
fn said_already(parts: &[InputPart], line: Option<&str>) -> bool {
    let Some(line) = line.map(str::trim).filter(|line| !line.is_empty()) else {
        return false;
    };
    parts.iter().any(|part| part.text.contains(line))
}

fn part(kind: InputPartKind, label: &str, text: &str) -> InputPart {
    InputPart {
        kind,
        label: label.to_owned(),
        text: text.to_owned(),
        approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
        source: None,
    }
}

/// An end the engine records itself, when the worker gave none.
pub fn crashed_end(message: &str) -> SessionEnd {
    SessionEnd {
        cli_session: None,
        exit_code: None,
        signal: None,
        duration_ms: 0,
        reason: EndReason::Crashed {
            message: message.to_owned(),
        },
        spend: Spend::default(),
        tool_calls: 0,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log: None,
    }
}

/// An amount in a limit's unit, as the run's notes say it.
fn amount_words(amount: f64, unit: LimitUnit) -> String {
    match unit {
        LimitUnit::Usd => format!("${amount:.2}"),
        LimitUnit::Tokens => format!("{} tokens", amount.round()),
        LimitUnit::PlanUsage => format!("{:.0}% of plan usage", amount * 100.0),
        LimitUnit::Minutes => format!("{} minutes", amount.round()),
    }
}

/// The run's limit in words, such as "its $5.00 spend limit".
fn limit_words(limit: Option<&LimitAmount>) -> String {
    let Some(limit) = limit else {
        return "the limit you set".to_owned();
    };
    match limit.unit {
        LimitUnit::Usd => format!("its ${:.2} spend limit", limit.amount),
        LimitUnit::Tokens => format!("its {} token limit", limit.amount.round()),
        LimitUnit::PlanUsage => format!("its {:.0}% plan usage limit", limit.amount * 100.0),
        LimitUnit::Minutes => format!("its {} minute limit", limit.amount.round()),
    }
}

#[cfg(test)]
mod request_once_tests {
    use super::*;

    #[test]
    fn a_goal_the_instructions_gave_reads_once() {
        let mut parts = vec![
            part(
                InputPartKind::Instructions,
                "From the orchestrator",
                "Make the limiter per key\n\nKeep the interface.",
            ),
            part(
                InputPartKind::Goal,
                "goal",
                "Make the limiter per key\n\n- each key its own bucket",
            ),
        ];
        once_each(&mut parts);
        assert_eq!(parts[1].text, "- each key its own bucket");
        assert!(said_already(&parts, Some("Make the limiter per key")));

        let mut alone = vec![part(
            InputPartKind::Goal,
            "goal",
            "Make the limiter per key",
        )];
        once_each(&mut alone);
        assert_eq!(alone.len(), 1, "nothing said it before");
        let mut twice = vec![
            part(
                InputPartKind::Instructions,
                "Instructions",
                "Make the limiter per key",
            ),
            part(InputPartKind::Goal, "goal", "Make the limiter per key\n"),
        ];
        once_each(&mut twice);
        assert_eq!(twice.len(), 1, "an empty goal goes");
    }
}
