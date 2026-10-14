//! The run host: one `argonauts host` process per run. It owns the run's lock, journal,
//! socket, and agent processes, and exits when the run finishes or waits.

pub(crate) mod clis;
mod door;
pub(crate) mod log_views;
mod orphans;
mod relay;
mod reloader;
mod revert_check;
mod server;
pub(crate) mod services;
mod sink;
mod start;
mod time_limit;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use argonauts_core::control::{ControlReply, ControlRequest};
use argonauts_core::engine::{
    EngineContext, EngineExit, EventSink as _, RepoContext, RunEngine, Services,
};
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome, WaitReason};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::hidden_git::CLEARED_STALE_LOCK;
use argonauts_core::journal::{Journal, RunLock, read_all, read_since_snapshot};
use argonauts_core::layout::run_files;
use argonauts_core::paths::write_private_file;
use argonauts_core::settings::Settings;
use argonauts_core::{Note, NoteLevel, Paths, RunId};
use time::OffsetDateTime;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

pub(crate) use self::server::RUN_HOST_EXITING;

use self::door::Door;
use self::relay::{Queue, Receipt};
use self::server::ServerState;
use self::sink::JournalSink;
use crate::ancestry::{self, AgentMarkers};
use crate::env::{AppEnv, FolderGit};
use crate::live_registry::{self, LiveRunHost};
use crate::run_files::{self as files, SettingsRecord, StartRecord};
use crate::usage_ledger::UsageLedger;

const SOURCE: &str = "argonauts-code";
/// Live events kept for a slow subscriber before it reads the rest from the journal.
const EVENT_BUFFER: usize = 1024;
/// One command at a time, so the host knows which ones an engine took.
const INBOX: usize = 1;
/// `run_host.wait_grace` when it doesn't parse.
const DEFAULT_WAIT_GRACE: Duration = Duration::from_secs(10);
/// How long an exiting host waits to log what became of the pending requests.
const REPORT_WAIT: Duration = Duration::from_secs(2);

/// Builds the engine for a run. Tests pass a scripted one.
pub(crate) type EngineFactory = Arc<dyn Fn(&Settings, RunMode) -> Box<dyn RunEngine> + Send + Sync>;

pub(crate) struct HostOptions {
    pub paths: Paths,
    pub engine: EngineFactory,
    /// `run_host.wait_grace` when `None`.
    pub wait_grace: Option<Duration>,
    /// Off only for tests that host a run in their own process, whose parent is the test runner.
    pub check_ancestry: bool,
    /// Cancelled on SIGTERM: the engine stops its sessions after their tool call and waits, and
    /// the host exits without waiting out the grace.
    pub shutdown: CancellationToken,
    /// Hold the computer's sleep off while this process hosts the run, when the run's
    /// settings say so. Off for tests, which host runs in their own process.
    pub keep_awake: bool,
}

