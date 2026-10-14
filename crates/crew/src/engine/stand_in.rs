//! Who carries on when a session ends for a vendor reason. A vendor error never
//! ends a run, in any mode: the same agent goes again once after a short wait when the failure
//! may pass, with a line that says so, then its seat goes to the next agent on its handoff list,
//! and only when nobody can take it does the run park, saying why and what fixes it. An agent whose CLI says it's logged
//! out, or whose vendor is down or out of quota, is out: its seats go straight to a stand-in
//! until its CLI says it's logged in again or its vendor is back. Each handover is said once.
//! A blind role's session keeps your plugins out, so only an agent whose CLI can do the same
//! takes its seat, and a CLI that loaded them anyway is out of that seat, every model of it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use argonauts_core::CardId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::{EndReason, loaded_words};
use argonauts_core::limits::{HealthState, TEST_TASK_SUCCEEDED, VendorHealth};
use argonauts_core::log::NO_SECOND_TRY;
use argonauts_core::names::{RoleName, VendorId, WorkerRef};
use argonauts_core::registry::company_of;
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::{FallbackEntry, HandoffMode};
use argonauts_core::steps::StepName;
use argonauts_core::worker::WorkerKind;
use argonauts_core::{ErrorCategory, Note, NoteLevel};
use time::OffsetDateTime;

use crate::engine::host::{Host, round_window};
use crate::engine::shared::lock;
use crate::error::{EngineError, EngineResult, Stall};
use crate::handoff::rules;
use crate::roles::{self, display_name};
use crate::state::RunState;
use crate::state::sessions::SessionRecord;
use crate::workflow::StepAt;

/// Where a session sits, which a stand-in must fit: its step and round, its role, its card, and
/// whose work it judges.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Seat {
    pub at: Option<StepAt>,
    pub role: RoleName,
    pub step: Option<StepName>,
    pub card: Option<CardId>,
    /// Agents a stand-in may not share a company with: the owner of the card a reviewer
    /// reviews, or the authors of the plans a judge scores.
    pub independent_of: Vec<WorkerRef>,
    /// Its session keeps your plugins out, as a blind role's does, so a stand-in's CLI must
    /// keep them out too.
    pub isolated: bool,
}

impl Seat {
    /// The same seat in the same step and round, whoever is independent of whom.
    fn same_place(&self, other: &Self) -> bool {
        self.at == other.at
            && self.role == other.role
            && self.step == other.step
            && self.card == other.card
    }
}

/// What the engine remembers of the run's agents while it runs. The log keeps the rest: a
/// resumed engine asks again the CLI of an agent whose last session ended on a sign-in error,
/// and finds the handovers already made in its sessions.
#[derive(Debug, Default)]
pub(crate) struct Memory {
    /// What each agent's CLI said of its login when last asked.
    logins: BTreeMap<WorkerRef, Login>,
    /// Seats stand-ins took, and seats their own agents hold, so two seats of one step never
    /// go to the same agent.
    taken: Vec<Taken>,
    /// Handovers already said, from whom to whom, so a repeat says nothing new.
    said: BTreeSet<(WorkerRef, WorkerRef)>,
    /// Agents whose sessions crashed twice in a row. Until the run resumes or their vendor's
    /// health check passes, their later seats go straight to a stand-in rather than starting a
    /// CLI that fails every time.
    keeps_failing: BTreeMap<WorkerRef, Failing>,
}

/// How an agent's sessions keep ending, in words, the resume it happened after, and when.
#[derive(Debug, Clone)]
struct Failing {
    words: String,
    after: u64,
    at: OffsetDateTime,
}

impl Failing {
    /// Whether it still holds: the run hasn't resumed since, and the vendor's test task hasn't
    /// succeeded since. A passing test means whatever broke the CLI may have been fixed; errors
    /// that merely stopped coming back say nothing of it.
    fn holds(&self, resumed: u64, health: &VendorHealth) -> bool {
        let tested_since = health.state == HealthState::Healthy
            && health.since > self.at
            && health.reason.as_deref() == Some(TEST_TASK_SUCCEEDED);
        self.after == resumed && !tested_since
    }
}

/// A seat a stand-in took: where, whose it was, and who took it. A seat its own agent holds
/// names that agent as both.
#[derive(Debug, Clone)]
struct Taken {
    seat: Seat,
    asked: WorkerRef,
    by: WorkerRef,
}

