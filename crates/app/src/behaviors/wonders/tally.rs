//! What one run's log holds that a wonder can rise from, worked out once and kept with the
//! wonders, so a log is read again only when it has grown.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::agents::runtimes::{local_route, runtime};
use argonauts_core::base::names::{ModelRef, RoleName, SeatId, SessionKey};
use argonauts_core::models::model_catalog::ModelCatalog;
use argonauts_core::models::registry::provider::is_known;
use argonauts_core::runs::events::agent_status::FallbackEvent;
use argonauts_core::runs::events::council::{CouncilEventKind, MessageKind, Speaker};
use argonauts_core::runs::events::run::{Party, ResumeCause, RunLifecycle, RunOutcome, WaitReason};
use argonauts_core::runs::events::session::{Billing, EndReason, SessionEvent};
use argonauts_core::runs::events::task::{CheckKind, MergeEvent, ReviewEvent, TaskEvent, TaskKind};
use argonauts_core::runs::events::task::{TaskState, TryCause};
use argonauts_core::runs::events::{ApprovalEvent, BlackboardEvent, RunEvent, RunEventKind};
use argonauts_core::runs::events::{WorkflowEvent, shared::BlackboardMessage};
use argonauts_core::workflows::steps::judgment::ReviewVerdict;
use argonauts_core::workflows::workflow::StepRef;
use argonauts_core::{AgentEvent, TaskId};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, Time, UtcOffset};

use super::Zone;
use crate::usage::work_ledger::filed_as;

/// Bumped whenever what a tally holds or how it is worked out changes, so kept tallies are
/// worked out again.
pub(super) const TALLY_RULES: u32 = 1;

/// A longer silence than this means the run was not working through it: a crashed host, a
/// closed laptop. A working run's sessions report far more often, even through a long build.
const LONGEST_WORKING_SILENCE: Duration = Duration::hours(1);

/// Your night, in local time: from 10:00 PM to 5:00 AM.
const NIGHT_FROM: Time = time::macros::time!(22:00);
const NIGHT_TO: Time = time::macros::time!(05:00);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Tally {
    /// When it finished with its work merged.
    #[serde(with = "time::serde::rfc3339::option")]
    pub merged_at: Option<OffsetDateTime>,
    /// The models its agents ran, busiest first.
    pub crew: Vec<Member>,
    /// Its own agents and the agents of the workflows its CLIs ran.
    pub agents: u32,
    /// How many tasks it had, when every one merged on its first try; otherwise 0.
    pub first_try_tasks: u32,
    /// The task that merged after the most check rounds failing one after another.
    pub failing_rounds: Option<TaskFeat>,
    /// Every time a guard check stopped work.
    pub guard_stops: Vec<Stamp>,
    /// Tasks that merged while a guard check of theirs still failed.
    pub guard_breaches: u32,
    /// A task caught weakening its tests that then merged with every test kept.
    pub tamper_caught: Option<TaskFeat>,
    /// The most of your night it worked through with nothing from you, in hours.
    pub night_hours: f32,
    /// The task whose model usage limits took the most times, when it merged all the same.
    pub limit_falls: Option<TaskFeat>,
    /// Its councils that agreed.
    pub councils: Vec<Council>,
    pub merged_tasks: Vec<MergedTask>,
    /// A task built on your own machine that merged after another maker reviewed it.
    pub local_reviewed: Option<TaskFeat>,
    /// It stopped, `/resume` carried it on, and its work merged.
    pub resumed_and_merged: bool,
    /// Every disagreement its judge settled.
    pub settles: Vec<Stamp>,
    /// A task its judge let stand failed its checks after it merged.
    pub let_stand_failed: bool,
}

/// A model as a run ran it, written `cli/model`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Member {
    pub model: String,
    /// As the registry names providers, such as `anthropic`.
    pub maker: String,
    pub actions: u32,
}

/// Something one task did, how many times, the models it names, and when it merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TaskFeat {
    pub times: u32,
    pub models: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// When something happened, and the model that did it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Stamp {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Council {
    /// Each maker that sat, with the model of its first seat.
    pub makers: Vec<Member>,
    /// How many times the seat that wrote least wrote, its first answer included.
    pub rounds: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub agreed: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct MergedTask {
    pub task: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// No check failed once it had merged.
    pub held: bool,
    /// A test task whose acceptance tests merged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrote_tests: bool,
}

