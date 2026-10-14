//! The sessions a run's agents ran in. Each is a real session of its CLI, so `/resume` lists
//! them under their runs, and once a run ended, its sessions carry on in new conversations
//! through their CLI's own resume, in the folder each ran in. Until then the run owns them: its
//! next round on a task resumes the same CLI session, so nothing else may.
//!
//! The CLIs find a session by that folder: Claude Code keeps its sessions per folder, agy scopes
//! its history to it, and Codex and OpenCode record it with the session. A session carries on
//! only while its CLI is installed and its folder, or the branch `run_worktree` makes it again
//! from, is still there.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use anyhow::{Context as _, bail};
use argonauts_core::agent_sessions::{AgentSession, AgentSessionRef, Helper, Resume};
use argonauts_core::api::RunSummary;
use argonauts_core::event::AgentEvent;
use argonauts_core::events::session::{SessionEvent, SessionStarted, SubagentUpdate};
use argonauts_core::events::task::TaskEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{self, RunLock};
use argonauts_core::layout::run_files;
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::RunnerKind;
use argonauts_core::steps::StepName;
use argonauts_core::{RunId, TaskId};
use time::OffsetDateTime;

use super::run_worktree;
use crate::commands::adapters_here;
use crate::controller::Ctx;
use crate::conversation_store::ConversationPlace;
use crate::git;

/// How many of the newest runs `/resume` reads the sessions of up front. An older run's are
/// read when you open it in the picker, and any session carries on by name.
const LISTED_RUNS: usize = 20;

/// One session, as its run's journal tells it.
#[derive(Debug, Clone)]
pub(super) struct Found {
    pub run: RunId,
    pub key: SessionKey,
    pub role: RoleName,
    pub worker: ModelRef,
    kind: RunnerKind,
    pub task: Option<TaskId>,
    pub task_title: Option<String>,
    step: Option<StepName>,
    /// Its name in the run, such as `build:slugify`, when the run gave it one.
    name: Option<String>,
    /// Where its CLI ran, and so where its CLI finds it again.
    pub folder: Option<PathBuf>,
    /// The branch of the run's worktree it ran in.
    pub branch: Option<String>,
    pub cli_session: Option<String>,
    pub last_message: Option<String>,
    updated: OffsetDateTime,
    ended: bool,
    helpers: Vec<Helper>,
}

impl Found {
    fn new(run: &RunId, key: &SessionKey, started: &SessionStarted, event: &RunEvent) -> Self {
        Self {
            run: run.clone(),
            key: key.clone(),
            role: started.role,
            worker: started.worker.clone(),
            kind: started.kind,
            task: event.task_id.clone(),
            task_title: None,
            step: started.step,
            name: started.label.clone(),
            folder: None,
            branch: None,
            cli_session: None,
            last_message: None,
            updated: event.ts,
            ended: false,
            helpers: Vec::new(),
        }
    }

    /// Its name in the run, such as `build:slugify`, as the conversation and the panel call it. A
    /// run from before agents had names gives its task, or its step when it had none, such as
    /// `task-001` or `judge`.
    fn label(&self) -> Option<String> {
        if let Some(name) = &self.name {
            return Some(name.clone());
        }
        if let Some(task) = &self.task {
            return Some(task.to_string());
        }
        let step = serde_json::to_value(self.step?).ok()?;
        Some(step.as_str()?.replace('_', " "))
    }

    /// Who it is in a sentence: `build:slugify in run-01` by its name, or for a run from before
    /// agents had names, `the worker of task-001 in run-01`.
    pub(super) fn who(&self) -> String {
        if let Some(name) = &self.name {
            return format!("{name} in {}", self.run);
        }
        match &self.task {
            Some(task) => format!("the {} of {task} in {}", self.role.as_str(), self.run),
            None => format!("the {} in {}", self.role.as_str(), self.run),
        }
    }

