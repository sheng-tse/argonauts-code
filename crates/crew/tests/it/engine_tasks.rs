//! The task pipeline: parallel worktrees, checks before review, the check and retry ladders,
//! stuck answers, task session lifetimes, and the status part for workers without a
//! status tool. Each scenario seeds an approved goal and its tasks, then runs the `build` step.
use crate::support;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};

use argonauts_core::agents::session_runner::{
    Access, RunnerCapabilities, RunnerEvent, SessionRunner, SessionSpec,
};
use argonauts_core::base::names::{ApprovalId, ModelRef, WorkflowId};
use argonauts_core::models::limits::{Candidate, CostEstimate, RecoveryAction};
use argonauts_core::models::meter::{MeterAction, MeterLimit, SpendStatus};
use argonauts_core::runs::api::ApprovalAnswer;
use argonauts_core::runs::events::agent_status::{FallbackPlan, RunSpend, SpendEvent};
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, PlanPacket,
};
use argonauts_core::runs::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::runs::events::plan::{LimitPlan, TaskGraph};
use argonauts_core::runs::events::run::{
    Party, ResumeCause, RunLifecycle, RunMode, RunResumed, RunStarted,
};
use argonauts_core::runs::events::session::{
    CallCount, EndReason, InputPartKind, LimitEvent, SessionEvent, Watchdog,
};
use argonauts_core::runs::events::task::{
    CheckKind, TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState, TryCause,
};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::safety::policy::StuckFacts;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::settings::fallback::FallbackMode;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit, OnHit};
use argonauts_core::settings::prompts::{RolePrompts, Writer};
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::workflow::{StandardPhase, StepRef};
use argonauts_core::{LimitDimension, NoteLevel, RunId, Spend, TaskId};
use argonauts_crew::EngineError;
use argonauts_crew::approvals;
use argonauts_crew::engine::host::Host;
use argonauts_crew::git::Git;
use argonauts_crew::state::RunState;
use argonauts_crew::task;
use argonauts_crew::task::checks::{self, NO_COMMAND_CHECKS};
use argonauts_crew::workflow::{StepAt, StepCall, StepDone};
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};
use argonauts_testkit::engine::services::MemorySink;
use argonauts_testkit::pty::deadline;

use support::Crew;

const WORKER_A: &str = "claude-code/claude-x";
const WORKER_B: &str = "codex/gpt-x";
const REVIEWER: &str = "agy/gemini-x";

#[tokio::test(flavor = "multi_thread")]
async fn tasks_run_in_parallel_worktrees() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let slow = Duration::from_millis(200);
    let a = crew.services.workers.add(slow_worker(
        WORKER_A,
        &[("src/a.rs", "pub fn a() {}\n")],
        slow,
    ));
    let b = crew.services.workers.add(slow_worker(
        WORKER_B,
        &[("src/b.rs", "pub fn b() {}\n")],
        slow,
    ));
    crew.services.workers.add(reviewer(REVIEWER));
    let tasks = vec![
        spec("task-001", &[], WORKER_A, REVIEWER),
        spec("task-002", &[], WORKER_B, REVIEWER),
    ];
    // Your own words for the workers: a file in your checkout, and a sentence of the request.
    crew.repo
        .write(".argonauts/roles/worker.md", "Keep functions short.\n")?;
    crew.settings.prompts.roles.insert(
        "worker".to_owned(),
        RolePrompts {
            request: Some("focus on date parsing".to_owned()),
            ..RolePrompts::default()
        },
    );
    let bench = Bench::new(&crew, RunMode::Crew, &tasks).await?;

    let done = bench.build().await?;
    assert_eq!(done.summary.as_deref(), Some("2 tasks merged into the run"));
    let events = bench.sink.events();

    // Both worker sessions started before either ended.
    let worker_sessions: Vec<(u64, bool)> = events
        .iter()
        .filter(|e| e.task_id.is_some())
        .filter_map(|e| match &e.kind {
            RunEventKind::Session(SessionEvent::Started(s)) if s.step == Some(StepName::Build) => {
                Some((e.seq, true))
            }
            RunEventKind::Session(SessionEvent::Ended(_)) => Some((e.seq, false)),
            _ => None,
        })
        .collect();
    let starts: Vec<u64> = worker_sessions
        .iter()
        .filter(|(_, s)| *s)
        .map(|(q, _)| *q)
        .collect();
    assert_eq!(starts.len(), 2, "{worker_sessions:?}");
    let first_end = worker_sessions
        .iter()
        .find(|(_, s)| !*s)
        .map(|(q, _)| *q)
        .unwrap();
    assert!(starts.iter().all(|s| *s < first_end), "{worker_sessions:?}");

    // Each task in its own worktree on its own branch.
    let trees = worktrees(&events);
    assert_eq!(trees.len(), 2);
    let run = RunId::new("run-01")?;
    for (id, worker) in [("task-001", &a), ("task-002", &b)] {
        let (path, branch) = &trees[&TaskId::new(id)?];
        assert_eq!(branch, &format!("argonauts/{run}/{id}"));
        assert!(path.ends_with(id), "{path:?}");
        assert_eq!(worker.specs()[0].workdir, *path);
        // Each worker's frame says other workers may be building the run's other tasks.
        let frame = &worker.specs()[0].prompt[0];
        assert_eq!(frame.kind, InputPartKind::Frame);
        assert!(
            frame
                .text
                .contains("Other workers build the run's other tasks"),
            "{}",
            frame.text
        );
        // Then the orchestrator's brief for the task, as its own words and nowhere twice.
        let prompt = &worker.specs()[0].prompt;
        assert_eq!(
            argonauts_core::prompts::prompt_parts::heading(&prompt[1]).as_deref(),
            Some("From the orchestrator · Claude Code")
        );
        assert_eq!(prompt[1].text.trim_end(), format!("Do {id}."));
        // Then your words, from the file that stands to this one request.
        let headings: Vec<String> = prompt
            .iter()
            .filter_map(argonauts_core::prompts::prompt_parts::heading)
            .collect();
        assert_eq!(
            headings[2..],
            ["Your file · roles/worker.md", "From your request"],
            "{headings:?}"
        );
        assert_eq!(prompt[2].text.trim_end(), "Keep functions short.");
        assert_eq!(prompt[3].text.trim_end(), "focus on date parsing");
        // The word on questions follows the instructions while the run decides without you.
        assert_eq!(prompt[4].label, "decide, don't ask");
        let task = &prompt[5];
        assert_eq!(task.kind, InputPartKind::Task);
        assert!(task.text.starts_with(&format!("{id} · Build {id}")));
        assert!(!task.text.contains(&format!("Do {id}.")), "{}", task.text);
    }
    assert_ne!(
        trees[&TaskId::new("task-001")?].0,
        trees[&TaskId::new("task-002")?].0
    );
    assert!(state_of(&bench, "task-001") == TaskState::Merged);
    assert!(state_of(&bench, "task-002") == TaskState::Merged);
    Ok(())
}

/// An agent that commits part of its work keeps its commit, Argonauts Code commits only what it
/// left, and git notes name the agent as the author of both.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_own_commits_are_kept_and_noted() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(
        FakeRunner::cli(WORKER_B, &provider_of(WORKER_B)).respond_with(|spec| {
            let dir = &spec.workdir;
            if !dir.join("committed.rs").exists() {
                let _ = std::fs::write(dir.join("committed.rs"), "fn a() {}\n");
                git_in(dir, &["add", "committed.rs"]);
                git_in(dir, &["commit", "-qm", "Add a"]);
                let _ = std::fs::write(dir.join("left.rs"), "fn b() {}\n");
            }
            FakeSession::structured(report("done"))
                .with_event(support::wrote("committed.rs"))
                .with_event(support::wrote("left.rs"))
        }),
    );
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;

    let commits: Vec<_> = task_events(&bench.sink.events(), "task-001")
        .into_iter()
        .filter_map(|event| match event {
            TaskEvent::Committed(info) if info.sha.is_some() => Some(info),
            _ => None,
        })
        .collect();
    let [own, left] = commits.as_slice() else {
        bail!("expected the agent's commit and Argonauts Code's, got {commits:?}");
    };
    assert_eq!(own.message.trim(), "Add a");
    assert!(
        left.message.starts_with("task-001: work from"),
        "{}",
        left.message
    );
    for commit in [own, left] {
        assert_eq!(commit.by, Some(worker(WORKER_B)));
        let sha = commit.sha.as_deref().context("no sha")?;
        let note = crew
            .repo
            .git(&["notes", "--ref=refs/notes/argonauts", "show", sha])?;
        assert_eq!(note.trim(), format!("author: {WORKER_B}"));
    }
    let left_sha = left.sha.as_deref().context("no sha")?;
    let files = crew
        .repo
        .git(&["show", "--name-only", "--format=", left_sha])?;
    assert_eq!(files.trim(), "left.rs");
    assert_eq!(left.stat.files_changed, 1);
    Ok(())
}

