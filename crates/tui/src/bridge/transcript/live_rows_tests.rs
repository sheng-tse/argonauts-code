//! A call's row appears as the model starts writing it, fills in where it stands, and gets its
//! result in place. A call the model never finished goes.

use std::time::Instant;

use argonauts_core::app::{AppEvent, ConversationEvent};
use argonauts_core::names::{ModelRef, TurnId};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::{AgentEvent, ErrorCategory, Verbosity};
use serde_json::{Value, json};
use time::macros::datetime;

use super::Transcript;
use crate::bridge::{Cx, Effects};
use crate::format::WallClock;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};
use ratatui::text::Line;

pub(super) struct Screen {
    pub model: ViewModel,
    pub state: AppState,
    pub settings: UiSettings,
    transcript: Transcript,
    turn: TurnId,
    /// Rows printed so far, as the terminal's scrollback holds them.
    printed: Vec<Line<'static>>,
    glyphs: Glyphs,
}

impl Screen {
    pub fn new() -> Self {
        Self {
            model: ViewModel::default(),
            state: AppState::default(),
            settings: UiSettings::default(),
            transcript: Transcript::with_clock(WallClock::pinned(datetime!(2026-10-09 10:42 UTC))),
            turn: TurnId::numbered(1),
            printed: Vec::new(),
            glyphs: Glyphs::UNICODE,
        }
    }

    /// A screen that draws as on macOS, where items start with Claude Code's `⏺`, as the
    /// pictures do.
    pub fn on_macos() -> Self {
        Self {
            glyphs: Glyphs::MACOS,
            ..Self::new()
        }
    }

    pub fn send(&mut self, event: ConversationEvent) {
        self.app(AppEvent::Conversation { event });
    }

    /// An event of the app's own, such as what a command says it did.
    pub fn app(&mut self, event: AppEvent) {
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out: &mut out,
        };
        self.transcript.on_event(&event, &mut cx);
    }

    /// An event the screen's state takes in first, as the app's does, and then prints.
    pub fn seen(&mut self, event: AppEvent) {
        self.state.apply(&event);
        self.app(event);
    }

    pub fn ask(&mut self, text: &str) {
        self.send(ConversationEvent::TurnStarted {
            turn: self.turn.clone(),
            text: text.into(),
            worker: ModelRef::new("claude-code", Some("haiku".into())),
            read_only: false,
            commit: "8b41d07".into(),
            uncommitted: vec![],
            settings_sha256: None,
            continues: false,
        });
    }

    pub fn agent(&mut self, event: AgentEvent) {
        self.send(ConversationEvent::Agent {
            turn: self.turn.clone(),
            event,
        });
    }

    pub fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    /// What prints, then the live region under it.
    pub fn lines(&mut self, width: u16) -> Vec<Line<'static>> {
        let mut theme = Self::theme();
        theme.glyphs = self.glyphs.clone();
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        let printed = self.model.history.drain_printable(&ctx);
        self.printed.extend(printed);
        let mut lines = self.printed.clone();
        for block in self.model.history.live_blocks(&ctx) {
            if block.gap_before && !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(block.lines);
        }
        lines
    }

    /// [`Screen::lines`] as text, one string per row.
    pub fn rows(&mut self, width: u16) -> Vec<String> {
        self.lines(width)
            .iter()
            .map(|line| plain_text(std::slice::from_ref(line)))
            .collect()
    }

    pub fn text(&mut self, width: u16) -> String {
        self.rows(width).join("\n")
    }
}

pub(super) fn pending(id: &str, name: &str) -> AgentEvent {
    AgentEvent::ToolPending {
        id: id.into(),
        name: name.into(),
        parent: None,
    }
}

pub(super) fn started(id: &str, name: &str, input: Value) -> AgentEvent {
    AgentEvent::ToolStarted {
        id: id.into(),
        name: name.into(),
        input,
        parent: None,
    }
}

pub(super) fn finished(id: &str, output: &str) -> AgentEvent {
    AgentEvent::ToolFinished {
        id: id.into(),
        ok: true,
        output: Some(output.into()),
        parent: None,
    }
}

fn dropped(id: &str) -> AgentEvent {
    AgentEvent::ToolDropped {
        id: id.into(),
        parent: None,
    }
}

