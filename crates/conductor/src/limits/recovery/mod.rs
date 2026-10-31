//! The limits service: each session's limits from the settings, and what happens after a session
//! stops for anything but a clean finish, following the recovery table (usage limits, outages,
//! logins, billing, broken updates, retired models, context, stuck agents, limits, crashes).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use argonauts_core::agents::session_runner::{RunnerDirectory, SessionLimits, model_run};
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::{AgentId, ModelRef, RoleName};
use argonauts_core::models::limits::{
    Candidate, CandidateQuery, CostEstimate, FallbackNote, FallbackPlanner, HealthBoard,
    HealthState, LimitsService, ModelState, PlanUsageBoard, RecoveryAction, Shortlist, StopContext,
    may_have_lost_the_network, served_on_this_machine,
};
use argonauts_core::models::registry::{ModelEntry, ModelRegistry};
use argonauts_core::runs::events::agent_status::{FallbackEvent, FallbackPlan};
use argonauts_core::runs::events::run::WaitReason;
use argonauts_core::runs::events::session::{EndReason, SessionEvent};
use argonauts_core::runs::events::task::{TaskEvent, TaskSpec, TaskState};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::runs::fallback::CheckpointTag;
use argonauts_core::settings::Settings;
use argonauts_core::settings::fallback::{
    AppliedFallback, FallbackMode, FallbackStrategy, OnReturn,
};
use argonauts_core::settings::limit::LimitUnit;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::{AgentEvent, DEFAULT_SOFT_RATIO, ErrorCategory, RunId, TaskId};
use time::OffsetDateTime;

use crate::boards::health::cooldown;
use crate::fallback::history::{History, Placements, TaskLog, task_log};
use crate::fallback::shortlist::{at_plan_limit, auto_check, balance, out_of_plan_usage};
use crate::fallback::why::{one_line, stop_line};
use crate::fallback::{applied, entry_rules};

/// The native cap sits this far above the meter's dollar limit, so the meter stops first.
const NATIVE_CAP_MARGIN: f64 = 1.1;

/// Successors followed before a retired model gives up and asks you.
const MAX_SUCCESSOR_HOPS: usize = 5;

/// Why a blind role that loaded your plugins has nowhere to go.
const NO_AGENT_ISOLATES: &str = "no other agent keeps your plugins out of a blind role";

const RESUME_MESSAGE: &str =
    "The run was paused and is resuming. Continue the task from where you stopped.";

const NETWORK_BACK_MESSAGE: &str = "The network was down and is back. Continue the task from \
     where you stopped.";

/// For a session that did its work but missed the answer format, so the work isn't done twice.
const REPORT_ONLY_MESSAGE: &str = "Your work is done, but your report didn't match the answer \
     format. Don't change anything else: send only the report now, in that format.";

/// A task `spread` passed to a model, and the newest log event read when it did.
type Picks = HashMap<TaskId, (ModelRef, u64)>;

pub struct Limits {
    settings: Arc<Settings>,
    plan_usage: Arc<dyn PlanUsageBoard>,
    health: Arc<dyn HealthBoard>,
    fallback: Arc<dyn FallbackPlanner>,
    registry: Option<Arc<dyn ModelRegistry>>,
    /// Which agents keep your plugins out of a blind role's session. Without it, none do.
    workers: Option<Arc<dyn RunnerDirectory>>,
    /// Each run's `spread` picks the log may not show yet, since an agent's tasks stop at once.
    picks: Mutex<HashMap<RunId, Picks>>,
}

impl Limits {
    pub fn new(
        settings: Arc<Settings>,
        plan_usage: Arc<dyn PlanUsageBoard>,
        health: Arc<dyn HealthBoard>,
        fallback: Arc<dyn FallbackPlanner>,
    ) -> Self {
        Self {
            settings,
            plan_usage,
            health,
            fallback,
            registry: None,
            workers: None,
            picks: Mutex::default(),
        }
    }

    /// Lets a blind role whose session loaded your plugins go to an agent that keeps them out.
    /// Without it, you're asked.
    pub fn with_workers(mut self, workers: Arc<dyn RunnerDirectory>) -> Self {
        self.workers = Some(workers);
        self
    }