/// One answer of a CLI's login check.
#[derive(Debug, Clone, Copy)]
struct Login {
    at: Instant,
    logged_out: bool,
}

impl Memory {
    fn logged_out(&self, worker: &WorkerRef) -> bool {
        self.logins
            .get(worker)
            .is_some_and(|login| login.logged_out)
    }
}

/// Why an agent left its seat.
enum Why {
    /// Its session ended for this reason.
    Ended(EndReason),
    /// Its session ended for this reason again, after the retry that was said.
    EndedAgain(EndReason),
    /// Its CLI says it's logged out, after its session ended for this reason when it had one.
    LoggedOut(Option<EndReason>),
    /// Its vendor is down or out of quota, in words.
    Unusable(&'static str),
    /// Its sessions crashed twice in a row earlier in the run, in words.
    KeepsFailing(String),
}

/// Before a session's first start: the agent that takes the seat at once because the asked one
/// is out, its CLI having said it's logged out or its vendor being down or out of quota. `None`
/// starts the asked one. `Err(Stalled)` when it's out and nobody else may take the seat.
pub(crate) async fn instead_of(
    host: &Host,
    asked: &WorkerRef,
    seat: &Seat,
) -> EngineResult<Option<WorkerRef>> {
    let why = if is_logged_out(host, asked).await {
        Why::LoggedOut(None)
    } else if let Some(words) = unusable(host, asked) {
        Why::Unusable(words)
    } else if let Some(words) = keeps_failing(host, asked) {
        // With nobody to stand in, the agent goes again rather than the run parking.
        let why = Why::KeepsFailing(words);
        return Ok(take_over(host, seat, asked, asked, &[asked], &why).ok());
    } else {
        return Ok(None);
    };
    take_over(host, seat, asked, asked, &[asked], &why).map(Some)
}

/// How `worker`'s sessions keep ending, when they crashed twice in a row since the run last
/// resumed and its vendor's health check last passed.
fn keeps_failing(host: &Host, worker: &WorkerRef) -> Option<String> {
    let since = host.read(|state| state.run.last_resumed).unwrap_or(0);
    let failing = lock(&host.shared().stand_ins)
        .keeps_failing
        .get(worker)
        .cloned()?;
    let health = host.services().health.state(&worker.vendor());
    failing.holds(since, &health).then_some(failing.words)
}

/// Whether the run already knows `worker` can't take a seat now, without asking its CLI: its
/// sessions crashed twice in a row, its CLI last said it's logged out, or its vendor is down or
/// out of quota.
pub(crate) fn known_out(host: &Host, worker: &WorkerRef) -> bool {
    let logged_out = lock(&host.shared().stand_ins).logged_out(worker);
    logged_out || keeps_failing(host, worker).is_some() || unusable(host, worker).is_some()
}

/// Whether `worker` is out before a session of its own starts: its CLI says it's logged out,
/// or its provider is down or its plan ran out.
pub(crate) async fn is_out(host: &Host, worker: &WorkerRef) -> bool {
    is_logged_out(host, worker).await || unusable(host, worker).is_some()
}

/// Notes that `holder` has `seat` as its own from the step's start, before a session of its own
/// shows it. A step that asks several agents at once holds each one's seat first, so none
/// takes another's beside its own and does the same work twice.
pub(crate) fn hold(host: &Host, seat: &Seat, holder: &WorkerRef) {
    let mut memory = lock(&host.shared().stand_ins);
    let held = memory
        .taken
        .iter()
        .any(|taken| taken.by == *holder && taken.asked == *holder && taken.seat.same_place(seat));
    if !held {
        memory.taken.push(Taken {
            seat: seat.clone(),
            asked: holder.clone(),
            by: holder.clone(),
        });
    }
}

/// Up to `count` agents that take `seat`, `asked`'s, at once, beside those of `with` who have
/// it already (`fallback.<role>.together`). Each is remembered in the seat, so no other seat
/// of the step takes it. Fewer than `count` when fewer may take it, and none when the role's
/// fallbacks wait.
pub(crate) fn together(
    host: &Host,
    seat: &Seat,
    asked: &WorkerRef,
    with: &[&WorkerRef],
    count: usize,
) -> Vec<WorkerRef> {
    let applied = rules::applied(host, seat.role, &asked.vendor());
    if applied.mode == HandoffMode::Wait {
        return Vec::new();
    }
    let mut memory = lock(&host.shared().stand_ins);
    let listed = &applied.candidates;
    let takers: Vec<WorkerRef> = candidates(host, seat, asked, listed, &[asked], None, &memory)
        .into_iter()
        .filter(|taker| !with.contains(&taker))
        .take(count)
        .collect();
    for taker in &takers {
        memory.taken.push(Taken {
            seat: seat.clone(),
            asked: asked.clone(),
            by: taker.clone(),
        });
    }
    takers
}

/// The next agent for `seat` once `from` ended for `reason`, after every one of `failed` did too,
/// for a session the caller starts itself. `Err(Stalled)` when nobody may take it.
pub(crate) async fn after(
    host: &Host,
    seat: &Seat,
    from: &WorkerRef,
    failed: &[&WorkerRef],
    reason: &EndReason,
) -> EngineResult<WorkerRef> {
    let why = if is_logged_out(host, from).await {
        Why::LoggedOut(Some(reason.clone()))
    } else {
        Why::Ended(reason.clone())
    };
    take_over(host, seat, from, from, failed, &why)
}

/// The worker the session of `record` starts again with, after it ended for a vendor reason:
/// the same one once more, else the next that may take its seat. After a resume the asked agent
/// starts over, unless it's out. `Err(Stalled)` when nobody can, with what to say.
pub(crate) async fn next(
    host: &Host,
    record: &SessionRecord,
    seat: &Seat,
) -> EngineResult<WorkerRef> {
    if failures(host, record).is_empty() {
        let asked = record.asked_worker();
        return Ok(instead_of(host, asked, seat)
            .await?
            .unwrap_or_else(|| asked.clone()));
    }
    if let Some(again) = again(host, record).await? {
        return Ok(again);
    }
    let failed = failures(host, record);
    let current = &record.started.worker;
    let reason = end_reason(record);
    let why = if is_logged_out(host, current).await {
        Why::LoggedOut(Some(reason))
    } else if went_again(&failed, current, &reason) {
        // A CLI that crashed twice in a row crashes the same way on its next seat, where a busy
        // service may well have room by then.
        if matches!(reason, EndReason::Crashed { .. }) {
            let since = host.read(|state| state.run.last_resumed).unwrap_or(0);
            let failing = Failing {
                words: in_words(&reason),
                after: since,
                at: host.now(),
            };
            lock(&host.shared().stand_ins)
                .keeps_failing
                .insert(current.clone(), failing);
        }
        Why::EndedAgain(reason)
    } else {
        Why::Ended(reason)
    };
    take_over(host, seat, record.asked_worker(), current, &failed, &why)
}

/// The same agent again on the session of `record`, after `supervisor.retry_wait`, when its
/// failure may pass, it had fewer than two tries since the run last resumed, and it hasn't
/// crashed twice in a row on an earlier seat. A 401 its CLI's
/// own login check contradicts was passing. One line says it goes again, before the wait. After
/// a resume, the asked agent starts over, since whatever stopped it may have been fixed
/// meanwhile. `None` when it may not go again.
pub(crate) async fn again(host: &Host, record: &SessionRecord) -> EngineResult<Option<WorkerRef>> {
    let failed = failures(host, record);
    if failed.is_empty() {
        return Ok(Some(record.asked_worker().clone()));
    }
    let current = &record.started.worker;
    if keeps_failing(host, current).is_some() {
        return Ok(None);
    }
    let reason = end_reason(record);
    let logged_in = login_check(host, current, &reason).await;
    let tries = failed.iter().filter(|worker| **worker == current).count();
    if tries < 2 && reason.may_pass() && logged_in != Some(false) {
        say_retry(host, record, &reason)?;
        wait(host).await?;
        return Ok(Some(current.clone()));
    }
    Ok(None)
}

/// What the line that says who takes over opens with, such as `Still unreachable`, when the
/// session of `record` went again after a failure that might pass and ended the same way: the
/// retry line came before it.
pub(crate) fn still_out(host: &Host, record: &SessionRecord) -> Option<&'static str> {
    let failed = failures(host, record);
    let reason = end_reason(record);
    went_again(&failed, &record.started.worker, &reason).then(|| passing_words(&reason).1)
}