#[test]
fn a_row_appears_as_the_model_starts_writing_the_call() {
    let mut screen = Screen::new();
    screen.ask("fix the rounding");
    screen.agent(pending("e1", "Edit"));
    let rows = screen.text(80);
    assert!(rows.ends_with("● Update"), "its name alone:\n{rows}");

    let edit = json!({"file_path": "src/money.py", "old_string": "a\n", "new_string": "b\n"});
    screen.agent(started("e1", "Edit", edit));
    let rows = screen.text(80);
    assert!(
        rows.ends_with("● Update(src/money.py)\n  ⎿  Updating…"),
        "filled in where it stood, and nothing of its diff yet:\n{rows}"
    );
    assert_eq!(rows.matches("● Update").count(), 1, "{rows}");

    screen.agent(AgentEvent::FilePatch {
        id: "e1".into(),
        path: "src/money.py".into(),
        hunks: "@@ -4,1 +4,1 @@\n-a\n+b\n".into(),
        parent: None,
    });
    screen.agent(finished("e1", "The file has been updated."));
    let rows = screen.text(80);
    assert!(
        rows.ends_with(
            "● Update(src/money.py)\n  ⎿  Added 1 line, removed 1 line\n      4 -a\n      4 +b"
        ),
        "its result in place, with the file's line numbers:\n{rows}"
    );
    assert_eq!(rows.matches("● Update").count(), 1, "{rows}");
}

#[test]
fn an_edit_without_a_patch_shows_its_preview_once_it_ends() {
    let mut screen = Screen::new();
    screen.ask("fix the rounding");
    screen.agent(pending("e1", "Edit"));
    let edit = json!({"file_path": "src/money.py", "old_string": "a\n", "new_string": "b\n"});
    screen.agent(started("e1", "Edit", edit));
    screen.agent(finished("e1", "The file has been updated."));
    let rows = screen.text(80);
    assert!(
        rows.ends_with(
            "● Update(src/money.py)\n  ⎿  Added 1 line, removed 1 line\n        -a\n        +b"
        ),
        "{rows}"
    );
}

#[test]
fn a_call_the_model_never_finished_goes() {
    let mut screen = Screen::new();
    screen.ask("write the notes");
    screen.agent(pending("w1", "Write"));
    assert!(screen.text(80).contains("● Write"));
    screen.agent(dropped("w1"));
    let rows = screen.text(80);
    assert!(!rows.contains("Write"), "{rows}");
}

#[test]
fn a_quiet_call_being_written_joins_the_fold_and_leaves_it_when_dropped() {
    let mut screen = Screen::new();
    screen.settings.verbosity = Verbosity::Normal;
    screen.ask("look around");
    screen.agent(started("r1", "Read", json!({"file_path": "a.py"})));
    screen.agent(finished("r1", "1\tx"));
    screen.agent(pending("r2", "Read"));
    let rows = screen.text(80);
    assert!(rows.contains("Reading"), "{rows}");
    screen.agent(dropped("r2"));
    let rows = screen.text(80);
    assert!(!rows.contains("Reading"), "{rows}");
    assert!(rows.contains("Read 1 file"), "{rows}");
}

#[test]
fn esc_takes_the_call_being_written_with_it() {
    let mut screen = Screen::new();
    screen.ask("run the tests");
    screen.agent(pending("b1", "Bash"));
    screen.agent(AgentEvent::Error {
        category: ErrorCategory::Interrupted,
        message: "interrupted".into(),
        retryable: false,
    });
    let rows = screen.text(80);
    assert!(!rows.contains("Bash"), "{rows}");
    assert!(rows.contains("Interrupted"), "{rows}");
}

#[test]
fn a_call_shows_once_from_written_to_done() {
    let mut screen = Screen::new();
    screen.ask("run the tests");
    screen.agent(pending("b1", "Bash"));
    screen.agent(started("b1", "Bash", json!({"command": "ls"})));
    screen.agent(finished("b1", "a"));
    let rows = screen.text(80);
    assert_eq!(rows.matches("● Bash").count(), 1, "{rows}");
}

