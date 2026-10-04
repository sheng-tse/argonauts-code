//! Who carries on when a session ends for a vendor reason. A vendor error never
//! ends a run, in any mode: the same agent goes again once after a short wait when the failure
//! may pass, then its seat goes to the next agent on its handoff list, and only when nobody can
//! take it does the run park, saying why and what fixes it. An agent whose CLI says it's logged
//! out, or whose vendor is down or out of quota, is out: its seats go straight to a stand-in
//! until its CLI says it's logged in again or its vendor is back. Each handover is said once.
//! A blind role's session keeps your plugins out, so only an agent whose CLI can do the same
//! takes its seat, and a CLI that loaded them anyway is out of that seat, every model of it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::{EndReason, loaded_words};
use argonauts_core::limits::HealthState;
use argonauts_core::names::{RoleName, VendorId, WorkerRef};
use argonauts_core::registry::company_of;
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::{FallbackEntry, HandoffMode};
use argonauts_core::steps::StepName;
use argonauts_core::worker::WorkerKind;
use argonauts_core::{ErrorCategory, Note, NoteLevel};

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
    /// Seats stand-ins took, so two seats of one step never go to the same agent.
    taken: Vec<Taken>,
    /// Handovers already said, from whom to whom, so a repeat says nothing new.
    said: BTreeSet<(WorkerRef, WorkerRef)>,
}

/// A seat a stand-in took: where, whose it was, and who took it.
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
    /// Its CLI says it's logged out, after its session ended for this reason when it had one.
    LoggedOut(Option<EndReason>),
    /// Its vendor is down or out of quota, in words.
    Unusable(&'static str),
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
    } else {
        return Ok(None);
    };
    take_over(host, seat, asked, asked, &[asked], &why).map(Some)
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
    } else {
        Why::Ended(reason)
    };
    take_over(host, seat, record.asked_worker(), current, &failed, &why)
}

/// The same agent again on the session of `record`, after `supervisor.retry_wait`, when its
/// failure may pass and it had fewer than two tries since the run last resumed. A 401 its CLI's
/// own login check contradicts was passing. After a resume, the asked agent starts over, since
/// whatever stopped it may have been fixed meanwhile. `None` when it may not go again.
pub(crate) async fn again(host: &Host, record: &SessionRecord) -> EngineResult<Option<WorkerRef>> {
    let failed = failures(host, record);
    if failed.is_empty() {
        return Ok(Some(record.asked_worker().clone()));
    }
    let current = &record.started.worker;
    let reason = end_reason(record);
    let logged_in = login_check(host, current, &reason).await;
    let tries = failed.iter().filter(|worker| **worker == current).count();
    if tries < 2 && reason.may_pass() && logged_in != Some(false) {
        wait(host).await?;
        return Ok(Some(current.clone()));
    }
    Ok(None)
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
        HealthState::Down => "its vendor is down",
        HealthState::Testing => "its vendor is being tested after an outage",
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
            .iter()
            .filter(|worker| available.contains(worker))
            .cloned()
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
        Why::Ended(reason) => format!(
            "{from_name}'s session as {} ended early ({}), so {taker_name} takes it over.",
            seat_in_words(seat),
            in_words(reason),
        ),
    };
    note(host, seat.card.as_ref(), text)
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
        Why::Ended(reason) | Why::LoggedOut(Some(reason)) => {
            format!("{from_name}'s session ended early ({})", in_words(reason))
        }
        Why::LoggedOut(None) => format!("{from_name} is logged out"),
        Why::Unusable(words) => format!("{from_name} is out ({words})"),
    };
    let (detail, line) = if mode == HandoffMode::Wait {
        (
            format!("{role} waits for {from_name}"),
            format!(
                "{} waits for {from_name}: {stopped}, and your handoff settings say to wait \
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
        EndReason::ModelUnavailable { model } => format!("{model} isn't available"),
        EndReason::VendorError { category, .. } if *category == ErrorCategory::Auth => {
            "a sign-in error".to_owned()
        }
        EndReason::VendorError { .. } => "a vendor error".to_owned(),
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
fn seat_in_words(seat: &Seat) -> String {
    let words = match (seat.role, seat.step) {
        (RoleName::Owner, Some(StepName::ResolveConflict)) => "the conflict's resolver",
        (_, Some(StepName::SpikeReport)) => "a spike",
        (RoleName::PhaseLead, _) => "the lead",
        (RoleName::Coordinator, _) => "the coordinator",
        (RoleName::Restater, _) => "a restater",
        (RoleName::Differ, _) => "the differ",
        (RoleName::Drafter, _) => "a drafter",
        (RoleName::Mapper, _) => "the mapper",
        (RoleName::Critic, _) => "a critic",
        (RoleName::Spike, _) => "a spike",
        (RoleName::Judge, _) => "a judge",
        (RoleName::Owner, _) => "the card's owner",
        (RoleName::Consultant, _) => "the consultant",
        (RoleName::Reviewer, _) => "a reviewer",
        (RoleName::Adjudicator, _) => "the adjudicator",
        (RoleName::Retro, _) => "the retro",
        (RoleName::Approver, _) => "the approver",
        (RoleName::Narrator, _) => "the narrator",
        (RoleName::Default, _) => "the agent",
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

    fn stall(logged_out: &[&str]) -> Stall {
        Stall {
            detail: "nobody could take over as the lead".to_owned(),
            line: "Nobody could take over as the lead: GPT 5.6 Luna on Codex's session ended \
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
                        detail: "nobody could take over as the lead".to_owned(),
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
}