    /// Lets a retired model move to its successor and the retry ladder tell providers apart.
    /// Without it, a retired model asks you.
    pub fn with_registry(mut self, registry: Arc<dyn ModelRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// How long a fallback approval for this role and agent waits for you before it falls
    /// back to auto. `None` waits for you.
    pub fn ask_timeout(
        &self,
        role: RoleName,
        agent_id: &AgentId,
    ) -> Result<Option<Duration>, String> {
        applied(&self.settings, role, agent_id).ask_timeout.parse()
    }

    /// When a fallback approval's `ask_timeout` passes: the stop decided as in `auto` mode, so
    /// the task goes to the next valid candidate within the cost cap, or `Ask` again when none is
    /// left.
    pub fn after_ask_timeout<'a>(
        &'a self,
        context: &'a StopContext,
    ) -> BoxFuture<'a, RecoveryAction> {
        Box::pin(self.recover(context, true))
    }

    async fn recover(&self, context: &StopContext, timed_out: bool) -> RecoveryAction {
        if self.lost_the_internet(context).await {
            return RecoveryAction::ResumeSame {
                message: NETWORK_BACK_MESSAGE.to_owned(),
            };
        }
        let (log, unread) = match task_log(&context.events, &context.task.id).await {
            Ok(log) => (log, None),
            Err(error) => {
                tracing::warn!(
                    path = %context.events.display(),
                    %error,
                    "the fallback note has only the task, since the event log couldn't be read"
                );
                (TaskLog::default(), Some(error.to_string()))
            }
        };
        let stop = self.stop(context, log, unread, timed_out);
        self.decide(&stop)
    }

    /// A session that needs a provider, which ended on the network while the internet is
    /// gone: its agent did nothing wrong, so it goes on where it stopped once it's back.
    async fn lost_the_internet(&self, context: &StopContext) -> bool {
        let worker = &context.worker;
        may_have_lost_the_network(&context.end.reason)
            && !served_on_this_machine(&worker.agent, worker.model.as_deref())
            && self.health.check_network().await.is_down()
    }

    fn stop<'a>(
        &self,
        context: &'a StopContext,
        log: TaskLog,
        unread: Option<String>,
        timed_out: bool,
    ) -> Stop<'a> {
        let events = &log.events;
        let model = self.stopped_model(events, &context.worker);
        let mut note = self.fallback.note(&context.task, events);
        note.why_stopped = match &context.end.reason {
            // Logged out only where the CLI's own check says so.
            EndReason::Auth { message }
            | EndReason::CliError {
                category: ErrorCategory::Auth,
                message,
            } if context.logged_in != Some(false) => {
                with_detail("a sign-in error".to_owned(), message)
            }
            EndReason::ModelUnavailable { model: named } if named.is_empty() => {
                stop_line(&EndReason::ModelUnavailable {
                    model: model
                        .clone()
                        .unwrap_or_else(|| format!("{}'s default", context.worker)),
                })
            }
            reason => stop_line(reason),
        };
        if note.work.branch.is_empty() {
            note.work.branch = format!("argonauts/{}/{}", context.run, context.task.id);
        }
        if let Some(error) = unread {
            note.notes.push(format!(
                "The run's event log couldn't be read ({}), so this note has only the task.",
                one_line(&error, 160)
            ));
        }
        let history = History::of(events, &context.task.id);
        Stop {
            context,
            rules: applied(&self.settings, context.role, &context.worker.agent_id()),
            fell_back: history.fallbacks,
            missed_answers: history.missed_answers,
            checks_ran: history.checks_ran,
            provider: self.provider(events, &context.worker),
            model,
            note,
            timed_out,
            placements: log.placements,
        }
    }

    /// The provider of the model that stopped: the registry's for the model it ran, else what
    /// its session recorded, else the agent's own name.
    fn provider(&self, events: &[RunEvent], worker: &ModelRef) -> String {
        if let Some(registry) = &self.registry {
            return registry.provider(&self.runs(worker));
        }
        events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                RunEventKind::Session(SessionEvent::Started(started))
                    if &started.worker == worker =>
                {
                    Some(started.provider.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| worker.agent.clone())
    }

    /// The model `worker` runs, as its runner names it where there are runners.
    fn runs(&self, worker: &ModelRef) -> ModelRef {
        model_run(self.workers.as_deref(), &self.settings, worker)
    }

    /// The model the stopped agent ran: its own, else `agents.<id>.model`, else the one its
    /// session's CLI reported. The agent's error text is prose, so it never names the model.
    fn stopped_model(&self, events: &[RunEvent], worker: &ModelRef) -> Option<String> {
        worker
            .model
            .clone()
            .or_else(|| {
                let agent = self.settings.agents.get(&worker.agent);
                agent.and_then(|agent| agent.model.clone())
            })
            .or_else(|| reported_model(events, worker))
    }

    fn decide(&self, stop: &Stop<'_>) -> RecoveryAction {
        match &stop.context.end.reason {
            EndReason::Completed => RecoveryAction::Proceed,
            EndReason::Paused => RecoveryAction::ResumeSame {
                message: RESUME_MESSAGE.to_owned(),
            },
            EndReason::Stopped => RecoveryAction::AskYou {
                reason: "you stopped its session".to_owned(),
            },
            EndReason::UsageLimit { resets_at } => self.agent_out(stop, Out::PlanUsage(*resets_at)),
            EndReason::RateLimited => self.agent_out(stop, Out::PlanUsage(None)),
            EndReason::CliError { category, .. } => self.cli_error(stop, *category),
            EndReason::Auth { message } => self.logged_out(stop, message),
            EndReason::Billing { message } => self.billing(stop, message),
            EndReason::ContextFull => RecoveryAction::Rotate {
                note: Box::new(stop.note.clone()),
            },
            EndReason::ModelUnavailable { .. } => self.successor(stop),
            EndReason::AdapterBroken { .. } => self.adapter_broken(stop),
            EndReason::IsolationBroken { .. } => self.isolation_broken(stop),
            EndReason::Stuck { .. } | EndReason::StartFailed { .. } | EndReason::Crashed { .. } => {
                self.ladder(stop)
            }
            EndReason::LimitHit { .. } => {
                RecoveryAction::LimitHit(self.settings.limits.spend.when_reached)
            }
        }
    }

    fn cli_error(&self, stop: &Stop<'_>, category: ErrorCategory) -> RecoveryAction {
        match category {
            ErrorCategory::Overloaded
            | ErrorCategory::Server
            | ErrorCategory::Network
            | ErrorCategory::Timeout => self.agent_out(stop, Out::Outage),
            ErrorCategory::UsageLimit | ErrorCategory::RateLimit => {
                self.agent_out(stop, Out::PlanUsage(None))
            }
            ErrorCategory::Auth => self.logged_out(stop, ""),
            ErrorCategory::Billing => self.billing(stop, ""),
            // A fresh session takes over with the note at once, since a resume can't work.
            ErrorCategory::ContextFull | ErrorCategory::SessionLost => RecoveryAction::Rotate {
                note: Box::new(stop.note.clone()),
            },
            ErrorCategory::ModelUnavailable => self.successor(stop),
            ErrorCategory::Limit => {
                RecoveryAction::LimitHit(self.settings.limits.spend.when_reached)
            }
            // Once, and only after real work: a session that missed the format before doing
            // anything has nothing a report could save.
            ErrorCategory::AnswerMissed if report_only_due(stop) => RecoveryAction::ResumeSame {
                message: REPORT_ONLY_MESSAGE.to_owned(),
            },
            ErrorCategory::InvalidRequest
            | ErrorCategory::AnswerMissed
            | ErrorCategory::Permission
            | ErrorCategory::Interrupted
            | ErrorCategory::Cli
            | ErrorCategory::Stalled
            | ErrorCategory::Unknown => self.ladder(stop),
        }
    }

    /// A usage limit or an outage: the agent is out for a while, so the fallback mode decides.
    fn agent_out(&self, stop: &Stop<'_>, out: Out) -> RecoveryAction {
        let agent_id = stop.agent_id();
        let until = match out {
            Out::PlanUsage(resets_at) => resets_at.or_else(|| {
                self.plan_usage
                    .get(&agent_id)
                    .and_then(|reading| reading.resets_at)
            }),
            // The whole CLI's test when it is down, else the stopped model's own.
            Out::Outage => self
                .health
                .state(&agent_id)
                .next_test_at
                .or_else(|| self.health.model(&stop.context.worker).next_test_at),
        };
        let mode = stop.mode();
        if mode != FallbackMode::Wait && stop.fell_back >= stop.rules.max_fallbacks {
            return too_many_fallbacks(stop);
        }
        let reason = stop.reason();
        match mode {
            FallbackMode::Ask => self.ask(stop, reason, until, self.others(stop)),
            // With no known return, as for an error before the board marks the agent Down or a
            // limit with no reset, the wait is a retry after the breaker's cool-down, so the
            // waiting task always has something to wake it.
            FallbackMode::Wait => RecoveryAction::Wait {
                agent_id,
                until: until.or_else(|| Some(self.retry_at())),
                kept_off: None,
            },
            FallbackMode::Auto => {
                let shortlist = self.others(stop);
                let none_to_take_it = !shortlist
                    .candidates
                    .iter()
                    .any(|candidate| auto_check(candidate).is_ok());
                // Staying on the CLI that stopped comes before finishing sooner elsewhere, so
                // with none of its models left the task waits for it, and says until when.
                if none_to_take_it && stop.rules.same_cli {
                    return RecoveryAction::Wait {
                        kept_off: self.elsewhere(stop),
                        agent_id,
                        until: until.or_else(|| Some(self.retry_at())),
                    };
                }
                // Every agent of the crew is out, so the run waits until the first one is back
                // and the task goes on then.
                if none_to_take_it && let Some(out) = self.all_out(&self.crew(stop)) {
                    return RecoveryAction::RunWaits(out);
                }
                self.fallback(stop, reason, until, shortlist)
            }
        }
    }

    /// The crew the run draws on: your `crew`, the main agent, and the one that stopped.
    fn crew(&self, stop: &Stop<'_>) -> Vec<ModelRef> {
        let mut workers = self.settings.crew.members();
        for worker in [&self.settings.crew.main, &stop.context.worker] {
            if !workers.contains(worker) {
                workers.push(worker.clone());
            }
        }
        workers
    }

    fn retry_at(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc() + cooldown(&self.settings.health)
    }

    /// A sign-in error. A login is the fix only where the CLI's own check says it's logged out:
    /// then it always asks, with the retry the engine lists first, and candidates still offered.
    /// Otherwise the error was passing, so the task goes on as for an outage, by the fallback
    /// mode.
    fn logged_out(&self, stop: &Stop<'_>, message: &str) -> RecoveryAction {
        if stop.context.logged_in != Some(false) {
            return self.agent_out(stop, Out::Outage);
        }
        let agent_id = stop.agent_id();
        let reason = with_detail(
            format!("logged out of {agent_id}: log in to {agent_id}, then retry"),
            message,
        );
        self.ask_until_timeout(stop, reason, self.others(stop))
    }

    /// Always asks, and never moves on to another paid API by itself, even after the timeout.
    fn billing(&self, stop: &Stop<'_>, message: &str) -> RecoveryAction {
        let agent_id = stop.agent_id();
        let reason = with_detail(
            format!("{agent_id} hit its billing cap: top up, then retry"),
            message,
        );
        let mut shortlist = self.others(stop);
        if stop.timed_out {
            shortlist
                .candidates
                .retain(|candidate| !matches!(candidate.cost, CostEstimate::Usd(_)));
        }
        self.ask_until_timeout(stop, reason, shortlist)
    }

    /// The registry's successor in the same provider, on the same agent; else you decide.
    fn successor(&self, stop: &Stop<'_>) -> RecoveryAction {
        let next = self.registry.as_deref().and_then(|registry| {
            successor_of(registry, &stop.context.worker, stop.model.as_deref())
        });
        match next {
            Some(to) => RecoveryAction::Fallback {
                to,
                note: Box::new(stop.note.clone()),
                checkpoint: stop.checkpoint(),
            },
            None => {
                let reason = format!("{}, with no successor in its provider", stop.reason());
                self.ask_until_timeout(stop, reason, self.shortlist(stop, false, Vec::new()))
            }
        }
    }

    /// The agent stays disabled until you pin a working version, so waiting doesn't help.
    fn adapter_broken(&self, stop: &Stop<'_>) -> RecoveryAction {
        if stop.fell_back >= stop.rules.max_fallbacks {
            return too_many_fallbacks(stop);
        }
        let reason = stop.reason();
        match stop.mode() {
            FallbackMode::Auto => self.fallback(stop, reason, None, self.others(stop)),
            FallbackMode::Ask | FallbackMode::Wait => {
                self.ask(stop, reason, None, self.others(stop))
            }
        }
    }

    /// Every isolated session on this CLI would load the same, so another agent takes the role,
    /// as for a broken adapter, but only one whose CLI keeps your plugins out. Giving it to one
    /// that loads everything would undo the stop.
    fn isolation_broken(&self, stop: &Stop<'_>) -> RecoveryAction {
        if stop.fell_back >= stop.rules.max_fallbacks {
            return too_many_fallbacks(stop);
        }
        let mut shortlist = self.others(stop);
        let before = shortlist.candidates.len();
        shortlist
            .candidates
            .retain(|candidate| self.isolates(&candidate.worker));
        let mut reason = stop.reason();
        if shortlist.candidates.is_empty() && before > 0 {
            reason = format!("{reason} · {NO_AGENT_ISOLATES}");
        }
        match stop.mode() {
            FallbackMode::Auto => self.fallback(stop, reason, None, shortlist),
            FallbackMode::Ask | FallbackMode::Wait => self.ask(stop, reason, None, shortlist),
        }
    }

    fn isolates(&self, worker: &ModelRef) -> bool {
        self.workers
            .as_ref()
            .is_some_and(|workers| workers.isolates(worker))
    }

    /// Stuck, crashed, or failed to start: one fresh session on another provider, then you.
    /// With `same_cli` the fresh session is another model on the same CLI. Who takes it is
    /// yours to say unless `fallback.mode` is auto, as for a signed-out agent, and waiting
    /// can't help an agent that is still there, so it asks too.
    fn ladder(&self, stop: &Stop<'_>) -> RecoveryAction {
        let why = stop.reason();
        if stop.context.reassigned_once {
            return RecoveryAction::AskYou {
                reason: format!("{why}, and it was already retried on another agent"),
            };
        }
        let shortlist = if stop.rules.same_cli {
            let agent_id = stop.agent_id();
            let mut shortlist = self.shortlist(stop, true, Vec::new());
            shortlist
                .candidates
                .retain(|candidate| candidate.worker.agent_id() == agent_id);
            shortlist
        } else {
            self.shortlist(stop, true, vec![stop.provider.clone()])
        };
        if stop.mode() != FallbackMode::Auto && !shortlist.candidates.is_empty() {
            return self.ask(stop, why, None, shortlist);
        }
        match shortlist
            .candidates
            .iter()
            .find(|candidate| auto_check(candidate).is_ok())
        {
            Some(candidate) => RecoveryAction::Fallback {
                to: candidate.worker.clone(),
                note: Box::new(self.note_for(stop, std::slice::from_ref(&candidate.worker))),
                checkpoint: stop.checkpoint(),
            },
            None if stop.rules.same_cli => RecoveryAction::AskYou {
                reason: format!(
                    "{why}, and no other model on {} can retry it, where fallback.same_cli \
                     keeps it",
                    stop.agent_id()
                ),
            },
            None => RecoveryAction::AskYou {
                reason: format!("{why}, and no agent of another provider can retry it"),
            },
        }
    }

    /// Who the task may fall back to. While only the stopped model is out, any model may, its
    /// own CLI's others among them. Once the CLI stopped as a whole (the network, the account,
    /// the CLI itself, or two of its models out), none of it may. With `same_cli`, only the
    /// stopped CLI's own models may, so a CLI that stopped as a whole leaves nobody. A model
    /// another CLI serves from the provider that is failing would fail the same way, so it is
    /// left out too.
    fn others(&self, stop: &Stop<'_>) -> Shortlist {
        let mut shortlist = self.shortlist(stop, false, Vec::new());
        let agent_id = stop.agent_id();
        let whole_cli = stop.cli_wide() || self.cli_down(&agent_id);
        let failing = self.failing_providers(stop);
        shortlist.candidates.retain(|candidate| {
            let same_cli = candidate.worker.agent_id() == agent_id;
            if same_cli {
                return !whole_cli;
            }
            let fails_too = self.registry.as_ref().is_some_and(|registry| {
                failing.contains(&registry.provider(&self.runs(&candidate.worker)))
            });
            !stop.rules.same_cli && !fails_too
        });
        shortlist
    }

    /// The CLI the task would have gone to if it didn't have to stay on its own: the first
    /// other one with a model that could take it now.
    fn elsewhere(&self, stop: &Stop<'_>) -> Option<AgentId> {
        let agent_id = stop.agent_id();
        let failing = self.failing_providers(stop);
        let fails_too = |worker: &ModelRef| {
            self.registry
                .as_ref()
                .is_some_and(|registry| failing.contains(&registry.provider(&self.runs(worker))))
        };
        self.shortlist(stop, true, Vec::new())
            .candidates
            .iter()
            .filter(|candidate| candidate.worker.agent_id() != agent_id)
            .find(|candidate| auto_check(candidate).is_ok() && !fails_too(&candidate.worker))
            .map(|candidate| candidate.worker.agent_id())
    }

    fn cli_down(&self, agent_id: &AgentId) -> bool {
        matches!(
            self.health.state(agent_id).state,
            HealthState::Down | HealthState::Testing
        )
    }

    /// The providers behind a CLI that went down on overloads or server errors: the stopped
    /// model's, and those of its other models that are out. Empty while the CLI is up, since one
    /// model's trouble says nothing of its provider's other models.
    fn failing_providers(&self, stop: &Stop<'_>) -> Vec<String> {
        let agent_id = stop.agent_id();
        if !stop.is_outage() || !self.cli_down(&agent_id) {
            return Vec::new();
        }
        let mut providers = vec![stop.provider.clone()];
        if let Some(registry) = &self.registry {
            for health in self.health.models(&agent_id) {
                let provider = registry.provider(&health.model);
                if health.state == ModelState::Out && !providers.contains(&provider) {
                    providers.push(provider);
                }
            }
        }
        providers
    }

    fn shortlist(
        &self,
        stop: &Stop<'_>,
        all_valid: bool,
        exclude_providers: Vec<String>,
    ) -> Shortlist {
        let context = stop.context;
        self.fallback.shortlist(&CandidateQuery {
            task: context.task.clone(),
            role: context.role,
            stopped: context.worker.clone(),
            exclude_providers,
            fallback: Vec::new(),
            all_valid,
        })
    }

    /// Auto mode: by the strategy, to the candidates auto may take. A race needs two of them.
    fn fallback(
        &self,
        stop: &Stop<'_>,
        reason: String,
        until: Option<OffsetDateTime>,
        mut shortlist: Shortlist,
    ) -> RecoveryAction {
        let eligible: Vec<&Candidate> = shortlist
            .candidates
            .iter()
            .filter(|candidate| auto_check(candidate).is_ok())
            .collect();
        let racers = usize::try_from(shortlist.race_count).unwrap_or(usize::MAX);
        if shortlist.strategy == FallbackStrategy::Race && racers >= 2 && eligible.len() >= 2 {
            let to: Vec<ModelRef> = eligible
                .iter()
                .take(racers)
                .map(|candidate| candidate.worker.clone())
                .collect();
            return RecoveryAction::Race {
                note: Box::new(self.note_for(stop, &to)),
                to,
                pick: shortlist.pick,
                checkpoint: stop.checkpoint(),
            };
        }
        let to = if shortlist.strategy == FallbackStrategy::Spread {
            self.spread_pick(stop, &mut shortlist.candidates)
        } else {
            eligible.first().map(|candidate| candidate.worker.clone())
        };
        match to {
            Some(to) => RecoveryAction::Fallback {
                note: Box::new(self.note_for(stop, std::slice::from_ref(&to))),
                to,
                checkpoint: stop.checkpoint(),
            },
            None => self.ask(stop, reason, until, shortlist),
        }
    }

    /// The valid candidate `spread` gives this task, noted until the log shows it, so the
    /// agent's next stop goes to someone else.
    fn spread_pick(&self, stop: &Stop<'_>, candidates: &mut [Candidate]) -> Option<ModelRef> {
        let mut picks = self.picks();
        let run = picks.entry(stop.context.run.clone()).or_default();
        let placements = overlay(&stop.placements, run);
        balance(candidates, |worker| placements.fell_back_to(worker));
        let to = candidates
            .iter()
            .find(|candidate| auto_check(candidate).is_ok())
            .map(|candidate| candidate.worker.clone());
        match &to {
            Some(to) => {
                run.insert(
                    stop.context.task.id.clone(),
                    (to.clone(), stop.placements.last_seq),
                );
            }
            None if run.is_empty() => {
                picks.remove(&stop.context.run);
            }
            None => {}
        }
        to
    }

    /// The run's placements with the `spread` picks the log doesn't show yet.
    fn placements(&self, stop: &Stop<'_>) -> Placements {
        let mut picks = self.picks();
        let Some(run) = picks.get_mut(&stop.context.run) else {
            return stop.placements.clone();
        };
        let placements = overlay(&stop.placements, run);
        if run.is_empty() {
            picks.remove(&stop.context.run);
        }
        placements
    }

    /// A panic elsewhere can't leave the picks half written, so a poisoned lock is still good.
    fn picks(&self) -> MutexGuard<'_, HashMap<RunId, Picks>> {
        self.picks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks, or after the ask timeout decides as auto would.
    fn ask_until_timeout(
        &self,
        stop: &Stop<'_>,
        reason: String,
        shortlist: Shortlist,
    ) -> RecoveryAction {
        if stop.timed_out {
            self.fallback(stop, reason, None, shortlist)
        } else {
            self.ask(stop, reason, None, shortlist)
        }
    }

    fn ask(
        &self,
        stop: &Stop<'_>,
        reason: String,
        resets_at: Option<OffsetDateTime>,
        mut shortlist: Shortlist,
    ) -> RecoveryAction {
        let context = stop.context;
        let mut also_moving = Vec::new();
        if shortlist.strategy == FallbackStrategy::Spread {
            let placements = self.placements(stop);
            balance(&mut shortlist.candidates, |worker| {
                placements.fell_back_to(worker)
            });
            if stop.agent_wide() {
                also_moving = placements.on_agent(&stop.agent_id(), &context.task.id);
            }
        }
        // Unless the reason already says why no one is left.
        let reason = if shortlist.candidates.is_empty() && !reason.ends_with(NO_AGENT_ISOLATES) {
            format!("{reason} · no valid fallback")
        } else {
            reason
        };
        RecoveryAction::Ask(Box::new(FallbackPlan {
            task: context.task.id.clone(),
            title: context.task.title.clone(),
            stopped: context.worker.clone(),
            reason,
            resets_at,
            work: stop.note.work.stat,
            checks_ran: stop.checks_ran,
            candidates: shortlist.candidates,
            mode: stop.rules.mode,
            strategy: shortlist.strategy,
            race_count: shortlist.race_count,
            also_moving,
        }))
    }

    /// The note with any instructions their fallback entries carry.
    fn note_for(&self, stop: &Stop<'_>, to: &[ModelRef]) -> FallbackNote {
        let mut note = stop.note.clone();
        for worker in to {
            let Some(rules) = entry_rules(&stop.rules.candidates, worker) else {
                continue;
            };
            let whom = if to.len() > 1 {
                format!(" for {worker}")
            } else {
                String::new()
            };
            if let Some(instructions) = &rules.instructions {
                note.notes
                    .push(format!("Instructions{whom}: {instructions}"));
            }
            if rules.read_only {
                note.notes
                    .push(format!("Read-only{whom}: don't edit files on this task."));
            }
        }
        note
    }
}

