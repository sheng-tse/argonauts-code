//! Agent sessions on the main screen: live blocks, what reaches scrollback at each verbosity,
//! and every vendor's calls saying what they did. Events are built by hand from the shapes the
//! adapters' recorded fixtures produce.

mod common;

use std::collections::BTreeMap;

use argonauts_core::app::AppEvent;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::card::{CardEvent, CardState};
use argonauts_core::events::session::{
    EndReason, FileDiff, SessionEnd, SessionEvent, SessionStarted, SubagentKind, SubagentStatus,
    SubagentUpdate,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::model_catalog::{CatalogEntry, ModelCatalog};
use argonauts_core::names::{RoleName, SessionKey, VendorId, WorkerRef};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::settings::ui::{AgentsInChat, UiSettings};
use argonauts_core::steps::StepName;
use argonauts_core::worker::{Access, WorkerKind};
use argonauts_core::{
    AgentEvent, CardId, ErrorCategory, ModOutputKind, RetryReason, RunId, Spend, TodoItem,
    TodoStatus, Verbosity,
};
use argonauts_tui::HistoryCell;
use argonauts_tui::bridge::agents::AgentBlocks;
use argonauts_tui::bridge::{Cx, Effects};
use argonauts_tui::model::ViewModel;
use argonauts_tui::render::{Clock, RenderCtx};
use argonauts_tui::stability::{self, samples};
use argonauts_tui::state::{
    AppState, CardRow, Doing, Replay, RunState, SessionState, SessionStatus,
};
use argonauts_tui::theme::Role;
use common::{assert_fits, at, style_of, t0, text, theme};
use ratatui::text::Line;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;

const WIDTHS: [u16; 2] = [80, 100];

const START: OffsetDateTime = datetime!(2026-09-30 10:00 UTC);

/// A worktree path as Claude Code reports it, which calls show from inside the worktree.
const WORKTREE: &str = "/Users/me/.argonauts/worktrees/repo-1/run-01/task-003";

/// A run's events driven through the agents part of the bridge, with a clock in seconds.
struct Crew {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    blocks: AgentBlocks,
    run: RunId,
    seq: u64,
    now: f64,
    /// Anything rang the bell.
    rang: bool,
}

impl Crew {
    fn new(verbosity: Verbosity) -> Self {
        let run = RunId::at(START, 1);
        let mut state = AppState::default();
        state.runs.insert(run.clone(), RunState::new(run.clone()));
        // Most of these draw the live block, which agents have when they show as they work.
        let settings = UiSettings {
            verbosity,
            agents_in_chat: AgentsInChat::AsTheyWork,
            ..UiSettings::default()
        };
        Self {
            model: ViewModel::new("crew"),
            state,
            settings,
            blocks: AgentBlocks::default(),
            run,
            seq: 0,
            now: 0.0,
            rang: false,
        }
    }

    fn run_state(&mut self) -> &mut RunState {
        self.state.runs.get_mut(&self.run).expect("the run")
    }

    fn card(&mut self, n: u32, title: &str, state: Option<CardState>) {
        let id = CardId::numbered(n);
        self.run_state().cards.insert(
            id.clone(),
            CardRow {
                id,
                title: title.to_owned(),
                state,
                owner: None,
                attempt: 1,
                diff: None,
                standing_in_for: None,
                debt: Vec::new(),
                checkpoints: 0,
                rotations: 0,
                last_gate: None,
                reviews: 0,
                waiting_on_vendor: None,
                item: None,
            },
        );
    }

    fn at(&mut self, seconds: f64) -> &mut Self {
        self.now = seconds;
        self
    }

    fn send(&mut self, session: &str, card: Option<u32>, kind: RunEventKind) {
        self.seq += 1;
        let event = RunEvent {
            seq: self.seq,
            ts: START + time::Duration::seconds_f64(self.now),
            run_id: self.run.clone(),
            card_id: card.map(CardId::numbered),
            session: SessionKey::parse(session),
            source: "test".to_owned(),
            kind,
        };
        self.app(&AppEvent::Run {
            event: Box::new(event),
        });
    }

    fn app(&mut self, event: &AppEvent) {
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: at(self.now),
            out: &mut out,
        };
        self.blocks.on_event(event, &mut cx);
        self.rang |= out.bell;
    }

    /// The app's state gets the session first, as `AppState::apply` does before the parts run.
    fn start(&mut self, session: &str, worker: &str, card: Option<u32>) {
        self.start_step(session, worker, card, None);
    }

    fn start_step(
        &mut self,
        session: &str,
        worker: &str,
        card: Option<u32>,
        step: Option<StepName>,
    ) {
        let worker: WorkerRef = worker.parse().expect("a worker");
        let key = SessionKey::parse(session).expect("a key");
        let ts = START + time::Duration::seconds_f64(self.now);
        let mut state = session_state(&key, &worker, card.map(CardId::numbered), ts);
        state.step = step;
        let run = self.run_state();
        run.sessions.insert(key.clone(), state);
        run.session_order.push(key);
        let started = SessionStarted {
            vendor: VendorId(worker.worker.clone()),
            worker,
            family: "test".to_owned(),
            kind: WorkerKind::Harness,
            role: RoleName::Worker,
            step,
            access: Access::Build,
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: None,
            workflow_step: None,
            label: None,
            asked: None,
        };
        self.send(
            session,
            card,
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        );
    }

    /// A reviewer asked through Argonauts' tool: tied to no task and no step of its own.
    fn start_asked(&mut self, session: &str, worker: &str) {
        let worker: WorkerRef = worker.parse().expect("a worker");
        let key = SessionKey::parse(session).expect("a key");
        let ts = START + time::Duration::seconds_f64(self.now);
        let mut state = session_state(&key, &worker, None, ts);
        state.role = RoleName::Reviewer;
        let run = self.run_state();
        run.sessions.insert(key.clone(), state);
        run.session_order.push(key);
        let started = SessionStarted {
            vendor: VendorId(worker.worker.clone()),
            worker,
            family: "test".to_owned(),
            kind: WorkerKind::Harness,
            role: RoleName::Reviewer,
            step: None,
            access: Access::ReadOnly,
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: None,
            workflow_step: None,
            label: Some("review:due-dates".to_owned()),
            asked: None,
        };
        self.send(
            session,
            None,
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        );
    }

    fn tokens(&mut self, session: &str, tokens: u64) {
        let key = SessionKey::parse(session).expect("a key");
        if let Some(state) = self.run_state().sessions.get_mut(&key) {
            state.tokens_out = tokens;
        }
    }

    fn agent(&mut self, session: &str, event: AgentEvent) -> &mut Self {
        self.send(session, None, RunEventKind::Agent(event));
        self
    }

    fn tool(&mut self, session: &str, id: &str, name: &str, input: Value) -> &mut Self {
        self.agent(
            session,
            AgentEvent::ToolStarted {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
                parent: None,
            },
        )
    }

    fn done(&mut self, session: &str, id: &str, ok: bool, output: Option<&str>) -> &mut Self {
        self.agent(
            session,
            AgentEvent::ToolFinished {
                id: id.to_owned(),
                ok,
                output: output.map(str::to_owned),
                parent: None,
            },
        )
    }

    fn message(&mut self, session: &str, text: &str) -> &mut Self {
        self.agent(
            session,
            AgentEvent::Message {
                text: text.to_owned(),
                parent: None,
            },
        )
    }

    fn end(&mut self, session: &str, reason: EndReason, seconds: u64, tokens: u64) {
        let end = SessionEnd {
            vendor_session: None,
            exit_code: Some(0),
            signal: None,
            duration_ms: seconds * 1000,
            reason,
            spend: Spend {
                input_tokens: tokens * 3 / 4,
                output_tokens: tokens / 4,
                ..Spend::default()
            },
            tool_calls: 0,
            result_text: Some("done".to_owned()),
            structured: Some(json!({"status": "done"})),
            last_message: None,
            raw_log: None,
        };
        self.send(
            session,
            None,
            RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
        );
    }

    /// The live region as drawn, each block after a blank row.
    fn live(&self, width: u16) -> Vec<Line<'static>> {
        let theme = theme();
        let ctx = RenderCtx::new(width, &theme, Clock::new(t0(), at(self.now)));
        let mut lines = Vec::new();
        for block in self.model.history.live_blocks(&ctx) {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(block.lines);
        }
        lines
    }

    /// Scrollback, one blank row between items.
    fn printed(&self, width: u16) -> Vec<Line<'static>> {
        let theme = theme();
        let ctx = RenderCtx::new(width, &theme, Clock::still(at(self.now)));
        let mut lines = Vec::new();
        for cell in self.model.history.cells() {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(cell.lines(&ctx));
        }
        lines
    }
}

