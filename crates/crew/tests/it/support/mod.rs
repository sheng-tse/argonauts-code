//! Shared helpers for the engine scenarios: a crew of fake workers over a real temp repo, runs
//! driven to where they wait or finish, and answers to their approvals. Each `tests/engine_*.rs`
//! file includes it with `mod support;`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use argonauts_core::agents::session_runner::{RunnerEvent, SessionRunner, SessionSpec};
use argonauts_core::base::clock::Clock;
use argonauts_core::base::paths::Paths;
use argonauts_core::runs::api::ApprovalAnswer;
use argonauts_core::runs::engine::{EngineCommand, EngineContext, EngineExit, RepoContext};
use argonauts_core::runs::events::approval::{AnswerAction, ApprovalEvent, ApprovalKind};
use argonauts_core::runs::events::run::{RunLifecycle, RunMode, RunStarted};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::fallback::Retry;
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::settings::{Settings, Span};
use argonauts_core::workflows::steps::StepName;
use argonauts_core::{AgentEvent, RepoId, RunId};
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::{Shared, building_with_git_env, with_git_env};
use argonauts_crew::prompts::trust;
use argonauts_crew::state::RunState;
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};
use argonauts_testkit::engine::services::{FakeServices, MemorySink, TEST_RUN, engine_context_in};
use argonauts_testkit::home::repo::TempRepo;

/// A crew of fake workers, the fake services, and a temp repo with one commit.
pub struct Crew {
    pub services: FakeServices,
    pub repo: TempRepo,
    pub settings: Settings,
    /// A `PATH` for the engine's git and the tools it runs beside it, where a scenario stands
    /// in for one, such as `gh`.
    pub path: Option<String>,
    /// What the orchestrator begins from, as a run your words in a conversation records it.
    pub starts_from: Option<argonauts_core::runs::events::run::StartsFrom>,
}

/// A run going on in its own task.
pub struct Spawned {
    pub shutdown: tokio_util::sync::CancellationToken,
    pub commands: mpsc::Sender<EngineCommand>,
    pub sink: Arc<MemorySink>,
    pub exit: tokio::task::JoinHandle<EngineExit>,
}

/// Where a run stopped, with everything it logged so far: the whole log, from the history it
/// was given on, so it can be handed straight to [`Crew::resume`].
pub struct Outcome {
    pub exit: EngineExit,
    pub events: Vec<RunEvent>,
}

impl Crew {
    /// The default settings with an empty crew, running the crew's fixed phases: blind drafts,
    /// the map, critique and judging, which most scenarios script, while Council's talk has
    /// scenarios of its own. A run with an approval waiting waits at once instead of after the
    /// usual grace, and a session that failed for an agent reason goes again at once, so
    /// scenarios don't wait. The main agent doesn't design the workflow and no final check
    /// runs, unless a scenario turns them on, so scripted answers go to the steps they were
    /// written for. For the same reason a built-in workflow runs its fixed phases here, and the
    /// scenarios of a workflow's own steps ask for the script.
    pub fn new() -> anyhow::Result<Self> {
        let mut settings = Settings {
            workflow: argonauts_core::workflows::workflow_files::BASE.0.to_owned(),
            ..Settings::default()
        };
        settings.crew.clear();
        settings.workflows.adapt_with = AdaptWith::Phases;
        settings.run_host.wait_grace = Span::new("0s");
        settings.fallback.retry = Some(Retry::without_wait());
        settings.workflows.orchestrator_designs = false.into();
        settings.workflows.final_check = false;
        Ok(Self {
            services: FakeServices::new(),
            repo: TempRepo::new()?,
            settings,
            path: None,
            starts_from: None,
        })
    }

    /// Adds a fake CLI worker such as `claude-code/opus` of `provider` to the directory and
    /// to `settings.crew`. The first one added is also the main agent.
    pub fn worker(&mut self, worker: &str, provider: &str) -> Arc<FakeRunner> {
        self.enlist(FakeRunner::cli(worker, provider))
    }

    /// Adds a fake CLI worker that answers every step with the smallest valid answer for
    /// the step's schema, and ends a session without one as done.
    pub fn answering_worker(&mut self, worker: &str, provider: &str) -> Arc<FakeRunner> {
        self.enlist(FakeRunner::cli(worker, provider).respond_with(smallest_answer))
    }

