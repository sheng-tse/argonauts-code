//! A workflow's tasks done the way the approved plan says, which the engine runs on every CLI:
//! agents of the worker's model each take a part, or the worker goes through a workflow's
//! steps and a fresh session from another maker takes its checks and picks among its tries.
//! A script's writers run them, and so does the build step of a workflow's phases.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serde_json::json;

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::SessionSpec;
use argonauts_core::base::names::{ApprovalId, RoleName, WorkflowId};
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, PlanPacket,
};
use argonauts_core::runs::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::runs::events::plan::{DivideEvent, LimitPlan, TaskGraph};
use argonauts_core::runs::events::run::{Party, RunMode};
use argonauts_core::runs::events::session::SessionEvent;
use argonauts_core::runs::events::task::{
    MergeEvent, ReviewEvent, TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState, TaskWay, Way,
};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith, WhenDeclined};
use argonauts_core::settings::crew::{CrewSettings, Orchestrator};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::workflows::workflow::{StandardPhase, StepRef, WorkflowRef};
use argonauts_crew::council::ways;
use argonauts_crew::git::Git;
use argonauts_crew::task;
use argonauts_crew::workflow::{StepAt, StepCall, StepDone};
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};
use argonauts_testkit::pty::deadline;

use crate::support::{Crew, Outcome, recommended, smallest_answer};

const MAIN: &str = "claude-code/opus";
const WORKER: &str = "codex/gpt";
const REVIEWER: &str = "antigravity-cli/gemini";
const LOCAL: &str = "opencode/qwen";

/// A workflow whose first step is the orchestrator's, which plans a whole request.
const PLAN_FIRST: &str = r#"
name = "plan-first"

[[step]]
name = "Plan"
role = "orchestrator"

[[step]]
name = "Build"
role = "worker"
"#;

const STEPS: &str = r#"
name  = "ship"
about = "Plan, build each task, review it."
adapt = false

[[step]]
name = "Plan"
role = "orchestrator"
does = "Split the request into tasks: {request}"

[[step]]
name     = "Build"
role     = "worker"
for_each = "task"

[[step]]
name         = "Review"
role         = "reviewer"
for_each     = "task"
send_back_to = "Build"
"#;

fn prompt(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The orchestrator plans the one task `title`; the worker writes the file its part names, or
/// the task's own file, saying which worktree wrote it; the reviewer accepts everything, as the
/// script's review and as a check, and picks the second of any tries.
fn crew(title: &'static str) -> anyhow::Result<(Crew, Arc<FakeRunner>, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(move |_| {
        FakeSession::structured(json!({
            "summary": "One task",
            "tasks": [{"title": title, "done_when": ["it works"]}],
        }))
    });
    crew.services.workers.add(main);
    let worker = FakeRunner::cli(WORKER, "openai")
        .respond_with(|spec| checks(spec).unwrap_or_else(|| builds(spec)));
    let worker = crew.services.workers.add(worker);
    let reviewer = FakeRunner::cli(REVIEWER, "google")
        .respond_with(|spec| checks(spec).unwrap_or_else(|| smallest_answer(spec)));
    let reviewer = crew.services.workers.add(reviewer);
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![WORKER.parse()?],
        reviewer: vec![REVIEWER.parse()?],
        judge: vec![REVIEWER.parse()?],
        worker_workflows: false,
        merger: Vec::new(),
        orchestrator_context: Default::default(),
        orchestrator_picks: Default::default(),
        ways: Default::default(),
    };
    crew.settings.workflow = "ship".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    let folder = crew.repo.root().join(".argonauts/workflows");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("ship.toml"), STEPS)?;
    Ok((crew, worker, reviewer))
}

/// A worker's answer: it writes the file its part names, or the task's own file, saying which
/// worktree wrote it.
fn builds(spec: &SessionSpec) -> FakeSession {
    let asked = prompt(spec);
    let file = ["due.rs", "due_test.rs", "docs.md"]
        .into_iter()
        .find(|file| asked.contains(&format!("Your part is {file}")))
        .unwrap_or(if asked.contains("Due docs") {
            "docs.md"
        } else {
            "due.rs"
        });
    let tree = spec
        .workdir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    std::fs::write(spec.workdir.join(file), format!("built in {tree}\n")).expect("writes its file");
    FakeSession::structured(json!({
        "status": "done",
        "summary": format!("Wrote {file} in {tree}"),
        "files_changed": [file],
        "tests_run": [],
        "concerns": [],
        "questions": [],
        "change_requests": [],
        "memory_proposals": [],
        "answer": format!("Wrote {file}"),
    }))
}

