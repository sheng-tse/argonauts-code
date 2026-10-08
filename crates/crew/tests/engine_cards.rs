//! The card pipeline: parallel worktrees, gates before review, the gate and retry ladders,
//! escalation answers, card session lifetimes, and the status part for workers without a
//! status tool. Each scenario seeds an approved goal and its cards, then runs the `build` step.
mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};

use argonauts_core::api::CheckpointAnswer;
use argonauts_core::events::card::{
    AttemptCause, CardBudget, CardEvent, CardKind, CardSpec, CardState, GateKind,
};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, PlanPacket,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{BudgetPlan, TaskGraph};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::{EndReason, InputPartKind, SessionEvent};
use argonauts_core::events::vendor::{BudgetEvent, RunBudget};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::{Candidate, CostEstimate, RecoveryAction};
use argonauts_core::names::{CheckpointId, WorkerRef, WorkflowId};
use argonauts_core::policy::EscalationFacts;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit, OnHit};
use argonauts_core::settings::prompts::{RolePrompts, Writer};
use argonauts_core::settings::protocol::GateCommand;
use argonauts_core::steps::StepName;
use argonauts_core::worker::{SessionSpec, Worker, WorkerCapabilities};
use argonauts_core::workflow::{StandardPhase, StepRef};
use argonauts_core::{BudgetDimension, CardId, NoteLevel, RunId, Spend};
use argonauts_crew::card;
use argonauts_crew::card::gates::{self, NO_COMMAND_GATES};
use argonauts_crew::checkpoints;
use argonauts_crew::engine::host::Host;
use argonauts_crew::engine::shared::Shared;
use argonauts_crew::git::Git;
use argonauts_crew::state::RunState;
use argonauts_crew::workflow::{StepAt, StepCall, StepDone};
use argonauts_testkit::services::MemorySink;
use argonauts_testkit::worker::{FakeSession, FakeWorker};

use support::Crew;

const OWNER_A: &str = "claude-code/claude-x";
const OWNER_B: &str = "codex/gpt-x";
const REVIEWER: &str = "agy/gemini-x";

#[tokio::test(flavor = "multi_thread")]
async fn cards_run_in_parallel_worktrees() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let slow = Duration::from_millis(200);
    let a = crew.services.workers.add(slow_owner(
        OWNER_A,
        &[("src/a.rs", "pub fn a() {}\n")],
        slow,
    ));
    let b = crew.services.workers.add(slow_owner(
        OWNER_B,
        &[("src/b.rs", "pub fn b() {}\n")],
        slow,
    ));
    crew.services.workers.add(reviewer(REVIEWER));
    let cards = vec![
        spec("task-001", &[], OWNER_A, REVIEWER),
        spec("task-002", &[], OWNER_B, REVIEWER),
    ];
    // Your own words for the workers: a file in your checkout, and a sentence of the request.
    crew.repo
        .write(".argonauts/roles/workers.md", "Keep functions short.\n")?;
    crew.settings.prompts.roles.insert(
        "workers".to_owned(),
        RolePrompts {
            request: Some("focus on date parsing".to_owned()),
            ..RolePrompts::default()
        },
    );
    let bench = Bench::new(&crew, RunMode::Crew, &cards).await?;

    let done = bench.build().await?;
    assert_eq!(done.summary.as_deref(), Some("2 tasks merged into the run"));
    let events = bench.sink.events();

    // Both owner sessions started before either ended.
    let owner_sessions: Vec<(u64, bool)> = events
        .iter()
        .filter(|e| e.card_id.is_some())
        .filter_map(|e| match &e.kind {
            RunEventKind::Session(SessionEvent::Started(s)) if s.step == Some(StepName::Build) => {
                Some((e.seq, true))
            }
            RunEventKind::Session(SessionEvent::Ended(_)) => Some((e.seq, false)),
            _ => None,
        })
        .collect();
    let starts: Vec<u64> = owner_sessions
        .iter()
        .filter(|(_, s)| *s)
        .map(|(q, _)| *q)
        .collect();
    assert_eq!(starts.len(), 2, "{owner_sessions:?}");
    let first_end = owner_sessions
        .iter()
        .find(|(_, s)| !*s)
        .map(|(q, _)| *q)
        .unwrap();
    assert!(starts.iter().all(|s| *s < first_end), "{owner_sessions:?}");

    // Each card in its own worktree on its own branch.
    let trees = worktrees(&events);
    assert_eq!(trees.len(), 2);
    let run = RunId::new("run-01")?;
    for (id, worker) in [("task-001", &a), ("task-002", &b)] {
        let (path, branch) = &trees[&CardId::new(id)?];
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
            argonauts_core::prompt_parts::heading(&prompt[1]).as_deref(),
            Some("From the orchestrator · Claude Code")
        );
        assert_eq!(prompt[1].text.trim_end(), format!("Do {id}."));
        // Then your words, from the file that stands to this one request.
        let headings: Vec<String> = prompt
            .iter()
            .filter_map(argonauts_core::prompt_parts::heading)
            .collect();
        assert_eq!(
            headings[2..],
            ["Your file · roles/workers.md", "From your request"],
            "{headings:?}"
        );
        assert_eq!(prompt[2].text.trim_end(), "Keep functions short.");
        assert_eq!(prompt[3].text.trim_end(), "focus on date parsing");
        // The word on questions follows the instructions while the run decides without you.
        assert_eq!(prompt[4].label, "decide, don't ask");
        let task = &prompt[5];
        assert_eq!(task.kind, InputPartKind::Card);
        assert!(task.text.starts_with(&format!("{id} · Build {id}")));
        assert!(!task.text.contains(&format!("Do {id}.")), "{}", task.text);
    }
    assert_ne!(
        trees[&CardId::new("task-001")?].0,
        trees[&CardId::new("task-002")?].0
    );
    assert!(state_of(&bench, "task-001") == CardState::Integrated);
    assert!(state_of(&bench, "task-002") == CardState::Integrated);
    Ok(())
}

