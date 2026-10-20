//! A council's agents as the conversation prints them, one row each as each one finishes,
//! compared cell for cell with rows 5 to 37 of `tests/golden/council_rows_after`: the
//! rate-limiter council where Propose and Challenge ran, Decide chose p2, the planner is
//! plan:council and the sign-off read the task's change.
//!
//! The picture's agents used tools that print nothing in the conversation, so their rows are
//! their Done lines alone; an agent whose calls print has them above its Done line, in the same
//! row.

use std::path::PathBuf;
use std::time::Instant;

use std::sync::Arc;

use argonauts_core::api::UsageSummary;
use argonauts_core::app::AppEvent;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::{
    EndReason, InputPart, InputPartKind, LaunchRecord, PromptVia, SessionEnd, SessionEvent,
    SessionInput, SessionStarted,
};
use argonauts_core::events::task::{CheckKind, CheckOutcome};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{AgentId, ModelRef, RoleName, SessionKey};
use argonauts_core::overview::ModelTokens;
use argonauts_core::session_runner::{Access, RunnerKind};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::StepName;
use argonauts_core::{AgentEvent, RunId, Spend, TaskId};
use ratatui::text::Line;
use serde_json::{Value, json};
use time::macros::datetime;

use super::Bridge;
use super::transcript::picture_check::assert_rows_match;
use crate::model::ViewModel;
use crate::options::UiOptions;
use crate::render::{Clock, RenderCtx};
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

const PICTURE: &str = include_str!("../../tests/golden/council_rows_after.txt");
const COLOURS: &str = include_str!("../../tests/golden/council_rows_after.html");
const WIDTH: u16 = 118;

/// The picture's rows of the agents, from plan:council's header to the sign-off's last line.
const ROWS: std::ops::Range<usize> = 5..38;

const HAIKU: &str = "claude-code/claude-haiku-5-5";
const LUNA: &str = "codex/gpt-6-luna";
const FLASH: &str = "antigravity-cli/gemini-3.8-flash";
const TASK: &str = "per-key-rate-limit";

struct Run {
    bridge: Bridge,
    model: ViewModel,
    seq: u64,
}

/// One of the council's agents: its session, label, model, role, whether it only reads, the
/// task it works on, its calls, seconds and tokens, and its answer.
struct Agent {
    session: &'static str,
    label: &'static str,
    worker: &'static str,
    role: RoleName,
    reads: bool,
    task: bool,
    calls: u32,
    seconds: u64,
    tokens: u64,
    answer: Value,
    /// Its task's checks ran and passed before it ended.
    checked: bool,
}

impl Run {
    fn new() -> Self {
        let mut bridge = Bridge::new(&UiOptions::default());
        bridge.state.usage = Some(Arc::new(ranked()));
        Self {
            bridge,
            model: ViewModel::default(),
            seq: 0,
        }
    }

    fn app(&mut self, event: AppEvent) {
        self.bridge
            .on_event(&mut self.model, &event, Instant::now());
    }

    fn send(&mut self, session: Option<&str>, task: bool, kind: RunEventKind) {
        self.seq += 1;
        let event = RunEvent {
            seq: self.seq,
            ts: datetime!(2026-10-17 07:10 UTC),
            run_id: RunId::new("run-01").unwrap(),
            task_id: task.then(|| TaskId::new(TASK).unwrap()),
            session: session.and_then(SessionKey::parse),
            source: "argonauts-code".to_owned(),
            kind,
        };
        self.app(AppEvent::Run {
            event: Box::new(event),
        });
    }

