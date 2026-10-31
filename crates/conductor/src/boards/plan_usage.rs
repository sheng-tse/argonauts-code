//! The plan usage board: the latest reading per agent, in `agents.json`, so the app's bottom line
//! and every run host agree. Probes never spend: agy's free `/usage`, Codex's rate-limit
//! read over `codex app-server`, and Claude Code's `get_usage` asked before any message. Each
//! answers with a live reading kept for the account it names.
//!
//! Probes run at app start, after each session of an agent that has one, and at most once every
//! [`PROBE_EVERY`] while such a session runs. Every process claims a probe in `agents.json`
//! before running it, so processes take turns.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::AgentId;
use argonauts_core::models::limits::{PlanUsageAnswer, PlanUsageBoard, PlanUsageReading};
use argonauts_core::models::plan_usage::LiveUsage;
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

use crate::boards::agents_file::AgentsStore;
use crate::process::{SpawnError, resolve_program};

/// A probe that hasn't answered by then reads as unknown.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// While sessions run, an agent is probed at most this often, across every process.
pub const PROBE_EVERY: Duration = Duration::from_secs(60);

/// How to read an agent's plan usage without spending any: its adapter's `plan_usage_probe`, run
/// with the binary your settings name.
#[derive(Clone)]
pub struct Prober {
    pub adapter: Arc<dyn Adapter>,
    /// From `agents.<id>.binary`, else the adapter's binary on `PATH`, found with
    /// [`find_unsandboxed`].
    pub binary: PathBuf,
    /// Where each probe gets a folder of its own to run in. A probe reads no repo.
    pub workdir: PathBuf,
    /// The model the agent's sessions use, for a CLI that names one on every call.
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

/// Finds a binary that runs outside the sandbox with your environment, such as a plan usage probe
/// or one of doctor's checks. As for a session, a name is looked up on absolute `PATH` entries only
/// and a path must be absolute. One inside `workdir`, the repo you run from, or Argonauts Code's
/// home, where agents write, is refused, so neither a repo nor an agent can plant it.
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
    // Nothing to refuse means no home either, and Argonauts Code can't run without one.
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

/// Each CLI runner registers its adapter, so a probe at app start finds the binary the
/// settings name. The last registration for an agent wins.
pub fn register_prober(agent_id: &AgentId, prober: Prober) {
    if let Ok(mut probers) = PROBERS.lock() {
        probers.insert(agent_id.0.clone(), prober);
    }
}

pub fn registered_prober(agent_id: &AgentId) -> Option<Prober> {
    PROBERS.lock().ok()?.get(&agent_id.0).cloned()
}

/// Agents whose registered adapter has a free plan usage probe.
fn probed_agents() -> Vec<AgentId> {
    let Ok(probers) = PROBERS.lock() else {
        return Vec::new();
    };
    probers
        .iter()
        .filter(|(_, prober)| has_plan_usage_probe(prober.adapter.as_ref()))
        .map(|(agent_id, _)| AgentId(agent_id.clone()))
        .collect()
}

pub fn has_plan_usage_probe(adapter: &dyn Adapter) -> bool {
    adapter
        .plan_usage_probe(&std::env::temp_dir(), None)
        .is_some()
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
pub async fn run_probe(prober: &Prober) -> Option<PlanUsageReading> {
    run_probe_within(prober, PROBE_TIMEOUT).await
}

/// Like [`run_probe`], with another timeout.
pub async fn run_probe_within(prober: &Prober, timeout: Duration) -> Option<PlanUsageReading> {
    run_probe_answer(prober, timeout)
        .await
        .and_then(|answer| answer.binding().cloned())
}

/// Every window the probe read, and the plan.
async fn run_probe_answer(prober: &Prober, timeout: Duration) -> Option<PlanUsageAnswer> {
    match try_probe_answer(prober, timeout).await {
        Ok(answer) => Some(answer),
        Err(err) => {
            tracing::warn!("plan usage probe: {err}");
            None
        }
    }
}

/// Runs the probe and says why when there's no reading. Gives the window that binds soonest.
pub async fn try_probe(prober: &Prober, timeout: Duration) -> Result<PlanUsageReading, ProbeError> {
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
) -> Result<PlanUsageAnswer, ProbeError> {
    try_probe_answer_early(prober, timeout, &mut |_| {}).await
}

/// Like [`try_probe_answer`], handing `early` the first part of the answer the adapter can
/// read before the rest, once.
async fn try_probe_answer_early(
    prober: &Prober,
    timeout: Duration,
    early: &mut (dyn FnMut(PlanUsageAnswer) + Send),
) -> Result<PlanUsageAnswer, ProbeError> {
    let adapter = prober.adapter.as_ref();
    let folder = ProbeFolder::new(&prober.workdir).map_err(|detail| ProbeError::Run {
        program: adapter.binary().to_owned(),
        detail,
    })?;
    let spec = adapter
        .plan_usage_probe(&folder.0, prober.model.as_deref())
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
        // wherever you started Argonauts Code.
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

    let mut text = String::new();
    let answer = tokio::time::timeout(
        timeout,
        exchange(&mut child, &spec, adapter, &mut text, early),
    )
    .await;
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
        // What came before the time ran out may still answer, as Claude Code's windows do
        // while its slower scan goes on.
        Err(_) => adapter
            .parse_plan_usage_probe_end(&text)
            .ok_or(ProbeError::TimedOut {
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

/// Sends the probe's stdin, then reads stdout until the adapter can parse it, handing `early`
/// the first part it can read before then. The error is the exit status, for the message.
async fn exchange(
    child: &mut Child,
    spec: &CommandSpec,
    adapter: &dyn Adapter,
    text: &mut String,
    early: &mut (dyn FnMut(PlanUsageAnswer) + Send),
) -> Result<PlanUsageAnswer, String> {
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
    let mut handed_early = false;
    while let Ok(Some(line)) = lines.next_line().await {
        text.push_str(&line);
        text.push('\n');
        if let Some(answer) = adapter.parse_plan_usage_probe_answer(text) {
            return Ok(answer);
        }
        if !handed_early && let Some(part) = adapter.parse_plan_usage_probe_early(text) {
            handed_early = true;
            early(part);
        }
    }
    let status = child
        .wait()
        .await
        .map_or_else(|err| err.to_string(), |status| status.to_string());
    adapter.parse_plan_usage_probe_end(text).ok_or(status)
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

pub struct FilePlanUsageBoard {
    store: AgentsStore,
    probers: BTreeMap<String, Prober>,
    probe_timeout: Duration,
    /// Why each CLI's last probe got no reading, and when. Only this process shows it, so it
    /// isn't kept.
    failed: Mutex<BTreeMap<String, (String, OffsetDateTime)>>,
    /// When each CLI was last signed in or out from Argonauts Code.
    signed: Mutex<BTreeMap<String, OffsetDateTime>>,
}

impl FilePlanUsageBoard {
    pub fn new(paths: &Paths) -> Self {
        Self {
            store: AgentsStore::new(paths),
            probers: BTreeMap::new(),
            probe_timeout: PROBE_TIMEOUT,
            failed: Mutex::new(BTreeMap::new()),
            signed: Mutex::new(BTreeMap::new()),
        }
    }

    fn note_failure(&self, agent_id: &AgentId, why: Option<String>) {
        let mut failed = self
            .failed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match why {
            Some(why) => {
                failed.insert(agent_id.0.clone(), (why, OffsetDateTime::now_utc()));
            }
            None => {
                failed.remove(&agent_id.0);
            }
        }
    }

    /// Uses this prober for `agent` instead of the registered one, as tests do.
    pub fn with_prober(mut self, agent_id: &AgentId, prober: Prober) -> Self {
        self.probers.insert(agent_id.0.clone(), prober);
        self
    }

    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    fn prober(&self, agent_id: &AgentId) -> Option<Prober> {
        self.probers
            .get(&agent_id.0)
            .cloned()
            .or_else(|| registered_prober(agent_id))
    }

    fn read(&self) -> Option<crate::boards::agents_file::AgentsFile> {
        match self.store.read() {
            Ok(file) => Some(file),
            Err(err) => {
                tracing::warn!("the plan usage board could not read agents.json: {err}");
                None
            }
        }
    }
}

impl PlanUsageBoard for FilePlanUsageBoard {
    /// Keeps the newest reading of each window, so a late one from another process never
    /// replaces it.
    fn observe(&self, reading: PlanUsageReading) {
        let stored = self.store.update(|file| {
            let entry = file
                .agent_ids
                .entry(reading.agent_id.0.clone())
                .or_default();
            entry.observe_plan_usage(reading);
        });
        if let Err(err) = stored {
            tracing::warn!("the plan usage board could not write agents.json: {err}");
        }
    }

    fn get(&self, agent_id: &AgentId) -> Option<PlanUsageReading> {
        self.read()?.agent_ids.remove(&agent_id.0)?.plan_usage
    }

    fn all(&self) -> Vec<PlanUsageReading> {
        self.read()
            .map(|file| {
                file.agent_ids
                    .into_values()
                    .filter_map(|entry| entry.plan_usage)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn windows(&self, agent_id: &AgentId) -> Vec<PlanUsageReading> {
        self.read()
            .and_then(|file| Some(file.agent_ids.get(&agent_id.0)?.plan_usage_windows()))
            .unwrap_or_default()
    }

    fn plan(&self, agent_id: &AgentId) -> Option<String> {
        self.read()?.agent_ids.remove(&agent_id.0)?.plan
    }

    fn observe_plan(&self, agent_id: &AgentId, plan: String) {
        let stored = self.store.update(|file| {
            file.agent_ids.entry(agent_id.0.clone()).or_default().plan = Some(plan);
        });
        if let Err(err) = stored {
            tracing::warn!("the plan usage board could not write agents.json: {err}");
        }
    }

    fn observe_live(&self, reading: LiveUsage) {
        let stored = self.store.update(|file| {
            let entry = file
                .agent_ids
                .entry(reading.agent_id.0.clone())
                .or_default();
            entry.observe_live(reading);
        });
        if let Err(err) = stored {
            tracing::warn!("the plan usage board could not write agents.json: {err}");
        }
    }

    fn live(&self, agent_id: &AgentId) -> Option<LiveUsage> {
        self.read()?.agent_ids.remove(&agent_id.0)?.live.pop()
    }

    fn failed(&self, agent_id: &AgentId) -> Option<String> {
        let failed = self
            .failed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        failed.get(&agent_id.0).map(|(why, _)| why.clone())
    }

    fn doubted_at(&self, agent_id: &AgentId) -> Option<OffsetDateTime> {
        let failed = self
            .failed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&agent_id.0)
            .map(|(_, at)| *at);
        let signed = self
            .signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&agent_id.0)
            .copied();
        failed.max(signed)
    }

    fn doubt(&self, agent_id: &AgentId) {
        self.signed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(agent_id.0.clone(), OffsetDateTime::now_utc());
    }

    /// Runs this board's prober for `agent`, else the registered one, and records each window
    /// it reads and the plan. Gives the window that binds soonest. `None` means unknown, which
    /// the bottom line shows as `?`, and [`PlanUsageBoard::failed`] says why.
    fn probe(&self, agent_id: &AgentId) -> BoxFuture<'_, Option<PlanUsageReading>> {
        let prober = self.prober(agent_id);
        let agent_id = agent_id.clone();
        Box::pin(async move {
            // What shows first, as Claude Code's windows before its slower scan, so the task
            // isn't left waiting for the rest.
            let mut early = |part: PlanUsageAnswer| {
                if let Some(mut live) = part.live {
                    live.agent_id = agent_id.clone();
                    self.observe_live(live);
                }
            };
            let prober = prober?;
            let cli = prober.adapter.display_name();
            let read = try_probe_answer_early(&prober, self.probe_timeout, &mut early).await;
            let mut answer = match read {
                Ok(answer) => answer,
                Err(err) => {
                    tracing::warn!("plan usage probe: {err}");
                    self.note_failure(&agent_id, failure_words(cli, &err));
                    return None;
                }
            };
            let said = answer.failed.as_deref();
            self.note_failure(&agent_id, said.map(|said| own_words(cli, said)));
            for reading in &mut answer.windows {
                reading.agent_id = agent_id.clone();
            }
            // First, so a reading of a new account clears the old account's windows before
            // the new ones arrive.
            if let Some(mut live) = answer.live.take() {
                live.agent_id = agent_id.clone();
                self.observe_live(live);
            }
            if let Some(plan) = answer.plan.take() {
                self.observe_plan(&agent_id, plan);
            }
            for reading in &answer.windows {
                self.observe(reading.clone());
            }
            answer.binding().cloned()
        })
    }
}

/// Why a probe of the CLI named `cli` got no reading, said after "Failed to load usage
/// data:". A CLI with no free read failed at nothing, so it has none.
fn failure_words(cli: &str, err: &ProbeError) -> Option<String> {
    match err {
        ProbeError::NoProbe(_) => None,
        ProbeError::Run { detail, .. } => Some(format!("{cli} couldn't start: {detail}")),
        ProbeError::TimedOut { seconds, .. } => {
            Some(format!("{cli} didn't answer within {seconds}s"))
        }
        ProbeError::Unreadable { status, .. } => {
            Some(format!("{cli} ended without an answer ({status})"))
        }
    }
}

/// Words in a CLI's own failure that say its read never got through.
const NETWORK_WORDS: [&str; 7] = [
    "network",
    "connection",
    "connect to",
    "unreachable",
    "dns",
    "offline",
    "timed out",
];

/// What the CLI named `cli` said of a read that failed, in its own words, unless they say the
/// read never got through: then that it couldn't be reached, and the cause it gave last.
fn own_words(cli: &str, said: &str) -> String {
    let lower = said.to_lowercase();
    if !NETWORK_WORDS.iter().any(|words| lower.contains(words)) {
        return said.to_owned();
    }
    let cause = said.rsplit(": ").next().unwrap_or(said).trim();
    format!("{cli} couldn't be reached ({cause})")
}

/// Reads again, once at start, each of `agents` whose CLI runner registered a free probe
/// and whose newest reading is older than `stale_after`, so the line doesn't open on an old
/// reading. The app calls it once the crew's runners exist. An agent never read is left to its
/// first session, and one probed within `stale_after` by any process is skipped. Returns the
/// new readings.
pub async fn probe_at_start(
    board: &dyn PlanUsageBoard,
    home: &Paths,
    agent_ids: &[AgentId],
    stale_after: Duration,
) -> Vec<PlanUsageReading> {
    let free = probed_agents();
    let stale: Vec<AgentId> = agent_ids
        .iter()
        .filter(|agent_id| free.contains(agent_id))
        .filter(|agent_id| board.get(agent_id).is_some() || !board.windows(agent_id).is_empty())
        .cloned()
        .collect();
    let since = OffsetDateTime::now_utc() - stale_after;
    probe_each(board, &AgentsStore::new(home), &stale, since).await
}

async fn probe_each(
    board: &dyn PlanUsageBoard,
    store: &AgentsStore,
    agent_ids: &[AgentId],
    since: OffsetDateTime,
) -> Vec<PlanUsageReading> {
    let probes = agent_ids
        .iter()
        .map(|agent_id| probe_unless_since(board, store, agent_id, since));
    futures::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Probes `agent` unless it has a reading or a probe from `since` on. `None` when skipped or
/// unknown.
async fn probe_unless_since(
    board: &dyn PlanUsageBoard,
    store: &AgentsStore,
    agent_id: &AgentId,
    since: OffsetDateTime,
) -> Option<PlanUsageReading> {
    if !claim_probe(store, agent_id, since) {
        return None;
    }
    board.probe(agent_id).await
}

/// Claims the next probe of `agent` under the file lock, so two processes never probe it
/// together. Says whether this caller won.
fn claim_probe(store: &AgentsStore, agent_id: &AgentId, since: OffsetDateTime) -> bool {
    let now = OffsetDateTime::now_utc();
    // A time ahead of now means the clock was set back, and it holds nothing off.
    let recent = |at: OffsetDateTime| since <= at && at <= now;
    let claimed = store.update(|file| {
        let entry = file.agent_ids.entry(agent_id.0.clone()).or_default();
        let fresh = entry.probed_at.is_some_and(recent)
            || entry
                .plan_usage
                .as_ref()
                .is_some_and(|plan_usage| recent(plan_usage.observed_at));
        if !fresh {
            entry.probed_at = Some(now);
        }
        !fresh
    });
    claimed.unwrap_or_else(|err| {
        // The reading couldn't be recorded either, so there's no use probing.
        tracing::warn!("could not claim a plan usage probe in agents.json: {err}");
        false
    })
}

/// Probes an agent's plan usage around one session: at most once every [`PROBE_EVERY`] while it
/// runs, and once after it ends, since some streams (`codex exec`) say nothing of plan usage until
/// a limit stops them. The pace is kept in `agents.json`, so it holds across every session
/// of every process.
pub struct PlanUsageWatch {
    board: Arc<dyn PlanUsageBoard>,
    store: AgentsStore,
    agent_id: AgentId,
    every: Duration,
}

impl PlanUsageWatch {
    pub fn new(board: Arc<dyn PlanUsageBoard>, home: &Paths, agent_id: AgentId) -> Self {
        Self {
            board,
            store: AgentsStore::new(home),
            agent_id,
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
        let mut in_flight: Option<BoxFuture<'_, Option<PlanUsageReading>>> = None;
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

    async fn probe_unless_since(&self, since: OffsetDateTime) -> Option<PlanUsageReading> {
        probe_unless_since(self.board.as_ref(), &self.store, &self.agent_id, since).await
    }
}

/// Waits for the probe in flight, or forever when there's none.
async fn finish(probe: &mut Option<BoxFuture<'_, Option<PlanUsageReading>>>) {
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
    use crate::testing::wait_until_gone;
    use argonauts_core::models::limits::{PlanUsageSource, PlanUsageStatus};
    use argonauts_core::{AgentEvent, SessionRequest, StopPlan, StreamParser, extract_version};
    use std::time::Instant;
    use time::OffsetDateTime;

    /// A fake CLI whose plan usage probe is a shell script printing `used=<share>`.
    struct ProbeAdapter {
        script: Option<String>,
        stdin: Option<String>,
        keep_stdin_open: bool,
        /// Answers with a live reading of the account in its `account=` line, once a `done`
        /// line follows, or at the end with what came.
        live: bool,
    }

    impl ProbeAdapter {
        fn new(script: &str) -> Self {
            Self {
                script: Some(script.to_owned()),
                stdin: None,
                keep_stdin_open: false,
                live: false,
            }
        }

        fn live(script: &str) -> Self {
            Self {
                live: true,
                ..Self::new(script)
            }
        }

        fn live_answer(&self, stdout: &str) -> Option<PlanUsageAnswer> {
            let reading = self.parse_plan_usage_probe(stdout)?;
            let mut live = LiveUsage::new(
                reading.agent_id.clone(),
                reading.source,
                reading.observed_at,
            );
            live.account = stdout
                .lines()
                .find_map(|line| line.strip_prefix("account="))
                .map(str::to_owned);
            live.windows = vec![
                argonauts_core::models::plan_usage::LiveWindow::new("five_hour", "5-hour", "5h")
                    .used(reading.used),
            ];
            Some(PlanUsageAnswer {
                windows: live.plan_usage_readings(),
                plan: None,
                live: Some(live),
                failed: None,
            })
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
            StopPlan::stepped(Duration::from_secs(1), Duration::from_secs(1))
        }
        fn plan_usage_probe(&self, workdir: &Path, _: Option<&str>) -> Option<CommandSpec> {
            let mut spec = CommandSpec::new("sh", workdir).args(["-c", self.script.as_deref()?]);
            spec.stdin = self.stdin.clone();
            spec.keep_stdin_open = self.keep_stdin_open;
            Some(spec)
        }
        fn parse_plan_usage_probe(&self, stdout: &str) -> Option<PlanUsageReading> {
            let used: f64 = stdout
                .lines()
                .find_map(|line| line.strip_prefix("used="))?
                .trim()
                .parse()
                .ok()?;
            Some(reading("probe-fake", used, OffsetDateTime::now_utc()))
        }
        fn parse_plan_usage_probe_answer(&self, stdout: &str) -> Option<PlanUsageAnswer> {
            if let Some(error) = stdout.lines().find_map(|line| line.strip_prefix("error=")) {
                return Some(PlanUsageAnswer::failed(error));
            }
            if !self.live {
                let reading = self.parse_plan_usage_probe(stdout)?;
                return Some(PlanUsageAnswer {
                    windows: vec![reading],
                    plan: None,
                    live: None,
                    failed: None,
                });
            }
            stdout.lines().any(|line| line == "done").then_some(())?;
            self.live_answer(stdout)
        }
        fn parse_plan_usage_probe_end(&self, stdout: &str) -> Option<PlanUsageAnswer> {
            if self.live {
                self.live_answer(stdout)
            } else {
                self.parse_plan_usage_probe_answer(stdout)
            }
        }
        /// The live reading so far, once an `early` line says it's worth showing.
        fn parse_plan_usage_probe_early(&self, stdout: &str) -> Option<PlanUsageAnswer> {
            stdout.lines().any(|line| line == "early").then_some(())?;
            self.live_answer(stdout)
        }
    }

    fn reading(agent_id: &str, used: f64, observed_at: OffsetDateTime) -> PlanUsageReading {
        PlanUsageReading {
            agent_id: AgentId(agent_id.into()),
            status: if used < 0.8 {
                PlanUsageStatus::Allowed
            } else {
                PlanUsageStatus::Warning
            },
            used: Some(used),
            resets_at: None,
            window: Some("five_hour".into()),
            source: PlanUsageSource::UsageProbe,
            observed_at,
            detail: None,
            wrap_up: false,
        }
    }

    fn agent_id(id: &str) -> AgentId {
        AgentId(id.into())
    }

    fn window(
        agent_id: &str,
        window: &str,
        used: f64,
        observed_at: OffsetDateTime,
    ) -> PlanUsageReading {
        PlanUsageReading {
            window: Some(window.into()),
            ..reading(agent_id, used, observed_at)
        }
    }

    #[test]
    fn the_board_keeps_one_reading_per_agent_and_window() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let board = FilePlanUsageBoard::new(&paths);
        let codex = agent_id("codex");
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
        let windows: Vec<_> = FilePlanUsageBoard::new(&paths)
            .windows(&codex)
            .into_iter()
            .map(|r| (r.window.unwrap(), r.used.unwrap()))
            .collect();
        assert_eq!(
            windows,
            [("five_hour".to_owned(), 0.92), ("weekly".to_owned(), 0.43)]
        );
        // What wants one number per agent gets the fullest window.
        assert_eq!(
            board.get(&codex).unwrap().window.as_deref(),
            Some("five_hour")
        );
        assert_eq!(board.all().len(), 2);
        board.observe_plan(&codex, "plus".into());
        assert_eq!(board.plan(&codex).as_deref(), Some("plus"));
    }

    #[test]
    fn an_old_agents_json_reads_as_one_window() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let t = OffsetDateTime::now_utc();
        let old = serde_json::json!({"agent_ids": {"codex": {
            "plan_usage": reading("codex", 0.7, t), "health": null}}});
        std::fs::create_dir_all(home.path()).unwrap();
        std::fs::write(
            AgentsStore::new(&paths).path(),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        let board = FilePlanUsageBoard::new(&paths);
        let codex = agent_id("codex");
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
    fn plan_usage_board_shared_between_processes() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let app = FilePlanUsageBoard::new(&paths);
        let run_host = FilePlanUsageBoard::new(&paths);
        let codex = agent_id("codex");
        let t = OffsetDateTime::now_utc();
        assert_eq!(app.get(&codex), None);
        assert!(app.all().is_empty());

        app.observe(reading("codex", 0.3, t));
        assert_eq!(run_host.get(&codex).unwrap().used, Some(0.3));
        run_host.observe(reading("codex", 0.5, t + time::Duration::seconds(1)));
        assert_eq!(app.get(&codex).unwrap().used, Some(0.5));
        // A late reading from before never replaces a newer one.
        app.observe(reading("codex", 0.1, t - time::Duration::seconds(1)));
        assert_eq!(run_host.get(&codex).unwrap().used, Some(0.5));

        run_host.observe(reading("antigravity-cli", 0.9, t));
        let all: Vec<_> = app
            .all()
            .into_iter()
            .map(|r| (r.agent_id.0, r.used))
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
    async fn a_live_reading_is_kept_for_its_account_and_a_new_account_clears_the_old() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let first = ProbeAdapter::live("echo account=one@example.com; echo used=0.6; echo done");
        let board = FilePlanUsageBoard::new(&paths)
            .with_prober(&agent_id("probe-live"), first.prober(dir.path()));
        board.probe(&agent_id("probe-live")).await.unwrap();
        let live = board.live(&agent_id("probe-live")).unwrap();
        assert_eq!(live.account.as_deref(), Some("one@example.com"));
        assert_eq!(live.agent_id, agent_id("probe-live"));
        assert_eq!(board.windows(&agent_id("probe-live")).len(), 1);

        let second = ProbeAdapter::live("echo account=two@example.com; echo used=0.1; echo done");
        let board = FilePlanUsageBoard::new(&paths)
            .with_prober(&agent_id("probe-live"), second.prober(dir.path()));
        board.probe(&agent_id("probe-live")).await.unwrap();
        let live = board.live(&agent_id("probe-live")).unwrap();
        assert_eq!(live.account.as_deref(), Some("two@example.com"));
        // Only the new account's window speaks for the CLI now.
        let windows = board.windows(&agent_id("probe-live"));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used, Some(0.1));
    }

    #[tokio::test]
    async fn a_failed_read_says_why_until_a_reading_lands_and_keeps_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let said = dir.path().join("said.txt");
        let answers = |text: &str| std::fs::write(&said, text).unwrap();
        let cli = agent_id("probe-failing");
        let script = format!("cat '{}'", said.display());
        let board = FilePlanUsageBoard::new(&paths)
            .with_prober(&cli, ProbeAdapter::live(&script).prober(dir.path()))
            .with_probe_timeout(Duration::from_secs(5));

        answers("account=one@example.com\nused=0.4\ndone\n");
        assert!(board.probe(&cli).await.is_some());
        assert_eq!(board.failed(&cli), None);

        answers("error=failed to fetch codex rate limits\n");
        assert!(board.probe(&cli).await.is_none());
        assert_eq!(
            board.failed(&cli).as_deref(),
            Some("failed to fetch codex rate limits")
        );
        // The error names no account, so the reading before it is kept, but it no longer
        // stands as the CLI's own: the account may have changed since.
        let kept = board.live(&cli).unwrap();
        assert_eq!(kept.account.as_deref(), Some("one@example.com"));
        let now = OffsetDateTime::now_utc();
        assert!(!kept.current_at(now, board.doubted_at(&cli)));
        assert!(argonauts_core::models::plan_usage::current_live(&board, &cli, now).is_none());

        answers("nothing useful\n");
        assert!(board.probe(&cli).await.is_none());
        assert_eq!(
            board.failed(&cli).as_deref(),
            Some("Probe Fake ended without an answer (exit status: 0)")
        );

        answers("account=one@example.com\nused=0.5\ndone\n");
        assert!(board.probe(&cli).await.is_some());
        assert_eq!(board.failed(&cli), None);
        let now = OffsetDateTime::now_utc();
        assert!(argonauts_core::models::plan_usage::current_live(&board, &cli, now).is_some());

        // Signing in again casts the same doubt until the next reading.
        board.doubt(&cli);
        let now = OffsetDateTime::now_utc();
        assert!(argonauts_core::models::plan_usage::current_live(&board, &cli, now).is_none());
        answers("account=two@example.com\nused=0.1\ndone\n");
        assert!(board.probe(&cli).await.is_some());
        let now = OffsetDateTime::now_utc();
        let live = argonauts_core::models::plan_usage::current_live(&board, &cli, now).unwrap();
        assert_eq!(live.account.as_deref(), Some("two@example.com"));
    }

    #[tokio::test]
    async fn the_first_part_of_an_answer_shows_before_the_rest_comes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let go = dir.path().join("go");
        let cli = agent_id("probe-early");
        // The rest waits until the first part was seen on the board.
        let script = format!(
            "echo account=one@example.com; echo used=0.3; echo early; \
             while [ ! -f '{}' ]; do sleep 0.05; done; echo done",
            go.display()
        );
        let board = FilePlanUsageBoard::new(&paths)
            .with_prober(&cli, ProbeAdapter::live(&script).prober(dir.path()))
            .with_probe_timeout(Duration::from_secs(20));
        let watch = async {
            wait_for("the first part", || board.live(&cli).is_some()).await;
            std::fs::write(&go, "").unwrap();
        };
        let (read, ()) = tokio::join!(board.probe(&cli), watch);
        assert_eq!(read.unwrap().used, Some(0.3));
        let live = board.live(&cli).unwrap();
        assert_eq!(live.account.as_deref(), Some("one@example.com"));
        // The whole answer took the first part's place for the same account.
        let file = board.read().unwrap();
        assert_eq!(file.agent_ids[&cli.0].live.len(), 1);
    }

    #[test]
    fn a_cli_with_no_free_read_has_no_failure_to_show() {
        let none = ProbeError::NoProbe("OpenCode".into());
        assert_eq!(failure_words("OpenCode", &none), None);
        let timed_out = ProbeError::TimedOut {
            program: "/usr/local/bin/agy".into(),
            seconds: 15,
        };
        assert_eq!(
            failure_words("agy", &timed_out).as_deref(),
            Some("agy didn't answer within 15s")
        );
    }

    /// A CLI's own words that say its read never got through say it couldn't be reached, with
    /// the cause it gave; any other words stay its own.
    #[test]
    fn a_read_that_never_got_through_says_the_cli_couldnt_be_reached() {
        assert_eq!(
            own_words(
                "Codex",
                "failed to fetch codex rate limits: application network policy is unavailable"
            ),
            "Codex couldn't be reached (application network policy is unavailable)"
        );
        assert_eq!(
            own_words("Codex", "failed to fetch codex rate limits"),
            "failed to fetch codex rate limits"
        );
    }

    #[tokio::test]
    async fn a_probe_out_of_time_answers_with_what_came_before() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        // The slow part never says `done`.
        let slow = ProbeAdapter::live("echo account=one@example.com; echo used=0.3; sleep 30");
        let board = FilePlanUsageBoard::new(&paths)
            .with_prober(&agent_id("probe-slow-live"), slow.prober(dir.path()))
            .with_probe_timeout(Duration::from_millis(500));
        let started = Instant::now();
        let read = board.probe(&agent_id("probe-slow-live")).await.unwrap();
        assert_eq!(read.used, Some(0.3));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(board.live(&agent_id("probe-slow-live")).is_some());
    }

    #[tokio::test]
    async fn probe_runs_registered_prober_with_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));

        register_prober(
            &agent_id("probe-fast"),
            ProbeAdapter::new("echo starting; echo used=0.25").prober(dir.path()),
        );
        let board = FilePlanUsageBoard::new(&paths);
        let read = board.probe(&agent_id("probe-fast")).await.unwrap();
        assert_eq!(read.agent_id, agent_id("probe-fast"));
        assert_eq!(read.used, Some(0.25));
        assert_eq!(board.get(&agent_id("probe-fast")), Some(read));

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
        register_prober(&agent_id("probe-server"), server.prober(dir.path()));
        let started = Instant::now();
        let read = FilePlanUsageBoard::new(&paths)
            .probe(&agent_id("probe-server"))
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
            &agent_id("probe-slow"),
            ProbeAdapter::new(&format!(
                "sleep 30 & echo $! > {}; wait",
                sleeper_file.display()
            ))
            .prober(dir.path()),
        );
        let impatient =
            FilePlanUsageBoard::new(&paths).with_probe_timeout(Duration::from_millis(500));
        let started = Instant::now();
        assert_eq!(impatient.probe(&agent_id("probe-slow")).await, None);
        assert!(started.elapsed() < Duration::from_secs(5));
        let sleeper: i32 = std::fs::read_to_string(&sleeper_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_until_gone(sleeper).await;
        assert_eq!(board.get(&agent_id("probe-slow")), None);
        let err = try_probe(
            &registered_prober(&agent_id("probe-slow")).unwrap(),
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ProbeError::TimedOut { .. }), "{err}");

        // The board's own prober comes before the registered one.
        let own = FilePlanUsageBoard::new(&paths).with_prober(
            &agent_id("probe-slow"),
            ProbeAdapter::new("echo used=0.75").prober(dir.path()),
        );
        assert_eq!(
            own.probe(&agent_id("probe-slow")).await.unwrap().used,
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
        // `$PWD` names its own folder, not where Argonauts Code started, and what it leaves there
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
        let board = FilePlanUsageBoard::new(&Paths::new(dir.path()));
        assert_eq!(board.probe(&agent_id("probe-nobody")).await, None);

        let missing = Prober {
            binary: PathBuf::from("/nonexistent/codex"),
            ..ProbeAdapter::new("echo used=0.1").prober(dir.path())
        };
        let err = try_probe(&missing, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, ProbeError::Run { .. }), "{err}");
        let board = board.with_prober(&agent_id("probe-missing"), missing);
        assert_eq!(board.probe(&agent_id("probe-missing")).await, None);

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
        assert_eq!(board.get(&agent_id("probe-missing")), None);
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

    fn probed_at(store: &AgentsStore, agent_id: &AgentId) -> Option<OffsetDateTime> {
        store.read().unwrap().entry(agent_id)?.probed_at
    }

    #[tokio::test]
    async fn processes_take_turns_probing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let log = dir.path().join("probes.log");
        let quiet = agent_id("probe-quiet");
        let failing = logging_prober(dir.path(), &log, None, 0);
        let app = FilePlanUsageBoard::new(&paths).with_prober(&quiet, failing.clone());
        let run_host = FilePlanUsageBoard::new(&paths).with_prober(&quiet, failing);
        let store = AgentsStore::new(&paths);
        let minute_ago = || OffsetDateTime::now_utc() - PROBE_EVERY;

        // A probe that reads nothing still holds the next one off, in every process.
        let other_store = AgentsStore::new(&paths);
        let (first, second) = tokio::join!(
            probe_each(&app, &store, std::slice::from_ref(&quiet), minute_ago()),
            probe_each(
                &run_host,
                &other_store,
                std::slice::from_ref(&quiet),
                minute_ago()
            ),
        );
        assert!(first.is_empty() && second.is_empty());
        assert_eq!(lines(&log), 1);

        // A probe from before `since` holds nothing off.
        let since = OffsetDateTime::now_utc();
        probe_each(&run_host, &store, std::slice::from_ref(&quiet), since).await;
        assert_eq!(lines(&log), 2);
        assert!(probed_at(&store, &quiet).unwrap() >= since);

        // Nor does one ahead of now, as after the clock was set back.
        store
            .update(|file| {
                let entry = file.agent_ids.get_mut(&quiet.0).unwrap();
                entry.probed_at = Some(OffsetDateTime::now_utc() + time::Duration::hours(1));
            })
            .unwrap();
        probe_each(&app, &store, std::slice::from_ref(&quiet), minute_ago()).await;
        assert_eq!(lines(&log), 3);

        // A fresh reading holds a probe off like a probe does, and each agent has its turn.
        let read = agent_id("probe-read");
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
            .map(|r| (r.agent_id, r.used))
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
            agent_id("start-stale"),
            agent_id("start-fresh"),
            agent_id("start-unread"),
        );
        for each in [&stale, &fresh, &unread] {
            register_prober(each, logging_prober(dir.path(), &log, Some(0.4), 0));
        }
        let board = FilePlanUsageBoard::new(&paths);
        let hours_ago = |hours: i64| OffsetDateTime::now_utc() - time::Duration::hours(hours);
        for (each, at) in [(&stale, hours_ago(3)), (&fresh, hours_ago(0))] {
            board.observe(reading(&each.0, 0.2, at));
        }
        let crew = [stale.clone(), fresh.clone(), unread.clone()];
        let stale_after = Duration::from_secs(30 * 60);
        let read = probe_at_start(&board, &paths, &crew, stale_after).await;
        assert_eq!(read.len(), 1, "{read:?}");
        assert_eq!(read[0].agent_id, stale);
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
    fn only_agents_with_a_probe_are_probed_at_start() {
        let dir = tempfile::tempdir().unwrap();
        register_prober(
            &agent_id("probe-listed"),
            ProbeAdapter::new("echo used=0.1").prober(dir.path()),
        );
        let none = ProbeAdapter {
            script: None,
            ..ProbeAdapter::new("")
        };
        register_prober(&agent_id("probe-unlisted"), none.prober(dir.path()));
        let agent_ids = probed_agents();
        assert!(agent_ids.contains(&agent_id("probe-listed")));
        assert!(!agent_ids.contains(&agent_id("probe-unlisted")));
    }