/// A checker's answer: it picks the second of any tries and accepts everything. `None` for an
/// ask that checks nothing.
fn checks(spec: &SessionSpec) -> Option<FakeSession> {
    let takes = |field: &str| {
        spec.json_schema
            .as_ref()
            .and_then(|schema| schema.pointer(&format!("/properties/{field}")))
            .is_some()
    };
    if takes("pick") {
        Some(FakeSession::structured(
            json!({"pick": 2, "why": "It says when a task is due."}),
        ))
    } else if takes("accept") {
        Some(FakeSession::structured(
            json!({"accept": true, "findings": []}),
        ))
    } else if takes("verdict") {
        Some(FakeSession::structured(json!({
            "verdict": "approve",
            "summary": "It holds up.",
            "comments": [],
            "tamper_suspected": false,
        })))
    } else {
        None
    }
}

/// An approved plan's task, as the run's log holds it before the workflow runs.
fn planned(title: &str, owned: &[&str], how: Way) -> TaskSpec {
    TaskSpec {
        id: TaskId::numbered(1),
        title: title.to_owned(),
        task: format!("{title}."),
        kind: TaskKind::Build,
        owned_paths: owned.iter().map(|path| (*path).to_owned()).collect(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(WORKER.parse().expect("a worker")),
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: Some(TaskWay {
            how,
            asked_by_you: true,
            steps: None,
            why: None,
        }),
    }
}

/// Runs the request with `task` in the plan, answering each approval with its recommended
/// option. Returns every event of the run.
async fn run_with(crew: &Crew, task: TaskSpec) -> anyhow::Result<Vec<RunEvent>> {
    let started = crew.run_started(RunMode::Crew, "Add due dates to tasks")?;
    let graph = RunEvent {
        seq: 1,
        kind: RunEventKind::Divide(DivideEvent::Graph(TaskGraph {
            tasks: vec![task],
            critical_path: Vec::new(),
        })),
        ..started.clone()
    };
    let _env = crew.repo.env_guard();
    let mut last: Outcome = crew.resume(vec![started, graph], Vec::new()).await?;
    for _ in 0..3 {
        if !matches!(last.exit, EngineExit::Waiting(_)) {
            break;
        }
        let Some((command, _reply)) = recommended(&last.events) else {
            break;
        };
        last = crew.resume(last.events.clone(), vec![command]).await?;
    }
    Ok(last.events)
}

/// The build step of a workflow's phases, over a run whose goal and plan, `task` alone, are
/// approved and whose task is created as the approval creates it. Returns how the step ended
/// and every event of the run.
async fn build_with(crew: &Crew, task: TaskSpec) -> anyhow::Result<(StepDone, Vec<RunEvent>)> {
    let started = crew.run_started(RunMode::Crew, "Add due dates to tasks")?;
    let task = ways::briefed(task);
    let id = Some(task.id.clone());
    let kinds = [
        (None, goal_approved(&started)),
        (
            None,
            plan_approval(ApprovalEvent::Opened(Box::new(plan_opened(&task)))),
        ),
        (None, plan_approval(ApprovalEvent::Decided(plan_decided()))),
        (
            id.clone(),
            RunEventKind::Task(TaskEvent::Created(Box::new(task.clone()))),
        ),
        (
            id,
            RunEventKind::Task(TaskEvent::State {
                from: None,
                to: TaskState::Waiting,
                reason: "created from the approved plan".to_owned(),
            }),
        ),
    ];
    let mut history = vec![started.clone()];
    for (n, (task_id, kind)) in kinds.into_iter().enumerate() {
        history.push(RunEvent {
            seq: n as u64 + 1,
            task_id,
            kind,
            ..started.clone()
        });
    }
    let base = crew.repo.head()?;
    let base_tree = crew.repo_context()?.base_worktree;
    Git::with_env(crew.git_env())
        .add_detached_worktree(crew.repo.root(), &base_tree, &base)
        .await?;
    let (host, sink) = crew.host_over(history)?;
    let _env = crew.repo.env_guard();
    let build = tokio::spawn(async move { task::build_step(&host, &build_call()).await });
    let done = tokio::time::timeout(deadline::scaled(Duration::from_secs(60)), build)
        .await
        .context("the build step didn't finish")???;
    Ok((done, sink.events()))
}

fn goal_approved(started: &RunEvent) -> RunEventKind {
    RunEventKind::Goal(GoalEvent::Approved {
        file: GoalFile {
            run: started.run_id.clone(),
            version: 1,
            summary: "Add due dates to tasks".to_owned(),
            criteria: vec![Criterion::new("G1", "A task can have a due date.")],
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: Vec::new(),
            answers: Vec::new(),
            source: GoalSource::Readings,
            path: PathBuf::from(".argonauts/goal/run-01.md"),
            sha256: "0".repeat(64),
        },
        by: Party::User,
    })
}

fn plan_approval(event: ApprovalEvent) -> RunEventKind {
    RunEventKind::Approval(event)
}

fn plan_opened(task: &TaskSpec) -> ApprovalOpened {
    let packet = PlanPacket {
        summary: "One task".to_owned(),
        judging: None,
        near_tie: false,
        decisions: Vec::new(),
        graph: TaskGraph {
            tasks: vec![task.clone()],
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
    ApprovalOpened {
        id: ApprovalId::numbered(1),
        kind: ApprovalKind::Plan,
        title: "Approve this plan?".to_owned(),
        task: None,
        packet: ApprovalPacket::Plan(Box::new(packet)),
        evidence_sha256: "0".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    }
}

fn plan_decided() -> ApprovalDecided {
    ApprovalDecided {
        id: ApprovalId::numbered(1),
        action: AnswerAction::Approve,
        text: None,
        by: Party::User,
        evidence_sha256: "0".repeat(64),
        auto_preview: None,
        tasks: Vec::new(),
    }
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

fn last_state(events: &[RunEvent], task: &str) -> Option<TaskState> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Task(TaskEvent::State { to, .. })
            if event.task_id.as_ref().map(TaskId::as_str) == Some(task) =>
        {
            Some(*to)
        }
        _ => None,
    })
}

fn sessions_of(events: &[RunEvent], role: RoleName) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) if started.role == role => {
                Some(started.worker.to_string())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn agents_of_the_workers_model_each_take_a_part_and_merge_as_one_task() -> anyhow::Result<()>
{
    let (crew, worker, _) = crew("Due field")?;
    let task = planned(
        "Due field",
        &["due.rs", "due_test.rs"],
        Way::Agents { count: 2 },
    );
    let events = run_with(&crew, task).await?;

    let asked: Vec<String> = worker.specs().iter().map(prompt).collect();
    for part in ["Your part is due.rs", "Your part is due_test.rs"] {
        assert!(
            asked.iter().any(|prompt| prompt.contains(part)),
            "{part}: {asked:#?}"
        );
    }
    // The second part is a task of its own, whose work the first takes in.
    assert_eq!(
        last_state(&events, "task-due-field-part-2"),
        Some(TaskState::InReview)
    );
    assert_eq!(
        last_state(&events, "task-due-field"),
        Some(TaskState::Merged)
    );
    let merged = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Merge(MergeEvent::Merged { task, sha })
                if task.as_str() == "task-due-field" =>
            {
                Some(sha.clone())
            }
            _ => None,
        })
        .expect("the task merged into the run");
    let files = crew.repo.git(&["ls-tree", "--name-only", &merged])?;
    let files: Vec<&str> = files.lines().collect();
    assert!(
        files.contains(&"due.rs") && files.contains(&"due_test.rs"),
        "{files:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_worker_builds_through_maker_checker_and_another_maker_checks_it() -> anyhow::Result<()> {
    let (crew, _, reviewer) = crew("Due docs")?;
    let task = planned(
        "Due docs",
        &["docs.md"],
        Way::Workflow {
            name: WorkflowRef::new("maker-checker"),
        },
    );
    let events = run_with(&crew, task).await?;

    // Maker-checker's own Review, then the workflow's Review step.
    assert_eq!(
        sessions_of(&events, RoleName::Reviewer),
        [REVIEWER, REVIEWER],
        "{:#?}",
        reviewer.specs().iter().map(prompt).collect::<Vec<_>>()
    );
    let reviewed = events
        .iter()
        .filter(|event| {
            matches!(&event.kind, RunEventKind::Review(ReviewEvent::Reviewed { reviewer, .. })
                if reviewer.to_string() == REVIEWER)
        })
        .count();
    assert_eq!(reviewed, 2);
    assert_eq!(
        last_state(&events, "task-due-docs"),
        Some(TaskState::Merged)
    );
    Ok(())
}

