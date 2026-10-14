//! The checks, a reviewer and a sign-off as the conversation prints them, compared cell for cell
//! with the drawing in `golden/review_after.txt`, from the same example: a rate limiter made per
//! API key, its tests passing, one reviewer and one sign-off on Haiku 4.5.

use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::app::AppEvent;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::{
    EndReason, InputPart, InputPartKind, LaunchRecord, PromptVia, SessionEnd, SessionEvent,
    SessionInput, SessionStarted,
};
use argonauts_core::events::task::{CheckKind, CheckOutcome, TaskEvent, TaskState};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{AgentId, RoleName, SessionKey};
use argonauts_core::session_runner::{Access, RunnerKind};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::StepName;
use argonauts_core::{AgentEvent, RunId, Spend, TaskId};
use ratatui::style::Modifier;
use ratatui::text::Line;
use serde_json::{Value, json};
use time::macros::datetime;

use super::Bridge;
use crate::model::ViewModel;
use crate::options::UiOptions;
use crate::render::{Clock, RenderCtx};
use crate::text::plain_text;
use crate::theme::{Glyphs, Role, Theme};

const PICTURE: &str = include_str!("../../golden/review_after.txt");
const COLOURS: &str = include_str!("../../golden/review_after.html");
const WIDTH: u16 = 118;

struct Run {
    bridge: Bridge,
    model: ViewModel,
    seq: u64,
}

impl Run {
    fn new() -> Self {
        Self {
            bridge: Bridge::new(&UiOptions::default()),
            model: ViewModel::default(),
            seq: 0,
        }
    }

    fn send(&mut self, session: Option<&str>, kind: RunEventKind) {
        self.seq += 1;
        let event = AppEvent::Run {
            event: Box::new(RunEvent {
                seq: self.seq,
                ts: datetime!(2026-10-09 16:20 UTC),
                run_id: RunId::new("run-01").unwrap(),
                task_id: Some(TaskId::new("task-001").unwrap()),
                session: session.and_then(SessionKey::parse),
                source: "argonauts-code".to_owned(),
                kind,
            }),
        };
        self.bridge
            .on_event(&mut self.model, &event, Instant::now());
    }

    fn start(&mut self, session: &str, role: RoleName, step: Option<StepName>, label: &str) {
        let started = SessionStarted {
            worker: "claude-code/claude-haiku-4-5".parse().unwrap(),
            agent_id: AgentId("claude-code".to_owned()),
            provider: "anthropic".to_owned(),
            kind: RunnerKind::Cli,
            role,
            step,
            access: Access::ReadOnly,
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: None,
            workflow_step: None,
            label: Some(label.to_owned()),
            asked: None,
        };
        self.send(
            Some(session),
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        );
    }

    fn given(&mut self, session: &str, parts: Vec<InputPart>) {
        let input = SessionInput {
            launch: LaunchRecord {
                program: "claude".to_owned(),
                args: Vec::new(),
                cwd: PathBuf::from("/w"),
                env_set: Vec::new(),
                env_removed: Vec::new(),
                prompt_via: PromptVia::StreamInput,
                sandbox: None,
            },
            parts,
            queued: Vec::new(),
        };
        self.send(
            Some(session),
            RunEventKind::Session(SessionEvent::Input(Box::new(input))),
        );
    }

    fn read(&mut self, session: &str, id: &str, path: &str) {
        self.send(
            Some(session),
            RunEventKind::Agent(AgentEvent::ToolStarted {
                id: id.to_owned(),
                name: "Read".to_owned(),
                input: json!({"file_path": path}),
                parent: None,
            }),
        );
        self.send(
            Some(session),
            RunEventKind::Agent(AgentEvent::ToolFinished {
                id: id.to_owned(),
                ok: true,
                output: Some("1\tclass TokenBucket:".to_owned()),
                parent: None,
            }),
        );
    }

    fn end(&mut self, session: &str, answer: Value) {
        let end = SessionEnd {
            cli_session: None,
            exit_code: Some(0),
            signal: None,
            duration_ms: 13_000,
            reason: EndReason::Completed,
            spend: Spend::default(),
            tool_calls: 3,
            result_text: Some(answer.to_string()),
            structured: Some(answer),
            last_message: None,
            raw_log: None,
        };
        self.send(
            Some(session),
            RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
        );
    }

    fn printed(&mut self) -> Vec<Line<'static>> {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
        self.model.history.drain_printable(&ctx)
    }
}

fn part(kind: InputPartKind, label: &str, text: &str) -> InputPart {
    InputPart {
        kind,
        label: label.to_owned(),
        text: text.to_owned(),
        approx_tokens: 1,
        source: None,
    }
}

const CHANGE: &str = "diff --git a/ratelimit/bucket.py b/ratelimit/bucket.py\n+x\n\
                      diff --git a/ratelimit/middleware.py b/ratelimit/middleware.py\n+y\n\
                      diff --git a/tests/test_middleware.py b/tests/test_middleware.py\n+z\n";

const CHECKS: &str = "Argonauts Code ran these after the build, in the task's worktree:\n\
                      tests (python3 -m unittest -q) passed: 12 passed";