    #[tokio::test]
    async fn a_session_is_probed_while_it_runs_and_after_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let log = dir.path().join("probes.log");
        let watched = agent_id("probe-watched");
        let prober = logging_prober(dir.path(), &log, Some(0.3), 0);
        let board = Arc::new(FilePlanUsageBoard::new(&paths).with_prober(&watched, prober));
        let store = AgentsStore::new(&paths);
        let every = Duration::from_millis(500);

        let (end, ended) = oneshot::channel();
        let started = Instant::now();
        let relayed = PlanUsageWatch::new(board.clone(), &paths, watched.clone())
            .every(every)
            .relay(ended);
        // Ends right after a probe answered, so none is in flight and the last one is recent.
        wait_for("probes while running", || {
            lines(&log) >= 2
                && probed_at(&store, &watched).is_some_and(|claimed| {
                    board
                        .get(&watched)
                        .is_some_and(|plan_usage| plan_usage.observed_at >= claimed)
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
                    .is_some_and(|plan_usage| plan_usage.observed_at >= ended_at)
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
        let slow = agent_id("probe-slow-watched");
        let prober = logging_prober(dir.path(), &log, None, 2);
        let board = Arc::new(FilePlanUsageBoard::new(&paths).with_prober(&slow, prober));
        let watch = || {
            PlanUsageWatch::new(board.clone(), &paths, slow.clone())
                .every(Duration::from_millis(100))
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