/// What a task that fell back from an agent does once that agent is healthy again.
#[derive(Debug, Clone, PartialEq)]
pub enum OriginalBack {
    /// No quality debt and no `give_back`, so the task stays with its holder, and the notice
    /// says how to move it back.
    Stays { task: TaskId, notice: String },
    /// `on_return = "give_back"`: the original takes the task at its next try.
    GiveBack { task: TaskId, to: ModelRef },
    /// The task carries quality debt, so `on_return` decides, and `ask` opens
    /// `OriginalReturned`.
    Debt {
        task: TaskId,
        original: ModelRef,
        holder: ModelRef,
        on_return: OnReturn,
    },
}

impl Limits {
    /// For each open task that fell back from `agent` and still sits elsewhere, what its return
    /// means, read from the run's events. `name` is the agent's display name. A running step
    /// finishes first, so the engine applies each outcome at the task's next try.
    pub fn original_returned(
        &self,
        agent_id: &AgentId,
        name: &str,
        events: &[RunEvent],
    ) -> Vec<OriginalBack> {
        let mut tasks: BTreeMap<TaskId, AwayTask> = BTreeMap::new();
        for event in events {
            let Some(task) = task_of(event) else {
                continue;
            };
            tasks.entry(task.clone()).or_default().follow(&event.kind);
        }
        // Fallbacks move a task's worker, so the worker's rules decide.
        let on_return = applied(&self.settings, RoleName::Worker, agent_id).on_return;
        tasks
            .into_iter()
            .filter(|(_, away)| !away.done)
            .filter_map(|(task, away)| {
                let holder = away.holder?;
                let original = away
                    .taken_from
                    .into_iter()
                    .find(|worker| &worker.agent_id() == agent_id)?;
                Some(if away.debt {
                    OriginalBack::Debt {
                        task,
                        original,
                        holder,
                        on_return,
                    }
                } else if on_return == OnReturn::GiveBack {
                    OriginalBack::GiveBack { task, to: original }
                } else {
                    let notice = format!(
                        "{name} is back · {task} stays with {holder} · /reassign {task} {original} moves it back"
                    );
                    OriginalBack::Stays { task, notice }
                })
            })
            .collect()
    }
}

