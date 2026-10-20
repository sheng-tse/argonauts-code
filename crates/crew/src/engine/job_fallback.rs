//! Who carries on when a session ends for an agent reason. A CLI error never
//! ends a run, in any mode: the same agent goes again once after a short wait when the failure
//! may pass, with a line that says so, then its job goes to the next agent on its fallback list,
//! and only when nobody can take it does the run wait, saying why and what fixes it. An agent
//! whose CLI says it's logged out, or that is down or out of plan usage, is out: its jobs go
//! straight to a fallback until its CLI says it's logged in again or the agent is back. Each
//! fallback is said once.
//! A blind role's session keeps your plugins out, so only an agent whose CLI can do the same
//! takes its place, and a CLI that loaded them anyway is out of that job, every model of it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use argonauts_core::TaskId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::agent_status::{JobKeeper, JobRef, JobWait};
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::{EndReason, loaded_words, stuck_words};
use argonauts_core::events::workflow::{RunNote, WorkflowEvent};
use argonauts_core::limits::{AgentHealth, HealthState, ModelState, TEST_TASK_SUCCEEDED, Trouble};
use argonauts_core::log::{NO_SECOND_TRY, NOBODY_TO_FALL_BACK_TO, SAME_CLI_IS_ON};
use argonauts_core::names::{AgentId, ModelRef, RoleName, WorkflowId};
use argonauts_core::registry::listed::{self, nearest_listed, other_models, runs_in_place};
use argonauts_core::session_runner::RunnerKind;
use argonauts_core::settings::Settings;
use argonauts_core::settings::fallback::{
    AppliedFallback, FallbackEntry, FallbackMode, Retry, SameStep,
};
use argonauts_core::steps::StepName;
use argonauts_core::{ErrorCategory, Note, NoteLevel};
use time::OffsetDateTime;

use crate::engine::host::{Host, round_window};
use crate::engine::shared::lock;
use crate::error::{Beside, EngineError, EngineResult, Stall, StalledJob};
use crate::fallback::rules;
use crate::roles::{self, display_name};
use crate::state::RunState;
use crate::state::sessions::SessionRecord;
use crate::workflow::StepAt;

/// The job a session does, which a fallback must fit: its step and round, its role, its task, and
/// whose work it judges.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Job {
    pub at: Option<StepAt>,
    pub role: RoleName,
    pub step: Option<StepName>,
    pub task: Option<TaskId>,
    /// Agents a fallback may not share a provider with: the worker of the task a reviewer
    /// reviews, or the authors of the plans a judge scores.
    pub independent_of: Vec<ModelRef>,
    /// Its session keeps your plugins out, as a blind role's does, so a fallback's CLI must
    /// keep them out too.
    pub isolated: bool,
}

impl Job {
    /// The same job in the same step and round, whoever is independent of whom.
    fn same_place(&self, other: &Self) -> bool {
        self.at == other.at
            && self.role == other.role
            && self.step == other.step
            && self.task == other.task
    }

    /// The job as the log names it, so an engine that wakes later finds it again.
    fn named(&self) -> JobRef {
        JobRef {
            step: self.at.as_ref().map(|at| at.step.clone()),
            round: self.at.as_ref().map(|at| at.round),
            role: self.role,
            does: self.step,
            task: self.task.clone(),
        }
    }
}

/// A role's fallback rules as one job has them. A job you let leave its CLI no longer has
/// `same_cli`, and goes first to the agent you gave it to.
struct JobRules {
    applied: AppliedFallback,
    first: Option<ModelRef>,
    /// Every installed agent may take it, beyond the role's list, as when you pick one.
    anyone: bool,
    /// A model of another CLI already at work in the step may take the job, as
    /// `fallback.same_step` lets once nothing else can.
    second_in_step: bool,
}

impl JobRules {
    fn of(host: &Host, job: &Job, agent_id: &AgentId) -> Self {
        let mut applied = rules::applied(host, job.role, agent_id);
        let freed = host.read(|state| {
            state
                .run
                .freed(&job.named())
                .map(Option::<&ModelRef>::cloned)
        });
        if freed.is_some() {
            applied.same_cli = false;
        }
        Self {
            applied,
            first: freed.flatten(),
            anyone: false,
            second_in_step: false,
        }
    }
}

/// What the engine remembers of the run's agents while it runs. The log keeps the rest: a
/// resumed engine asks again the CLI of an agent whose last session ended on a sign-in error,
/// and finds the fallbacks already made in its sessions.
#[derive(Debug, Default)]
pub(crate) struct Memory {
    /// What each agent's CLI said of its login when last asked.
    logins: BTreeMap<ModelRef, Login>,
    /// Jobs fallbacks took, and jobs their own agents hold, so two jobs of one step never
    /// go to the same agent.
    taken: Vec<Taken>,
    /// Fallbacks already said, from whom to whom, so a repeat says nothing new.
    said: BTreeSet<(ModelRef, ModelRef)>,
    /// Agents whose sessions crashed twice in a row. Until the run resumes or their agent's
    /// health check passes, their later jobs go straight to a fallback rather than starting a
    /// CLI that fails every time.
    keeps_failing: BTreeMap<ModelRef, Failing>,
    /// CLIs that couldn't start. A CLI that won't start for one job won't for the next, on any
    /// of its models, so its later jobs go straight to a fallback, as for one that keeps
    /// crashing.
    wont_start: BTreeMap<AgentId, Failing>,
    /// The questions about a CLI that is down which this engine already follows. A second
    /// follower would act on the one answer twice.
    followed: BTreeSet<Followed>,
    /// Models that run in place of one their CLI no longer lists, each with that one.
    listed: BTreeMap<ModelRef, ModelRef>,
    /// The agents already told their model runs in place of one no longer listed.
    said_listed: BTreeSet<String>,
    /// Each CLI's last crash as it started, with no session of its own yet, by its message.
    started_crashing: BTreeMap<AgentId, String>,
}

/// A question someone in this engine waits on the answer of: the one about a CLI that is
/// down, or one task's own after you chose to pick an agent for each.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Followed {
    Cli(AgentId),
    Task(TaskId),
}

/// How an agent's sessions keep ending, in words, the resume it happened after, and when.
#[derive(Debug, Clone)]
struct Failing {
    words: String,
    after: u64,
    at: OffsetDateTime,
}

impl Failing {
    /// Whether it still holds: the run hasn't resumed since, and the agent's test task hasn't
    /// succeeded since. A passing test means whatever broke the CLI may have been fixed; errors
    /// that merely stopped coming back say nothing of it.
    fn holds(&self, resumed: u64, health: &AgentHealth) -> bool {
        let tested_since = health.state == HealthState::Healthy
            && health.since > self.at
            && health.reason.as_deref() == Some(TEST_TASK_SUCCEEDED);
        self.after == resumed && !tested_since
    }
}

/// A job a fallback took: where, whose it was, and who took it. A job its own agent holds
/// names that agent as both.
#[derive(Debug, Clone)]
struct Taken {
    job: Job,
    asked: ModelRef,
    by: ModelRef,
}

/// One answer of a CLI's login check.
#[derive(Debug, Clone, Copy)]
struct Login {
    at: Instant,
    logged_out: bool,
}

impl Memory {
    /// Takes `question` to follow. `false` when someone already does.
    pub(crate) fn follow(&mut self, question: Followed) -> bool {
        self.followed.insert(question)
    }

    pub(crate) fn unfollow(&mut self, question: &Followed) {
        self.followed.remove(question);
    }

    /// How `worker`'s sessions keep ending, when that's remembered: its own crashes, or its
    /// CLI's failed start, which is every one of its models'.
    fn failing(&self, worker: &ModelRef) -> Option<&Failing> {
        let own = self.keeps_failing.get(worker);
        own.or_else(|| self.wont_start.get(&worker.agent_id()))
    }

    fn logged_out(&self, worker: &ModelRef) -> bool {
        self.logins
            .get(worker)
            .is_some_and(|login| login.logged_out)
    }
}