    /// An [`Crew::answering_worker`] whose build sessions also write `text` to `file` in their
    /// worktree, so the task has a commit to make and to merge.
    pub fn building_worker(
        &mut self,
        worker: &str,
        provider: &str,
        file: &str,
        text: &str,
    ) -> Arc<FakeRunner> {
        let (file, text) = (file.to_owned(), text.to_owned());
        let fake = FakeRunner::cli(worker, provider).respond_with(move |spec| {
            if spec.step != Some(StepName::Build) {
                return smallest_answer(spec);
            }
            std::fs::write(spec.workdir.join(&file), &text)
                .expect("the build session writes its file");
            smallest_answer(spec).with_event(wrote(&file))
        });
        self.enlist(fake)
    }

    pub fn enlist(&mut self, fake: FakeRunner) -> Arc<FakeRunner> {
        let fake = self.services.workers.add(fake);
        let reference = fake.model_ref().clone();
        if self.settings.crew.is_empty() {
            self.settings.crew.main = reference.clone();
        }
        if !self.settings.crew.contains(&reference) {
            self.settings.crew.add(reference);
        }
        fake
    }

    /// A council of your own in the repo, made of steps the orchestrator writes a script for,
    /// as a file of steps named after the built-in one replaces it. Scenarios of the script's
    /// engine use it; the built-in Council talks instead.
    pub fn with_council_steps(&self) -> anyhow::Result<()> {
        self.repo
            .write(".argonauts/workflows/council.toml", COUNCIL_STEPS)?;
        Ok(())
    }

    /// The temp repo's home and git config, for the engine's git.
    pub fn git_env(&self) -> Vec<(String, String)> {
        let path = self.path.clone().map(|path| ("PATH".to_owned(), path));
        git_env_of(&self.repo).into_iter().chain(path).collect()
    }

    /// The engine's shared state over `context`, built by hand, its git on the temp repo's
    /// home and config.
    pub fn shared(&self, context: &EngineContext, state: RunState) -> anyhow::Result<Shared> {
        Ok(building_with_git_env(self.git_env(), || {
            Shared::new(context, state)
        })?)
    }

    /// Runs `request` to its end, answering each approval it waits on with its recommended
    /// option. Returns every stretch the engine ran, the last one finished or still waiting
    /// after `rounds` answers.
    pub async fn run_to_end(
        &self,
        mode: RunMode,
        request: &str,
        rounds: usize,
    ) -> anyhow::Result<Vec<Outcome>> {
        let mut outcomes = vec![self.start(mode, request).await?];
        for _ in 0..rounds {
            let last = &outcomes[outcomes.len() - 1];
            if !matches!(last.exit, EngineExit::Waiting(_)) {
                break;
            }
            let Some((command, _reply)) = recommended(&last.events) else {
                break;
            };
            let next = self.resume(last.events.clone(), vec![command]).await?;
            outcomes.push(next);
        }
        Ok(outcomes)
    }

    /// Records that the run follows a learned workflow, the one thing a run review's proposal
    /// can change, so the run review is asked for one.
    pub fn with_a_learned_workflow(&self) -> anyhow::Result<()> {
        use argonauts_crew::run_review::learned_workflow::{self, RunLearnedWorkflow};
        let recorded = RunLearnedWorkflow {
            learned_workflow: argonauts_learning::LearnedWorkflowRef {
                name: "cli-flag".to_owned(),
                version: 1,
            },
            sha256: "cd".repeat(32),
            canary: false,
            dir: None,
        };
        learned_workflow::write(&self.repo_context()?.run_dir, &recorded)?;
        Ok(())
    }

    /// The run's directories in this repo's temp home.
    pub fn repo_context(&self) -> anyhow::Result<RepoContext> {
        let run = RunId::new(TEST_RUN).context("the test run id")?;
        let id = RepoId::from_canonical_path(self.repo.root());
        let paths = Paths::new(self.repo.home());
        let worktrees = self
            .repo
            .home()
            .join("worktrees")
            .join(id.as_str())
            .join(run.as_str());
        Ok(RepoContext {
            run_dir: paths.run_dir(&id, &run),
            base_worktree: worktrees.join("base"),
            worktrees,
            root: self.repo.root().to_path_buf(),
            id,
        })
    }