/// Whether `current` was tried again on its session and stopped again for a `reason` that
/// might pass.
fn went_again(failed: &[&WorkerRef], current: &WorkerRef, reason: &EndReason) -> bool {
    reason.may_pass() && failed.iter().filter(|worker| **worker == current).count() >= 2
}

/// `Opus 5.5 on Claude Code couldn't be reached · trying again in 5s`.
fn say_retry(host: &Host, record: &SessionRecord, reason: &EndReason) -> EngineResult<()> {
    let name = display_name(&record.started.worker);
    let (stopped, _) = passing_words(reason);
    let wait = &host.settings().supervisor.retry_wait;
    let again = match wait.parse() {
        Ok(Some(span)) if !span.is_zero() => format!("trying again in {}", wait.0.trim()),
        _ => "trying again".to_owned(),
    };
    note(
        host,
        record.card.as_ref(),
        format!("{name} {stopped} · {again}"),
    )
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
        EndReason::VendorError { category, .. } => match category {
            ErrorCategory::Auth => SIGN_IN,
            ErrorCategory::RateLimit => RATE,
            ErrorCategory::Overloaded
            | ErrorCategory::Server
            | ErrorCategory::Network
            | ErrorCategory::Timeout => ("couldn't be reached", "Still unreachable"),
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
    worker: &WorkerRef,
    reason: &EndReason,
) -> Option<bool> {
    if !reason.is_sign_in_error() {
        return None;
    }
    let logged_in = logged_in_now(host, worker).await;
    let login = Login {
        at: Instant::now(),
        logged_out: logged_in == Some(false),
    };
    lock(&host.shared().stand_ins)
        .logins
        .insert(worker.clone(), login);
    logged_in
}

/// Whether `worker`'s CLI says it's logged out: as it said within the breaker's cool-down, else
/// asked again when it said so before, or when its last session in the run ended on a sign-in
/// error. An agent that logged in meanwhile is back.
async fn is_logged_out(host: &Host, worker: &WorkerRef) -> bool {
    let known = lock(&host.shared().stand_ins).logins.get(worker).copied();
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
    lock(&host.shared().stand_ins)
        .logins
        .insert(worker.clone(), login);
    logged_out
}

/// Whether `worker`'s newest session in the run ended on a sign-in error.
fn last_ended_on_sign_in(state: &RunState, worker: &WorkerRef) -> bool {
    state.sessions.sign_in_failed.get(worker) == Some(&true)
}

async fn logged_in_now(host: &Host, worker: &WorkerRef) -> Option<bool> {
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

/// Why `worker`'s vendor can't take work now, in words, or `None` when it can.
fn unusable(host: &Host, worker: &WorkerRef) -> Option<&'static str> {
    let vendor = worker.vendor();
    if rules::vendor_usable(host, &vendor) {
        return None;
    }
    Some(match host.services().health.state(&vendor).state {
        HealthState::Down => "its agent is down",
        HealthState::Testing => "its agent is being tested after an outage",
        HealthState::Healthy | HealthState::Degraded => "it's out of usage until its plan resets",
    })
}

/// Its vendor failures since the run last resumed, by the worker each time.
fn failures<'r>(host: &Host, record: &'r SessionRecord) -> Vec<&'r WorkerRef> {
    let since = host.read(|state| state.run.last_resumed).unwrap_or(0);
    record
        .vendor_failures
        .iter()
        .filter(|(_, seq)| *seq > since)
        .map(|(worker, _)| worker)
        .collect()
}

