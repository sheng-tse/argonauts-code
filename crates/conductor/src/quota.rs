//! The quota board: the latest reading per vendor, in `vendors.json`, so the app's status line
//! and every supervisor agree. Probes never spend: agy's free `/usage`, and Codex's rate-limit
//! read over `codex app-server`. Claude Code needs none, since its stream reports quota.
//!
//! Probes run at app start, after each session of a vendor that has one, and at most once every
//! [`PROBE_EVERY`] while such a session runs. Every process claims a probe in `vendors.json`
//! before running it, so processes take turns.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{QuotaAnswer, QuotaBoard, QuotaReading};
use argonauts_core::names::VendorId;
use argonauts_core::{
    Adapter, CommandSpec, PARENT_SESSION_ENV_VARS, Paths, create_private_dir_all,
    write_private_file,
};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::time::MissedTickBehavior;

use crate::process::{SpawnError, resolve_program};
use crate::vendors_file::VendorsStore;

/// A probe that hasn't answered by then reads as unknown.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// While sessions run, a vendor is probed at most this often, across every process.
pub const PROBE_EVERY: Duration = Duration::from_secs(60);

/// How to read a vendor's quota without spending any: its adapter's `quota_probe`, run with the
/// binary your settings name.
#[derive(Clone)]
pub struct Prober {
    pub adapter: Arc<dyn Adapter>,
    /// From `agents.<id>.binary`, else the adapter's binary on `PATH`, found with
    /// [`find_unsandboxed`].
    pub binary: PathBuf,
    /// Where each probe gets a folder of its own to run in. A probe reads no repo.
    pub workdir: PathBuf,
    /// The model the vendor's sessions use, for a CLI that names one on every call.
    pub model: Option<String>,
}

static PROBERS: Mutex<BTreeMap<String, Prober>> = Mutex::new(BTreeMap::new());

/// Why a binary that would run outside the sandbox isn't used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BinaryError {
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("refusing to run {}, since it's inside {}", path.display(), dir.display())]
    Inside { path: PathBuf, dir: PathBuf },
}

/// Finds a binary that runs outside the sandbox with your environment, such as a quota probe or
/// one of doctor's checks. As for a session, a name is looked up on absolute `PATH` entries only
/// and a path must be absolute. One inside `workdir`, the repo you run from, or Argonauts' home,
/// where agents write, is refused, so neither a repo nor an agent can plant it.
pub fn find_unsandboxed(program: &str, workdir: Option<&Path>) -> Result<PathBuf, BinaryError> {
    let cwd = std::env::current_dir().ok();
    let refused: Vec<PathBuf> = workdir
        .map(Path::to_path_buf)
        .into_iter()
        .chain(cwd.as_deref().and_then(repo_around))
        .chain(
            Paths::from_env()
                .ok()
                .map(|paths| paths.home().to_path_buf()),
        )
        .collect();
    find_outside(program, std::env::var_os("PATH").as_deref(), &refused)
}

/// The nearest folder holding `dir` with a `.git`, which a linked worktree has as a file.
fn repo_around(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

fn find_outside(
    program: &str,
    path_var: Option<&OsStr>,
    refused: &[PathBuf],
) -> Result<PathBuf, BinaryError> {
    let not_installed = || BinaryError::NotInstalled(program.to_owned());
    // Nothing to refuse means no home either, and Argonauts can't run without one.
    let (first, rest) = refused.split_first().ok_or_else(not_installed)?;
    let found =
        resolve_program(program, path_var, first).map_err(|err| refusal(err, program, first))?;
    // A path that isn't UTF-8 can't be checked against the other folders.
    let absolute = found.to_str().ok_or_else(not_installed)?;
    for dir in rest {
        // `absolute` has a slash, so this only asks whether it's inside `dir`.
        resolve_program(absolute, None, dir).map_err(|err| refusal(err, program, dir))?;
    }
    if found.is_file() {
        Ok(found)
    } else {
        Err(not_installed())
    }
}

/// `resolve_program` only fails to find `program` or finds it inside `dir`.
fn refusal(err: SpawnError, program: &str, dir: &Path) -> BinaryError {
    match err {
        SpawnError::InsideWorkdir { path, .. } => BinaryError::Inside {
            path,
            dir: dir.to_owned(),
        },
        _ => BinaryError::NotInstalled(program.to_owned()),
    }
}

/// Each harness worker registers its adapter, so a probe at app start finds the binary the
/// settings name. The last registration for a vendor wins.
pub fn register_prober(vendor: &VendorId, prober: Prober) {
    if let Ok(mut probers) = PROBERS.lock() {
        probers.insert(vendor.0.clone(), prober);
    }
}

pub fn registered_prober(vendor: &VendorId) -> Option<Prober> {
    PROBERS.lock().ok()?.get(&vendor.0).cloned()
}

/// Vendors whose registered adapter has a free quota probe.
fn probed_vendors() -> Vec<VendorId> {
    let Ok(probers) = PROBERS.lock() else {
        return Vec::new();
    };
    probers
        .iter()
        .filter(|(_, prober)| has_quota_probe(prober.adapter.as_ref()))
        .map(|(vendor, _)| VendorId(vendor.clone()))
        .collect()
}

pub fn has_quota_probe(adapter: &dyn Adapter) -> bool {
    adapter.quota_probe(&std::env::temp_dir(), None).is_some()
}

/// Why a probe gave no reading, so `doctor` can say why the chip shows `?`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("{0} has no free way to read its plan usage")]
    NoProbe(String),
    #[error("could not run {program}: {detail}")]
    Run { program: String, detail: String },
    #[error("{program} gave no answer within {seconds}s")]
    TimedOut { program: String, seconds: u64 },
    #[error("{program} said nothing about plan usage ({status})")]
    Unreadable { program: String, status: String },
}