    fn on_event(&mut self, event: &RunEvent) {
        self.updated = event.ts;
        match &event.kind {
            RunEventKind::Session(SessionEvent::Input(input)) => {
                self.folder.get_or_insert_with(|| input.launch.cwd.clone());
            }
            RunEventKind::Session(SessionEvent::CliSession { id, .. }) => {
                self.cli_session = Some(id.clone());
            }
            RunEventKind::Session(SessionEvent::Ended(end)) => {
                self.ended = true;
                if let Some(id) = &end.cli_session {
                    self.cli_session = Some(id.clone());
                }
                if let Some(text) = &end.last_message {
                    self.last_message = Some(text.clone());
                }
            }
            RunEventKind::Agent(AgentEvent::Message { text, parent: None }) => {
                self.last_message = Some(text.clone());
            }
            RunEventKind::Agent(AgentEvent::Subagent { update }) => self.helper(update),
            _ => {}
        }
    }

    /// The CLI says more of a helper as it goes, so the latest word on each wins.
    fn helper(&mut self, update: &SubagentUpdate) {
        let helper = Helper {
            id: update.id.clone(),
            kind: update.kind,
            label: update.label.clone(),
            status: update.status,
            last_message: update
                .last_message
                .clone()
                .or_else(|| update.prompt.clone()),
        };
        match self.helpers.iter_mut().find(|known| known.id == update.id) {
            Some(known) => {
                let last_message = helper.last_message.or(known.last_message.take());
                *known = Helper {
                    label: helper.label.or(known.label.take()),
                    last_message,
                    ..helper
                };
            }
            None => self.helpers.push(helper),
        }
    }
}

/// Every session in the journal, in the order they started.
pub(super) fn sessions_of(run: &RunId, events: &[RunEvent]) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let mut tree_branches: HashMap<PathBuf, String> = HashMap::new();
    let mut task_branches: HashMap<TaskId, String> = HashMap::new();
    let mut task_titles: HashMap<TaskId, String> = HashMap::new();
    for event in events {
        match (&event.kind, &event.session) {
            (RunEventKind::Task(TaskEvent::Worktree { path, branch, .. }), _) => {
                tree_branches.insert(path.clone(), branch.clone());
                if let Some(task) = &event.task_id {
                    task_branches.insert(task.clone(), branch.clone());
                }
            }
            (RunEventKind::Task(TaskEvent::Created(spec)), _) => {
                task_titles.insert(spec.id.clone(), spec.title.clone());
            }
            (RunEventKind::Session(SessionEvent::Started(started)), Some(key)) => {
                found.push(Found::new(run, key, started, event));
            }
            (_, Some(key)) => {
                if let Some(session) = found.iter_mut().rev().find(|found| &found.key == key) {
                    session.on_event(event);
                }
            }
            _ => {}
        }
    }
    for session in &mut found {
        // A retry works in a worktree of its own, on a branch of its own.
        session.branch = session
            .folder
            .as_ref()
            .and_then(|folder| tree_branches.get(folder))
            .or_else(|| task_branches.get(session.task.as_ref()?))
            .cloned();
        session.task_title = session
            .task
            .as_ref()
            .and_then(|task| task_titles.get(task))
            .cloned();
    }
    found
}

/// Each journal's sessions as last read, with the journal's length and modification time then,
/// so `/resume` reads again only the journals that changed since.
#[derive(Default)]
pub(crate) struct SessionCache {
    read: Mutex<HashMap<PathBuf, Cached>>,
}

struct Cached {
    len: u64,
    modified: SystemTime,
    sessions: Arc<Vec<Found>>,
}