/// The sample run's commit, with no list of file types. A worker writes one file with its edit
/// tool, makes another with a command and lists its folder in its report, and its tests leave
/// bytecode beside them. The commit takes what it meant to create, and the transcript names
/// what stayed out. With `commit.unclaimed = "keep"` every new file goes in, as before.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_commits_only_the_new_files_its_agent_meant_to_create() -> anyhow::Result<()> {
    use argonauts_core::settings::delivery::Unclaimed;

    for (unclaimed, committed) in [
        (Unclaimed::Leave, "gen/schema.py\nsteps.py"),
        (
            Unclaimed::Keep,
            "__pycache__/steps.cpython-312.pyc\ngen/schema.py\nsteps.py",
        ),
    ] {
        let mut crew = Crew::new()?;
        crew.settings.commit.unclaimed = unclaimed;
        crew.services.workers.add(
            FakeRunner::cli(WORKER_B, &provider_of(WORKER_B)).respond_with(|spec| {
                for (file, text) in [
                    ("steps.py", "def steps(): ...\n"),
                    ("gen/schema.py", "SCHEMA = {}\n"),
                    ("__pycache__/steps.cpython-312.pyc", "bytecode"),
                ] {
                    let path = spec.workdir.join(file);
                    let _ = std::fs::create_dir_all(path.parent().expect("a folder"));
                    let _ = std::fs::write(path, text);
                }
                let mut report = report("done");
                report["files_changed"] = json!(["steps.py", "gen/"]);
                FakeSession::structured(report).with_event(support::wrote("steps.py"))
            }),
        );
        crew.services.workers.add(reviewer(REVIEWER));
        let bench = Bench::new(
            &crew,
            RunMode::Crew,
            &[spec("task-001", &[], WORKER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;

        let events = bench.sink.events();
        let sha = task_events(&events, "task-001")
            .into_iter()
            .find_map(|event| match event {
                TaskEvent::Committed(info) => info.sha,
                _ => None,
            })
            .context("the task has no commit")?;
        let files = crew.repo.git(&["show", "--name-only", "--format=", &sha])?;
        assert_eq!(files.trim(), committed, "{unclaimed:?}");

        let told: Vec<&str> = events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Note(note) if note.text.starts_with("Left out of") => {
                    Some(note.text.as_str())
                }
                _ => None,
            })
            .collect();
        match unclaimed {
            Unclaimed::Leave => {
                let [note] = told.as_slice() else {
                    bail!("expected one note on what stayed out, got {told:?}");
                };
                assert!(note.contains("__pycache__/"), "{note}");
                assert!(note.contains("commit.unclaimed"), "{note}");
            }
            Unclaimed::Keep => assert!(told.is_empty(), "{told:?}"),
        }
    }
    Ok(())
}

/// A file the agent wrote that the repository's ignore rules match is never forced into the
/// commit, where a `.env` would reach history. The transcript names it with the rule.
#[tokio::test(flavor = "multi_thread")]
async fn a_written_file_the_ignore_rules_match_is_told_with_the_rule() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.repo.write(".gitignore", "lib/\n")?;
    crew.repo.commit_all("ignore lib")?;
    crew.services.workers.add(
        FakeRunner::cli(WORKER_B, &provider_of(WORKER_B)).respond_with(|spec| {
            for file in ["lib/util.py", "app.py"] {
                let path = spec.workdir.join(file);
                let _ = std::fs::create_dir_all(path.parent().expect("a folder"));
                let _ = std::fs::write(path, "def util(): ...\n");
            }
            FakeSession::structured(report("done"))
                .with_event(support::wrote("lib/util.py"))
                .with_event(support::wrote("app.py"))
        }),
    );
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;

    let events = bench.sink.events();
    let sha = task_events(&events, "task-001")
        .into_iter()
        .find_map(|event| match event {
            TaskEvent::Committed(info) => info.sha,
            _ => None,
        })
        .context("the task has no commit")?;
    let files = crew.repo.git(&["show", "--name-only", "--format=", &sha])?;
    assert_eq!(files.trim(), "app.py");
    let told = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Note(note)
            if note.text.contains("ignore rules match")
                && note.text.contains("lib/util.py (.gitignore:1:lib/)"))
    });
    assert!(told, "the ignored file wasn't named");
    Ok(())
}