/// Why an agent stopped.
enum Why {
    /// Its session ended for this reason.
    Ended(EndReason),
    /// Its session ended for this reason again, after the retry that was said.
    EndedAgain(EndReason),
    /// Its CLI says it's logged out, after its session ended for this reason when it had one.
    LoggedOut(Option<EndReason>),
    /// Its agent is down or out of plan usage, in words.
    Unusable(&'static str),
    /// The model itself is out while its CLI's other models go on, in words.
    ModelOut(&'static str),
    /// Its sessions crashed twice in a row earlier in the run, in words.
    KeepsFailing(String),
}

impl Why {
    /// Whether it's about the one model alone, an overload, server errors, or a model its CLI
    /// refused, so its CLI's other models may still take the job.
    fn model_only(&self) -> bool {
        match self {
            Self::Ended(reason) | Self::EndedAgain(reason) => {
                is_outage(reason) || is_refused_model(reason)
            }
            Self::ModelOut(_) => true,
            Self::LoggedOut(_) | Self::Unusable(_) | Self::KeepsFailing(_) => false,
        }
    }

    /// Whether its CLI is signed out, which its row and its stop word as `signed out`.
    fn signed_out(&self) -> bool {
        matches!(
            self,
            Self::LoggedOut(_) | Self::Ended(EndReason::Auth { .. })
        )
    }
}

/// An overload or a server error, which is the provider's doing and about one model.
fn is_outage(reason: &EndReason) -> bool {
    matches!(
        reason,
        EndReason::CliError {
            category: ErrorCategory::Overloaded | ErrorCategory::Server,
            ..
        }
    )
}

/// A model its CLI won't run, which says nothing of the CLI's other models.
fn is_refused_model(reason: &EndReason) -> bool {
    matches!(
        reason,
        EndReason::ModelUnavailable { .. }
            | EndReason::CliError {
                category: ErrorCategory::ModelUnavailable,
                ..
            }
    )
}

impl Why {
    /// A model its CLI refused, whose CLI's other models come first.
    fn refused_model(&self) -> bool {
        matches!(self, Self::Ended(reason) | Self::EndedAgain(reason) if is_refused_model(reason))
    }
}

/// Before a session's first start: the agent that takes the job at once because the asked one
/// is out, its CLI having said it's logged out or the agent being down or out of plan usage.
/// `None` starts the asked one. `Err(Stalled)` when it's out and nobody else may take the job.
pub(crate) async fn instead_of(
    host: &Host,
    asked: &ModelRef,
    job: &Job,
) -> EngineResult<Option<ModelRef>> {
    let why = if is_logged_out(host, asked).await {
        Why::LoggedOut(None)
    } else if let Some(words) = unusable(host, asked) {
        Why::Unusable(words)
    } else if let Some(words) = model_out(host, asked) {
        Why::ModelOut(words)
    } else if let Some(words) = keeps_failing(host, asked) {
        // With no fallback, the agent goes again rather than the run waiting.
        let why = Why::KeepsFailing(words);
        return Ok(fall_back(host, job, asked, asked, &[asked], &why).ok());
    } else {
        return Ok(None);
    };
    fall_back(host, job, asked, asked, &[asked], &why).map(Some)
}

/// The model a session of `worker` runs: its own, or, when its CLI no longer lists the model
/// `worker` is pinned to, the nearest one it does list. A run keeps the settings it started
/// with, so only the CLI's own list can tell a model is gone, and asking for it would only be
/// refused. The swap is remembered, so the job's first session says it.
pub(crate) fn listed(host: &Host, worker: &ModelRef) -> ModelRef {
    let pinned = pinned(host, worker);
    let Some(nearest) = nearest_listed(host.services().registry.as_ref(), &pinned) else {
        return worker.clone();
    };
    lock(&host.shared().job_fallbacks)
        .listed
        .insert(nearest.clone(), pinned);
    nearest
}

/// `worker` with the model it runs named: its own, else the one its CLI's settings give it.
fn pinned(host: &Host, worker: &ModelRef) -> ModelRef {
    if worker.model.is_some() {
        return worker.clone();
    }
    let set = host.settings().agents.get(&worker.agent);
    ModelRef::new(
        worker.agent.clone(),
        set.and_then(|agent| agent.model.clone()),
    )
}

/// Says once, as job `label` first starts on `worker`, that `worker` runs in place of a model
/// its CLI no longer lists: `propose:p3 runs on Gemini 3.8 Flash (Low): Antigravity CLI no
/// longer offers Gemini 3.6 Flash (Low)`.
pub(crate) fn say_listed(
    host: &Host,
    label: &str,
    worker: &ModelRef,
    task: Option<&TaskId>,
) -> EngineResult<()> {
    let gone = {
        let mut memory = lock(&host.shared().job_fallbacks);
        let Some(gone) = memory.listed.get(worker).cloned() else {
            return Ok(());
        };
        if !memory.said_listed.insert(label.to_owned()) {
            return Ok(());
        }
        gone
    };
    let registry = host.services().registry.as_ref();
    note(host, task, runs_in_place(registry, label, &gone, worker))
}

/// The CLI's own name for a model it lists, as agy's `Gemini 3.8 Flash (Low)`, else the name
/// the catalog knows it by.
pub(crate) fn model_name(host: &Host, model: &ModelRef) -> Option<String> {
    listed::model_name(host.services().registry.as_ref(), model)
}

/// How `worker`'s sessions keep ending, when they crashed twice in a row since the run last
/// resumed and its agent's health check last passed.
fn keeps_failing(host: &Host, worker: &ModelRef) -> Option<String> {
    let since = host.read(|state| state.run.last_resumed).unwrap_or(0);
    let failing = lock(&host.shared().job_fallbacks)
        .failing(worker)
        .cloned()?;
    let health = host.services().health.state(&worker.agent_id());
    failing.holds(since, &health).then_some(failing.words)
}

/// Notes that the CLI of `worker` couldn't start, for the rest of the run or until it resumes.
fn remember_failed_start(host: &Host, worker: &ModelRef, reason: &EndReason) {
    if !matches!(reason, EndReason::StartFailed { .. }) {
        return;
    }
    let failing = Failing {
        words: in_words(reason),
        after: host.read(|state| state.run.last_resumed).unwrap_or(0),
        at: host.now(),
    };
    lock(&host.shared().job_fallbacks)
        .wont_start
        .insert(worker.agent_id(), failing);
}

/// How quickly a CLI that crashes as it starts gives up, before it has a session of its own.
const STARTUP_CRASH: Duration = Duration::from_secs(2);

/// Notes a crash of `worker`'s CLI as it started, with no session of its own, quickly. Two
/// in a row with the same message take the CLI out for the rest of the run, every model of
/// it, with one line, since it fails the same way on every start. The next run asks it again.
fn remember_startup_crash(
    host: &Host,
    worker: &ModelRef,
    record: &SessionRecord,
) -> EngineResult<()> {
    let Some(end) = &record.end else {
        return Ok(());
    };
    let EndReason::Crashed { message } = &end.reason else {
        lock(&host.shared().job_fallbacks)
            .started_crashing
            .remove(&worker.agent_id());
        return Ok(());
    };
    let at_start =
        end.cli_session.is_none() && Duration::from_millis(end.duration_ms) < STARTUP_CRASH;
    let message = first_line(message).to_owned();
    let twice = {
        let mut memory = lock(&host.shared().job_fallbacks);
        if !at_start {
            memory.started_crashing.remove(&worker.agent_id());
            return Ok(());
        }
        let before = memory
            .started_crashing
            .insert(worker.agent_id(), message.clone());
        let twice = before.as_deref() == Some(message.as_str());
        if twice {
            let words = if message.is_empty() {
                "crashed twice as it started".to_owned()
            } else {
                format!("crashed twice as it started: \"{message}\"")
            };
            let failing = Failing {
                words,
                after: host.read(|state| state.run.last_resumed).unwrap_or(0),
                at: host.now(),
            };
            memory.wont_start.insert(worker.agent_id(), failing);
        }
        twice
    };
    if !twice {
        return Ok(());
    }
    let cli = DisplayNames::default().cli(worker.agent.as_str());
    let how = if message.is_empty() {
        String::new()
    } else {
        format!(": \"{message}\"")
    };
    note(
        host,
        record.task.as_ref(),
        format!("{cli} is out for this run: it crashed twice as it started{how}"),
    )
}

/// Whether the run already knows `worker` can't take a job now, without asking its CLI: its
/// sessions crashed twice in a row or its CLI couldn't start, its CLI would refuse to start,
/// its CLI last said it's logged out, or its agent is down or out of plan usage.
pub(crate) fn known_out(host: &Host, worker: &ModelRef) -> bool {
    let logged_out = lock(&host.shared().job_fallbacks).logged_out(worker);
    logged_out
        || keeps_failing(host, worker).is_some()
        || start_refusal(host, worker).is_some()
        || unusable(host, worker).is_some()
        || model_out(host, worker).is_some()
}

/// Why `worker`'s CLI would refuse a session before starting one, such as OpenCode with no
/// model set, so nobody starts it only to hear so.
fn start_refusal(host: &Host, worker: &ModelRef) -> Option<String> {
    host.services().workers.get(worker).ok()?.start_refusal()
}

/// Whether `worker` is out before a session of its own starts: its CLI says it's logged out,
/// or its agent is down or its plan ran out.
pub(crate) async fn is_out(host: &Host, worker: &ModelRef) -> bool {
    is_logged_out(host, worker).await
        || unusable(host, worker).is_some()
        || model_out(host, worker).is_some()
}

/// Notes that `holder` has `job` as its own from the step's start, before a session of its own
/// shows it. A step that asks several agents at once holds each one's job first, so none
/// takes another's beside its own and does the same work twice.
pub(crate) fn hold(host: &Host, job: &Job, holder: &ModelRef) {
    let mut memory = lock(&host.shared().job_fallbacks);
    let held = memory
        .taken
        .iter()
        .any(|taken| taken.by == *holder && taken.asked == *holder && taken.job.same_place(job));
    if !held {
        memory.taken.push(Taken {
            job: job.clone(),
            asked: holder.clone(),
            by: holder.clone(),
        });
    }
}

/// Up to `count` agents that take `job`, `asked`'s, at once, beside those of `with` who have
/// it already (`fallback.<role>.together`). Each is remembered in the job, so no other job
/// of the step takes it. Fewer than `count` when fewer may take it, and none when the role's
/// fallbacks wait.
pub(crate) fn together(
    host: &Host,
    job: &Job,
    asked: &ModelRef,
    with: &[&ModelRef],
    count: usize,
) -> Vec<ModelRef> {
    let rules = JobRules::of(host, job, &asked.agent_id());
    if rules.applied.mode == FallbackMode::Wait {
        return Vec::new();
    }
    let mut memory = lock(&host.shared().job_fallbacks);
    let stop = Stopped {
        from: asked,
        failed: &[asked],
        why: None,
    };
    let takers: Vec<ModelRef> = candidates(host, job, asked, &rules, &stop, &memory)
        .into_iter()
        .filter(|taker| !with.contains(&taker))
        .take(count)
        .collect();
    for taker in &takers {
        memory.taken.push(Taken {
            job: job.clone(),
            asked: asked.clone(),
            by: taker.clone(),
        });
    }
    takers
}

/// The next agent for `job` once `from` ended for `reason`, after every one of `failed` did too,
/// for a session the caller starts itself. `Err(Stalled)` when nobody may take it.
pub(crate) async fn after(
    host: &Host,
    job: &Job,
    from: &ModelRef,
    failed: &[&ModelRef],
    reason: &EndReason,
) -> EngineResult<ModelRef> {
    remember_failed_start(host, from, reason);
    let why = if is_logged_out(host, from).await {
        Why::LoggedOut(Some(reason.clone()))
    } else {
        Why::Ended(reason.clone())
    };
    fall_back(host, job, from, from, failed, &why)
}

/// The worker the session of `record` starts again with, after it ended for an agent reason:
/// the same one once more, else the next that may take its job. After a resume the asked agent
/// starts over, unless it's out. `Err(Stalled)` when nobody can, with what to say.
pub(crate) async fn next(host: &Host, record: &SessionRecord, job: &Job) -> EngineResult<ModelRef> {
    if failures(host, record).is_empty() {
        let asked = record.asked_worker();
        return Ok(instead_of(host, asked, job)
            .await?
            .unwrap_or_else(|| asked.clone()));
    }
    if let Some(again) = again(host, record).await? {
        return Ok(again);
    }
    let failed = failures(host, record);
    let current = &record.started.worker;
    let reason = end_reason(record);
    remember_failed_start(host, current, &reason);
    let why = if is_logged_out(host, current).await {
        Why::LoggedOut(Some(reason))
    } else if went_again(&failed, current, &reason) {
        // A CLI that crashed twice in a row crashes the same way on its next job, where a busy
        // service may well have room by then.
        if matches!(reason, EndReason::Crashed { .. }) {
            let since = host.read(|state| state.run.last_resumed).unwrap_or(0);
            let failing = Failing {
                words: format!("{} twice in a row", in_words(&reason)),
                after: since,
                at: host.now(),
            };
            lock(&host.shared().job_fallbacks)
                .keeps_failing
                .insert(current.clone(), failing);
        }
        Why::EndedAgain(reason)
    } else {
        Why::Ended(reason)
    };
    fall_back(host, job, record.asked_worker(), current, &failed, &why)
}

/// The same agent again on the session of `record`, after the wait `[fallback.retry]` gives,
/// when its failure may pass, it has retries left since the run last resumed, and it hasn't
/// crashed twice in a row on an earlier job. A 401 its CLI's
/// own login check contradicts was passing. One line says it goes again, before the wait. After
/// a resume, the asked agent starts over, since whatever stopped it may have been fixed
/// meanwhile. `None` when it may not go again.
pub(crate) async fn again(host: &Host, record: &SessionRecord) -> EngineResult<Option<ModelRef>> {
    let failed = failures(host, record);
    if failed.is_empty() {
        return Ok(Some(record.asked_worker().clone()));
    }
    // Its task resumes it to ask for the report alone. Started over here, it would do the work
    // again and could miss the format again.
    if missed_only_the_report(record) {
        return Ok(None);
    }
    let current = &record.started.worker;
    // Once for each session's end: `next` asks here first.
    remember_startup_crash(host, current, record)?;
    if keeps_failing(host, current).is_some() {
        return Ok(None);
    }
    let reason = end_reason(record);
    let logged_in = login_check(host, current, &reason).await;
    let tries = failed.iter().filter(|worker| **worker == current).count();
    let tries = u32::try_from(tries).unwrap_or(u32::MAX);
    let retry = rules::applied(host, record.started.role, &current.agent_id()).retry;
    if tries <= retry.tries && reason.may_pass() && logged_in != Some(false) {
        let span = retry.wait_before(tries);
        say_retry(host, record, &reason, &retry, span)?;
        wait(host, span).await?;
        return Ok(Some(current.clone()));
    }
    Ok(None)
}

/// What the line that says who it fell back to opens with, such as `Still unreachable`, when the
/// session of `record` went again after a failure that might pass and ended the same way: the
/// retry line came before it.
pub(crate) fn still_out(host: &Host, record: &SessionRecord) -> Option<&'static str> {
    let failed = failures(host, record);
    let reason = end_reason(record);
    went_again(&failed, &record.started.worker, &reason).then(|| passing_words(&reason).1)
}

/// Whether `current` was tried again on its session and stopped again for a `reason` that
/// might pass.
fn went_again(failed: &[&ModelRef], current: &ModelRef, reason: &EndReason) -> bool {
    reason.may_pass() && failed.iter().filter(|worker| **worker == current).count() >= 2
}

/// `Opus 5.5 on Claude Code is overloaded · trying again in 5s`.
fn say_retry(
    host: &Host,
    record: &SessionRecord,
    reason: &EndReason,
    retry: &Retry,
    span: Duration,
) -> EngineResult<()> {
    let name = display_name(&record.started.worker);
    let (stopped, _) = passing_words(reason);
    let again = if span.is_zero() {
        "trying again".to_owned()
    } else {
        format!("trying again in {}", wait_in_words(retry, span))
    };
    note(
        host,
        record.task.as_ref(),
        format!("{name} {stopped} · {again}"),
    )
}

/// A retry's wait as its line says it: the span as you wrote it while it hasn't grown, else
/// `10s`, `40s`, `1m 20s`.
fn wait_in_words(retry: &Retry, span: Duration) -> String {
    if retry.wait.parse().ok().flatten() == Some(span) {
        return retry.wait.0.trim().to_owned();
    }
    let seconds = span.as_secs();
    match (seconds / 60, seconds % 60) {
        (0, seconds) => format!("{seconds}s"),
        (minutes, 0) => format!("{minutes}m"),
        (minutes, seconds) => format!("{minutes}m {seconds}s"),
    }
}

/// A failure that may pass: what the agent did, for the retry line, and how the line after
/// opens when the retry ended the same way.
fn passing_words(reason: &EndReason) -> (&'static str, &'static str) {
    const SIGN_IN: (&str, &str) = ("got a sign-in error", "Still a sign-in error");
    const RATE: (&str, &str) = ("is rate limited", "Still rate limited");
    match reason {
        EndReason::Auth { .. } => SIGN_IN,
        EndReason::RateLimited => RATE,
        EndReason::StartFailed { .. } => ("couldn't start", "Still can't start"),
        EndReason::CliError { category, .. } => match category {
            ErrorCategory::Auth => SIGN_IN,
            ErrorCategory::RateLimit => RATE,
            // An overload is about the one model, where the others say nothing got through.
            ErrorCategory::Overloaded => ("is overloaded", "Still overloaded"),
            ErrorCategory::Server | ErrorCategory::Network | ErrorCategory::Timeout => {
                ("couldn't be reached", "Still unreachable")
            }
            _ => ("gave an error", "Still failing"),
        },
        _ => ("crashed", "Crashed again"),
    }
}

/// The CLI's own word on its login, asked only after a sign-in error: `Some(false)` is logged
/// out, and the agent is out of the run until it says otherwise. A 401 the check contradicts
/// was passing.
pub(crate) async fn login_check(
    host: &Host,
    worker: &ModelRef,
    reason: &EndReason,
) -> Option<bool> {
    if !reason.is_sign_in_error() {
        return None;
    }
    // A token revoked on the server looks fine to the CLI's own check, so the error's words win.
    let logged_in = if says_signed_out(reason) {
        Some(false)
    } else {
        logged_in_now(host, worker).await
    };
    let login = Login {
        at: Instant::now(),
        logged_out: logged_in == Some(false),
    };
    lock(&host.shared().job_fallbacks)
        .logins
        .insert(worker.clone(), login);
    logged_in
}

/// A sign-in error whose own words say the sign-in is gone, as a conversation reads them too.
pub(crate) fn says_signed_out(reason: &EndReason) -> bool {
    argonauts_core::sign_in::signed_out(reason, "").is_some()
}

/// Whether `worker`'s CLI says it's logged out: as it said within the breaker's cool-down, else
/// asked again when it said so before, or when its last session in the run ended on a sign-in
/// error. An agent that logged in meanwhile is back.
async fn is_logged_out(host: &Host, worker: &ModelRef) -> bool {
    let known = lock(&host.shared().job_fallbacks)
        .logins
        .get(worker)
        .copied();
    match known {
        Some(login) if login.at.elapsed() < recheck_after(host) => return login.logged_out,
        Some(login) if login.logged_out => {}
        _ if !host.read(|state| last_ended_on_sign_in(state, worker)) => return false,
        _ => {}
    }
    let logged_out = logged_in_now(host, worker).await == Some(false);
    let login = Login {
        at: Instant::now(),
        logged_out,
    };
    lock(&host.shared().job_fallbacks)
        .logins
        .insert(worker.clone(), login);
    logged_out
}

/// Whether `worker`'s newest session in the run ended on a sign-in error.
fn last_ended_on_sign_in(state: &RunState, worker: &ModelRef) -> bool {
    state.sessions.sign_in_failed.get(worker) == Some(&true)
}

/// Names a stalled job by `own`, the model it was meant for, as signed out where its newest
/// session ended on a sign-in error, when the stall names a fallback the job went on to: one
/// it may have found free only because the step's other jobs were still starting.
pub(crate) fn named_by_its_own(host: &Host, stall: &mut Stall, own: &ModelRef) {
    if stall.job.asked.as_ref() == Some(own)
        || !host.read(|state| last_ended_on_sign_in(state, own))
    {
        return;
    }
    let (short, said) = failure_words(&Why::LoggedOut(None), own);
    stall.job.asked = Some(own.clone());
    stall.job.short = short;
    stall.job.said = said;
}

async fn logged_in_now(host: &Host, worker: &ModelRef) -> Option<bool> {
    let worker = host.services().workers.get(worker).ok()?;
    worker.logged_in().await
}

fn recheck_after(host: &Host) -> Duration {
    let cooldown = &host.settings().health.cooldown;
    cooldown
        .parse()
        .ok()
        .flatten()
        .unwrap_or(Duration::from_secs(300))
}

/// Why `worker`'s agent can't take work now, in words, or `None` when it can.
fn unusable(host: &Host, worker: &ModelRef) -> Option<&'static str> {
    let agent_id = worker.agent_id();
    if rules::agent_usable(host, &agent_id) {
        return None;
    }
    Some(match host.services().health.state(&agent_id).state {
        HealthState::Down => "its agent is down",
        HealthState::Testing => "its agent is being tested after an outage",
        HealthState::Healthy | HealthState::Degraded => "it's out of usage until its plan resets",
    })
}