fn session_state(
    key: &SessionKey,
    worker: &WorkerRef,
    card: Option<CardId>,
    at: OffsetDateTime,
) -> SessionState {
    SessionState {
        key: key.clone(),
        follow: None,
        worker: worker.clone(),
        model_assumed: false,
        vendor: worker.worker.clone(),
        role: RoleName::Worker,
        step: None,
        card,
        workflow_step: None,
        label: None,
        status: SessionStatus::Working,
        started: at,
        ended: None,
        last_event: at,
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: None,
        context: None,
        context_window: None,
        rotations: 0,
        todos: Vec::new(),
        tools: Vec::new(),
        messages: Vec::new(),
        streaming: String::new(),
        diffs: Vec::new(),
        patches: Vec::new(),
        subagents: Vec::new(),
        inputs: Vec::new(),
        queued: Vec::new(),
        retry: None,
        standing_in_for: None,
        end: None,
        unknown: Vec::new(),
        doing: Doing::Nothing,
        stalled: false,
        vendor_session: None,
        workflows: Vec::new(),
    }
}

fn snapshot(name: &str, lines: &[Line<'_>], width: u16) {
    assert!(!lines.is_empty(), "{name} drew nothing at {width}");
    assert_fits(lines, width);
    insta::assert_snapshot!(format!("{name}_{width}"), text(lines));
}

/// Claude Code's calls as its stream reports them: a read, an edit, a failing test run, a
/// search, and a write.
fn claude_calls(crew: &mut Crew) {
    let numbered: String = (1..=120).map(|n| format!("{n:>6}→line {n}\n")).collect();
    let test_output: String = std::iter::once("Exit code 101".to_owned())
        .chain((1..=15).map(|n| format!("test export::case_{n} ... ok")))
        .collect::<Vec<_>>()
        .join("\n");
    crew.at(10.0)
        .tool("s-001", "t1", "Read", json!({"file_path": format!("{WORKTREE}/src/app.rs")}))
        .done("s-001", "t1", true, Some(&numbered))
        .tool(
            "s-001",
            "t2",
            "Edit",
            json!({
                "file_path": format!("{WORKTREE}/src/app.rs"),
                "old_string": "    let area = frame.area();\n    draw_header(frame);\n",
                "new_string": "    let area = frame.area();\n    draw_header(frame, &state);\n    draw_prompt(frame, &state);\n",
            }),
        )
        .done("s-001", "t2", true, Some("The file has been updated."))
        .tool("s-001", "t3", "Bash", json!({"command": "cargo test export", "description": "tests"}))
        .done("s-001", "t3", false, Some(&test_output))
        .tool("s-001", "t4", "Grep", json!({"pattern": "fn draw_prompt", "path": "src"}))
        .done("s-001", "t4", true, Some("src/app.rs:88:fn draw_prompt(frame: &mut Frame) {"))
        .tool("s-001", "t5", "Write", json!({"file_path": format!("{WORKTREE}/hello.txt"), "content": "hi\n"}))
        .done("s-001", "t5", true, Some("File created successfully at: hello.txt"));
}

#[test]
fn tool_calls_read_update_bash_search_write() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, "Middleware", Some(CardState::Running));
    crew.start("s-001", "claude-code/opus", Some(3));
    claude_calls(&mut crew);
    crew.message("s-001", "The prompt draws under the header now.");
    for width in WIDTHS {
        let lines = crew.printed(width);
        snapshot("tool_calls_read_update_bash_search_write", &lines, width);
        let all = text(&lines);
        for said in [
            "Read 120 lines",
            "Added 2 lines, removed 1 line",
            "… +10 lines (ctrl+o to expand)",
            "Error: Exit code 101",
            "Found 1 match",
            "Wrote 1 line to hello.txt",
        ] {
            assert!(all.contains(said), "missing {said:?} in\n{all}");
        }
    }
    let lines = crew.printed(80);
    assert_eq!(
        style_of(&lines, "Exit code 101").and_then(|s| s.fg),
        theme().style(Role::Error).fg
    );
}

#[test]
fn live_block_all_parts() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, "Middleware", Some(CardState::Running));
    crew.start("s-001", "claude-code/opus", Some(3));
    claude_calls(&mut crew);
    crew.at(200.0).send(
        "s-001",
        Some(3),
        RunEventKind::Session(SessionEvent::FileDiff(FileDiff {
            path: "src/middleware.rs".to_owned(),
            added: 2,
            removed: 1,
            hunk: "@@ -41,2 +41,3 @@\n     fn wire(&self) {\n-        self.old();\n+        self.auth();\n+        self.log();\n".to_owned(),
            truncated: false,
        })),
    );
    crew.agent(
        "s-001",
        AgentEvent::Todo {
            items: vec![
                TodoItem {
                    text: "wire middleware".to_owned(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    text: "tests".to_owned(),
                    status: TodoStatus::InProgress,
                },
            ],
        },
    );
    crew.agent(
        "s-001",
        AgentEvent::TextDelta {
            text: "Now the middleware runs first.\nNext I'll add the tests for it".to_owned(),
            parent: None,
        },
    );
    crew.tokens("s-001", 18_100);
    for (seconds, delay) in [(250.0, 5_000), (252.0, 12_000)] {
        crew.at(seconds).agent(
            "s-001",
            AgentEvent::Retry {
                reason: RetryReason::Overloaded,
                delay_ms: Some(delay),
            },
        );
    }
    crew.at(252.0);
    for width in WIDTHS {
        let lines = crew.live(width);
        snapshot("live_block_all_parts", &lines, width);
    }
    let wide = text(&crew.live(140));
    assert!(
        wide.starts_with(
            "● Opus on Claude Code · worker · task-003 (Middleware) · 4m12s · ↓ 18.1k · retrying in 12s (attempt 2)"
        ),
        "{wide}"
    );
    for part in [
        "Added 2 lines, removed 1 line",
        "Next I'll add the tests for it▎",
        "☒ wire middleware  ☐ tests",
    ] {
        assert!(wide.contains(part), "missing {part:?} in\n{wide}");
    }
    let lines = crew.live(140);
    assert_eq!(
        style_of(&lines, "retrying").and_then(|s| s.fg),
        theme().style(Role::Warning).fg
    );

    // Any progress clears the retry, and nothing of the block went to scrollback line by line.
    crew.at(255.0)
        .tool("s-001", "t6", "Read", json!({"file_path": "src/lib.rs"}));
    assert!(!text(&crew.live(140)).contains("retrying"));
    assert!(!text(&crew.printed(140)).contains("Next I'll add"));
}

