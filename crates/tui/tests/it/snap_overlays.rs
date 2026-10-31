use crate::common;
use std::path::PathBuf;

use argonauts_core::agents::cli_sessions::CliSession;
use argonauts_core::app::agent_panel::{AgentPanelRows, CrewTab};
use argonauts_core::app::views::ContextUse;
use argonauts_core::app::{
    AgentReady, AnswerTarget, ConversationSummary, HeaderAndBottomLine, UiCommand, ViewRequest,
};
use argonauts_core::base::names::{
    AgentId, ConversationId, MessageId, ModelRef, RoleName, SessionKey,
};
use argonauts_core::input::{MentionTarget, ResumeTarget, SlashCommand};
use argonauts_core::runs::api::{DiffView, RawView};
use argonauts_core::runs::events::approval::ApprovalOpened;
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::session::{
    Delivery, FileDiff, QueuedMessage, SessionInput, SubagentKind, SubagentStatus, SubagentUpdate,
};
use argonauts_core::{DiffStat, RunId, TaskId};
use argonauts_tui::base::format::WallClock;
use argonauts_tui::base::input::keymap::Keymap;
use argonauts_tui::base::stability::{WIDTHS, assert_frames, buffer_text, samples};
use argonauts_tui::base::theme::{Role, Theme};
use argonauts_tui::frame::state::{
    AppState, OpenApproval, RunState, SessionState, SessionStatus, TaskRow, ToolRecord, Welcome,
};
use argonauts_tui::frame::view::pager::Pager;
use argonauts_tui::frame::view::{View, ViewCtx, ViewOutcome};
use argonauts_tui::views::crew::AgentPanelView;
use argonauts_tui::views::help::HelpView;
use argonauts_tui::views::resume::ResumePicker;
use argonauts_tui::views::session::SessionView;
use common::{t0, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::json;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

const SIZES: [(u16, u16); 2] = [(100, 32), (80, 24)];

const WALL: OffsetDateTime = datetime!(2026-09-30 12:00 UTC);

fn run_id() -> RunId {
    RunId::new("run-01").unwrap()
}

fn key(s: &str) -> SessionKey {
    SessionKey::parse(s).unwrap()
}

fn worker(s: &str) -> ModelRef {
    s.parse().unwrap()
}

fn ctx<'a>(theme: &'a Theme, state: &'a AppState) -> ViewCtx<'a> {
    ViewCtx {
        theme,
        state,
        now: t0(),
    }
}

fn press(view: &mut dyn View, code: KeyCode, state: &AppState) -> ViewOutcome {
    let theme = theme();
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx(&theme, state))
}

fn draw(view: &mut dyn View, width: u16, height: u16, state: &AppState) -> Buffer {
    let theme = theme();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.render(area, frame.buffer_mut(), &ctx(&theme, state));
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

/// The screen as text, trailing spaces trimmed, after the frame check.
fn screen(view: &mut dyn View, width: u16, height: u16, state: &AppState) -> String {
    let buf = draw(view, width, height, state);
    assert_frames(&buf);
    buffer_text(&buf)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn row_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} in:\n{text}"))
}

fn position(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} in:\n{text}"))
}

fn session(
    key_: &str,
    by: &str,
    role: RoleName,
    task: u32,
    status: SessionStatus,
    ago_secs: i64,
) -> SessionState {
    let worker = worker(by);
    let agent_id = worker.agent.clone();
    let mut session = SessionState::new(
        key(key_),
        worker,
        agent_id,
        role,
        WALL - Duration::seconds(ago_secs),
    );
    session.task = Some(TaskId::numbered(task));
    session.status = status;
    session
}

fn tool(id: &str, name: &str, input: serde_json::Value, done: Option<(bool, &str)>) -> ToolRecord {
    ToolRecord {
        id: id.into(),
        name: name.into(),
        input,
        parent: None,
        ok: done.map(|(ok, _)| ok),
        output: done.map(|(_, output)| output.to_owned()),
    }
}

fn reading(agent_id: &str, used: f64, resets: Option<&str>) -> serde_json::Value {
    json!({"agent_id": agent_id, "status": if used >= 0.9 { "warning" } else { "allowed" },
           "used": used, "resets_at": resets, "window": "five_hour",
           "source": "rate_limit_event", "observed_at": "2026-09-30T11:59:00Z", "detail": null})
}

fn question(id: &str, from: &str, task: &str, text: &str) -> OpenApproval {
    let option = |key: &str, label: &str, recommended: bool| {
        json!({"key": key, "label": label, "action": {"action": "choose", "value": label},
               "recommended": recommended, "detail": null})
    };
    let opened: ApprovalOpened = serde_json::from_value(json!({
        "id": id, "kind": "question", "title": "Question approval · run-01", "task": task,
        "packet": {"kind": "question", "from": from, "task": task, "text": text,
                   "options": ["Yes, keep it", "No, JSON only"]},
        "evidence_sha256": "e3b0c442", "mode": "supervised", "decide_with": "rules",
        "options": [option("a", "Yes, keep it", true), option("b", "No, JSON only", false)],
        "free_text": "answer", "needs_you": true}))
    .unwrap();
    OpenApproval {
        opened: Box::new(opened),
        set_aside: false,
        replayed: false,
    }
}

fn session_input(parts: &[(&str, &str)]) -> SessionInput {
    let parts: Vec<serde_json::Value> = parts
        .iter()
        .map(|(label, text)| {
            json!({"kind": part_kind(label), "label": label, "text": text,
                   "approx_tokens": text.len() / 4, "source": null})
        })
        .collect();
    serde_json::from_value(json!({
        "launch": {"program": "codex", "args": ["exec", "--json", "<prompt>"],
                   "cwd": "/wt/task-002", "env_set": ["ARGONAUTS_SESSION"], "env_removed": [],
                   "prompt_via": "stdin", "sandbox": "worktree"},
        "parts": parts, "queued": []}))
    .unwrap()
}