impl SessionCache {
    fn lock(&self) -> MutexGuard<'_, HashMap<PathBuf, Cached>> {
        self.read.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sessions in `run_dir`'s journal, none when it has no journal yet.
    fn sessions(&self, run_dir: &Path, run: &RunId) -> anyhow::Result<Arc<Vec<Found>>> {
        let path = run_dir.join(run_files::EVENTS);
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Arc::default()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let (len, modified) = (meta.len(), meta.modified()?);
        if let Some(cached) = self.lock().get(&path)
            && cached.len == len
            && cached.modified == modified
        {
            return Ok(Arc::clone(&cached.sessions));
        }
        let events = journal::read_all(&path)
            .with_context(|| format!("reading {}", path.display()))?
            .events;
        let sessions = Arc::new(sessions_of(run, &events));
        let cached = Cached {
            len,
            modified,
            sessions: Arc::clone(&sessions),
        };
        self.lock().insert(path, cached);
        Ok(sessions)
    }
}

/// What each session of a listing is checked against, found once for the listing.
struct Checks {
    /// Every branch in the repo, or `None` when git couldn't list them.
    branches: Option<HashSet<String>>,
    /// The workers whose CLI is installed and on.
    installed: HashSet<String>,
}

/// The repo's branches by name, or `None` when git couldn't list them.
fn branches(repo: &Path) -> Option<HashSet<String>> {
    let listed = git::git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"]);
    match listed {
        Ok(text) => Some(
            text.lines()
                .filter_map(|line| line.strip_prefix("refs/heads/"))
                .map(str::to_owned)
                .collect(),
        ),
        Err(err) => {
            tracing::warn!("listing the repo's branches: {err:#}");
            None
        }
    }
}

fn installed(ctx: &Ctx) -> HashSet<String> {
    ctx.services
        .workers
        .available()
        .into_iter()
        .map(|worker| worker.agent)
        .collect()
}

/// The sessions of this repo's newest runs, each run's in the order they started, for
/// `/resume`, and which runs those are. A journal that can't be read lists none, since the rest
/// of `/resume` still works.
pub(crate) async fn list(ctx: &Ctx, runs: &[RunSummary]) -> (Vec<AgentSession>, Vec<RunId>) {
    let listed: Vec<&RunSummary> = runs.iter().take(LISTED_RUNS).collect();
    let agents = read(ctx, &listed).await;
    (agents, listed.iter().map(|run| run.run.clone()).collect())
}

/// One run's sessions, for a run `/resume` listed without them.
pub(crate) async fn list_run(ctx: &Ctx, run: &RunId) -> anyhow::Result<Vec<AgentSession>> {
    let runs = ctx.services.conductor.list().await?;
    let summary = runs
        .iter()
        .find(|summary| &summary.run == run)
        .with_context(|| format!("There's no run {run} in this repo. /resume lists them."))?;
    Ok(read(ctx, &[summary]).await)
}

async fn read(ctx: &Ctx, runs: &[&RunSummary]) -> Vec<AgentSession> {
    let cache = Arc::clone(&ctx.agent_sessions);
    let repo = ctx.env.repo_root.clone();
    let dirs: Vec<(RunId, PathBuf)> = runs
        .iter()
        .map(|run| (run.run.clone(), ctx.env.run_dir(&run.run)))
        .collect();
    let read = tokio::task::spawn_blocking(move || {
        let sessions: Vec<Arc<Vec<Found>>> = dirs
            .iter()
            .map(|(run, dir)| {
                cache.sessions(dir, run).unwrap_or_else(|err| {
                    tracing::warn!("listing the sessions of {run}: {err:#}");
                    Arc::default()
                })
            })
            .collect();
        (sessions, branches(&repo))
    })
    .await;
    let (sessions, branches) = match read {
        Ok(read) => read,
        Err(err) => {
            tracing::warn!("listing the runs' sessions: {err}");
            return Vec::new();
        }
    };
    let checks = Checks {
        branches,
        installed: installed(ctx),
    };
    let mut agents = Vec::new();
    for (run, found) in runs.iter().zip(&sessions) {
        for found in found.iter() {
            agents.push(AgentSession {
                run: found.run.clone(),
                session: found.key.clone(),
                role: found.role,
                label: found.label(),
                worker: found.worker.clone(),
                last_message: found.last_message.as_deref().map(one_line),
                updated: found.updated,
                resume: resume_of(ctx, found, Some(run), &checks),
                helpers: found
                    .helpers
                    .iter()
                    .map(|helper| Helper {
                        last_message: helper.last_message.as_deref().map(one_line),
                        ..helper.clone()
                    })
                    .collect(),
            });
        }
    }
    agents
}