/// Claude Code reports each edit's patch, which its call draws. The change Argonauts sees in the
/// file after it is the same edit, so the live block draws it once.
#[test]
fn an_edit_its_call_drew_is_not_drawn_again_under_the_calls() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, "Middleware", Some(CardState::Running));
    crew.start("s-001", "claude-code/opus", Some(3));
    let file = format!("{WORKTREE}/src/config.js");
    crew.at(10.0).tool(
        "s-001",
        "t1",
        "Edit",
        json!({"file_path": file, "old_string": "const pkg = \"api\";", "new_string": "const pkg = \"@acme/api\";"}),
    );
    let hunk = "@@ -2,3 +2,3 @@\n const registry = \"https://registry.npmjs.org\";\n-const pkg = \"api\";\n+const pkg = \"@acme/api\";\n \n";
    crew.agent(
        "s-001",
        AgentEvent::FilePatch {
            id: "t1".to_owned(),
            path: file.clone(),
            hunks: hunk.to_owned(),
            parent: None,
        },
    );
    crew.done("s-001", "t1", true, Some("The file has been updated."));
    crew.at(12.0).send(
        "s-001",
        Some(3),
        RunEventKind::Session(SessionEvent::FileDiff(FileDiff {
            path: "src/config.js".to_owned(),
            added: 1,
            removed: 1,
            hunk: hunk.to_owned(),
            truncated: false,
        })),
    );
    let live = text(&crew.live(120));
    assert_eq!(
        live.matches("Added 1 line, removed 1 line").count(),
        1,
        "{live}"
    );
    assert_eq!(live.matches("+const pkg").count(), 1, "{live}");

    // A file no call drew still gets its preview.
    crew.at(14.0).send(
        "s-001",
        Some(3),
        RunEventKind::Session(SessionEvent::FileDiff(FileDiff {
            path: "src/other.js".to_owned(),
            added: 1,
            removed: 0,
            hunk: "@@ -1,0 +1,1 @@\n+export const other = 1;\n".to_owned(),
            truncated: false,
        })),
    );
    let live = text(&crew.live(120));
    assert!(live.contains("export const other = 1;"), "{live}");
}

/// A CLI that missed its heartbeat may yet answer, so the stall reads as a dim note while an
/// error keeps the error color.
#[test]
fn a_stall_reads_dim_and_an_error_red() {
    let mut crew = Crew::new(Verbosity::Normal);
    crew.start("s-001", "claude-code/opus", None);
    let stall = "Claude Code has sent nothing for 45 seconds, not even its heartbeat";
    crew.agent(
        "s-001",
        AgentEvent::Error {
            category: ErrorCategory::Stalled,
            message: stall.to_owned(),
            retryable: true,
        },
    );
    crew.agent(
        "s-001",
        AgentEvent::Error {
            category: ErrorCategory::Network,
            message: "the stream broke".to_owned(),
            retryable: true,
        },
    );
    let lines = crew.live(100);
    assert_eq!(
        style_of(&lines, "sent nothing for 45 seconds").and_then(|s| s.fg),
        theme().style(Role::Dim).fg,
        "{}",
        text(&lines)
    );
    assert_eq!(
        style_of(&lines, "the stream broke").and_then(|s| s.fg),
        theme().style(Role::Error).fg
    );
}

/// The shape in which Codex passes on the service turning down an answer format.
const CODEX_SCHEMA_ERROR: &str = r#"{"type":"error","message":"{\"error\":{\"message\":\"Invalid schema for response_format 'codex_output_schema': Missing 'summary'.\",\"type\":\"invalid_request_error\",\"code\":\"invalid_json_schema\"}}"}"#;

/// An agent's error and the failed end that follows say it once, in words, with `r` for the
/// CLI's own text; its JSON never reaches the chat.
#[test]
fn an_agent_error_then_its_failed_end_say_it_once_in_words() {
    for width in WIDTHS {
        let mut crew = Crew::new(Verbosity::Normal);
        crew.start("s-001", "codex/gpt-5.6", None);
        crew.at(1.0).agent(
            "s-001",
            AgentEvent::Error {
                category: ErrorCategory::InvalidRequest,
                message: CODEX_SCHEMA_ERROR.to_owned(),
                retryable: false,
            },
        );
        let reason = EndReason::VendorError {
            category: ErrorCategory::InvalidRequest,
            message: CODEX_SCHEMA_ERROR.to_owned(),
        };
        crew.at(2.0).end("s-001", reason, 2, 0);
        let printed = text(&scrollback(&mut crew, width))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let said = "Codex refused the answer format Argonauts asked for";
        assert_eq!(printed.matches(said).count(), 1, "{width}: {printed}");
        assert_eq!(
            printed.matches("r shows the raw error").count(),
            1,
            "{printed}"
        );
        assert!(!printed.contains("invalid_json_schema"), "{printed}");
        assert!(!printed.contains('{'), "{printed}");
        let raw = crew
            .model
            .raw_failure
            .as_ref()
            .expect("the raw text is kept");
        assert_eq!(raw.text, CODEX_SCHEMA_ERROR);
        assert!(raw.title.ends_with("· the raw error"), "{}", raw.title);
    }
}

#[test]
fn a_block_that_doesnt_fit_keeps_its_header_todo_and_text() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, "Middleware", None);
    crew.start("s-001", "claude-code/opus", Some(3));
    claude_calls(&mut crew);
    crew.agent(
        "s-001",
        AgentEvent::Todo {
            items: vec![TodoItem {
                text: "tests".to_owned(),
                status: TodoStatus::InProgress,
            }],
        },
    );
    crew.agent(
        "s-001",
        AgentEvent::TextDelta {
            text: "Writing the tests".to_owned(),
            parent: None,
        },
    );
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::new(t0(), at(60.0)));
    let (_, cell) = crew.model.history.active_cells().next().expect("a block");
    let lines = cell.lines_within(&ctx, 5);
    assert_eq!(lines.len(), 5, "{}", text(&lines));
    let squeezed = text(&lines);
    assert!(squeezed.starts_with("● Opus on Claude Code"), "{squeezed}");
    assert!(squeezed.contains("Writing the tests▎"), "{squeezed}");
    assert!(squeezed.ends_with("☐ tests"), "{squeezed}");
}

#[test]
fn codex_block_draws_its_thread_without_a_note() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(2, "Redis store", Some(CardState::Running));
    crew.start("s-002", "codex/gpt-5.6", Some(2));
    crew.at(5.0)
        .tool(
            "s-002",
            "item_1",
            "command_execution",
            json!({"command": "bash -lc ls"}),
        )
        .done("s-002", "item_1", true, Some("docs\nsdk\n"))
        .tool(
            "s-002",
            "child-1",
            "spawn_agent",
            json!({"prompt": "look around", "receiver_thread_ids": ["child-1"]}),
        )
        .done("s-002", "child-1", true, None)
        .agent(
            "s-002",
            AgentEvent::Subagent {
                update: SubagentUpdate {
                    id: "child-1".to_owned(),
                    parent: None,
                    kind: SubagentKind::Thread,
                    label: Some("explorer".to_owned()),
                    prompt: Some("look around".to_owned()),
                    status: SubagentStatus::Running,
                    last_message: Some("Reading the store's tests".to_owned()),
                    log_uri: None,
                    steps_visible: false,
                    tokens: None,
                },
            },
        )
        .message("s-002", "The store needs a connection pool first.");
    crew.tokens("s-002", 12_900);
    crew.at(220.0);
    for width in WIDTHS {
        snapshot(
            "codex_block_draws_its_thread_without_a_note",
            &crew.live(width),
            width,
        );
    }
    let live = text(&crew.live(100));
    assert!(!live.contains("Messages arrive whole"), "{live}");
    assert!(
        live.contains("Running · Reading the store's tests"),
        "{live}"
    );
    assert!(
        live.contains("The store needs a connection pool first."),
        "{live}"
    );
    assert!(
        !live.contains('▎'),
        "Codex doesn't stream, so no cursor:\n{live}"
    );
}