/// Why `worker` itself can't take work now while its CLI's other models can, in words: two
/// overloads or server errors in a row took the model out until its test task.
fn model_out(host: &Host, worker: &ModelRef) -> Option<&'static str> {
    let health = host.services().health.model(worker);
    (health.state == ModelState::Out).then_some(match health.trouble {
        Some(Trouble::ServerErrors) => "it keeps giving server errors",
        Some(Trouble::Overload) | None => "it's overloaded",
    })
}

/// Its agent failures since the run last resumed, by the worker each time.
fn failures<'r>(host: &Host, record: &'r SessionRecord) -> Vec<&'r ModelRef> {
    let since = host.read(|state| state.run.last_resumed).unwrap_or(0);
    record
        .agent_failures
        .iter()
        .filter(|(_, seq)| *seq > since)
        .map(|(worker, _)| worker)
        .collect()
}

/// A task's build session that worked, then missed its answer format, and can be resumed.
fn missed_only_the_report(record: &SessionRecord) -> bool {
    record.task.is_some()
        && record.started.step == Some(StepName::Build)
        && record.end.as_ref().is_some_and(|end| {
            matches!(
                end.reason,
                EndReason::CliError {
                    category: ErrorCategory::AnswerMissed,
                    ..
                }
            ) && end.tool_calls > 0
                && end.cli_session.is_some()
        })
}