/// One task's models as the events tell them.
#[derive(Default)]
struct AwayTask {
    holder: Option<ModelRef>,
    /// Models the task fell back from, oldest first, while it sits on another agent.
    taken_from: Vec<ModelRef>,
    /// The model whose stop asked for the pending fallback.
    stopped: Option<ModelRef>,
    debt: bool,
    done: bool,
}

impl AwayTask {
    fn follow(&mut self, kind: &RunEventKind) {
        match kind {
            RunEventKind::Fallback(FallbackEvent::Needed(plan)) => {
                self.stopped = Some(plan.stopped.clone());
            }
            RunEventKind::Fallback(FallbackEvent::FellBack { to, .. }) => {
                let from = self.stopped.take().or_else(|| self.holder.clone());
                if let Some(from) = from
                    && !self.taken_from.contains(&from)
                {
                    self.taken_from.push(from);
                }
                self.moved_to(to);
            }
            RunEventKind::Task(
                TaskEvent::Try { worker: to, .. } | TaskEvent::Reassigned { to, .. },
            ) => self.moved_to(to),
            RunEventKind::Task(TaskEvent::QualityDebt(_)) => self.debt = true,
            RunEventKind::Task(TaskEvent::DebtCleared { .. }) => self.debt = false,
            RunEventKind::Task(TaskEvent::State { to, .. }) => {
                self.done = matches!(to, TaskState::Merged | TaskState::Cancelled);
            }
            _ => {}
        }
    }