/// A crew of four at work: one asks you `asks`, one writes, one waits on a review, and one is
/// done and was the fallback for another.
fn crew_state(asks: &str) -> AppState {
    let mut run = RunState::new(run_id());
    run.title = "Add a --json flag to export".into();
    run.started = Some(WALL - Duration::minutes(20));
    for (n, title) in [
        (1, "Rename the CSV writer"),
        (2, "Add the JSON writer"),
        (3, "Wire the --json flag"),
    ] {
        run.tasks.insert(
            TaskId::numbered(n),
            TaskRow::new(TaskId::numbered(n), title),
        );
    }
    let mut claude = session(
        "s-001",
        "claude-code/opus",
        RoleName::Worker,
        3,
        SessionStatus::Working,
        340,
    );
    claude.follow = Some(1);
    claude.tokens_out = 71_000;
    claude.tools = vec![
        tool(
            "t-1",
            "Read",
            json!({"file_path": "src/mw/limit.ts"}),
            Some((true, "export function limit() {}")),
        ),
        tool(
            "t-2",
            "Edit",
            json!({"file_path": "src/mw/limit.ts", "old_string": "a", "new_string": "b"}),
            None,
        ),
    ];
    let mut codex = session(
        "s-002",
        "codex/gpt-5.6",
        RoleName::Worker,
        2,
        SessionStatus::Working,
        120,
    );
    codex.follow = Some(2);
    codex.tokens_out = 12_400;
    let mut agy = session(
        "s-003",
        "antigravity-cli/gemini-3.6-pro",
        RoleName::Reviewer,
        3,
        SessionStatus::Waiting,
        70,
    );
    agy.follow = Some(3);
    agy.tokens_out = 4_000;
    let mut done = session(
        "s-004",
        "codex/gpt-5.6",
        RoleName::Worker,
        1,
        SessionStatus::Done,
        770,
    );
    done.ended = Some(WALL - Duration::seconds(190));
    done.tokens_out = 61_000;
    done.messages = vec!["Merged task-001.".into()];
    done.fallback_for = Some(worker("claude-code/opus"));
    for session in [claude, codex, agy, done] {
        run.session_order.push(session.key.clone());
        run.sessions.insert(session.key.clone(), session);
    }
    run.approvals
        .push(question("approval-001", "codex/gpt-5.6", "task-002", asks));

    let mut state = AppState::default();
    state.attached = Some(run_id());
    state.runs.insert(run_id(), run);
    state.welcome = Some(Welcome {
        version: "0.1.0".into(),
        repo: "~/code/export".into(),
        workflow: "council".into(),
        main_agent: None,
        agents: ["claude-code", "codex", "antigravity-cli", "opencode"]
            .into_iter()
            .map(|id| AgentReady {
                id: id.into(),
                version: None,
                ready: true,
                note: None,
            })
            .collect(),
        runs_going: 1,
        runs_need_you: 0,
        example_prompt: None,
    });
    let status: HeaderAndBottomLine = serde_json::from_value(json!({
        "title": "Argonauts Code", "repo": "~/code/export", "workflow": "council", "run": "run-01",
        "main_agent": "claude-code/opus",
        "plan_chips": [
            {"label": "Claude", "reading": reading("claude-code", 0.72, Some("2026-09-30T15:10:00Z")), "health": "healthy"},
            {"label": "Codex", "reading": reading("codex", 0.41, Some("2026-09-30T13:45:00Z")), "health": "healthy"},
            {"label": "agy", "reading": reading("antigravity-cli", 0.9, None), "health": "healthy"}],
        "spent": null, "limit": null, "narrator": "on_demand", "approval_mode": "supervised"}))
    .unwrap();
    state.status = Some(status);
    state
}

/// The crew of four with the labels a run gives its agents now. `crew_state` alone reads as a
/// log from before agents had labels, which the session view's snapshots keep.
fn labelled(mut state: AppState) -> AppState {
    let run = state.runs.get_mut(&run_id()).unwrap();
    for (session, label) in [
        ("s-001", "build:json-flag"),
        ("s-002", "build:json-writer"),
        ("s-003", "review:json-flag"),
        ("s-004", "build:csv-writer"),
    ] {
        run.sessions.get_mut(&key(session)).unwrap().label = Some(label.into());
    }
    state
}

fn open_crew(state: &AppState) -> AgentPanelView {
    let mut view = AgentPanelView::new(state);
    view.clock = WallClock::pinned(WALL);
    view
}

/// What the controller lists beside this screen's agents: a conversation, a Codex session of
/// its own, this run (which the screen's agents stand for), and two runs in another repo.
/// `text` is the other repo's waiting run's title.
fn fetched_panel(text: &str) -> AgentPanelRows {
    serde_json::from_value(json!({
        "repo": "~/code/export",
        "rows": [
            {"target": {"kind": "conversation", "conversation": "conversation-003"}, "state": "ready",
             "title": "How does the export pick a format?", "repo": "~/code/export",
             "worker": "claude-code/opus",
             "latest": "It reads --format, then the file's extension.",
             "updated": "2026-09-30T11:20:00Z",
             "details": {"last_message": "It reads --format first.\n\nWithout it, the file's extension picks: .json for JSON, anything else for CSV.",
                         "prompt": "How does the export pick a format?"}},
            {"target": {"kind": "cli_session", "session": {"agent_id": "codex", "id": "019a2f"}},
             "state": "ready", "title": "Explain the input handling", "repo": "~/code/export",
             "worker": "codex/gpt-5.6", "latest": "It reads keys in one loop.",
             "updated": "2026-09-29T12:00:00Z",
             "details": {"branch": "main", "prompt": "Explain the input handling"}},
            {"target": {"kind": "run", "run": "run-01"}, "state": "working",
             "title": "Add a --json flag to export", "repo": "~/code/export",
             "run": {"id": "run-01", "title": "Add a --json flag to export"},
             "latest": "running", "updated": "2026-09-30T11:59:00Z"},
            {"target": {"kind": "run", "run": "run-07"}, "state": "needs_you",
             "title": text, "repo": "~/code/api",
             "run": {"id": "run-07", "title": text, "stands": {"state": "needs_you",
                 "word": "plan approval, waiting for you", "since": "2026-09-29T06:00:00Z",
                 "spent": {"input_tokens": 40000, "output_tokens": 9000, "cached_tokens": 0,
                     "cost_usd": 0.46, "cost_partial": false, "cost_estimated": false}}},
             "latest": "plan approval, waiting for you", "updated": "2026-09-29T06:00:00Z",
             "details": {"spent": {"input_tokens": 40000, "output_tokens": 9000,
                 "cached_tokens": 0, "cost_usd": 0.46, "cost_partial": false,
                 "cost_estimated": false}}},
            {"target": {"kind": "run", "run": "run-06"}, "state": "done",
             "title": "Tidy the README", "repo": "~/code/api",
             "run": {"id": "run-06", "title": "Tidy the README", "name": "tidy-readme",
                 "stands": {"state": "done", "word": "merged",
                     "since": "2026-09-28T10:00:00Z"}},
             "latest": "merged", "updated": "2026-09-28T10:00:00Z"}
        ]
    }))
    .unwrap()
}

