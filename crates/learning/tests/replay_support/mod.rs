//! Shared helpers for the replay tests: your repository with merged tasks and the journals of
//! the runs that merged them, checks that run a test command for real, and a runner that plays
//! the agents from a script. Each `tests/replay_*.rs` file includes it with
//! `mod replay_support;`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use time::OffsetDateTime;

use argonauts_core::events::agent_status::RunSpend;
use argonauts_core::events::run::{RunMode, RunStarted};
use argonauts_core::events::task::CheckKind;
use argonauts_core::events::{
    CheckOutcome, MergeEvent, ReviewEvent, RunEvent, RunEventKind, RunLifecycle, SpendEvent,
};
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{CheckRequest, CheckRunner};
use argonauts_core::meter::Spend;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::agents::EndpointSettings;
use argonauts_core::steps::intake::ChangeEstimate;
use argonauts_core::{RunId, TaskId};
use argonauts_learning::git;
use argonauts_learning::learned_workflows::{LearnedWorkflow, LoadedLearnedWorkflow, Match};
use argonauts_learning::replay::{
    Entry, Proposal, ReplayRequest, ReplayRun, ReplayRunner, ReplaySet, holdout_draw,
};
use argonauts_learning::{Result, Store};
use argonauts_testkit::repo::TempRepo;

pub const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Replay Agent"),
    ("GIT_AUTHOR_EMAIL", "agent@argonauts.invalid"),
    ("GIT_COMMITTER_NAME", "Replay Agent"),
    ("GIT_COMMITTER_EMAIL", "agent@argonauts.invalid"),
];

/// What each run of the fixture spent, and so each entry's replay limit.
pub const RUN_SPEND_USD: f64 = 1.0;

/// A task that merged: the commits on either side and what solved it.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub run: RunId,
    pub request: String,
    pub base: String,
    pub merged: String,
    pub test: String,
    pub solution: Vec<(String, String)>,
}

pub struct Fixture {
    pub repo: TempRepo,
    pub store: Store,
    pub set: ReplaySet,
    pub tasks: Vec<Task>,
}

impl Fixture {
    pub fn new() -> Self {
        let repo = TempRepo::new().unwrap();
        let state = repo.scratch().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let store = Store::new(state, repo.root());
        let set = ReplaySet::open(&store).unwrap();
        Self {
            repo,
            store,
            set,
            tasks: Vec::new(),
        }
    }

    /// A task whose request asks for `src/<name>.txt` and whose test checks it's there.
    pub fn task(&mut self, name: &str) -> Task {
        self.task_with(
            &format!("Add the {name} feature"),
            &[(&format!("src/{name}.txt"), &format!("{name} ready\n"))],
            &format!("tests/{name}.sh"),
            &format!("test -f src/{name}.txt\n"),
        )
    }

    /// Commits `solution` and `test` as one merged change, and writes the journal of the run
    /// that merged it.
    pub fn task_with(
        &mut self,
        request: &str,
        solution: &[(&str, &str)],
        test: &str,
        test_body: &str,
    ) -> Task {
        let base = self.repo.head().unwrap();
        for (path, body) in solution {
            self.repo.write(path, body).unwrap();
        }
        self.repo.write(test, test_body).unwrap();
        let merged = self.repo.commit_all(request).unwrap();
        let run = RunId::new(format!("run-{:03}", self.tasks.len() + 1)).unwrap();
        self.write_journal(&run, request, &base, &merged, test);
        let task = Task {
            id: run.as_str().to_owned(),
            run,
            request: request.to_owned(),
            base,
            merged,
            test: test.to_owned(),
            solution: solution
                .iter()
                .map(|(path, body)| ((*path).to_owned(), (*body).to_owned()))
                .collect(),
        };
        self.tasks.push(task.clone());
        task
    }

    pub fn replay_dir(&self) -> PathBuf {
        self.store.replay_dir()
    }

    pub async fn consider(
        &self,
        task: &Task,
        settings: &Settings,
        checks: &Arc<ShellChecks>,
    ) -> Option<Proposal> {
        let checks: Arc<dyn CheckRunner> = checks.clone();
        self.set
            .consider(&task.run, settings, checks)
            .await
            .unwrap()
    }