    /// A task back on an agent it was taken from is no longer away from it.
    fn moved_to(&mut self, to: &ModelRef) {
        let agent_id = to.agent_id();
        self.taken_from.retain(|from| from.agent_id() != agent_id);
        self.holder = Some(to.clone());
    }
}

fn task_of(event: &RunEvent) -> Option<&TaskId> {
    match &event.kind {
        RunEventKind::Fallback(FallbackEvent::Needed(plan)) => Some(&plan.task),
        RunEventKind::Fallback(FallbackEvent::FellBack { task, .. }) => Some(task),
        _ => event.task_id.as_ref(),
    }
}

impl LimitsService for Limits {
    fn session_limits(&self, task: Option<&TaskSpec>, role: RoleName) -> SessionLimits {
        let settings = &self.settings;
        let lifetime = settings
            .roles
            .get(&role)
            .and_then(|role| role.lifetime)
            .unwrap_or(settings.sessions.lifetime);
        // A session that lives past one task answers to the run's limit lines, not a task's.
        let per_task = task.is_some() || lifetime == SessionLifetime::Task;
        let own = task.map(|task| &task.limit);
        let limit = if per_task {
            own.and_then(|limit| limit.limit)
                .or_else(|| settings.limits.spend.task_limit())
        } else {
            None
        };
        let watchdogs = &settings.watchdogs;
        SessionLimits {
            limit,
            soft_stop: soft_stop(settings.limits.spend.soft_stop),
            max_minutes: own
                .and_then(|limit| limit.max_minutes)
                .or(watchdogs.wall_clock_min.filter(|_| per_task))
                .filter(|minutes| *minutes > 0.0),
            max_tool_calls: own
                .and_then(|limit| limit.max_tool_calls)
                .or(watchdogs.max_tool_calls.filter(|_| per_task))
                .filter(|calls| *calls > 0),
            inactivity_min: own
                .and_then(|limit| limit.inactivity_min)
                .or(Some(watchdogs.inactivity_min))
                .filter(|minutes| *minutes > 0.0),
            repeat_limit: Some(watchdogs.repeat_limit).filter(|limit| *limit > 0),
            // The crew keeps it only for a later try on committed work, which it alone knows.
            no_progress_calls: Some(watchdogs.no_progress_calls)
                .filter(|calls| *calls > 0 && task.is_some()),
            native_cap_usd: limit
                .filter(|limit| limit.unit == LimitUnit::Usd)
                .map(|limit| limit.amount * NATIVE_CAP_MARGIN),
            run_left: None,
            spends_unseen: false,
        }
    }