/// The session `reference` names, and whether it can carry on.
pub(super) async fn pick(
    ctx: &Ctx,
    reference: &AgentSessionRef,
) -> anyhow::Result<(Found, Resume)> {
    let run = reference.run.clone();
    let dir = ctx.env.run_dir(&run);
    if !dir.is_dir() {
        bail!("There's no run {run} in this repo. /resume lists them.");
    }
    let cache = Arc::clone(&ctx.agent_sessions);
    let repo = ctx.env.repo_root.clone();
    let (sessions, branches) = tokio::task::spawn_blocking(move || {
        cache
            .sessions(&dir, &run)
            .map(|sessions| (sessions, branches(&repo)))
    })
    .await
    .context("reading the run's journal")??;
    let found = sessions
        .iter()
        .rev()
        .find(|found| found.key == reference.session)
        .cloned()
        .with_context(|| {
            format!(
                "{} has no session {}. /resume lists them under it.",
                reference.run, reference.session
            )
        })?;
    let summary = summary_of(ctx, &reference.run).await;
    let checks = Checks {
        branches,
        installed: installed(ctx),
    };
    let resume = resume_of(ctx, &found, summary.as_ref(), &checks);
    Ok((found, resume))
}

async fn summary_of(ctx: &Ctx, run: &RunId) -> Option<RunSummary> {
    let runs = ctx.services.conductor.list().await;
    runs.ok()?.into_iter().find(|summary| &summary.run == run)
}

/// Whether `found` carries on here: its run ended and no run host holds it, its CLI is here
/// and resumes, and its folder is there or can be made again. A session still running is taken
/// over instead.
fn resume_of(ctx: &Ctx, found: &Found, run: Option<&RunSummary>, checks: &Checks) -> Resume {
    let cannot = |why: String| Resume::Cannot { why };
    let Some(run) = run else {
        return cannot("Argonauts Code can't tell whether its run ended".to_owned());
    };
    if run.live && !found.ended {
        return Resume::Working;
    }
    if run.live || !run.state.ended() {
        return Resume::RunOwns;
    }
    let cli = ctx.names.cli(&found.worker.agent);
    if found.kind == RunnerKind::Endpoint {
        return cannot("an endpoint keeps no session".to_owned());
    }
    if found.cli_session.is_none() {
        return cannot(format!("{cli} left no session to resume"));
    }
    if let Some(why) = cli_cannot(ctx, &found.worker, &checks.installed) {
        return cannot(why);
    }
    let folder = found.folder.as_deref();
    let branches = checks.branches.as_ref();
    if let Some(why) = worktree_cannot(folder, found.branch.as_deref(), branches) {
        return cannot(why);
    }
    Resume::Ready
}

/// Why `worker`'s CLI can't resume a session here, if it can't.
fn cli_cannot(ctx: &Ctx, worker: &ModelRef, installed: &HashSet<String>) -> Option<String> {
    let cli = ctx.names.cli(&worker.agent);
    match ctx.services.workers.get(worker) {
        Err(_) => return Some(format!("{cli} isn't set up here")),
        Ok(handle) if !handle.capabilities().resume => {
            return Some(format!("{cli} can't resume a session"));
        }
        Ok(_) => {}
    }
    // Any built-in CLI gets a worker, installed or not, so ask the list of those that are.
    if !installed.contains(&worker.agent) {
        return Some(format!("{} isn't installed", binary(ctx, &worker.agent)));
    }
    None
}