    fn agent(&mut self, agent: &Agent, given: Vec<InputPart>) {
        let worker: ModelRef = agent.worker.parse().unwrap();
        let started = SessionStarted {
            agent_id: AgentId(worker.agent.clone()),
            worker,
            provider: "provider".to_owned(),
            kind: RunnerKind::Cli,
            role: agent.role,
            step: (agent.role == RoleName::Worker && !agent.reads).then_some(StepName::Build),
            access: if agent.reads {
                Access::ReadOnly
            } else {
                Access::Build
            },
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: None,
            workflow_step: None,
            label: Some(agent.label.to_owned()),
            asked: None,
        };
        let at = Some(agent.session);
        self.send(
            at,
            agent.task,
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        );
        if !given.is_empty() {
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
                parts: given,
                queued: Vec::new(),
            };
            self.send(
                at,
                agent.task,
                RunEventKind::Session(SessionEvent::Input(Box::new(input))),
            );
        }
        // Argonauts Code's own calls, which count as tool uses and print nothing.
        for n in 0..agent.calls {
            let id = format!("{}-{n}", agent.session);
            self.send(
                at,
                agent.task,
                RunEventKind::Agent(AgentEvent::ToolStarted {
                    id: id.clone(),
                    name: "mcp__argonauts__status".to_owned(),
                    input: json!({}),
                    parent: None,
                }),
            );
            self.send(
                at,
                agent.task,
                RunEventKind::Agent(AgentEvent::ToolFinished {
                    id,
                    ok: true,
                    output: None,
                    parent: None,
                }),
            );
        }
        if agent.checked {
            self.send(None, true, passing_checks());
        }
        let end = SessionEnd {
            cli_session: None,
            exit_code: Some(0),
            signal: None,
            duration_ms: agent.seconds * 1000,
            reason: EndReason::Completed,
            spend: Spend {
                input_tokens: agent.tokens,
                ..Spend::default()
            },
            tool_calls: u64::from(agent.calls),
            result_text: Some(agent.answer.to_string()),
            structured: Some(agent.answer.clone()),
            last_message: None,
            raw_log: None,
        };
        self.send(
            at,
            agent.task,
            RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
        );
    }

    fn printed(&mut self) -> Vec<Line<'static>> {
        let theme = theme();
        let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
        self.model.history.drain_printable(&ctx)
    }
}

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The names the CLIs give these models.
fn names() -> AppEvent {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::model_catalog::CatalogEntry;
    let mut names = DisplayNames::default();
    for (model, name) in [
        (HAIKU, "Haiku 5.5"),
        (LUNA, "GPT 6 Luna"),
        (FLASH, "Gemini 3.8 Flash"),
    ] {
        let mut entry = CatalogEntry::new(model.parse().unwrap());
        entry.name = Some(name.to_owned());
        names.catalog.models.push(entry);
    }
    for (cli, name) in [
        ("claude-code", "Claude Code"),
        ("codex", "Codex"),
        ("antigravity-cli", "Antigravity CLI"),
    ] {
        names.clis.insert(cli.to_owned(), name.to_owned());
    }
    AppEvent::Names {
        names: Box::new(names),
    }
}

fn council_started() -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: "Give each X-Api-Key its own token bucket".into(),
        title: "Per key rate limiter".into(),
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
    })))
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

/// The task's change as the sign-off reads it, under the task's name: 2 files, +17 −4.
fn change() -> String {
    let mut diff = String::from("# per-key-rate-limit\n");
    for (file, added, removed) in [
        ("ratelimit/bucket.py", 11, 3),
        ("ratelimit/middleware.py", 6, 1),
    ] {
        diff.push_str(&format!(
            "diff --git a/{file} b/{file}\n--- a/{file}\n+++ b/{file}\n"
        ));
        diff.push_str(&"-old\n".repeat(removed));
        diff.push_str(&"+new\n".repeat(added));
    }
    diff
}

const CHECKS: &str = "Argonauts Code ran these after the build, in the task's worktree:\n\
                      tests (python3 -m unittest -q) passed: 5 passed";

#[allow(clippy::too_many_arguments)]
fn agent(
    session: &'static str,
    label: &'static str,
    worker: &'static str,
    role: RoleName,
    (reads, task): (bool, bool),
    (calls, seconds, tokens): (u32, u64, u64),
    answer: Value,
) -> Agent {
    Agent {
        session,
        label,
        worker,
        role,
        reads,
        task,
        calls,
        seconds,
        tokens,
        answer,
        checked: false,
    }
}

fn passing_checks() -> RunEventKind {
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
        summary: "5 passed".into(),
        output_tail: String::new(),
        violations: vec![],
        sandboxed: true,
        skipped: false,
    })
}