    fn after_stop<'a>(&'a self, context: &'a StopContext) -> BoxFuture<'a, RecoveryAction> {
        Box::pin(self.recover(context, false))
    }

    /// Every agent among `workers` is down or out of plan usage. The run waits until the earliest
    /// time one is known to come back.
    fn all_out(&self, workers: &[ModelRef]) -> Option<WaitReason> {
        if workers.is_empty() {
            return None;
        }
        let now = OffsetDateTime::now_utc();
        let mut agent_ids: Vec<AgentId> = workers.iter().map(ModelRef::agent_id).collect();
        agent_ids.sort();
        agent_ids.dedup();
        let mut until: Option<OffsetDateTime> = None;
        for agent_id in &agent_ids {
            let health = self.health.state(agent_id);
            // Out of plan: its provider refused it, or it is as full as you let it get.
            let rejected = self.plan_usage.get(agent_id).filter(|reading| {
                out_of_plan_usage(reading, now) || at_plan_limit(reading, &self.settings, now)
            });
            let down = health.state == HealthState::Down;
            if !down && rejected.is_none() {
                return None;
            }
            // Out for both reasons, it's back only when both clear.
            let times = [
                down.then_some(health.next_test_at),
                rejected.map(|reading| reading.resets_at),
            ];
            let known: Option<Vec<OffsetDateTime>> = times.into_iter().flatten().collect();
            if let Some(back) = known.and_then(|times| times.into_iter().max()) {
                until = Some(until.map_or(back, |until| until.min(back)));
            }
        }
        Some(WaitReason::AllAgentsOut { until })
    }
}