fn end_reason(record: &SessionRecord) -> EndReason {
    record.end.as_ref().map_or_else(
        || crate::engine::host::crashed_end("").reason,
        |end| end.reason.clone(),
    )
}

/// The first agent that may take `seat`, `asked`'s, over from `from`, claimed so no other seat
/// of the same step goes to it too, with the handover said once. `Err(Stalled)` when there's
/// none.
fn take_over(
    host: &Host,
    seat: &Seat,
    asked: &WorkerRef,
    from: &WorkerRef,
    failed: &[&WorkerRef],
    why: &Why,
) -> EngineResult<WorkerRef> {
    let applied = rules::applied(host, seat.role, &from.vendor());
    if applied.mode != HandoffMode::Wait {
        // A CLI that loaded your plugins into a blind role's session does so on any model.
        let leaked =
            matches!(why, Why::Ended(EndReason::IsolationBroken { .. })).then(|| from.vendor());
        let taker = {
            let mut memory = lock(&host.shared().stand_ins);
            let listed = &applied.candidates;
            let taker = candidates(host, seat, asked, listed, failed, leaked.as_ref(), &memory)
                .into_iter()
                .next();
            if let Some(taker) = &taker {
                memory.taken.push(Taken {
                    seat: seat.clone(),
                    asked: asked.clone(),
                    by: taker.clone(),
                });
            }
            taker
        };
        if let Some(taker) = taker {
            say_handover(host, seat, from, &taker, why)?;
            return Ok(taker);
        }
    }
    Err(stalled(host, seat, from, failed, why, applied.mode))
}