fn end_reason(record: &SessionRecord) -> EndReason {
    record.end.as_ref().map_or_else(
        || crate::engine::host::crashed_end("").reason,
        |end| end.reason.clone(),
    )
}

/// The first agent that may take `job`, `asked`'s, over from `from`, claimed so no other job
/// of the same step goes to it too, with the fallback said once. `Err(Stalled)` when there's
/// none.
fn fall_back(
    host: &Host,
    job: &Job,
    asked: &ModelRef,
    from: &ModelRef,
    failed: &[&ModelRef],
    why: &Why,
) -> EngineResult<ModelRef> {
    let rules = JobRules::of(host, job, &from.agent_id());
    let (mut kept, mut beside) = (None, None);
    if rules.applied.mode != FallbackMode::Wait {
        let stop = Stopped {
            from,
            failed,
            why: Some(why),
        };
        let (taker, anywhere) = {
            let mut memory = lock(&host.shared().job_fallbacks);
            let taker = candidates(host, job, asked, &rules, &stop, &memory)
                .into_iter()
                .next();
            if let Some(taker) = &taker {
                memory.taken.push(Taken {
                    job: job.clone(),
                    asked: asked.clone(),
                    by: taker.clone(),
                });
            }
            let anywhere = without_same_cli(host, job, asked, &rules, &stop, &memory);
            if taker.is_none() {
                beside = beside_in_step(host, job, asked, &rules, &stop, &memory);
            }
            (taker, anywhere)
        };
        if let Some(taker) = taker {
            // Why no other CLI was asked, where another would have come first.
            let kept_off = anywhere
                .first()
                .map(ModelRef::agent_id)
                .filter(|first| *first != asked.agent_id());
            say_fallback(host, job, from, &taker, why, kept_off.as_ref())?;
            return Ok(listed(host, &taker));
        }
        if let Some(taker) = builders_provider_on_autopilot(host, job, asked, &rules, &stop) {
            say_builders_provider(host, job, from, &taker, why)?;
            return Ok(taker);
        }
        kept = kept_on_its_cli(host, job, asked, anywhere)
            .or_else(|| left_to_builders_provider(host, job, asked, &rules, &stop));
    }
    let mut stall = stalled(host, job, from, failed, why, &rules.applied, kept);
    if let EngineError::Stalled(stall) = &mut stall {
        stall.beside = beside;
    }
    Err(stall)
}

/// Other models of the CLIs at work in `job`'s step that may take it when nothing else can,
/// as `fallback.same_step` lets: each CLI's models nearest the one it runs there first. `None`
/// where the setting says never, where `same_cli` keeps the job on its own CLI, or where none
/// may take it.
fn beside_in_step(
    host: &Host,
    job: &Job,
    asked: &ModelRef,
    rules: &JobRules,
    stop: &Stopped<'_>,
    memory: &Memory,
) -> Option<Beside> {
    let applied = &rules.applied;
    if applied.same_step == SameStep::Never || applied.same_cli {
        return None;
    }
    let registry = host.services().registry.as_ref();
    let at_work = host.read(|state| at_work_in_step(state, job, stop.from));
    let mut pool: Vec<FallbackEntry> = Vec::new();
    for model in &at_work {
        for other in other_models(registry, model) {
            let entry = FallbackEntry::Model(other);
            if !at_work.contains(entry.model()) && !pool.contains(&entry) {
                pool.push(entry);
            }
        }
    }
    let beside = JobRules {
        applied: AppliedFallback {
            candidates: pool,
            same_cli: false,
            ..applied.clone()
        },
        first: None,
        anyone: false,
        second_in_step: true,
    };
    let models = candidates(host, job, asked, &beside, stop, memory);
    (!models.is_empty()).then(|| Beside {
        models,
        ask: applied.same_step == SameStep::Ask,
    })
}

/// On autopilot nobody is at the terminal to ask, so a task's review or ruling that nobody
/// outside the builder's provider can take goes to that provider after all, claimed like any
/// other fallback. The merge then waits for you, as
/// [`crate::approvals::builders_provider_reviewed`] finds from the session it starts. A role's
/// model the step can do without, such as a second reviewer or a judge of several plans, is
/// left out instead, so it never gets here.
fn builders_provider_on_autopilot(
    host: &Host,
    job: &Job,
    asked: &ModelRef,
    rules: &JobRules,
    stop: &Stopped<'_>,
) -> Option<ModelRef> {
    let on_autopilot = crate::approvals::autopilot::applies_now(
        host,
        argonauts_core::events::approval::ApprovalKind::Fallback,
    );
    if !on_autopilot {
        return None;
    }
    let open = without_its_builders(host, job, asked)?;
    let mut memory = lock(&host.shared().job_fallbacks);
    let taker = candidates(host, &open, asked, rules, stop, &memory)
        .into_iter()
        .next()?;
    memory.taken.push(Taken {
        job: job.clone(),
        asked: asked.clone(),
        by: taker.clone(),
    });
    Some(taker)
}

/// The one line that says the fallback is from the builder's own provider, and that the merge
/// is yours.
fn say_builders_provider(
    host: &Host,
    job: &Job,
    from: &ModelRef,
    taker: &ModelRef,
    why: &Why,
) -> EngineResult<()> {
    let new = lock(&host.shared().job_fallbacks)
        .said
        .insert((from.clone(), taker.clone()));
    if !new {
        return Ok(());
    }
    let stopped = stopped_in_words(&display_name(from), why);
    let text = format!(
        "{stopped}, and nobody outside the builder's provider is left, so {} takes {} on \
         autopilot · the merge is left to you",
        display_name(taker),
        job_in_words(job),
    );
    note(
        host,
        job.task.as_ref(),
        argonauts_core::text::sentence_case(&text),
    )
}

/// `job` as it would be if its builder's provider could take it, where that is the question:
/// a task's review or ruling, alone in its step. A second reviewer or a judge of several
/// plans is left out instead.
fn without_its_builders(host: &Host, job: &Job, asked: &ModelRef) -> Option<Job> {
    let reviews_a_task =
        job.task.is_some() && matches!(job.role, RoleName::Reviewer | RoleName::Judge);
    if job.independent_of.is_empty() || !reviews_a_task {
        return None;
    }
    if !host.read(|state| others_in_step(state, job, asked).is_empty()) {
        return None;
    }
    Some(Job {
        independent_of: Vec::new(),
        ..job.clone()
    })
}

/// The wait of a task's review or ruling that only its builder's own provider could take
/// now, for the question that lets you choose: wait for the agent it was asked of, let that
/// provider take it with the merge left to you, or pick an agent. `elsewhere` lists the role's
/// own fallbacks first, then every other installed agent that may take it. `None` when the
/// builder's provider couldn't take it either.
fn left_to_builders_provider(
    host: &Host,
    job: &Job,
    asked: &ModelRef,
    rules: &JobRules,
    stop: &Stopped<'_>,
) -> Option<JobWait> {
    let open = without_its_builders(host, job, asked)?;
    let mut elsewhere = {
        let memory = lock(&host.shared().job_fallbacks);
        let by_rules = candidates(host, &open, asked, rules, stop, &memory);
        if by_rules.is_empty() {
            return None;
        }
        let anyone = JobRules {
            applied: AppliedFallback {
                same_cli: false,
                ..rules.applied.clone()
            },
            first: None,
            anyone: true,
            second_in_step: false,
        };
        let rest = candidates(host, &open, asked, &anyone, stop, &memory);
        let mut elsewhere = by_rules;
        elsewhere.extend(rest);
        elsewhere
    };
    let mut seen = BTreeSet::new();
    elsewhere.retain(|model| seen.insert(model.clone()));
    let from = display_name(stop.from);
    let why = match stop.why {
        Some(why) => stopped_in_words(&from, why),
        None => format!("{from} stopped"),
    };
    Some(JobWait {
        job: job.named(),
        stopped: asked.clone(),
        elsewhere,
        title: job_title(host, job),
        kept_by: JobKeeper::BuildersProvider {
            builders: job.independent_of.clone(),
            why,
        },
    })
}

/// What a waiting job is called: its task's title, or its role in words.
fn job_title(host: &Host, job: &Job) -> String {
    let title = host.read(|state| {
        let task = job.task.as_ref().and_then(|task| state.tasks.get(task));
        task.map(|record| record.spec.title.clone())
    });
    title.unwrap_or_else(|| argonauts_core::text::sentence_case(&job_in_words(job)))
}

/// `Haiku 5.5's work`, or `the work of Haiku 5.5 and Sonnet 5.5` for several builders.
pub(crate) fn work_of(builders: &[ModelRef]) -> String {
    let names: Vec<String> = builders.iter().map(roles::short_name).collect();
    let mut unique: Vec<String> = Vec::new();
    for name in names {
        if !unique.contains(&name) {
            unique.push(name);
        }
    }
    match unique.as_slice() {
        [] => "the builder's work".to_owned(),
        [one] => format!("{one}'s work"),
        [rest @ .., last] => format!("the work of {} and {last}", rest.join(", ")),
    }
}

/// What a role does to the work it is given, for a line about it: `review` or `judge`.
pub(crate) fn verb_of(role: RoleName) -> &'static str {
    match role {
        RoleName::Judge => "judge",
        _ => "review",
    }
}