/// A step that ends in a structured result, as each vendor sends it: Codex's last message
/// (which its adapter turns into a report call), Claude Code's `StructuredOutput`, agy's `finish`.
fn reporting_session(worker: &str, report_call: &str) -> Crew {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(2, "Hello file", Some(CardState::Running));
    crew.start("s-002", worker, Some(2));
    crew.at(4.0)
        .message("s-002", "Creating hello.txt now.")
        .tool(
            "s-002",
            "item_1",
            "file_change",
            json!({"changes": [{"path": "hello.txt", "kind": "add"}]}),
        )
        .done("s-002", "item_1", true, None)
        .tool(
            "s-002",
            "item_2",
            report_call,
            json!({
                "status": "done",
                "summary": "Created hello.txt with a greeting.\nNothing else changed.",
                "files_changed": ["hello.txt"],
                "tests_run": [],
                "concerns": []
            }),
        )
        .done("s-002", "item_2", true, None);
    crew.at(30.0).end("s-002", EndReason::Completed, 30, 28_000);
    crew
}

/// What reaches scrollback, without what's kept for the overlay alone.
fn scrollback(crew: &mut Crew, width: u16) -> Vec<Line<'static>> {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(at(crew.now)));
    crew.model.history.drain_printable(&ctx)
}

#[test]
fn a_structured_result_shows_as_its_summary() {
    for width in WIDTHS {
        let mut crew = reporting_session("codex/gpt-5.6", "structured_output");
        let lines = scrollback(&mut crew, width);
        assert_fits(&lines, width);
        snapshot("a_structured_result_shows_as_its_summary", &lines, width);
    }
    let cases = [
        ("codex/gpt-5.6", "structured_output"),
        ("claude-code/opus", "StructuredOutput"),
        ("antigravity-cli/gemini-3.6-flash", "finish"),
    ];
    for (worker, name) in cases {
        let mut crew = reporting_session(worker, name);
        let scrollback = text(&scrollback(&mut crew, 100));
        assert!(
            scrollback.contains("Created hello.txt with a greeting."),
            "{worker}: {scrollback}"
        );
        assert!(!scrollback.contains("Nothing else changed"), "{scrollback}");
        assert!(
            !scrollback.contains("files_changed"),
            "{worker}: {scrollback}"
        );
        assert!(!scrollback.contains("Report"), "{worker}: {scrollback}");

        let theme = theme();
        let overlay = RenderCtx::new(100, &theme, Clock::still(at(crew.now))).with_expanded(true);
        let detail: String = crew
            .model
            .history
            .cells()
            .iter()
            .map(|cell| text(&cell.lines(&overlay)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(detail.contains("Report(done)"), "{worker}: {detail}");
        assert!(
            detail.contains(r#""files_changed": ["#),
            "the JSON stays in Ctrl+O for {worker}: {detail}"
        );
    }
}

/// Claude Code wrote its report out as fenced JSON, then called the report tool with it. The
/// transcript reads its words and the summary once, and the JSON stays in Ctrl+O.
#[test]
fn a_report_written_out_and_called_shows_once() {
    let report = json!({
        "status": "done",
        "summary": "Created hello.txt with a greeting.",
        "files_changed": ["hello.txt"],
        "tests_run": [],
        "concerns": []
    });
    let pretty = serde_json::to_string_pretty(&report).unwrap();
    let mut crew = Crew::new(Verbosity::Normal);
    crew.card(2, "Hello file", Some(CardState::Running));
    crew.start("s-002", "claude-code/opus", Some(2));
    crew.at(4.0)
        .message("s-002", &format!("Done\n\n```json\n{pretty}\n```"))
        .tool("s-002", "t1", "StructuredOutput", report)
        .done("s-002", "t1", true, None);
    crew.at(30.0).end("s-002", EndReason::Completed, 30, 28_000);
    let scrollback = text(&scrollback(&mut crew, 100));
    assert_eq!(
        scrollback
            .matches("Created hello.txt with a greeting.")
            .count(),
        1,
        "{scrollback}"
    );
    for json in ["```", "\"status\"", "files_changed"] {
        assert!(!scrollback.contains(json), "{json} in:\n{scrollback}");
    }
}

/// One session's scrollback: reads and a shell command, an edit, a message, and its end.
fn one_session(verbosity: Verbosity) -> Crew {
    one_session_with(Crew::new(verbosity))
}

fn one_session_with(mut crew: Crew) -> Crew {
    crew.card(3, "Middleware", Some(CardState::InReview));
    crew.start("s-001", "claude-code/opus", Some(3));
    crew.at(12.0)
        .tool("s-001", "t1", "Read", json!({"file_path": "src/app.rs"}))
        .done("s-001", "t1", true, Some("fn main() {}\n"))
        .tool("s-001", "t2", "Read", json!({"file_path": "src/lib.rs"}))
        .done("s-001", "t2", true, Some("pub mod app;\npub mod middleware;\n"))
        .tool("s-001", "t3", "Bash", json!({"command": "cargo build"}))
        .done("s-001", "t3", true, Some("Finished `dev` profile in 2.41s"))
        .tool(
            "s-001",
            "t4",
            "Edit",
            json!({"file_path": "src/middleware.rs", "old_string": "old();\n", "new_string": "auth();\n"}),
        )
        .done("s-001", "t4", true, Some("ok"))
        .message("s-001", "Wired the middleware in front of the router.");
    crew.at(580.0)
        .end("s-001", EndReason::Completed, 580, 61_000);
    crew
}

#[test]
fn verbosity_full_normal_quiet() {
    for width in WIDTHS {
        let mut sections = Vec::new();
        for level in [Verbosity::Full, Verbosity::Normal, Verbosity::Quiet] {
            let crew = one_session(level);
            let lines = crew.printed(width);
            assert_fits(&lines, width);
            assert!(
                crew.live(width).is_empty(),
                "the block left the live region"
            );
            sections.push(format!("── {level:?} ──\n{}", text(&lines)));
        }
        insta::assert_snapshot!(
            format!("verbosity_full_normal_quiet_{width}"),
            sections.join("\n\n")
        );
    }
    let full = text(&one_session(Verbosity::Full).printed(100));
    assert!(full.contains("⎿  Read(src/app.rs)"), "{full}");
    assert!(
        full.ends_with("⎿  Done · 4 tool uses · report ✓ · checks ✓ · 9m 40s · 61k tokens"),
        "{full}"
    );
    let normal = text(&one_session(Verbosity::Normal).printed(100));
    assert!(
        normal.contains("Read 2 files, ran 1 shell command"),
        "{normal}"
    );
    assert!(!normal.contains("Read(src/app.rs)"), "{normal}");
    assert!(
        normal.ends_with("⎿  Done · 4 tool uses · report ✓ · checks ✓ · 9m 40s · 61k tokens"),
        "{normal}"
    );
    assert!(one_session(Verbosity::Quiet).printed(100).is_empty());
}

/// Every Claude Code session in the crew loads your mods, so what they push stays in each
/// agent's block, dim but for a warning, never on the notice line or with the bell. A mod that
/// says the same again shows it once.
#[test]
fn a_crew_sessions_mod_pushes_stay_in_its_block() {
    let push = |kind, plugin: &str, text: &str| AgentEvent::ModOutput {
        kind,
        plugin: Some(plugin.to_owned()),
        text: Some(text.to_owned()),
        timeout_ms: None,
    };
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, "Middleware", Some(CardState::Running));
    crew.start("s-001", "claude-code/opus", Some(3));
    for _ in 0..3 {
        crew.at(5.0)
            .agent("s-001", push(ModOutputKind::Toast, "jokes", "Knock knock"));
    }
    crew.agent("s-001", push(ModOutputKind::Urgent, "pager", "Build broke"));
    crew.agent("s-001", push(ModOutputKind::Status, "pager", "2 open"));
    assert!(crew.model.bottom.mode.notice.is_none());
    assert!(!crew.rang);
    let shown = text(&crew.live(100));
    assert_eq!(shown.matches("jokes: Knock knock").count(), 1, "{shown}");
    assert!(shown.contains("pager: Build broke"), "{shown}");
    assert!(!shown.contains("2 open"), "{shown}");
}

#[test]
fn agents_started_rows() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(2, "Redis store", None);
    crew.card(3, "Middleware", None);
    crew.card(4, "Docs", None);
    crew.start("s-001", "codex/gpt-5.6", Some(2));
    crew.start("s-002", "claude-code/opus", Some(3));
    crew.at(1.0)
        .start("s-003", "antigravity-cli/gemini-3.6-flash", Some(4));
    assert!(
        crew.printed(80).is_empty(),
        "the tree waits for the last start"
    );
    crew.at(3.0)
        .tool("s-001", "t1", "command_execution", json!({"command": "ls"}));
    for width in WIDTHS {
        snapshot("agents_started_rows", &crew.printed(width), width);
    }
    assert_eq!(
        text(&crew.printed(100)),
        "● 3 agents started (↓ to manage)\n   ├ GPT 5.6 on Codex · worker · task-002 Redis store\n   ├ Opus on Claude Code · worker · task-003 Middleware\n   └ Gemini 3.6 Flash on Antigravity CLI · worker · task-004 Docs"
    );
    assert_eq!(crew.model.history.active_cells().count(), 3);

    // One agent on its own gets only its live block.
    let mut solo = Crew::new(Verbosity::Full);
    solo.start("s-001", "codex/gpt-5.6", None);
    solo.at(3.0).message("s-001", "hi");
    assert!(!text(&solo.printed(80)).contains("started"));
}

#[test]
fn agents_started_between_card_events() {
    // A conductor that makes each card and then starts its agent.
    let mut crew = Crew::new(Verbosity::Full);
    let cards = [
        (2, "Redis store", "s-001", "codex/gpt-5.6"),
        (3, "Middleware", "s-002", "claude-code/opus"),
        (4, "Docs", "s-003", "antigravity-cli/gemini-3.6-flash"),
    ];
    for (n, title, session, worker) in cards {
        crew.card(n, title, None);
        let made = CardEvent::State {
            from: None,
            to: CardState::Ready,
            reason: String::new(),
        };
        crew.send("", Some(n), RunEventKind::Card(made));
        crew.start(session, worker, Some(n));
    }
    crew.at(3.0)
        .tool("s-001", "t1", "command_execution", json!({"command": "ls"}));
    assert_eq!(
        text(&crew.printed(100)),
        "● 3 agents started (↓ to manage)\n   ├ GPT 5.6 on Codex · worker · task-002 Redis store\n   ├ Opus on Claude Code · worker · task-003 Middleware\n   └ Gemini 3.6 Flash on Antigravity CLI · worker · task-004 Docs"
    );
}

/// Owners forecast their cards before you approve the plan, and that mustn't read as building.
#[test]
fn an_owner_forecasting_its_card_is_an_estimator() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(1, "Limiter interface", None);
    crew.card(2, "Redis store", None);
    crew.start_step("s-001", "codex/gpt-5.6", Some(1), Some(StepName::Predict));
    crew.start_step(
        "s-002",
        "claude-code/opus",
        Some(2),
        Some(StepName::Predict),
    );
    crew.at(3.0).end("s-001", EndReason::Completed, 3, 180);
    let printed = text(&crew.printed(100));
    assert!(
        printed.starts_with(
            "● 2 agents started (↓ to manage)\n   ├ GPT 5.6 on Codex · estimator · task-001 Limiter interface\n   └ Opus on Claude Code · estimator · task-002 Redis store"
        ),
        "{printed}"
    );
    assert!(
        printed
            .contains("● GPT 5.6 on Codex · estimator · task-001 (Limiter interface)\n  ⎿  Done"),
        "{printed}"
    );
    assert!(!printed.contains("worker"), "{printed}");
}