fn council_run() -> Run {
    let mut run = Run::new();
    run.app(names());
    run.send(None, false, council_started());
    let proposal = json!({"plan": "a bucket a key"});
    let against = |a: &str, b: &str, points: usize| json!({"against": [a, b], "points": vec!["a point"; points]});
    let reading = (true, false);
    let agents = [
        agent(
            "s-001",
            "plan:council",
            HAIKU,
            RoleName::Orchestrator,
            reading,
            (1, 22, 17_700),
            json!({"script": "…"}),
        ),
        agent(
            "s-002",
            "propose:p1",
            HAIKU,
            RoleName::Worker,
            reading,
            (4, 41, 14_200),
            proposal.clone(),
        ),
        agent(
            "s-003",
            "propose:p2",
            LUNA,
            RoleName::Worker,
            reading,
            (3, 52, 11_000),
            proposal.clone(),
        ),
        agent(
            "s-004",
            "propose:p3",
            FLASH,
            RoleName::Worker,
            reading,
            (2, 38, 9_400),
            proposal,
        ),
        agent(
            "s-005",
            "challenge:p1",
            HAIKU,
            RoleName::Worker,
            reading,
            (0, 24, 8_100),
            against("p2", "p3", 3),
        ),
        agent(
            "s-006",
            "challenge:p2",
            LUNA,
            RoleName::Worker,
            reading,
            (0, 29, 7_600),
            against("p1", "p3", 2),
        ),
        agent(
            "s-007",
            "challenge:p3",
            FLASH,
            RoleName::Worker,
            reading,
            (0, 21, 6_900),
            against("p1", "p2", 2),
        ),
        agent(
            "s-008",
            "decide:plan",
            HAIKU,
            RoleName::Judge,
            reading,
            (0, 11, 43_800),
            json!({
                "chose": "p2",
                "summary": "Chose p2, with p3's whole-second Retry-After",
                "tasks": [{"task": TASK, "title": "Per-key buckets"}],
            }),
        ),
    ];
    for agent in &agents {
        run.agent(agent, Vec::new());
    }
    let mut build = agent(
        "s-009",
        "build:per-key-rate-limit",
        HAIKU,
        RoleName::Worker,
        (false, true),
        (9, 18, 118_800),
        json!({"status": "done", "summary": "Per-key buckets", "files_changed": ["ratelimit/bucket.py"]}),
    );
    build.checked = true;
    run.agent(&build, Vec::new());
    let checks = || part(InputPartKind::CheckOutput, "the checks", CHECKS);
    let review = agent(
        "s-010",
        "review:per-key-rate-limit",
        LUNA,
        RoleName::Reviewer,
        (true, true),
        (0, 4, 17_200),
        json!({"accept": true, "findings": []}),
    );
    run.agent(&review, vec![checks()]);
    let sign_off = agent(
        "s-011",
        "signoff:per-key-rate-limit",
        HAIKU,
        RoleName::Judge,
        reading,
        (0, 10, 41_600),
        json!({"accept": true, "findings": []}),
    );
    run.agent(
        &sign_off,
        vec![
            part(InputPartKind::Evidence, "the change", &change()),
            checks(),
        ],
    );
    run
}

/// Your models ranked by the tokens each ran here, as the picture's colours have them: Haiku
/// 5.5 is Claude Code's third, GPT 6 Luna Codex's second, and Gemini 3.8 Flash Antigravity
/// CLI's third.
fn ranked() -> UsageSummary {
    let ranked = [
        ("claude-code", "claude-opus-5-5", 900_000),
        ("claude-code", "claude-sonnet-5-5", 500_000),
        ("claude-code", "claude-haiku-5-5", 100_000),
        ("codex", "gpt-5.6", 800_000),
        ("codex", "gpt-6-luna", 300_000),
        ("antigravity-cli", "gemini-3.6-pro", 600_000),
        ("antigravity-cli", "gemini-3.6-flash", 400_000),
        ("antigravity-cli", "gemini-3.8-flash", 100_000),
    ];
    UsageSummary {
        all_time_models: ranked
            .map(|(agent, model, tokens)| ModelTokens {
                agent: agent.to_owned(),
                model: Some(model.to_owned()),
                tokens,
            })
            .to_vec(),
        ..UsageSummary::default()
    }
}

#[test]
fn a_councils_agents_print_one_row_each_as_drawn() {
    let mut run = council_run();
    let printed = run.printed();
    let blocks: Vec<Vec<Line<'static>>> = printed
        .split(|line| plain_text(std::slice::from_ref(line)).is_empty())
        .filter(|block| {
            plain_text(&block[..1])
                .strip_prefix("● ")
                .is_some_and(|rest| rest.contains(':'))
        })
        .map(<[Line<'static>]>::to_vec)
        .collect();
    let mut drawn: Vec<Line<'static>> = Vec::new();
    for block in blocks {
        drawn.extend(block);
        drawn.push(Line::default());
    }
    drawn.pop();
    // The picture pads each row to its width; the screen leaves the rest of a row empty.
    let want: Vec<&str> = PICTURE.lines().map(str::trim_end).collect();
    let picture = want.join("\n");
    for line in &drawn {
        assert!(
            crate::text::str_width(&line.to_string()) <= usize::from(WIDTH),
            "{line}"
        );
    }
    assert_rows_match(
        "council_rows_after",
        &drawn,
        (&picture, COLOURS),
        ROWS,
        &theme(),
        &[],
    );
}