/// Who would take `job` if `same_cli` didn't keep it on its own CLI, best first. Nobody
/// where the setting is off.
fn without_same_cli(
    host: &Host,
    job: &Job,
    asked: &ModelRef,
    rules: &JobRules,
    stop: &Stopped<'_>,
    memory: &Memory,
) -> Vec<ModelRef> {
    if !rules.applied.same_cli {
        return Vec::new();
    }
    let anywhere = JobRules {
        applied: AppliedFallback {
            same_cli: false,
            ..rules.applied.clone()
        },
        first: None,
        anyone: false,
        second_in_step: false,
    };
    candidates(host, job, asked, &anywhere, stop, memory)
}

/// The wait of a job that only `same_cli` keeps from an agent of another CLI, for the
/// question about its CLI. `None` when no other CLI's agent could take it either.
fn kept_on_its_cli(
    host: &Host,
    job: &Job,
    asked: &ModelRef,
    anywhere: Vec<ModelRef>,
) -> Option<JobWait> {
    let elsewhere: Vec<ModelRef> = anywhere
        .into_iter()
        .filter(|model| model.agent != asked.agent)
        .collect();
    if elsewhere.is_empty() {
        return None;
    }
    Some(JobWait {
        job: job.named(),
        stopped: asked.clone(),
        elsewhere,
        title: job_title(host, job),
        kept_by: JobKeeper::SameCli,
    })
}

/// What a job's fallbacks are chosen against: who stopped last and why, and everyone who
/// failed in the job before.
struct Stopped<'a> {
    from: &'a ModelRef,
    failed: &'a [&'a ModelRef],
    /// `None` when nobody failed: the job only wants more agents beside its own.
    why: Option<&'a Why>,
}

/// Who may take `job`, `asked`'s, now, best first: the role's fallback list when one names any,
/// else, in a crew run, the crew's other agents in crew order and then whatever else is installed.
/// A Captain run works like the CLI you chose, so only a list you set brings in another. With
/// `same_cli` only the asked CLI's own models may, the listed ones first and then its others,
/// and none of them once the CLI stopped as a whole. Left out:
/// a model that failed here, every model of an agent that failed here unless only its model
/// did (an overload or server errors), one whose CLI couldn't start earlier in the run or would
/// refuse to now, one that's out, one another CLI serves from the provider
/// that took the stopped CLI down, one in
/// another model's job of the same step and round or of another CLI working there (unless
/// `second_in_step`), one of the provider of whoever it must be
/// independent of, one that may not judge the task's own work, one whose CLI can't keep your
/// plugins out of an isolated job, and every one of an agent whose CLI loaded them
/// anyway. Who was already the fallback for `asked` here may be again, as a main agent's later
/// asks in one step are the same job. A job you let leave its CLI goes first to the agent you
/// gave it to, when that one may take it.
fn candidates(
    host: &Host,
    job: &Job,
    asked: &ModelRef,
    rules: &JobRules,
    stop: &Stopped<'_>,
    memory: &Memory,
) -> Vec<ModelRef> {
    let services = host.services();
    let applied = &rules.applied;
    let (listed, failed) = (&applied.candidates, stop.failed);
    // A CLI that loaded your plugins into a blind role's session does so on any model.
    let leaked = matches!(
        stop.why,
        Some(Why::Ended(EndReason::IsolationBroken { .. }))
    )
    .then(|| stop.from.agent_id());
    let model_only = stop.why.is_some_and(Why::model_only);
    let stopped: Vec<_> = failed
        .iter()
        .filter(|model| !(model_only && **model == stop.from))
        .map(|model| model.agent_id())
        .collect();
    let pool: Vec<ModelRef> = if applied.same_cli {
        let whole_cli = stop.why.is_some() && !model_only;
        let mut pool: Vec<ModelRef> = listed.iter().map(|entry| entry.model().clone()).collect();
        pool.extend(services.workers.available());
        pool.retain(|model| model.agent == asked.agent && !whole_cli);
        pool
    } else if listed.is_empty() && host.shared().mode == RunMode::Captain && !rules.anyone {
        Vec::new()
    } else if listed.is_empty() || rules.anyone {
        let available = services.workers.available();
        // Your list first where anyone may take it, then the crew.
        let mut pool: Vec<ModelRef> = listed.iter().map(|entry| entry.model().clone()).collect();
        pool.extend(
            host.settings()
                .crew
                .members()
                .into_iter()
                .filter(|model| available.contains(model)),
        );
        // The rest of what's installed after the crew; a repeat is dropped below.
        pool.extend(available);
        pool.retain(|model| {
            !stopped.contains(&model.agent_id())
                && services
                    .workers
                    .get(model)
                    .is_ok_and(|agent| agent.kind() == RunnerKind::Cli)
        });
        pool
    } else {
        listed.iter().map(|entry| entry.model().clone()).collect()
    };
    // A model its CLI refused leaves the CLI's other models ahead of any other CLI, those of
    // its line nearest first. Which model a CLI's own default was isn't known, so that one
    // leaves none.
    let refused = pinned(host, stop.from);
    let siblings = match stop.why {
        Some(why) if why.refused_model() && refused.model.is_some() => {
            other_models(services.registry.as_ref(), &refused)
        }
        _ => Vec::new(),
    };
    // The agent you gave the job to comes before the lists. A repeat is dropped below.
    let pool = rules.first.iter().cloned().chain(siblings).chain(pool);
    let (worker, others) = host.read(|state| {
        let worker = job
            .task
            .as_ref()
            .and_then(|task| state.tasks.get(task).and_then(rules::worker));
        (worker, others_in_step(state, job, asked))
    });
    let provider = |model: &ModelRef| services.registry.provider(model);
    let resumed = host.read(|state| state.run.last_resumed).unwrap_or(0);
    // An agent whose sessions crashed twice in a row, or whose CLI couldn't start, would only
    // fail in this job too.
    let crashing = |model: &ModelRef| {
        memory.failing(model).is_some_and(|failing| {
            failing.holds(resumed, &services.health.state(&model.agent_id()))
        })
    };
    let providers: Vec<String> = job.independent_of.iter().map(&provider).collect();
    // Two models out took the stopped CLI down, so its provider fails whoever serves it.
    let failing = stop
        .why
        .filter(|why| why.model_only())
        .filter(|_| services.health.state(&stop.from.agent_id()).state == HealthState::Down)
        .map(|_| provider(stop.from));
    // The agent you gave the job to may take it whoever built the work: you were told the
    // merge is then yours.
    let chosen = |model: &ModelRef| rules.first.as_ref() == Some(model);
    let mut kept = Vec::new();
    for model in pool {
        // Its own CLI's other models may, but not a CLI another job of the step is on.
        let in_step = others.contains(&model)
            || (!rules.second_in_step
                && model.agent != asked.agent
                && others.iter().any(|other| other.agent == model.agent));
        let elsewhere = in_step
            || memory.taken.iter().any(|taken| {
                taken.by == model && taken.asked != *asked && taken.job.same_place(job)
            });
        let usable = !failed.contains(&&model)
            && !kept.contains(&model)
            && !elsewhere
            && !memory.logged_out(&model)
            && !crashing(&model)
            && (chosen(&model) || !providers.contains(&provider(&model)))
            && services
                .workers
                .get(&model)
                .is_ok_and(|runner| runner.start_refusal().is_none())
            && rules::takes_work(host, &model)
            && !(model.agent != stop.from.agent && failing == Some(provider(&model)))
            && roles::conflict(job.role, &model, worker.as_ref(), &provider).is_none()
            && leaked.as_ref() != Some(&model.agent_id())
            && (!job.isolated || services.workers.isolates(&model));
        if usable {
            kept.push(model);
        }
    }
    kept
}

/// The agents in the other jobs of `job`'s step and round, those not asked of `asked`: whoever
/// each was asked of, and whoever runs it now.
fn others_in_step(state: &RunState, job: &Job, asked: &ModelRef) -> Vec<ModelRef> {
    let (from, until) = round_window(state, job.at.as_ref());
    let step = job.at.as_ref().map(|at| &at.step);
    let mut others = Vec::new();
    for session in state.sessions.by_key.values() {
        let here = session.since >= from
            && until.is_none_or(|until| session.since < until)
            && session.started.workflow_step.as_ref() == step
            && session.started.role == job.role
            && session.started.step == job.step
            && session.task == job.task
            && session.asked_worker() != asked;
        if here {
            others.push(session.asked_worker().clone());
            others.push(session.started.worker.clone());
        }
    }
    others
}

/// Says who the job fell back to, once for each pair of agents: a later fallback between the same
/// two says nothing new. An agent whose CLI says it's logged out gets the login fix with it.
fn say_fallback(
    host: &Host,
    job: &Job,
    from: &ModelRef,
    taker: &ModelRef,
    why: &Why,
    kept_off: Option<&AgentId>,
) -> EngineResult<()> {
    let new = lock(&host.shared().job_fallbacks)
        .said
        .insert((from.clone(), taker.clone()));
    // A session `taker` already runs in `from`'s place, before a resume, said it then.
    let made_before = host.read(|state| {
        state
            .sessions
            .by_key
            .values()
            .any(|session| session.asked_worker() == from && session.started.worker == *taker)
    });
    if !new || made_before {
        return Ok(());
    }
    let (from_name, taker_name) = (display_name(from), display_name(taker));
    let text = match why {
        Why::LoggedOut(_) => format!(
            "{from_name} says it's logged out: log in again; {taker_name} takes its roles \
             meanwhile."
        ),
        Why::Unusable(words) | Why::ModelOut(words) => {
            format!("{from_name} is out ({words}), so {taker_name} takes its roles meanwhile.")
        }
        // The failure's own line said what went wrong, and named the model that failed, so
        // this one names only the CLI that steps in.
        Why::Ended(reason) if repeats(reason) => format!(
            "Fell back to {} as {} · {NO_SECOND_TRY}",
            DisplayNames::default().cli(taker.agent.as_str()),
            job_as_role(job),
        ),
        Why::Ended(reason) => format!(
            "{from_name}'s session as {} ended early ({}), so it fell back to {taker_name}.",
            job_in_words(job),
            in_words(reason),
        ),
        // The retry line said who stopped and why.
        Why::EndedAgain(reason) => format!(
            "{} · fell back to {taker_name} as {}",
            passing_words(reason).1,
            job_in_words(job)
        ),
        Why::KeepsFailing(words) => format!(
            "{from_name} isn't started again this run, since {words}; {taker_name} takes its \
             roles."
        ),
    };
    // Why no other CLI was asked, where another would have come first.
    let text = match kept_off {
        Some(other) => format!(
            "{text}\n{SAME_CLI_IS_ON}{} isn't asked",
            DisplayNames::default().cli(other.0.as_str())
        ),
        None => text,
    };
    note(host, job.task.as_ref(), text)?;
    // The workflow whose step it is says so under its header as well.
    if let Some(at) = &job.at {
        let line = in_the_view(&from_name, &taker_name, Some(why), kept_off.is_some());
        say_in_the_view(host, &at.step.workflow, line)?;
    }
    Ok(())
}

