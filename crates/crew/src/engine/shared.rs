//! Everything the loop and the running steps share. Locks here are never held across an await.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use argonauts_core::engine::{EngineContext, EventSink, RepoContext, Services};
use argonauts_core::events::run::RunMode;
use argonauts_core::main_sessions::{MainSessionLease, MainSessions};
use argonauts_core::names::{CheckpointId, SessionKey};
use argonauts_core::policy::PolicyOutcome;
use argonauts_core::settings::Settings;
use argonauts_core::{CardId, RunId};

use crate::checkpoints::Desk;
use crate::engine::journal::DeltaBuffer;
use crate::engine::live::LiveSessions;
use crate::engine::run_meter::RunMeter;
use crate::engine::stand_in;
use crate::engine::tasks::Tasks;
use crate::error::EngineResult;
use crate::git::Git;
use crate::replay_terms::{self, ReplayTerms};
use crate::state::RunState;
use crate::state::ids::Counters;
use crate::status::Viewer;
use crate::status::files::StatusFiles;
use crate::status::folder::Folder;

/// Whether new sessions may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Open,
    /// Paused: running sessions stop after their tool call and nothing new starts.
    Paused,
    /// Stopping or shutting down: steps unwind with `EngineError::Interrupted`.
    Closed,
}

/// A session's `status.json` and the viewer it's filtered for.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusTarget {
    pub path: PathBuf,
    pub viewer: Viewer,
}

/// A policy outcome for a checkpoint, sent by whoever runs `policy.evaluate` beside the loop.
pub type PolicyResult = (CheckpointId, PolicyOutcome);

tokio::task_local! {
    static GIT_ENV: Vec<(String, String)>;
}

/// Runs `future` with `env` set for every git command of the engines it builds, over the
/// process's own. Tests use it to run engines over separate temp homes side by side, since
/// git reads `HOME` and `GIT_CONFIG_GLOBAL` for its identity and config.
pub async fn with_git_env<F: Future>(env: Vec<(String, String)>, future: F) -> F::Output {
    GIT_ENV.scope(env, future).await
}

pub struct Shared {
    pub run: RunId,
    pub repo: RepoContext,
    pub settings: Arc<Settings>,
    pub settings_sha256: String,
    pub sink: Arc<dyn EventSink>,
    pub services: Services,
    pub git: Git,
    pub mode: RunMode,
    /// Always the fold of every event recorded so far.
    pub state: Mutex<RunState>,
    /// Ids handed out, which may run ahead of the log until their events are recorded.
    pub allocated: Mutex<Counters>,
    pub live: LiveSessions,
    pub desk: Desk,
    /// The user's status summary, folded as events are recorded.
    pub status: Mutex<Folder>,
    pub status_files: Mutex<StatusFiles>,
    /// Running sessions whose status tool reads a `status.json`, and who reads it.
    pub status_targets: Mutex<BTreeMap<SessionKey, StatusTarget>>,
    pub deltas: Mutex<DeltaBuffer>,
    pub admission: watch::Sender<Admission>,
    /// The interrupt of the pause or stop under way. A cancelled pause swaps in a fresh one,
    /// so the waits after it aren't cut short by a pause that no longer holds.
    interrupt: Mutex<CancellationToken>,
    pub shutdown: CancellationToken,
    /// Session pumps, sessions run for asks, and policy evaluations: everything that records
    /// beside the loop. The loop joins them before it parks or finishes.
    pub tasks: Tasks,
    /// Cancelled once the loop winds down, so tracked tasks end without recording more.
    pub halt: CancellationToken,
    /// Notified after every recorded event, so the loop checks whether to park.
    pub changed: Notify,
    /// Where policy outcomes go; the loop applies them in order.
    pub policy_outcomes: mpsc::UnboundedSender<PolicyResult>,
    policy_inbox: Mutex<Option<mpsc::UnboundedReceiver<PolicyResult>>>,
    /// Roles' main sessions across runs, for lifetime `project`.
    pub mains: MainSessions,
    /// Main-session leases held by running sessions, released when they end.
    pub leases: Mutex<BTreeMap<SessionKey, MainSessionLease>>,
    /// Sessions a pause stopped: they end `Paused`, and their steps resume them.
    pub pausing: Mutex<BTreeSet<SessionKey>>,
    /// Sessions the pause or stop under way told to stop, with their cards, for its commit.
    /// Holds the ones that went live after it began too.
    pub stopped: Mutex<BTreeMap<SessionKey, Option<CardId>>>,
    /// Held while a pause commits and records `Paused`, across those awaits. A second Esc's
    /// pause waits for it, rather than record `Paused` first with nothing committed.
    pub settling_pause: tokio::sync::Mutex<()>,
    /// One of the evolution loop's replays: what its cards keep hidden and what it reuses.
    pub replay: Option<ReplayTerms>,
    /// Held while an ask takes a recorded answer, so two asks of one step never take the same.
    pub reusing: Mutex<()>,
    /// The run's spend against its budget, from every session's meter readings.
    pub run_meter: Mutex<RunMeter>,
    /// The build and cache output the run's commits left out that it has named so far, so each
    /// commit names only what's new.
    pub told_left_out: Mutex<BTreeSet<String>>,
    /// Who is out, which stand-in took which seat, and which handovers were said.
    pub(crate) stand_ins: Mutex<stand_in::Memory>,
}