/// An agent's own commit is no claim. One that sweeps in what its tests left behind, with
/// `git add -A`, keeps its commit, and Argonauts Code's commit takes the leftovers back out of
/// the branch and leaves them in the worktree, as if the agent had never committed. With
/// `commit.unclaimed = "keep"` the agent's commit stands as it is.
#[tokio::test(flavor = "multi_thread")]
async fn what_an_agents_own_commit_swept_in_is_taken_back_out() -> anyhow::Result<()> {
    use argonauts_core::settings::delivery::Unclaimed;

    const BYTECODE: &str = "__pycache__/steps.cpython-312.pyc";
    for unclaimed in [Unclaimed::Leave, Unclaimed::Keep] {
        let mut crew = Crew::new()?;
        crew.settings.commit.unclaimed = unclaimed;
        crew.services.workers.add(
            FakeRunner::cli(WORKER_B, &provider_of(WORKER_B)).respond_with(|spec| {
                let dir = &spec.workdir;
                if !dir.join("steps.py").exists() {
                    let _ = std::fs::write(dir.join("steps.py"), "def steps(): ...\n");
                    let _ = std::fs::create_dir_all(dir.join("__pycache__"));
                    let _ = std::fs::write(dir.join(BYTECODE), "bytecode");
                    git_in(dir, &["add", "-A"]);
                    git_in(dir, &["commit", "-qm", "Add steps"]);
                }
                FakeSession::structured(report("done")).with_event(support::wrote("steps.py"))
            }),
        );
        crew.services.workers.add(reviewer(REVIEWER));
        let bench = Bench::new(
            &crew,
            RunMode::Crew,
            &[spec("task-001", &[], WORKER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;

        let events = bench.sink.events();
        let (worktree, branch) = worktrees(&events)
            .remove(&TaskId::new("task-001")?)
            .context("the task has no worktree")?;
        let held = crew.repo.git(&["ls-tree", "-r", "--name-only", &branch])?;
        let swept_in = held.lines().any(|path| path == BYTECODE);
        assert!(held.lines().any(|path| path == "steps.py"), "{held}");
        assert_eq!(
            swept_in,
            unclaimed == Unclaimed::Keep,
            "{unclaimed:?}: {held}"
        );
        assert!(
            worktree.join(BYTECODE).exists(),
            "the file left the worktree"
        );

        let subjects = crew.repo.git(&["log", "--format=%s", "-2", &branch])?;
        let subjects: Vec<&str> = subjects.lines().collect();
        match unclaimed {
            Unclaimed::Leave => {
                assert!(
                    subjects[0].starts_with("task-001: work from"),
                    "{subjects:?}"
                );
                assert_eq!(subjects[1], "Add steps");
            }
            Unclaimed::Keep => assert_eq!(subjects[0], "Add steps"),
        }
        let told = events.iter().any(|event| {
            matches!(&event.kind, RunEventKind::Note(note)
                if note.text.starts_with("Left out of") && note.text.contains("__pycache__/"))
        });
        assert_eq!(told, unclaimed == Unclaimed::Leave, "{unclaimed:?}");
    }
    Ok(())
}

/// Runs git in `dir` as an agent would, with its own identity.
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=agent", "-c", "user.email=agent@local"])
        .args(args)
        .current_dir(dir)
        .status();
    assert!(status.is_ok_and(|status| status.success()), "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_before_review() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_check()];
    let worker = crew
        .services
        .workers
        .add(fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    let reviewer = crew.services.workers.add(reviewer(REVIEWER));
    // The first check run fails, the resumed session fixes it.
    crew.services.checks.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let events = bench.sink.events();
    // The task's own checks, apart from the ones run on the starting code.
    let check_seqs: Vec<u64> = events
        .iter()
        .filter(|e| {
            e.task_id
                .as_ref()
                .is_some_and(|task| task.as_str() != "start")
        })
        .filter(|e| matches!(&e.kind, RunEventKind::Check(g) if g.kind == CheckKind::Command))
        .map(|e| e.seq)
        .collect();
    let first_review = events
        .iter()
        .find(|e| matches!(e.kind, RunEventKind::Review(_)))
        .map(|e| e.seq)
        .context("no review recorded")?;
    assert_eq!(check_seqs.len(), 2, "one failed run and one passing run");
    assert!(check_seqs.iter().all(|g| *g < first_review));
    assert_eq!(
        worker.specs().len(),
        2,
        "the worker resumed once for the checks output"
    );
    assert_eq!(
        reviewer.specs().len(),
        1,
        "a task failing its checks never reaches review"
    );
    assert_eq!(
        states(&events, "task-001"),
        [
            TaskState::Waiting,
            TaskState::Ready,
            TaskState::Running,
            TaskState::Checking,
            TaskState::Running,
            TaskState::Checking,
            TaskState::InReview,
            TaskState::Merging,
            TaskState::Merged,
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn no_command_checks_blocks_auto_merge() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    assert!(crew.settings.checks.run.is_empty());
    crew.services
        .workers
        .add(fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let record = bench
        .host
        .read(|state| state.tasks.get(&TaskId::new("task-001").unwrap()).cloned())
        .context("no task")?;
    let marker = record
        .checks
        .iter()
        .find(|g| checks::is_no_command_checks(g))
        .context("no checks cell note")?;
    assert_eq!(marker.summary, NO_COMMAND_CHECKS);
    assert!(
        marker.skipped,
        "an empty checks.run is recorded as skipped, not as a failure"
    );
    // Merge rules that need the suite see it as not run.
    assert_eq!(checks::checks_passed(&record.checks), None);
    // The built-in checks still ran on the agent's commits.
    for kind in [
        CheckKind::ProtectedPaths,
        CheckKind::SecretScan,
        CheckKind::TestTamper,
    ] {
        assert!(
            record.checks.iter().any(|g| g.kind == kind && g.passed),
            "{kind:?}"
        );
    }
    Ok(())
}

/// A run whose checks only a project file you haven't trusted names leaves them out, and its
/// checks cell names that file rather than saying none are set.
#[tokio::test(flavor = "multi_thread")]
async fn checks_a_file_you_havent_trusted_names_are_said_to_wait() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let dir = crew.repo.root().join(".argonauts");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("config.toml"),
        "[checks]\nrun = [{ name = \"tests\", command = [\"pytest\"] }]\n",
    )?;
    crew.services
        .workers
        .add(fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let record = bench
        .host
        .read(|state| state.tasks.get(&TaskId::new("task-001").unwrap()).cloned())
        .context("no task")?;
    let marker = record
        .checks
        .iter()
        .find(|g| checks::is_no_command_checks(g))
        .context("no checks cell note")?;
    assert_eq!(
        marker.summary,
        checks::held_back_text(".argonauts/config.toml")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_done_report_needs_its_changes_on_the_branch() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let resumed_with = Arc::new(std::sync::Mutex::new(String::new()));
    let seen = resumed_with.clone();
    // The first session claims a change it never made. Told so, its resume makes it.
    crew.services.workers.add(
        FakeRunner::cli(WORKER_B, &provider_of(WORKER_B)).respond_with(move |spec| {
            if spec.resume.is_some() {
                *seen.lock().unwrap() = format!("{:?}", spec.prompt);
                return work(spec, &[("src/a.rs", "fn a() {}\n")], Duration::ZERO);
            }
            let mut claimed = report("done");
            claimed["files_changed"] = json!(["src/a.rs"]);
            FakeSession::structured(claimed).with_cli_session(&format!("v-{}", spec.key))
        }),
    );
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let events = bench.sink.events();
    assert_eq!(
        tries(&events, "task-001"),
        [
            (TryCause::First, worker(WORKER_B)),
            (TryCause::CheckRetry, worker(WORKER_B)),
        ]
    );
    assert!(
        resumed_with
            .lock()
            .unwrap()
            .contains("the report lists src/a.rs, but the branch has no changes"),
        "{}",
        resumed_with.lock().unwrap()
    );
    let record = bench
        .host
        .read(|state| state.tasks.get(&TaskId::new("task-001").unwrap()).cloned())
        .context("no task")?;
    let checks: Vec<bool> = record
        .checks
        .iter()
        .filter(|g| g.kind == CheckKind::ReportedChanges)
        .map(|g| g.passed)
        .collect();
    assert_eq!(checks, [false], "the resume's changes need no check");
    assert_eq!(state_of(&bench, "task-001"), TaskState::Merged);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_that_changed_nothing_says_so() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(fake_worker(WORKER_B, &[]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let record = bench
        .host
        .read(|state| state.tasks.get(&TaskId::new("task-001").unwrap()).cloned())
        .context("no task")?;
    let check = record
        .checks
        .iter()
        .find(|g| g.kind == CheckKind::ReportedChanges)
        .context("no check of the empty branch")?;
    assert!(check.passed);
    assert_eq!(
        check.summary,
        "no changes on the branch, as the report says"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_ladder_same_then_other_then_ask_you() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_check()];
    crew.services
        .workers
        .add(fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services
        .workers
        .add(fake_worker(WORKER_A, &[("src/a.rs", "fn a() { 1 }\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .fallback
        .set_candidates(vec![candidate(WORKER_A)]);
    for _ in 0..4 {
        crew.services.checks.fail_next("still failing");
    }
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    // No limit is set, so one more try always fits.
    let limits_off = RunSpend {
        spent: Spend::default(),
        limit: None,
        lines: Vec::new(),
        reserve_left: None,
    };
    bench
        .host
        .record(None, None, RunEventKind::Spend(SpendEvent::Run(limits_off)))?;

    let build = bench.spawn_build();
    let approval = bench
        .answer(ApprovalKind::Stuck, AnswerAction::Cancel, None)
        .await?;
    let done = finish(build).await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("0 tasks merged into the run, 1 cancelled")
    );

    let events = bench.sink.events();
    let tries = tries(&events, "task-001");
    assert_eq!(
        tries,
        [
            (TryCause::First, worker(WORKER_B)),
            (TryCause::CheckRetry, worker(WORKER_B)),
            (TryCause::AfterFailure, worker(WORKER_A)),
            (TryCause::CheckRetry, worker(WORKER_A)),
        ]
    );
    let reassigned: Vec<_> = task_events(&events, "task-001")
        .into_iter()
        .filter_map(|e| match e {
            TaskEvent::Reassigned { to, by, .. } => Some((to, by)),
            _ => None,
        })
        .collect();
    assert_eq!(reassigned, [(worker(WORKER_A), Party::ArgonautsCode)]);
    let states = states(&events, "task-001");
    let failed = states.iter().filter(|s| **s == TaskState::Failed).count();
    assert_eq!(failed, 2, "{states:?}");
    assert_eq!(states[states.len() - 2], TaskState::NeedsYou);
    assert_eq!(states.last(), Some(&TaskState::Cancelled));
    assert!(opened(&events).iter().any(|(id, _)| *id == approval.0));

    // The policy saw what it needs to grant one more try: both providers tried, no
    // fallback of a third provider left, and none granted yet.
    let evaluated = crew.services.policy.evaluated();
    let (_, evidence) = evaluated
        .iter()
        .find(|(point, _)| point.approval == approval.0)
        .context("the policy never saw the stuck question")?;
    let facts = evidence.facts.stuck.clone().context("no stuck facts")?;
    assert_eq!(
        facts,
        StuckFacts {
            fallback_mode: crew.settings.fallback.mode,
            providers_tried: vec!["anthropic".to_owned(), "openai".to_owned()],
            next_provider: None,
            next_name: None,
            extra_tries: 0,
            fits_reserve: Some(true),
        }
    );
    Ok(())
}

/// A task that reaches a limit you set goes to a fallback, by rule and with no question.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_at_its_limit_goes_to_a_fallback() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(at_its_limit(WORKER_B));
    crew.services
        .workers
        .add(fake_worker(WORKER_A, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .fallback
        .set_candidates(vec![candidate(WORKER_A)]);
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let events = bench.sink.events();
    let workers: Vec<ModelRef> = tries(&events, "task-001")
        .into_iter()
        .map(|(_, worker)| worker)
        .collect();
    assert_eq!(workers, [worker(WORKER_B), worker(WORKER_A)]);
    let reassigned: Vec<_> = task_events(&events, "task-001")
        .into_iter()
        .filter_map(|e| match e {
            TaskEvent::Reassigned { to, by, .. } => Some((to, by)),
            _ => None,
        })
        .collect();
    assert_eq!(reassigned, [(worker(WORKER_A), Party::Rules)]);
    assert_eq!(asked_while_building(&events), [], "nothing was asked");
    assert_eq!(state_of(&bench, "task-001"), TaskState::Merged);
    Ok(())
}

/// With `limits.when_reached = "stop"` the task stops and needs you: no fallback takes it,
/// and no approval about the limit opens.
#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_a_task_at_its_limit() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(at_its_limit(WORKER_B));
    crew.services
        .workers
        .add(fake_worker(WORKER_A, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .fallback
        .set_candidates(vec![candidate(WORKER_A)]);
    crew.services
        .limits
        .set_next(RecoveryAction::LimitHit(OnHit::Stop));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;

    let build = bench.spawn_build();
    bench
        .answer(ApprovalKind::Stuck, AnswerAction::Cancel, None)
        .await?;
    finish(build).await?;
    let events = bench.sink.events();
    let workers: Vec<ModelRef> = tries(&events, "task-001")
        .into_iter()
        .map(|(_, worker)| worker)
        .collect();
    assert_eq!(workers, [worker(WORKER_B)]);
    assert_eq!(asked_while_building(&events), [ApprovalKind::Stuck]);
    let states = states(&events, "task-001");
    assert_eq!(states[states.len() - 2], TaskState::NeedsYou);
    assert_eq!(states.last(), Some(&TaskState::Cancelled));
    Ok(())
}

/// A task's session the run's limit has no room for never starts and leaves nothing in the
/// log, so the run pauses with the task's try still open: nobody takes the task over, nothing
/// asks about the task's own limit, and no try is counted. Once you raise the limit and
/// resume, the task starts that try, fresh and with its whole prompt.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_the_run_had_no_room_for_starts_its_try_once_the_limit_is_raised()
-> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.limits.spend.spend_per_run = Some(0.60);
    // The first task's session opens at $0.3619 and ends at $0.3745, which leaves $0.2255, and
    // the second's, on the same worker, would cost as much to open.
    let builder = crew.services.workers.add(
        FakeRunner::cli(WORKER_A, &provider_of(WORKER_A)).respond_with(|spec| {
            let first = spec
                .task
                .as_ref()
                .is_some_and(|task| task.as_str() == "task-001");
            let file: &[(&str, &str)] = if first {
                &[("src/a.rs", "pub fn a() {}\n")]
            } else {
                &[("src/b.rs", "pub fn b() {}\n")]
            };
            let mut session = work(spec, file, Duration::ZERO);
            if first {
                session.events.insert(0, spent(0.361891));
                session.end.spend.cost_usd = 0.3745;
            }
            session
        }),
    );
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .fallback
        .set_candidates(vec![candidate(WORKER_B)]);
    let tasks = [
        spec("task-001", &[], WORKER_A, REVIEWER),
        spec("task-002", &["task-001"], WORKER_A, REVIEWER),
    ];
    let bench = Bench::new(&crew, RunMode::Crew, &tasks).await?;

    let built = tokio::time::timeout(
        deadline::scaled(Duration::from_secs(60)),
        bench.spawn_build(),
    )
    .await??;
    assert!(
        matches!(built, Err(EngineError::Interrupted)),
        "the build unwinds as a pause has it: {built:?}"
    );
    let mut paused = false;
    for _ in 0..500 {
        paused = bench
            .sink
            .events()
            .iter()
            .any(|event| matches!(&event.kind, RunEventKind::Run(RunLifecycle::Paused { .. })));
        if paused {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(paused, "the run paused");
    let history = bench.sink.events();
    let said = "The run can't start another session under its $0.60 spend limit: a session of \
                X on Claude Code costs about $0.36 to start here and $0.23 is left, so it pauses \
                here with its work kept.";
    assert!(
        notes(&bench).iter().any(|note| note.starts_with(said)),
        "{:?}",
        notes(&bench)
    );
    assert_eq!(
        builds(&builder).len(),
        1,
        "the second task's session started"
    );
    let second = TaskId::new("task-002")?;
    let state = RunState::fold(&history);
    assert!(
        state
            .sessions
            .in_order()
            .iter()
            .all(|session| session.task.as_ref() != Some(&second)),
        "the session it had no room for is in the log"
    );
    assert_eq!(
        state.tasks.get(&second).map(|task| task.state),
        Some(TaskState::Running)
    );
    assert_eq!(
        tries(&history, "task-002"),
        [(TryCause::First, worker(WORKER_A))]
    );
    let reassigned = task_events(&history, "task-002")
        .into_iter()
        .any(|event| matches!(event, TaskEvent::Reassigned { .. }));
    assert!(!reassigned, "a fallback took the task");
    let hit = history
        .iter()
        .any(|event| matches!(&event.kind, RunEventKind::Spend(SpendEvent::Hit { .. })));
    assert!(!hit, "the task was said to reach its own limit");
    assert_eq!(asked_while_building(&history), [], "something was asked");

    crew.settings.limits.spend.spend_per_run = Some(1.00);
    let resumed = Bench::over(&crew, history)?;
    resumed.host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
            cause: ResumeCause::User,
            settings_changed: true,
            cli_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        })),
    )?;
    let done = resumed.build().await?;
    assert_eq!(done.summary.as_deref(), Some("2 tasks merged into the run"));
    let built = builds(&builder);
    assert_eq!(built.len(), 2, "the second task's session never started");
    let started = &built[1];
    assert!(started.resume.is_none(), "{:?}", started.resume);
    let task = started
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::Task)
        .context("the whole prompt, with its task")?;
    assert!(
        task.text.starts_with("task-002 · Build task-002"),
        "{}",
        task.text
    );
    assert_eq!(
        tries(&resumed.sink.events(), "task-002"),
        [(TryCause::First, worker(WORKER_A))],
        "it went on with the try it had begun"
    );
    assert_eq!(state_of(&resumed, "task-002"), TaskState::Merged);
    Ok(())
}

/// What a session has spent so far, as its CLI reports it.
fn spent(usd: f64) -> RunnerEvent {
    RunnerEvent::Session(SessionEvent::Limit(LimitEvent::Spend(SpendStatus {
        level: MeterAction::Continue,
        limit: MeterLimit::Usd(0.60),
        spend: Spend {
            cost_usd: usd,
            ..Spend::default()
        },
        fraction_used: None,
        work: Default::default(),
        work_limits: Default::default(),
        binding: None,
    })))
}

/// The worker's build sessions, in the order they started.
fn builds(worker: &FakeRunner) -> Vec<SessionSpec> {
    worker
        .specs()
        .into_iter()
        .filter(|spec| spec.access == Access::Build)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn stuck_defer_and_split() -> anyhow::Result<()> {
    // Deferred: the task waits and the run goes on without it, and without the task that
    // depends on it.
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_check()];
    crew.services
        .workers
        .add(fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.checks.fail_next("1 failed");
    crew.services.checks.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[
            spec("task-001", &[], WORKER_B, REVIEWER),
            spec("task-002", &["task-001"], WORKER_B, REVIEWER),
        ],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(ApprovalKind::Stuck, AnswerAction::Defer, None)
        .await?;
    let done = finish(build).await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("0 tasks merged into the run, 1 deferred, 1 waiting on deferred tasks")
    );
    assert_eq!(state_of(&bench, "task-001"), TaskState::Deferred);
    assert_eq!(state_of(&bench, "task-002"), TaskState::Waiting);

    // Split: your note becomes new tasks through PlanToTasks and the graph validator.
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_check()];
    // The build's tie-breaker owns its critical-path task, so this task's worker splits it.
    let proposal = json!({
        "tasks": [
            {"key": "store", "title": "Store", "task": "The store.", "kind": "build",
             "owned_paths": ["src/store/**"], "read_only_paths": [], "depends_on": [],
             "criteria": ["G1"]},
            {"key": "api", "title": "API", "task": "The API.", "kind": "build",
             "owned_paths": ["src/api/**"], "read_only_paths": [], "depends_on": ["store"],
             "criteria": ["G1"]}
        ]
    });
    let splitter = FakeRunner::cli(WORKER_B, "openai").respond_with(move |spec| {
        if spec.step == Some(StepName::PlanToTasks) {
            return FakeSession::structured(proposal.clone());
        }
        work(spec, &[("src/part.rs", "fn part() {}\n")], Duration::ZERO)
    });
    crew.services.workers.add(splitter);
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.checks.fail_next("1 failed");
    crew.services.checks.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(
            ApprovalKind::Stuck,
            AnswerAction::Split,
            Some("one task for the store, one for the API"),
        )
        .await?;
    let done = finish(build).await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("2 tasks merged into the run, 1 cancelled")
    );
    let events = bench.sink.events();
    let last = task_events(&events, "task-001")
        .into_iter()
        .rev()
        .find_map(|e| match e {
            TaskEvent::State { to, reason, .. } => Some((to, reason)),
            _ => None,
        })
        .context("no state")?;
    assert_eq!(last.0, TaskState::Cancelled);
    assert_eq!(last.1, "split into task-002, task-003");
    let created: Vec<TaskSpec> = events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) => Some((**spec).clone()),
            _ => None,
        })
        .filter(|spec| spec.id.as_str() != "task-001")
        .collect();
    let ids: Vec<&str> = created.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["task-002", "task-003"]);
    assert_eq!(created[1].depends_on, [TaskId::new("task-002")?]);
    assert_eq!(created[1].worker, Some(worker(WORKER_B)));
    Ok(())
}

/// A worker that writes `files`, then repeats one call until the watchdog stops it, as Qwen on
/// OpenCode did once its tests passed.
fn stuck_worker(name: &str, files: &'static [(&'static str, &'static str)]) -> FakeRunner {
    FakeRunner::cli(name, &provider_of(name)).respond_with(move |spec| {
        work(spec, files, Duration::ZERO);
        let stuck = FakeSession::ended(EndReason::Stuck {
            watchdog: Watchdog::Repeat,
            calls: Some(CallCount {
                name: "execute".to_owned(),
                times: 12,
            }),
        });
        files.iter().fold(stuck, |session, (path, _)| {
            session.with_event(support::wrote(path))
        })
    })
}

/// Whether each run of the task's command checks passed.
fn command_checks(events: &[RunEvent], task: &str) -> Vec<bool> {
    events
        .iter()
        .filter(|e| e.task_id.as_ref().is_some_and(|id| id.as_str() == task))
        .filter_map(|e| match &e.kind {
            RunEventKind::Check(outcome) if outcome.kind == CheckKind::Command => {
                Some(outcome.passed)
            }
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_session_whose_work_passes_the_checks_goes_on_as_done() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_check()];
    crew.services
        .workers
        .add(stuck_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.limits.set_fallback(worker(WORKER_A));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    let done = bench.build().await?;
    assert_eq!(done.summary.as_deref(), Some("1 task merged into the run"));

    let events = bench.sink.events();
    assert_eq!(
        tries(&events, "task-001"),
        [(TryCause::First, worker(WORKER_B))]
    );
    let states = states(&events, "task-001");
    assert!(!states.contains(&TaskState::Failed), "{states:?}");
    // The checks ran once, on the stopped session's commit, and nothing fell back.
    assert_eq!(command_checks(&events, "task-001"), [true]);
    assert!(crew.services.limits.stops().is_empty());
    let checking = task_events(&events, "task-001")
        .into_iter()
        .find_map(|e| match e {
            TaskEvent::State {
                to: TaskState::Checking,
                reason,
                ..
            } => Some(reason),
            _ => None,
        })
        .context("never checked")?;
    assert_eq!(
        checking,
        "its work passes the checks, though its session was stopped: it repeated the same \
         execute call 12 times"
    );
    assert!(asked_while_building(&events).is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_session_whose_work_fails_the_checks_falls_back_as_before() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_check()];
    crew.services
        .workers
        .add(stuck_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services
        .workers
        .add(fake_worker(WORKER_A, &[("src/a.rs", "fn a() { 1 }\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.limits.set_fallback(worker(WORKER_A));
    crew.services.checks.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    let done = bench.build().await?;
    assert_eq!(done.summary.as_deref(), Some("1 task merged into the run"));

    let events = bench.sink.events();
    let tries = tries(&events, "task-001");
    assert_eq!(tries[0], (TryCause::First, worker(WORKER_B)));
    assert_eq!(tries[1].1, worker(WORKER_A));
    let states = states(&events, "task-001");
    assert!(states.contains(&TaskState::Failed), "{states:?}");
    assert_eq!(command_checks(&events, "task-001"), [false, true]);
    // Failing checks leave the stop to recovery, as any stuck session's.
    let stops = crew.services.limits.stops();
    assert_eq!(stops.len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn with_no_checks_set_you_are_asked_and_may_keep_its_work() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    assert!(crew.settings.checks.run.is_empty());
    crew.services
        .workers
        .add(stuck_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    // Another model would take it, yet it isn't given away unasked.
    crew.services.limits.set_fallback(worker(WORKER_A));
    crew.services
        .fallback
        .set_candidates(vec![candidate(WORKER_A)]);
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    let (approval, _) = bench
        .answer(ApprovalKind::Stuck, AnswerAction::KeepWork, None)
        .await?;
    let done = finish(build).await?;
    assert_eq!(done.summary.as_deref(), Some("1 task merged into the run"));

    let events = bench.sink.events();
    let asked = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(o)) if o.id == approval => Some(o.clone()),
            _ => None,
        })
        .context("never asked")?;
    let first = &asked.options[0];
    assert_eq!((first.key, first.label.as_str()), ('k', "Keep its work"));
    assert_eq!(first.action, AnswerAction::KeepWork);
    let detail = first.detail.as_deref().unwrap_or_default();
    assert!(detail.starts_with("src/a.rs +2 −0"), "{detail}");
    let labels: Vec<&str> = asked.options.iter().map(|o| o.label.as_str()).collect();
    assert!(labels.contains(&"Give it to a fallback"), "{labels:?}");
    assert!(crew.services.limits.stops().is_empty());
    assert_eq!(
        tries(&events, "task-001"),
        [(TryCause::First, worker(WORKER_B))]
    );
    let kept = task_events(&events, "task-001").into_iter().any(|e| {
        matches!(e, TaskEvent::State { to: TaskState::Checking, reason, .. }
            if reason == "you kept its work; the checks decide")
    });
    assert!(kept);
    Ok(())
}

/// A bare OpenCode set to the very model that got stuck would only run it again, so it is no
/// other model to give the task to.
#[tokio::test(flavor = "multi_thread")]
async fn a_bare_cli_set_to_the_stuck_model_is_no_other_model() -> anyhow::Result<()> {
    // The fake registry tells Qwen's provider by its model id, and a bare CLI's by nothing.
    const QWEN: &str = "opencode/qwen3-coder:30b";
    let mut crew = Crew::new()?;
    crew.settings
        .agents
        .entry("opencode".to_owned())
        .or_default()
        .model = Some("qwen3-coder:30b".to_owned());
    crew.services.workers.add(stuck_worker(
        QWEN,
        &[("slug.py", "def slugify(title): ...\n")],
    ));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .fallback
        .set_candidates(vec![candidate("opencode")]);
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], QWEN, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(ApprovalKind::Stuck, AnswerAction::Defer, None)
        .await?;
    finish(build).await?;

    let events = bench.sink.events();
    let asked = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(o)) if o.kind == ApprovalKind::Stuck => {
                Some(o.clone())
            }
            _ => None,
        })
        .context("never asked")?;
    let labels: Vec<&str> = asked.options.iter().map(|o| o.label.as_str()).collect();
    assert!(
        labels.contains(&"Try again on the same model"),
        "{labels:?}"
    );
    assert_eq!(
        tries(&events, "task-001"),
        [(TryCause::First, worker(QWEN))]
    );
    Ok(())
}

/// The same with OpenCode left unset: its runner names the Qwen its own list marks Default, so
/// the bare `opencode` is that Qwen too, and the question names it.
#[tokio::test(flavor = "multi_thread")]
async fn a_bare_cli_that_runs_the_stuck_model_is_no_other_model() -> anyhow::Result<()> {
    const QWEN: &str = "opencode/qwen3-coder:30b";
    let crew = Crew::new()?;
    crew.services.workers.add(stuck_worker(
        QWEN,
        &[("slug.py", "def slugify(title): ...\n")],
    ));
    crew.services
        .workers
        .add(FakeRunner::cli("opencode", "").running(QWEN));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .fallback
        .set_candidates(vec![candidate("opencode")]);
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], QWEN, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(ApprovalKind::Stuck, AnswerAction::Defer, None)
        .await?;
    finish(build).await?;

    let events = bench.sink.events();
    let asked = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(o)) if o.kind == ApprovalKind::Stuck => {
                Some(o.clone())
            }
            _ => None,
        })
        .context("never asked")?;
    let labels: Vec<&str> = asked.options.iter().map(|o| o.label.as_str()).collect();
    assert!(
        labels.contains(&"Try again on the same model"),
        "{labels:?}"
    );
    assert_eq!(
        tries(&events, "task-001"),
        [(TryCause::First, worker(QWEN))]
    );
    Ok(())
}

/// On autopilot work nothing checked is never kept: the same model tries once more, told
/// what stopped it, and then the task waits deferred with its work on its branch.
#[tokio::test(flavor = "multi_thread")]
async fn on_autopilot_unchecked_work_gets_one_more_try_then_waits_deferred() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.approvals.stuck.mode = ApprovalMode::Autopilot;
    let stuck = crew
        .services
        .workers
        .add(stuck_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    // No limit is set, so one more try always fits.
    let limits_off = RunSpend {
        spent: Spend::default(),
        limit: None,
        lines: Vec::new(),
        reserve_left: None,
    };
    bench
        .host
        .record(None, None, RunEventKind::Spend(SpendEvent::Run(limits_off)))?;
    let done = bench.build().await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("0 tasks merged into the run, 1 deferred")
    );

    let events = bench.sink.events();
    assert_eq!(
        tries(&events, "task-001"),
        [
            (TryCause::First, worker(WORKER_B)),
            (TryCause::AfterFailure, worker(WORKER_B)),
        ]
    );
    let decided: Vec<AnswerAction> = events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Approval(ApprovalEvent::Decided(d)) if d.by == Party::Autopilot => {
                Some(d.action.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(decided, [AnswerAction::Fallback, AnswerAction::Defer]);
    // No other model can take it, so the fallback says it's the same model again.
    let asked = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(o)) if o.kind == ApprovalKind::Stuck => {
                Some(o.clone())
            }
            _ => None,
        })
        .context("never asked")?;
    let labels: Vec<&str> = asked.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels[0], "Keep its work");
    assert!(
        labels.contains(&"Try again on the same model"),
        "{labels:?}"
    );
    assert!(!labels.contains(&"Give it to a fallback"), "{labels:?}");
    let ApprovalPacket::Stuck(packet) = &asked.packet else {
        bail!("not a stuck packet");
    };
    assert!(
        packet.last_failure.ends_with(
            " repeated the same execute call 12 times. Its commit is on the task's branch. No \
             other model can retry it."
        ),
        "{}",
        packet.last_failure
    );
    assert_eq!(builds(&stuck).len(), 2);
    assert_eq!(state_of(&bench, "task-001"), TaskState::Deferred);
    Ok(())
}

