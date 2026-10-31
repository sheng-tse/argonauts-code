//! An Orchestrator run's agents as the conversation prints them when each one finishes, headed
//! as Claude Code heads its agents: the label bold with the task it works in parentheses, then
//! the logo of whoever serves its model, the model in its colour and its CLI, and under it the
//! counts in parentheses in Claude Code's order. The run plans with Opus 5.5, builds with Haiku
//! 5.5 and reviews with GPT 6 Luna, the first two of Claude Code's models and Codex's first.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use argonauts_core::agents::session_runner::{Access, RunnerKind};
use argonauts_core::app::AppEvent;
use argonauts_core::app::overview::ModelTokens;
use argonauts_core::base::names::{AgentId, ModelRef, RoleName, SeatId, SessionKey, WorkflowId};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::model_catalog::CatalogEntry;
use argonauts_core::runs::api::UsageSummary;
use argonauts_core::runs::events::run::{RunLifecycle, RunMode, RunStarted};
use argonauts_core::runs::events::session::{
    EndReason, InputPart, InputPartKind, LaunchRecord, PromptVia, SessionEnd, SessionEvent,
    SessionInput, SessionStarted,
};
use argonauts_core::runs::events::task::{
    CheckKind, CheckOutcome, TaskEvent, TaskKind, TaskLimit, TaskSpec,
};
use argonauts_core::runs::events::workflow::{DeclaredPhase, WorkflowDeclared, WorkflowEvent};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::workflow::StandardPhase;
use argonauts_core::{AgentEvent, RunId, Spend, TaskId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use serde_json::{Value, json};
use time::macros::datetime;

use crate::app::model::ViewModel;
use crate::app::options::UiOptions;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::theme::{Glyphs, Role, Theme};
use crate::bridge::Bridge;
use crate::cells::cell::UserMessage;
use crate::frame::state::WorkflowState;

const OPUS: &str = "claude-code/claude-opus-5-5";
const HAIKU: &str = "claude-code/claude-haiku-5-5";
const LUNA: &str = "codex/gpt-6-luna";
const SOL: &str = "codex/gpt-6.1-sol";
const ASTRA: &str = "codex/gpt-6-astra";

/// One of the run's agents: its session, label, model, role and step, the task it works, and
/// its calls, seconds and tokens. A reviewer is given the checks' result to judge.
struct Agent {
    session: &'static str,
    label: &'static str,
    worker: &'static str,
    role: RoleName,
    step: StepName,
    task: Option<&'static str>,
    calls: u32,
    seconds: u64,
    tokens: u64,
}

struct Run {
    bridge: Bridge,
    model: ViewModel,
    seq: u64,
}

impl Run {
    /// The run you asked for with `prompt`, your message printed first when there is one.
    fn new(prompt: Option<&str>) -> Self {
        let mut bridge = Bridge::new(&UiOptions::default());
        bridge.state.usage = Some(Arc::new(ranked()));
        let mut run = Self {
            bridge,
            model: ViewModel::default(),
            seq: 0,
        };
        if let Some(prompt) = prompt {
            run.model.history.push(UserMessage::new(prompt));
        }
        run.app(AppEvent::Names {
            names: Box::new(names()),
        });
        run.send(None, None, started());
        for (task, title) in [
            ("due-tests", "test --due"),
            ("due-flag", "add the --due flag"),
        ] {
            run.send(None, Some(task), planned(task, title));
        }
        run
    }

    fn app(&mut self, event: AppEvent) {
        self.bridge
            .on_event(&mut self.model, &event, Instant::now());
    }

    fn send(&mut self, session: Option<&str>, task: Option<&str>, kind: RunEventKind) {
        self.seq += 1;
        let event = RunEvent {
            seq: self.seq,
            ts: datetime!(2026-10-28 07:10 UTC),
            run_id: RunId::new("run-01").unwrap(),
            task_id: task.map(|task| TaskId::new(task).unwrap()),
            session: session.and_then(SessionKey::parse),
            source: "argonauts-code".to_owned(),
            kind,
        };
        self.app(AppEvent::Run {
            event: Box::new(event),
        });
    }

    fn agent(&mut self, agent: &Agent) {
        self.start(agent, None);
        let (at, task) = (Some(agent.session), agent.task);
        if agent.role == RoleName::Reviewer {
            self.send(
                at,
                task,
                RunEventKind::Session(SessionEvent::Input(Box::new(given_the_checks()))),
            );
        }
        for n in 0..agent.calls {
            let id = format!("{}-{n}", agent.session);
            self.send(
                at,
                task,
                RunEventKind::Agent(AgentEvent::ToolStarted {
                    id: id.clone(),
                    name: "mcp__argonauts__status".to_owned(),
                    input: json!({}),
                    parent: None,
                }),
            );
            self.send(
                at,
                task,
                RunEventKind::Agent(AgentEvent::ToolFinished {
                    id,
                    ok: true,
                    output: None,
                    parent: None,
                }),
            );
        }
        if agent.role == RoleName::Worker {
            self.send(None, task, passing_checks());
        }
        let answer = match agent.role {
            RoleName::Reviewer => json!({"accept": true, "findings": []}),
            _ => json!({"summary": "done"}),
        };
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
            result_text: Some(answer.to_string()),
            structured: Some(answer),
            last_message: None,
            raw_log: None,
        };
        self.send(
            at,
            task,
            RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
        );
    }

    /// The agent's session starts, at a council's `seat` when it sits at one.
    fn start(&mut self, agent: &Agent, seat: Option<SeatId>) {
        let worker: ModelRef = agent.worker.parse().unwrap();
        let reads = agent.role != RoleName::Worker;
        let started = SessionStarted {
            agent_id: AgentId(worker.agent.clone()),
            worker,
            provider: "provider".to_owned(),
            kind: RunnerKind::Cli,
            role: agent.role,
            step: Some(agent.step),
            access: if reads {
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
            asked_by: None,
            asked_why: None,
            forked: false,
            seat,
            billing: None,
        };
        self.send(
            Some(agent.session),
            agent.task,
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        );
    }

    /// The run declares its workflow, an orchestrator's, which prints its Workflow call.
    fn declare(&mut self) {
        let id = WorkflowId::numbered(1);
        let run = self
            .bridge
            .state
            .runs
            .get_mut(&RunId::new("run-01").unwrap())
            .expect("the run");
        run.workflows.push(WorkflowState {
            id: id.clone(),
            name: "add-due-dates".into(),
            from: Some("orchestrator".into()),
            description: String::new(),
            phases: vec!["Plan".into(), "Build".into(), "Review".into()],
            parent: None,
            steps: Vec::new(),
        });
        let phase = |index: u32, name: &str| DeclaredPhase {
            index,
            name: name.into(),
            detail: String::new(),
            standard: StandardPhase::Do,
        };
        self.send(
            None,
            None,
            RunEventKind::Workflow(WorkflowEvent::Declared(Box::new(WorkflowDeclared {
                id,
                name: "add-due-dates".into(),
                description: "Add --due to the todo CLI".into(),
                phases: vec![phase(0, "Plan"), phase(1, "Build"), phase(2, "Review")],
                required: Vec::new(),
                talks: false,
            }))),
        );
    }

    fn printed(&mut self, theme: &Theme) -> Vec<Line<'static>> {
        self.printed_at(theme, 120)
    }

    fn printed_at(&mut self, theme: &Theme, cols: u16) -> Vec<Line<'static>> {
        let ctx = RenderCtx::new(cols, theme, Clock::still(Instant::now()));
        self.model.history.drain_printable(&ctx)
    }
}