impl Shared {
    /// Takes what the loop keeps for itself (the inbox and vendor events) out of the context
    /// first; builds the rest from it and the folded history.
    pub fn new(context: &EngineContext, state: RunState) -> EngineResult<Self> {
        let mut status = Folder::default();
        let mut run_meter = RunMeter::new(&context.settings.budget);
        for event in &context.history {
            status.push(event);
            run_meter.observe(event);
        }
        let (policy_outcomes, policy_inbox) = mpsc::unbounded_channel();
        // `<state>/<repo>/runs/<run>`: main sessions live in the repo's state directory.
        let repo_state = context
            .repo
            .run_dir
            .parent()
            .and_then(Path::parent)
            .unwrap_or(&context.repo.run_dir);
        let settings = context.settings.clone();
        Ok(Self {
            run: context.run.clone(),
            repo: context.repo.clone(),
            settings_sha256: context.settings_sha256.clone(),
            sink: context.sink.clone(),
            services: context.services.clone(),
            git: GIT_ENV
                .try_with(|env| Git::with_env(env.clone()))
                .unwrap_or_default()
                .leaving_out(
                    context.settings.merge.leave_out_builtin,
                    &context.settings.merge.leave_out,
                ),
            mode: state.run.mode.unwrap_or(RunMode::Solo),
            allocated: Mutex::new(state.ids.clone()),
            state: Mutex::new(state),
            live: LiveSessions::default(),
            desk: Desk::default(),
            status: Mutex::new(status),
            status_files: Mutex::new(StatusFiles::default()),
            status_targets: Mutex::new(BTreeMap::new()),
            deltas: Mutex::new(DeltaBuffer::new(
                settings.supervisor.journal_deltas_per_second,
            )),
            admission: watch::channel(Admission::Open).0,
            interrupt: Mutex::new(CancellationToken::new()),
            shutdown: context.shutdown.clone(),
            tasks: Tasks::default(),
            halt: CancellationToken::new(),
            changed: Notify::new(),
            policy_outcomes,
            policy_inbox: Mutex::new(Some(policy_inbox)),
            mains: MainSessions::open(repo_state),
            leases: Mutex::new(BTreeMap::new()),
            pausing: Mutex::new(BTreeSet::new()),
            stopped: Mutex::new(BTreeMap::new()),
            settling_pause: tokio::sync::Mutex::new(()),
            replay: replay_terms::read(&context.repo.run_dir)?,
            reusing: Mutex::new(()),
            run_meter: Mutex::new(run_meter),
            told_left_out: Mutex::new(BTreeSet::new()),
            stand_ins: Mutex::default(),
            settings,
        })
    }

    /// The receiving end of `policy_outcomes`, once, for the loop.
    pub fn take_policy_inbox(&self) -> Option<mpsc::UnboundedReceiver<PolicyResult>> {
        lock(&self.policy_inbox).take()
    }

    pub fn admission(&self) -> Admission {
        *self.admission.borrow()
    }

    /// The interrupt as it is now. A wait reads it when it starts.
    pub fn interrupt(&self) -> Interrupt {
        Interrupt(lock(&self.interrupt).clone())
    }

    /// After a cancelled pause: waits that start from now on get an interrupt of their own.
    pub fn renew_interrupt(&self) {
        let mut interrupt = lock(&self.interrupt);
        if interrupt.is_cancelled() {
            *interrupt = CancellationToken::new();
        }
    }
}

/// One pause's or stop's interrupt: cancelled on a `now` pause, a stop, or shutdown.
#[derive(Debug, Clone)]
pub struct Interrupt(CancellationToken);

impl Interrupt {
    /// Owns the token, so `host.interrupted().cancelled()` can sit in a `select!` branch.
    pub fn cancelled(self) -> WaitForCancellationFutureOwned {
        self.0.cancelled_owned()
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }
}

/// Locks, carrying on after a panic elsewhere: every guarded value stays consistent between
/// statements, so a poisoned lock holds nothing half-written.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