/// Runs a probe command in its own process group, with [`PROBE_TIMEOUT`], and parses its
/// output. `None` when it can't run or says nothing useful.
pub async fn run_probe(prober: &Prober) -> Option<QuotaReading> {
    run_probe_within(prober, PROBE_TIMEOUT).await
}

/// Like [`run_probe`], with another timeout.
pub async fn run_probe_within(prober: &Prober, timeout: Duration) -> Option<QuotaReading> {
    run_probe_answer(prober, timeout)
        .await
        .and_then(|answer| answer.binding().cloned())
}

/// Every window the probe read, and the plan.
async fn run_probe_answer(prober: &Prober, timeout: Duration) -> Option<QuotaAnswer> {
    match try_probe_answer(prober, timeout).await {
        Ok(answer) => Some(answer),
        Err(err) => {
            tracing::warn!("plan usage probe: {err}");
            None
        }
    }
}

/// Runs the probe and says why when there's no reading. Gives the window that binds soonest.
pub async fn try_probe(prober: &Prober, timeout: Duration) -> Result<QuotaReading, ProbeError> {
    let program = prober.adapter.binary().to_owned();
    let answer = try_probe_answer(prober, timeout).await?;
    answer.binding().cloned().ok_or(ProbeError::Unreadable {
        program,
        status: "it reported no window".to_owned(),
    })
}

