use std::path::PathBuf;

use argonauts_core::events::goal::{GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{DesignDecision, PlanEvent};
use argonauts_core::events::run::Party;
use argonauts_core::events::session::{
    EndReason, InputPart, InputPartKind, LaunchRecord, PromptVia, SessionEnd, SessionEvent,
    SessionInput,
};
use argonauts_core::events::task::{CheckKind, CheckOutcome, TaskEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{DecisionId, SessionKey};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::steps::task::{ReportStatus, TaskReport};
use argonauts_core::{AgentEvent, CommitInfo, DiffStat, Spend, TaskId, TodoItem, TodoStatus};
use serde_json::json;
use time::OffsetDateTime;

use super::{build, build_with_limit, prompt_part};
use crate::fallback::testing::{TASK, run, task, worker};

struct Log {
    events: Vec<RunEvent>,
}

impl Log {
    fn new() -> Self {
        Self { events: Vec::new() }
    }

    fn push(&mut self, task: Option<&str>, session: Option<u32>, kind: RunEventKind) {
        self.events.push(RunEvent {
            seq: self.events.len() as u64,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: run(),
            task_id: task.map(|id| TaskId::new(id).expect("a valid task id")),
            session: session.map(SessionKey::numbered),
            source: "argonauts-code".to_owned(),
            kind,
        });
    }

    fn task(&mut self, event: TaskEvent) {
        self.push(Some(TASK), None, RunEventKind::Task(event));
    }

    /// Agent events carry only their session, as the runner records them.
    fn agent(&mut self, session: u32, event: AgentEvent) {
        self.push(None, Some(session), RunEventKind::Agent(event));
    }
}

fn goal(sha: &str) -> GoalEvent {
    GoalEvent::Approved {
        file: GoalFile {
            run: run(),
            version: 1,
            summary: "Rate limiting".to_owned(),
            criteria: Vec::new(),
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: Vec::new(),
            answers: Vec::new(),
            source: GoalSource::Prompt,
            path: PathBuf::from(".argonauts/goal.md"),
            sha256: sha.to_owned(),
        },
        by: Party::User,
    }
}

fn input(parts: Vec<InputPart>) -> SessionEvent {
    SessionEvent::Input(Box::new(SessionInput {
        launch: LaunchRecord {
            program: "claude".to_owned(),
            args: vec!["<prompt>".to_owned()],
            cwd: PathBuf::from("/tmp/wt"),
            env_set: Vec::new(),
            env_removed: Vec::new(),
            prompt_via: PromptVia::Argument,
            sandbox: None,
        },
        parts,
        queued: Vec::new(),
    }))
}

fn part(kind: InputPartKind, source: &str) -> InputPart {
    InputPart {
        kind,
        label: "notes".to_owned(),
        text: "…".to_owned(),
        approx_tokens: 1,
        source: Some(source.to_owned()),
    }
}

fn tool(id: &str, name: &str, input: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolStarted {
        id: id.to_owned(),
        name: name.to_owned(),
        input,
        parent: None,
    }
}

fn finished(id: &str, ok: bool, output: &str) -> AgentEvent {
    AgentEvent::ToolFinished {
        id: id.to_owned(),
        ok,
        output: Some(output.to_owned()),
        parent: None,
    }
}

fn ended(reason: EndReason, usd: f64, minutes: u64) -> SessionEvent {
    SessionEvent::Ended(Box::new(SessionEnd {
        cli_session: Some("v-1".to_owned()),
        exit_code: Some(1),
        signal: None,
        duration_ms: minutes * 60_000,
        reason,
        spend: Spend {
            cost_usd: usd,
            ..Spend::default()
        },
        tool_calls: 4,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log: None,
    }))
}

fn check(name: &str, passed: bool, summary: &str) -> CheckOutcome {
    CheckOutcome {
        name: name.to_owned(),
        kind: CheckKind::Command,
        passed,
        command: Some(vec!["cargo".to_owned(), "test".to_owned()]),
        exit_code: Some(i32::from(!passed)),
        duration_ms: 10,
        summary: summary.to_owned(),
        output_tail: String::new(),
        violations: Vec::new(),
        sandboxed: true,
        skipped: false,
    }
}

/// A task's log: the run's goal and a decision, one session on this task that read, edited,
/// and ran tests, and another task's session that must not leak in.
fn task_log() -> Log {
    let mut log = Log::new();
    log.push(None, None, RunEventKind::Goal(goal("goal-sha")));
    log.push(
        None,
        None,
        RunEventKind::Plan(PlanEvent::Decided(DesignDecision {
            id: DecisionId::parse("D-003").expect("a valid decision id"),
            title: "Store".to_owned(),
            choice: "Redis".to_owned(),
            rationale: "Shared across nodes.".to_owned(),
            alternatives: Vec::new(),
            path: None,
        })),
    );
    log.task(TaskEvent::Worktree {
        path: PathBuf::from("/tmp/wt"),
        branch: "argonauts/run-01/task-003".to_owned(),
        base: "main".to_owned(),
    });
    log.push(
        Some(TASK),
        Some(1),
        RunEventKind::Session(input(vec![
            part(InputPartKind::Notes, "notes/rate-limit-middleware.md"),
            part(InputPartKind::Decisions, "D-003"),
        ])),
    );
    log.agent(1, tool("t1", "Read", json!({ "file_path": "src/lib.rs" })));
    log.agent(1, finished("t1", true, "fn main() {}"));
    log.agent(1, tool("t2", "Read", json!({ "file_path": "src/lib.rs" })));
    log.agent(
        1,
        AgentEvent::FileChanged {
            path: "src/middleware/limit.rs".to_owned(),
            kind: "add".to_owned(),
        },
    );
    log.agent(
        1,
        tool(
            "t3",
            "Edit",
            json!({ "file_path": "src/middleware/limit.rs" }),
        ),
    );
    log.agent(1, tool("t4", "Bash", json!({ "command": "cargo test" })));
    log.agent(
        1,
        finished(
            "t4",
            false,
            "running 42 tests\ntest result: FAILED. 41 passed; 1 failed\n",
        ),
    );
    log.agent(
        1,
        AgentEvent::Todo {
            items: vec![TodoItem {
                text: "old list".to_owned(),
                status: TodoStatus::Pending,
            }],
        },
    );
    log.agent(
        1,
        AgentEvent::Todo {
            items: vec![
                TodoItem {
                    text: "Write the limiter".to_owned(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    text: "Fix the failing test".to_owned(),
                    status: TodoStatus::InProgress,
                },
            ],
        },
    );
    // Another task's session, which the note must leave out.
    log.push(
        Some("task-004"),
        Some(2),
        RunEventKind::Session(input(vec![part(InputPartKind::Notes, "notes/other.md")])),
    );
    log.agent(
        2,
        tool("x1", "Read", json!({ "file_path": "src/other.rs" })),
    );
    log.push(
        Some(TASK),
        None,
        RunEventKind::Check(check("tests", true, "old run")),
    );
    log.push(
        Some(TASK),
        None,
        RunEventKind::Check(check("tests", false, "41 passed, 1 failed")),
    );
    log.task(TaskEvent::Reported(Box::new(TaskReport {
        status: ReportStatus::Blocked,
        summary: "Limiter written; one test fails on the burst case.".to_owned(),
        files_changed: vec!["src/middleware/limit.rs".to_owned()],
        tests_run: vec!["cargo test".to_owned()],
        concerns: vec!["The burst window may be off by one.".to_owned()],
        questions: Vec::new(),
        change_requests: Vec::new(),
        memory_proposals: Vec::new(),
        answer: String::new(),
    })));
    log.push(
        Some(TASK),
        Some(1),
        RunEventKind::Session(ended(EndReason::UsageLimit { resets_at: None }, 1.25, 12)),
    );
    log.task(TaskEvent::Committed(CommitInfo {
        branch: "argonauts/run-01/task-003".to_owned(),
        sha: Some("7c1e2a0ffee".to_owned()),
        message: "task-003: work so far".to_owned(),
        stat: DiffStat {
            files_changed: 4,
            insertions: 96,
            deletions: 12,
        },
        by: Some(worker("claude-code/opus")),
    }));
    log
}

#[test]
fn fallback_note_contents() {
    let mut spec = task(TASK);
    spec.limit.limit = Some(LimitAmount {
        unit: LimitUnit::Usd,
        amount: 3.0,
    });
    let note = build(&spec, &task_log().events);

    assert_eq!(note.task, spec);
    assert_eq!(note.goal_sha256.as_deref(), Some("goal-sha"));
    assert_eq!(
        note.notes,
        ["notes/rate-limit-middleware.md", "D-003 Store: Redis"]
    );
    assert_eq!(note.work.branch, "argonauts/run-01/task-003");
    assert_eq!(note.work.commit.as_deref(), Some("7c1e2a0ffee"));
    assert_eq!(
        note.work.stat,
        DiffStat {
            files_changed: 4,
            insertions: 96,
            deletions: 12
        }
    );
    assert_eq!(
        note.happened,
        [
            "read src/lib.rs",
            "edited src/middleware/limit.rs (add)",
            "ran `cargo test`: failed, test result: FAILED. 41 passed; 1 failed",
        ]
    );
    assert_eq!(note.last_todo.len(), 2);
    assert_eq!(note.last_todo[1].text, "Fix the failing test");
    assert_eq!(note.checks.len(), 1, "only the latest run of each check");
    assert!(!note.checks[0].passed);
    assert_eq!(note.why_stopped, "usage limit");
    assert_eq!(
        note.last_words.as_ref().map(|report| report.status),
        Some(ReportStatus::Blocked)
    );
    let left = note.limit_left.expect("a dollar limit");
    assert_eq!(left.unit, LimitUnit::Usd);
    assert!((left.amount - 1.75).abs() < 1e-9);

    let part = prompt_part(&note);
    assert_eq!(part.kind, InputPartKind::Fallback);
    assert_eq!(part.source.as_deref(), Some(TASK));
    for expected in [
        "Fallback · task-003 Middleware",
        "Goal: sha256 goal-sha",
        "notes/rate-limit-middleware.md",
        "Work so far: commit 7c1e2a0 on argonauts/run-01/task-003: 4 files, +96 -12",
        "  read src/lib.rs",
        "  [~] Fix the failing test",
        "  tests: failed, 41 passed, 1 failed",
        "Why it stopped: usage limit",
        "Limiter written",
        "Limit left: $1.75",
    ] {
        assert!(
            part.text.contains(expected),
            "missing {expected:?} in\n{}",
            part.text
        );
    }
    assert!(!part.text.contains("src/other.rs"));
    assert!(!part.text.contains("notes/other.md"));
    assert_eq!(
        part.approx_tokens as usize,
        part.text.chars().count().div_ceil(4)
    );
}

#[test]
fn note_from_no_events_still_names_the_task() {
    let spec = task(TASK);
    let note = build_with_limit(
        &spec,
        &[],
        Some(LimitAmount {
            unit: LimitUnit::PlanUsage,
            amount: 0.2,
        }),
    );
    assert_eq!(note.work.branch, "");
    assert!(note.happened.is_empty());
    assert_eq!(
        note.limit_left, None,
        "plan usage spend isn't known per task"
    );
    assert!(prompt_part(&note).text.contains("nothing committed yet"));
}

#[test]
fn long_histories_keep_the_latest_steps() {
    let mut log = Log::new();
    log.push(
        Some(TASK),
        Some(1),
        RunEventKind::Session(input(Vec::new())),
    );
    for n in 0..100 {
        log.agent(
            1,
            tool(
                &format!("t{n}"),
                "Bash",
                json!({ "command": format!("step {n}") }),
            ),
        );
    }
    let note = build(&task(TASK), &log.events);
    assert_eq!(note.happened.len(), super::MAX_HAPPENED);
    assert_eq!(note.happened[0], "(41 earlier steps left out)");
    assert_eq!(
        note.happened.last().map(String::as_str),
        Some("ran `step 99`")
    );
    assert_eq!(note.work.branch, "argonauts/run-01/task-003");
}
