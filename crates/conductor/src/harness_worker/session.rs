//! Starting a session and running it to its end: refuse what must not start, build the request,
//! wrap it in the sandbox, take a slot, spawn in its own process group, register the group, then
//! stream, meter, watch, steer, and stop until the end, which is sent after the last event.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use argonauts_core::create_private_dir_all;
use argonauts_core::events::session::{EndReason, LimitEvent, SessionEnd, SessionEvent};
use argonauts_core::worker::{
    SandboxMode, SessionControl, SessionHandle, SessionLimits, SessionSpec, Worker, WorkerError,
};
use tokio::sync::mpsc;

use super::context::{Context, Launch};
use super::observe::Observer;
use super::outbox::{self, Outbox};
use super::{HarnessWorker, driven, lines, refuse};
use crate::rawlog::RawLog;
use crate::sandbox::{SelfTest, selftest};
use crate::slots::Slots;

pub async fn start(
    worker: &HarnessWorker,
    mut spec: SessionSpec,
) -> Result<SessionHandle, WorkerError> {
    let (rules, sandbox) = worker.sandbox_for(&spec.role);
    spec.sandbox.mode = effective_mode(spec.sandbox.mode, sandbox.mode())?;
    let self_test = match spec.sandbox.mode {
        SandboxMode::None => SelfTest {
            ok: true,
            detail: "no sandbox to test".to_owned(),
        },
        SandboxMode::Os | SandboxMode::Container => {
            selftest::self_test_once(sandbox.as_ref()).await
        }
    };
    // The board may read a file every supervisor shares.
    let (board, vendor) = (worker.health.clone(), worker.vendor());
    let health = tokio::task::spawn_blocking(move || board.state(&vendor))
        .await
        .map_err(|err| WorkerError::Io(format!("could not read the vendor's health: {err}")))?;
    refuse::check(&spec, &rules, &health, &self_test)?;
    let ctx = worker
        .home()
        .map(|home| Arc::new(Context::new(worker, spec.clone(), home)))
        .map_err(|err| format!("no Argonauts home for the session: {err}"));
    let (handle, out, control) = outbox::open(spec.key);
    tokio::spawn(run(ctx, out, control));
    Ok(handle)
}

/// The sandbox the session really gets: none when the spec asks for none or its role's
/// `sandbox.mode` is `tool` or `none` (which `refuse::check` handles), else the one that mode
/// names. A container asked for is never quietly swapped for the OS sandbox.
fn effective_mode(
    requested: SandboxMode,
    available: SandboxMode,
) -> Result<SandboxMode, WorkerError> {
    match (requested, available) {
        (SandboxMode::None, _) | (_, SandboxMode::None) => Ok(SandboxMode::None),
        (SandboxMode::Container, SandboxMode::Os) => Err(WorkerError::Refused(
            "this session needs a container sandbox, and the worker has only the OS sandbox"
                .to_owned(),
        )),
        (_, available) => Ok(available),
    }
}

async fn run(
    ctx: Result<Arc<Context>, String>,
    out: Outbox,
    control: mpsc::Receiver<SessionControl>,
) {
    let started = Instant::now();
    let end = match ctx {
        Ok(ctx) => session(ctx, out.clone(), control, started).await,
        Err(message) => not_started(EndReason::StartFailed { message }, started, None),
    };
    out.end(end);
}

/// A session's parts once its launch is ready, for the line and the driven runners.
pub(super) struct Running {
    pub ctx: Arc<Context>,
    pub out: Outbox,
    pub obs: Observer,
    pub raw: RawLog,
    pub control: mpsc::Receiver<SessionControl>,
    /// Controls that came while the session waited for a slot.
    pub early: Vec<SessionControl>,
    pub started: Instant,
}

async fn session(
    ctx: Arc<Context>,
    out: Outbox,
    mut control: mpsc::Receiver<SessionControl>,
    started: Instant,
) -> SessionEnd {
    let failed = |message: String| not_started(EndReason::StartFailed { message }, started, None);
    if let Err(err) = create_private_dir_all(&ctx.spec.session_dir) {
        return failed(format!("could not create the session folder: {err}"));
    }
    let raw = match RawLog::create(&ctx.spec.session_dir) {
        Ok(raw) => raw,
        Err(err) => return failed(format!("could not open the session's raw log: {err}")),
    };
    let raw_path = Some(raw.stdout_path());
    let launch = match ctx.launch(&ctx.spec) {
        Ok(launch) => launch,
        Err(message) => return not_started(EndReason::StartFailed { message }, started, raw_path),
    };
    let slots = Slots::new(&ctx.home, &ctx.settings.concurrency);
    let mut early = Vec::new();
    let _slot = tokio::select! {
        slot = slots.acquire(&ctx.vendor) => match slot {
            Ok(slot) => slot,
            Err(err) => return not_started(
                EndReason::StartFailed { message: err.to_string() },
                started,
                raw_path,
            ),
        },
        reason = stopped_while_waiting(&mut control, &mut early) => {
            return not_started(reason, started, raw_path);
        }
    };
    let obs = Observer::new(ctx.clone(), out.clone(), started);
    let running = Running {
        ctx: ctx.clone(),
        out,
        obs,
        raw,
        control,
        early,
        started,
    };
    match ctx.adapter.session_driver() {
        Some(driver) => driven::run(running, launch, driver).await,
        None => lines::run(running, launch).await,
    }
}