fn reviewed_run() -> Run {
    let mut run = Run::new();
    run.send(
        None,
        RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
            request: "Make the rate limiter per API key".into(),
            title: "Make the rate limiter per API key".into(),
            mode: RunMode::Crew,
            forced: false,
            workflow: "council".into(),
            estimate: None,
            captain_or_crew: None,
            conversation_turn: None,
            continues: None,
            repo_root: PathBuf::from("/repo"),
            base_branch: Some("main".into()),
            base_commit: "8b41d07aa".into(),
            uncommitted: vec![],
            uncommitted_included: false,
            settings_sha256: String::new(),
            settings: Value::Null,
            overrides: vec![],
            cli_versions: Default::default(),
            version: "0.0.1".into(),
            outside_agent: None,
            instructions: Vec::new(),
            required_steps: Vec::new(),
        }))),
    );
    // The builder is still at work, so only its label reaches the conversation, on the checks' row.
    run.start(
        "s-001",
        RoleName::Worker,
        Some(StepName::Build),
        "build:make-the-rate",
    );
    run.printed();
    run.send(
        None,
        RunEventKind::Task(TaskEvent::State {
            from: Some(TaskState::Running),
            to: TaskState::Checking,
            reason: String::new(),
        }),
    );
    run.send(
        None,
        RunEventKind::Check(CheckOutcome {
            name: "tests".into(),
            kind: CheckKind::Command,
            passed: true,
            command: Some(vec![
                "python3".into(),
                "-m".into(),
                "unittest".into(),
                "-q".into(),
            ]),
            exit_code: Some(0),
            duration_ms: 30,
            summary: "12 passed".into(),
            output_tail: String::new(),
            violations: vec![],
            sandboxed: true,
            skipped: false,
        }),
    );
    run.send(
        None,
        RunEventKind::Task(TaskEvent::State {
            from: Some(TaskState::Checking),
            to: TaskState::InReview,
            reason: String::new(),
        }),
    );
    run.start("s-002", RoleName::Reviewer, None, "review:per-key");
    run.given(
        "s-002",
        vec![
            part(InputPartKind::Frame, "Frame", "You are a reviewer."),
            part(InputPartKind::Evidence, "the change", CHANGE),
            part(InputPartKind::CheckOutput, "the checks", CHECKS),
        ],
    );
    run.read("s-002", "t1", "ratelimit/bucket.py");
    run.read("s-002", "t2", "ratelimit/middleware.py");
    run.read("s-002", "t3", "tests/test_middleware.py");
    run.end(
        "s-002",
        json!({"accept": true, "findings": [], "summary": "Per-key buckets, the shared no-key \
            bucket and the refill clamp are right; 429 carries Retry-After."}),
    );
    run.start("s-003", RoleName::Judge, None, "sign-off:per-key");
    run.given(
        "s-003",
        vec![
            part(InputPartKind::Frame, "Frame", "You are the judge."),
            part(InputPartKind::Evidence, "the change", CHANGE),
            part(InputPartKind::CheckOutput, "the checks", CHECKS),
        ],
    );
    run.end(
        "s-003",
        json!({"accept": true, "findings": [], "summary": "Ready to merge."}),
    );
    run
}

#[test]
fn checks_a_reviewer_and_a_sign_off_read_as_drawn() {
    let mut run = reviewed_run();
    // The agents' start rows print between them too; the picture is of these three.
    let printed: Vec<Line<'static>> = run
        .printed()
        .split(|line| plain_text(std::slice::from_ref(line)).is_empty())
        .filter(|block| {
            let head = plain_text(&block[..1]);
            ["● Checks(", "● review:", "● sign-off:"]
                .iter()
                .any(|start| head.starts_with(start))
        })
        .collect::<Vec<_>>()
        .join(&Line::default());
    let text = plain_text(&printed);
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let want: Vec<&str> = PICTURE.lines().map(str::trim_end).collect();
    assert_eq!(lines.join("\n"), want.join("\n"), "\nprinted:\n{text}");
    for line in &lines {
        assert!(crate::text::str_width(line) <= usize::from(WIDTH), "{line}");
    }

    // The colours: green bullets, bold names, the agent and every ⎿ line dim, the verdict plain,
    // and every other cell as the drawing colours it.
    let theme = {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    };
    super::transcript::picture_check::assert_matches(
        "review_after",
        &printed,
        PICTURE,
        COLOURS,
        &theme,
        &[],
    );
    let rows = &printed;
    let success = theme.style(Role::Success).fg;
    let dim = theme.style(Role::Dim).fg;
    let span = |row: &Line<'static>, needle: &str| {
        row.spans
            .iter()
            .find(|span| span.content.contains(needle))
            .cloned()
            .unwrap_or_else(|| panic!("no {needle:?} in {row:?}"))
    };
    for (at, name) in [
        (0, "Checks"),
        (3, "review:per-key"),
        (7, "sign-off:per-key"),
    ] {
        assert_eq!(span(&rows[at], "●").style.fg, success, "{name}");
        assert!(
            span(&rows[at], name)
                .style
                .add_modifier
                .contains(Modifier::BOLD),
            "{name} is bold"
        );
    }
    assert_eq!(span(&rows[3], "Haiku 4.5").style.fg, dim);
    for at in [1, 4, 8] {
        for needle in ["⎿", rows[at].spans.last().unwrap().content.trim()] {
            assert_eq!(span(&rows[at], needle).style.fg, dim, "row {at}: {needle}");
        }
    }
    assert_ne!(
        span(&rows[5], "Accepted.").style.fg,
        dim,
        "the verdict reads plain"
    );
}