    /// A context over this repo's real layout, rebuilding from `history`. The project files
    /// a test wrote and committed are trusted as they are, as you would when asked once.
    pub fn context(
        &self,
        history: Vec<RunEvent>,
    ) -> anyhow::Result<(EngineContext, Arc<MemorySink>, mpsc::Sender<EngineCommand>)> {
        let repo = self.repo_context()?;
        if let Some(state_dir) = repo.run_dir.parent().and_then(Path::parent) {
            let waiting: Vec<PathBuf> = trust::waiting(&repo.root, state_dir).all().collect();
            trust::trust(state_dir, &waiting)?;
        }
        Ok(engine_context_in(
            &self.services,
            self.settings.clone(),
            repo,
            history,
        ))
    }

    /// The `RunStarted` the run host writes before the engine runs.
    /// The repo's head, or the head of the history Argonauts Code keeps for a folder with no git.
    pub fn base_commit(&self) -> anyhow::Result<String> {
        let state_dir = self.repo_context()?.state_dir().to_path_buf();
        match argonauts_core::runs::hidden_git::of(self.repo.root(), &state_dir) {
            Some(hidden) => {
                let git_dir = format!("--git-dir={}", hidden.display());
                self.repo.git(&[&git_dir, "rev-parse", "HEAD"])
            }
            None => self.repo.head(),
        }
    }

    /// The branch checked out as the run starts, which the app records: none for a detached
    /// HEAD. A folder whose `.git` a test moved aside into a hidden history keeps that
    /// history's one branch.
    fn base_branch(&self) -> Option<String> {
        if !self.repo.root().join(".git").exists() {
            return Some("main".to_owned());
        }
        self.repo
            .git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|branch| branch.trim().to_owned())
            .filter(|branch| !branch.is_empty())
    }

    pub fn run_started(&self, mode: RunMode, request: &str) -> anyhow::Result<RunEvent> {
        let (context, _, _) = self.context(Vec::new())?;
        let started = RunStarted {
            request: request.to_owned(),
            title: request.to_owned(),
            mode,
            forced: false,
            workflow: self.settings.workflow.clone(),
            estimate: None,
            captain_or_crew: None,
            conversation_turn: None,
            continues: None,
            repo_root: self.repo.root().to_path_buf(),
            base_branch: self.base_branch(),
            base_commit: self.base_commit()?,
            uncommitted: Vec::new(),
            uncommitted_included: false,
            settings_sha256: context.settings_sha256.clone(),
            settings: serde_json::to_value(&self.settings)?,
            overrides: Vec::new(),
            cli_versions: BTreeMap::new(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            outside_agent: None,
            instructions: Vec::new(),
            required_steps: Vec::new(),
            starts_from: self.starts_from.clone(),
        };
        Ok(RunEvent {
            seq: 0,
            ts: self.services.clock.now(),
            run_id: context.run,
            task_id: None,
            session: None,
            source: "run-host".to_owned(),
            kind: RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
        })
    }

    /// A host over a run that has only started, for driving `Host` calls without the loop.
    pub fn host(&self, mode: RunMode) -> anyhow::Result<(Host, Arc<MemorySink>)> {
        let started = self.run_started(mode, "a test request")?;
        self.host_over(vec![started])
    }

    /// A host rebuilt from `history`, as the engine builds it.
    pub fn host_over(&self, history: Vec<RunEvent>) -> anyhow::Result<(Host, Arc<MemorySink>)> {
        let state = RunState::fold(&history);
        let (context, sink, _) = self.context(history)?;
        let shared = self.shared(&context, state)?;
        Ok((Host::new(Arc::new(shared)), sink))
    }

    /// Starts a run for `request` (its `RunStarted` written as the run host would) and drives
    /// the engine until it waits or finishes.
    pub async fn start(&self, mode: RunMode, request: &str) -> anyhow::Result<Outcome> {
        let started = self.run_started(mode, request)?;
        self.drive(mode, vec![started], Vec::new(), false).await
    }

    /// Wakes a waiting run with `commands` queued in its inbox, as the run host does after an
    /// answer, and drives it until it waits or finishes again.
    pub async fn resume(
        &self,
        history: Vec<RunEvent>,
        commands: Vec<EngineCommand>,
    ) -> anyhow::Result<Outcome> {
        self.drive(mode_of(&history), history, commands, false)
            .await
    }

    /// Reopens a stopped run, as `/resume` asks its next run host to, and drives it until it
    /// waits or finishes again.
    pub async fn reopen(&self, history: Vec<RunEvent>) -> anyhow::Result<Outcome> {
        self.drive(mode_of(&history), history, Vec::new(), true)
            .await
    }

    /// Starts a run in its own task and returns what a test needs to act on it mid-run:
    /// its shutdown token, its inbox, and its log. Hold `repo.env_guard()` while it runs.
    pub fn spawn(&self, mode: RunMode, request: &str) -> anyhow::Result<Spawned> {
        let started = self.run_started(mode, request)?;
        let (context, sink, commands) = self.context(vec![started])?;
        let shutdown = context.shutdown.clone();
        let engine = argonauts_crew::engine_for(&self.settings, mode);
        let exit = tokio::spawn(engine.run(context));
        Ok(Spawned {
            shutdown,
            commands,
            sink,
            exit,
        })
    }

    /// Wakes a waiting run with `commands` queued, as [`Crew::resume`] does, but in its own task
    /// like [`Crew::spawn`], so a test can act on it mid-run. Hold `repo.env_guard()` while it
    /// runs.
    pub async fn resume_spawned(
        &self,
        history: Vec<RunEvent>,
        commands: Vec<EngineCommand>,
    ) -> anyhow::Result<Spawned> {
        let mode = mode_of(&history);
        let (context, sink, sender) = self.context(history)?;
        for command in commands {
            if sender.send(command).await.is_err() {
                bail!("the engine's inbox closed before it started");
            }
        }
        let shutdown = context.shutdown.clone();
        let engine = argonauts_crew::engine_for(&self.settings, mode);
        let exit = tokio::spawn(engine.run(context));
        Ok(Spawned {
            shutdown,
            commands: sender,
            sink,
            exit,
        })
    }

    async fn drive(
        &self,
        mode: RunMode,
        history: Vec<RunEvent>,
        commands: Vec<EngineCommand>,
        reopen: bool,
    ) -> anyhow::Result<Outcome> {
        let (mut context, sink, sender) = self.context(history)?;
        context.reopen = reopen;
        for command in commands {
            if sender.send(command).await.is_err() {
                bail!("the engine's inbox closed before it started");
            }
        }
        // The engine's git gets the test repo's home and config passed to it rather than
        // through the process environment, so runs over separate repos go side by side.
        let engine = argonauts_crew::engine_for(&self.settings, mode).run(context);
        let exit = with_git_env(self.git_env(), engine).await;
        Ok(Outcome {
            exit,
            events: sink.events(),
        })
    }
}