/// Who may take `seat`, `asked`'s, now, best first: the role's handoff list when one names any,
/// else, in a crew run, the crew's other agents in crew order and then whatever else is
/// installed. A solo run works like the CLI you chose, so only a list you set brings in another.
/// Left out: an agent that failed here, one of a vendor that failed here, one that's out, one in
/// another agent's seat of the same step and round, one of the company of whoever it must be
/// independent of, one that may not judge the card's own work, one whose CLI can't keep your
/// plugins out of an isolated seat, and every one of the `leaked` vendor, whose CLI loaded them
/// anyway. Who already stood in for `asked` here may again, as a lead's later asks in one step
/// are the same seat.
fn candidates(
    host: &Host,
    seat: &Seat,
    asked: &WorkerRef,
    listed: &[FallbackEntry],
    failed: &[&WorkerRef],
    leaked: Option<&VendorId>,
    memory: &Memory,
) -> Vec<WorkerRef> {
    let services = host.services();
    let stopped: Vec<_> = failed.iter().map(|worker| worker.vendor()).collect();
    let pool: Vec<WorkerRef> = if listed.is_empty() && host.shared().mode == RunMode::Solo {
        Vec::new()
    } else if listed.is_empty() {
        let available = services.workers.available();
        let mut pool: Vec<WorkerRef> = host
            .settings()
            .crew
            .members()
            .into_iter()
            .filter(|worker| available.contains(worker))
            .collect();
        // The rest of what's installed after the crew; a repeat is dropped below.
        pool.extend(available);
        pool.retain(|worker| {
            !stopped.contains(&worker.vendor())
                && services
                    .workers
                    .get(worker)
                    .is_ok_and(|agent| agent.kind() == WorkerKind::Harness)
        });
        pool
    } else {
        listed.iter().map(|entry| entry.worker().clone()).collect()
    };
    let (owner, seated) = host.read(|state| {
        let owner = seat
            .card
            .as_ref()
            .and_then(|card| state.cards.get(card).and_then(rules::owner));
        (owner, seated(state, seat, asked))
    });
    let family = |worker: &WorkerRef| services.registry.family(worker);
    let resumed = host.read(|state| state.run.last_resumed).unwrap_or(0);
    // An agent whose sessions crashed twice in a row would only crash in this seat too.
    let crashing = |worker: &WorkerRef| {
        memory
            .keeps_failing
            .get(worker)
            .is_some_and(|failing| failing.holds(resumed, &services.health.state(&worker.vendor())))
    };
    let companies: Vec<String> = seat
        .independent_of
        .iter()
        .map(|worker| company_of(&family(worker)))
        .collect();
    let mut kept = Vec::new();
    for worker in pool {
        let elsewhere = seated.contains(&worker)
            || memory.taken.iter().any(|taken| {
                taken.by == worker && taken.asked != *asked && taken.seat.same_place(seat)
            });
        let usable = !failed.contains(&&worker)
            && !kept.contains(&worker)
            && !elsewhere
            && !memory.logged_out(&worker)
            && !crashing(&worker)
            && !companies.contains(&company_of(&family(&worker)))
            && services.workers.get(&worker).is_ok()
            && rules::vendor_usable(host, &worker.vendor())
            && roles::conflict(seat.role, &worker, owner.as_ref(), &family).is_none()
            && leaked != Some(&worker.vendor())
            && (!seat.isolated || services.workers.isolates(&worker));
        if usable {
            kept.push(worker);
        }
    }
    kept
}