fn names() -> DisplayNames {
    let mut names = DisplayNames::default();
    for (model, name) in [
        (OPUS, "Opus 5.5"),
        (HAIKU, "Haiku 5.5"),
        (LUNA, "GPT 6 Luna"),
        (SOL, "GPT 6.1 Sol"),
        (ASTRA, "GPT 6 Astra"),
    ] {
        let mut entry = CatalogEntry::new(model.parse().unwrap());
        entry.name = Some(name.to_owned());
        names.catalog.models.push(entry);
    }
    for (cli, name) in [("claude-code", "Claude Code"), ("codex", "Codex")] {
        names.clis.insert(cli.to_owned(), name.to_owned());
    }
    names
}

/// Haiku 5.5 ran the most of Claude Code's models through Argonauts Code, Opus 5.5 the next.
fn ranked() -> UsageSummary {
    let ranked = [
        ("claude-code", "claude-haiku-5-5", 6_500_000),
        ("claude-code", "claude-opus-5-5", 1_970_000),
        ("codex", "gpt-6-luna", 1_470_000),
        ("codex", "gpt-6.1-sol", 920_000),
        ("codex", "gpt-6-astra", 610_000),
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

fn started() -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: "Add --due to the todo CLI".into(),
        title: "Add due dates".into(),
        mode: RunMode::Crew,
        forced: false,
        workflow: "orchestrator".into(),
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
        starts_from: None,
    })))
}