#[tokio::test]
async fn with_one_maker_set_up_a_fresh_session_of_the_workers_model_checks_it() -> anyhow::Result<()>
{
    let (mut crew, _, reviewer) = crew("Due docs")?;
    crew.settings.crew.reviewer = vec![WORKER.parse()?];
    crew.settings.crew.judge = vec![WORKER.parse()?];
    let task = planned(
        "Due docs",
        &["docs.md"],
        Way::Workflow {
            name: WorkflowRef::new("maker-checker"),
        },
    );
    let events = run_with(&crew, task).await?;

    // Never a sibling from another role's list: the worker's own model, in a fresh session.
    let checked_by = sessions_of(&events, RoleName::Reviewer);
    assert_eq!(checked_by.first().map(String::as_str), Some(WORKER));
    assert!(reviewer.specs().is_empty());
    let marked = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Note(note)
            if note.text.starts_with("Same maker: ") && note.text.contains("no other maker"))
    });
    assert!(marked, "{events:#?}");
    Ok(())
}

#[tokio::test]
async fn best_of_n_makes_three_tries_and_the_task_keeps_the_one_picked() -> anyhow::Result<()> {
    let (crew, worker, _) = crew("Due docs")?;
    let task = planned(
        "Due docs",
        &["docs.md"],
        Way::Workflow {
            name: WorkflowRef::new("best-of-n"),
        },
    );
    let events = run_with(&crew, task).await?;

    // The task's own session and two fresh ones of the same model, each in its own worktree.
    let trees: Vec<String> = worker
        .specs()
        .iter()
        .filter_map(|spec| Some(spec.workdir.file_name()?.to_string_lossy().into_owned()))
        .collect();
    for tree in [
        "task-due-docs",
        "task-due-docs-try-2",
        "task-due-docs-try-3",
    ] {
        assert!(trees.iter().any(|made| made == tree), "{tree}: {trees:?}");
    }
    assert_eq!(
        sessions_of(&events, RoleName::Worker),
        [WORKER, WORKER, WORKER]
    );
    // Another maker picks, then reviews the work the task kept.
    assert_eq!(sessions_of(&events, RoleName::Judge), [REVIEWER]);
    let log = serde_json::to_string(&events)?;
    assert!(
        log.contains("kept try 2 of the 3 that passed its checks · It says when a task is due."),
        "the pick is noted"
    );
    assert_eq!(
        last_state(&events, "task-due-docs"),
        Some(TaskState::Merged)
    );
    let merged = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Merge(MergeEvent::Merged { task, sha })
                if task.as_str() == "task-due-docs" =>
            {
                Some(sha.clone())
            }
            _ => None,
        })
        .expect("the task merged into the run");
    let docs = crew.repo.git(&["show", &format!("{merged}:docs.md")])?;
    assert_eq!(docs, "built in task-due-docs-try-2");
    let reported = events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Task(TaskEvent::Reported(report))
            if event.task_id.as_ref().map(TaskId::as_str) == Some("task-due-docs") =>
        {
            Some(report.summary.clone())
        }
        _ => None,
    });
    assert_eq!(
        reported.as_deref(),
        Some("Wrote docs.md in task-due-docs-try-2"),
        "the kept try's report"
    );
    // The tries it didn't keep stay on their branches.
    assert_eq!(
        last_state(&events, "task-due-docs-try-3"),
        Some(TaskState::InReview)
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_one_task_cant_follow_says_why_instead_of_running() -> anyhow::Result<()> {
    let (crew, worker, _) = crew("Due docs")?;
    let folder = crew.repo.root().join(".argonauts/workflows");
    std::fs::write(folder.join("plan-first.toml"), PLAN_FIRST)?;
    let task = planned(
        "Due docs",
        &["docs.md"],
        Way::Workflow {
            name: WorkflowRef::new("plan-first"),
        },
    );
    let events = run_with(&crew, task).await?;
    assert!(worker.specs().is_empty(), "nothing was built");
    assert_eq!(last_state(&events, "task-due-docs"), None);
    let log = serde_json::to_string(&events)?;
    assert!(
        log.contains("Plan-first can't do one task: its Plan step is the orchestrator's."),
        "the reason is in the run's log"
    );
    Ok(())
}

#[tokio::test]
async fn draft_and_vote_drafts_by_each_maker_and_keeps_the_draft_most_judges_pick()
-> anyhow::Result<()> {
    let (mut crew, worker, reviewer) = crew("Due docs")?;
    let qwen = FakeRunner::cli(LOCAL, "qwen")
        .respond_with(|spec| checks(spec).unwrap_or_else(|| builds(spec)));
    let qwen = crew.services.workers.add(qwen);
    crew.settings.crew.worker = vec![WORKER.parse()?, LOCAL.parse()?];
    crew.settings.crew.judge = vec![REVIEWER.parse()?, LOCAL.parse()?];
    let task = planned(
        "Due docs",
        &["docs.md"],
        Way::Workflow {
            name: WorkflowRef::new("draft-and-vote"),
        },
    );
    let events = run_with(&crew, task).await?;

    // One draft by the task's worker, one by the other maker's, none seeing the other.
    let drafted_by = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Task(TaskEvent::Created(spec))
            if spec.id.as_str() == "task-due-docs-try-2" =>
        {
            spec.worker.clone()
        }
        _ => None,
    });
    assert_eq!(drafted_by, Some(LOCAL.parse()?));
    assert!(!worker.specs().is_empty() && !qwen.specs().is_empty());
    // Both judges vote for the second draft, which the task keeps and its maker now owns.
    let voted = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Note(note) if note.text.contains("the vote kept") => Some(note.text.clone()),
        _ => None,
    });
    let voted = voted.expect("a note says how the vote went");
    assert!(
        voted.ends_with(
            " · the vote kept try 2 of the 2 that passed its checks, with 2 of 2 votes · It says \
             when a task is due."
        ),
        "{voted}"
    );
    assert!(
        reviewer
            .specs()
            .iter()
            .any(|spec| prompt(spec).contains("try 2"))
    );
    let given = events.iter().any(|event| {
        matches!(&event.kind, RunEventKind::Task(TaskEvent::Reassigned { to, .. })
            if to.to_string() == LOCAL && event.task_id.as_ref().map(TaskId::as_str) == Some("task-due-docs"))
    });
    assert!(given, "the kept draft's maker owns the task");
    let merged = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Merge(MergeEvent::Merged { task, sha })
            if task.as_str() == "task-due-docs" =>
        {
            Some(sha.clone())
        }
        _ => None,
    });
    let merged = merged.expect("the task merged into the run");
    let docs = crew.repo.git(&["show", &format!("{merged}:docs.md")])?;
    assert!(docs.contains("task-due-docs-try-2"), "{docs}");
    Ok(())
}