/// The model the CLI reported at the start of `worker`'s latest session on the task.
fn reported_model(events: &[RunEvent], worker: &ModelRef) -> Option<String> {
    let session = events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Session(SessionEvent::Started(started)) if &started.worker == worker => {
            Some(event.session.as_ref())
        }
        _ => None,
    })??;
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Agent(AgentEvent::Init {
            model: Some(model), ..
        }) if event.session.as_ref() == Some(session) => Some(model.clone()),
        _ => None,
    })
}

/// The first model along the registry's successor chain that isn't retired, in the stopped
/// model's provider. It stays on the same agent, since another one could bill you differently.
fn successor_of(
    registry: &dyn ModelRegistry,
    worker: &ModelRef,
    model: Option<&str>,
) -> Option<ModelRef> {
    let stopped = ModelRef::new(worker.agent.clone(), model.map(str::to_owned));
    let entry = registry.get(&stopped).or_else(|| registry.get(worker))?;
    let mut current = entry.clone();
    for _ in 0..MAX_SUCCESSOR_HOPS {
        let to = successor_ref(&current)?;
        if to.agent != worker.agent || to == stopped || registry.provider(&to) != entry.provider {
            return None;
        }
        match registry.get(&to) {
            Some(later) if later.retired => current = later,
            _ => return Some(to),
        }
    }
    None
}