/// Runs the probe and says why when there's no answer. The whole process group is killed
/// once the adapter can read an answer, or at the timeout, so an app server never lingers.
pub async fn try_probe_answer(
    prober: &Prober,
    timeout: Duration,
) -> Result<QuotaAnswer, ProbeError> {
    let adapter = prober.adapter.as_ref();
    let folder = ProbeFolder::new(&prober.workdir).map_err(|detail| ProbeError::Run {
        program: adapter.binary().to_owned(),
        detail,
    })?;
    let spec = adapter
        .quota_probe(&folder.0, prober.model.as_deref())
        .ok_or_else(|| ProbeError::NoProbe(adapter.display_name().to_owned()))?;
    let wanted = if spec.program == adapter.binary() {
        prober.binary.to_string_lossy().into_owned()
    } else {
        spec.program.clone()
    };
    let program =
        find_unsandboxed(&wanted, Some(&prober.workdir)).map_err(|err| ProbeError::Run {
            program: wanted.clone(),
            detail: err.to_string(),
        })?;
    let name = program.display().to_string();
    let run_error = |detail: String| ProbeError::Run {
        program: name.clone(),
        detail,
    };
    write_spec_files(&spec).map_err(&run_error)?;

    let mut command = Command::new(&program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .envs(spec.env_set.iter().map(|(k, v)| (k, v)))
        // A CLI that works in `$PWD`, as OpenCode 1.18.34 does, would otherwise probe from
        // wherever you started Argonauts.
        .env("PWD", &spec.cwd)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    for var in spec
        .env_remove
        .iter()
        .map(String::as_str)
        .chain(PARENT_SESSION_ENV_VARS.iter().copied())
    {
        command.env_remove(var);
    }
    let mut child = command.spawn().map_err(|err| run_error(err.to_string()))?;
    let group = child.id();

    let answer = tokio::time::timeout(timeout, exchange(&mut child, &spec, adapter)).await;
    if let Some(group) = group.and_then(|id| i32::try_from(id).ok()) {
        // The group may be gone already, which is what we want.
        let _ = killpg(Pid::from_raw(group), Signal::SIGKILL);
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;

    match answer {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(status)) => Err(ProbeError::Unreadable {
            program: name,
            status,
        }),
        Err(_) => Err(ProbeError::TimedOut {
            program: name,
            seconds: timeout.as_secs_f64().ceil() as u64,
        }),
    }
}

/// A fresh private folder for one probe, so what a CLI leaves in its working folder never
/// reaches the next probe or another program. Removed when dropped.
struct ProbeFolder(PathBuf);

impl ProbeFolder {
    fn new(parent: &Path) -> Result<Self, String> {
        create_private_dir_all(parent).map_err(|err| describe(parent, err))?;
        let path = parent.join(format!("argonauts-probe-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|err| describe(&path, err))?;
        Ok(Self(path))
    }
}

impl Drop for ProbeFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Sends the probe's stdin, then reads stdout until the adapter can parse it. The error is the
/// exit status, for the message.
async fn exchange(
    child: &mut Child,
    spec: &CommandSpec,
    adapter: &dyn Adapter,
) -> Result<QuotaAnswer, String> {
    // An app server answers only while its stdin stays open, so it's held until the end.
    let mut input_pipe = child.stdin.take();
    if let (Some(stdin), Some(input)) = (input_pipe.as_mut(), &spec.stdin) {
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|err| format!("could not write its input: {err}"))?;
        stdin.flush().await.map_err(|err| err.to_string())?;
    }
    if !spec.keep_stdin_open {
        drop(input_pipe.take());
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "its output was not captured".to_owned())?;
    let mut lines = BufReader::new(stdout).lines();
    let mut text = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        text.push_str(&line);
        text.push('\n');
        if let Some(answer) = adapter.parse_quota_probe_answer(&text) {
            return Ok(answer);
        }
    }
    let status = child
        .wait()
        .await
        .map_or_else(|err| err.to_string(), |status| status.to_string());
    adapter.parse_quota_probe_answer(&text).ok_or(status)
}

fn write_spec_files(spec: &CommandSpec) -> Result<(), String> {
    for file in &spec.files {
        let path = if file.path.is_absolute() {
            file.path.clone()
        } else {
            spec.cwd.join(&file.path)
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_private_dir_all(parent).map_err(|err| describe(parent, err))?;
        }
        write_private_file(&path, file.contents.as_bytes()).map_err(|err| describe(&path, err))?;
    }
    Ok(())
}

fn describe(path: &Path, err: std::io::Error) -> String {
    format!("{}: {err}", path.display())
}

pub struct FileQuotaBoard {
    store: VendorsStore,
    probers: BTreeMap<String, Prober>,
    probe_timeout: Duration,
}

impl FileQuotaBoard {
    pub fn new(paths: &Paths) -> Self {
        Self {
            store: VendorsStore::new(paths),
            probers: BTreeMap::new(),
            probe_timeout: PROBE_TIMEOUT,
        }
    }

    /// Uses this prober for `vendor` instead of the registered one, as tests do.
    pub fn with_prober(mut self, vendor: &VendorId, prober: Prober) -> Self {
        self.probers.insert(vendor.0.clone(), prober);
        self
    }

    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    fn prober(&self, vendor: &VendorId) -> Option<Prober> {
        self.probers
            .get(&vendor.0)
            .cloned()
            .or_else(|| registered_prober(vendor))
    }

    fn read(&self) -> Option<crate::vendors_file::VendorsFile> {
        match self.store.read() {
            Ok(file) => Some(file),
            Err(err) => {
                tracing::warn!("the plan usage board could not read vendors.json: {err}");
                None
            }
        }
    }
}

impl QuotaBoard for FileQuotaBoard {
    /// Keeps the newest reading of each window, so a late one from another process never
    /// replaces it.
    fn observe(&self, reading: QuotaReading) {
        let stored = self.store.update(|file| {
            let entry = file.vendors.entry(reading.vendor.0.clone()).or_default();
            entry.observe_quota(reading);
        });
        if let Err(err) = stored {
            tracing::warn!("the plan usage board could not write vendors.json: {err}");
        }
    }

    fn get(&self, vendor: &VendorId) -> Option<QuotaReading> {
        self.read()?.vendors.remove(&vendor.0)?.quota
    }

    fn all(&self) -> Vec<QuotaReading> {
        self.read()
            .map(|file| {
                file.vendors
                    .into_values()
                    .filter_map(|entry| entry.quota)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn windows(&self, vendor: &VendorId) -> Vec<QuotaReading> {
        self.read()
            .and_then(|file| Some(file.vendors.get(&vendor.0)?.quota_windows()))
            .unwrap_or_default()
    }

    fn plan(&self, vendor: &VendorId) -> Option<String> {
        self.read()?.vendors.remove(&vendor.0)?.plan
    }

    fn observe_plan(&self, vendor: &VendorId, plan: String) {
        let stored = self.store.update(|file| {
            file.vendors.entry(vendor.0.clone()).or_default().plan = Some(plan);
        });
        if let Err(err) = stored {
            tracing::warn!("the plan usage board could not write vendors.json: {err}");
        }
    }

    /// Runs this board's prober for `vendor`, else the registered one, and records each window
    /// it reads and the plan. Gives the window that binds soonest. `None` means unknown, which
    /// the status line shows as `?`.
    fn probe(&self, vendor: &VendorId) -> BoxFuture<'_, Option<QuotaReading>> {
        let prober = self.prober(vendor);
        let vendor = vendor.clone();
        Box::pin(async move {
            let mut answer = run_probe_answer(&prober?, self.probe_timeout).await?;
            for reading in &mut answer.windows {
                reading.vendor = vendor.clone();
            }
            if let Some(plan) = answer.plan.take() {
                self.observe_plan(&vendor, plan);
            }
            for reading in &answer.windows {
                self.observe(reading.clone());
            }
            answer.binding().cloned()
        })
    }
}

/// Reads again, once at start, each of `vendors` whose harness worker registered a free probe
/// and whose newest reading is older than `stale_after`, so the line doesn't open on an old
/// reading. The app calls it once the crew's workers exist. A vendor never read is left to its
/// first session, and one probed within `stale_after` by any process is skipped. Returns the
/// new readings.
pub async fn probe_at_start(
    board: &dyn QuotaBoard,
    home: &Paths,
    vendors: &[VendorId],
    stale_after: Duration,
) -> Vec<QuotaReading> {
    let free = probed_vendors();
    let stale: Vec<VendorId> = vendors
        .iter()
        .filter(|vendor| free.contains(vendor))
        .filter(|vendor| board.get(vendor).is_some() || !board.windows(vendor).is_empty())
        .cloned()
        .collect();
    let since = OffsetDateTime::now_utc() - stale_after;
    probe_each(board, &VendorsStore::new(home), &stale, since).await
}

async fn probe_each(
    board: &dyn QuotaBoard,
    store: &VendorsStore,
    vendors: &[VendorId],
    since: OffsetDateTime,
) -> Vec<QuotaReading> {
    let probes = vendors
        .iter()
        .map(|vendor| probe_unless_since(board, store, vendor, since));
    futures::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Probes `vendor` unless it has a reading or a probe from `since` on. `None` when skipped or
/// unknown.
async fn probe_unless_since(
    board: &dyn QuotaBoard,
    store: &VendorsStore,
    vendor: &VendorId,
    since: OffsetDateTime,
) -> Option<QuotaReading> {
    if !claim_probe(store, vendor, since) {
        return None;
    }
    board.probe(vendor).await
}

/// Claims the next probe of `vendor` under the file lock, so two processes never probe it
/// together. Says whether this caller won.
fn claim_probe(store: &VendorsStore, vendor: &VendorId, since: OffsetDateTime) -> bool {
    let now = OffsetDateTime::now_utc();
    // A time ahead of now means the clock was set back, and it holds nothing off.
    let recent = |at: OffsetDateTime| since <= at && at <= now;
    let claimed = store.update(|file| {
        let entry = file.vendors.entry(vendor.0.clone()).or_default();
        let fresh = entry.probed_at.is_some_and(recent)
            || entry
                .quota
                .as_ref()
                .is_some_and(|quota| recent(quota.observed_at));
        if !fresh {
            entry.probed_at = Some(now);
        }
        !fresh
    });
    claimed.unwrap_or_else(|err| {
        // The reading couldn't be recorded either, so there's no use probing.
        tracing::warn!("could not claim a plan usage probe in vendors.json: {err}");
        false
    })
}

/// Probes a vendor's quota around one session: at most once every [`PROBE_EVERY`] while it
/// runs, and once after it ends, since some streams (`codex exec`) say nothing of quota until
/// a limit stops them. The pace is kept in `vendors.json`, so it holds across every session
/// of every process.
pub struct QuotaWatch {
    board: Arc<dyn QuotaBoard>,
    store: VendorsStore,
    vendor: VendorId,
    every: Duration,
}

impl QuotaWatch {
    pub fn new(board: Arc<dyn QuotaBoard>, home: &Paths, vendor: VendorId) -> Self {
        Self {
            board,
            store: VendorsStore::new(home),
            vendor,
            every: PROBE_EVERY,
        }
    }

    /// Probes at most this often instead, as tests do.
    pub fn every(mut self, every: Duration) -> Self {
        self.every = every;
        self
    }

    /// Passes a session's end on, probing until it comes and once after. The end goes out
    /// first, and a probe in flight never holds it up.
    pub fn relay<T: Send + 'static>(self, ended: oneshot::Receiver<T>) -> oneshot::Receiver<T> {
        let (relayed, receiver) = oneshot::channel();
        tokio::spawn(async move { self.watch(ended, relayed).await });
        receiver
    }

    async fn watch<T>(&self, mut ended: oneshot::Receiver<T>, relayed: oneshot::Sender<T>) {
        // Checking a few times per period keeps probes close to `every` apart even when
        // another process claimed the last one.
        let check = (self.every / 4).max(Duration::from_millis(10));
        let mut checks = tokio::time::interval_at(tokio::time::Instant::now() + check, check);
        checks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut in_flight: Option<BoxFuture<'_, Option<QuotaReading>>> = None;
        let end = loop {
            tokio::select! {
                end = &mut ended => break end,
                _ = checks.tick(), if in_flight.is_none() => {
                    let since = OffsetDateTime::now_utc() - self.every;
                    in_flight = Some(Box::pin(self.probe_unless_since(since)));
                }
                () = finish(&mut in_flight) => in_flight = None,
            }
        };
        let ended_at = OffsetDateTime::now_utc();
        // Sent or dropped before the last probe, so the handle never waits for it. The probe
        // runs even when nobody holds the handle anymore.
        match end {
            Ok(end) => {
                let _ = relayed.send(end);
            }
            Err(_) => drop(relayed),
        }
        // Dropping a probe would leave its process group running, so it finishes first.
        if let Some(probe) = in_flight {
            probe.await;
        }
        self.probe_unless_since(ended_at).await;
    }

    async fn probe_unless_since(&self, since: OffsetDateTime) -> Option<QuotaReading> {
        probe_unless_since(self.board.as_ref(), &self.store, &self.vendor, since).await
    }
}

/// Waits for the probe in flight, or forever when there's none.
async fn finish(probe: &mut Option<BoxFuture<'_, Option<QuotaReading>>>) {
    match probe {
        Some(probe) => {
            probe.await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::wait_until_gone;
    use argonauts_core::limits::{QuotaSource, QuotaStatus};
    use argonauts_core::{AgentEvent, SessionRequest, StopPlan, StreamParser, extract_version};
    use std::time::Instant;
    use time::OffsetDateTime;

    /// A fake CLI whose quota probe is a shell script printing `used=<share>`.
    struct ProbeAdapter {
        script: Option<String>,
        stdin: Option<String>,
        keep_stdin_open: bool,
    }

    impl ProbeAdapter {
        fn new(script: &str) -> Self {
            Self {
                script: Some(script.to_owned()),
                stdin: None,
                keep_stdin_open: false,
            }
        }

        fn prober(self, dir: &Path) -> Prober {
            Prober {
                adapter: Arc::new(self),
                binary: PathBuf::from("/bin/sh"),
                workdir: dir.to_owned(),
                model: None,
            }
        }
    }

    struct NoParser;

    impl StreamParser for NoParser {
        fn parse_stdout_line(&mut self, _: &str) -> Vec<AgentEvent> {
            Vec::new()
        }
        fn parse_stderr_line(&mut self, _: &str) -> Vec<AgentEvent> {
            Vec::new()
        }
        fn finish(&mut self, _: Option<i32>) -> Vec<AgentEvent> {
            Vec::new()
        }
    }

    impl Adapter for ProbeAdapter {
        fn id(&self) -> &'static str {
            "probe-fake"
        }
        fn display_name(&self) -> &'static str {
            "Probe Fake"
        }
        fn binary(&self) -> &'static str {
            "sh"
        }
        fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
            Ok(CommandSpec::new("sh", &req.workdir))
        }
        fn parser(&self) -> Box<dyn StreamParser> {
            Box::new(NoParser)
        }
        fn version_args(&self) -> &'static [&'static str] {
            &["--version"]
        }
        fn parse_version(&self, output: &str) -> Option<String> {
            extract_version(output)
        }
        fn tested_versions(&self) -> &'static [&'static str] {
            &["1.0.0"]
        }
        fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
            ("tiny", None)
        }
        fn stop_plan(&self) -> StopPlan {
            StopPlan::escalating(Duration::from_secs(1), Duration::from_secs(1))
        }
        fn quota_probe(&self, workdir: &Path, _: Option<&str>) -> Option<CommandSpec> {
            let mut spec = CommandSpec::new("sh", workdir).args(["-c", self.script.as_deref()?]);
            spec.stdin = self.stdin.clone();
            spec.keep_stdin_open = self.keep_stdin_open;
            Some(spec)
        }
        fn parse_quota_probe(&self, stdout: &str) -> Option<QuotaReading> {
            let used: f64 = stdout
                .lines()
                .find_map(|line| line.strip_prefix("used="))?
                .trim()
                .parse()
                .ok()?;
            Some(reading("probe-fake", used, OffsetDateTime::now_utc()))
        }
    }

    fn reading(vendor: &str, used: f64, observed_at: OffsetDateTime) -> QuotaReading {
        QuotaReading {
            vendor: VendorId(vendor.into()),
            status: if used < 0.8 {
                QuotaStatus::Allowed
            } else {
                QuotaStatus::Warning
            },
            used: Some(used),
            resets_at: None,
            window: Some("five_hour".into()),
            source: QuotaSource::UsageProbe,
            observed_at,
            detail: None,
            wrap_up: false,
        }
    }

    fn vendor(id: &str) -> VendorId {
        VendorId(id.into())
    }

    fn window(vendor: &str, window: &str, used: f64, observed_at: OffsetDateTime) -> QuotaReading {
        QuotaReading {
            window: Some(window.into()),
            ..reading(vendor, used, observed_at)
        }
    }

    #[test]
    fn the_board_keeps_one_reading_per_vendor_and_window() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let board = FileQuotaBoard::new(&paths);
        let codex = vendor("codex");
        let t = OffsetDateTime::now_utc();
        board.observe(window("codex", "five_hour", 0.92, t));
        board.observe(window("codex", "weekly", 0.41, t));
        board.observe(window(
            "codex",
            "weekly",
            0.43,
            t + time::Duration::seconds(1),
        ));
        board.observe(window("claude-code", "five_hour", 0.2, t));
        let windows: Vec<_> = FileQuotaBoard::new(&paths)
            .windows(&codex)
            .into_iter()
            .map(|r| (r.window.unwrap(), r.used.unwrap()))
            .collect();
        assert_eq!(
            windows,
            [("five_hour".to_owned(), 0.92), ("weekly".to_owned(), 0.43)]
        );
        // What wants one number per vendor gets the fullest window.
        assert_eq!(
            board.get(&codex).unwrap().window.as_deref(),
            Some("five_hour")
        );
        assert_eq!(board.all().len(), 2);
        board.observe_plan(&codex, "plus".into());
        assert_eq!(board.plan(&codex).as_deref(), Some("plus"));
    }

    #[test]
    fn an_old_vendors_json_reads_as_one_window() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let t = OffsetDateTime::now_utc();
        let old = serde_json::json!({"vendors": {"codex": {
            "quota": reading("codex", 0.7, t), "health": null}}});
        std::fs::create_dir_all(home.path()).unwrap();
        std::fs::write(
            VendorsStore::new(&paths).path(),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        let board = FileQuotaBoard::new(&paths);
        let codex = vendor("codex");
        assert_eq!(board.get(&codex).unwrap().used, Some(0.7));
        assert_eq!(board.windows(&codex).len(), 1);
        // The old reading stays as its window when another window comes.
        board.observe(window(
            "codex",
            "weekly",
            0.2,
            t + time::Duration::seconds(1),
        ));
        let windows: Vec<_> = board
            .windows(&codex)
            .into_iter()
            .map(|r| r.window.unwrap())
            .collect();
        assert_eq!(windows, ["five_hour", "weekly"]);
        assert_eq!(board.get(&codex).unwrap().used, Some(0.7));
    }

    #[test]
    fn quota_board_shared_between_processes() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let app = FileQuotaBoard::new(&paths);
        let supervisor = FileQuotaBoard::new(&paths);
        let codex = vendor("codex");
        let t = OffsetDateTime::now_utc();
        assert_eq!(app.get(&codex), None);
        assert!(app.all().is_empty());

        app.observe(reading("codex", 0.3, t));
        assert_eq!(supervisor.get(&codex).unwrap().used, Some(0.3));
        supervisor.observe(reading("codex", 0.5, t + time::Duration::seconds(1)));
        assert_eq!(app.get(&codex).unwrap().used, Some(0.5));
        // A late reading from before never replaces a newer one.
        app.observe(reading("codex", 0.1, t - time::Duration::seconds(1)));
        assert_eq!(supervisor.get(&codex).unwrap().used, Some(0.5));

        supervisor.observe(reading("antigravity-cli", 0.9, t));
        let all: Vec<_> = app
            .all()
            .into_iter()
            .map(|r| (r.vendor.0, r.used))
            .collect();
        assert_eq!(
            all,
            vec![
                ("antigravity-cli".to_owned(), Some(0.9)),
                ("codex".to_owned(), Some(0.5))
            ]
        );
    }

    #[tokio::test]
    async fn probe_runs_registered_prober_with_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));

        register_prober(
            &vendor("probe-fast"),
            ProbeAdapter::new("echo starting; echo used=0.25").prober(dir.path()),
        );
        let board = FileQuotaBoard::new(&paths);
        let read = board.probe(&vendor("probe-fast")).await.unwrap();
        assert_eq!(read.vendor, vendor("probe-fast"));
        assert_eq!(read.used, Some(0.25));
        assert_eq!(board.get(&vendor("probe-fast")), Some(read));

        // Input goes to stdin, and an app server that stays up is stopped once it answered.
        let pid_file = dir.path().join("server.pid");
        let server = ProbeAdapter {
            stdin: Some("0.4\n".into()),
            keep_stdin_open: true,
            ..ProbeAdapter::new(&format!(
                "echo $$ > {}; read share; echo used=$share; sleep 30",
                pid_file.display()
            ))
        };
        register_prober(&vendor("probe-server"), server.prober(dir.path()));
        let started = Instant::now();
        let read = FileQuotaBoard::new(&paths)
            .probe(&vendor("probe-server"))
            .await
            .unwrap();
        assert_eq!(read.used, Some(0.4));
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_until_gone(pid).await;

        // A probe that hangs is unknown after the timeout, and its whole group is killed.
        let sleeper_file = dir.path().join("sleeper.pid");
        register_prober(
            &vendor("probe-slow"),
            ProbeAdapter::new(&format!(
                "sleep 30 & echo $! > {}; wait",
                sleeper_file.display()
            ))
            .prober(dir.path()),
        );
        let impatient = FileQuotaBoard::new(&paths).with_probe_timeout(Duration::from_millis(500));
        let started = Instant::now();
        assert_eq!(impatient.probe(&vendor("probe-slow")).await, None);
        assert!(started.elapsed() < Duration::from_secs(5));
        let sleeper: i32 = std::fs::read_to_string(&sleeper_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_until_gone(sleeper).await;
        assert_eq!(board.get(&vendor("probe-slow")), None);
        let err = try_probe(
            &registered_prober(&vendor("probe-slow")).unwrap(),
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ProbeError::TimedOut { .. }), "{err}");

        // The board's own prober comes before the registered one.
        let own = FileQuotaBoard::new(&paths).with_prober(
            &vendor("probe-slow"),
            ProbeAdapter::new("echo used=0.75").prober(dir.path()),
        );
        assert_eq!(
            own.probe(&vendor("probe-slow")).await.unwrap().used,
            Some(0.75)
        );
    }

    fn install_agent(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("agent");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn unsandboxed_binaries_come_from_absolute_path_entries_outside_refused_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let installed = install_agent(&tmp.path().join("bin"));
        let repo = tmp.path().join("repo");
        let planted = install_agent(&repo.join("node_modules/.bin"));
        let elsewhere = tmp.path().join("elsewhere");
        let path_var = |entries: &[&Path]| std::env::join_paths(entries).unwrap();
        let bin = installed.parent().unwrap();

        let relative = path_var(&[
            Path::new("."),
            Path::new(""),
            Path::new("node_modules/.bin"),
            bin,
        ]);
        assert_eq!(
            find_outside("agent", Some(&relative), std::slice::from_ref(&elsewhere)),
            Ok(installed.clone())
        );

        // An absolute entry into the repo, as direnv adds, is refused, not skipped.
        let into_repo = path_var(&[planted.parent().unwrap(), bin]);
        for refused in [
            [repo.clone(), elsewhere.clone()],
            [elsewhere.clone(), repo.clone()],
        ] {
            assert_eq!(
                find_outside("agent", Some(&into_repo), &refused),
                Err(BinaryError::Inside {
                    path: planted.clone(),
                    dir: repo.clone(),
                })
            );
        }
        let err = find_outside(
            planted.to_str().unwrap(),
            None,
            &[elsewhere.clone(), repo.clone()],
        )
        .unwrap_err();
        assert!(err.to_string().starts_with("refusing to run "), "{err}");

        for program in ["./agent", "/nonexistent/agent", "agent"] {
            assert_eq!(
                find_outside(program, None, std::slice::from_ref(&elsewhere)),
                Err(BinaryError::NotInstalled(program.to_owned()))
            );
        }
        assert_eq!(
            find_outside("agent", Some(&path_var(&[bin])), &[]),
            Err(BinaryError::NotInstalled("agent".to_owned()))
        );
    }

    #[test]
    fn the_repo_you_run_from_is_the_nearest_folder_with_git() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = repo.join("nested/worktree");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(repo_around(&repo.join("nested")), Some(repo.clone()));
        assert_eq!(repo_around(&worktree.join("src")), Some(worktree));
    }

    #[tokio::test]
    async fn a_probe_runs_in_a_folder_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        // `$PWD` names its own folder, not where Argonauts started, and what it leaves there
        // goes with the folder.
        let script = format!(
            r#"[ "$(dirname "$PWD")" = "{}" ] && [ "$(cd "$PWD" && pwd -P)" = "$(pwd -P)" ] \
               && touch left-behind && echo used=0.5"#,
            dir.path().display()
        );
        let prober = ProbeAdapter::new(&script).prober(dir.path());
        let reading = try_probe(&prober, Duration::from_secs(5)).await.unwrap();
        assert_eq!(reading.used, Some(0.5));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn probe_refuses_a_binary_inside_its_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let planted = Prober {
            binary: install_agent(dir.path()),
            ..ProbeAdapter::new("echo used=0.1").prober(dir.path())
        };
        let err = try_probe(&planted, Duration::from_secs(5))
            .await
            .unwrap_err();
        let ProbeError::Run { detail, .. } = &err else {
            panic!("expected a refusal, got {err}");
        };
        assert!(detail.starts_with("refusing to run "), "{detail}");
    }

    #[tokio::test]
    async fn probe_unknown_when_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let board = FileQuotaBoard::new(&Paths::new(dir.path()));
        assert_eq!(board.probe(&vendor("probe-nobody")).await, None);

        let missing = Prober {
            binary: PathBuf::from("/nonexistent/codex"),
            ..ProbeAdapter::new("echo used=0.1").prober(dir.path())
        };
        let err = try_probe(&missing, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, ProbeError::Run { .. }), "{err}");
        let board = board.with_prober(&vendor("probe-missing"), missing);
        assert_eq!(board.probe(&vendor("probe-missing")).await, None);

        let silent = ProbeAdapter {
            script: None,
            ..ProbeAdapter::new("")
        };
        let err = try_probe(&silent.prober(dir.path()), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err, ProbeError::NoProbe("Probe Fake".into()));

        let garbled = ProbeAdapter::new("echo 'app server not reachable' >&2; echo '{'; exit 3")
            .prober(dir.path());
        let err = try_probe(&garbled, Duration::from_secs(5))
            .await
            .unwrap_err();
        let ProbeError::Unreadable { status, .. } = &err else {
            panic!("expected an unreadable answer, got {err}");
        };
        assert!(status.contains('3'), "{status}");
        assert_eq!(board.get(&vendor("probe-missing")), None);
        assert!(board.all().is_empty());
    }

    /// A probe that appends a line to `log` before it answers, or fails when `share` is `None`.
    fn logging_prober(dir: &Path, log: &Path, share: Option<f64>, pause_secs: u32) -> Prober {
        let answer = share.map_or("exit 1".to_owned(), |share| format!("echo used={share}"));
        let script = format!("echo x >> {}; sleep {pause_secs}; {answer}", log.display());
        ProbeAdapter::new(&script).prober(dir)
    }

    fn lines(path: &Path) -> usize {
        std::fs::read_to_string(path).map_or(0, |text| text.lines().count())
    }

    async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn probed_at(store: &VendorsStore, vendor: &VendorId) -> Option<OffsetDateTime> {
        store.read().unwrap().entry(vendor)?.probed_at
    }

    #[tokio::test]
    async fn processes_take_turns_probing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let log = dir.path().join("probes.log");
        let quiet = vendor("probe-quiet");
        let failing = logging_prober(dir.path(), &log, None, 0);
        let app = FileQuotaBoard::new(&paths).with_prober(&quiet, failing.clone());
        let supervisor = FileQuotaBoard::new(&paths).with_prober(&quiet, failing);
        let store = VendorsStore::new(&paths);
        let minute_ago = || OffsetDateTime::now_utc() - PROBE_EVERY;

        // A probe that reads nothing still holds the next one off, in every process.
        let other_store = VendorsStore::new(&paths);
        let (first, second) = tokio::join!(
            probe_each(&app, &store, std::slice::from_ref(&quiet), minute_ago()),
            probe_each(
                &supervisor,
                &other_store,
                std::slice::from_ref(&quiet),
                minute_ago()
            ),
        );
        assert!(first.is_empty() && second.is_empty());
        assert_eq!(lines(&log), 1);

        // A probe from before `since` holds nothing off.
        let since = OffsetDateTime::now_utc();
        probe_each(&supervisor, &store, std::slice::from_ref(&quiet), since).await;
        assert_eq!(lines(&log), 2);
        assert!(probed_at(&store, &quiet).unwrap() >= since);

        // Nor does one ahead of now, as after the clock was set back.
        store
            .update(|file| {
                let entry = file.vendors.get_mut(&quiet.0).unwrap();
                entry.probed_at = Some(OffsetDateTime::now_utc() + time::Duration::hours(1));
            })
            .unwrap();
        probe_each(&app, &store, std::slice::from_ref(&quiet), minute_ago()).await;
        assert_eq!(lines(&log), 3);

        // A fresh reading holds a probe off like a probe does, and each vendor has its turn.
        let read = vendor("probe-read");
        let app = app.with_prober(&read, logging_prober(dir.path(), &log, Some(0.2), 0));
        app.observe(reading("probe-read", 0.1, OffsetDateTime::now_utc()));
        let both = [read.clone(), quiet.clone()];
        assert!(
            probe_each(&app, &store, &both, minute_ago())
                .await
                .is_empty()
        );
        assert_eq!(lines(&log), 3);
        let since = OffsetDateTime::now_utc();
        let readings: Vec<_> = probe_each(&app, &store, &both, since)
            .await
            .into_iter()
            .map(|r| (r.vendor, r.used))
            .collect();
        assert_eq!(readings, vec![(read.clone(), Some(0.2))]);
        assert_eq!(lines(&log), 5);
        assert_eq!(app.get(&read).unwrap().used, Some(0.2));
    }

    #[tokio::test]
    async fn at_start_only_a_stale_reading_with_a_free_probe_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let log = dir.path().join("probes.log");
        let (stale, fresh, unread) = (
            vendor("start-stale"),
            vendor("start-fresh"),
            vendor("start-unread"),
        );
        for each in [&stale, &fresh, &unread] {
            register_prober(each, logging_prober(dir.path(), &log, Some(0.4), 0));
        }
        let board = FileQuotaBoard::new(&paths);
        let hours_ago = |hours: i64| OffsetDateTime::now_utc() - time::Duration::hours(hours);
        for (each, at) in [(&stale, hours_ago(3)), (&fresh, hours_ago(0))] {
            board.observe(reading(&each.0, 0.2, at));
        }
        let crew = [stale.clone(), fresh.clone(), unread.clone()];
        let stale_after = Duration::from_secs(30 * 60);
        let read = probe_at_start(&board, &paths, &crew, stale_after).await;
        assert_eq!(read.len(), 1, "{read:?}");
        assert_eq!(read[0].vendor, stale);
        assert_eq!(
            lines(&log),
            1,
            "the fresh one and the one never read weren't probed"
        );
        // Once read, a second start in the same half hour leaves it alone.
        assert!(
            probe_at_start(&board, &paths, &crew, stale_after)
                .await
                .is_empty()
        );
    }

    #[test]
    fn only_vendors_with_a_probe_are_probed_at_start() {
        let dir = tempfile::tempdir().unwrap();
        register_prober(
            &vendor("probe-listed"),
            ProbeAdapter::new("echo used=0.1").prober(dir.path()),
        );
        let none = ProbeAdapter {
            script: None,
            ..ProbeAdapter::new("")
        };
        register_prober(&vendor("probe-unlisted"), none.prober(dir.path()));
        let vendors = probed_vendors();
        assert!(vendors.contains(&vendor("probe-listed")));
        assert!(!vendors.contains(&vendor("probe-unlisted")));
    }

    #[tokio::test]
    async fn a_session_is_probed_while_it_runs_and_after_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let log = dir.path().join("probes.log");
        let watched = vendor("probe-watched");
        let prober = logging_prober(dir.path(), &log, Some(0.3), 0);
        let board = Arc::new(FileQuotaBoard::new(&paths).with_prober(&watched, prober));
        let store = VendorsStore::new(&paths);
        let every = Duration::from_millis(500);

        let (end, ended) = oneshot::channel();
        let started = Instant::now();
        let relayed = QuotaWatch::new(board.clone(), &paths, watched.clone())
            .every(every)
            .relay(ended);
        // Ends right after a probe answered, so none is in flight and the last one is recent.
        wait_for("probes while running", || {
            lines(&log) >= 2
                && probed_at(&store, &watched).is_some_and(|claimed| {
                    board
                        .get(&watched)
                        .is_some_and(|quota| quota.observed_at >= claimed)
                })
        })
        .await;
        // Probes are never closer than `every`, however often the watch checks.
        let running = lines(&log) as u128;
        let elapsed = started.elapsed();
        assert!(
            running <= elapsed.as_millis() / every.as_millis() + 1,
            "{running} probes in {elapsed:?}"
        );

        let ended_at = OffsetDateTime::now_utc();
        end.send("done").unwrap();
        let end = tokio::time::timeout(Duration::from_secs(5), relayed).await;
        assert_eq!(end.unwrap().unwrap(), "done");
        // The end has its own probe, however recent the last one.
        wait_for("the probe after the end", || {
            probed_at(&store, &watched).is_some_and(|at| at >= ended_at)
                && board
                    .get(&watched)
                    .is_some_and(|quota| quota.observed_at >= ended_at)
        })
        .await;
        let after = lines(&log);
        tokio::time::sleep(every * 3).await;
        assert_eq!(lines(&log), after, "a probe ran after the watch was over");
    }

    #[tokio::test]
    async fn the_end_never_waits_for_a_probe() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let log = dir.path().join("probes.log");
        let slow = vendor("probe-slow-watched");
        let prober = logging_prober(dir.path(), &log, None, 2);
        let board = Arc::new(FileQuotaBoard::new(&paths).with_prober(&slow, prober));
        let watch = || {
            QuotaWatch::new(board.clone(), &paths, slow.clone()).every(Duration::from_millis(100))
        };

        let (end, ended) = oneshot::channel();
        let relayed = watch().relay(ended);
        wait_for("a probe in flight", || lines(&log) >= 1).await;
        let sent = Instant::now();
        end.send(7).unwrap();
        let end = tokio::time::timeout(Duration::from_secs(1), relayed).await;
        assert_eq!(end.unwrap().unwrap(), 7);
        assert!(sent.elapsed() < Duration::from_secs(1));
        // The probe in flight finishes, and since it read nothing, the end has its own. A
        // reading taken after the end would have done instead.
        wait_for("the probe after the end", || lines(&log) >= 2).await;

        // A session that goes away without an end passes that on.
        let (end, ended) = oneshot::channel::<u8>();
        let relayed = watch().relay(ended);
        drop(end);
        let end = tokio::time::timeout(Duration::from_secs(1), relayed).await;
        assert!(end.unwrap().is_err());
    }
}