/// Paths read as Claude Code shows them, whichever CLI named them.
#[test]
fn paths_show_from_the_folder_then_home_and_whole_outside_both() {
    use crate::bridge::tools::Places;
    let places = Places {
        folder: Some("/Users/me/code/captain".into()),
        home: Some("/Users/me".into()),
    };
    for (path, shown) in [
        (
            "/Users/me/code/captain/invoice/money.py",
            "invoice/money.py",
        ),
        ("/Users/me/code/captain", "."),
        ("/Users/me/code/captain-two/a.py", "~/code/captain-two/a.py"),
        ("/Users/me/.zshrc", "~/.zshrc"),
        ("/etc/hosts", "/etc/hosts"),
        ("file:///Users/me/code/captain/README.md", "README.md"),
        (
            "/Users/me/.argonauts/worktrees/captain/run-01/task-002/src/lib.rs",
            "src/lib.rs",
        ),
        ("invoice/money.py", "invoice/money.py"),
    ] {
        assert_eq!(places.show(path), shown, "{path}");
    }
}

#[test]
fn a_codex_edit_names_its_files_from_the_folder_too() {
    use crate::bridge::tools::{Places, start_call_in};
    let places = Places {
        folder: Some("/work/captain".into()),
        home: Some("/home/you".into()),
    };
    let changes = json!({"changes": [
        {"path": "/work/captain/invoice/money.py", "kind": "update"},
        {"path": "/home/you/notes.md", "kind": "update"},
    ]});
    let call = start_call_in("codex", "file_change", &changes, &places);
    assert_eq!(
        call.title.plain(),
        "Update(invoice/money.py, ~/notes.md)",
        "{call:#?}"
    );
}

/// What the session asks of Argonauts Code reads as a plain line, never a tool's internal name, and
/// its answer, which is for the session, stays out.
#[test]
fn argonauts_own_calls_read_as_plain_lines() {
    let mut screen = Screen::new();
    screen.ask("how did codex review last week?");
    let lookup = "mcp__argonauts_status__argonauts_lookup";
    screen.agent(pending("l1", lookup));
    screen.agent(started("l1", lookup, json!({"what": "reviews", "days": 7})));
    screen.agent(finished(
        "l1",
        "{\"reviews\": [{\"model\": \"gpt-5.6-luna\"}]}",
    ));
    let rows = screen.text(80);
    assert!(
        rows.contains("● Looked up reviews in Argonauts Code's records"),
        "{rows}"
    );
    for internal in ["argonauts_", "(MCP)", "gpt-5.6-luna", "⎿"] {
        assert!(!rows.contains(internal), "{internal}:\n{rows}");
    }
}

/// The start tool's own words when it starts nothing: one line, with no break after its first
/// sentence, as the Captain run's first start got them.
const WHY_NOT: &str = "No workflow started. With Captain, one starts only when you asked for a \
    workflow in this message or the one before, and you said how it should run (how_to_run).";

impl Screen {
    /// The main agent's turn ends with `said`.
    fn answered(&mut self, said: &str) {
        self.send(ConversationEvent::TurnFinished {
            turn: self.turn.clone(),
            reply: argonauts_core::app::TurnReply::Answer {
                markdown: said.into(),
            },
            spend: argonauts_core::Spend::default(),
            elapsed_ms: 4_000,
        });
    }
}

/// Starting a workflow shows as the session's line and the workflow's row, so its call draws
/// nothing, unless Argonauts Code turned it down and no later start in the turn worked: then
/// one dim line, in the refused look, and none of the words meant for the session.
#[test]
fn a_start_draws_a_line_only_when_turned_down() {
    let start = "mcp__argonauts_status__argonauts_start_workflow";
    let mut screen = Screen::new();
    screen.ask("use a workflow for this");
    screen.agent(pending("s1", start));
    screen.agent(started("s1", start, json!({"request": "fix the money"})));
    screen.agent(finished("s1", "Starting a workflow: Orchestrator."));
    screen.answered("Started.");
    let rows = screen.text(80);
    assert!(!rows.contains("argonauts_"), "{rows}");
    assert!(!rows.contains("No workflow started"), "{rows}");

    let mut screen = Screen::new();
    screen.ask("use a workflow for this");
    screen.agent(started(
        "s1",
        start,
        json!({"request": "fix the money", "who": "reviewer"}),
    ));
    screen.agent(finished("s1", WHY_NOT));
    screen.answered("It needs how it should run.");
    let rows = screen.text(80);
    assert!(rows.contains("No workflow started"), "{rows}");
    assert!(!rows.contains("With Captain"), "{rows}");
    assert!(!rows.contains("argonauts_"), "{rows}");
    let danger = Screen::theme().style(crate::theme::Role::Danger).fg;
    let refused = screen
        .lines(80)
        .into_iter()
        .find(|line| plain_text(std::slice::from_ref(line)).contains("No workflow started"))
        .expect("the line");
    assert!(
        refused.spans.iter().all(|span| span.style.fg != danger),
        "{refused:?}"
    );
    assert!(!rows.contains("failed"), "{rows}");
}