impl HostOptions {
    /// `ARGONAUTS_HOME` and `argonauts_crew::engine_for`.
    pub(crate) fn production() -> anyhow::Result<Self> {
        let paths = Paths::from_env()
            .map_err(|_| anyhow!("can't find your home directory; set ARGONAUTS_HOME"))?;
        Ok(Self {
            paths,
            engine: Arc::new(|settings: &Settings, mode: RunMode| {
                argonauts_crew::engine_for(settings, mode)
            }),
            wait_grace: None,
            check_ancestry: true,
            shutdown: CancellationToken::new(),
            keep_awake: true,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HostExit {
    /// Another run host holds the lock, so this one left quietly.
    AlreadyHosted,
    Finished(RunOutcome),
    Waiting(WaitReason),
    Crashed(String),
}

/// `argonauts host`: exits 0 when the run finishes or waits, and fails when it crashed.
pub(crate) async fn host(repo_root: &Path, run: &RunId) -> anyhow::Result<()> {
    let options = HostOptions::production()?;
    let mut terminate = signal(SignalKind::terminate()).context("listening for SIGTERM")?;
    let shutdown = options.shutdown.clone();
    tokio::spawn(async move {
        if terminate.recv().await.is_some() {
            shutdown.cancel();
        }
    });
    match host_with(repo_root, run, options).await? {
        HostExit::Crashed(reason) => anyhow::bail!("run {run} crashed: {reason}"),
        HostExit::AlreadyHosted | HostExit::Finished(_) | HostExit::Waiting(_) => Ok(()),
    }
}

/// Takes the lock, serves the socket, kills orphans, queues pending requests, and drives the
/// engine, calling it again when a request arrives within the wait grace.
pub(crate) async fn host_with(
    repo_root: &Path,
    run: &RunId,
    options: HostOptions,
) -> anyhow::Result<HostExit> {
    let env = AppEnv::for_repo(options.paths.clone(), repo_root, false)?;
    let run_dir = env.run_dir(run);
    if !run_dir.join(run_files::START).is_file() {
        bail!("run {run} has no start.json, so there's nothing to host");
    }
    if options.check_ancestry {
        // Other agents' shells, such as `OPENCLAW_SHELL`, may start runs; only the approval
        // path refuses them, and it checks each peer on the socket.
        ancestry::check_self(&options.paths, &AgentMarkers::own())?;
    }
    let Some(lock) = RunLock::try_acquire(&run_dir)? else {
        return Ok(HostExit::AlreadyHosted);
    };
    let recorded = files::read_settings(&run_dir)?;
    let start = files::read_start(&run_dir)?;
    let keep_awake = options.keep_awake;
    let (host, mut queue) =
        Host::open(env, run.clone(), run_dir, &lock, &recorded, start, options)?;
    // The run works for as long as this process hosts it, and the hold ends with the
    // process however it ends.
    let _awake = (keep_awake && host.settings.awake.while_working)
        .then(|| crate::keep_awake::while_hosting(&host.settings.awake))
        .flatten();
    // Clients get their Welcome now, and their requests queue while the slow start runs.
    let serving = Serving::start(&host);
    let exit = match (host.begin(&recorded).await, &serving) {
        (Ok(()), Ok(_)) => Ok(host.run(&mut queue).await),
        (Ok(()), Err(err)) => Ok(host.crash(format!("{err:#}"))),
        (Err(err), _) => Err(err),
    };
    // `run` settles the queue once its engine is done. This answers what the socket queued when
    // the start failed first.
    let failed = HostExit::Crashed(String::new());
    host.settle(&mut queue, exit.as_ref().unwrap_or(&failed));
    if let Ok(HostExit::Finished(_)) = &exit {
        host.record_usage();
    }
    if let Ok(serving) = serving {
        serving.stop().await;
    }
    host.close();
    // The journal inside the host must not outlive the lock.
    drop(host);
    drop(lock);
    exit
}

/// The control socket's accept loop.
struct Serving {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Serving {
    fn start(host: &Host) -> anyhow::Result<Self> {
        let listener = server::bind(&host.run_dir)?;
        let cancel = CancellationToken::new();
        let task = tokio::spawn(server::serve(listener, host.state.clone(), cancel.clone()));
        Ok(Self { cancel, task })
    }

    async fn stop(self) {
        self.cancel.cancel();
        match self.task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!("the control socket failed: {err:#}"),
            Err(err) => tracing::warn!("the control socket's task failed: {err}"),
        }
    }
}

/// One run host's hold on its run, from the lock to exit.
struct Host {
    env: AppEnv,
    run: RunId,
    run_dir: PathBuf,
    journal: PathBuf,
    /// No events yet, so `begin` records `RunStarted`.
    fresh: bool,
    /// What a new run says about where it starts from, held until the engine has read its
    /// history: an engine that finds more than `RunStarted` there takes the run for a resumed
    /// one.
    start_notes: Mutex<Vec<String>>,
    start: StartRecord,
    settings: Arc<Settings>,
    turned_on: Vec<String>,
    settings_sha256: String,
    sink: Arc<JournalSink>,
    state: Arc<ServerState>,
    pid: u32,
    options: HostOptions,
}

impl Host {
    /// The journal, the pid file, the registry entry, what the socket needs, and the queue the
    /// socket feeds. A failure here leaves no run behind to record it in, so it's an error.
    fn open(
        env: AppEnv,
        run: RunId,
        run_dir: PathBuf,
        lock: &RunLock,
        recorded: &SettingsRecord,
        start: StartRecord,
        options: HostOptions,
    ) -> anyhow::Result<(Self, Queue)> {
        let (pid, started) = ancestry::own_process();
        let token = files::read_token(&run_dir).context("reading the approval token")?;
        write_private_file(&run_dir.join(run_files::PID), format!("{pid}\n").as_bytes())
            .context("writing the run host's pid file")?;
        let journal_path = run_dir.join(run_files::EVENTS);
        let journal = Journal::open(&journal_path, lock)?;
        let fresh = journal.next_seq() == 0;
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let sink = JournalSink::new(journal, run.clone(), events.clone())
            .counting_sessions(options.paths.clone(), pid);
        let entry = LiveRunHost {
            pid,
            start_time: started,
            repo_root: env.repo_root.clone(),
            run: run.clone(),
            sessions: 0,
        };
        if let Err(err) = live_registry::register(&options.paths, &entry) {
            tracing::warn!("adding this run host to registry.json: {err:#}");
        }
        let (door, queue) = Door::new();
        let state = Arc::new(ServerState {
            run: run.clone(),
            pid,
            token,
            home: options.paths.clone(),
            markers: AgentMarkers::from_settings(&recorded.settings),
            run_dir: run_dir.clone(),
            journal: journal_path.clone(),
            door,
            events,
            ancestry: Arc::new(ancestry::agent_ancestor),
            check_peers: options.check_ancestry,
        });
        let host = Self {
            settings: Arc::new(recorded.settings.clone()),
            turned_on: recorded.turned_on.clone(),
            settings_sha256: recorded.sha256.clone(),
            sink: Arc::new(sink),
            journal: journal_path,
            fresh,
            start_notes: Mutex::new(Vec::new()),
            env,
            run,
            run_dir,
            start,
            state,
            pid,
            options,
        };
        Ok((host, queue))
    }

    /// A new run's `RunStarted`, then the agents a crashed run host left. Before `RunStarted`
    /// there's no run to record a failure in, so it's an error.
    async fn begin(&self, recorded: &SettingsRecord) -> anyhow::Result<()> {
        if self.fresh {
            let (started, notes) = off_the_runtime(|| {
                start::run_started(&self.env, &self.run, &self.run_dir, &self.start, recorded)
            })?;
            self.record(RunEventKind::Run(RunLifecycle::Started(Box::new(started))))?;
            *self
                .start_notes
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = notes;
        } else if matches!(self.env.git(), FolderGit::Hidden(_)) {
            // A stop in the middle of a write left its lock, which every git call of the run
            // taken up again would fail on.
            let root = &self.env.repo_root;
            let writers = start::other_writers(&self.env);
            match off_the_runtime(|| crate::hidden_git::clear_locks(root, writers)) {
                Ok(true) => self.note(NoteLevel::Warn, CLEARED_STALE_LOCK.to_owned()),
                Ok(false) => {}
                Err(err) => {
                    self.note(NoteLevel::Error, format!("{err:#}"));
                    return Err(err);
                }
            }
        }
        let killed = orphans::kill_orphans(&self.run_dir, &self.options.paths).await?;
        let stopped = match killed.len() {
            0 => None,
            1 => Some("an agent".to_owned()),
            n => Some(format!("{n} agents")),
        };
        if let Some(stopped) = stopped {
            self.note(
                NoteLevel::Warn,
                format!("Stopped {stopped} a crashed run host left running."),
            );
        }
        Ok(())
    }

    fn record(&self, kind: RunEventKind) -> anyhow::Result<RunEvent> {
        Ok(self.sink.record(None, None, SOURCE, kind)?)
    }

    fn note(&self, level: NoteLevel, text: String) {
        note(self.sink.as_ref(), level, text);
    }

    /// A failure once the run has a log: it goes in the log, and the run counts as crashed.
    fn crash(&self, reason: String) -> HostExit {
        self.note(NoteLevel::Error, reason.clone());
        HostExit::Crashed(reason)
    }

    async fn run(&self, queue: &mut Queue) -> HostExit {
        let started = match start::first_started(&self.journal) {
            Ok(started) => started,
            Err(err) => return self.crash(format!("{err:#}")),
        };
        // Read before this start's own notes, so a new run's engine sees only its RunStarted.
        let history = match self.history() {
            Ok(history) => history,
            Err(reason) => return self.crash(reason),
        };
        let start_notes = std::mem::take(
            &mut *self
                .start_notes
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for text in start_notes {
            self.note(NoteLevel::Info, text);
        }
        let (repo, services) = match off_the_runtime(|| self.workspace(&started.base_commit)) {
            Ok(workspace) => workspace,
            Err(reason) => return self.crash(reason),
        };
        let (receipts, reopen) = match self.queue_pending(queue).await {
            Ok(pending) => pending,
            Err(reason) => return self.crash(reason),
        };
        let mut reporter = tokio::spawn(report_pending(self.sink.clone(), receipts));
        let time_limit = self.pause_when_time_is_spent();

        let exit = self
            .drive(&repo, &services, started.mode, history, reopen, queue)
            .await;
        if let Some(timer) = time_limit {
            timer.abort();
        }
        self.settle(queue, &exit);
        if tokio::time::timeout(REPORT_WAIT, &mut reporter)
            .await
            .is_err()
        {
            reporter.abort();
        }
        exit
    }

    /// The base worktree and the run's services, then the revert check.
    fn workspace(&self, base_commit: &str) -> Result<(RepoContext, Services), String> {
        let paths = &self.options.paths;
        let worktrees = paths
            .home()
            .join("worktrees")
            .join(self.env.repo_id.as_str())
            .join(self.run.as_str());
        let repo = RepoContext {
            id: self.env.repo_id.clone(),
            root: self.env.repo_root.clone(),
            run_dir: self.run_dir.clone(),
            base_worktree: worktrees.join("base"),
            worktrees,
        };
        start::base_worktree(&repo.root, &repo.base_worktree, base_commit)
            .map_err(|err| format!("{err:#}"))?;
        let reload = reloader::reloader(&self.env, &self.start, self.sink.clone());
        let services = services::services(
            self.settings.clone(),
            self.turned_on.clone(),
            paths,
            &repo,
            reload,
        )
        .map_err(|err| format!("building the run's services: {err:#}"))?;
        self.check_reverts(&services);
        Ok((repo, services))
    }

    /// Queues the pending requests ahead of any the socket took while the run started. They're
    /// taken only now, so a failed start leaves them on disk, and one still queued when the host
    /// exits goes back there. Returns their receipts, and whether one asked to reopen a stopped
    /// run.
    async fn queue_pending(
        &self,
        queue: &mut Queue,
    ) -> Result<(Vec<(String, Receipt)>, bool), String> {
        let mut pending =
            files::set_aside_pending(&self.run_dir).map_err(|err| format!("{err:#}"))?;
        for line in &pending.skipped {
            self.note(
                NoteLevel::Warn,
                format!("A request sent while the run was stopped couldn't be read ({line})."),
            );
        }
        let mut entries = Vec::new();
        let mut receipts = Vec::new();
        let mut reopen = false;
        for request in std::mem::take(&mut pending.requests) {
            // Only `/resume` of a stopped run defers a `CancelPause`, so the engine goes on.
            reopen |= matches!(request, ControlRequest::CancelPause);
            // Nobody waits for what a take-back returns, so its messages stay queued rather
            // than vanish. Clients ask a live run host instead.
            if let ControlRequest::TakeBack { .. } = request {
                self.note(
                    NoteLevel::Warn,
                    "A take-back sent while the run was stopped was dropped, so its messages will \
                     still be delivered."
                        .to_owned(),
                );
                continue;
            }
            let what = describe(&request);
            match server::admit_pending(&self.state, request).await {
                Ok((entry, receipt)) => {
                    entries.push(entry);
                    receipts.push((what, receipt));
                }
                Err(message) => self.note(NoteLevel::Warn, refused_while_stopped(&what, &message)),
            }
        }
        queue.restore(entries);
        pending.delivered().map_err(|err| format!("{err:#}"))?;
        Ok((receipts, reopen))
    }

    /// Runs the engine until it finishes or crashes, or waits with no request inside the grace.
    /// `reopen` goes to the first engine only.
    async fn drive(
        &self,
        repo: &RepoContext,
        services: &Services,
        mode: RunMode,
        history: Vec<RunEvent>,
        mut reopen: bool,
        queue: &mut Queue,
    ) -> HostExit {
        let state = &self.state;
        let grace = self.options.wait_grace.unwrap_or_else(|| {
            self.settings
                .run_host
                .wait_grace
                .parse()
                .ok()
                .flatten()
                .unwrap_or(DEFAULT_WAIT_GRACE)
        });
        let shutdown = &self.options.shutdown;
        let mut history = Some(history);
        // Set after a restart for requests an engine left, when that engine took none of them.
        let mut retried_idle = false;
        loop {
            let history = match history.take() {
                Some(history) => history,
                None => match self.history() {
                    Ok(history) => history,
                    Err(reason) => return self.crash(reason),
                },
            };
            state.door.open();
            let (inbox, engine_inbox) = mpsc::channel(INBOX);
            let context = EngineContext {
                run: self.run.clone(),
                repo: repo.clone(),
                settings: self.settings.clone(),
                settings_sha256: self.settings_sha256.clone(),
                history,
                sink: self.sink.clone(),
                inbox: engine_inbox,
                agent_events: services.health.subscribe(),
                services: services.clone(),
                shutdown: shutdown.clone(),
                reopen: std::mem::take(&mut reopen),
            };
            let engine = (self.options.engine)(&self.settings, mode);
            let running = tokio::spawn(engine.run(context));
            let (joined, forwarded) = tokio::join!(running, relay::forward(queue, inbox));
            let taken = forwarded.settle(queue, joined.as_ref().ok());
            let reason = match joined {
                Ok(EngineExit::Finished(outcome)) => return HostExit::Finished(outcome),
                Ok(EngineExit::Crashed(reason)) => {
                    tracing::error!("run {} crashed: {reason}", self.run);
                    return HostExit::Crashed(reason);
                }
                Ok(EngineExit::Waiting(reason)) => reason,
                Err(err) => return self.crash(format!("the engine failed: {err}")),
            };
            if shutdown.is_cancelled() {
                return HostExit::Waiting(reason);
            }
            // A request the engine didn't take before it began to wait came in the grace.
            // An engine that takes nothing gets one more try, not a loop.
            if !queue.is_empty() && (taken > 0 || !retried_idle) {
                retried_idle = taken == 0;
                continue;
            }
            state.door.wait();
            answer_waiting(state, queue).await;
            tokio::select! {
                () = state.door.woken() => {}
                () = tokio::time::sleep(grace) => {
                    if state.door.close_if_waiting() {
                        return HostExit::Waiting(reason);
                    }
                    // A request woke it just as the grace ran out.
                    state.door.woken().await;
                }
                () = shutdown.cancelled() => return HostExit::Waiting(reason),
            }
            retried_idle = false;
        }
    }

    /// Answers every request still queued as the host exits. What a client would have saved for
    /// the next run host goes to `pending-answers.jsonl`; the rest are refused.
    fn settle(&self, queue: &mut Queue, exit: &HostExit) {
        self.state.door.close();
        for entry in queue.close() {
            let reply = self.leftover(&entry.request, exit);
            entry.answer(reply);
        }
    }

    fn leftover(&self, request: &ControlRequest, exit: &HostExit) -> ControlReply {
        let finished = match exit {
            HostExit::Finished(RunOutcome::Merged) => {
                return server::error("the run was merged, so it takes no more requests");
            }
            HostExit::Finished(_) => true,
            HostExit::AlreadyHosted | HostExit::Waiting(_) | HostExit::Crashed(_) => false,
        };
        let stops = matches!(
            request,
            ControlRequest::Stop | ControlRequest::StopSession { .. }
        );
        if finished && stops {
            return server::error("the run has already finished");
        }
        // No engine took it, so the client can send it to the next run host.
        if !relay::deferrable(request) {
            return server::error(RUN_HOST_EXITING);
        }
        match files::append_pending(&self.run_dir, request) {
            Ok(()) => relay::deferred_reply(request),
            Err(err) => server::error(format!(
                "the run's run host exited, and saving the request for the next one failed: \
                 {err:#}"
            )),
        }
    }

    /// The finished run's spend goes to the usage ledger, by agent and model, from its whole
    /// log.
    fn record_usage(&self) {
        match read_all(&self.journal) {
            Ok(back) => UsageLedger::new(&self.options.paths).run_ended(
                &self.env.repo_root,
                &self.run,
                &back.events,
                OffsetDateTime::now_utc(),
            ),
            Err(err) => tracing::warn!("couldn't add run {} to the usage ledger: {err}", self.run),
        }
    }

    /// The log from its latest snapshot. Unreadable lines, such as a torn last one, are skipped.
    fn history(&self) -> Result<Vec<RunEvent>, String> {
        let back = read_since_snapshot(&self.journal)
            .map_err(|err| format!("reading the run's log: {err}"))?;
        for (line, err) in &back.skipped {
            tracing::warn!("skipping line {line} of the run's log: {err}");
        }
        Ok(back.events)
    }

    /// Losing autonomy on a revert. A failed check only costs this start's look.
    fn check_reverts(&self, services: &Services) {
        let notices = revert_check::check_reverts(
            &self.env.repo_root,
            &self.env.state_dir,
            services.policy.as_ref(),
            OffsetDateTime::now_utc(),
        );
        match notices {
            Ok(notices) => {
                for notice in notices {
                    self.note(NoteLevel::Warn, notice);
                }
            }
            Err(err) => self.note(
                NoteLevel::Warn,
                format!("Couldn't check earlier auto-merges for reverts: {err:#}"),
            ),
        }
    }

    /// Lets go of the socket, the pid file, and the registry entry. The lock goes last.
    fn close(&self) {
        if let Err(err) = files::remove_socket(&self.run_dir) {
            tracing::warn!("removing the control socket: {err}");
        }
        let pid = self.run_dir.join(run_files::PID);
        if let Err(err) = std::fs::remove_file(&pid)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("removing {}: {err}", pid.display());
        }
        if let Err(err) = live_registry::unregister(&self.options.paths, self.pid) {
            tracing::warn!("removing this run host from registry.json: {err:#}");
        }
    }
}

/// Runs slow blocking work, such as git, without holding up the socket's tasks, which may wait
/// in this worker thread's queue.
fn off_the_runtime<T>(work: impl FnOnce() -> T) -> T {
    let flavor = tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor());
    match flavor {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(work),
        _ => work(),
    }
}

/// Answers the queued requests a waiting run doesn't wake for, from the run's files. The rest
/// wait for the next engine.
async fn answer_waiting(state: &ServerState, queue: &mut Queue) {
    let mut waiting = Vec::new();
    for entry in queue.take_all() {
        if server::while_waiting(&entry.request).is_some() {
            let reply = server::waiting_reply(state, &entry.request).await;
            entry.answer(reply);
        } else {
            waiting.push(entry);
        }
    }
    queue.restore(waiting);
}

/// Pending requests have no one waiting on the other end, so their refusals go in the run's log.
async fn report_pending(sink: Arc<JournalSink>, receipts: Vec<(String, Receipt)>) {
    for (what, receipt) in receipts {
        if let ControlReply::Error { message } = receipt.reply().await {
            note(
                sink.as_ref(),
                NoteLevel::Warn,
                refused_while_stopped(&what, &message),
            );
        }
    }
}

fn refused_while_stopped(what: &str, message: &str) -> String {
    format!("{what}, sent while the run was stopped, was refused: {message}.")
}

/// A pending request, named for a note.
fn describe(request: &ControlRequest) -> String {
    match request {
        ControlRequest::Answer { answer, .. } => format!("Your answer to {}", answer.id),
        ControlRequest::Reassign { task, to, .. } => format!("Moving {task} to {to}"),
        ControlRequest::Guidance { .. } => "Your guidance".to_owned(),
        ControlRequest::Mention { .. } => "Your message".to_owned(),
        ControlRequest::ApprovalMode { .. } => "The approval mode change".to_owned(),
        ControlRequest::TakeOverEnded { task, .. } => {
            format!("The end of your take-over of {task}")
        }
        ControlRequest::TakeBack { .. } => "Taking your messages back".to_owned(),
        ControlRequest::Stop => "Stopping the run".to_owned(),
        ControlRequest::StopSession {
            session,
            again: false,
        } => format!("Stopping {session}"),
        ControlRequest::StopSession { session, .. } => format!("Starting {session} over"),
        ControlRequest::SpotCheck { id, .. } => format!("Your spot check of {id}"),
        ControlRequest::Goal { .. } => "Your change to the goal".to_owned(),
        _ => "A request".to_owned(),
    }
}

/// A note in the run's log, or in the run host log when even that fails.
fn note(sink: &JournalSink, level: NoteLevel, text: String) {
    let kind = RunEventKind::Note(Note {
        level,
        text: text.clone(),
    });
    if let Err(err) = sink.record(None, None, SOURCE, kind) {
        tracing::error!("{text} (and recording that failed: {err:#})");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    use argonauts_core::TaskId;
    use argonauts_core::api::{DiffScope, StartRun};
    use argonauts_core::control::ControlRequest;
    use argonauts_core::future::BoxFuture;
    use argonauts_core::input::MentionTarget;
    use argonauts_core::journal::read_all;
    use argonauts_core::names::{ModelRef, RoleName, SessionKey};
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_testkit::TempRepo;

    use super::server::tests::{Client, Log, TOKEN, answer, answer_command, command_name};
    use super::*;

    type Script = Arc<dyn Fn(u32, EngineContext) -> BoxFuture<'static, EngineExit> + Send + Sync>;

    struct ScriptedEngine {
        round: u32,
        script: Script,
    }

    impl RunEngine for ScriptedEngine {
        fn run(self: Box<Self>, context: EngineContext) -> BoxFuture<'static, EngineExit> {
            (self.script)(self.round, context)
        }
    }

    struct TestRun {
        repo: TempRepo,
        _home: tempfile::TempDir,
        paths: Paths,
        run: RunId,
        run_dir: PathBuf,
        rounds: Arc<AtomicU32>,
    }

    impl TestRun {
        /// A run as `start` leaves it, in a home as long as the system's temp folder gives.
        fn new() -> Self {
            let repo = TempRepo::new().unwrap();
            let home = tempfile::tempdir().unwrap();
            let paths = Paths::new(home.path());
            let env = AppEnv::for_repo(paths.clone(), repo.root(), false).unwrap();
            let run = RunId::new("run-20260930-010000-00a1").unwrap();
            let run_dir = env.run_dir(&run);
            std::fs::create_dir_all(&run_dir).unwrap();
            let start = StartRecord {
                run: run.clone(),
                request: StartRun {
                    request: "tidy the readme\nand nothing else".to_owned(),
                    mode: RunMode::Captain,
                    forced: true,
                    estimate: None,
                    captain_or_crew: None,
                    conversation_turn: None,
                    continues: None,
                    overrides: Vec::new(),
                    include_uncommitted: false,
                    main_agent: None,
                    approval_mode: None,
                    outside_agent: None,
                    instructions: Vec::new(),
                    request_settings: Vec::new(),
                    required_steps: Vec::new(),
                },
                overrides: Vec::new(),
                interactive: true,
                replay: false,
                repo_root: env.repo_root.clone(),
                created: OffsetDateTime::now_utc(),
            };
            files::write_start(&run_dir, &start).unwrap();
            let settings = SettingsRecord {
                sha256: "0".repeat(64),
                settings: Settings::default(),
                turned_on: Vec::new(),
            };
            files::write_settings(&run_dir, &settings).unwrap();
            files::write_token(&run_dir, TOKEN).unwrap();
            Self {
                repo,
                _home: home,
                paths,
                run,
                run_dir,
                rounds: Arc::new(AtomicU32::new(0)),
            }
        }

        fn options(&self, grace: Duration, script: Script) -> HostOptions {
            let rounds = self.rounds.clone();
            HostOptions {
                paths: self.paths.clone(),
                engine: Arc::new(move |_: &Settings, _: RunMode| {
                    let round = rounds.fetch_add(1, Ordering::SeqCst);
                    Box::new(ScriptedEngine {
                        round,
                        script: script.clone(),
                    }) as Box<dyn RunEngine>
                }),
                wait_grace: Some(grace),
                check_ancestry: false,
                shutdown: CancellationToken::new(),
                keep_awake: false,
            }
        }

        /// A child that another test is spawning holds a copy of every descriptor until its
        /// exec closes them, so a lock let go a moment ago can read as held for a little while.
        async fn lock_released(&self) -> bool {
            let deadline = Instant::now() + Duration::from_secs(5);
            while RunLock::is_held(&self.run_dir) {
                if Instant::now() > deadline {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            true
        }

        async fn host(&self, options: HostOptions) -> HostExit {
            host_with(self.repo.root(), &self.run, options)
                .await
                .unwrap()
        }

        /// Hosts the run once the last run host's lock is free. A child that another test forks
        /// can hold a copy of the lock until its exec, even a copy taken only to check it.
        async fn host_again(&self, grace: Duration, script: Script) -> HostExit {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let exit = self.host(self.options(grace, script.clone())).await;
                if exit != HostExit::AlreadyHosted || Instant::now() > deadline {
                    return exit;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        fn events(&self) -> Vec<RunEvent> {
            read_all(&self.run_dir.join(run_files::EVENTS))
                .unwrap()
                .events
        }

        async fn connect(&self) -> Client {
            let socket = files::socket_path(&self.run_dir);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !socket.exists() {
                assert!(Instant::now() < deadline, "the socket never appeared");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Client::connect(&files::socket_address(&self.run_dir).unwrap()).await
        }
    }

    fn finish(outcome: RunOutcome) -> Script {
        Arc::new(move |_, _| Box::pin(async move { EngineExit::Finished(outcome) }))
    }

    fn wait() -> Script {
        Arc::new(|_, _| Box::pin(async { EngineExit::Waiting(WaitReason::Paused) }))
    }

    fn started_count(events: &[RunEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Started(_))))
            .count()
    }

    fn with_token(request: ControlRequest) -> Vec<ControlRequest> {
        vec![request]
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_pending_frame_kind_is_applied_first_then_removed() {
        let test = TestRun::new();
        let task = TaskId::new("task-001").unwrap();
        let session = SessionKey::numbered(1);
        let mut pending = vec![
            answer(TOKEN),
            answer("not the token"),
            ControlRequest::Guidance {
                text: "prefer small commits".to_owned(),
                outside_agent: None,
            },
            ControlRequest::Mention {
                target: MentionTarget::Task(task.clone()),
                text: "check the edge case".to_owned(),
            },
            ControlRequest::Reassign {
                task: task.clone(),
                to: ModelRef::new("codex", None),
                rule_override: None,
                token: None,
            },
            ControlRequest::Reassign {
                task: task.clone(),
                to: ModelRef::new("codex", None),
                rule_override: Some("review.other_provider".to_owned()),
                token: Some(TOKEN.to_owned()),
            },
            ControlRequest::ApprovalMode {
                mode: ApprovalMode::Auto,
                token: TOKEN.to_owned(),
            },
            ControlRequest::TakeOver {
                task: task.clone(),
                token: TOKEN.to_owned(),
            },
            ControlRequest::TakeOverEnded {
                task: task.clone(),
                token: TOKEN.to_owned(),
            },
            ControlRequest::TakeBack {
                messages: Vec::new(),
            },
            ControlRequest::Pause {
                now: false,
                outside_agent: None,
            },
            ControlRequest::CancelPause,
            ControlRequest::StopSession {
                session,
                again: false,
            },
            ControlRequest::Steer {
                session: SessionKey::numbered(2),
                text: "look again".to_owned(),
                now: false,
            },
            ControlRequest::Status,
            ControlRequest::Diff {
                task: task.clone(),
                scope: DiffScope::Task,
            },
            ControlRequest::Raw {
                session: SessionKey::numbered(1),
            },
        ];
        pending.extend(with_token(ControlRequest::Stop));
        for request in &pending {
            files::append_pending(&test.run_dir, request).unwrap();
        }

        let log = Log::default();
        let history = Arc::new(Mutex::new(Vec::new()));
        let script: Script = {
            let log = log.clone();
            let history = history.clone();
            Arc::new(move |_, mut context: EngineContext| {
                let log = log.clone();
                *history.lock().unwrap() = context.history.clone();
                Box::pin(async move {
                    while let Some(command) = context.inbox.recv().await {
                        let last = command_name(&command) == "stop";
                        answer_command(command, &log);
                        if last {
                            break;
                        }
                    }
                    EngineExit::Finished(RunOutcome::Merged)
                })
            })
        };
        let exit = test
            .host(test.options(Duration::from_millis(50), script))
            .await;
        assert_eq!(exit, HostExit::Finished(RunOutcome::Merged));

        assert_eq!(
            *log.lock().unwrap(),
            [
                "answer",
                "guidance",
                "mention",
                "reassign",
                "reassign",
                "approval_mode",
                "take_over",
                "take_over_ended",
                "pause",
                "cancel_pause",
                "stop_session",
                "steer",
                "status",
                "diff",
                "raw",
                "stop",
            ],
            "in order, before anything else, without the one whose token was wrong or the \
             take-back"
        );
        assert!(!test.run_dir.join(run_files::PENDING_ANSWERS).exists());

        let history = history.lock().unwrap().clone();
        assert_eq!(history.len(), 1, "a new run's history is its RunStarted");
        let RunEventKind::Run(RunLifecycle::Started(started)) = &history[0].kind else {
            panic!("{:?}", history[0].kind);
        };
        assert_eq!(started.title, "tidy the readme");
        assert_eq!(started.base_commit, test.repo.head().unwrap());
        assert_eq!(started.base_branch.as_deref(), Some("main"));

        let events = test.events();
        assert_eq!(started_count(&events), 1);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            RunEventKind::Note(note) if note.text.contains("token")
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            RunEventKind::Note(note) if note.text.contains("take-back")
        )));
        let env = AppEnv::for_repo(test.paths.clone(), test.repo.root(), false).unwrap();
        let base = test
            .paths
            .home()
            .join("worktrees")
            .join(env.repo_id.as_str())
            .join(test.run.as_str())
            .join("base");
        assert!(base.join("README.md").is_file(), "the base worktree exists");
        assert!(!files::socket_path(&test.run_dir).exists());
        assert!(!test.run_dir.join(run_files::PID).exists());
        assert!(live_registry::live_run_hosts(&test.paths).is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_requests_outlive_a_crash_at_start_and_bad_lines_are_skipped() {
        use std::io::Write as _;

        let test = TestRun::new();
        let guidance = ControlRequest::Guidance {
            text: "use the v2 API".to_owned(),
            outside_agent: None,
        };
        files::append_pending(&test.run_dir, &guidance).unwrap();
        let path = test.run_dir.join(run_files::PENDING_ANSWERS);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"type\":\"from_a_later_version\"}\n")
            .unwrap();
        // A folder where the socket goes makes binding it fail.
        let socket = files::socket_path(&test.run_dir);
        std::fs::create_dir_all(socket.join("in-the-way")).unwrap();

        let exit = test
            .host(test.options(Duration::ZERO, finish(RunOutcome::Merged)))
            .await;
        assert!(matches!(exit, HostExit::Crashed(_)), "{exit:?}");
        assert_eq!(test.rounds.load(Ordering::SeqCst), 0);
        assert!(path.is_file(), "the crash kept the pending requests");

        std::fs::remove_dir_all(&socket).unwrap();
        let log = Log::default();
        let script: Script = {
            let log = log.clone();
            Arc::new(move |_, mut context: EngineContext| {
                let log = log.clone();
                Box::pin(async move {
                    if let Some(command) = context.inbox.recv().await {
                        answer_command(command, &log);
                    }
                    EngineExit::Finished(RunOutcome::Merged)
                })
            })
        };
        let exit = test.host_again(Duration::from_millis(50), script).await;
        assert_eq!(exit, HostExit::Finished(RunOutcome::Merged));
        assert_eq!(*log.lock().unwrap(), ["guidance"]);
        assert!(test.events().iter().any(|event| matches!(
            &event.kind,
            RunEventKind::Note(note)
                if note.level == NoteLevel::Warn && note.text.contains("line 2 of")
        )));
        let left: Vec<_> = std::fs::read_dir(&test.run_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("pending-answers"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_run_host_returns_already_hosted() {
        let test = TestRun::new();
        let held = RunLock::try_acquire(&test.run_dir).unwrap().unwrap();
        let exit = test
            .host(test.options(Duration::ZERO, finish(RunOutcome::Merged)))
            .await;
        assert_eq!(exit, HostExit::AlreadyHosted);
        assert_eq!(test.rounds.load(Ordering::SeqCst), 0);
        assert!(!test.run_dir.join(run_files::EVENTS).exists());
        drop(held);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waits_and_exits_after_the_grace_then_resumes_from_the_log() {
        let test = TestRun::new();
        let began = Instant::now();
        let exit = test
            .host(test.options(Duration::from_millis(200), wait()))
            .await;
        assert_eq!(exit, HostExit::Waiting(WaitReason::Paused));
        assert!(began.elapsed() >= Duration::from_millis(200));
        assert_eq!(test.rounds.load(Ordering::SeqCst), 1);
        assert!(test.lock_released().await, "the lock is released");
        assert!(!files::socket_path(&test.run_dir).exists());

        let seen = Arc::new(Mutex::new(0));
        let script: Script = {
            let seen = seen.clone();
            Arc::new(move |_, context: EngineContext| {
                *seen.lock().unwrap() = context.history.len();
                Box::pin(async { EngineExit::Finished(RunOutcome::Stopped) })
            })
        };
        let exit = test
            .host(test.options(Duration::from_secs(10), script))
            .await;
        assert_eq!(exit, HostExit::Finished(RunOutcome::Stopped));
        assert_eq!(
            started_count(&test.events()),
            1,
            "a resume writes no second RunStarted"
        );
        assert!(*seen.lock().unwrap() >= 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_outside_agent_run_pauses_once_its_time_limit_is_spent() {
        let test = TestRun::new();
        let mut start = files::read_start(&test.run_dir).unwrap();
        start.request.outside_agent = Some("other-tool".to_owned());
        files::write_start(&test.run_dir, &start).unwrap();
        let mut recorded = files::read_settings(&test.run_dir).unwrap();
        recorded.settings.mcp.run_limit = argonauts_core::settings::Span::new("1s");
        files::write_settings(&test.run_dir, &recorded).unwrap();
        let script: Script = Arc::new(|_, mut context: EngineContext| {
            Box::pin(async move {
                match context.inbox.recv().await {
                    Some(argonauts_core::engine::EngineCommand::Pause { now: false }) => {
                        EngineExit::Waiting(WaitReason::Paused)
                    }
                    _ => EngineExit::Crashed("expected a pause".to_owned()),
                }
            })
        });
        let began = Instant::now();
        let exit = test
            .host(test.options(Duration::from_millis(50), script))
            .await;
        assert_eq!(exit, HostExit::Waiting(WaitReason::Paused));
        assert!(began.elapsed() >= Duration::from_secs(1));
        let notes = notes(&test.events());
        assert!(
            notes
                .iter()
                .any(|note| note.text.contains("time limit of 1s is spent")),
            "{notes:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_inside_the_grace_restarts_the_engine() {
        let test = TestRun::new();
        let log = Log::default();
        let script: Script = {
            let log = log.clone();
            Arc::new(move |round, mut context: EngineContext| {
                let log = log.clone();
                Box::pin(async move {
                    if round == 0 {
                        return EngineExit::Waiting(WaitReason::Approval);
                    }
                    if let Some(command) = context.inbox.recv().await {
                        answer_command(command, &log);
                    }
                    EngineExit::Finished(RunOutcome::Merged)
                })
            })
        };
        let options = test.options(Duration::from_secs(30), script);
        let began = Instant::now();
        let (exit, reply) = tokio::join!(test.host(options), async {
            let mut client = test.connect().await;
            client.call(2, answer(TOKEN)).await
        });
        assert_eq!(reply, ControlReply::Ok, "the restarted engine answered");
        assert_eq!(exit, HostExit::Finished(RunOutcome::Merged));
        assert_eq!(test.rounds.load(Ordering::SeqCst), 2);
        assert_eq!(*log.lock().unwrap(), ["answer"]);
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "it didn't wait out the grace"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_without_waiting_out_the_grace() {
        let test = TestRun::new();
        let script: Script = Arc::new(|_, context: EngineContext| {
            Box::pin(async move {
                context.shutdown.cancelled().await;
                EngineExit::Waiting(WaitReason::Paused)
            })
        });
        let options = test.options(Duration::from_secs(30), script);
        let shutdown = options.shutdown.clone();
        let began = Instant::now();
        let (exit, ()) = tokio::join!(test.host(options), async {
            test.connect().await;
            shutdown.cancel();
        });
        assert_eq!(exit, HostExit::Waiting(WaitReason::Paused));
        assert!(began.elapsed() < Duration::from_secs(30));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_crashed_or_panicking_engine_is_a_crash() {
        let test = TestRun::new();
        let crashed: Script =
            Arc::new(|_, _| Box::pin(async { EngineExit::Crashed("the disk is full".to_owned()) }));
        let exit = test.host(test.options(Duration::ZERO, crashed)).await;
        assert_eq!(exit, HostExit::Crashed("the disk is full".to_owned()));

        let test = TestRun::new();
        let panicked: Script = Arc::new(|_, _| Box::pin(async { panic!("engine bug") }));
        let exit = test.host(test.options(Duration::ZERO, panicked)).await;
        assert!(
            matches!(&exit, HostExit::Crashed(reason) if reason.contains("engine bug")),
            "{exit:?}"
        );
        assert!(
            notes(&test.events())
                .iter()
                .any(|note| note.level == NoteLevel::Error && note.text.contains("engine bug")),
            "the panic is in the run's log"
        );
    }

    fn notes(events: &[RunEvent]) -> Vec<Note> {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Note(note) => Some(note.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pending_answer_the_engine_refuses_is_noted() {
        let test = TestRun::new();
        files::append_pending(&test.run_dir, &answer(TOKEN)).unwrap();
        let script: Script = Arc::new(|_, mut context: EngineContext| {
            Box::pin(async move {
                if let Some(argonauts_core::engine::EngineCommand::Answer { reply, .. }) =
                    context.inbox.recv().await
                {
                    let _ = reply.send(Err("the evidence changed".to_owned()));
                }
                EngineExit::Finished(RunOutcome::Stopped)
            })
        });
        let exit = test
            .host(test.options(Duration::from_millis(50), script))
            .await;
        assert_eq!(exit, HostExit::Finished(RunOutcome::Stopped));
        let notes = notes(&test.events());
        assert!(
            notes.iter().any(|note| note.level == NoteLevel::Warn
                && note.text.contains("answer to approval-001")
                && note.text.contains("the evidence changed")),
            "{notes:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_no_engine_took_is_kept_for_the_next_run_host() {
        let test = TestRun::new();
        let mode = ControlRequest::ApprovalMode {
            mode: ApprovalMode::Supervised,
            token: TOKEN.to_owned(),
        };
        files::append_pending(&test.run_dir, &mode).unwrap();
        let exit = test
            .host(test.options(Duration::from_millis(50), wait()))
            .await;
        assert_eq!(exit, HostExit::Waiting(WaitReason::Paused));
        assert_eq!(
            test.rounds.load(Ordering::SeqCst),
            2,
            "one more engine gets the chance to take it, and no more"
        );
        assert_eq!(files::take_pending(&test.run_dir).unwrap(), [mode]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pending_cancel_pause_asks_only_the_first_engine_to_reopen() {
        let test = TestRun::new();
        files::append_pending(&test.run_dir, &ControlRequest::CancelPause).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let script: Script = {
            let seen = seen.clone();
            Arc::new(move |round, mut context| {
                let seen = seen.clone();
                Box::pin(async move {
                    seen.lock().unwrap().push(context.reopen);
                    // The first waits without taking the request, so the next one gets it.
                    if round == 0 {
                        return EngineExit::Waiting(WaitReason::Paused);
                    }
                    let _ = context.inbox.recv().await;
                    EngineExit::Finished(RunOutcome::Stopped)
                })
            })
        };
        let exit = test
            .host(test.options(Duration::from_millis(50), script))
            .await;
        assert_eq!(exit, HostExit::Finished(RunOutcome::Stopped));
        assert_eq!(*seen.lock().unwrap(), [true, false]);
        assert!(files::take_pending(&test.run_dir).unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_without_start_json_is_an_error() {
        let test = TestRun::new();
        std::fs::remove_file(test.run_dir.join(run_files::START)).unwrap();
        let options = test.options(Duration::ZERO, finish(RunOutcome::Merged));
        assert!(
            host_with(test.repo.root(), &test.run, options)
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_finished_run_goes_to_the_usage_ledger_by_agent() {
        use crate::client::test_support::{session_started, usage};
        let test = TestRun::new();
        let script: Script = Arc::new(move |_, context: EngineContext| {
            Box::pin(async move {
                let key = SessionKey::numbered(1);
                let started = session_started(0, &key, None, RoleName::Worker, "codex");
                let spent = usage(1, &key, 2000, 100, 0.25);
                for event in [started, spent] {
                    context
                        .sink
                        .record(None, Some(&key), "test", event.kind)
                        .unwrap();
                }
                EngineExit::Finished(RunOutcome::Merged)
            })
        });
        let options = test.options(Duration::ZERO, script);
        assert_eq!(
            test.host(options).await,
            HostExit::Finished(RunOutcome::Merged)
        );
        let lines = UsageLedger::new(&test.paths).read_since(None);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].run.as_ref(), Some(&test.run));
        assert_eq!(lines[0].agent, "codex");
        assert!((lines[0].cost - 0.25).abs() < 1e-9);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_subscriber_gets_the_last_event_before_the_run_host_exits() {
        let test = TestRun::new();
        let subscribed = Arc::new(tokio::sync::Notify::new());
        let script: Script = {
            let subscribed = subscribed.clone();
            Arc::new(move |_, context: EngineContext| {
                let subscribed = subscribed.clone();
                Box::pin(async move {
                    subscribed.notified().await;
                    let finished = RunEventKind::Run(RunLifecycle::Finished {
                        outcome: RunOutcome::Merged,
                        detail: None,
                    });
                    context.sink.record(None, None, "test", finished).unwrap();
                    EngineExit::Finished(RunOutcome::Merged)
                })
            })
        };
        let options = test.options(Duration::from_secs(10), script);
        let (exit, last) = tokio::join!(test.host(options), async {
            let mut client = test.connect().await;
            let subscribe = ControlRequest::Subscribe {
                from_seq: 0,
                from_snapshot: false,
            };
            client.send(2, subscribe).await;
            loop {
                let ControlReply::Event { event } = client.next().await.body else {
                    continue;
                };
                // The backlog came, so the subscription is live.
                subscribed.notify_one();
                if matches!(event.kind, RunEventKind::Run(RunLifecycle::Finished { .. })) {
                    return event.seq;
                }
            }
        });
        assert_eq!(exit, HostExit::Finished(RunOutcome::Merged));
        assert_eq!(Some(last), test.events().last().map(|event| event.seq));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_start_keeps_pending_requests_for_the_next_run_host() {
        let test = TestRun::new();
        let guidance = ControlRequest::Guidance {
            text: "prefer small commits".to_owned(),
            outside_agent: None,
        };
        files::append_pending(&test.run_dir, &guidance).unwrap();
        files::append_pending(&test.run_dir, &ControlRequest::Stop).unwrap();
        let env = AppEnv::for_repo(test.paths.clone(), test.repo.root(), false).unwrap();
        // A file where the run's worktrees go makes the base worktree fail.
        let worktrees = test
            .paths
            .home()
            .join("worktrees")
            .join(env.repo_id.as_str())
            .join(test.run.as_str());
        std::fs::create_dir_all(worktrees.parent().unwrap()).unwrap();
        std::fs::write(&worktrees, "in the way").unwrap();

        let exit = test
            .host(test.options(Duration::ZERO, finish(RunOutcome::Merged)))
            .await;
        assert!(matches!(exit, HostExit::Crashed(_)), "{exit:?}");
        assert_eq!(test.rounds.load(Ordering::SeqCst), 0);
        // Set aside and left there, as a run host that crashed right then would leave them.
        let kept = files::set_aside_pending(&test.run_dir).unwrap();
        assert_eq!(kept.requests, vec![guidance.clone(), ControlRequest::Stop]);
        drop(kept);
        assert!(test.lock_released().await);

        std::fs::remove_file(&worktrees).unwrap();
        let log = Log::default();
        let script: Script = {
            let log = log.clone();
            Arc::new(move |_, mut context: EngineContext| {
                let log = log.clone();
                Box::pin(async move {
                    while let Some(command) = context.inbox.recv().await {
                        let last = command_name(&command) == "stop";
                        answer_command(command, &log);
                        if last {
                            break;
                        }
                    }
                    EngineExit::Finished(RunOutcome::Stopped)
                })
            })
        };
        let exit = test
            .host(test.options(Duration::from_millis(50), script))
            .await;
        assert_eq!(exit, HostExit::Finished(RunOutcome::Stopped));
        assert_eq!(*log.lock().unwrap(), ["guidance", "stop"]);
        assert!(files::take_pending(&test.run_dir).unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_socket_answers_while_the_start_is_still_slow() {
        use argonauts_core::agents::AgentGroup;

        use crate::ancestry::tests::{child_of, spawn_group};

        let test = TestRun::new();
        // An orphan that ignores SIGTERM holds the start up for the whole grace.
        let mut orphan = spawn_group("trap '' TERM; sleep 30; true", &[]);
        child_of(orphan.id());
        let group = AgentGroup {
            pgid: orphan.id(),
            pid: orphan.id(),
            start_time: ancestry::Processes::new().start_time(orphan.id()).unwrap(),
        };
        let session = argonauts_core::layout::session_dir(&test.run_dir, &SessionKey::numbered(1));
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join(argonauts_core::layout::session_files::PROCESS),
            serde_json::to_vec(&group).unwrap(),
        )
        .unwrap();

        let began = Instant::now();
        let options = test.options(Duration::ZERO, finish(RunOutcome::Merged));
        // Its own task, since the host blocks its own while the start runs.
        let socket = files::socket_path(&test.run_dir);
        let address = files::socket_address(&test.run_dir).unwrap();
        let welcomed = tokio::spawn(async move {
            while !socket.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let _client = Client::connect(&address).await;
            began.elapsed()
        });
        let exit = test.host(options).await;
        let welcomed = welcomed.await.unwrap();
        assert_eq!(exit, HostExit::Finished(RunOutcome::Merged));
        assert!(
            welcomed < Duration::from_millis(1500),
            "welcomed after {welcomed:?}, not after the orphan's grace"
        );
        assert!(
            began.elapsed() >= Duration::from_secs(2),
            "the orphan got its grace"
        );
        assert!(orphan.wait().unwrap().code().is_none());
    }
}