/// Says `line` about `workflow`, whose view shows the latest such line under its header.
pub(crate) fn say_in_the_view(
    host: &Host,
    workflow: &WorkflowId,
    line: String,
) -> EngineResult<()> {
    let about = RunNote {
        text: line,
        workflow: Some(workflow.clone()),
        left: false,
    };
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Note(about)),
    )?;
    Ok(())
}

/// What a workflow's view says under its header when one of its steps goes to another model:
/// who stopped and why, then who it fell back to, as in `Opus 5.5 on Claude Code is overloaded
/// · fell back to Sonnet 5.5 on Claude Code, as same_cli asks`. A failure that might have
/// passed reads as its retry line said it, and no `why` says only that it stopped. `same_cli`
/// adds that the setting is why no other CLI was asked.
fn in_the_view(from_name: &str, taker_name: &str, why: Option<&Why>, same_cli: bool) -> String {
    let stopped = match why {
        Some(Why::Ended(reason) | Why::EndedAgain(reason)) if reason.may_pass() => {
            format!("{from_name} {}", passing_words(reason).0)
        }
        Some(why) => stopped_in_words(from_name, why),
        None => format!("{from_name} stopped"),
    };
    let as_asked = if same_cli { ", as same_cli asks" } else { "" };
    format!("{stopped} · fell back to {taker_name}{as_asked}")
}

/// The same line for a task that fell back to another worker, from how its last session with
/// the one that stopped ended. A session that finished, or that you, a pause or the task's own
/// limit ended, is no failure of its agent, so the line gives no reason for it. Each is named
/// by the model it runs, and a taker that runs the very model that stopped says so: `fell back
/// to Qwen3 Coder 30B on OpenCode (the same model)`.
pub(crate) fn task_in_the_view(
    from: &ModelRef,
    taker: &ModelRef,
    ended: Option<&EndReason>,
    same_cli: bool,
) -> String {
    let why = ended
        .filter(|reason| reason.is_agent_failure())
        .map(|reason| Why::Ended(reason.clone()));
    let same_model = taker.model.is_some() && taker == from;
    let taker_name = if same_model {
        format!("{} (the same model)", display_name(taker))
    } else {
        display_name(taker)
    };
    in_the_view(&display_name(from), &taker_name, why.as_ref(), same_cli)
}

/// Who stopped and why, in one clause, such as `Opus 5.5 on Claude Code is signed out`.
fn stopped_in_words(from_name: &str, why: &Why) -> String {
    match why {
        Why::Ended(reason) | Why::EndedAgain(reason) | Why::LoggedOut(Some(reason)) => {
            format!("{from_name}'s session ended early ({})", in_words(reason))
        }
        Why::LoggedOut(None) => format!("{from_name} is signed out"),
        Why::Unusable(words) | Why::ModelOut(words) => format!("{from_name} is out ({words})"),
        Why::KeepsFailing(words) => format!("{from_name}'s sessions keep ending ({words})"),
    }
}

/// An error the agent's service gave that comes back the same on a retry, such as an answer
/// format it refuses, so its job goes to the next agent at once.
fn repeats(reason: &EndReason) -> bool {
    matches!(
        reason,
        EndReason::CliError { .. } | EndReason::Auth { .. } | EndReason::StartFailed { .. }
    ) && !reason.may_pass()
}

/// The stall when nobody may take `job`: who stopped and why, whose CLI says it's logged out,
/// and when a usage limit resets.
fn stalled(
    host: &Host,
    job: &Job,
    from: &ModelRef,
    failed: &[&ModelRef],
    why: &Why,
    applied: &AppliedFallback,
    kept: Option<JobWait>,
) -> EngineError {
    let mode = applied.mode;
    let memory = lock(&host.shared().job_fallbacks);
    let mut logged_out: Vec<String> = Vec::new();
    for worker in failed.iter().copied().chain([from]) {
        let name = display_name(worker);
        if memory.logged_out(worker) && !logged_out.contains(&name) {
            logged_out.push(name);
        }
    }
    drop(memory);
    let role = job_in_words(job);
    let from_name = display_name(from);
    let stopped = stopped_in_words(&from_name, why);
    let cli = DisplayNames::default().cli(from.agent.as_str());
    let builders = match kept.as_ref().map(|wait| &wait.kept_by) {
        Some(JobKeeper::BuildersProvider { builders, .. }) => Some(builders),
        _ => None,
    };
    let (detail, line) = if let Some(builders) = builders.filter(|_| mode == FallbackMode::Auto) {
        (
            format!("{role} waits for {from_name}"),
            format!(
                "{} waits for {from_name}: {stopped}, and nobody outside the builder's provider \
                 is left to {} {}.",
                argonauts_core::text::sentence_case(&role),
                verb_of(job.role),
                work_of(builders),
            ),
        )
    } else if mode == FallbackMode::Wait {
        (
            format!("{role} waits for {from_name}"),
            format!(
                "{} waits for {from_name}: {stopped}, and your fallback settings say to wait \
                 for it.",
                argonauts_core::text::sentence_case(&role)
            ),
        )
    } else if applied.same_cli {
        (
            format!("nobody to fall back to as {role}"),
            format!(
                "{NOBODY_TO_FALL_BACK_TO} as {role}: {stopped}, and fallback.same_cli keeps \
                 it on {cli}."
            ),
        )
    } else {
        let other = if job.isolated {
            "no other agent that keeps your plugins out"
        } else {
            "no other agent"
        };
        (
            format!("nobody to fall back to as {role}"),
            format!(
                "{NOBODY_TO_FALL_BACK_TO} as {role}: {stopped}, and {other} can take the role."
            ),
        )
    };
    let resets_at = match why {
        Why::Ended(EndReason::UsageLimit { resets_at }) => *resets_at,
        Why::Unusable(_) => host
            .services()
            .plan_usage
            .get(&from.agent_id())
            .and_then(|reading| reading.resets_at),
        _ => None,
    };
    let (short, said) = failure_words(why, from);
    EngineError::Stalled(Box::new(Stall {
        detail,
        line,
        logged_out,
        resets_at,
        // A role told to ask or to wait has its own answer already.
        kept: kept.filter(|_| mode == FallbackMode::Auto).map(Box::new),
        job: StalledJob {
            name: role,
            asked: Some(from.clone()),
            short,
            said,
            signed_out: why.signed_out(),
        },
        tried: {
            let mut tried = tried_elsewhere(host, from);
            if applied.same_cli && mode != FallbackMode::Wait {
                tried.push(format!("fallback.same_cli keeps it on {cli}"));
            }
            tried
        },
        in_step: working_in_step(host, job, from),
        stop: None,
        // The caller adds who could take it beside the step's other agents.
        beside: None,
    }))
}

/// What `from`'s own CLI said, in short words for its row, `signed out`, and whole for the
/// run's stop, `Antigravity CLI is signed out`.
fn failure_words(why: &Why, from: &ModelRef) -> (String, String) {
    let cli = DisplayNames::default().cli(from.agent.as_str());
    let model = |id: &str| argonauts_core::model_catalog::display_name_from_id(id);
    match why {
        Why::LoggedOut(_) | Why::Ended(EndReason::Auth { .. }) => {
            ("signed out".to_owned(), format!("{cli} is signed out"))
        }
        Why::Unusable(words) | Why::ModelOut(words) => {
            ((*words).to_owned(), format!("{cli} is out: {words}"))
        }
        Why::KeepsFailing(words) => {
            let words = words.strip_prefix("it ").unwrap_or(words);
            (words.to_owned(), format!("{cli} {words}"))
        }
        Why::Ended(reason) | Why::EndedAgain(reason) => match reason {
            EndReason::ModelUnavailable { model: id } => {
                let name = model(id);
                (
                    format!("can't run {name}"),
                    format!("{cli} can't run {name}"),
                )
            }
            EndReason::StartFailed { message } => (
                "couldn't start".to_owned(),
                format!("{cli} couldn't start: {}", first_line(message)),
            ),
            reason => {
                let words = in_words(reason);
                (
                    words.clone(),
                    format!("{cli}'s session ended early ({words})"),
                )
            }
        },
    }
}