/// How the `gh` a scenario installs answers `gh pr create`.
#[derive(Debug, Clone, Copy)]
pub enum Gh {
    /// Prints the pull request's address.
    Opens(&'static str),
    /// Fails with this on stderr.
    Fails(&'static str),
}

impl Crew {
    /// Gives the engine a `PATH` of its own: git as this process finds it, and `gh` only when
    /// `gh` says how it answers. That `gh` writes the arguments of each call, one to a line,
    /// into the file this returns. The machine's own `gh` is never run.
    pub fn with_tools(&mut self, gh: Option<Gh>) -> anyhow::Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;

        let tools = self.repo.scratch().join("tools");
        std::fs::create_dir_all(&tools)?;
        let log = tools.join("gh.log");
        let script = |name: &str, body: String| -> anyhow::Result<()> {
            let path = tools.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}"))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            Ok(())
        };
        let git = which::which("git").context("git is installed")?;
        script("git", format!("exec '{}' \"$@\"\n", git.display()))?;
        let logged = format!("printf '%s\\n' \"$@\" >> '{}'\n", log.display());
        match gh {
            Some(Gh::Opens(url)) => script("gh", format!("{logged}echo '{url}'\n"))?,
            Some(Gh::Fails(why)) => script("gh", format!("{logged}echo '{why}' >&2\nexit 1\n"))?,
            None => {}
        }
        self.path = Some(tools.to_string_lossy().into_owned());
        Ok(log)
    }

    /// Adds a bare repository as `origin` and pushes `main` to it. Returns its folder.
    pub fn with_remote(&self) -> anyhow::Result<PathBuf> {
        let remote = self.repo.scratch().join("origin.git");
        let path = remote.to_string_lossy().into_owned();
        self.repo.git(&["init", "--bare", "--quiet", &path])?;
        self.repo.git(&["remote", "add", "origin", &path])?;
        self.repo.git(&["push", "--quiet", "origin", "main"])?;
        Ok(remote)
    }
}