#[test]
fn stopped_agent_prints_one_line() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, "Middleware", None);
    crew.start("s-001", "claude-code/opus", Some(3));
    crew.at(20.0)
        .tool("s-001", "t1", "Read", json!({"file_path": "a.rs"}))
        .done("s-001", "t1", true, Some("x"))
        .tool("s-001", "t2", "Bash", json!({"command": "cargo test"}));
    crew.at(123.0).end("s-001", EndReason::Stopped, 123, 12_000);
    let printed = text(&crew.printed(80));
    assert_eq!(
        printed,
        "● Opus on Claude Code · worker · task-003 (Middleware)\n  ⎿  Stopped · 2 tool uses · 2m 3s · 12k tokens"
    );
    assert_eq!(crew.model.history.cells().len(), 1);
    assert_eq!(crew.model.history.active_cells().count(), 0);
    // The overlay still has every call, the one it was running marked stopped.
    let theme = theme();
    let overlay = RenderCtx::new(80, &theme, Clock::still(t0())).with_expanded(true);
    let whole = text(&crew.model.history.cells()[0].lines(&overlay));
    assert!(
        whole.contains("Read(a.rs)") && whole.contains("Bash(cargo test)"),
        "{whole}"
    );
    // The overlay is the detail view, so the stamp over the block keeps the id.
    let stamp = whole.lines().next().unwrap_or_default();
    assert!(stamp.trim_end().ends_with(" claude-code/opus"), "{whole}");
}

/// With the app's catalog, an alias reads as the model it runs, everywhere the agent shows.
#[test]
fn an_agent_is_named_from_the_catalog() {
    let mut opus = CatalogEntry::new("claude-code/claude-opus-5-5".parse().unwrap());
    opus.aliases = Some(vec!["opus".into()]);
    let mut crew = Crew::new(Verbosity::Full);
    crew.state.names = DisplayNames {
        catalog: ModelCatalog {
            models: vec![opus],
            ..ModelCatalog::default()
        },
        ..DisplayNames::default()
    };
    crew.card(3, "Middleware", None);
    crew.start("s-001", "claude-code/opus", Some(3));
    crew.at(123.0)
        .end("s-001", EndReason::Completed, 123, 12_000);
    let printed = text(&crew.printed(80));
    assert!(
        printed.starts_with("● Opus 5.5 on Claude Code · worker · task-003 (Middleware)"),
        "{printed}"
    );
}