/// The first start was turned down and the retry in the same turn started it: the Workflow
/// row says it started, and the refusal shows only in the full transcript.
#[test]
fn a_refused_start_whose_retry_worked_draws_nothing() {
    let start = "mcp__argonauts_status__argonauts_start_workflow";
    let mut screen = Screen::new();
    screen.ask("Use a workflow for this, with Codex as the reviewer");
    screen.agent(started("s1", start, json!({"request": "fix the money"})));
    screen.agent(finished("s1", WHY_NOT));
    screen.agent(started(
        "s2",
        start,
        json!({"request": "fix the money", "how_to_run": "captain"}),
    ));
    screen.agent(finished("s2", "Starting a workflow: Captain."));
    screen.answered("Starting a workflow: Captain.");
    let rows = screen.text(80);
    assert!(!rows.contains("No workflow started"), "{rows}");
    assert!(!rows.contains("With Captain"), "{rows}");
}

/// A lookup of held-back tools never shows, as in Claude Code.
#[test]
fn a_tool_lookup_never_shows() {
    let mut screen = Screen::new();
    screen.ask("start it");
    screen.agent(pending("t1", "ToolSearch"));
    screen.agent(started(
        "t1",
        "ToolSearch",
        json!({"query": "select:mcp__argonauts_status__argonauts_start_workflow"}),
    ));
    screen.agent(finished("t1", "<functions>…</functions>"));
    let rows = screen.text(80);
    assert!(!rows.contains("ToolSearch"), "{rows}");
    assert!(!rows.contains("select:"), "{rows}");
}

/// Claude Code streams every call's start before its input, so a shell call's command shows
/// only once it's whole. A read and two commands that only look around still make one fold
/// line, and a test run written after them leaves it for a row of its own.
#[test]
fn reads_and_look_around_commands_written_one_by_one_fold_into_one_line() {
    let mut screen = Screen::new();
    screen.settings.verbosity = Verbosity::Normal;
    screen.ask("Fix the money rounding.");
    screen.agent(pending("r1", "Read"));
    screen.agent(started(
        "r1",
        "Read",
        json!({"file_path": "invoice/money.py"}),
    ));
    screen.agent(finished("r1", "1\tcents"));
    for (id, command) in [
        ("b1", "ls invoice tests"),
        ("b2", "cat tests/test_money.py"),
    ] {
        screen.agent(pending(id, "Bash"));
        screen.agent(started(id, "Bash", json!({"command": command})));
        screen.agent(finished(id, "ok"));
    }
    screen.agent(pending("b3", "Bash"));
    screen.agent(started(
        "b3",
        "Bash",
        json!({"command": "python3 -m unittest -q"}),
    ));
    screen.agent(finished("b3", "Ran 8 tests · OK"));
    let rows = screen.rows(100);
    let folds: Vec<&String> = rows
        .iter()
        .filter(|row| row.contains("Read 1 file") || row.contains("shell command"))
        .collect();
    assert_eq!(folds.len(), 1, "{rows:#?}");
    assert!(
        folds[0].contains("Read 1 file, ran 2 shell commands"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("Bash(python3 -m unittest -q)")),
        "the test run keeps its own row: {rows:#?}"
    );
}

/// A turn that runs one of the CLI's own commands echoes the command, and that echo is never
/// the prompt kept at the top when you scroll up. Words that only start with a slash are.
#[test]
fn a_cli_command_is_never_kept_at_the_top_as_your_prompt() {
    for (asked, kept) in [("/ship 12", 0), ("/tmp/out.log is empty, why?", 1)] {
        let mut screen = Screen::new();
        screen.state.cli_commands = Some(crate::commands::tests::claude_commands());
        screen.ask(asked);
        screen.lines(100);
        let prompts = screen.model.history.take_prompt_rows();
        assert_eq!(prompts.len(), kept, "{asked}: {prompts:?}");
    }
}