/// The crew of four, with what the controller listed, and the asking agent's task, context,
/// and prompt filled in for the details.
fn panel_state(asks: &str, other: &str) -> AppState {
    let mut state = labelled(crew_state(asks));
    let run = state.runs.get_mut(&run_id()).unwrap();
    let codex = run.sessions.get_mut(&key("s-002")).unwrap();
    codex.context = Some(ContextUse {
        used: 41_000,
        window: Some(200_000),
        cap: Some(200_000),
    });
    codex.inputs = vec![session_input(&[(
        "task",
        "Add a JSON writer beside the CSV one. Keep the CSV output byte for byte.",
    )])];
    state.agent_panel = Some(fetched_panel(other));
    state
}

/// The agent panel's sizes: the details sit beside the list from 120 columns.
const PANEL_SIZES: [(u16, u16); 4] = [(80, 24), (100, 32), (120, 32), (200, 40)];

#[test]
fn agent_panel_view_says_what_a_run_waits_on_and_what_each_step_does() {
    let mut state = labelled(crew_state("Keep a CSV fallback?"));
    let run = state.runs.get_mut(&run_id()).unwrap();
    // A merge waits on you with no agent of its own.
    let mut merge = question(
        "approval-009",
        "codex/gpt-5.6",
        "task-001",
        "Merge this run into main?",
    );
    merge.opened.task = None;
    merge.opened.title = "Merge this run into main?".into();
    run.approvals.push(merge);
    for (key, role) in [("s-005", RoleName::Reader), ("s-006", RoleName::Draft)] {
        let mut step = session(key, "claude-code/haiku", role, 1, SessionStatus::Done, 900);
        step.task = None;
        run.session_order.push(step.key.clone());
        run.sessions.insert(step.key.clone(), step);
    }
    for (width, height) in PANEL_SIZES {
        let mut view = open_crew(&state);
        insta::assert_snapshot!(
            format!("agent_panel_view_asks_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = open_crew(&state);
    let wide = screen(&mut view, 200, 40, &state);
    // The approval's row says what it is and asks its question once, under the run's heading,
    // which names the work once. The step agents with no task say which step they are.
    let merge_row = wide
        .lines()
        .find(|line| line.contains("asks: Merge this run into main?"))
        .unwrap_or_else(|| panic!("{wide}"));
    assert!(merge_row.contains("◆ question"), "{wide}");
    assert_eq!(
        wide.matches("Merge this run into main?").count(),
        2,
        "row and details"
    );
    assert_eq!(wide.matches("Add a --json flag to export").count(), 3);
    for name in ["✓ reader", "✓ draft"] {
        assert!(wide.contains(name), "{name}\n{wide}");
    }
}

#[test]
fn a_session_s_title_takes_the_room_the_last_lines_leave() {
    let long = "How does the export pick a format when the two disagree?";
    let mut state = labelled(crew_state("Keep a CSV fallback?"));
    let mut panel = fetched_panel("Fix the login redirect");
    panel.rows[0].title = long.into();
    state.agent_panel = Some(panel);
    let mut view = open_crew(&state);
    let narrow = screen(&mut view, 80, 24, &state);
    assert!(!narrow.contains(long), "{narrow}");
    // Cut or not, the agents' labels beside it stay whole.
    assert!(narrow.contains("build:json-writer  asks:"), "{narrow}");
    let mut view = open_crew(&state);
    let wide = screen(&mut view, 200, 40, &state);
    assert!(wide.contains(long), "{wide}");
}

#[test]
fn agent_panel_view() {
    let state = panel_state("Keep a CSV fallback?", "Fix the login redirect");
    for (width, height) in PANEL_SIZES {
        let mut view = open_crew(&state);
        insta::assert_snapshot!(
            format!("agent_panel_view_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = open_crew(&state);
    let text = screen(&mut view, 100, 32, &state);

    // Plan usage sits on one line at the top right, and the tabs count their rows.
    let title = text.lines().next().unwrap();
    assert!(title.starts_with("argonauts › crew"), "{text}");
    assert!(
        title.ends_with("Claude 72% · Codex 41% · agy 90%"),
        "{text}"
    );
    let tabs = text.lines().nth(1).unwrap();
    assert_eq!(
        tabs,
        "  Needs you 2    Working 2    Ready 2    Done 2  [ All 8 ]"
    );

    // This repo first, its run's agents most urgent first, then its conversations and CLI
    // sessions; the controller's row for the run on screen gives way to its agents.
    let order = [
        "~/code/export",
        "Add a --json flag to export",
        "build:json-writer",
        "build:json-flag",
        "Sessions",
        "How does the",
        "Explain the input",
        "~/code/api",
        "Fix the login redirect",
        "tidy-readme",
    ]
    .map(|needle| position(&text, needle));
    assert!(order.is_sorted(), "{order:?}\n{text}");
    assert!(!text.contains("running"), "{text}");
    // The run's title shows once, in its heading, and each agent goes by its label.
    assert_eq!(text.matches("Add a --json flag to export").count(), 1);
    let asks = row_with(&text, "asks: Keep a CSV fallback?");
    for cell in ["❯ ◆ build:json-writer", "GPT 5.6 on Codex", "2m ago"] {
        assert!(asks.contains(cell), "{cell}:\n{text}");
    }
    let writing = row_with(&text, "Update(src/mw/limit.ts)");
    assert!(writing.contains("● build:json-flag"), "{text}");
    assert!(writing.contains("Opus on Claude Code") && writing.contains("5m ago"));
    assert!(row_with(&text, "◎ Explain the input").contains("Codex · enter continues"));
    assert!(row_with(&text, "Merged task-001.").contains("✓ build:csv-writer"));
    // A run with none of its agents listed is one row: its heading.
    assert!(row_with(&text, "plan approval").starts_with("◆ Fix the login"));
    assert!(text.contains("✓ tidy-readme · merged 2d ago"), "{text}");
    let footer = text.lines().last().unwrap();
    assert_eq!(
        footer,
        "↑↓  move · →  open · Space details · Tab filter · x stop · ? keys · esc back"
    );

    // At 120 columns the details sit beside the list, as in Codex.
    let wide = screen(&mut view, 120, 32, &state);
    for needle in [
        "│ Add the JSON writer",
        "│ ◆ Needs you",
        "│ Last message",
        "│ asks: Keep a CSV fallback?",
        "│ Repo     ~/code/export",
        "│ Run      run-01 · Add a --json flag",
        "│          to export",
        "│ Task     task-002",
        "│ Model    GPT 5.6 on Codex",
        "(codex/gpt-5.6)",
        "│ Tokens   0 in · 12.4k out",
        "│ Context  41k of 200k (21%)",
        "│ Branch   argonauts/run-01/task-002",
        "│ Prompt",
        "│ Add a JSON writer beside the CSV",
    ] {
        assert!(wide.contains(needle), "{needle}:\n{wide}");
    }
    assert!(!wide.contains("Space details"), "{wide}");

    // Under 120 columns Space shows them under the list, and Esc hides them first.
    press(&mut view, KeyCode::Char(' '), &state);
    let narrow = screen(&mut view, 100, 32, &state);
    insta::assert_snapshot!("agent_panel_view_details_100", narrow);
    assert!(position(&narrow, "Explain the input") < position(&narrow, "Branch   argonauts"));
    assert!(matches!(
        press(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Consumed
    ));
    assert!(!screen(&mut view, 100, 32, &state).contains("Branch   argonauts"));

    // Enter on the ◆ row answers its question; → on a working row opens its session.
    let answer = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&answer, ViewOutcome::AnswerQuestion(AnswerTarget::Approval { id, .. }) if id.as_str() == "approval-001"),
        "{answer:?}"
    );
    press(&mut view, KeyCode::Down, &state);
    let open = press(&mut view, KeyCode::Right, &state);
    let ViewOutcome::Open(mut session) = open else {
        panic!("{open:?}");
    };
    assert!(
        screen(session.as_mut(), 100, 32, &state).contains("build:json-flag · Opus on Claude Code")
    );

    // x asks first, and the second x stops that agent.
    assert!(matches!(
        press(&mut view, KeyCode::Char('x'), &state),
        ViewOutcome::Consumed
    ));
    assert!(
        screen(&mut view, 100, 32, &state)
            .contains("x again to stop Opus on Claude Code · esc to keep")
    );
    let stop = press(&mut view, KeyCode::Char('x'), &state);
    assert!(
        matches!(&stop, ViewOutcome::Send(UiCommand::StopAgent { session, .. }) if session.as_str() == "s-001"),
        "{stop:?}"
    );

    // ? shows the keys in place of the list, and ? or Esc hides them.
    press(&mut view, KeyCode::Char('?'), &state);
    let keys = screen(&mut view, 100, 32, &state);
    assert!(
        keys.contains("Tab, Shift+Tab  the next or the previous tab"),
        "{keys}"
    );
    assert!(!keys.contains("build:json-flag"), "{keys}");
    press(&mut view, KeyCode::Esc, &state);
    assert!(screen(&mut view, 100, 32, &state).contains("build:json-flag"));

    // Tab and Shift+Tab cycle the tabs, each with its rows only.
    press(&mut view, KeyCode::Tab, &state);
    assert_eq!(view.tab, CrewTab::NeedsYou);
    let needs_you = screen(&mut view, 100, 32, &state);
    assert!(needs_you.contains("[ Needs you 2 ]"), "{needs_you}");
    assert!(needs_you.contains("Fix the login redirect"), "{needs_you}");
    assert!(!needs_you.contains("build:json-flag"), "{needs_you}");
    press(&mut view, KeyCode::Tab, &state);
    press(&mut view, KeyCode::Tab, &state);
    assert_eq!(view.tab, CrewTab::Ready);
    let ready = screen(&mut view, 100, 32, &state);
    assert!(position(&ready, "How does the export") < position(&ready, "Explain the input"));
    assert!(!ready.contains("~/code/api"), "{ready}");
    // Enter carries on a conversation or a CLI session in the conversation.
    let conversation = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&conversation, ViewOutcome::SendToConversation(UiCommand::Slash { command: SlashCommand::Resume { target: Some(ResumeTarget::Conversation(conversation)) } }) if conversation.as_str() == "conversation-003"),
        "{conversation:?}"
    );
    press(&mut view, KeyCode::Down, &state);
    let codex = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&codex, ViewOutcome::SendToConversation(UiCommand::Slash { command: SlashCommand::Resume { target: Some(ResumeTarget::CliSession(session)) } }) if session.to_string() == "codex:019a2f"),
        "{codex:?}"
    );
    press(&mut view, KeyCode::BackTab, &state);
    press(&mut view, KeyCode::BackTab, &state);
    press(&mut view, KeyCode::BackTab, &state);
    assert_eq!(view.tab, CrewTab::All);

    // A run in another repo says where to go; ← goes back to the conversation.
    for _ in 0..7 {
        press(&mut view, KeyCode::Down, &state);
    }
    let elsewhere = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&elsewhere, ViewOutcome::Notice(text) if text.contains("runs in ~/code/api")),
        "{elsewhere:?}"
    );
    assert!(matches!(
        press(&mut view, KeyCode::Left, &state),
        ViewOutcome::Close
    ));
}