/// When your fallback mode asks, a stuck task's fallback is yours to pick: the question opens,
/// and the retry ladder doesn't give the task away first.
#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_task_asks_who_takes_it_when_the_fallback_mode_asks() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let stuck = FakeRunner::cli(WORKER_B, &provider_of(WORKER_B)).respond_with(|_| {
        FakeSession::ended(EndReason::Stuck {
            watchdog: Watchdog::Repeat,
            calls: None,
        })
    });
    crew.services.workers.add(stuck);
    crew.services
        .workers
        .add(fake_worker(WORKER_A, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.limits.set_fallback(worker(REVIEWER));
    crew.services
        .limits
        .set_next(RecoveryAction::Ask(Box::new(FallbackPlan {
            task: TaskId::new("task-001")?,
            title: "Build task-001".to_owned(),
            stopped: worker(WORKER_B),
            reason: "stuck (Repeat)".to_owned(),
            resets_at: None,
            work: Default::default(),
            checks_ran: false,
            candidates: vec![candidate(WORKER_A)],
            mode: FallbackMode::Ask,
            strategy: Default::default(),
            race_count: 1,
            also_moving: Vec::new(),
        })));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    let pick = AnswerAction::Candidate {
        worker: worker(WORKER_A),
    };
    bench.answer(ApprovalKind::Fallback, pick, None).await?;
    let done = finish(build).await?;
    assert_eq!(done.summary.as_deref(), Some("1 task merged into the run"));
    let events = bench.sink.events();
    assert_eq!(asked_while_building(&events), [ApprovalKind::Fallback]);
    let tries = tries(&events, "task-001");
    assert_eq!(tries.len(), 2, "{tries:?}");
    assert_eq!(tries[1].1, worker(WORKER_A));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lifetime_task_resumes_on_retry() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_check()];
    let worker = crew
        .services
        .workers
        .add(fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.checks.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let specs = worker.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].resume, None, "the task's first session is fresh");
    assert_eq!(
        specs[1].resume.as_deref(),
        Some(format!("v-{}", specs[0].key).as_str()),
        "the checks retry resumes the task's own session"
    );
    let kinds: Vec<InputPartKind> = specs[1].prompt.iter().map(|p| p.kind).collect();
    assert!(kinds.contains(&InputPartKind::CheckOutput), "{kinds:?}");
    assert!(
        !kinds.contains(&InputPartKind::Prompt),
        "a resume sends only what's new"
    );
    Ok(())
}