/// An agent that commits part of its work keeps its commit, the conductor commits only what it
/// left, and git notes name the agent as the author of both.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_own_commits_are_kept_and_noted() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(
        FakeWorker::harness(OWNER_B, &family_of(OWNER_B)).respond_with(|spec| {
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
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;

    let commits: Vec<_> = card_events(&bench.sink.events(), "task-001")
        .into_iter()
        .filter_map(|event| match event {
            CardEvent::Committed(info) if info.sha.is_some() => Some(info),
            _ => None,
        })
        .collect();
    let [own, left] = commits.as_slice() else {
        bail!("expected the agent's commit and the conductor's, got {commits:?}");
    };
    assert_eq!(own.message.trim(), "Add a");
    assert!(
        left.message.starts_with("task-001: work from"),
        "{}",
        left.message
    );
    for commit in [own, left] {
        assert_eq!(commit.by, Some(worker(OWNER_B)));
        let sha = commit.sha.as_deref().context("no sha")?;
        let note = crew
            .repo
            .git(&["notes", "--ref=refs/notes/argonauts", "show", sha])?;
        assert_eq!(note.trim(), format!("author: {OWNER_B}"));
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
            FakeWorker::harness(OWNER_B, &family_of(OWNER_B)).respond_with(|spec| {
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
            &[spec("task-001", &[], OWNER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;

        let events = bench.sink.events();
        let sha = card_events(&events, "task-001")
            .into_iter()
            .find_map(|event| match event {
                CardEvent::Committed(info) => info.sha,
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
        FakeWorker::harness(OWNER_B, &family_of(OWNER_B)).respond_with(|spec| {
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
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;

    let events = bench.sink.events();
    let sha = card_events(&events, "task-001")
        .into_iter()
        .find_map(|event| match event {
            CardEvent::Committed(info) => info.sha,
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
/// `git add -A`, keeps its commit, and the conductor's commit takes the leftovers back out of
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
            FakeWorker::harness(OWNER_B, &family_of(OWNER_B)).respond_with(|spec| {
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
            &[spec("task-001", &[], OWNER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;

        let events = bench.sink.events();
        let (worktree, branch) = worktrees(&events)
            .remove(&CardId::new("task-001")?)
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
async fn gates_before_review() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_gate()];
    let owner = crew
        .services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    let reviewer = crew.services.workers.add(reviewer(REVIEWER));
    // The first gate run fails, the resumed session fixes it.
    crew.services.gates.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let events = bench.sink.events();
    let gate_seqs: Vec<u64> = events
        .iter()
        .filter(|e| matches!(&e.kind, RunEventKind::Gate(g) if g.kind == GateKind::Command))
        .map(|e| e.seq)
        .collect();
    let first_review = events
        .iter()
        .find(|e| matches!(e.kind, RunEventKind::Review(_)))
        .map(|e| e.seq)
        .context("no review recorded")?;
    assert_eq!(gate_seqs.len(), 2, "one failed run and one passing run");
    assert!(gate_seqs.iter().all(|g| *g < first_review));
    assert_eq!(
        owner.specs().len(),
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
            CardState::Waiting,
            CardState::Ready,
            CardState::Running,
            CardState::Gating,
            CardState::Running,
            CardState::Gating,
            CardState::InReview,
            CardState::Integrating,
            CardState::Integrated,
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn no_command_gates_blocks_auto_merge() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    assert!(crew.settings.checks.run.is_empty());
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let record = bench
        .host
        .read(|state| state.cards.get(&CardId::new("task-001").unwrap()).cloned())
        .context("no task")?;
    let marker = record
        .gates
        .iter()
        .find(|g| gates::is_no_command_gates(g))
        .context("no checks cell note")?;
    assert_eq!(marker.summary, NO_COMMAND_GATES);
    assert!(
        marker.skipped,
        "an empty checks.run is recorded as skipped, not as a failure"
    );
    // Merge rules that need the suite see it as not run.
    assert_eq!(gates::gates_passed(&record.gates), None);
    // The built-in checks still ran on the agent's commits.
    for kind in [
        GateKind::ProtectedPaths,
        GateKind::SecretScan,
        GateKind::TestTamper,
    ] {
        assert!(
            record.gates.iter().any(|g| g.kind == kind && g.passed),
            "{kind:?}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_done_report_needs_its_changes_on_the_branch() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    let resumed_with = Arc::new(std::sync::Mutex::new(String::new()));
    let seen = resumed_with.clone();
    // The first session claims a change it never made. Told so, its resume makes it.
    crew.services.workers.add(
        FakeWorker::harness(OWNER_B, &family_of(OWNER_B)).respond_with(move |spec| {
            if spec.resume.is_some() {
                *seen.lock().unwrap() = format!("{:?}", spec.prompt);
                return work(spec, &[("src/a.rs", "fn a() {}\n")], Duration::ZERO);
            }
            let mut claimed = report("done");
            claimed["files_changed"] = json!(["src/a.rs"]);
            FakeSession::structured(claimed).with_vendor_session(&format!("v-{}", spec.key))
        }),
    );
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let events = bench.sink.events();
    assert_eq!(
        attempts(&events, "task-001"),
        [
            (AttemptCause::First, worker(OWNER_B)),
            (AttemptCause::GateRetry, worker(OWNER_B)),
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
        .read(|state| state.cards.get(&CardId::new("task-001").unwrap()).cloned())
        .context("no task")?;
    let checks: Vec<bool> = record
        .gates
        .iter()
        .filter(|g| g.kind == GateKind::ReportedChanges)
        .map(|g| g.passed)
        .collect();
    assert_eq!(checks, [false], "the resume's changes need no check");
    assert_eq!(state_of(&bench, "task-001"), CardState::Integrated);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_that_changed_nothing_says_so() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(owner(OWNER_B, &[]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let record = bench
        .host
        .read(|state| state.cards.get(&CardId::new("task-001").unwrap()).cloned())
        .context("no task")?;
    let check = record
        .gates
        .iter()
        .find(|g| g.kind == GateKind::ReportedChanges)
        .context("no check of the empty branch")?;
    assert!(check.passed);
    assert_eq!(
        check.summary,
        "no changes on the branch, as the report says"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_ladder_same_then_other_then_escalate() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_gate()];
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services
        .workers
        .add(owner(OWNER_A, &[("src/a.rs", "fn a() { 1 }\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .handoff
        .set_candidates(vec![candidate(OWNER_A)]);
    for _ in 0..4 {
        crew.services.gates.fail_next("still failing");
    }
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    // No limit is set, so one more attempt always fits.
    let budgets_off = RunBudget {
        spent: Spend::default(),
        limit: None,
        lines: Vec::new(),
        reserve_left: None,
    };
    bench.host.record(
        None,
        None,
        RunEventKind::Budget(BudgetEvent::Run(budgets_off)),
    )?;

    let build = bench.spawn_build();
    let checkpoint = bench
        .answer(CheckpointKind::Escalation, AnswerAction::Cancel, None)
        .await?;
    let done = finish(build).await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("0 tasks merged into the run, 1 cancelled")
    );

    let events = bench.sink.events();
    let attempts = attempts(&events, "task-001");
    assert_eq!(
        attempts,
        [
            (AttemptCause::First, worker(OWNER_B)),
            (AttemptCause::GateRetry, worker(OWNER_B)),
            (AttemptCause::AfterFailure, worker(OWNER_A)),
            (AttemptCause::GateRetry, worker(OWNER_A)),
        ]
    );
    let reassigned: Vec<_> = card_events(&events, "task-001")
        .into_iter()
        .filter_map(|e| match e {
            CardEvent::Reassigned { to, by, .. } => Some((to, by)),
            _ => None,
        })
        .collect();
    assert_eq!(reassigned, [(worker(OWNER_A), Party::Conductor)]);
    let states = states(&events, "task-001");
    let failed = states.iter().filter(|s| **s == CardState::Failed).count();
    assert_eq!(failed, 2, "{states:?}");
    assert_eq!(states[states.len() - 2], CardState::Escalated);
    assert_eq!(states.last(), Some(&CardState::Cancelled));
    assert!(opened(&events).iter().any(|(id, _)| *id == checkpoint.0));

    // The policy saw what it needs to grant one more attempt: both families tried, no
    // fallback of a third family left, and none granted yet.
    let evaluated = crew.services.policy.evaluated();
    let (_, evidence) = evaluated
        .iter()
        .find(|(point, _)| point.checkpoint == checkpoint.0)
        .context("the policy never saw the escalation")?;
    let facts = evidence
        .facts
        .escalation
        .clone()
        .context("no escalation facts")?;
    assert_eq!(
        facts,
        EscalationFacts {
            handoff_mode: crew.settings.fallback.mode,
            families_tried: vec!["claude".to_owned(), "gpt".to_owned()],
            next_family: None,
            extra_attempts: 0,
            fits_reserve: true,
        }
    );
    Ok(())
}

/// A task that reaches a limit you set goes to a fallback, by rule and with no question.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_at_its_limit_goes_to_a_fallback() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(at_its_limit(OWNER_B));
    crew.services
        .workers
        .add(owner(OWNER_A, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .handoff
        .set_candidates(vec![candidate(OWNER_A)]);
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let events = bench.sink.events();
    let workers: Vec<WorkerRef> = attempts(&events, "task-001")
        .into_iter()
        .map(|(_, worker)| worker)
        .collect();
    assert_eq!(workers, [worker(OWNER_B), worker(OWNER_A)]);
    let reassigned: Vec<_> = card_events(&events, "task-001")
        .into_iter()
        .filter_map(|e| match e {
            CardEvent::Reassigned { to, by, .. } => Some((to, by)),
            _ => None,
        })
        .collect();
    assert_eq!(reassigned, [(worker(OWNER_A), Party::Rules)]);
    assert_eq!(asked_while_building(&events), [], "nothing was asked");
    assert_eq!(state_of(&bench, "task-001"), CardState::Integrated);
    Ok(())
}

/// With `limits.when_reached = "stop"` the task stops and needs you: nothing takes it over,
/// and no approval about the limit opens.
#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_a_task_at_its_limit() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.services.workers.add(at_its_limit(OWNER_B));
    crew.services
        .workers
        .add(owner(OWNER_A, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services
        .handoff
        .set_candidates(vec![candidate(OWNER_A)]);
    crew.services
        .limits
        .set_next(RecoveryAction::BudgetHit(OnHit::Stop));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    let build = bench.spawn_build();
    bench
        .answer(CheckpointKind::Escalation, AnswerAction::Cancel, None)
        .await?;
    finish(build).await?;
    let events = bench.sink.events();
    let workers: Vec<WorkerRef> = attempts(&events, "task-001")
        .into_iter()
        .map(|(_, worker)| worker)
        .collect();
    assert_eq!(workers, [worker(OWNER_B)]);
    assert_eq!(asked_while_building(&events), [CheckpointKind::Escalation]);
    let states = states(&events, "task-001");
    assert_eq!(states[states.len() - 2], CardState::Escalated);
    assert_eq!(states.last(), Some(&CardState::Cancelled));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn escalation_defer_and_split() -> anyhow::Result<()> {
    // Deferred: the card waits and the run goes on without it, and without the card that
    // depends on it.
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_gate()];
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.gates.fail_next("1 failed");
    crew.services.gates.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[
            spec("task-001", &[], OWNER_B, REVIEWER),
            spec("task-002", &["task-001"], OWNER_B, REVIEWER),
        ],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(CheckpointKind::Escalation, AnswerAction::Defer, None)
        .await?;
    let done = finish(build).await?;
    assert_eq!(
        done.summary.as_deref(),
        Some("0 tasks merged into the run, 1 deferred, 1 waiting on deferred tasks")
    );
    assert_eq!(state_of(&bench, "task-001"), CardState::Deferred);
    assert_eq!(state_of(&bench, "task-002"), CardState::Waiting);

    // Split: your note becomes new cards through PlanToCards and the graph validator.
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_gate()];
    // The build's phase lead owns its critical-path card, so this card's owner splits it.
    let proposal = json!({
        "cards": [
            {"key": "store", "title": "Store", "task": "The store.", "kind": "build",
             "owned_paths": ["src/store/**"], "read_only_paths": [], "depends_on": [],
             "criteria": ["G1"]},
            {"key": "api", "title": "API", "task": "The API.", "kind": "build",
             "owned_paths": ["src/api/**"], "read_only_paths": [], "depends_on": ["store"],
             "criteria": ["G1"]}
        ]
    });
    let splitter = FakeWorker::harness(OWNER_B, "gpt").respond_with(move |spec| {
        if spec.step == Some(StepName::PlanToCards) {
            return FakeSession::structured(proposal.clone());
        }
        work(spec, &[("src/part.rs", "fn part() {}\n")], Duration::ZERO)
    });
    crew.services.workers.add(splitter);
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.gates.fail_next("1 failed");
    crew.services.gates.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(
            CheckpointKind::Escalation,
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
    let last = card_events(&events, "task-001")
        .into_iter()
        .rev()
        .find_map(|e| match e {
            CardEvent::State { to, reason, .. } => Some((to, reason)),
            _ => None,
        })
        .context("no state")?;
    assert_eq!(last.0, CardState::Cancelled);
    assert_eq!(last.1, "split into task-002, task-003");
    let created: Vec<CardSpec> = events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Card(CardEvent::Created(spec)) => Some((**spec).clone()),
            _ => None,
        })
        .filter(|spec| spec.id.as_str() != "task-001")
        .collect();
    let ids: Vec<&str> = created.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["task-002", "task-003"]);
    assert_eq!(created[1].depends_on, [CardId::new("task-002")?]);
    assert_eq!(created[1].owner, Some(worker(OWNER_B)));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lifetime_card_resumes_on_retry() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.checks.run = vec![test_gate()];
    let owner = crew
        .services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    crew.services.gates.fail_next("1 failed");
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;

    bench.build().await?;
    let specs = owner.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].resume, None, "the task's first session is fresh");
    assert_eq!(
        specs[1].resume.as_deref(),
        Some(format!("v-{}", specs[0].key).as_str()),
        "the checks retry resumes the task's own session"
    );
    let kinds: Vec<InputPartKind> = specs[1].prompt.iter().map(|p| p.kind).collect();
    assert!(kinds.contains(&InputPartKind::GateOutput), "{kinds:?}");
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
    let owner = crew.services.workers.add(asks_before_it_works(OWNER_B));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;
    assert_eq!(state_of(&bench, "task-001"), CardState::Integrated);

    let specs = owner.specs();
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
    let always_asks = FakeWorker::harness(OWNER_B, "gpt").respond_with(|spec| {
        FakeSession::completed("Which do you prefer, A or B?")
            .with_vendor_session(&format!("v-{}", spec.key))
    });
    let owner = crew.services.workers.add(always_asks);
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(CheckpointKind::Escalation, AnswerAction::Cancel, None)
        .await?;
    finish(build).await?;

    let replies: Vec<String> = owner
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
        &mut approvals.goal,
        &mut approvals.plan,
        &mut approvals.merge,
        &mut approvals.escalation,
        &mut approvals.budget,
        &mut approvals.change_request,
    ] {
        point.mode = ApprovalMode::Supervised;
    }
    let owner = crew.services.workers.add(asks_before_it_works(OWNER_B));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    let build = bench.spawn_build();
    bench
        .answer(CheckpointKind::Escalation, AnswerAction::Cancel, None)
        .await?;
    finish(build).await?;

    let specs = owner.specs();
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
        let builds = crew.services.workers.add(owner(OWNER_B, FILES));
        let reviews = crew.services.workers.add(reviewer(REVIEWER));
        let bench = Bench::new(
            &crew,
            RunMode::Crew,
            &[spec("task-001", &[], OWNER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;
        assert_eq!(state_of(&bench, "task-001"), CardState::Integrated);

        let specs = [builds.specs(), reviews.specs()].concat();
        assert!(specs.len() >= 2, "a build and its review");
        assert!(
            specs.iter().all(|spec| spec.autopilot == autopilot),
            "autopilot {autopilot}"
        );
    }
    Ok(())
}

/// An owner whose first turn ends with a question, and who does the work once it is resumed,
/// saying in its report what it assumed.
fn asks_before_it_works(name: &str) -> FakeWorker {
    const FILES: &[(&str, &str)] = &[("src/a.rs", "fn a() {}\n")];
    FakeWorker::harness(name, &family_of(name)).respond_with(|spec| {
        if spec.resume.is_some() {
            let _written = work(spec, FILES, Duration::ZERO);
            let mut done = report("done");
            done["concerns"] = json!(["Assumed: the simpler of the two ways."]);
            return FakeSession::structured(done).with_vendor_session(&format!("v-{}", spec.key));
        }
        FakeSession::completed("I found two ways to do it.\n\nShould I proceed?")
            .with_vendor_session(&format!("v-{}", spec.key))
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
        crew.settings.checks.run = vec![test_gate()];
        let capabilities = WorkerCapabilities {
            status_tool,
            ..FakeWorker::harness(OWNER_B, "gpt").capabilities()
        };
        let owner = crew
            .services
            .workers
            .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]).with_capabilities(capabilities));
        crew.services.workers.add(reviewer(REVIEWER));
        crew.services.gates.fail_next("1 failed");
        let bench = Bench::new(
            &crew,
            RunMode::Crew,
            &[spec("task-001", &[], OWNER_B, REVIEWER)],
        )
        .await?;
        bench.build().await?;

        let specs = owner.specs();
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
        "reviewers".to_owned(),
        RolePrompts {
            writer: Some(Writer::Model(worker(WRITER))),
            ..RolePrompts::default()
        },
    );
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    let reviewer = crew.services.workers.add(reviewer(REVIEWER));
    let writer = crew
        .services
        .workers
        .add(FakeWorker::harness(WRITER, "writer").respond_with(|_| {
            FakeSession::structured(json!({ "instructions": "Review it for races." }))
        }));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
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
        input.text.contains("\"role\": \"reviewers\""),
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
        .filter_map(argonauts_core::prompt_parts::heading)
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

/// A workers file that says `mode: replace` takes the place of the orchestrator's brief as
/// the worker's instructions. The brief still says what the task is, so it stays in the task
/// the worker is handed.
#[tokio::test]
async fn a_workers_file_that_replaces_keeps_the_task_readable() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.repo.write(
        ".argonauts/roles/workers.md",
        "---\nmode: replace\n---\nBuild it test first.\n",
    )?;
    let worker = crew
        .services
        .workers
        .add(owner(OWNER_B, &[("src/a.rs", "fn a() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[spec("task-001", &[], OWNER_B, REVIEWER)],
    )
    .await?;
    bench.build().await?;

    let prompt = &worker.specs()[0].prompt;
    let headings: Vec<String> = prompt
        .iter()
        .filter_map(argonauts_core::prompt_parts::heading)
        .collect();
    assert_eq!(
        headings,
        ["Frame · built in", "Your file · roles/workers.md"],
        "{headings:?}"
    );
    assert_eq!(prompt[1].text.trim_end(), "Build it test first.");
    assert_eq!(prompt[2].label, "decide, don't ask");
    let task = &prompt[3];
    assert_eq!(task.kind, InputPartKind::Card);
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
        .add(owner(OWNER_A, &[("src/a.rs", "fn a() {}\n")]));
    let second = crew
        .services
        .workers
        .add(owner(OWNER_B, &[("src/b.rs", "fn b() {}\n")]));
    crew.services.workers.add(reviewer(REVIEWER));
    let bench = Bench::new(
        &crew,
        RunMode::Crew,
        &[
            spec("task-001", &[], OWNER_A, REVIEWER),
            spec("task-002", &[], OWNER_B, REVIEWER),
        ],
    )
    .await?;
    let as_written = CallAuthor::Workflow {
        name: "spec-first",
        step: Some("build"),
    };
    brief_written_by(&bench.host, &CardId::new("task-001")?, as_written)?;
    // Said twice, as a run that picks up again says it, and said of a task that isn't one.
    brief_written_by(&bench.host, &CardId::new("task-001")?, as_written)?;
    brief_written_by(
        &bench.host,
        &CardId::new("task-002")?,
        CallAuthor::Orchestrator,
    )?;
    bench.build().await?;

    let leads_with = |worker: &FakeWorker| {
        let prompt = &worker.specs()[0].prompt;
        let heading = argonauts_core::prompt_parts::heading(&prompt[1]);
        (
            heading.unwrap_or_default(),
            prompt[1].text.trim_end().to_owned(),
        )
    };
    assert_eq!(
        leads_with(&first),
        (
            "From the workflow · spec-first, step build".to_owned(),
            "Do task-001.".to_owned()
        )
    );
    assert_eq!(
        leads_with(&second),
        (
            "From the orchestrator · Claude Code".to_owned(),
            "Do task-002.".to_owned()
        )
    );
    Ok(())
}

/// A reviewers file that names a CLI on the crew tells the reviewers who made the work. The
/// run says so once, however many reviewers read the file, and they read it all the same.
#[tokio::test(flavor = "multi_thread")]
async fn a_reviewers_file_that_names_a_cli_is_warned_about_once() -> anyhow::Result<()> {
    let crew = Crew::new()?;
    crew.repo.write(
        ".argonauts/roles/reviewers.md",
        "Go easy on Codex's code.\n",
    )?;
    crew.services
        .workers
        .add(owner(OWNER_A, &[("src/a.rs", "fn a() {}\n")]));
    crew.services
        .workers
        .add(owner(OWNER_B, &[("src/b.rs", "fn b() {}\n")]));
    let reviewer = crew.services.workers.add(reviewer(REVIEWER));
    let cards = [
        spec("task-001", &[], OWNER_A, REVIEWER),
        spec("task-002", &[], OWNER_B, REVIEWER),
    ];
    let bench = Bench::new(&crew, RunMode::Crew, &cards).await?;
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
            "roles/reviewers.md names Codex, which tells the reviewers who made the work. They \
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
    /// A run with an approved goal and `cards` created, as `divide` leaves it.
    async fn new(crew: &Crew, mode: RunMode, cards: &[CardSpec]) -> anyhow::Result<Self> {
        let base = crew.repo.head()?;
        let history = history(crew, mode, &base, cards)?;
        let (context, sink, _commands) = crew.context(history)?;
        Git::new()
            .add_detached_worktree(crew.repo.root(), &context.repo.base_worktree, &base)
            .await?;
        let state = RunState::fold(&context.history);
        let shared = Shared::new(&context, state)?;
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
        tokio::spawn(async move { card::build_step(&host, &build_call()).await })
    }

    /// Answers the newest open checkpoint of `kind` once it opens.
    async fn answer(
        &self,
        kind: CheckpointKind,
        action: AnswerAction,
        text: Option<&str>,
    ) -> anyhow::Result<(CheckpointId, String)> {
        for _ in 0..600 {
            let events = self.sink.events();
            let decided: Vec<_> = events
                .iter()
                .filter_map(|e| match &e.kind {
                    RunEventKind::Checkpoint(CheckpointEvent::Decided(d)) => Some(d.id.clone()),
                    _ => None,
                })
                .collect();
            let open = events.iter().rev().find_map(|e| match &e.kind {
                RunEventKind::Checkpoint(CheckpointEvent::Opened(o))
                    if o.kind == kind && !decided.contains(&o.id) =>
                {
                    Some((o.id.clone(), o.evidence_sha256.clone(), o.title.clone()))
                }
                _ => None,
            });
            if let Some((id, evidence, title)) = open {
                let answer = CheckpointAnswer {
                    id: id.clone(),
                    action: action.clone(),
                    text: text.map(str::to_owned),
                    evidence_sha256: evidence,
                    cards: Vec::new(),
                };
                checkpoints::answer(&self.host, answer).map_err(anyhow::Error::msg)?;
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
    let joined = tokio::time::timeout(Duration::from_secs(60), build)
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
    cards: &[CardSpec],
) -> anyhow::Result<Vec<RunEvent>> {
    let run = RunId::new("run-01")?;
    let started = RunStarted {
        request: "Build it".into(),
        title: "Build it".into(),
        mode,
        forced: true,
        workflow: "default".into(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: crew.repo.root().to_path_buf(),
        base_branch: Some("main".into()),
        base_commit: base.to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::to_value(&crew.settings)?,
        overrides: Vec::new(),
        harness_versions: BTreeMap::new(),
        version: "0.0.1".into(),
        front_end: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
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
        source: GoalSource::Restatements,
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
    kinds.extend(approved_plan(cards));
    for spec in cards {
        let id = Some(spec.id.clone());
        kinds.push((
            id.clone(),
            RunEventKind::Card(CardEvent::Created(Box::new(spec.clone()))),
        ));
        kinds.push((
            id,
            RunEventKind::Card(CardEvent::State {
                from: None,
                to: CardState::Waiting,
                reason: "created".into(),
            }),
        ));
    }
    Ok(kinds
        .into_iter()
        .enumerate()
        .map(|(seq, (card_id, kind))| RunEvent {
            seq: seq as u64,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: run.clone(),
            card_id,
            session: None,
            source: "conductor".into(),
            kind,
        })
        .collect())
}

/// The plan checkpoint, opened and approved, which crew build sessions wait for.
fn approved_plan(cards: &[CardSpec]) -> Vec<(Option<CardId>, RunEventKind)> {
    let id = CheckpointId::numbered(1);
    let evidence = "0".repeat(64);
    let packet = PlanPacket {
        summary: "The plan".into(),
        judging: None,
        near_tie: false,
        decisions: Vec::new(),
        graph: TaskGraph {
            cards: cards.to_vec(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        budget: BudgetPlan {
            lines: Vec::new(),
            total: BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: 10.0,
            },
            estimate_low: 0.0,
            estimate_high: 0.0,
        },
        new_dependencies: Vec::new(),
        flags: Vec::new(),
        workflow: None,
    };
    let opened = CheckpointOpened {
        id: id.clone(),
        kind: CheckpointKind::Plan,
        title: "Approve this plan?".into(),
        card: None,
        packet: CheckpointPacket::Plan(Box::new(packet)),
        evidence_sha256: evidence.clone(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    let decided = CheckpointDecided {
        id,
        action: AnswerAction::Approve,
        text: None,
        by: Party::User,
        evidence_sha256: evidence,
        shadow: None,
        cards: Vec::new(),
    };
    vec![
        (
            None,
            RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened))),
        ),
        (
            None,
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)),
        ),
    ]
}

fn spec(id: &str, deps: &[&str], owner: &str, reviewer: &str) -> CardSpec {
    CardSpec {
        id: CardId::new(id).unwrap(),
        title: format!("Build {id}"),
        task: format!("Do {id}."),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: deps.iter().map(|d| CardId::new(*d).unwrap()).collect(),
        criteria: vec!["G1".into()],
        owner: Some(worker(owner)),
        reviewers: vec![worker(reviewer)],
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    }
}

fn worker(name: &str) -> WorkerRef {
    name.parse().unwrap()
}

fn test_gate() -> GateCommand {
    GateCommand {
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

/// A harness owner that writes `files` into its worktree and reports done. Its vendor session
/// id is `v-<session key>`, so resumes can be traced.
fn owner(name: &str, files: &'static [(&'static str, &'static str)]) -> FakeWorker {
    slow_owner(name, files, Duration::ZERO)
}

/// An owner whose sessions take `delay` per event, so two can be seen running at once.
fn slow_owner(
    name: &str,
    files: &'static [(&'static str, &'static str)],
    delay: Duration,
) -> FakeWorker {
    FakeWorker::harness(name, &family_of(name)).respond_with(move |spec| work(spec, files, delay))
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
        .with_vendor_session(&format!("v-{}", spec.key))
        .with_delay(delay)
}

/// An owner whose every session ends at a limit you set.
fn at_its_limit(name: &str) -> FakeWorker {
    FakeWorker::harness(name, &family_of(name)).respond_with(|_| {
        FakeSession::ended(EndReason::BudgetHit {
            dimension: BudgetDimension::Spend,
        })
    })
}

fn reviewer(name: &str) -> FakeWorker {
    FakeWorker::harness(name, &family_of(name)).respond_with(|_| {
        FakeSession::structured(json!({
            "verdict": "approve", "summary": "fine", "comments": [], "tamper_suspected": false
        }))
    })
}

fn family_of(name: &str) -> String {
    let model = name.split_once('/').map_or("", |(_, m)| m);
    model.split('-').next().unwrap_or("unknown").to_owned()
}

fn candidate(name: &str) -> Candidate {
    Candidate {
        worker: worker(name),
        quota_left: None,
        quota_text: String::new(),
        track_record: None,
        cost: CostEstimate::Unknown,
        needs_confirm: false,
        may_run_out: false,
    }
}

fn card_events(events: &[RunEvent], card: &str) -> Vec<CardEvent> {
    events
        .iter()
        .filter(|e| e.card_id.as_ref().is_some_and(|c| c.as_str() == card))
        .filter_map(|e| match &e.kind {
            RunEventKind::Card(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

fn states(events: &[RunEvent], card: &str) -> Vec<CardState> {
    card_events(events, card)
        .into_iter()
        .filter_map(|e| match e {
            CardEvent::State { to, .. } => Some(to),
            _ => None,
        })
        .collect()
}

fn attempts(events: &[RunEvent], card: &str) -> Vec<(AttemptCause, WorkerRef)> {
    card_events(events, card)
        .into_iter()
        .filter_map(|e| match e {
            CardEvent::Attempt { cause, worker, .. } => Some((cause, worker)),
            _ => None,
        })
        .collect()
}

fn worktrees(events: &[RunEvent]) -> BTreeMap<CardId, (PathBuf, String)> {
    events
        .iter()
        .filter_map(|e| match (&e.card_id, &e.kind) {
            (Some(card), RunEventKind::Card(CardEvent::Worktree { path, branch, .. })) => {
                Some((card.clone(), (path.clone(), branch.clone())))
            }
            _ => None,
        })
        .collect()
}

fn opened(events: &[RunEvent]) -> Vec<(CheckpointId, CheckpointKind)> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(o)) => Some((o.id.clone(), o.kind)),
            _ => None,
        })
        .collect()
}

/// The approvals opened after the plan's, which every seeded run starts with.
fn asked_while_building(events: &[RunEvent]) -> Vec<CheckpointKind> {
    opened(events)
        .into_iter()
        .map(|(_, kind)| kind)
        .filter(|kind| *kind != CheckpointKind::Plan)
        .collect()
}

fn state_of(bench: &Bench, card: &str) -> CardState {
    let id = CardId::new(card).unwrap();
    bench
        .host
        .read(|state| state.cards.get(&id).map(|r| r.state))
        .unwrap_or(CardState::Waiting)
}