/// An agent has one name: from its start, the model its CLI's settings give it, else the model
/// it reports at its start, in its block and in the started rows alike.
#[test]
fn an_agent_keeps_one_name_from_its_start() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.state.names = DisplayNames {
        configured: BTreeMap::from([("codex".to_owned(), "gpt-5.6-luna".to_owned())]),
        ..DisplayNames::default()
    };
    crew.card(2, "Redis store", None);
    crew.card(3, "Middleware", None);
    crew.start("s-001", "codex", Some(2));
    crew.start("s-002", "claude-code", Some(3));
    // Claude Code runs its own default and says which at its start; the app's state takes it
    // in before the blocks hear of it.
    let key = SessionKey::parse("s-002").unwrap();
    crew.run_state().sessions.get_mut(&key).unwrap().worker =
        "claude-code/claude-haiku-4-5".parse().unwrap();
    crew.at(0.5).agent(
        "s-002",
        AgentEvent::Init {
            session_id: Some("claude-1".into()),
            model: Some("claude-haiku-4-5".into()),
            tools: Vec::new(),
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        },
    );
    let live = text(&crew.live(100));
    assert!(
        live.contains("● Haiku 4.5 on Claude Code · worker"),
        "{live}"
    );
    assert!(live.contains("● GPT 5.6 Luna on Codex · worker"), "{live}");
    crew.at(3.0)
        .tool("s-001", "t1", "command_execution", json!({"command": "ls"}));
    let printed = text(&crew.printed(100));
    assert!(
        printed.contains("├ GPT 5.6 Luna on Codex · worker · task-002 Redis store"),
        "{printed}"
    );
    assert!(
        printed.contains("└ Haiku 4.5 on Claude Code · worker · task-003 Middleware"),
        "{printed}"
    );
}

/// A CLI running its own default is named by it from its start, so the started rows, which can
/// print before it says which model it runs, name it as its block does.
#[test]
fn an_agent_on_its_clis_default_is_named_by_it_from_its_start() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.state.names = DisplayNames {
        defaults: BTreeMap::from([("claude-code".to_owned(), "claude-opus-5-5".to_owned())]),
        ..DisplayNames::default()
    };
    crew.card(2, "Redis store", None);
    crew.card(3, "Middleware", None);
    crew.start("s-001", "codex/gpt-5.6", Some(2));
    crew.start("s-002", "claude-code", Some(3));
    crew.at(3.0)
        .tool("s-001", "t1", "command_execution", json!({"command": "ls"}));
    let printed = text(&crew.printed(100));
    assert!(
        printed.contains("└ Opus 5.5 on Claude Code · worker · task-003 Middleware"),
        "{printed}"
    );
    let live = text(&crew.live(100));
    assert!(
        live.contains("Opus 5.5 on Claude Code · worker · task-003 (Middleware)"),
        "{live}"
    );
}

#[test]
fn unknown_event_line() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.start("s-001", "claude-code/opus", None);
    crew.at(2.0).agent(
        "s-001",
        AgentEvent::Unknown {
            raw: json!({"type": "system", "subtype": "hook_response", "x": 1}),
        },
    );
    crew.agent(
        "s-001",
        AgentEvent::Unknown {
            raw: Value::String("not json at all".to_owned()),
        },
    );
    let lines = crew.live(80);
    let live = text(&lines);
    assert!(
        live.contains("⎿  ? unknown event (system/hook_response)"),
        "{live}"
    );
    // One hook per block, so the second line sits at the body column.
    assert!(live.contains("\n     ? unknown event (not JSON)"), "{live}");
    assert_eq!(
        style_of(&lines, "? unknown event").and_then(|s| s.fg),
        theme().style(Role::Dim).fg
    );
    crew.message("s-001", "ok");
    let printed = text(&crew.printed(80));
    assert!(
        printed.contains("? unknown event (system/hook_response)"),
        "{printed}"
    );
}

#[test]
fn agent_text_is_sanitized() {
    let evil = "\u{1b}[31mred\u{1b}[0m \u{1b}]0;owned\u{7} \u{1b}[2Jclear\r\u{8}";
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, evil, None);
    crew.start("s-001", "claude-code/opus", Some(3));
    crew.at(2.0)
        .tool(
            "s-001",
            "t1",
            "Bash",
            json!({"command": format!("echo {evil}")}),
        )
        .done("s-001", "t1", true, Some(evil))
        .tool("s-001", "t2", "mcp__x\u{1b}[1m__y", json!({"q": evil}))
        .agent(
            "s-001",
            AgentEvent::Todo {
                items: vec![TodoItem {
                    text: evil.to_owned(),
                    status: TodoStatus::Pending,
                }],
            },
        )
        .agent(
            "s-001",
            AgentEvent::TextDelta {
                text: evil.to_owned(),
                parent: None,
            },
        )
        .agent(
            "s-001",
            AgentEvent::Unknown {
                raw: json!({"type": evil}),
            },
        );
    let mut all = crew.live(80);
    crew.message("s-001", evil);
    crew.at(9.0).end(
        "s-001",
        EndReason::Crashed {
            message: evil.to_owned(),
        },
        9,
        10,
    );
    all.extend(crew.printed(80));
    for line in &all {
        for span in &line.spans {
            assert!(
                !span.content.chars().any(|c| c.is_control()),
                "a control character reached the screen: {:?}",
                span.content
            );
        }
    }
    assert!(text(&all).contains("red"));
}

#[test]
fn quiet_harnesses_say_how_long_they_have_been_quiet() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.start("s-001", "antigravity-cli/gemini-3.6-flash", None);
    crew.at(5.0).tool(
        "s-001",
        "step-1",
        "run_command",
        json!({"CommandLine": "cargo test"}),
    );
    assert!(!text(&crew.live(120)).contains("quiet"));
    crew.at(50.0);
    let lines = crew.live(120);
    assert!(
        text(&lines).contains("· quiet 45s · may be waiting on a limit"),
        "{}",
        text(&lines)
    );
    assert_eq!(
        style_of(&lines, "quiet").and_then(|s| s.fg),
        theme().style(Role::Dim).fg
    );
    crew.at(185.0);
    let lines = crew.live(120);
    assert!(text(&lines).contains("· quiet 3m · may be waiting on a limit"));
    assert_eq!(
        style_of(&lines, "quiet").and_then(|s| s.fg),
        theme().style(Role::Warning).fg
    );

    // Claude Code says when it retries, so it never gets the hint.
    let mut claude = Crew::new(Verbosity::Full);
    claude.start("s-001", "claude-code/opus", None);
    claude.at(300.0);
    assert!(!text(&claude.live(120)).contains("quiet"));
}

#[test]
fn replay_keeps_finished_sessions_for_the_overlay() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.state.replay = Replay::Replaying {
        run: crew.run.clone(),
    };
    crew.card(2, "Redis store", None);
    crew.card(3, "Middleware", None);
    crew.start("s-001", "codex/gpt-5.6", Some(2));
    crew.start("s-002", "claude-code/opus", Some(3));
    crew.at(30.0)
        .tool("s-001", "t1", "command_execution", json!({"command": "ls"}))
        .done("s-001", "t1", true, Some("a"))
        .message("s-001", "done")
        .tool("s-002", "t2", "Read", json!({"file_path": "a.rs"}));
    crew.at(60.0).end("s-001", EndReason::Completed, 60, 2_000);
    crew.at(600.0);
    crew.state.replay = Replay::Live;
    crew.app(&AppEvent::ReplayDone {
        run: crew.run.clone(),
        seq: 9,
    });
    // The condensed replay prints in place of the finished session, which Ctrl+O still shows.
    assert!(!crew.model.history.has_unprinted());
    assert_eq!(
        text(&crew.printed(100)),
        "● GPT 5.6 on Codex · worker · task-002 (Redis store)\n  ⎿  Done · 1 tool use · report ✓ · 1m 0s · 2k tokens"
    );
    // The session still going keeps its block, timed from its start to the newest event rather
    // than from the replay.
    let live = text(&crew.live(100));
    assert!(
        live.starts_with("● Opus on Claude Code · worker · task-003 (Middleware) · 1m0s"),
        "{live}"
    );
}