/// The agents in the other seats of `seat`'s step and round, those not asked of `asked`: whoever
/// each was asked of, and whoever runs it now.
fn seated(state: &RunState, seat: &Seat, asked: &WorkerRef) -> Vec<WorkerRef> {
    let (from, until) = round_window(state, seat.at.as_ref());
    let step = seat.at.as_ref().map(|at| &at.step);
    let mut seated = Vec::new();
    for session in state.sessions.by_key.values() {
        let here = session.since >= from
            && until.is_none_or(|until| session.since < until)
            && session.started.workflow_step.as_ref() == step
            && session.started.role == seat.role
            && session.started.step == seat.step
            && session.card == seat.card
            && session.asked_worker() != asked;
        if here {
            seated.push(session.asked_worker().clone());
            seated.push(session.started.worker.clone());
        }
    }
    seated
}

/// Says who took the seat over, once for each pair of agents: a later handover between the same
/// two says nothing new. An agent whose CLI says it's logged out gets the login fix with it.
fn say_handover(
    host: &Host,
    seat: &Seat,
    from: &WorkerRef,
    taker: &WorkerRef,
    why: &Why,
) -> EngineResult<()> {
    let new = lock(&host.shared().stand_ins)
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
        Why::Unusable(words) => {
            format!("{from_name} is out ({words}), so {taker_name} takes its roles meanwhile.")
        }
        // The failure's own line said what went wrong, and named the model that failed, so
        // this one names only the CLI that steps in.
        Why::Ended(reason) if repeats(reason) => format!(
            "{} takes over as {} · {NO_SECOND_TRY}",
            DisplayNames::default().harness(taker.worker.as_str()),
            seat_as_role(seat),
        ),
        Why::Ended(reason) => format!(
            "{from_name}'s session as {} ended early ({}), so {taker_name} takes it over.",
            seat_in_words(seat),
            in_words(reason),
        ),
        // The retry line said who stopped and why.
        Why::EndedAgain(reason) => format!(
            "{} · {taker_name} takes over as {}",
            passing_words(reason).1,
            seat_in_words(seat)
        ),
        Why::KeepsFailing(words) => format!(
            "{from_name} isn't started again this run, since {words} twice in a row; \
             {taker_name} takes its roles."
        ),
    };
    note(host, seat.card.as_ref(), text)
}

/// An error the agent's service gave that comes back the same on a retry, such as an answer
/// format it refuses, so its seat goes to the next agent at once.
fn repeats(reason: &EndReason) -> bool {
    matches!(
        reason,
        EndReason::VendorError { .. } | EndReason::Auth { .. } | EndReason::StartFailed { .. }
    ) && !reason.may_pass()
}

/// The stall when nobody may take `seat`: who stopped and why, whose CLI says it's logged out,
/// and when a usage limit resets.
fn stalled(
    host: &Host,
    seat: &Seat,
    from: &WorkerRef,
    failed: &[&WorkerRef],
    why: &Why,
    mode: HandoffMode,
) -> EngineError {
    let memory = lock(&host.shared().stand_ins);
    let mut logged_out: Vec<String> = Vec::new();
    for worker in failed.iter().copied().chain([from]) {
        let name = display_name(worker);
        if memory.logged_out(worker) && !logged_out.contains(&name) {
            logged_out.push(name);
        }
    }
    drop(memory);
    let role = seat_in_words(seat);
    let from_name = display_name(from);
    let stopped = match why {
        Why::Ended(reason) | Why::EndedAgain(reason) | Why::LoggedOut(Some(reason)) => {
            format!("{from_name}'s session ended early ({})", in_words(reason))
        }
        Why::LoggedOut(None) => format!("{from_name} is logged out"),
        Why::Unusable(words) => format!("{from_name} is out ({words})"),
        Why::KeepsFailing(words) => format!("{from_name}'s sessions keep ending ({words})"),
    };
    let (detail, line) = if mode == HandoffMode::Wait {
        (
            format!("{role} waits for {from_name}"),
            format!(
                "{} waits for {from_name}: {stopped}, and your fallback settings say to wait \
                 for it.",
                argonauts_core::text::sentence_case(&role)
            ),
        )
    } else {
        let other = if seat.isolated {
            "no other agent that keeps your plugins out"
        } else {
            "no other agent"
        };
        (
            format!("nobody could take over as {role}"),
            format!("Nobody could take over as {role}: {stopped}, and {other} can take the role."),
        )
    };
    let resets_at = match why {
        Why::Ended(EndReason::UsageLimit { resets_at }) => *resets_at,
        Why::Unusable(_) => host
            .services()
            .quota
            .get(&from.vendor())
            .and_then(|reading| reading.resets_at),
        _ => None,
    };
    EngineError::Stalled(Box::new(Stall {
        detail,
        line,
        logged_out,
        resets_at,
    }))
}