/// The mode the run started in.
fn mode_of(history: &[RunEvent]) -> RunMode {
    history
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::Started(started)) => Some(started.mode),
            _ => None,
        })
        .unwrap_or(RunMode::Captain)
}

/// What a CLI's edit tool reports when it writes `file`, which is how a new file comes to be
/// the agent's own and not something a command left behind.
pub fn wrote(file: &str) -> RunnerEvent {
    RunnerEvent::Agent(AgentEvent::FileChanged {
        path: file.to_owned(),
        kind: "add".to_owned(),
    })
}

/// A council written as steps: everyone proposes, challenges and builds, and a judge decides.
pub const COUNCIL_STEPS: &str = r#"name  = "council"
about = "No single planner: everyone proposes and challenges, the judge decides"
adapt = true

[[step]]
name        = "Propose"
role        = "worker"
count       = "all"
independent = true
keep        = true

[[step]]
name  = "Challenge"
role  = "worker"
count = "all"
does  = "Argue against the other proposals. Their authors are hidden."
keep  = true

[[step]]
name = "Decide"
role = "judge"
does = "Pick the strongest plan or join the best parts. Split it into tasks."
keep = true

[[step]]
name     = "Build"
role     = "worker"
for_each = "task"

[[step]]
name           = "Cross-review"
role           = "worker"
for_each       = "task"
not_the_author = true
send_back_to   = "Build"

[[step]]
name = "Sign-off"
role = "judge"
"#;

/// The smallest valid answer for the step's schema, or a plain end when it has none.
pub fn smallest_answer(spec: &SessionSpec) -> FakeSession {
    match &spec.json_schema {
        Some(schema) => FakeSession::structured(fill(schema)),
        None => FakeSession::completed("done"),
    }
}

/// An `Answer` for the newest open approval of `kind`, carrying its evidence hash, and the
/// receiver for the engine's reply.
pub fn answer(
    events: &[RunEvent],
    kind: ApprovalKind,
    action: AnswerAction,
    text: Option<&str>,
) -> anyhow::Result<(EngineCommand, oneshot::Receiver<Result<(), String>>)> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.kind == kind => {
                open.push(opened.clone());
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.retain(|opened| opened.id != decided.id);
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                open.retain(|opened| &opened.id != id);
            }
            _ => {}
        }
    }
    let Some(opened) = open.pop() else {
        bail!("no open {kind:?} approval in {} events", events.len());
    };
    let (reply, replied) = oneshot::channel();
    let answer = ApprovalAnswer {
        id: opened.id,
        action,
        text: text.map(str::to_owned),
        evidence_sha256: opened.evidence_sha256,
        tasks: Vec::new(),
    };
    Ok((EngineCommand::Answer { answer, reply }, replied))
}

/// The recommended answer to the newest open approval of any kind.
pub fn recommended(
    events: &[RunEvent],
) -> Option<(EngineCommand, oneshot::Receiver<Result<(), String>>)> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => open.push(opened.clone()),
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.retain(|opened| opened.id != decided.id);
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                open.retain(|opened| &opened.id != id);
            }
            _ => {}
        }
    }
    let opened = open.pop()?;
    let option = opened
        .options
        .iter()
        .find(|option| option.recommended)
        .or_else(|| opened.options.first())?;
    let (reply, replied) = oneshot::channel();
    let answer = ApprovalAnswer {
        id: opened.id.clone(),
        action: option.action.clone(),
        text: None,
        evidence_sha256: opened.evidence_sha256.clone(),
        tasks: Vec::new(),
    };
    Some((EngineCommand::Answer { answer, reply }, replied))
}

/// The smallest value that matches a strict step schema: every required property, empty
/// arrays, the first enum value or `anyOf` branch, and null wherever null is allowed.
pub fn fill(schema: &Value) -> Value {
    fill_node(schema, schema, 0)
}