#[test]
fn agent_panel_view_keeps_the_highlighted_row_as_rows_move() {
    let mut state = panel_state("Keep a CSV fallback?", "Fix the login redirect");
    let mut view = open_crew(&state);
    screen(&mut view, 100, 32, &state);
    press(&mut view, KeyCode::Down, &state);
    let before = screen(&mut view, 100, 32, &state);
    assert!(
        row_with(&before, "❯").contains("build:json-flag"),
        "{before}"
    );
    // The question is answered, so the writer moves up to the top.
    state.runs.get_mut(&run_id()).unwrap().approvals.clear();
    let after = screen(&mut view, 100, 32, &state);
    assert!(row_with(&after, "❯").contains("build:json-flag"), "{after}");
    assert!(after.contains("Needs you 1"), "{after}");

    // Before the controller answers, the panel says it's still asking.
    state.agent_panel = None;
    let asking = screen(&mut view, 100, 32, &state);
    assert!(
        asking.contains("Listing conversations and CLI sessions…"),
        "{asking}"
    );
    assert!(!asking.contains("~/code/api"), "{asking}");
}

/// The session the output snapshots follow: tool calls with results, a diff, subagents, a
/// message, and a queued steer. `text` is its message.
fn session_state(text: &str) -> AppState {
    let mut state = crew_state(text);
    let run = state.runs.get_mut(&run_id()).unwrap();
    let codex = run.sessions.get_mut(&key("s-002")).unwrap();
    codex.messages = vec![text.to_owned()];
    codex.tools = vec![
        tool(
            "t-1",
            "Read",
            json!({"file_path": "src/export.rs"}),
            Some((true, "fn write(&self) {}\nfn write_csv(&self) {}")),
        ),
        tool(
            "t-2",
            "Bash",
            json!({"command": "cargo test -- export"}),
            Some((true, "test export::csv ... ok\ntest export::json ... ok")),
        ),
        tool(
            "t-3",
            "Edit",
            json!({"file_path": "src/export.rs", "old_string": "self.write_csv(out)",
                   "new_string": "match self.format {"}),
            None,
        ),
    ];
    codex.diffs = vec![FileDiff {
        path: "src/export.rs".into(),
        added: 2,
        removed: 1,
        hunk: "@@ -10,3 +10,4 @@\n fn write(&self) {\n-        self.write_csv(out)\n+        match self.format {\n+            Format::Json => self.write_json(out),\n }".into(),
        truncated: false,
    }];
    codex.subagents = vec![
        SubagentUpdate {
            id: "sub-1".into(),
            parent: None,
            kind: SubagentKind::Subagent,
            label: Some("explorer".into()),
            prompt: Some("Find every CSV writer".into()),
            status: SubagentStatus::Done,
            last_message: Some("Two writers, both in src/export.rs".into()),
            log_uri: None,
            steps_visible: true,
            tokens: None,
        },
        SubagentUpdate {
            id: "thread-2".into(),
            parent: Some("sub-1".into()),
            kind: SubagentKind::Thread,
            label: None,
            prompt: Some("Check the tests".into()),
            status: SubagentStatus::Running,
            last_message: None,
            log_uri: None,
            steps_visible: false,
            tokens: None,
        },
    ];
    codex.queued = vec![QueuedMessage {
        id: MessageId::numbered(1),
        from: Party::User,
        to: MentionTarget::Number(2),
        text: "use serde_json".into(),
        queued_at: WALL,
        delivery: Delivery::Steer,
    }];
    codex.inputs = vec![session_input(&[
        (
            "task",
            "You own task-002: Add the JSON writer.\nKeep the CSV path working.",
        ),
        ("goal", "Export takes --json and prints valid JSON."),
        ("notes", "Use serde_json.\nNo new dependencies."),
    ])];
    state
}