#[tokio::test]
async fn the_build_step_of_a_workflows_phases_runs_each_task_its_way() -> anyhow::Result<()> {
    let (mut crew, worker, _) = crew("Due field")?;
    crew.settings.workflows.adapt_with = AdaptWith::Phases;
    let task = planned(
        "Due field",
        &["due.rs", "due_test.rs"],
        Way::Agents { count: 2 },
    );
    let (done, events) = build_with(&crew, task).await?;

    // The task's own session takes its part, an agent of its model the other.
    let asked: Vec<String> = worker.specs().iter().map(prompt).collect();
    for part in ["Your part is due.rs", "Your part is due_test.rs"] {
        assert!(
            asked.iter().any(|prompt| prompt.contains(part)),
            "{part}: {asked:#?}"
        );
    }
    // The part merges into the task, never into the run on its own, and the step ends without
    // waiting for it.
    assert_eq!(done.summary.as_deref(), Some("1 task merged into the run"));
    assert_eq!(last_state(&events, "task-001"), Some(TaskState::Merged));
    assert_eq!(
        last_state(&events, "task-001-part-2"),
        Some(TaskState::InReview)
    );
    let merged = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Merge(MergeEvent::Merged { task, sha })
                if task.as_str() == "task-001" =>
            {
                Some(sha.clone())
            }
            _ => None,
        })
        .expect("the task merged into the run");
    let files = crew.repo.git(&["ls-tree", "--name-only", &merged])?;
    let files: Vec<&str> = files.lines().collect();
    assert!(
        files.contains(&"due.rs") && files.contains(&"due_test.rs"),
        "{files:?}"
    );
    Ok(())
}

#[tokio::test]
async fn the_tries_a_build_step_didnt_keep_are_set_aside() -> anyhow::Result<()> {
    let (mut crew, worker, _) = crew("Due docs")?;
    crew.settings.workflows.adapt_with = AdaptWith::Phases;
    let task = planned(
        "Due docs",
        &["docs.md"],
        Way::Workflow {
            name: WorkflowRef::new("best-of-n"),
        },
    );
    let (done, events) = build_with(&crew, task).await?;

    assert_eq!(worker.specs().len(), 3, "three tries");
    assert_eq!(done.summary.as_deref(), Some("1 task merged into the run"));
    let merged = events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Merge(MergeEvent::Merged { task, sha })
                if task.as_str() == "task-001" =>
            {
                Some(sha.clone())
            }
            _ => None,
        })
        .expect("the task merged into the run");
    let docs = crew.repo.git(&["show", &format!("{merged}:docs.md")])?;
    assert_eq!(docs, "built in task-001-try-2");
    for unkept in ["task-001-try-2", "task-001-try-3"] {
        assert_eq!(last_state(&events, unkept), Some(TaskState::InReview));
    }
    Ok(())
}