/// While a run decides without you, every agent hears up front, right after its instructions,
/// to decide where it would ask. One that ends its turn with a question anyway is told again
/// in its own session, carries on, and the task is built.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_asks_anyway_is_told_to_decide_and_carries_on() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let worker = crew.services.workers.add(asks_before_it_works(WORKER_B));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;
    assert_eq!(state_of(&bench, "task-001"), TaskState::Merged);

    let specs = worker.specs();
    assert_eq!(specs.len(), 2);
    let told_first = specs[0]
        .prompt
        .iter()
        .position(|part| part.label == "decide, don't ask");
    let instructions = specs[0]
        .prompt
        .iter()
        .take_while(|part| part.kind.instructs())
        .count();
    assert_eq!(
        told_first,
        Some(instructions),
        "right after its instructions"
    );
    assert!(
        specs[0].prompt[instructions]
            .text
            .starts_with("Decide, don't ask.")
    );
    assert!(specs[1].resume.is_some(), "it goes on in its own session");
    let reply = &specs[1].prompt[0];
    assert_eq!(reply.label, "decide and carry on");
    assert!(reply.text.contains("Decide yourself"), "{}", reply.text);
    assert!(reply.text.contains("\"Assumed:\""), "{}", reply.text);
    let said = notes(&bench);
    assert!(
        said.iter()
            .any(|note| note.contains("ended its turn with a question")
                && note.contains("told to decide and carry on")),
        "{said:?}"
    );
    // What it assumed is said once, for you to review.
    let assumed: Vec<&String> = said
        .iter()
        .filter(|note| note.contains("assumed:"))
        .collect();
    assert_eq!(assumed.len(), 1, "{said:?}");
    assert!(
        assumed[0].ends_with("assumed: the simpler of the two ways"),
        "{}",
        assumed[0]
    );
    Ok(())
}