#[test]
fn stays_in_shape_at_every_width() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, samples::CJK, Some(CardState::Running));
    crew.card(4, samples::EMOJI, None);
    crew.start("s-001", "claude-code/opus", Some(3));
    crew.start("s-002", "codex/gpt-5.6", Some(4));
    let long_word = samples::long_word();
    crew.at(4.0)
        .tool("s-001", "t1", "Read", json!({"file_path": samples::LONG_PATH}))
        .done("s-001", "t1", true, Some(samples::CJK))
        .tool("s-001", "t2", "Bash", json!({"command": long_word}))
        .done("s-001", "t2", false, Some(&format!("Exit code 2\n{}\n{}", samples::EMOJI, long_word)))
        .tool(
            "s-001",
            "t3",
            "Edit",
            json!({"file_path": samples::LONG_PATH, "old_string": samples::CJK, "new_string": samples::EMOJI}),
        )
        .done("s-001", "t3", true, None)
        .agent(
            "s-001",
            AgentEvent::Todo {
                items: vec![
                    TodoItem {
                        text: samples::CJK.to_owned(),
                        status: TodoStatus::Completed,
                    },
                    TodoItem {
                        text: samples::EMOJI.to_owned(),
                        status: TodoStatus::InProgress,
                    },
                ],
            },
        )
        .agent(
            "s-001",
            AgentEvent::TextDelta {
                text: format!("{} {}", samples::EMOJI, samples::CJK),
                parent: None,
            },
        )
        .message("s-002", &format!("{} {}", samples::CJK, samples::LONG_PATH));
    crew.tokens("s-001", 248_200);
    crew.at(4000.0);
    let mut snapshots = BTreeMap::new();
    for width in stability::WIDTHS {
        let live = crew.live(width);
        stability::assert_lines(&live, width);
        stability::assert_lines(&crew.printed(width), width);
        snapshots.insert(width, live.len());
    }
    crew.end(
        "s-001",
        EndReason::Crashed {
            message: samples::EMOJI.to_owned(),
        },
        4000,
        248_200,
    );
    for width in stability::WIDTHS {
        stability::assert_lines(&crew.printed(width), width);
        stability::assert_lines(&crew.live(width), width);
    }
    assert!(snapshots.values().all(|rows| *rows > 0));
}

#[test]
fn a_session_known_only_from_state_still_gets_a_block() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, "Middleware", None);
    let key = SessionKey::parse("s-004").expect("a key");
    let worker: WorkerRef = "codex/gpt-5.6".parse().expect("a worker");
    let state = session_state(&key, &worker, Some(CardId::numbered(3)), START);
    crew.run_state().sessions.insert(key, state);
    crew.at(30.0)
        .tool("s-004", "t1", "command_execution", json!({"command": "ls"}));
    let live = text(&crew.live(100));
    assert!(
        live.starts_with("● GPT 5.6 on Codex · worker · task-003 (Middleware)"),
        "{live}"
    );

    // A session that has ended gets none from a late event.
    let key = SessionKey::parse("s-005").expect("a key");
    let mut ended = session_state(&key, &worker, None, START);
    ended.status = SessionStatus::Done;
    crew.run_state().sessions.insert(key, ended);
    crew.agent(
        "s-005",
        AgentEvent::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cached_tokens: 0,
            cost_usd: None,
            cumulative: true,
        },
    );
    assert_eq!(crew.model.history.active_cells().count(), 1);
}