/// Waits for a control that ends the session before it starts, keeping the others for later.
async fn stopped_while_waiting(
    control: &mut mpsc::Receiver<SessionControl>,
    early: &mut Vec<SessionControl>,
) -> EndReason {
    loop {
        match control.recv().await {
            None | Some(SessionControl::Stop) => return EndReason::Stopped,
            Some(SessionControl::StopAfterTool) => return EndReason::Paused,
            Some(other) => early.push(other),
        }
    }
}

/// `Limit(NativeCap)` where the harness enforces one, and the vendor session when it's known
/// before the first event.
pub(super) fn announce_start(ctx: &Context, out: &Outbox, obs: &mut Observer, launch: &Launch) {
    if ctx.capabilities.native_budget_cap
        && let Some(usd) = launch.request.max_budget_usd
    {
        out.session(SessionEvent::Limit(LimitEvent::NativeCap { usd }));
    }
    let known = launch
        .request
        .session_id
        .clone()
        .or_else(|| ctx.spec.resume.clone());
    if let Some(id) = known {
        obs.vendor_session(id);
    }
}

pub(super) fn end_of(
    obs: &Observer,
    reason: EndReason,
    exit: (Option<i32>, Option<i32>),
    started: Instant,
    raw_log: Option<PathBuf>,
) -> SessionEnd {
    let facts = &obs.facts;
    SessionEnd {
        vendor_session: facts.vendor_session.clone(),
        exit_code: exit.0,
        signal: exit.1,
        duration_ms: millis(started.elapsed()),
        reason,
        spend: obs.spend(),
        tool_calls: obs.tool_calls(),
        result_text: facts.result_text.clone(),
        structured: facts.structured.clone(),
        last_message: facts.last_message.clone(),
        raw_log,
    }
}

/// The end of a session whose process never ran.
pub(super) fn not_started(
    reason: EndReason,
    started: Instant,
    raw_log: Option<PathBuf>,
) -> SessionEnd {
    SessionEnd {
        vendor_session: None,
        exit_code: None,
        signal: None,
        duration_ms: millis(started.elapsed()),
        reason,
        spend: Default::default(),
        tool_calls: 0,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log,
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// How often the wall clock and the watchdogs are checked: every second, or a hundredth of the
/// shortest time limit so short limits still act close to on time.
pub(super) fn tick_every(limits: &SessionLimits) -> Duration {
    const MOST: Duration = Duration::from_secs(1);
    const LEAST: Duration = Duration::from_millis(10);
    [limits.max_minutes, limits.inactivity_min]
        .into_iter()
        .flatten()
        .filter_map(|minutes| Duration::try_from_secs_f64(minutes * 60.0 / 100.0).ok())
        .filter(|hundredth| !hundredth.is_zero())
        .min()
        .map_or(MOST, |hundredth| hundredth.clamp(LEAST, MOST))
}

/// The deadline, or a year away for a `select!` branch that is disabled anyway.
pub(super) fn or_far(deadline: Option<tokio::time::Instant>) -> tokio::time::Instant {
    deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(365 * 24 * 3600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sandbox_is_none_when_either_side_has_none() {
        use SandboxMode::*;
        assert_eq!(effective_mode(None, Os).unwrap(), None);
        assert_eq!(effective_mode(Os, None).unwrap(), None);
        assert_eq!(effective_mode(Os, Os).unwrap(), Os);
        assert_eq!(effective_mode(Os, Container).unwrap(), Container);
        assert_eq!(effective_mode(Container, Container).unwrap(), Container);
    }

    #[test]
    fn a_container_is_never_swapped_for_the_os_sandbox() {
        match effective_mode(SandboxMode::Container, SandboxMode::Os) {
            Err(WorkerError::Refused(why)) => assert!(why.contains("container"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn short_limits_tick_often() {
        let mut limits = SessionLimits::default();
        assert_eq!(tick_every(&limits), Duration::from_secs(1));
        limits.inactivity_min = Some(0.05);
        assert_eq!(tick_every(&limits), Duration::from_millis(30));
        limits.max_minutes = Some(0.0001);
        assert_eq!(tick_every(&limits), Duration::from_millis(10));
        limits.max_minutes = Some(-1.0);
        assert_eq!(tick_every(&limits), Duration::from_millis(30));
    }
}