fn planned(task: &str, title: &str) -> RunEventKind {
    RunEventKind::Task(TaskEvent::Created(Box::new(TaskSpec {
        id: TaskId::new(task).unwrap(),
        title: title.to_owned(),
        task: title.to_owned(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(HAIKU.parse().unwrap()),
        reviewers: vec![LUNA.parse().unwrap()],
        limit: TaskLimit {
            limit: None,
            max_minutes: None,
            max_tool_calls: None,
            inactivity_min: None,
        },
        predictions: None,
        item: None,
        way: None,
    })))
}

fn passing_checks() -> RunEventKind {
    RunEventKind::Check(CheckOutcome {
        name: "tests".into(),
        kind: CheckKind::Command,
        passed: true,
        command: Some(vec!["node".into(), "--test".into()]),
        exit_code: Some(0),
        duration_ms: 30,
        summary: "14 passed".into(),
        output_tail: String::new(),
        violations: vec![],
        sandboxed: true,
        skipped: false,
    })
}

fn given_the_checks() -> SessionInput {
    SessionInput {
        launch: LaunchRecord {
            program: "codex".to_owned(),
            args: Vec::new(),
            cwd: PathBuf::from("/w"),
            env_set: Vec::new(),
            env_removed: Vec::new(),
            prompt_via: PromptVia::StreamInput,
            sandbox: None,
        },
        parts: vec![InputPart {
            kind: InputPartKind::CheckOutput,
            label: "the checks".to_owned(),
            text: "Argonauts Code ran these after the build, in the task's worktree:\n\
                   tests (node --test) passed: 14 passed"
                .to_owned(),
            approx_tokens: 1,
            source: None,
        }],
        queued: Vec::new(),
    }
}

fn orchestrator_run() -> Run {
    orchestrator_run_building_on(None, [HAIKU, HAIKU])
}

/// The run `prompt` asked for with its two builders on `builders`, the Workflow call printed
/// as it declares its workflow, before any agent ends.
fn orchestrator_run_building_on(prompt: Option<&str>, builders: [&'static str; 2]) -> Run {
    let mut run = Run::new(prompt);
    run.declare();
    let agent = |session, label, worker, (role, step), task, (calls, seconds, tokens)| Agent {
        session,
        label,
        worker,
        role,
        step,
        task,
        calls,
        seconds,
        tokens,
    };
    let planner = (RoleName::Orchestrator, StepName::Orchestrate);
    let builder = (RoleName::Worker, StepName::Build);
    let reviewer = (RoleName::Reviewer, StepName::Review);
    for one in [
        agent(
            "s-001",
            "plan:orchestrator",
            OPUS,
            planner,
            None,
            (2, 21, 54_600),
        ),
        agent(
            "s-002",
            "build:due-tests",
            builders[0],
            builder,
            Some("due-tests"),
            (6, 62, 30_300),
        ),
        agent(
            "s-003",
            "build:due-flag",
            builders[1],
            builder,
            Some("due-flag"),
            (9, 160, 22_400),
        ),
        agent(
            "s-004",
            "review:due-tests",
            LUNA,
            reviewer,
            Some("due-tests"),
            (0, 4, 9_000),
        ),
        agent(
            "s-005",
            "review:due-flag",
            LUNA,
            reviewer,
            Some("due-flag"),
            (0, 4, 9_000),
        ),
    ] {
        run.agent(&one);
    }
    run
}

fn theme(pictures: bool) -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme.logo_pictures = pictures;
    theme
}

/// The rows as text, each logo's two placeholder cells as `￼￼`.
fn as_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| match span.content.starts_with('\u{10EEEE}') {
                    true => "\u{FFFC}\u{FFFC}".to_owned(),
                    false => span.content.to_string(),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_orchestrator_runs_agents_read_as_claude_code_heads_its_agents() {
    let theme = theme(true);
    let printed = orchestrator_run().printed(&theme);
    let rows = as_text(&printed);
    let start = rows.find("● plan:orchestrator").expect("the planner's row");
    assert_eq!(
        &rows[start..],
        "● plan:orchestrator(plan the work) · \u{FFFC}\u{FFFC} Opus 5.5 on Claude Code\n\
         \x20 ⎿  Done (2 tool uses · 54.6k tokens · 21s) · report ✓\n\
         \n\
         ● build:due-tests(test --due) · \u{FFFC}\u{FFFC} Haiku 5.5 on Claude Code\n\
         \x20 ⎿  Done (6 tool uses · 30.3k tokens · 1m 2s) · report ✓ · checks ✓\n\
         \n\
         ● build:due-flag(add the --due flag) · \u{FFFC}\u{FFFC} Haiku 5.5 on Claude Code\n\
         \x20 ⎿  Done (9 tool uses · 22.4k tokens · 2m 40s) · report ✓ · checks ✓\n\
         \n\
         ● review:due-tests(review the tests) · \u{FFFC}\u{FFFC} 6 Luna on Codex\n\
         \x20 ⎿  Accepted · used the checks' result: 14 passed\n\
         \n\
         ● review:due-flag(review the flag) · \u{FFFC}\u{FFFC} 6 Luna on Codex\n\
         \x20 ⎿  Accepted · used the checks' result: 14 passed"
    );

    // Each label bold, who runs it dim but for the model in its CLI's hue: Claude Code's
    // first two models and Codex's first, by palette B, are their CLI's own hue.
    let header = |label: &str| {
        printed
            .iter()
            .find(|line| line.to_string().starts_with(&format!("● {label}")))
            .unwrap_or_else(|| panic!("no {label}"))
    };
    for (label, model, hue) in [
        ("plan:orchestrator", "Opus 5.5", Role::CliClaude),
        ("build:due-flag", "Haiku 5.5", Role::CliClaude),
        ("review:due-tests", "6 Luna", Role::CliCodex),
    ] {
        let line = header(label);
        let span = |text: &str| {
            line.spans
                .iter()
                .find(|span| span.content == text)
                .unwrap_or_else(|| panic!("no {text:?} in {line:?}"))
        };
        assert!(span(label).style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(span(model).style.fg, theme.style(hue).fg, "{label}");
        assert_eq!(span(" · ").style.fg, theme.style(Role::Dim).fg, "{label}");
        let on = line.spans.last().expect("its CLI");
        assert_eq!(on.style.fg, theme.style(Role::Dim).fg, "{label}");
    }
}

/// Where logos don't draw as pictures, the names read whole, since nothing else says who made
/// the model.
#[test]
fn without_pictures_the_names_read_whole() {
    let printed = orchestrator_run().printed(&theme(false));
    let rows = as_text(&printed);
    assert!(
        rows.contains("● review:due-flag(review the flag) · GPT 6 Luna on Codex\n"),
        "{rows}"
    );
    assert!(
        rows.contains("● plan:orchestrator(plan the work) · Opus 5.5 on Claude Code\n"),
        "{rows}"
    );
    assert!(!rows.contains('\u{FFFC}'), "{rows}");
}

/// The rows `printed` at `cols` as the top of a screen `cols` × `rows` draws them, each
/// row's own fill to the screen's edge.
fn screen_of(printed: &[Line<'_>], (cols, rows): (u16, u16)) -> Buffer {
    let area = Rect::new(0, 0, cols, printed.len().min(rows.into()) as u16);
    let mut buf = Buffer::empty(area);
    crate::terminal::paint_rows(&mut buf, area, printed);
    buf
}

/// The run as the conversation draws it at either size, from its Workflow row down.
#[test]
fn an_orchestrator_run_matches_its_drawings() {
    let theme = crate::testing::pictures::approved_screen::theme();
    for (file, size) in [
        ("conversation/orchestrator-run@120x38.txt", (120, 38)),
        ("conversation/orchestrator-run@177x40.txt", (177, 40)),
    ] {
        let printed = orchestrator_run().printed_at(&theme, size.0);
        crate::testing::pictures::approved_screen::assert_rows_match(
            file,
            0,
            &screen_of(&printed, size),
            size,
            0,
        );
    }
}

/// The run whose builders work on Codex's second and third models, your prompt above it, as
/// the conversation draws it at either size.
#[test]
fn a_run_building_on_codexs_other_models_matches_its_drawings() {
    let theme = crate::testing::pictures::approved_screen::theme();
    for (file, size) in [
        (
            "conversation/orchestrator-run-codex-builds@120x38.txt",
            (120, 38),
        ),
        (
            "conversation/orchestrator-run-codex-builds@177x40.txt",
            (177, 40),
        ),
    ] {
        let printed = orchestrator_run_building_on(
            Some("Add --due to the todo CLI with an orchestrator, Codex reviewing."),
            [SOL, ASTRA],
        )
        .printed_at(&theme, size.0);
        crate::testing::pictures::approved_screen::assert_rows_match(
            file,
            0,
            &screen_of(&printed, size),
            size,
            0,
        );
    }
}

/// Codex's second and third models building, each in Codex's hue as its first is, as the
/// drawing of that run has them.
#[test]
fn builders_on_codexs_other_models_read_in_its_hue() {
    let theme = crate::testing::pictures::approved_screen::theme();
    let printed = orchestrator_run_building_on(
        Some("Add --due to the todo CLI with an orchestrator, Codex reviewing."),
        [SOL, ASTRA],
    )
    .printed_at(&theme, 120);
    let rows = as_text(&printed);
    assert!(
        rows.starts_with("❯ Add --due to the todo CLI with an orchestrator, Codex reviewing.\n"),
        "{rows}"
    );
    for (label, model) in [
        ("build:due-tests", "6.1 Sol"),
        ("build:due-flag", "6 Astra"),
    ] {
        let line = printed
            .iter()
            .find(|line| line.to_string().starts_with(&format!("● {label}")))
            .unwrap_or_else(|| panic!("no {label}"));
        assert!(
            line.to_string().ends_with(&format!(" {model} on Codex")),
            "{line}"
        );
        let span = line
            .spans
            .iter()
            .find(|span| span.content == model)
            .unwrap_or_else(|| panic!("no {model:?} in {line:?}"));
        assert_eq!(span.style.fg, theme.style(Role::CliCodex).fg, "{label}");
    }
}

/// A council's seats start their first answers together, yet the conversation prints no
/// `agents started` tree and no rows for them: each seat's work shows in its bubble and on its
/// row under the request. Sessions that sit at no council still print theirs.
#[test]
fn seats_print_no_agent_rows_in_the_conversation() {
    let theme = theme(false);
    let agent = |session, label, worker| Agent {
        session,
        label,
        worker,
        role: RoleName::Worker,
        step: StepName::Talk,
        task: None,
        calls: 0,
        seconds: 0,
        tokens: 0,
    };
    let seats = [
        agent("s-001", "talk:anthropic", OPUS),
        agent("s-002", "talk:openai", LUNA),
    ];
    let mut run = Run::new(None);
    for (n, seat) in (1..).zip(&seats) {
        run.start(seat, Some(SeatId::numbered(n)));
    }
    run.send(
        Some("s-001"),
        None,
        RunEventKind::Agent(AgentEvent::ToolStarted {
            id: "read-1".to_owned(),
            name: "Read".to_owned(),
            input: json!({"file_path": "src/server.ts"}),
            parent: None,
        }),
    );
    let rows = as_text(&run.printed(&theme));
    assert!(!rows.contains("agents started"), "{rows}");
    assert!(!rows.contains("talk:"), "{rows}");

    let mut run = Run::new(None);
    for seat in &seats {
        run.start(seat, None);
    }
    run.send(
        Some("s-001"),
        None,
        RunEventKind::Agent(AgentEvent::ToolStarted {
            id: "read-1".to_owned(),
            name: "Read".to_owned(),
            input: json!({"file_path": "src/server.ts"}),
            parent: None,
        }),
    );
    let rows = as_text(&run.printed(&theme));
    assert!(rows.contains("2 agents started"), "{rows}");
}