#[test]
fn argonauts_own_calls_stay_in_the_overlay() {
    for level in [Verbosity::Full, Verbosity::Normal] {
        let mut crew = Crew::new(level);
        crew.card(3, "Middleware", Some(CardState::InReview));
        crew.start("s-001", "claude-code/opus", Some(3));
        crew.at(12.0)
            .tool("s-001", "t1", "Read", json!({"file_path": "src/app.rs"}))
            .done("s-001", "t1", true, Some("fn main() {}\n"))
            .tool(
                "s-001",
                "t2",
                "mcp__argonauts_status__argonauts_status",
                json!({}),
            );
        let live = text(&crew.live(100));
        assert!(live.contains("Read"), "{live}");
        assert!(!live.contains("argonauts_status"), "{live}");
        crew.done("s-001", "t2", true, Some("task-003 running"))
            .message("s-001", "Wired the middleware in front of the router.")
            .tool("s-001", "t3", "StructuredOutput", json!({"status": "done"}))
            .done("s-001", "t3", true, Some("ok"));
        let live = text(&crew.live(100));
        assert!(!live.contains("Report"), "{live}");
        crew.at(580.0)
            .end("s-001", EndReason::Completed, 580, 61_000);

        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(at(crew.now)));
        let scrollback = text(&crew.model.history.drain_printable(&ctx));
        assert!(scrollback.contains("Wired the middleware"), "{scrollback}");
        assert!(!scrollback.contains("argonauts_status"), "{scrollback}");
        assert!(!scrollback.contains("Report"), "{scrollback}");

        let overlay = ctx.with_expanded(true);
        let detail: String = crew
            .model
            .history
            .cells()
            .iter()
            .map(|cell| text(&cell.lines(&overlay)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(detail.contains("argonauts_status"), "{level:?}: {detail}");
        assert!(detail.contains("Report(done)"), "{level:?}: {detail}");
        assert!(detail.contains("Sent the report"), "{level:?}: {detail}");
        assert_eq!(detail.matches("Report(done)").count(), 1, "{detail}");
    }
}

/// A reviewer that Claude Code's own workflow asks through Argonauts answers inside the
/// workflow's row and view: it gets no block of its own and prints nothing in the chat.
#[test]
fn a_reviewer_asked_inside_a_cli_workflow_prints_nothing_of_its_own() {
    use argonauts_core::events::session::{VendorPhase, VendorWorkflow};
    let mut crew = Crew::new(Verbosity::Full);
    crew.start("s-001", "claude-code/haiku", Some(1));
    let key = SessionKey::parse("s-001").expect("a key");
    if let Some(host) = crew.run_state().sessions.get_mut(&key) {
        host.workflows.push(VendorWorkflow {
            id: "wf_1".into(),
            call: None,
            name: "add-due-dates".into(),
            description: String::new(),
            phases: vec![VendorPhase {
                index: 1,
                title: "Review".into(),
                detail: String::new(),
            }],
            agents: Vec::new(),
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        });
    }
    crew.at(5.0);
    crew.start_asked("s-002", "codex/gpt-5.6-luna");
    crew.message("s-002", "The draft answer: past dates pass.");
    crew.end("s-002", EndReason::Completed, 15, 36_400);
    let printed = text(&crew.printed(100));
    let live = text(&crew.live(100));
    assert!(!printed.contains("past dates pass"), "{printed}");
    assert!(!printed.contains("GPT 5.6 Luna"), "{printed}");
    assert!(!live.contains("GPT 5.6 Luna"), "{live}");
}

/// The call that starts Claude Code's own workflow returns at once, but its row stays live
/// until the workflow ends, then prints once with its final phases.
#[test]
fn a_workflow_row_stays_live_until_its_workflow_ends() {
    use argonauts_core::events::session::{VendorPhase, VendorWorkflow};
    let workflow = |status| VendorWorkflow {
        id: "wf_1".into(),
        call: Some("toolu_w".into()),
        name: "add-due-dates".into(),
        description: String::new(),
        phases: ["Build", "Review"]
            .iter()
            .zip(1..)
            .map(|(title, index)| VendorPhase {
                index,
                title: (*title).to_owned(),
                detail: String::new(),
            })
            .collect(),
        agents: Vec::new(),
        status,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        summary: None,
        started_at_ms: None,
    };
    let mut crew = Crew::new(Verbosity::Full);
    crew.start("s-001", "claude-code/haiku", Some(1));
    crew.tool(
        "s-001",
        "toolu_w",
        "Workflow",
        json!({"name": "add-due-dates", "script": "..."}),
    )
    .done(
        "s-001",
        "toolu_w",
        true,
        Some("Workflow launched in background"),
    )
    .agent(
        "s-001",
        AgentEvent::Workflow {
            workflow: Box::new(workflow(SubagentStatus::Running)),
        },
    );
    crew.message("s-001", "The workflow is running.");
    let printed = text(&crew.printed(100));
    assert!(!printed.contains("Workflow(add-due-dates)"), "{printed}");
    let live = text(&crew.live(100));
    assert!(live.contains("Workflow(add-due-dates)"), "{live}");
    crew.agent(
        "s-001",
        AgentEvent::Workflow {
            workflow: Box::new(workflow(SubagentStatus::Done)),
        },
    );
    crew.end("s-001", EndReason::Completed, 96, 264_000);
    let printed = text(&crew.printed(100));
    assert_eq!(
        printed.matches("Workflow(add-due-dates)").count(),
        1,
        "{printed}"
    );
}

/// By default an agent at work stays out of the chat, which the list under the input covers,
/// and lands there once, when it finishes, as it would have printed going along.
#[test]
fn an_agent_at_work_lands_in_the_chat_once_when_it_finishes() {
    for level in [Verbosity::Full, Verbosity::Normal] {
        let mut crew = Crew::new(level);
        crew.settings.agents_in_chat = AgentsInChat::WhenDone;
        crew.card(3, "Middleware", Some(CardState::InReview));
        crew.start("s-001", "claude-code/opus", Some(3));
        crew.at(12.0)
            .tool("s-001", "t1", "Read", json!({"file_path": "src/app.rs"}))
            .done("s-001", "t1", true, Some("fn main() {}\n"));
        for n in 0..8 {
            let id = format!("b{n}");
            crew.tool(
                "s-001",
                &id,
                "Bash",
                json!({"command": format!("cargo test {n}")}),
            )
            .done("s-001", &id, true, Some("ok"));
        }
        crew.message("s-001", "Halfway there.");
        assert!(crew.live(100).is_empty(), "no live block in the chat");
        assert!(
            crew.printed(100).is_empty(),
            "nothing printed while it works"
        );
        crew.at(580.0)
            .end("s-001", EndReason::Completed, 580, 61_000);
        let printed = text(&crew.printed(100));
        assert!(printed.contains("Halfway there."), "{printed}");
        assert!(printed.contains("⎿  Done · 9 tool uses · "), "{printed}");
        assert_eq!(
            printed
                .matches("● Opus on Claude Code · worker · task-003")
                .count(),
            2,
            "its calls once, then its closing line:\n{printed}"
        );
    }
}

/// Landing once, it reads as it does printed going along.
#[test]
fn landing_once_reads_as_printing_along_did() {
    for level in [Verbosity::Full, Verbosity::Normal, Verbosity::Quiet] {
        let mut when_done = Crew::new(level);
        when_done.settings.agents_in_chat = AgentsInChat::WhenDone;
        let when_done = one_session_with(when_done);
        let along = one_session(level);
        assert_eq!(
            text(&when_done.printed(100)),
            text(&along.printed(100)),
            "{level:?}"
        );
    }
}

/// A call shows in its agent's block as the model starts writing it, by its name alone, fills
/// in where it stands, and goes when the model never finishes it.
#[test]
fn a_call_shows_in_the_block_as_its_model_starts_writing_it() {
    let mut crew = Crew::new(Verbosity::Full);
    crew.card(3, "Middleware", Some(CardState::Running));
    crew.start("s-001", "claude-code/opus", Some(3));
    let pending = |id: &str, name: &str| AgentEvent::ToolPending {
        id: id.to_owned(),
        name: name.to_owned(),
        parent: None,
    };
    crew.at(10.0).agent("s-001", pending("t1", "Edit"));
    let live = text(&crew.live(100));
    assert!(live.contains("Update"), "{live}");
    assert!(!live.contains("Update("), "{live}");
    crew.tool(
        "s-001",
        "t1",
        "Edit",
        json!({"file_path": format!("{WORKTREE}/src/app.rs"), "old_string": "a", "new_string": "b"}),
    );
    let live = text(&crew.live(100));
    assert_eq!(live.matches("Update").count(), 1, "{live}");
    assert!(live.contains("Update(src/app.rs)"), "{live}");
    crew.agent("s-001", pending("t2", "Write"));
    assert!(text(&crew.live(100)).contains("Write"));
    crew.agent(
        "s-001",
        AgentEvent::ToolDropped {
            id: "t2".to_owned(),
            parent: None,
        },
    );
    let live = text(&crew.live(100));
    assert!(!live.contains("Write"), "{live}");
    // A lookup of held-back tools stays out of the block, as Claude Code leaves it out.
    crew.agent("s-001", pending("t3", "ToolSearch"));
    crew.tool("s-001", "t3", "ToolSearch", json!({"query": "select:Read"}));
    crew.done("s-001", "t3", true, Some("<functions></functions>"));
    let live = text(&crew.live(100));
    assert!(!live.contains("ToolSearch"), "{live}");
}

/// An agent at work stays out of the chat while its calls are written, filled in or dropped,
/// and lands once with only the calls that ran.
#[test]
fn an_agent_at_work_lands_without_a_call_it_never_finished() {
    let pending = |id: &str, name: &str| AgentEvent::ToolPending {
        id: id.to_owned(),
        name: name.to_owned(),
        parent: None,
    };
    let mut crew = Crew::new(Verbosity::Full);
    crew.settings.agents_in_chat = AgentsInChat::WhenDone;
    crew.card(3, "Middleware", Some(CardState::Running));
    crew.start("s-001", "claude-code/opus", Some(3));
    crew.at(10.0).agent("s-001", pending("t1", "Edit"));
    crew.agent("s-001", pending("t2", "Write"));
    crew.tool(
        "s-001",
        "t1",
        "Edit",
        json!({"file_path": format!("{WORKTREE}/src/app.rs"), "old_string": "a", "new_string": "b"}),
    );
    crew.done("s-001", "t1", true, Some("The file has been updated."));
    crew.agent(
        "s-001",
        AgentEvent::ToolDropped {
            id: "t2".to_owned(),
            parent: None,
        },
    );
    assert!(crew.live(100).is_empty(), "no live block in the chat");
    assert!(
        crew.printed(100).is_empty(),
        "nothing printed while it works"
    );

    crew.at(60.0).end("s-001", EndReason::Completed, 60, 12_000);
    let printed = text(&crew.printed(100));
    assert_eq!(
        printed.matches("Update(src/app.rs)").count(),
        1,
        "{printed}"
    );
    assert!(!printed.contains("Write"), "{printed}");
}