/// Parks in a row before the run stops trying on its own.
const RETRIES: u32 = 5;

/// How long a run nobody could take a seat of waits before it tries again on its own, after
/// `tries` such parks in a row: the breaker's cool-down, doubled each time. `None` once it has
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

/// The line the run parks with: why nobody could take the seat, and what happens next. It
/// promises to carry on by itself only when a retry is set.
pub(crate) fn park_line(stall: &Stall, retry: Option<Duration>) -> String {
    let when = match (stall.resets_at, retry) {
        (Some(_), _) => Some("once its plan resets".to_owned()),
        (None, Some(retry)) => Some(format!("in {}", span_in_words(retry))),
        (None, None) => None,
    };
    let next = match (stall.logged_out.is_empty(), when) {
        (false, Some(when)) => format!(
            "{} It also tries again on its own {when}.",
            crate::error::login_fix(&stall.logged_out)
        ),
        (false, None) => crate::error::login_fix(&stall.logged_out),
        (true, Some(when)) => format!("It tries again on its own {when}; /resume tries now."),
        (true, None) => "/resume tries again.".to_owned(),
    };
    format!("{} {next}", stall.line)
}

/// `a minute`, `20 minutes`, `1 hour 20 minutes`.
fn span_in_words(span: Duration) -> String {
    let minutes = span.as_secs().div_ceil(60).max(1);
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let unit = |n: u64, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    match (hours, minutes) {
        (0, 1) => "a minute".to_owned(),
        (0, minutes) => unit(minutes, "minute", "minutes"),
        (hours, 0) => unit(hours, "hour", "hours"),
        (hours, minutes) => format!(
            "{} {}",
            unit(hours, "hour", "hours"),
            unit(minutes, "minute", "minutes")
        ),
    }
}

/// `supervisor.retry_wait`, cut short when the run halts.
async fn wait(host: &Host) -> EngineResult<()> {
    let wait = host
        .settings()
        .supervisor
        .retry_wait
        .parse()
        .ok()
        .flatten()
        .unwrap_or_default();
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
        EndReason::VendorError { category, .. } if *category == ErrorCategory::Auth => {
            "a sign-in error".to_owned()
        }
        EndReason::VendorError { .. } => "an agent error".to_owned(),
        EndReason::Stuck { .. } => "it stopped responding".to_owned(),
        EndReason::AdapterBroken { .. } => "its output changed".to_owned(),
        EndReason::IsolationBroken { plugins, skills } => format!(
            "it loaded {}, which a blind role mustn't see",
            loaded_words(plugins, skills)
        ),
        EndReason::StartFailed { .. } => "it couldn't start".to_owned(),
        _ => "it crashed".to_owned(),
    }
}

/// The seat as the transcript says it: `the lead`, `a reviewer`, `the conflict's resolver`.
/// The seat as what one takes over as: `reviewer` rather than `a reviewer`.
fn seat_as_role(seat: &Seat) -> String {
    let words = seat_in_words(seat);
    match words.strip_prefix("a ") {
        Some(role) => role.to_owned(),
        None => words,
    }
}

fn seat_in_words(seat: &Seat) -> String {
    let words = match (seat.role, seat.step) {
        (RoleName::Worker, Some(StepName::ResolveConflict)) => "the conflict's resolver",
        (_, Some(StepName::SpikeReport)) => "an experiment",
        (RoleName::PhaseLead, _) => "the main agent",
        (RoleName::Orchestrator, _) => "the orchestrator",
        (RoleName::Restater, _) => "a restater",
        (RoleName::Differ, _) => "the differ",
        (RoleName::Drafter, _) => "a drafter",
        (RoleName::Mapper, _) => "the mapper",
        (RoleName::Critic, _) => "a critic",
        (RoleName::Spike, _) => "an experiment",
        (RoleName::Judge, _) => "a judge",
        (RoleName::Worker, _) => "the task's worker",
        (RoleName::Consultant, _) => "the consultant",
        (RoleName::Reviewer, _) => "a reviewer",
        (RoleName::Adjudicator, _) => "the adjudicator",
        (RoleName::Retro, _) => "the run review",
        (RoleName::Approver, _) => "the approver",
        (RoleName::Narrator, _) => "the narrator",
        (RoleName::Main, _) => "the agent",
    };
    words.to_owned()
}