/// One session as the tally needs it.
struct Session {
    key: SessionKey,
    model: ModelRef,
    maker: String,
    task: Option<TaskId>,
    builds: bool,
    local: bool,
    seat: Option<SeatId>,
    actions: u32,
}

/// One task as the tally follows it through the log.
#[derive(Default)]
struct Task {
    worker: Option<ModelRef>,
    merged: Option<OffsetDateTime>,
    /// A failed check, a review that sent it back, or a try a failure caused.
    stumbled: bool,
    /// Whether a check of the round since the last try failed, once one ran.
    round_failed: Option<bool>,
    failing_rounds: u32,
    most_failing_rounds: u32,
    /// Guard checks whose latest run failed.
    guards_failing: Vec<CheckKind>,
    tamper_caught: Option<ModelRef>,
    tests_kept: bool,
    /// The model a usage limit just took, until the task falls to another.
    limit_took: Option<ModelRef>,
    falls: Vec<ModelRef>,
    let_stand: bool,
    failed_after_merge: bool,
    reviewed_by: Vec<(ModelRef, String)>,
    wrote_tests: bool,
}

#[derive(Default)]
struct Talk {
    seats: Vec<(SeatId, ModelRef)>,
    written: BTreeMap<SeatId, u32>,
    agreed: Option<OffsetDateTime>,
}

/// Works out what a wonder can rise from in one run's `events`, in the order they were logged.
pub(super) fn tally(events: &[RunEvent], catalog: &ModelCatalog, zone: Zone) -> Tally {
    let mut reading = Reading {
        catalog: Some(catalog),
        zone,
        working: true,
        ..Reading::default()
    };
    for event in events {
        reading.read(event);
    }
    reading.finish()
}

#[derive(Default)]
struct Reading<'a> {
    catalog: Option<&'a ModelCatalog>,
    zone: Zone,
    sessions: Vec<Session>,
    latest: BTreeMap<SessionKey, usize>,
    tasks: BTreeMap<TaskId, Task>,
    task_order: Vec<TaskId>,
    talks: Vec<(StepRef, Talk)>,
    /// Agents of the workflows a CLI ran, by workflow, that no session of the run's own ran.
    workflow_agents: BTreeMap<String, u32>,
    workflow_models: BTreeMap<String, Vec<(ModelRef, u32)>>,
    merged_at: Option<OffsetDateTime>,
    stopped: bool,
    resumed_after_stop: bool,
    resumed_and_merged: bool,
    guard_stops: Vec<Stamp>,
    guard_breaches: u32,
    settles: Vec<Stamp>,
    /// Whether its agents can be at work: not waiting, paused or finished.
    working: bool,
    last_at: Option<OffsetDateTime>,
    /// Night seconds worked since you last did anything, and the most of them so far.
    unattended: i64,
    most_unattended: i64,
}