/// A task's job as the run's stop names it, when nothing could take the task from `from`: its
/// title, the model it was asked of, and what its CLI said as its session ended, with why
/// each other CLI couldn't take it.
pub(crate) fn stalled_task(
    host: &Host,
    title: &str,
    from: &ModelRef,
    ended: Option<&EndReason>,
    logged_out: bool,
) -> (StalledJob, Vec<String>) {
    let why = match ended {
        _ if logged_out => Why::LoggedOut(ended.cloned()),
        Some(reason) => Why::Ended(reason.clone()),
        None => Why::Unusable("it stopped"),
    };
    let (short, said) = failure_words(&why, from);
    let job = StalledJob {
        name: format!("\"{}\"", title.trim()),
        asked: Some(from.clone()),
        short,
        said,
        signed_out: why.signed_out(),
    };
    (job, tried_elsewhere(host, from))
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// Why each other installed CLI couldn't take the job either, one clause each, as far as the
/// run knows without asking: `OpenCode has no model set`, `Codex is signed out`.
fn tried_elsewhere(host: &Host, from: &ModelRef) -> Vec<String> {
    let names = DisplayNames::default();
    let memory = lock(&host.shared().job_fallbacks);
    let mut tried: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    for model in host.services().workers.available() {
        if model.agent == from.agent || !seen.insert(model.agent.clone()) {
            continue;
        }
        let cli = names.cli(model.agent.as_str());
        let why = if let Some(refusal) = start_refusal(host, &model) {
            // A refusal says how to fix it after its first clause.
            first_line(&refusal)
                .split(", and ")
                .next()
                .unwrap_or_default()
                .trim_end_matches('.')
                .to_owned()
        } else if memory.logged_out(&model) {
            format!("{cli} is signed out")
        } else {
            continue;
        };
        tried.push(why);
    }
    tried
}

/// The CLIs at work in `job`'s step and round beside `from`'s job, by name: two agents of one
/// step never share a CLI, so the proposals stay independent. A job whose session ended on
/// its agent's failure isn't at work.
fn working_in_step(host: &Host, job: &Job, from: &ModelRef) -> Vec<String> {
    let names = DisplayNames::default();
    let mut working: Vec<String> = Vec::new();
    for model in host.read(|state| at_work_in_step(state, job, from)) {
        let cli = names.cli(model.agent.as_str());
        if !working.contains(&cli) {
            working.push(cli);
        }
    }
    working
}

/// The models at work in `job`'s step and round beside `from`'s job, those of other CLIs, in
/// the order their sessions started. A job whose session ended on its agent's failure isn't
/// at work.
fn at_work_in_step(state: &RunState, job: &Job, from: &ModelRef) -> Vec<ModelRef> {
    let (from_seq, until) = round_window(state, job.at.as_ref());
    let step = job.at.as_ref().map(|at| &at.step);
    let mut sessions: Vec<_> = state
        .sessions
        .by_key
        .values()
        .filter(|session| {
            session.since >= from_seq
                && until.is_none_or(|until| session.since < until)
                && session.started.workflow_step.as_ref() == step
                && session.started.role == job.role
                && session.started.step == job.step
                && session.task == job.task
                && session.started.worker.agent != from.agent
                && !session
                    .end
                    .as_ref()
                    .is_some_and(|end| end.reason.is_agent_failure())
        })
        .collect();
    sessions.sort_by_key(|session| session.since);
    let mut at_work: Vec<ModelRef> = Vec::new();
    for session in sessions {
        if !at_work.contains(&session.started.worker) {
            at_work.push(session.started.worker.clone());
        }
    }
    at_work
}

/// Waits in a row before the run stops trying on its own.
const RETRIES: u32 = 5;

/// How long a run whose jobs nobody could take waits before it tries again on its own, after
/// `tries` such waits in a row: the breaker's cool-down, doubled each time. `None` once it has
/// tried [`RETRIES`] times, when only `/resume` tries again.
pub(crate) fn retry_after(settings: &Settings, tries: u32) -> Option<Duration> {
    if tries >= RETRIES {
        return None;
    }
    // A minute at least, so a cool-down set to nothing never wakes the run at once.
    let cooldown = settings.health.cooldown.parse().ok().flatten();
    let base = cooldown
        .unwrap_or(Duration::from_secs(300))
        .max(Duration::from_secs(60));
    Some(base * 2u32.pow(tries))
}

/// A retry's wait, cut short when the run halts.
async fn wait(host: &Host, wait: Duration) -> EngineResult<()> {
    if wait.is_zero() {
        return Ok(());
    }
    let halt = host.shared().halt.clone();
    tokio::select! {
        () = tokio::time::sleep(wait) => Ok(()),
        () = halt.cancelled() => Err(EngineError::Interrupted),
    }
}

/// Why it ended, in a few plain words.
pub(crate) fn in_words(reason: &EndReason) -> String {
    match reason {
        EndReason::Auth { .. } => "a sign-in error".to_owned(),
        EndReason::RateLimited => "rate limited".to_owned(),
        EndReason::UsageLimit { .. } => "its usage limit".to_owned(),
        EndReason::Billing { .. } => "out of credits".to_owned(),
        EndReason::ModelUnavailable { model } => format!(
            "{} isn't available",
            argonauts_core::model_catalog::display_name_from_id(model)
        ),
        EndReason::CliError { category, .. } if *category == ErrorCategory::Auth => {
            "a sign-in error".to_owned()
        }
        EndReason::CliError { .. } => "an agent error".to_owned(),
        EndReason::Stuck { watchdog, calls } => {
            format!("it {}", stuck_words(*watchdog, calls.as_ref()))
        }
        EndReason::AdapterBroken { .. } => "its output changed".to_owned(),
        EndReason::IsolationBroken { plugins, skills } => format!(
            "it loaded {}, which a blind role mustn't see",
            loaded_words(plugins, skills)
        ),
        EndReason::StartFailed { .. } => "it couldn't start".to_owned(),
        _ => "it crashed".to_owned(),
    }
}

/// The job as the transcript says it: `the main agent`, `a reviewer`, `the conflict's resolver`.
/// The job as a fallback's line names it: `reviewer` rather than `a reviewer`.
fn job_as_role(job: &Job) -> String {
    let words = job_in_words(job);
    match words.strip_prefix("a ") {
        Some(role) => role.to_owned(),
        None => words,
    }
}

fn job_in_words(job: &Job) -> String {
    let words = match (job.role, job.step) {
        (RoleName::Worker, Some(StepName::ResolveConflict)) => "the conflict's resolver",
        (_, Some(StepName::ExperimentReport)) => "an experiment",
        (RoleName::Judge, Some(StepName::Settle | StepName::ModelJudge)) => "the judge",
        (RoleName::TieBreaker, _) => "the main agent",
        (RoleName::Orchestrator, _) => "the orchestrator",
        (RoleName::Reader, _) => "a reader",
        (RoleName::Comparer, _) => "the comparer",
        (RoleName::Draft, _) => "a drafting agent",
        (RoleName::Map, _) => "the mapping agent",
        (RoleName::Critic, _) => "a critic",
        (RoleName::Experiment, _) => "an experiment",
        (RoleName::Judge, _) => "a judge",
        (RoleName::Worker, _) => "the task's worker",
        (RoleName::Consultant, _) => "the consultant",
        (RoleName::Reviewer, _) => "a reviewer",
        (RoleName::RunReview, _) => "the run review",

        (RoleName::Narrator, _) => "the narrator",
        (RoleName::Main, _) => "the agent",
    };
    words.to_owned()
}

pub(crate) fn note(host: &Host, task: Option<&TaskId>, text: String) -> EngineResult<()> {
    let level = NoteLevel::Warn;
    host.record(task, None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::run::{RunLifecycle, WaitReason};
    use argonauts_core::events::session::SessionEvent;
    use argonauts_core::events::{RunEvent, RunEventKind};

    use super::*;
    use crate::state::RunState;

    fn health(state: HealthState, since: OffsetDateTime, reason: &str) -> AgentHealth {
        AgentHealth {
            agent_id: AgentId("codex".to_owned()),
            state,
            since,
            reason: Some(reason.to_owned()),
            next_test_at: None,
            disabled: None,
        }
    }

    #[test]
    fn a_crash_memory_holds_until_a_resume_or_a_passing_test_task() {
        let at = OffsetDateTime::UNIX_EPOCH + time::Duration::hours(1);
        let failing = Failing {
            words: "it crashed".to_owned(),
            after: 0,
            at,
        };
        let before = at - time::Duration::minutes(5);
        let after = at + time::Duration::minutes(5);
        let passed = TEST_TASK_SUCCEEDED;
        let quiet = "the errors stopped";
        assert!(failing.holds(0, &health(HealthState::Healthy, before, passed)));
        assert!(!failing.holds(7, &health(HealthState::Healthy, before, passed)));
        assert!(!failing.holds(0, &health(HealthState::Healthy, after, passed)));
        assert!(failing.holds(0, &health(HealthState::Healthy, after, quiet)));
        assert!(failing.holds(0, &health(HealthState::Testing, after, passed)));
        assert!(failing.holds(0, &health(HealthState::Down, after, passed)));
    }

    /// A CLI that couldn't start is remembered for every model of it, where a crash is one
    /// model's own.
    #[test]
    fn a_cli_that_couldnt_start_is_remembered_for_all_its_models() {
        let model = |text: &str| text.parse::<ModelRef>().unwrap();
        let failing = |words: &str| Failing {
            words: words.to_owned(),
            after: 0,
            at: OffsetDateTime::UNIX_EPOCH + time::Duration::hours(1),
        };
        let mut memory = Memory::default();
        memory.wont_start.insert(
            model("claude-code").agent_id(),
            failing("it couldn't start"),
        );
        memory
            .keeps_failing
            .insert(model("codex/gpt-5.6"), failing("it crashed twice in a row"));

        let words = |text: &str| memory.failing(&model(text)).map(|failing| &*failing.words);
        assert_eq!(words("claude-code/opus"), Some("it couldn't start"));
        assert_eq!(words("claude-code/haiku"), Some("it couldn't start"));
        assert_eq!(words("codex/gpt-5.6"), Some("it crashed twice in a row"));
        assert_eq!(words("codex/gpt-5.6-luna"), None);
        assert_eq!(words("antigravity-cli"), None);
    }

    #[test]
    fn a_retrys_wait_reads_as_you_wrote_it_until_it_grows() {
        use argonauts_core::settings::Span;
        use argonauts_core::settings::fallback::Backoff;
        let retry = Retry {
            tries: 4,
            wait: Span::new("5s"),
            backoff: Backoff::Double,
        };
        let said = |n: u32| wait_in_words(&retry, retry.wait_before(n));
        assert_eq!(
            [said(1), said(2), said(3), said(5)],
            ["5s", "10s", "20s", "1m 20s"]
        );
        let written = Retry {
            wait: Span::new("1500ms"),
            ..Retry::default()
        };
        assert_eq!(wait_in_words(&written, written.wait_before(3)), "1500ms");
        let minutes = Retry {
            wait: Span::new("30s"),
            backoff: Backoff::Double,
            ..Retry::default()
        };
        assert_eq!(wait_in_words(&minutes, minutes.wait_before(3)), "2m");
    }

    /// Each wait in a row waits twice as long before it tries again, and after five it leaves
    /// the next try to you.
    #[test]
    fn the_retries_wait_longer_each_time_then_stop() {
        let settings = Settings::default();
        let minutes = |tries| retry_after(&settings, tries).map(|wait| wait.as_secs() / 60);
        assert_eq!(
            (0..6).map(minutes).collect::<Vec<_>>(),
            [Some(5), Some(10), Some(20), Some(40), Some(80), None]
        );
    }

    /// An overload reads as its model being overloaded, any other outage as the agent being
    /// unreachable, and any other failure that may pass as what it was, on the retry line and
    /// on the line after it.
    #[test]
    fn a_passing_failure_is_named_for_what_it_was() {
        let error = |category| EndReason::CliError {
            category,
            message: String::new(),
        };
        assert_eq!(
            passing_words(&error(ErrorCategory::Overloaded)),
            ("is overloaded", "Still overloaded")
        );
        for outage in [
            ErrorCategory::Server,
            ErrorCategory::Network,
            ErrorCategory::Timeout,
        ] {
            assert_eq!(
                passing_words(&error(outage)),
                ("couldn't be reached", "Still unreachable")
            );
        }
        let rate = ("is rate limited", "Still rate limited");
        assert_eq!(passing_words(&EndReason::RateLimited), rate);
        assert_eq!(passing_words(&error(ErrorCategory::RateLimit)), rate);
        assert_eq!(
            passing_words(&error(ErrorCategory::Unknown)),
            ("gave an error", "Still failing")
        );
        let crash = EndReason::Crashed {
            message: String::new(),
        };
        assert_eq!(passing_words(&crash), ("crashed", "Crashed again"));
    }

    /// Only an agent that went again and stopped again for something that may pass is still
    /// out: a first stop, or a second for a spent plan, says why in full.
    #[test]
    fn only_a_retry_that_ended_the_same_way_is_still_out() {
        let (codex, claude): (ModelRef, ModelRef) = (
            "codex/gpt-5.6".parse().expect("a model"),
            "claude-code/opus".parse().expect("a model"),
        );
        let spent = EndReason::UsageLimit { resets_at: None };
        assert!(went_again(
            &[&codex, &codex],
            &codex,
            &EndReason::RateLimited
        ));
        assert!(!went_again(&[&codex], &codex, &EndReason::RateLimited));
        assert!(!went_again(
            &[&claude, &codex],
            &codex,
            &EndReason::RateLimited
        ));
        assert!(!went_again(&[&codex, &codex], &codex, &spent));
    }

    /// Waits where a job had nobody to fall back to count up, and an agent that finishes a
    /// session starts the count over.
    #[test]
    fn a_finished_session_starts_the_retry_count_over() {
        let event = |seq, kind| RunEvent {
            seq,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: argonauts_core::RunId::new("run-01").expect("a run id"),
            task_id: None,
            session: Some(argonauts_core::names::SessionKey::numbered(1)),
            source: "engine".to_owned(),
            kind,
        };
        let waiting = |seq| {
            event(
                seq,
                RunEventKind::Run(RunLifecycle::Waiting {
                    reason: WaitReason::NoFallback {
                        detail: "nobody to fall back to as the main agent".to_owned(),
                        until: None,
                        stop: None,
                    },
                    waiting_on: Vec::new(),
                }),
            )
        };
        let mut state = RunState::default();
        state.apply(&waiting(1));
        state.apply(&waiting(2));
        assert_eq!(state.run.fallback_waits, 2);
        let mut end = crate::engine::host::crashed_end("");
        end.reason = EndReason::Completed;
        state.apply(&event(
            3,
            RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
        ));
        assert_eq!(state.run.fallback_waits, 0);
    }

    /// The work a review or a ruling waits to judge is named by whoever built it, each once.
    #[test]
    fn the_work_is_named_by_its_builders() {
        let model = |text: &str| -> ModelRef { text.parse().expect("a model") };
        let (haiku, sonnet) = (model("claude-code/haiku"), model("claude-code/sonnet"));
        let short = |model: &ModelRef| roles::short_name(model);
        assert_eq!(
            work_of(std::slice::from_ref(&haiku)),
            format!("{}'s work", short(&haiku))
        );
        assert_eq!(
            work_of(&[haiku.clone(), sonnet.clone(), haiku.clone()]),
            format!("the work of {} and {}", short(&haiku), short(&sonnet))
        );
        assert_eq!(verb_of(RoleName::Judge), "judge");
        assert_eq!(verb_of(RoleName::Reviewer), "review");
    }

    /// A fallback's line names the role, as in `fell back to Claude Code as reviewer`,
    /// keeping `the` where the job is the only one of its kind.
    #[test]
    fn a_fallbacks_line_names_the_role() {
        let job = |role| Job {
            at: None,
            role,
            step: None,
            task: None,
            independent_of: Vec::new(),
            isolated: false,
        };
        assert_eq!(job_as_role(&job(RoleName::Reviewer)), "reviewer");
        assert_eq!(job_as_role(&job(RoleName::TieBreaker)), "the main agent");
        assert_eq!(job_as_role(&job(RoleName::Experiment)), "an experiment");
    }

    /// The line a workflow's view shows under its header for a fallback: who stopped, as the
    /// retry line said it when the failure might have passed, then who it fell back to, and
    /// that `same_cli` asked for it where the setting kept another CLI out.
    #[test]
    fn a_workflows_line_says_who_stopped_and_who_it_fell_back_to() {
        let (opus, sonnet) = ("Opus 5.5 on Claude Code", "Sonnet 5.5 on Claude Code");
        let ended = |category| EndReason::CliError {
            category,
            message: "it failed".to_owned(),
        };
        let overloaded = Why::EndedAgain(ended(ErrorCategory::Overloaded));
        assert_eq!(
            in_the_view(opus, sonnet, Some(&overloaded), true),
            "Opus 5.5 on Claude Code is overloaded · fell back to Sonnet 5.5 on Claude Code, as \
             same_cli asks"
        );
        assert_eq!(
            in_the_view(opus, "GPT 5.6 on Codex", Some(&overloaded), false),
            "Opus 5.5 on Claude Code is overloaded · fell back to GPT 5.6 on Codex"
        );
        // No retry was said for one that comes back the same, or for a model already out.
        let refused = Why::Ended(ended(ErrorCategory::InvalidRequest));
        assert_eq!(
            in_the_view(opus, sonnet, Some(&refused), false),
            "Opus 5.5 on Claude Code's session ended early (an agent error) · fell back to \
             Sonnet 5.5 on Claude Code"
        );
        assert_eq!(
            in_the_view(opus, sonnet, Some(&Why::ModelOut("it's overloaded")), true),
            "Opus 5.5 on Claude Code is out (it's overloaded) · fell back to Sonnet 5.5 on \
             Claude Code, as same_cli asks"
        );
    }

    /// A task's fallback reads as a step's does, from how the stopped worker's session ended.
    /// One that ended for no failure of its agent gives no reason.
    #[test]
    fn a_tasks_fallback_reads_as_a_steps_does() {
        let opus: ModelRef = "claude-code/claude-opus-5-5".parse().expect("a model");
        let sonnet: ModelRef = "claude-code/claude-sonnet-5-5".parse().expect("a model");
        let codex: ModelRef = "codex/gpt-5.6".parse().expect("a model");
        let (from, on_its_cli, elsewhere) = (
            display_name(&opus),
            display_name(&sonnet),
            display_name(&codex),
        );
        let overloaded = EndReason::CliError {
            category: ErrorCategory::Overloaded,
            message: "Overloaded".to_owned(),
        };
        assert_eq!(
            task_in_the_view(&opus, &sonnet, Some(&overloaded), true),
            format!("{from} is overloaded · fell back to {on_its_cli}, as same_cli asks")
        );
        let limit = EndReason::UsageLimit { resets_at: None };
        assert_eq!(
            task_in_the_view(&opus, &codex, Some(&limit), false),
            format!("{from}'s session ended early (its usage limit) · fell back to {elsewhere}")
        );
        for no_failure in [None, Some(&EndReason::Stopped), Some(&EndReason::Completed)] {
            assert_eq!(
                task_in_the_view(&opus, &codex, no_failure, false),
                format!("{from} stopped · fell back to {elsewhere}")
            );
        }
    }

    /// The local Qwen run's note said its stuck session "stopped responding" and that the task
    /// "fell back to OpenCode", which ran the same Qwen. It says what the watchdog saw and
    /// names the model, and that it's the same one.
    #[test]
    fn a_fallback_to_the_same_model_says_so() {
        use argonauts_core::events::session::{CallCount, Watchdog};

        let qwen: ModelRef = "opencode/ollama/qwen3-coder:30b-64k"
            .parse()
            .expect("a model");
        let name = display_name(&qwen);
        let repeated = EndReason::Stuck {
            watchdog: Watchdog::Repeat,
            calls: Some(CallCount {
                name: "execute".into(),
                times: 12,
            }),
        };
        assert_eq!(
            task_in_the_view(&qwen, &qwen, Some(&repeated), false),
            format!(
                "{name}'s session ended early (it repeated the same execute call 12 times) · \
                 fell back to {name} (the same model)"
            )
        );
        let bare: ModelRef = "opencode".parse().expect("a CLI");
        assert_eq!(
            task_in_the_view(&qwen, &bare, Some(&repeated), false),
            format!(
                "{name}'s session ended early (it repeated the same execute call 12 times) · \
                 fell back to OpenCode"
            ),
            "a CLI that names no model may run another"
        );
    }

    /// A token revoked on the server reads as signed out from the error's own words, which the
    /// CLI's local check can't see; a plain 401 still asks the CLI.
    #[test]
    fn a_revoked_sign_in_is_signed_out() {
        let auth = |message: &str| EndReason::Auth {
            message: message.to_owned(),
        };
        assert!(says_signed_out(&auth(
            "Failed to authenticate: OAuth token revoked. Please log in again or contact your \
             administrator."
        )));
        assert!(says_signed_out(&auth("Your session expired")));
        assert!(!says_signed_out(&auth("401 Unauthorized")));
        assert!(!says_signed_out(&EndReason::CliError {
            category: argonauts_core::ErrorCategory::Network,
            message: "token expired while offline".into(),
        }));
    }
}