fn note(host: &Host, card: Option<&CardId>, text: String) -> EngineResult<()> {
    let level = NoteLevel::Warn;
    host.record(card, None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::run::{ParkReason, RunLifecycle};
    use argonauts_core::events::session::SessionEvent;
    use argonauts_core::events::{RunEvent, RunEventKind};

    use super::*;
    use crate::state::RunState;

    fn health(state: HealthState, since: OffsetDateTime, reason: &str) -> VendorHealth {
        VendorHealth {
            vendor: VendorId("codex".to_owned()),
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

    fn stall(logged_out: &[&str]) -> Stall {
        Stall {
            detail: "nobody could take over as the main agent".to_owned(),
            line:
                "Nobody could take over as the main agent: GPT 5.6 Luna on Codex's session ended \
                   early (a sign-in error), and no other agent can take the role."
                    .to_owned(),
            logged_out: logged_out.iter().map(|name| (*name).to_owned()).collect(),
            resets_at: None,
        }
    }

    /// Each park in a row waits twice as long before it tries again, and after five it leaves
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

    /// The line promises a try on its own only when one is set, and asks for a login only when
    /// the CLI said it's logged out.
    #[test]
    fn the_park_line_says_what_happens_next() {
        let retry = Some(Duration::from_secs(300));
        assert!(
            park_line(&stall(&[]), retry)
                .ends_with("role. It tries again on its own in 5 minutes; /resume tries now.")
        );
        assert!(park_line(&stall(&[]), None).ends_with("role. /resume tries again."));
        let logged_out = park_line(&stall(&["GPT 5.6 Luna on Codex"]), retry);
        assert!(
            logged_out.ends_with(
                "GPT 5.6 Luna on Codex says it's logged out: log in again, then /resume. It \
                 also tries again on its own in 5 minutes."
            ),
            "{logged_out}"
        );
        assert_eq!(
            span_in_words(Duration::from_secs(80 * 60)),
            "1 hour 20 minutes"
        );
        assert_eq!(span_in_words(Duration::from_secs(60)), "a minute");
    }

    /// An outage reads as the agent being unreachable, and any other failure that may pass
    /// as what it was, on the retry line and on the line after it.
    #[test]
    fn a_passing_failure_is_named_for_what_it_was() {
        let error = |category| EndReason::VendorError {
            category,
            message: String::new(),
        };
        for outage in [
            ErrorCategory::Overloaded,
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
        let (codex, claude): (WorkerRef, WorkerRef) = (
            "codex/gpt-5.6".parse().expect("a worker"),
            "claude-code/opus".parse().expect("a worker"),
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

    /// Parks nobody could take a seat in count up, and an agent that finishes a session starts
    /// the count over.
    #[test]
    fn a_finished_session_starts_the_retry_count_over() {
        let event = |seq, kind| RunEvent {
            seq,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: argonauts_core::RunId::new("run-01").expect("a run id"),
            card_id: None,
            session: Some(argonauts_core::names::SessionKey::numbered(1)),
            source: "engine".to_owned(),
            kind,
        };
        let parked = |seq| {
            event(
                seq,
                RunEventKind::Run(RunLifecycle::Parked {
                    reason: ParkReason::NoStandIn {
                        detail: "nobody could take over as the main agent".to_owned(),
                        until: None,
                    },
                    waiting_on: Vec::new(),
                }),
            )
        };
        let mut state = RunState::default();
        state.apply(&parked(1));
        state.apply(&parked(2));
        assert_eq!(state.run.stand_in_waits, 2);
        let mut end = crate::engine::host::crashed_end("");
        end.reason = EndReason::Completed;
        state.apply(&event(
            3,
            RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
        ));
        assert_eq!(state.run.stand_in_waits, 0);
    }
    /// Taking over names the role, as in `takes over as reviewer`, keeping `the` where the
    /// seat is the only one of its kind.
    #[test]
    fn a_seat_reads_as_the_role_taken_over() {
        let seat = |role| Seat {
            at: None,
            role,
            step: None,
            card: None,
            independent_of: Vec::new(),
            isolated: false,
        };
        assert_eq!(seat_as_role(&seat(RoleName::Reviewer)), "reviewer");
        assert_eq!(seat_as_role(&seat(RoleName::PhaseLead)), "the main agent");
        assert_eq!(seat_as_role(&seat(RoleName::Spike)), "an experiment");
    }
}