    /// Considers every task under `ask` and accepts each with a seed that draws `holdout`.
    pub async fn accept(&self, tasks: &[(&Task, bool)], settings: &Settings) -> Vec<Entry> {
        let mut asking = settings.clone();
        asking.evolution.replay.accept_entries =
            argonauts_core::settings::evolution::EntryAcceptance::Ask;
        let checks = Arc::new(ShellChecks::default());
        let share = holdout_share(settings);
        let mut entries = Vec::new();
        for (task, holdout) in tasks {
            let proposal = self.consider(task, &asking, &checks).await.unwrap();
            assert!(proposal.valid(), "{:?}", proposal.checks);
            entries.push(
                self.set
                    .accept(&task.id, seed_for(*holdout, share))
                    .unwrap(),
            );
        }
        entries
    }

    fn write_journal(&self, run: &RunId, request: &str, base: &str, merged: &str, test: &str) {
        let started = RunStarted {
            request: request.to_owned(),
            title: request.to_owned(),
            mode: RunMode::Crew,
            forced: false,
            workflow: "council".to_owned(),
            estimate: Some(ChangeEstimate {
                task_type: "feature".to_owned(),
                summary: request.to_owned(),
                files_likely: Vec::new(),
                file_count: 1,
                risk_labels: Vec::new(),
                tests_exist: true,
                unknowns: Vec::new(),
            }),
            captain_or_crew: None,
            conversation_turn: None,
            continues: None,
            repo_root: self.repo.root().to_path_buf(),
            base_branch: Some("main".to_owned()),
            base_commit: base.to_owned(),
            uncommitted: Vec::new(),
            uncommitted_included: false,
            settings_sha256: String::new(),
            settings: Value::Null,
            overrides: Vec::new(),
            cli_versions: BTreeMap::new(),
            version: "0.0.1".to_owned(),
            outside_agent: None,
            instructions: Vec::new(),
            required_steps: Vec::new(),
        };
        let kinds = vec![
            RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
            RunEventKind::Review(ReviewEvent::TestsWritten {
                reviewer: ModelRef::new("codex", None),
                for_task: TaskId::numbered(1),
                files: vec![test.to_owned()],
            }),
            RunEventKind::Check(CheckOutcome {
                name: "suite".to_owned(),
                kind: CheckKind::FullSuite,
                passed: true,
                command: Some(vec!["sh".to_owned(), test.to_owned()]),
                exit_code: Some(0),
                duration_ms: 5,
                summary: "1 passed".to_owned(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            }),
            RunEventKind::Spend(SpendEvent::Run(RunSpend {
                spent: Spend {
                    cost_usd: RUN_SPEND_USD,
                    ..Spend::default()
                },
                limit: None,
                lines: Vec::new(),
                reserve_left: None,
            })),
            RunEventKind::Merge(MergeEvent::RunMerged {
                target: "main".to_owned(),
                sha: merged.to_owned(),
                fast_forward: true,
            }),
        ];
        let mut lines = String::new();
        for (seq, kind) in kinds.into_iter().enumerate() {
            let event = RunEvent {
                seq: seq as u64,
                ts: OffsetDateTime::now_utc(),
                run_id: run.clone(),
                task_id: None,
                session: None,
                source: "argonauts-code".to_owned(),
                kind,
            };
            lines.push_str(&serde_json::to_string(&event).unwrap());
            lines.push('\n');
        }
        let dir = self.store.state_dir.join("runs").join(run.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), lines).unwrap();
    }
}

/// Settings whose one worker is an endpoint you allowed to bill, so replays may run.
pub fn settings() -> Settings {
    let mut settings = Settings::default();
    let worker = ModelRef::new("openrouter", Some("open-model".to_owned()));
    settings.endpoints.insert(
        "openrouter".to_owned(),
        EndpointSettings {
            allow_api_billing: true,
            ..EndpointSettings::default()
        },
    );
    settings.crew = vec![worker.clone()].into();
    settings.crew.main = worker;
    settings
}

pub fn holdout_share(settings: &Settings) -> f64 {
    let replay = &settings.evolution.replay;
    f64::from(replay.holdout_tasks) / f64::from(replay.holdout_tasks + replay.tasks)
}

/// The first seed that draws the side asked for.
pub fn seed_for(holdout: bool, share: f64) -> u64 {
    (0u64..)
        .find(|&seed| holdout_draw(seed, share) == holdout)
        .unwrap()
}

pub fn learned_workflow(version: u32, settings: Value) -> LoadedLearnedWorkflow {
    let settings: BTreeMap<String, Value> = serde_json::from_value(settings).unwrap();
    LoadedLearnedWorkflow {
        learned_workflow: LearnedWorkflow {
            name: "api-feature".to_owned(),
            version,
            parent: version.checked_sub(1).filter(|&parent| parent > 0),
            description: "Endpoints with tests".to_owned(),
            workflow: None,
            mode: None,
            phases: None,
            matches: Match::default(),
            prompts: BTreeMap::new(),
            settings,
        },
        path: PathBuf::from(format!(".argonauts/learned/api-feature/v{version}.yaml")),
        text: String::new(),
        sha256: format!("sha-v{version}"),
    }
}