fn fill_node(root: &Value, node: &Value, depth: usize) -> Value {
    if depth > 32 {
        return Value::Null;
    }
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        let path = reference.trim_start_matches("#/");
        let mut target = root;
        for part in path.split('/') {
            target = &target[part];
        }
        return fill_node(root, target, depth + 1);
    }
    if let Some(value) = node.get("const") {
        return value.clone();
    }
    if let Some(first) = node
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
    {
        return first.clone();
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = node.get(key).and_then(Value::as_array) {
            let null = branches
                .iter()
                .find(|b| b.get("type") == Some(&Value::from("null")));
            return match null.or_else(|| branches.first()) {
                Some(branch) => fill_node(root, branch, depth + 1),
                None => Value::Null,
            };
        }
    }
    let kind = match node.get("type") {
        Some(Value::Array(kinds)) if kinds.contains(&Value::from("null")) => "null",
        Some(Value::Array(kinds)) => kinds.first().and_then(Value::as_str).unwrap_or("null"),
        Some(Value::String(kind)) => kind.as_str(),
        _ if node.get("properties").is_some() => "object",
        _ => "null",
    };
    match kind {
        "object" => {
            let mut object = serde_json::Map::new();
            if let Some(properties) = node.get("properties").and_then(Value::as_object) {
                for (name, property) in properties {
                    object.insert(name.clone(), fill_node(root, property, depth + 1));
                }
            }
            Value::Object(object)
        }
        "array" => {
            let count = node.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            let item = node.get("items").cloned().unwrap_or(Value::Null);
            Value::Array(
                (0..count)
                    .map(|_| fill_node(root, &item, depth + 1))
                    .collect(),
            )
        }
        "string" => {
            let length = node.get("minLength").and_then(Value::as_u64).unwrap_or(1);
            Value::from("x".repeat(usize::try_from(length.max(1)).unwrap_or(1)))
        }
        "integer" | "number" => node.get("minimum").cloned().unwrap_or(Value::from(0)),
        "boolean" => Value::Bool(false),
        _ => Value::Null,
    }
}

/// The main agent's brief for `DesignWorkflow`: an example by name with its params, or a workflow
/// file of its own, with one criterion.
pub fn brief(based_on: Option<&str>, params: Value, workflow: Option<&str>, why: &str) -> Value {
    serde_json::json!({
        "name": "look-at-it",
        "description": "Look at the repository",
        "based_on": based_on,
        "why": why,
        "params": params,
        "workflow": workflow,
        "done_when": [{"id": "G1", "text": "The checks pass", "short": "Checks",
                       "check": "review"}],
        "plan": [],
        "questions": [],
        "first_try": null,
    })
}

/// Scripts one structured answer per value for the worker's next step sessions.
pub fn script(worker: &FakeRunner, answers: Vec<Value>) {
    for answer in answers {
        worker.push(FakeSession::structured(answer));
    }
}

/// `type/event` names of the events, in order, for asserting sequences.
pub fn names(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            let value = serde_json::to_value(&event.kind).unwrap_or(Value::Null);
            let kind = value["type"].as_str().unwrap_or("?");
            let data = &value["data"];
            match ["event", "type", "kind"]
                .iter()
                .find_map(|tag| data[*tag].as_str())
            {
                Some(name) => format!("{kind}/{name}"),
                None => kind.to_owned(),
            }
        })
        .collect()
}

/// A run's recorded log with its placeholders (`{run}`, `{repo}`, `{run_dir}`, `{worktrees}`,
/// `{base_commit}`) filled in, one event per line.
pub fn recorded_run(text: &str, fill: &[(&str, &str)]) -> Vec<RunEvent> {
    text.lines()
        .map(|line| {
            let line = fill
                .iter()
                .fold(line.to_owned(), |line, (from, to)| line.replace(from, to));
            serde_json::from_str(&line).expect("a recorded event")
        })
        .collect()
}

/// `repo`'s home and git config, for the engine's git. Without them it reads the process's own,
/// which tests holding [`TempRepo::env_guard`] change meanwhile.
pub fn git_env_of(repo: &TempRepo) -> Vec<(String, String)> {
    repo.envs()
        .into_iter()
        .map(|(name, value)| (name, value.to_string_lossy().into_owned()))
        .collect()
}

/// The engine's shared state over `context`, built by hand, its git on `repo`'s home and config.
pub fn shared_in(
    repo: &TempRepo,
    context: &EngineContext,
    state: RunState,
) -> anyhow::Result<Shared> {
    Ok(building_with_git_env(git_env_of(repo), || {
        Shared::new(context, state)
    })?)
}