/// Why the folder a session ran in can't be had again, if it can't: it's gone, and so is the
/// branch to make it again from.
fn worktree_cannot(
    folder: Option<&Path>,
    branch: Option<&str>,
    branches: Option<&HashSet<String>>,
) -> Option<String> {
    if folder.is_some_and(Path::is_dir) {
        return None;
    }
    let Some(branch) = branch else {
        return Some("its folder is gone".to_owned());
    };
    match branches {
        Some(branches) if branches.contains(branch) => None,
        Some(_) => Some(format!(
            "its worktree is gone, and so is its branch {branch}"
        )),
        None => Some(format!(
            "its worktree is gone, and its branch {branch} can't be checked"
        )),
    }
}

/// Why a conversation that carries on an agent's session can't go on, checked again when you
/// come back to it: its run is going again, its CLI is gone, or so is its worktree and the branch
/// to make it from.
pub(super) async fn place_cannot(ctx: &Ctx, place: &ConversationPlace) -> Option<String> {
    let run = &place.agent.run;
    // A run cleaned away can't start the session again.
    if ctx.env.run_dir(run).is_dir() {
        match summary_of(ctx, run).await {
            None => return Some(format!("Argonauts Code can't tell whether {run} ended")),
            Some(summary) if summary.live || !summary.state.ended() => {
                return Some(owned_by(&place.agent));
            }
            Some(_) => {}
        }
    }
    if let Some(why) = cli_cannot(ctx, &place.worker, &installed(ctx)) {
        return Some(why);
    }
    let repo = ctx.env.repo_root.clone();
    let branches = tokio::task::spawn_blocking(move || branches(&repo))
        .await
        .ok()
        .flatten();
    worktree_cannot(
        Some(&place.folder),
        place.branch.as_deref(),
        branches.as_ref(),
    )
}

/// The run went again since the conversation carried its session on, and a run host holds
/// it now, so a turn here would write the same CLI session as the run.
pub(super) fn run_holds(ctx: &Ctx, place: &ConversationPlace) -> bool {
    RunLock::is_held(&ctx.env.run_dir(&place.agent.run))
}

/// Why a session can't carry on while its run hasn't ended.
pub(super) fn owned_by(agent: &AgentSessionRef) -> String {
    format!(
        "{run} hasn't ended, so it still owns {agent}. t in its agent view takes it over, or it \
         carries on here once the run ends",
        run = agent.run
    )
}

/// Where the session carries on: the folder it ran in, made again from its branch when the run's
/// worktree is gone. True when it was made again.
pub(super) fn place(repo: &Path, found: &Found) -> anyhow::Result<(PathBuf, bool)> {
    let folder = found
        .folder
        .clone()
        .context("its run's journal doesn't say where it ran")?;
    let made_again = run_worktree::ensure_worktree(repo, &folder, found.branch.as_deref())?;
    Ok((folder, made_again))
}

/// The CLI's command, such as `claude`, for saying whose resume carries the session on.
pub(super) fn binary(ctx: &Ctx, worker: &str) -> String {
    adapters_here(Some(&ctx.env), ctx.settings.resolved())
        .ok()
        .and_then(|adapters| adapters.into_iter().find(|adapter| adapter.id() == worker))
        .map_or_else(
            || ctx.names.cli(worker),
            |adapter| adapter.binary().to_owned(),
        )
}