/// A registry `successor` is a full `agent/model` reference, or a model on the entry's own
/// agent.
fn successor_ref(entry: &ModelEntry) -> Option<ModelRef> {
    let successor = entry.successor.as_deref()?;
    if !successor.contains('/') {
        return Some(ModelRef::new(
            entry.id.agent.clone(),
            Some(successor.to_owned()),
        ));
    }
    match successor.parse() {
        Ok(to) => Some(to),
        Err(error) => {
            tracing::warn!(model = %entry.id, %error, "the registry's successor can't be read");
            None
        }
    }
}

/// A ratio at or below zero would send the wrap-up at the start, so it falls back too.
fn soft_stop(ratio: f64) -> f64 {
    if ratio.is_nan() || ratio <= 0.0 {
        DEFAULT_SOFT_RATIO
    } else {
        ratio.min(1.0)
    }
}

enum Out {
    PlanUsage(Option<OffsetDateTime>),
    Outage,
}

/// One stop, with what the decision reads.
struct Stop<'a> {
    context: &'a StopContext,
    rules: AppliedFallback,
    /// Earlier fallbacks of this task.
    fell_back: u32,
    /// The task's sessions that missed the answer format, the stopped one too.
    missed_answers: u32,
    /// Checks ran on the task's latest commit.
    checks_ran: bool,
    provider: String,
    /// The model it ran, when known.
    model: Option<String>,
    note: FallbackNote,
    timed_out: bool,
    placements: Placements,
}

/// A session that used tools before it missed the answer format, the task's first to miss it,
/// which can be resumed.
fn report_only_due(stop: &Stop<'_>) -> bool {
    let end = &stop.context.end;
    end.tool_calls > 0 && end.cli_session.is_some() && stop.missed_answers <= 1
}

impl Stop<'_> {
    fn agent_id(&self) -> AgentId {
        self.context.worker.agent_id()
    }

    fn mode(&self) -> FallbackMode {
        if self.timed_out {
            FallbackMode::Auto
        } else {
            self.rules.mode
        }
    }

    fn reason(&self) -> String {
        self.note.why_stopped.clone()
    }

    /// An overload or a server error, which is the provider's doing.
    fn is_outage(&self) -> bool {
        matches!(
            self.context.end.reason,
            EndReason::CliError {
                category: ErrorCategory::Overloaded | ErrorCategory::Server,
                ..
            }
        )
    }

    /// The failure is no one model's doing (the network, the account, the CLI itself), so the
    /// CLI's other models would meet it too. An overload, a server error, and a model that's
    /// gone are about the model alone.
    fn cli_wide(&self) -> bool {
        match &self.context.end.reason {
            EndReason::ModelUnavailable { .. } => false,
            EndReason::CliError { category, .. } => !matches!(
                category,
                ErrorCategory::Overloaded | ErrorCategory::Server | ErrorCategory::ModelUnavailable
            ),
            _ => true,
        }
    }

    /// The whole agent stopped, not only its model, so its other tasks stop too.
    fn agent_wide(&self) -> bool {
        !matches!(
            self.context.end.reason,
            EndReason::ModelUnavailable { .. }
                | EndReason::CliError {
                    category: ErrorCategory::ModelUnavailable,
                    ..
                }
        )
    }

    fn checkpoint(&self) -> Option<CheckpointTag> {
        self.rules.checkpoint.then(|| CheckpointTag {
            run: self.context.run.clone(),
            task: self.context.task.id.clone(),
            n: self.fell_back + 1,
        })
    }
}

/// The log's placements with `picks` laid over them. A pick the log has since caught up with is
/// dropped.
fn overlay(log: &Placements, picks: &mut Picks) -> Placements {
    let mut placements = log.clone();
    picks.retain(|task, (worker, seen)| placements.pick(task, worker, *seen));
    placements
}

fn too_many_fallbacks(stop: &Stop<'_>) -> RecoveryAction {
    RecoveryAction::AskYou {
        reason: format!(
            "\"{}\" stopped after {} fallbacks, the most /fallback allows",
            stop.context.task.title.trim(),
            stop.fell_back
        ),
    }
}

fn with_detail(reason: String, detail: &str) -> String {
    let detail = one_line(detail, 120);
    if detail.is_empty() {
        reason
    } else {
        format!("{reason} ({detail})")
    }
}

#[cfg(test)]
mod tests;