pub fn git_in(dir: &Path, args: &[&str]) -> String {
    git::run(dir, args).unwrap()
}

/// Files under `dir`, relative, sorted. Empty when it's missing.
pub fn files_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(items) = std::fs::read_dir(&folder) else {
            continue;
        };
        for item in items {
            let path = item.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                found.push(path.strip_prefix(dir).unwrap().display().to_string());
            }
        }
    }
    found.sort();
    found
}

/// Runs each check's command in its folder, without a sandbox, and records what it was asked,
/// with the files the folder held while the command ran.
#[derive(Default)]
pub struct ShellChecks {
    requests: Mutex<Vec<(CheckRequest, Vec<String>)>>,
}

impl ShellChecks {
    pub fn requests(&self) -> Vec<(CheckRequest, Vec<String>)> {
        self.requests.lock().unwrap().clone()
    }
}

impl CheckRunner for ShellChecks {
    fn run(&self, request: CheckRequest) -> BoxFuture<'_, CheckOutcome> {
        let output = request.command.split_first().map(|(program, args)| {
            Command::new(program)
                .args(args)
                .current_dir(&request.workdir)
                .output()
        });
        let passed = matches!(output, Some(Ok(ref output)) if output.status.success());
        let files: Vec<String> = files_under(&request.workdir)
            .into_iter()
            .filter(|file| !file.starts_with(".git/"))
            .collect();
        let outcome = CheckOutcome {
            name: request.name.clone(),
            kind: CheckKind::Command,
            passed,
            command: Some(request.command.clone()),
            exit_code: Some(i32::from(!passed)),
            duration_ms: 1,
            summary: if passed { "passed" } else { "failed" }.to_owned(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: false,
            skipped: false,
        };
        self.requests.lock().unwrap().push((request, files));
        Box::pin(async move { outcome })
    }
}

/// Whether a version passes a task, given the request and which try it is, from 1.
type Script = dyn Fn(&ReplayRequest, u32) -> bool + Send + Sync;

/// Plays the agents: a version that passes commits the task's solution in the replay repo, and
/// one that fails leaves the base commit. Records every request, and whether a request or its
/// replay repo ever held the task's hidden test.
pub struct ScriptedRunner {
    tasks: Vec<Task>,
    passes: Box<Script>,
    pub cost_usd: f64,
    requests: Mutex<Vec<ReplayRequest>>,
    saw_hidden: AtomicBool,
}

impl ScriptedRunner {
    /// `passes` gets the request and which try of that task and version it is, from 1.
    pub fn new(
        tasks: &[Task],
        passes: impl Fn(&ReplayRequest, u32) -> bool + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            tasks: tasks.to_vec(),
            passes: Box::new(passes),
            cost_usd: RUN_SPEND_USD,
            requests: Mutex::new(Vec::new()),
            saw_hidden: AtomicBool::new(false),
        })
    }

    pub fn requests(&self) -> Vec<ReplayRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn saw_hidden(&self) -> bool {
        self.saw_hidden.load(Ordering::SeqCst)
    }

    fn play(&self, request: &ReplayRequest) -> Result<ReplayRun> {
        let try_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests
                .iter()
                .filter(|seen| {
                    seen.entry.id == request.entry.id
                        && seen.learned_workflow.learned_workflow.version
                            == request.learned_workflow.learned_workflow.version
                })
                .count() as u32
        };
        let task = self
            .tasks
            .iter()
            .find(|task| task.id == request.entry.id)
            .unwrap();
        if request.repo.join(&task.test).exists() || !request.entry.tests.is_empty() {
            self.saw_hidden.store(true, Ordering::SeqCst);
        }
        let repo = &request.repo;
        if (self.passes)(request, try_number) {
            for (path, body) in &task.solution {
                let path = repo.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, body).unwrap();
            }
            git::run(repo, &["add", "-A"])?;
            git::run_with(repo, &["commit", "-q", "-m", "replayed"], &IDENTITY, None)?;
        }
        Ok(ReplayRun {
            cost_usd: self.cost_usd,
            minutes: 2.0,
            head: Some(git::run(repo, &["rev-parse", "HEAD"])?),
        })
    }
}

impl ReplayRunner for ScriptedRunner {
    fn run<'a>(&'a self, request: &'a ReplayRequest) -> BoxFuture<'a, Result<ReplayRun>> {
        Box::pin(async move { self.play(request) })
    }
}