/// An agent that keeps asking is told to decide `approvals.ask_limit` times, then once to take
/// the option it recommended. Its next question is a report that doesn't match, as before, so
/// no task goes round forever.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_keeps_asking_runs_out_of_replies() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    assert_eq!(crew.settings.approvals.ask_limit, 2);
    let always_asks = FakeRunner::cli(WORKER_B, "openai").respond_with(|spec| {
        FakeSession::completed("Which do you prefer, A or B?")
            .with_cli_session(&format!("v-{}", spec.key))
    });
    let worker = crew.services.workers.add(always_asks);
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(ApprovalKind::Stuck, AnswerAction::Cancel, None)
        .await?;
    finish(build).await?;

    let replies: Vec<String> = worker
        .specs()
        .iter()
        .skip(1)
        .map(|spec| spec.prompt[0].text.clone())
        .collect();
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert!(replies[0].contains("Decide yourself"), "{}", replies[0]);
    assert!(replies[1].contains("Decide yourself"), "{}", replies[1]);
    assert!(
        replies[2].contains("Take the option you recommended"),
        "{}",
        replies[2]
    );
    Ok(())
}

/// In supervised mode nobody replies for you: the agent hears nothing up front, and its
/// question is a report that doesn't match, which comes to you.
#[tokio::test(flavor = "multi_thread")]
async fn in_supervised_mode_nobody_replies_to_an_agent_that_asks() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let approvals = &mut crew.settings.approvals;
    for point in [
        &mut approvals.understand,
        &mut approvals.plan,
        &mut approvals.merge,
        &mut approvals.stuck,
        &mut approvals.limit,
        &mut approvals.change_request,
    ] {
        point.mode = ApprovalMode::Supervised;
    }
    let worker = crew.services.workers.add(asks_before_it_works(WORKER_B));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(ApprovalKind::Stuck, AnswerAction::Cancel, None)
        .await?;
    finish(build).await?;

    let specs = worker.specs();
    assert_eq!(specs.len(), 1, "nobody replied");
    assert!(
        specs[0]
            .prompt
            .iter()
            .all(|part| part.label != "decide, don't ask")
    );
    assert!(notes(&bench).iter().all(|note| !note.contains("told to")));
    Ok(())
}