fn session_view() -> SessionView {
    let mut view = SessionView::new(run_id(), key("s-002"));
    view.clock = WallClock::pinned(WALL);
    view
}

#[test]
fn session_view_output() {
    let state = session_state("The JSON writer goes beside the CSV one.");
    for (width, height) in SIZES {
        let mut view = session_view();
        insta::assert_snapshot!(
            format!("session_view_output_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = session_view();
    let text = screen(&mut view, 100, 60, &state);
    // The name, the tabs with Output open, and the time and tokens at the right end.
    let header = text.lines().next().unwrap_or_default();
    assert!(
        header.starts_with("◐ GPT 5.6 on Codex · worker · task-002 (Add the JSON writer)  "),
        "{header}"
    );
    assert!(header.contains(" Output  Input  Raw "), "{header}");
    assert!(header.ends_with("2m00s · ↓  12.4k"), "{header}");
    for needle in [
        "Read(src/export.rs)",
        "test export::json ... ok",
        "Update(src/export.rs)",
        "self.write_json(out)",
        "2 subagents",
        "explorer · done · Two writers",
        "its steps don't stream here",
        "The JSON writer goes beside the CSV one.",
        "⎿ Queued use serde_json",
        "arrives after the current tool call",
    ] {
        assert!(text.contains(needle), "{needle}:\n{text}");
    }
    // A read says what it read, never the file.
    assert!(!text.contains("fn write(&self) {}"), "{text}");
    // The subagent's thread hangs under it.
    assert!(position(&text, "explorer") < position(&text, "Check the tests"));

    // Following the live end is the usual state, so only a pause is said.
    assert!(!text.contains("paused"), "{text}");

    // m messages the agent, and the line shows until the message is queued.
    press(&mut view, KeyCode::Char('m'), &state);
    for c in "keep the CSV path".chars() {
        press(&mut view, KeyCode::Char(c), &state);
    }
    assert!(screen(&mut view, 100, 32, &state).contains("message ❯ keep the CSV path"));
    let steer = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&steer, ViewOutcome::Send(UiCommand::Steer { session, text, .. })
            if session.as_str() == "s-002" && text == "keep the CSV path"),
        "{steer:?}"
    );
    assert!(screen(&mut view, 100, 60, &state).contains("⎿ Sent keep the CSV path"));

    let raw = press(&mut view, KeyCode::Char('r'), &state);
    assert!(
        matches!(&raw, ViewOutcome::Request(ViewRequest::Raw { session, .. }) if session.as_str() == "s-002"),
        "{raw:?}"
    );
    let diff = press(&mut view, KeyCode::Char('d'), &state);
    assert!(
        matches!(&diff, ViewOutcome::Request(ViewRequest::Diff { task, .. }) if task.as_str() == "task-002"),
        "{diff:?}"
    );
    let take = press(&mut view, KeyCode::Char('t'), &state);
    assert!(
        matches!(&take, ViewOutcome::ConfirmTakeOver { task, .. } if task.as_str() == "task-002"),
        "{take:?}"
    );
    // Opened by a follow key, ← opens the agent panel in its place.
    assert!(matches!(
        press(&mut view, KeyCode::Left, &state),
        ViewOutcome::Replace(_)
    ));
    let mut from_crew = SessionView::from_crew(run_id(), key("s-002"));
    assert!(matches!(
        press(&mut from_crew, KeyCode::Left, &state),
        ViewOutcome::Close
    ));
}

/// Enter on a finished agent in the agent panel opens its transcript, after its run merged,
/// and the footer offers only the keys that still act.
#[test]
fn the_agent_panel_opens_a_finished_agents_transcript() {
    let mut state = session_state("The JSON writer goes beside the CSV one.");
    let run = state.runs.get_mut(&run_id()).unwrap();
    run.finished = Some(argonauts_core::runs::events::run::RunOutcome::Merged);
    // A merged run asks nothing more.
    run.approvals.clear();
    for session in run.sessions.values_mut() {
        session.status = SessionStatus::Done;
        session.ended = Some(WALL);
    }
    let mut opened = None;
    for at in 0..12 {
        let mut crew = open_crew(&state);
        crew.list.selected = at;
        let picked = screen(&mut crew, 120, 32, &state);
        // The fixture's log has no labels, so its agents go by role and task.
        if !row_with(&picked, "❯").contains("worker · task-002") {
            continue;
        }
        opened = Some(press(&mut crew, KeyCode::Enter, &state));
        break;
    }
    let Some(ViewOutcome::Open(mut view)) = opened else {
        panic!("Enter opens the agent");
    };
    let text = screen(&mut *view, 120, 32, &state);
    assert!(
        text.starts_with("✔ GPT 5.6 on Codex · worker · task-002"),
        "{text}"
    );
    for (width, height) in [(80, 24), (100, 32), (120, 32)] {
        let mut view = SessionView::from_crew(run_id(), key("s-002"));
        view.clock = WallClock::pinned(WALL);
        let text = screen(&mut view, width, height, &state);
        let footer = text.lines().last().unwrap_or_default();
        for gone in ["m message it", "t take over", "x stop it"] {
            assert!(!footer.contains(gone), "{gone}: {footer}");
        }
        insta::assert_snapshot!(format!("crew_opens_a_finished_agent_{width}"), text);
    }
}

#[test]
fn follow_mode_keeps_the_live_end_on_screen() {
    let mut state = session_state("first");
    let mut view = session_view();
    let first = screen(&mut view, 80, 24, &state);
    assert!(first.contains("⎿ Queued use serde_json"), "{first}");
    view.handle_key(
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        &ctx(&theme(), &state),
    );
    let paused = screen(&mut view, 80, 24, &state);
    assert!(paused.contains("paused · End follows"), "{paused}");
    assert!(!paused.contains("⎿ Queued use serde_json"), "{paused}");
    // New output arrives while paused; the rows you read stay put.
    let codex = state
        .runs
        .get_mut(&run_id())
        .unwrap()
        .sessions
        .get_mut(&key("s-002"))
        .unwrap();
    codex.messages.push("Streamed while you read.".into());
    codex.last_event = WALL;
    assert_eq!(
        screen(&mut view, 80, 24, &state).lines().nth(3),
        paused.lines().nth(3)
    );
    press(&mut view, KeyCode::End, &state);
    let live = screen(&mut view, 80, 24, &state);
    assert!(live.contains("Streamed while you read."), "{live}");
    assert!(!live.contains("paused"), "{live}");
}

#[test]
fn session_view_input_side() {
    let state = session_state("text");
    for (width, height) in SIZES {
        let mut view = session_view();
        press(&mut view, KeyCode::Tab, &state);
        insta::assert_snapshot!(
            format!("session_view_input_side_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = session_view();
    press(&mut view, KeyCode::Tab, &state);
    let text = screen(&mut view, 100, 32, &state);
    // The launch folds to one line under the parts, and opens to the whole command.
    assert!(
        text.contains("Launched as codex exec --json '<prompt>' · →  expands"),
        "{text}"
    );
    assert!(!text.contains("cwd /wt/task-002"), "{text}");
    // Each line of each part ends its own row, word for word and in order.
    let mut last = 0;
    for line in [
        "You own task-002: Add the JSON writer.",
        "Keep the CSV path working.",
        "Export takes --json and prints valid JSON.",
        "Use serde_json.",
        "No new dependencies.",
    ] {
        let row = text
            .lines()
            .position(|row| row.ends_with(line))
            .unwrap_or_else(|| panic!("{line:?}:\n{text}"));
        assert!(row >= last, "{line:?} in order:\n{text}");
        last = row;
    }
    // Each part is headed by where it came from, with no token estimate beside it.
    assert!(text.contains("\nTask\n  You own task-002"), "{text}");
    assert!(!text.contains("tok"), "{text}");
    assert!(position(&text, "Launched as") > last);
    assert!(position(&text, "Queued") > last);
    for _ in 0..3 {
        press(&mut view, KeyCode::Down, &state);
    }
    press(&mut view, KeyCode::Right, &state);
    let open = screen(&mut view, 100, 32, &state);
    assert!(open.contains("$ codex exec --json '<prompt>'"), "{open}");
    assert!(open.contains("cwd /wt/task-002 · prompt on stdin · sandbox worktree"));

    // ↓ selects the next part, and → expands a long one.
    let mut long = session_state("text");
    let body = (1..=9)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    long.runs
        .get_mut(&run_id())
        .unwrap()
        .sessions
        .get_mut(&key("s-002"))
        .unwrap()
        .inputs = vec![session_input(&[("task", "short"), ("rules", &body)])];
    let mut view = session_view();
    press(&mut view, KeyCode::Tab, &long);
    press(&mut view, KeyCode::Down, &long);
    let folded = screen(&mut view, 100, 32, &long);
    assert!(folded.contains("… +3 lines · →  expands"), "{folded}");
    assert!(!folded.contains("line 9"));
    press(&mut view, KeyCode::Right, &long);
    assert!(screen(&mut view, 100, 32, &long).contains("line 9"));
    // Shift+Tab goes back to Output, and Tab past Input opens the Raw tab, which reads the log.
    press(&mut view, KeyCode::BackTab, &long);
    assert!(screen(&mut view, 100, 32, &long).contains("tab input"));
    press(&mut view, KeyCode::Tab, &long);
    let raw = press(&mut view, KeyCode::Tab, &long);
    assert!(
        matches!(&raw, ViewOutcome::Request(ViewRequest::Raw { session, .. }) if session.as_str() == "s-002"),
        "{raw:?}"
    );
    assert!(screen(&mut view, 100, 32, &long).contains("tab output"));
}

fn conversation_row(n: u32, title: &str, hours_ago: i64, needs_you: bool) -> ConversationSummary {
    ConversationSummary {
        conversation: ConversationId::numbered(n),
        title: title.into(),
        updated: WALL - Duration::hours(hours_ago),
        worker: Some(worker("claude-code")),
        turns: 3,
        runs: 1,
        needs_you,
    }
}

fn picker() -> ResumePicker {
    let mut view = ResumePicker::listing(
        vec![
            conversation_row(3, "Add a --json flag to export", 1, true),
            conversation_row(2, "Fix the flaky export test", 3, false),
            conversation_row(1, "Rename the CSV writer", 50, false),
        ],
        Vec::new(),
    );
    view.clock = WallClock::pinned(WALL);
    view
}

#[test]
fn resume_picker_rows() {
    let state = crew_state("text");
    for (width, height) in SIZES {
        let mut view = picker();
        insta::assert_snapshot!(
            format!("resume_picker_rows_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = picker();
    let text = screen(&mut view, 100, 32, &state);
    assert!(text.contains("Resume a conversation · in "), "{text}");
    // A conversation whose run waits on you is marked, since the run comes back with it.
    let waiting = row_with(&text, "Add a --json flag to export");
    assert!(waiting.contains('◆'), "{text}");
    assert!(waiting.contains("Argonauts"), "{text}");
    let other = row_with(&text, "Fix the flaky export test");
    assert!(!other.contains('◆'), "{text}");
}

fn cli_session(id: &str, title: &str, branch: Option<&str>, hours_ago: i64) -> CliSession {
    CliSession {
        agent_id: AgentId("claude-code".into()),
        id: id.into(),
        title: title.into(),
        prompt: Some(title.into()),
        folder: PathBuf::from("/work/shop"),
        updated: WALL - Duration::hours(hours_ago),
        model: Some("claude-opus-5-5".into()),
        branch: branch.map(Into::into),
        last_message: None,
        turns: 3,
    }
}

fn picker_with_sessions(title: &str) -> ResumePicker {
    let mut view = ResumePicker::listing(
        vec![ConversationSummary {
            conversation: ConversationId::numbered(2),
            title: "what does export do?".into(),
            updated: WALL - Duration::hours(2),
            worker: Some(worker("claude-code")),
            turns: 3,
            runs: 0,
            needs_you: false,
        }],
        vec![
            cli_session(
                "0a1b2c3d-1111-4aaa-8bbb-000000000001",
                title,
                Some("main"),
                0,
            ),
            cli_session(
                "0a1b2c3d-2222-4aaa-8bbb-000000000002",
                "Rename the CSV writer",
                None,
                26,
            ),
        ],
    );
    view.clock = WallClock::pinned(WALL);
    view
}

#[test]
fn resume_picker_with_cli_sessions() {
    let state = crew_state("text");
    for width in [80, 100, 120, 200] {
        let mut view = picker_with_sessions("Explain the input loop");
        insta::assert_snapshot!(
            format!("resume_picker_cli_sessions_{width}"),
            screen(&mut view, width, 24, &state)
        );
    }
    let mut view = picker_with_sessions("Explain the input loop");
    let text = screen(&mut view, 120, 24, &state);
    assert!(text.contains("Resume a conversation · in "), "{text}");
    let newest = row_with(&text, "Explain the in");
    for cell in ["Claude Code", "just now", "3 turns"] {
        assert!(newest.contains(cell), "{cell}:\n{text}");
    }
    assert!(position(&text, "Explain the in") < position(&text, "what does export do?"));
    assert!(
        text.contains("enter continues it · b branches from it, the original untouched"),
        "{text}"
    );
    // Its CLI wrote to it just now, so the first Enter asks and the second carries it on.
    let asked = press(&mut view, KeyCode::Enter, &state);
    assert!(matches!(&asked, ViewOutcome::Consumed), "{asked:?}");
    assert!(screen(&mut view, 120, 24, &state).contains("may still be open there"));
    let resume = press(&mut view, KeyCode::Enter, &state);
    assert!(
        matches!(&resume, ViewOutcome::Send(UiCommand::Slash { .. })),
        "{resume:?}"
    );

    // Every width holds its frame whatever the titles hold.
    for sample in samples::all() {
        for width in WIDTHS {
            let mut view = picker_with_sessions(&sample);
            screen(&mut view, width, 24, &state);
        }
    }
}

fn diff_data(text: &str) -> DiffView {
    DiffView {
        task: TaskId::numbered(2),
        stat: DiffStat {
            files_changed: 1,
            insertions: 3,
            deletions: 1,
        },
        text: text.into(),
        path: PathBuf::from("/state/tasks/task-002/diff"),
    }
}

const DIFF: &str = "diff --git a/src/export.rs b/src/export.rs
index 3f1c2d0..9a8b7c6 100644
--- a/src/export.rs
+++ b/src/export.rs
@@ -10,3 +10,5 @@ impl Export {
     fn write(&self, out: &mut dyn Write) -> Result<()> {
-        self.write_csv(out)
+        match self.format {
+            Format::Csv => self.write_csv(out),
+            Format::Json => self.write_json(out),
     }";

#[test]
fn diff_pager() {
    let state = AppState::default();
    for (width, height) in SIZES {
        let mut view = Pager::diff(diff_data(DIFF));
        insta::assert_snapshot!(
            format!("diff_pager_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = Pager::diff(diff_data(DIFF));
    let buf = draw(&mut view, 100, 32, &state);
    let text = buffer_text(&buf);
    assert!(text.contains("Diff · task-002"));
    assert!(text.contains("1 file changed · +3 −1"));
    let theme = theme();
    let style_of = |needle: &str| {
        let row = text.lines().position(|row| row.contains(needle)).unwrap();
        let col = text.lines().nth(row).unwrap().find(needle).unwrap();
        let x = text.lines().nth(row).unwrap()[..col].chars().count() as u16;
        buf[(x, row as u16)].style()
    };
    assert_eq!(
        style_of("+        match").bg,
        theme.style(Role::AddedRow).bg
    );
    assert_eq!(
        style_of("-        self.write_csv").bg,
        theme.style(Role::RemovedRow).bg
    );
    assert_eq!(style_of("@@ -10,3").fg, theme.style(Role::Accent).fg);

    // It scrolls, and says which rows show.
    let long = (0..80)
        .map(|n| format!("+line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut view = Pager::diff(diff_data(&long));
    assert!(screen(&mut view, 80, 24, &state).contains("1-18 of 80 ↓"));
    press(&mut view, KeyCode::PageDown, &state);
    assert!(screen(&mut view, 80, 24, &state).contains("19-36 of 80 ↓"));
    press(&mut view, KeyCode::End, &state);
    let end = screen(&mut view, 80, 24, &state);
    assert!(
        end.contains("63-80 of 80") && end.contains("+line 79"),
        "{end}"
    );
    assert!(matches!(
        press(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Close
    ));
}

fn raw_data(lines: &[&str]) -> RawView {
    RawView {
        session: key("s-002"),
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        truncated: true,
        path: PathBuf::from("/state/raw/s-002.jsonl"),
        cli_transcript: None,
    }
}

#[test]
fn raw_pager_is_sanitized() {
    let state = AppState::default();
    let lines = [
        "{\"type\":\"thread.started\",\"thread_id\":\"th_01\"}",
        "\u{1b}[31mred text\u{1b}[0m and \u{1b}]0;owned title\u{7}a title",
        "\u{1b}[2J\u{1b}[Hcleared\rreturned\ttabbed",
        "bidi \u{202e}reversed\u{202c} end \u{9b}31mC1",
    ];
    for (width, height) in SIZES {
        let mut view = Pager::raw(raw_data(&lines));
        insta::assert_snapshot!(
            format!("raw_pager_is_sanitized_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut view = Pager::raw(raw_data(&lines));
    let buf = draw(&mut view, 100, 32, &state);
    for cell in buf.content() {
        assert!(
            !cell
                .symbol()
                .chars()
                .any(|c| c.is_control() || c == '\u{202e}'),
            "{:?} reached the screen",
            cell.symbol()
        );
    }
    let text = buffer_text(&buf);
    assert!(text.contains("red text and a title"), "{text}");
    assert!(!text.contains("owned title"), "{text}");
    assert!(text.contains("thread.started"), "{text}");
    assert!(text.contains("the last 4 lines"), "{text}");
}

#[test]
fn help_view() {
    let state = AppState::default();
    for (width, height) in SIZES {
        let mut view = HelpView::new(Keymap::default());
        insta::assert_snapshot!(
            format!("help_view_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
    let mut keymap = Keymap::default();
    keymap.rebind(
        argonauts_tui::base::input::keymap::Action::Transcript,
        vec![argonauts_tui::base::input::keymap::KeySpec::parse("ctrl+y").unwrap()],
    );
    let mut view = HelpView::new(keymap);
    let mut seen = String::new();
    for _ in 0..20 {
        seen.push_str(&screen(&mut view, 100, 32, &state));
        press(&mut view, KeyCode::PageDown, &state);
    }
    assert!(seen.contains("ctrl+y"), "the keymap in effect:\n{seen}");
    let every = argonauts_core::input::SLASH_COMMANDS
        .iter()
        .chain(argonauts_core::input::SHARED_COMMANDS);
    for command in every {
        assert!(
            seen.contains(&format!("/{}", command.name)),
            "/{}",
            command.name
        );
    }
    assert!(matches!(
        press(&mut view, KeyCode::Esc, &state),
        ViewOutcome::Close
    ));
}

#[test]
fn overlays_hold_their_frames_at_every_width() {
    let long_word = samples::long_word();
    let texts = [
        samples::CJK,
        samples::EMOJI,
        samples::LONG_PATH,
        samples::EMPTY,
        long_word.as_str(),
    ];
    for text in texts {
        let state = session_state(text);
        let mut listed = state.clone();
        listed.agent_panel = Some(fetched_panel(text));
        let diff = format!("@@ -1 +1 @@\n-{text}\n+{text}\n {}", samples::CJK);
        let raw = [text, samples::EMOJI];
        for width in WIDTHS {
            for height in [12, 24] {
                let mut crew = open_crew(&state);
                assert_frames(&draw(&mut crew, width, height, &state));
                press(&mut crew, KeyCode::Down, &state);
                press(&mut crew, KeyCode::Char(' '), &state);
                assert_frames(&draw(&mut crew, width, height, &state));

                let mut panel = open_crew(&listed);
                assert_frames(&draw(&mut panel, width, height, &listed));
                for _ in 0..6 {
                    press(&mut panel, KeyCode::Down, &listed);
                }
                press(&mut panel, KeyCode::Char(' '), &listed);
                assert_frames(&draw(&mut panel, width, height, &listed));
                press(&mut panel, KeyCode::Char('?'), &listed);
                assert_frames(&draw(&mut panel, width, height, &listed));

                let mut output = session_view();
                assert_frames(&draw(&mut output, width, height, &state));
                press(&mut output, KeyCode::PageUp, &state);
                assert_frames(&draw(&mut output, width, height, &state));
                press(&mut output, KeyCode::Tab, &state);
                press(&mut output, KeyCode::Right, &state);
                assert_frames(&draw(&mut output, width, height, &state));

                let mut resume = picker();
                resume.conversations[0].title = text.into();
                resume.conversations[1].title = text.into();
                assert_frames(&draw(&mut resume, width, height, &state));

                assert_frames(&draw(
                    &mut Pager::diff(diff_data(&diff)),
                    width,
                    height,
                    &state,
                ));
                assert_frames(&draw(
                    &mut Pager::raw(raw_data(&raw)),
                    width,
                    height,
                    &state,
                ));
                assert_frames(&draw(
                    &mut Pager::text(text, vec![text.to_owned()]),
                    width,
                    height,
                    &state,
                ));
                assert_frames(&draw(
                    &mut HelpView::new(Keymap::default()),
                    width,
                    height,
                    &state,
                ));
            }
        }
    }
    // Columns hold while the selection moves.
    let state = crew_state("text");
    let mut view = open_crew(&state);
    let before = screen(&mut view, 100, 24, &state);
    press(&mut view, KeyCode::Down, &state);
    let after = screen(&mut view, 100, 24, &state);
    let column = |text: &str, needle: &str| {
        let row = row_with(text, needle);
        row.find(needle).map(|at| row[..at].chars().count())
    };
    assert_eq!(column(&before, "Update(src"), column(&after, "Update(src"));
    assert_eq!(column(&before, "[ All 4 ]"), column(&after, "[ All 4 ]"));
}

#[test]
fn rewind_picker() {
    use argonauts_core::app::views::CheckpointRow;
    use argonauts_tui::views::rewind::RewindPicker;
    let row = |task: &str, n, to: &str, rewound_here| CheckpointRow {
        task: TaskId::new(task).unwrap(),
        n,
        at: WALL - Duration::hours(2),
        commit: "0123456789abcdef".to_owned(),
        from: worker("claude-code/opus"),
        to: vec![worker(to)],
        reason: "usage limit, resets at 3:45 PM".to_owned(),
        rewound_here,
    };
    let state = AppState::default();
    for (width, height) in SIZES {
        let mut view = RewindPicker::new(
            run_id(),
            None,
            vec![
                row("task-001", 1, "codex", true),
                row("task-002", 1, "antigravity-cli", false),
                row("task-001", 2, "opencode", false),
            ],
        );
        view.clock = WallClock::pinned(WALL);
        insta::assert_snapshot!(
            format!("rewind_picker_{width}"),
            screen(&mut view, width, height, &state)
        );
    }
}

/// A part's kind by its label, as the engine sends them: the task goes as a task.
fn part_kind(label: &str) -> &str {
    match label {
        "goal" | "notes" | "rules" | "status" => label,
        _ => "task",
    }
}