impl Reading<'_> {
    fn read(&mut self, event: &RunEvent) {
        self.count_the_night(event);
        let task = event.task_id.as_ref();
        match &event.kind {
            RunEventKind::Run(lifecycle) => self.lifecycle(lifecycle, event.ts),
            RunEventKind::Session(SessionEvent::Started(started)) => {
                let Some(key) = &event.session else { return };
                let model = self.filed(&started.worker);
                let local = started.billing == Some(Billing::Free)
                    || runtime(&started.worker.agent).is_some()
                    || started
                        .worker
                        .model
                        .as_deref()
                        .and_then(local_route)
                        .is_some();
                self.latest.insert(key.clone(), self.sessions.len());
                self.sessions.push(Session {
                    key: key.clone(),
                    model,
                    maker: started.provider.clone(),
                    task: task.cloned(),
                    builds: matches!(started.role, RoleName::Worker | RoleName::Main),
                    local,
                    seat: started.seat.clone(),
                    actions: 0,
                });
            }
            RunEventKind::Session(SessionEvent::Ended(end)) => {
                if matches!(end.reason, EndReason::UsageLimit { .. })
                    && let Some(session) = self.session_of(event)
                    && let Some(id) = session.task.clone().or_else(|| task.cloned())
                {
                    let model = session.model.clone();
                    self.task(&id).limit_took = Some(model);
                }
            }
            RunEventKind::Agent(AgentEvent::Init {
                model: Some(model), ..
            }) => {
                let catalog = self.catalog;
                if let Some(session) = self.session_of_mut(event) {
                    let said = ModelRef::new(session.model.agent.clone(), Some(model.clone()));
                    session.model = filed(catalog, &said);
                }
            }
            RunEventKind::Agent(AgentEvent::ToolStarted { .. }) => {
                if let Some(session) = self.session_of_mut(event) {
                    session.actions += 1;
                }
            }
            RunEventKind::Agent(AgentEvent::Workflow { workflow }) => {
                let Some(cli) = self
                    .session_of(event)
                    .map(|session| session.model.agent.clone())
                else {
                    return;
                };
                let agents = workflow.agents.len() as u32;
                let models = workflow
                    .agents
                    .iter()
                    .filter_map(|agent| {
                        let model = agent.model.clone()?;
                        Some((ModelRef::new(cli.clone(), Some(model)), agent.tool_calls))
                    })
                    .collect();
                self.cli_workflow(&workflow.id, agents, models);
            }
            RunEventKind::Workflow(WorkflowEvent::Reported(report)) => {
                let agents = report
                    .workflow
                    .agents
                    .len()
                    .saturating_sub(report.sessions.len());
                self.cli_workflow(&report.workflow.id, agents as u32, Vec::new());
            }
            RunEventKind::Task(task_event) => {
                if let Some(task) = task {
                    self.task_event(task, task_event, event.ts);
                }
            }
            RunEventKind::Check(check) => {
                if check.skipped {
                    return;
                }
                if let Some(task) = task {
                    self.check(task, check.kind, check.passed, event.ts);
                } else if check.kind == CheckKind::FullSuite && !check.passed {
                    self.failed_after_merges();
                }
            }
            RunEventKind::Review(review) => self.review(event, review),
            RunEventKind::Merge(MergeEvent::Merged { task, .. }) => {
                self.merged(task, event.ts);
            }
            RunEventKind::Fallback(FallbackEvent::FellBack { task, to, .. }) => {
                self.fell_back(task, to);
            }
            RunEventKind::Council(council) => {
                let talk = self.talk(&council.talk);
                match &council.kind {
                    CouncilEventKind::Seated(seating) => {
                        talk.seats = seating
                            .seats
                            .iter()
                            .map(|seat| (seat.id.clone(), seat.model.clone()))
                            .collect();
                    }
                    CouncilEventKind::Message(message) => {
                        if let Speaker::Seat(seat) = &message.from
                            && message.kind != MessageKind::Notes
                        {
                            *talk.written.entry(seat.clone()).or_default() += 1;
                        }
                    }
                    CouncilEventKind::Agreed { .. } => {
                        talk.agreed.get_or_insert(event.ts);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn lifecycle(&mut self, lifecycle: &RunLifecycle, at: OffsetDateTime) {
        match lifecycle {
            RunLifecycle::Finished { outcome, .. } => {
                self.working = false;
                let merged = *outcome == RunOutcome::Merged;
                self.merged_at = merged.then_some(at);
                if merged && self.resumed_after_stop {
                    self.resumed_and_merged = true;
                }
                if *outcome == RunOutcome::Stopped {
                    self.stopped = true;
                }
            }
            RunLifecycle::Waiting { reason, .. } => {
                self.working = false;
                if matches!(reason, WaitReason::NoFallback { .. }) {
                    self.stopped = true;
                }
            }
            RunLifecycle::Paused { .. } => self.working = false,
            RunLifecycle::Resumed(resumed) => {
                self.working = true;
                if self.stopped && resumed.cause == ResumeCause::User {
                    self.resumed_after_stop = true;
                }
            }
            _ => {}
        }
    }

    fn task_event(&mut self, id: &TaskId, event: &TaskEvent, at: OffsetDateTime) {
        match event {
            TaskEvent::Created(spec) => {
                if !self.tasks.contains_key(id) {
                    self.task_order.push(id.clone());
                }
                let task = self.task(id);
                task.worker = task.worker.take().or_else(|| spec.worker.clone());
                if matches!(spec.kind, TaskKind::AcceptanceTests { .. }) {
                    task.wrote_tests = true;
                }
            }
            TaskEvent::Try { worker, cause, .. } => {
                let task = self.task(id);
                task.end_round();
                task.worker = Some(worker.clone());
                if matches!(
                    cause,
                    TryCause::CheckRetry
                        | TryCause::ReviewChanges
                        | TryCause::GoalRetry
                        | TryCause::AfterFailure
                        | TryCause::Restarted
                        | TryCause::MergeFailed
                ) {
                    task.stumbled = true;
                }
                if *cause == TryCause::Fallback {
                    self.fell_back(id, worker);
                }
            }
            TaskEvent::State {
                to: TaskState::Merged,
                ..
            } => self.merged(id, at),
            _ => {}
        }
    }

    fn check(&mut self, id: &TaskId, kind: CheckKind, passed: bool, at: OffsetDateTime) {
        let task = self.task(id);
        if matches!(kind, CheckKind::AffectedTests | CheckKind::FullSuite) {
            if !passed && task.merged.is_some() {
                task.failed_after_merge = true;
            }
            return;
        }
        if passed {
            task.round_failed.get_or_insert(false);
        } else {
            task.round_failed = Some(true);
            task.stumbled = true;
        }
        let model = task.worker.clone();
        match kind {
            CheckKind::ProtectedPaths | CheckKind::SecretScan | CheckKind::Dependencies => {
                task.guards_failing.retain(|failing| *failing != kind);
                if !passed {
                    task.guards_failing.push(kind);
                    let model = model.map(|model| model.to_string()).unwrap_or_default();
                    self.guard_stops.push(Stamp { at, model });
                }
            }
            CheckKind::TestTamper => {
                if !passed {
                    task.tamper_caught = task.tamper_caught.take().or(model);
                    task.tests_kept = false;
                } else if task.tamper_caught.is_some() {
                    task.tests_kept = true;
                }
            }
            _ => {}
        }
    }

    fn review(&mut self, event: &RunEvent, review: &ReviewEvent) {
        let Some(id) = event.task_id.as_ref() else {
            if let ReviewEvent::Settled { judge, .. } = review {
                let model = self.filed(judge).to_string();
                self.settles.push(Stamp {
                    at: event.ts,
                    model,
                });
            }
            return;
        };
        match review {
            ReviewEvent::Reviewed {
                reviewer, review, ..
            } => {
                let maker = self
                    .session_of(event)
                    .map(|session| session.maker.clone())
                    .or_else(|| self.maker_of(reviewer))
                    .unwrap_or_default();
                let reviewer = self.filed(reviewer);
                let task = self.task(id);
                if review.verdict != ReviewVerdict::Approve {
                    task.stumbled = true;
                }
                task.reviewed_by.push((reviewer, maker));
            }
            ReviewEvent::Settled { judge, ruling, .. } => {
                let model = self.filed(judge).to_string();
                self.settles.push(Stamp {
                    at: event.ts,
                    model,
                });
                let task = self.task(id);
                if ruling.verdict == ReviewVerdict::Approve {
                    task.let_stand = true;
                } else {
                    task.stumbled = true;
                }
            }
            ReviewEvent::TestsWritten { .. } => self.task(id).wrote_tests = true,
        }
    }

    fn merged(&mut self, id: &TaskId, at: OffsetDateTime) {
        let task = self.task(id);
        if task.merged.is_some() {
            return;
        }
        task.end_round();
        task.merged = Some(at);
        if !task.guards_failing.is_empty() {
            self.guard_breaches += 1;
        }
    }

    fn fell_back(&mut self, id: &TaskId, to: &ModelRef) {
        let to = self.filed(to);
        let task = self.task(id);
        let Some(from) = task.limit_took.take() else {
            return;
        };
        if from == to {
            return;
        }
        if task.falls.is_empty() {
            task.falls.push(from);
        }
        task.falls.push(to);
    }

    /// The run's final check failed, so every task merged before it failed after merging.
    fn failed_after_merges(&mut self) {
        for task in self.tasks.values_mut() {
            if task.merged.is_some() {
                task.failed_after_merge = true;
            }
        }
    }

    fn cli_workflow(&mut self, id: &str, agents: u32, models: Vec<(ModelRef, u32)>) {
        let most = self.workflow_agents.entry(id.to_owned()).or_default();
        *most = (*most).max(agents);
        if !models.is_empty() {
            let models = models
                .into_iter()
                .map(|(model, actions)| (filed(self.catalog, &model), actions))
                .collect();
            self.workflow_models.insert(id.to_owned(), models);
        }
    }

    /// Adds the night between the last event and this one to the time worked without you, and
    /// starts again from nothing when this one is yours.
    fn count_the_night(&mut self, event: &RunEvent) {
        if let Some(last) = self.last_at
            && self.working
            && event.ts > last
            && event.ts - last <= LONGEST_WORKING_SILENCE
        {
            self.unattended += night_seconds(last, event.ts, self.zone.offset_at(last));
        }
        self.last_at = Some(event.ts);
        if from_you(&event.kind) {
            self.most_unattended = self.most_unattended.max(self.unattended);
            self.unattended = 0;
        }
    }

    fn talk(&mut self, step: &StepRef) -> &mut Talk {
        let at = match self.talks.iter().position(|(talk, _)| talk == step) {
            Some(at) => at,
            None => {
                self.talks.push((step.clone(), Talk::default()));
                self.talks.len() - 1
            }
        };
        &mut self.talks[at].1
    }

    fn task(&mut self, id: &TaskId) -> &mut Task {
        self.tasks.entry(id.clone()).or_default()
    }

    fn session_of(&self, event: &RunEvent) -> Option<&Session> {
        let at = *self.latest.get(event.session.as_ref()?)?;
        self.sessions.get(at)
    }

    fn session_of_mut(&mut self, event: &RunEvent) -> Option<&mut Session> {
        let at = *self.latest.get(event.session.as_ref()?)?;
        self.sessions.get_mut(at)
    }

    fn maker_of(&self, worker: &ModelRef) -> Option<String> {
        let worker = self.filed(worker);
        self.sessions
            .iter()
            .find(|session| session.model == worker)
            .map(|session| session.maker.clone())
    }

    fn filed(&self, worker: &ModelRef) -> ModelRef {
        filed(self.catalog, worker)
    }

    fn finish(mut self) -> Tally {
        self.most_unattended = self.most_unattended.max(self.unattended);
        for task in self.tasks.values_mut() {
            task.end_round();
        }
        let crew = self.crew();
        let tasks: Vec<(&TaskId, &Task)> = self
            .task_order
            .iter()
            .filter_map(|id| Some((id, self.tasks.get(id)?)))
            .collect();
        let merged_only = || {
            tasks
                .iter()
                .filter_map(|(id, task)| Some((*id, *task, task.merged?)))
        };
        let every_task_first_try = !tasks.is_empty()
            && tasks
                .iter()
                .all(|(_, task)| task.merged.is_some() && !task.stumbled);
        let model_of = |task: &Task| {
            task.worker
                .as_ref()
                .map(|worker| self.filed(worker).to_string())
        };

        let failing_rounds = merged_only()
            .filter(|(_, task, _)| task.most_failing_rounds > 0)
            .max_by_key(|(_, task, at)| (task.most_failing_rounds, std::cmp::Reverse(*at)))
            .map(|(_, task, at)| TaskFeat {
                times: task.most_failing_rounds,
                models: model_of(task).into_iter().collect(),
                at,
            });
        let tamper_caught = merged_only()
            .filter(|(_, task, _)| task.tests_kept)
            .min_by_key(|(_, _, at)| *at)
            .map(|(_, task, at)| TaskFeat {
                times: 1,
                models: task
                    .tamper_caught
                    .iter()
                    .map(|model| self.filed(model).to_string())
                    .collect(),
                at,
            });
        let limit_falls = merged_only()
            .filter(|(_, task, _)| task.falls.len() > 1)
            .max_by_key(|(_, task, at)| (task.falls.len(), std::cmp::Reverse(*at)))
            .map(|(_, task, at)| TaskFeat {
                times: task.falls.len() as u32 - 1,
                models: distinct(task.falls.iter().map(ToString::to_string)),
                at,
            });
        let local_reviewed = merged_only()
            .filter_map(|(id, task, at)| {
                let built = self.sessions.iter().find(|session| {
                    session.local && session.builds && session.task.as_ref() == Some(id)
                })?;
                let (reviewer, _) = task
                    .reviewed_by
                    .iter()
                    .find(|(_, maker)| is_known(maker) && *maker != built.maker)?;
                Some(TaskFeat {
                    times: 1,
                    models: vec![built.model.to_string(), reviewer.to_string()],
                    at,
                })
            })
            .min_by_key(|feat| feat.at);
        let merged_tasks = merged_only()
            .map(|(id, task, at)| MergedTask {
                task: id.to_string(),
                at,
                held: !task.failed_after_merge,
                wrote_tests: task.wrote_tests,
            })
            .collect();
        let let_stand_failed = self
            .tasks
            .values()
            .any(|task| task.let_stand && task.failed_after_merge);
        let first_try_tasks = if every_task_first_try {
            tasks.len() as u32
        } else {
            0
        };
        let councils = self.councils();
        let agents = self.agents();
        Tally {
            merged_at: self.merged_at,
            crew,
            agents,
            first_try_tasks,
            failing_rounds,
            guard_stops: self.guard_stops,
            guard_breaches: self.guard_breaches,
            tamper_caught,
            night_hours: self.most_unattended as f32 / 3600.0,
            limit_falls,
            councils,
            merged_tasks,
            local_reviewed,
            resumed_and_merged: self.resumed_and_merged,
            settles: self.settles,
            let_stand_failed,
        }
    }

    fn crew(&self) -> Vec<Member> {
        let mut crew: Vec<Member> = Vec::new();
        let mut add = |model: String, maker: &str, actions: u32| match crew
            .iter_mut()
            .find(|member| member.model == model)
        {
            Some(member) => member.actions += actions,
            None => crew.push(Member {
                model,
                maker: maker.to_owned(),
                actions,
            }),
        };
        for session in &self.sessions {
            add(session.model.to_string(), &session.maker, session.actions);
        }
        for models in self.workflow_models.values() {
            for (model, actions) in models {
                let maker = self.maker_of(model).unwrap_or_default();
                add(model.to_string(), &maker, *actions);
            }
        }
        // Stable, so models that did as much stay in the order they first worked.
        crew.sort_by_key(|member| std::cmp::Reverse(member.actions));
        crew
    }

    fn agents(&self) -> u32 {
        let own: BTreeSet<&SessionKey> = self.sessions.iter().map(|session| &session.key).collect();
        own.len() as u32 + self.workflow_agents.values().sum::<u32>()
    }

    fn councils(&self) -> Vec<Council> {
        self.talks
            .iter()
            .filter_map(|(_, talk)| {
                let agreed = talk.agreed?;
                let mut makers: Vec<Member> = Vec::new();
                for (seat, model) in &talk.seats {
                    let maker = self
                        .sessions
                        .iter()
                        .find(|session| session.seat.as_ref() == Some(seat))
                        .map(|session| session.maker.clone())
                        .or_else(|| self.maker_of(model))
                        .unwrap_or_default();
                    if is_known(&maker) && !makers.iter().any(|member| member.maker == maker) {
                        makers.push(Member {
                            model: self.filed(model).to_string(),
                            maker,
                            actions: 0,
                        });
                    }
                }
                let rounds = talk
                    .seats
                    .iter()
                    .map(|(seat, _)| talk.written.get(seat).copied().unwrap_or(0))
                    .min()
                    .unwrap_or(0);
                Some(Council {
                    makers,
                    rounds,
                    agreed,
                })
            })
            .collect()
    }
}

impl Task {
    fn end_round(&mut self) {
        match self.round_failed.take() {
            Some(true) => {
                self.failing_rounds += 1;
                self.most_failing_rounds = self.most_failing_rounds.max(self.failing_rounds);
            }
            Some(false) => self.failing_rounds = 0,
            None => {}
        }
    }
}

fn filed(catalog: Option<&ModelCatalog>, worker: &ModelRef) -> ModelRef {
    match catalog {
        Some(catalog) => filed_as(catalog, worker),
        None => worker.clone(),
    }
}

fn distinct(models: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = Vec::new();
    for model in models {
        if !seen.contains(&model) {
            seen.push(model);
        }
    }
    seen
}

/// Whether you did it: answered, typed, paused, stopped, took over or picked a model.
fn from_you(kind: &RunEventKind) -> bool {
    let yours = |party: &Party| matches!(party, Party::User);
    match kind {
        RunEventKind::Run(lifecycle) => match lifecycle {
            RunLifecycle::PauseRequested { by, .. } | RunLifecycle::StopRequested { by } => {
                yours(by)
            }
            RunLifecycle::ApprovalModeChanged { .. } | RunLifecycle::PauseCancelled => true,
            RunLifecycle::Resumed(resumed) => {
                matches!(resumed.cause, ResumeCause::User | ResumeCause::Answer)
            }
            _ => false,
        },
        RunEventKind::Approval(ApprovalEvent::Decided(decided)) => yours(&decided.by),
        RunEventKind::Session(SessionEvent::Queued(queued)) => yours(&queued.from),
        RunEventKind::Blackboard(BlackboardEvent::Posted(BlackboardMessage { from, .. })) => {
            yours(from)
        }
        RunEventKind::Task(TaskEvent::TakenOver { .. }) => true,
        RunEventKind::Task(TaskEvent::Reassigned { by, .. }) => yours(by),
        RunEventKind::Fallback(FallbackEvent::FellBack { by, .. })
        | RunEventKind::Fallback(FallbackEvent::JobFreed { by, .. }) => yours(by),
        RunEventKind::Council(council) => match &council.kind {
            CouncilEventKind::Message(message) => message.from == Speaker::You,
            CouncilEventKind::Private { .. }
            | CouncilEventKind::Shared { .. }
            | CouncilEventKind::Released { .. } => true,
            _ => false,
        },
        _ => false,
    }
}

/// How much of `from..to` falls in your night, in seconds, at `offset` from UTC.
fn night_seconds(from: OffsetDateTime, to: OffsetDateTime, offset: UtcOffset) -> i64 {
    let (from, to) = (from.to_offset(offset), to.to_offset(offset));
    let mut seconds = 0;
    let mut day = from.date().previous_day().unwrap_or(from.date());
    while day <= to.date() {
        let Some(next) = day.next_day() else { break };
        let night = (
            day.with_time(NIGHT_FROM).assume_offset(offset),
            next.with_time(NIGHT_TO).assume_offset(offset),
        );
        let (start, end) = (from.max(night.0), to.min(night.1));
        if end > start {
            seconds += (end - start).whole_seconds();
        }
        day = next;
    }
    seconds
}

#[cfg(test)]
mod tests {
    use time::macros::{datetime, offset};

    use super::*;

    #[test]
    fn the_night_runs_from_ten_at_night_to_five_in_the_morning() {
        let hours = |from, to| night_seconds(from, to, offset!(+2)) as f64 / 3600.0;
        assert_eq!(
            hours(
                datetime!(2026-10-20 18:00 +2),
                datetime!(2026-10-21 09:00 +2)
            ),
            7.0
        );
        assert_eq!(
            hours(
                datetime!(2026-10-20 23:30 +2),
                datetime!(2026-10-21 00:30 +2)
            ),
            1.0
        );
        assert_eq!(
            hours(
                datetime!(2026-10-21 04:30 +2),
                datetime!(2026-10-21 06:00 +2)
            ),
            0.5
        );
        assert_eq!(
            hours(
                datetime!(2026-10-21 09:00 +2),
                datetime!(2026-10-21 21:00 +2)
            ),
            0.0
        );
        // The same moments read in UTC fall in the night by the local clock, not by UTC's.
        assert_eq!(
            hours(
                datetime!(2026-10-20 20:00 UTC),
                datetime!(2026-10-20 21:00 UTC)
            ),
            1.0
        );
    }
}