/// On autopilot every session of the run is started as one nobody answers, so a CLI with a
/// style of its own for that works in it (Claude Code's "Proactive"). In auto you may still be
/// asked, and none is.
#[tokio::test(flavor = "multi_thread")]
async fn only_a_run_on_autopilot_starts_its_sessions_as_unanswered() -> anyhow::Result<()> {
    const FILES: &[(&str, &str)] = &[("src/a.rs", "fn a() {}\n")];
    for autopilot in [false, true] {
        let mut crew = Crew::new()?;
        crew.settings.approvals.autopilot = autopilot;
        let builds = crew.services.workers.add(fake_worker(WORKER_B, FILES));
        let reviews = crew.services.workers.add(reviewer(REVIEWER));
        let bench = Bench::new(
            &crew,
            RunMode::Crew,
            &[spec("task-001", &[], WORKER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;
        assert_eq!(state_of(&bench, "task-001"), TaskState::Merged);

        let specs = [builds.specs(), reviews.specs()].concat();
        assert!(specs.len() >= 2, "a build and its review");
        assert!(
            specs.iter().all(|spec| spec.autopilot == autopilot),
            "autopilot {autopilot}"
        );
    }
    Ok(())
}

/// A worker whose first turn ends with a question, and who does the work once it is resumed,
/// saying in its report what it assumed.
fn asks_before_it_works(name: &str) -> FakeRunner {
    const FILES: &[(&str, &str)] = &[("src/a.rs", "fn a() {}\n")];
    FakeRunner::cli(name, &provider_of(name)).respond_with(|spec| {
        if spec.resume.is_some() {
            let _written = work(spec, FILES, Duration::ZERO);
            let mut done = report("done");
            done["concerns"] = json!(["Assumed: the simpler of the two ways."]);
            return FakeSession::structured(done).with_cli_session(&format!("v-{}", spec.key));
        }
        FakeSession::completed("I found two ways to do it.\n\nShould I proceed?")
            .with_cli_session(&format!("v-{}", spec.key))
    })
}

fn notes(bench: &Bench) -> Vec<String> {
    bench
        .sink
        .events()
        .into_iter()
        .filter_map(|event| match event.kind {
            RunEventKind::Note(note) => Some(note.text),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn status_part_on_each_delivery_without_tool() -> anyhow::Result<()> {
    for status_tool in [false, true] {
        let mut crew = Crew::new()?;
        crew.settings.checks.run = vec![test_check()];
        let capabilities = RunnerCapabilities {
            status_tool,
            ..FakeRunner::cli(WORKER_B, "openai").capabilities()
        };
        let worker = crew.services.workers.add(
            fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]).with_capabilities(capabilities),
        );
        crew.services.workers.add(reviewer(REVIEWER));
        crew.services.checks.fail_next("1 failed");
        let bench = Bench::new(
            &crew,
            RunMode::Crew,
            &[spec("task-001", &[], WORKER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;

        let specs = worker.specs();
        let status_parts = |i: usize| {
            specs[i]
                .prompt
                .iter()
                .filter(|p| p.kind == InputPartKind::Status)
                .count()
        };
        assert_eq!(
            status_parts(0),
            1,
            "the first prompt always has the status summary"
        );
        let expected = usize::from(!status_tool);
        assert_eq!(status_parts(1), expected, "status_tool = {status_tool}");
    }
    Ok(())
}

/// `[prompts.reviewers] writer = "<model>"`: that model is asked first, with what the reviewer
/// would have been told, and the reviewer reads what it wrote in the built-in's place.
#[tokio::test]
async fn a_writer_model_writes_the_reviewers_instructions() -> anyhow::Result<()> {
    const WRITER: &str = "opencode/writer-x";
    let mut crew = Crew::new()?;
    crew.settings.prompts.roles.insert(
        "reviewer".to_owned(),
        RolePrompts {
            writer: Some(Writer::Model(worker(WRITER))),
            ..RolePrompts::default()
        },
    );
    crew.services
        .workers
        .add(fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    let reviewer = crew.services.workers.add(reviewer(REVIEWER));
    let writer = crew
        .services
        .workers
        .add(FakeRunner::cli(WRITER, "writer").respond_with(|_| {
            FakeSession::structured(json!({ "instructions": "Review it for races." }))
        }));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;

    let asked = &writer.specs()[0];
    assert_eq!(asked.step, Some(StepName::WriteInstructions));
    let input = asked
        .prompt
        .iter()
        .find(|part| part.kind == InputPartKind::StepInput)
        .context("the writer got no input")?;
    assert!(
        input.text.contains("\"role\": \"reviewer\""),
        "{}",
        input.text
    );
    assert!(
        input.text.contains("Review the task's change"),
        "{}",
        input.text
    );

    let prompt = &reviewer.specs()[0].prompt;
    let headings: Vec<String> = prompt
        .iter()
        .filter_map(argonauts_core::prompts::prompt_parts::heading)
        .collect();
    assert_eq!(
        headings,
        ["Frame · built in", "From the writer · Writer X on OpenCode"],
        "{headings:?}"
    );
    assert_eq!(prompt[1].text.trim_end(), "Review it for races.");
    // The worker's role has no writer model, so nobody rewrote its brief.
    assert_eq!(writer.specs().len(), 1);
    Ok(())
}

/// A worker file that says `mode: replace` takes the place of the orchestrator's brief as
/// the worker's instructions. The brief still says what the task is, so it stays in the task
/// the worker is handed.
#[tokio::test]
async fn a_worker_file_that_replaces_keeps_the_task_readable() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.repo.write(
        ".argonauts/roles/worker.md",
        "---\nmode: replace\n---\nBuild it test first.\n",
    )?;
    let worker = crew
        .services
        .workers
        .add(fake_worker(WORKER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], WORKER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;

    let prompt = &worker.specs()[0].prompt;
    let headings: Vec<String> = prompt
        .iter()
        .filter_map(argonauts_core::prompts::prompt_parts::heading)
        .collect();
    assert_eq!(
        headings,
        ["Frame · built in", "Your file · roles/worker.md"],
        "{headings:?}"
    );
    assert_eq!(prompt[1].text.trim_end(), "Build it test first.");
    assert_eq!(prompt[2].label, "decide, don't ask");
    let task = &prompt[3];
    assert_eq!(task.kind, InputPartKind::Task);
    assert!(task.text.contains("Do task-001."), "{}", task.text);
    Ok(())
}

/// A workflow file whose steps run as written hands a worker its step's own words. Whatever
/// starts the task says so, and the worker reads them as the workflow's, not as words the
/// orchestrator wrote. A run's other tasks stay the orchestrator's.
#[tokio::test]
async fn a_brief_that_is_a_workflows_own_step_is_read_as_the_workflows() -> anyhow::Result<()> {
    use argonauts_crew::prompts::instructions::{CallAuthor, brief_written_by};

    let crew = Crew::new()?;
    let first = crew
        .services
        .workers
        .add(fake_worker(WORKER_A, &[("src/a.rs", "fn a() {}\n")]));
    let second = crew
        .services
        .workers
        .add(fake_worker(WORKER_B, &[("src/b.rs", "fn b() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[
            spec("task-001", &[], WORKER_A, REVIEWER),
            spec("task-002", &[], WORKER_B, REVIEWER),
        ],
    )
    .await?;
    let as_written = CallAuthor::Workflow {
        name: "spec-first",
        step: Some("build"),
    };
    brief_written_by(&bench.host, &TaskId::new("task-001")?, as_written)?;
    // Said twice, as a run that picks up again says it, and said of a task that isn't one.
    brief_written_by(&bench.host, &TaskId::new("task-001")?, as_written)?;
    brief_written_by(
        &bench.host,
        &TaskId::new("task-002")?,
        CallAuthor::Orchestrator,
    )?;
    bench.build().await?;

    let opens_with = |worker: &FakeRunner| {
        let prompt = &worker.specs()[0].prompt;
        let heading = argonauts_core::prompts::prompt_parts::heading(&prompt[1]);
        (
            heading.unwrap_or_default(),
            prompt[1].text.trim_end().to_owned(),
        )
    };
    assert_eq!(
        opens_with(&first),
        (
            "From the workflow · spec-first, step build".to_owned(),
            "Do task-001.".to_owned()
        )
    );
    assert_eq!(
        opens_with(&second),
        (
            "From the orchestrator · Claude Code".to_owned(),
            "Do task-002.".to_owned()
        )
    );
    Ok(())
}

/// A reviewer file that names a CLI on the crew tells the reviewers who made the work. The
/// run says so once, however many reviewers read the file, and they read it all the same.
#[tokio::test(flavor = "multi_thread")]
async fn a_reviewer_file_that_names_a_cli_is_warned_about_once() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.repo
        .write(".argonauts/roles/reviewer.md", "Go easy on Codex's code.\n")?;
    crew.services
        .workers
        .add(fake_worker(WORKER_A, &[("src/a.rs", "fn a() {}\n")]));
    crew.services
        .workers
        .add(fake_worker(WORKER_B, &[("src/b.rs", "fn b() {}\n")]));
    let reviewer = crew.services.workers.add(reviewer(REVIEWER));
    let tasks = [
        spec("task-001", &[], WORKER_A, REVIEWER),
        spec("task-002", &[], WORKER_B, REVIEWER),
    ];
    let bench = Bench::new(&crew, RunMode::Crew, &tasks).await?;
    bench.build().await?;

    let warned: Vec<String> = bench
        .sink
        .events()
        .into_iter()
        .filter_map(|event| match event.kind {
            RunEventKind::Note(note) if note.level == NoteLevel::Warn => Some(note.text),
            _ => None,
        })
        .collect();
    assert_eq!(
        warned,
        [
            "roles/reviewer.md names Codex, which tells the reviewers who made the work. They \
          aren't told otherwise"
        ]
    );
    let reviews = reviewer.specs();
    assert_eq!(reviews.len(), 2);
    for review in reviews {
        let yours = review
            .prompt
            .iter()
            .find(|part| part.kind == InputPartKind::RoleFile)
            .context("the reviewer didn't read your file")?;
        assert_eq!(yours.text.trim_end(), "Go easy on Codex's code.");
    }
    Ok(())
}

// Scenario helpers.

struct Bench {
    host: Host,
    sink: Arc<MemorySink>,
}

impl Bench {
    /// A run with an approved goal and `tasks` created, as `divide` leaves it.
    async fn new(crew: &Crew, mode: RunMode, tasks: &[TaskSpec]) -> anyhow::Result<Self> {
        let base = crew.repo.head()?;
        let history = history(crew, mode, &base, tasks)?;
        let (context, sink, _commands) = crew.context(history)?;
        Git::with_env(crew.git_env())
            .add_detached_worktree(crew.repo.root(), &context.repo.base_worktree, &base)
            .await?;
        let state = RunState::fold(&context.history);
        let shared = crew.shared(&context, state)?;
        Ok(Self {
            host: Host::new(Arc::new(shared)),
            sink,
        })
    }

    /// The same run rebuilt from its log, as the engine builds it when the run resumes.
    fn over(crew: &Crew, history: Vec<RunEvent>) -> anyhow::Result<Self> {
        let (context, sink, _commands) = crew.context(history)?;
        let state = RunState::fold(&context.history);
        let shared = crew.shared(&context, state)?;
        Ok(Self {
            host: Host::new(Arc::new(shared)),
            sink,
        })
    }

    async fn build(&self) -> anyhow::Result<StepDone> {
        finish(self.spawn_build()).await
    }

    fn spawn_build(&self) -> tokio::task::JoinHandle<argonauts_crew::EngineResult<StepDone>> {
        let host = self.host.clone();
        tokio::spawn(async move { task::build_step(&host, &build_call()).await })
    }

    /// Answers the newest open approval of `kind` once it opens.
    async fn answer(
        &self,
        kind: ApprovalKind,
        action: AnswerAction,
        text: Option<&str>,
    ) -> anyhow::Result<(ApprovalId, String)> {
        for _ in 0..600 {
            let events = self.sink.events();
            let decided: Vec<_> = events
                .iter()
                .filter_map(|e| match &e.kind {
                    RunEventKind::Approval(ApprovalEvent::Decided(d)) => Some(d.id.clone()),
                    _ => None,
                })
                .collect();
            let open = events.iter().rev().find_map(|e| match &e.kind {
                RunEventKind::Approval(ApprovalEvent::Opened(o))
                    if o.kind == kind && !decided.contains(&o.id) =>
                {
                    Some((o.id.clone(), o.evidence_sha256.clone(), o.title.clone()))
                }
                _ => None,
            });
            if let Some((id, evidence, title)) = open {
                let answer = ApprovalAnswer {
                    id: id.clone(),
                    action: action.clone(),
                    text: text.map(str::to_owned),
                    evidence_sha256: evidence,
                    tasks: Vec::new(),
                };
                approvals::answer(&self.host, answer).map_err(anyhow::Error::msg)?;
                return Ok((id, title));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        bail!("no {kind:?} approval opened")
    }
}

async fn finish(
    build: tokio::task::JoinHandle<argonauts_crew::EngineResult<StepDone>>,
) -> anyhow::Result<StepDone> {
    let joined = tokio::time::timeout(deadline::scaled(Duration::from_secs(60)), build)
        .await
        .context("the build step didn't finish")?;
    Ok(joined??)
}

fn build_call() -> StepCall {
    StepCall {
        at: StepAt {
            step: StepRef {
                workflow: WorkflowId::numbered(1),
                phase: 3,
                path: vec![0],
            },
            round: 1,
        },
        depth: 0,
        standard: StandardPhase::Do,
        step: serde_json::from_value(json!({"kind": "build", "input": "plan"}))
            .expect("a build step"),
        item: None,
    }
}

fn history(
    crew: &Crew,
    mode: RunMode,
    base: &str,
    tasks: &[TaskSpec],
) -> anyhow::Result<Vec<RunEvent>> {
    let run = RunId::new("run-01")?;
    let started = RunStarted {
        request: "Build it".into(),
        title: "Build it".into(),
        mode,
        forced: true,
        workflow: "default".into(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: crew.repo.root().to_path_buf(),
        base_branch: Some("main".into()),
        base_commit: base.to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::to_value(&crew.settings)?,
        overrides: Vec::new(),
        cli_versions: BTreeMap::new(),
        version: "0.0.1".into(),
        outside_agent: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    };
    let goal = GoalFile {
        run: run.clone(),
        version: 1,
        summary: "Build it".into(),
        criteria: vec![Criterion::new("G1", "It works.")],
        in_scope: Vec::new(),
        out_of_scope: Vec::new(),
        assumptions: Vec::new(),
        answers: Vec::new(),
        source: GoalSource::Readings,
        path: PathBuf::from(".argonauts/goal/run-01.md"),
        sha256: "0".repeat(64),
    };
    let mut kinds = vec![
        (
            None,
            RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
        ),
        (
            None,
            RunEventKind::Goal(GoalEvent::Approved {
                file: goal,
                by: Party::User,
            }),
        ),
    ];
    kinds.extend(approved_plan(tasks));
    for spec in tasks {
        let id = Some(spec.id.clone());
        kinds.push((
            id.clone(),
            RunEventKind::Task(TaskEvent::Created(Box::new(spec.clone()))),
        ));
        kinds.push((
            id,
            RunEventKind::Task(TaskEvent::State {
                from: None,
                to: TaskState::Waiting,
                reason: "created".into(),
            }),
        ));
    }
    Ok(kinds
        .into_iter()
        .enumerate()
        .map(|(seq, (task_id, kind))| RunEvent {
            seq: seq as u64,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: run.clone(),
            task_id,
            session: None,
            source: "argonauts-code".into(),
            kind,
        })
        .collect())
}

/// The plan approval, opened and approved, which crew build sessions wait for.
fn approved_plan(tasks: &[TaskSpec]) -> Vec<(Option<TaskId>, RunEventKind)> {
    let id = ApprovalId::numbered(1);
    let evidence = "0".repeat(64);
    let packet = PlanPacket {
        summary: "The plan".into(),
        judging: None,
        near_tie: false,
        decisions: Vec::new(),
        graph: TaskGraph {
            tasks: tasks.to_vec(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        limit: LimitPlan {
            lines: Vec::new(),
            total: LimitAmount {
                unit: LimitUnit::Usd,
                amount: 10.0,
            },
            estimate_low: 0.0,
            estimate_high: 0.0,
        },
        new_dependencies: Vec::new(),
        flags: Vec::new(),
        workflow: None,
        seats: Vec::new(),
        objections: Vec::new(),
        offer: None,
    };
    let opened = ApprovalOpened {
        id: id.clone(),
        kind: ApprovalKind::Plan,
        title: "Approve this plan?".into(),
        task: None,
        packet: ApprovalPacket::Plan(Box::new(packet)),
        evidence_sha256: evidence.clone(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    let decided = ApprovalDecided {
        id,
        action: AnswerAction::Approve,
        text: None,
        by: Party::User,
        evidence_sha256: evidence,
        auto_preview: None,
        tasks: Vec::new(),
    };
    vec![
        (
            None,
            RunEventKind::Approval(ApprovalEvent::Opened(Box::new(opened))),
        ),
        (
            None,
            RunEventKind::Approval(ApprovalEvent::Decided(decided)),
        ),
    ]
}

fn spec(id: &str, deps: &[&str], who: &str, reviewer: &str) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(id).unwrap(),
        title: format!("Build {id}"),
        task: format!("Do {id}."),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: deps.iter().map(|d| TaskId::new(*d).unwrap()).collect(),
        criteria: vec!["G1".into()],
        worker: Some(worker(who)),
        reviewers: vec![worker(reviewer)],
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
    }
}

fn worker(name: &str) -> ModelRef {
    name.parse().unwrap()
}

fn test_check() -> CheckCommand {
    CheckCommand {
        name: "test".into(),
        command: vec!["cargo".into(), "test".into()],
    }
}

fn report(status: &str) -> Value {
    json!({
        "status": status, "summary": "did the task", "files_changed": [],
        "tests_run": [], "concerns": [], "questions": [], "change_requests": [],
        "memory_proposals": []
    })
}

/// A CLI worker that writes `files` into its worktree and reports done. Its CLI session
/// id is `v-<session key>`, so resumes can be traced.
fn fake_worker(name: &str, files: &'static [(&'static str, &'static str)]) -> FakeRunner {
    slow_worker(name, files, Duration::ZERO)
}

/// A worker whose sessions take `delay` per event, so two can be seen running at once.
fn slow_worker(
    name: &str,
    files: &'static [(&'static str, &'static str)],
    delay: Duration,
) -> FakeRunner {
    FakeRunner::cli(name, &provider_of(name)).respond_with(move |spec| work(spec, files, delay))
}

/// Writes `files` into the session's worktree and reports done.
fn work(spec: &SessionSpec, files: &[(&str, &str)], delay: Duration) -> FakeSession {
    for (path, text) in files {
        let path = spec.workdir.join(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, format!("{text}// {}\n", spec.key));
    }
    FakeSession::structured(report("done"))
        .with_cli_session(&format!("v-{}", spec.key))
        .with_delay(delay)
}

/// A worker whose every session ends at a limit you set.
fn at_its_limit(name: &str) -> FakeRunner {
    FakeRunner::cli(name, &provider_of(name)).respond_with(|_| {
        FakeSession::ended(EndReason::LimitHit {
            dimension: LimitDimension::Spend,
        })
    })
}

fn reviewer(name: &str) -> FakeRunner {
    FakeRunner::cli(name, &provider_of(name)).respond_with(|_| {
        FakeSession::structured(json!({
            "verdict": "approve", "summary": "fine", "comments": [], "tamper_suspected": false
        }))
    })
}

fn provider_of(name: &str) -> String {
    let model = name.split_once('/').map_or("", |(_, m)| m);
    match model.split('-').next().unwrap_or("unknown") {
        "claude" => "anthropic",
        "gpt" => "openai",
        "gemini" => "google",
        other => other,
    }
    .to_owned()
}

fn candidate(name: &str) -> Candidate {
    Candidate {
        worker: worker(name),
        plan_usage_left: None,
        plan_usage_text: String::new(),
        track_record: None,
        cost: CostEstimate::Unknown,
        needs_confirm: false,
        may_run_out: false,
    }
}

fn task_events(events: &[RunEvent], task: &str) -> Vec<TaskEvent> {
    events
        .iter()
        .filter(|e| e.task_id.as_ref().is_some_and(|c| c.as_str() == task))
        .filter_map(|e| match &e.kind {
            RunEventKind::Task(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

fn states(events: &[RunEvent], task: &str) -> Vec<TaskState> {
    task_events(events, task)
        .into_iter()
        .filter_map(|e| match e {
            TaskEvent::State { to, .. } => Some(to),
            _ => None,
        })
        .collect()
}

fn tries(events: &[RunEvent], task: &str) -> Vec<(TryCause, ModelRef)> {
    task_events(events, task)
        .into_iter()
        .filter_map(|e| match e {
            TaskEvent::Try { cause, worker, .. } => Some((cause, worker)),
            _ => None,
        })
        .collect()
}

fn worktrees(events: &[RunEvent]) -> BTreeMap<TaskId, (PathBuf, String)> {
    events
        .iter()
        .filter_map(|e| match (&e.task_id, &e.kind) {
            (Some(task), RunEventKind::Task(TaskEvent::Worktree { path, branch, .. })) => {
                Some((task.clone(), (path.clone(), branch.clone())))
            }
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent]) -> Vec<(ApprovalId, ApprovalKind)> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(o)) => Some((o.id.clone(), o.kind)),
            _ => None,
        })
        .collect()
}

/// The approvals opened after the plan's, which every seeded run starts with.
fn asked_while_building(events: &[RunEvent]) -> Vec<ApprovalKind> {
    opened(events)
        .into_iter()
        .map(|(_, kind)| kind)
        .filter(|kind| *kind != ApprovalKind::Plan)
        .collect()
}

fn state_of(bench: &Bench, task: &str) -> TaskState {
    let id = TaskId::new(task).unwrap();
    bench
        .host
        .read(|state| state.tasks.get(&id).map(|r| r.state))
        .unwrap_or(TaskState::Waiting)
}