/// Agent text on one line, for a list.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::session::{SubagentKind, SubagentStatus};
    use argonauts_testkit::TempRepo;

    use super::*;

    fn update(id: &str, label: Option<&str>, status: SubagentStatus) -> SubagentUpdate {
        SubagentUpdate {
            id: id.to_owned(),
            parent: None,
            kind: SubagentKind::Subagent,
            label: label.map(str::to_owned),
            prompt: Some("Read the parser".to_owned()),
            status,
            last_message: None,
            log_uri: None,
            steps_visible: true,
            tokens: None,
        }
    }

    fn worker(task: Option<&str>, step: Option<StepName>, name: Option<&str>) -> Found {
        Found {
            run: RunId::new("run-01").unwrap(),
            key: SessionKey::numbered(1),
            role: RoleName::Worker,
            worker: "claude-code".parse().unwrap(),
            kind: RunnerKind::Cli,
            task: task.map(|task| TaskId::new(task).unwrap()),
            task_title: None,
            step,
            name: name.map(str::to_owned),
            folder: None,
            branch: None,
            cli_session: None,
            last_message: None,
            updated: OffsetDateTime::UNIX_EPOCH,
            ended: false,
            helpers: Vec::new(),
        }
    }

    #[test]
    fn a_helper_keeps_its_name_as_the_cli_says_more() {
        let mut found = worker(None, Some(StepName::Judge), None);
        found.helper(&update(
            "toolu_01",
            Some("explorer"),
            SubagentStatus::Running,
        ));
        found.helper(&update("toolu_01", None, SubagentStatus::Done));
        assert_eq!(found.helpers.len(), 1);
        assert_eq!(found.helpers[0].label.as_deref(), Some("explorer"));
        assert_eq!(found.helpers[0].status, SubagentStatus::Done);
        assert_eq!(found.label().as_deref(), Some("judge"));
        assert_eq!(found.who(), "the worker in run-01");
    }

    /// An agent the run named goes by that name, as the conversation, the panel, and the workflow
    /// view call it, and its task stays for Ctrl+O and commands. A run from before names gives the
    /// task.
    #[test]
    fn an_agent_goes_by_the_name_its_run_gave_it() {
        let named = worker(Some("task-001"), Some(StepName::Build), Some("build:money"));
        assert_eq!(named.label().as_deref(), Some("build:money"));
        assert_eq!(named.who(), "build:money in run-01");
        let unnamed = worker(Some("task-001"), Some(StepName::Build), None);
        assert_eq!(unnamed.label().as_deref(), Some("task-001"));
        assert_eq!(unnamed.who(), "the worker of task-001 in run-01");
    }

    /// A journal is read again only once it changed, so a long list of runs costs a read of
    /// each only the first time.
    #[test]
    fn a_journal_is_read_again_only_once_it_changed() {
        let repo = TempRepo::new().unwrap();
        let run = RunId::new("run-01").unwrap();
        let dir = repo.home().join("runs/run-01");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = SessionCache::default();
        assert!(
            cache.sessions(&dir, &run).unwrap().is_empty(),
            "no journal yet"
        );

        let event = |seq: u64, session: u32, kind: RunEventKind| RunEvent {
            seq,
            ts: OffsetDateTime::now_utc(),
            run_id: run.clone(),
            task_id: None,
            session: Some(SessionKey::numbered(session)),
            source: "argonauts-code".to_owned(),
            kind,
        };
        let started = |session: u32| {
            let started = SessionStarted {
                agent_id: argonauts_core::names::AgentId("codex".to_owned()),
                provider: "codex".to_owned(),
                worker: "codex".parse().unwrap(),
                kind: RunnerKind::Cli,
                role: RoleName::Worker,
                step: Some(StepName::Build),
                access: argonauts_core::session_runner::Access::Build,
                lifetime: argonauts_core::settings::sessions::SessionLifetime::Task,
                resumed_from: None,
                follow_key: None,
                workflow_step: None,
                label: Some(format!("build:item-{session}")),
                asked: None,
            };
            event(
                u64::from(session),
                session,
                RunEventKind::Session(SessionEvent::Started(Box::new(started))),
            )
        };
        let path = dir.join(run_files::EVENTS);
        let line = |event: RunEvent| format!("{}\n", serde_json::to_string(&event).unwrap());
        std::fs::write(&path, line(started(1))).unwrap();
        let first = cache.sessions(&dir, &run).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].label().as_deref(), Some("build:item-1"));
        assert!(Arc::ptr_eq(&first, &cache.sessions(&dir, &run).unwrap()));

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, line(started(2)).as_bytes()).unwrap();
        let again = cache.sessions(&dir, &run).unwrap();
        assert_eq!(again.len(), 2);
    }
}
