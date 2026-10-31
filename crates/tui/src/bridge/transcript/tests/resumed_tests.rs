//! A resumed conversation and a command's report, as they print.

use std::time::Instant;

use argonauts_core::Verbosity;
use argonauts_core::agents::cli_sessions::{CliCall, CliPatch, CliStep};
use argonauts_core::app::{AppEvent, ReplayTurn};
use argonauts_core::base::names::{ConversationId, ModelRef, SessionKey};
use argonauts_core::runs::events::run::Party;
use argonauts_core::settings::ui::UiSettings;
use serde_json::json;

use super::super::Transcript;
use crate::app::model::ViewModel;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::text::plain_text;
use crate::base::theme::{Glyphs, Theme};
use crate::bridge::{Cx, Effects};
use crate::cells::cell::HistoryCell as _;
use crate::frame::state::AppState;

#[derive(Default)]
struct Screen {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    transcript: Transcript,
    closed_views: bool,
}

impl Screen {
    fn send(&mut self, event: AppEvent) {
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out: &mut out,
        };
        self.transcript.on_event(&event, &mut cx);
        self.closed_views |= out.close_views;
    }

    fn printed(&mut self) -> String {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        plain_text(&self.model.history.drain_printable(&ctx))
    }
}

fn you(text: &str) -> ReplayTurn {
    ReplayTurn {
        from: Party::User,
        text: text.to_owned(),
        steps: Vec::new(),
    }
}

fn agent(text: &str) -> ReplayTurn {
    ReplayTurn {
        from: Party::Agent {
            session: SessionKey::parse("t-001").unwrap(),
            worker: ModelRef::new("claude-code", None),
        },
        text: text.to_owned(),
        steps: Vec::new(),
    }
}

/// A session carried on prints every turn as it was, with nothing above it to say so, as
/// Claude Code's resume draws the old messages on its own screen.
#[test]
fn a_resumed_session_prints_every_turn_as_it_was() {
    let mut screen = Screen::default();
    screen.printed();
    screen.send(AppEvent::ConversationResumed {
        conversation: ConversationId::numbered(3),
        title: "hi".to_owned(),
        worker: Some(ModelRef::new("claude-code", None)),
        turns: vec![
            you("hi"),
            agent("Hi. Ask me anything."),
            you("what does export do?"),
            agent("It writes CSV."),
        ],
        artifacts: Vec::new(),
    });
    let printed = screen.printed();
    assert_eq!(
        printed,
        "❯ hi\n\n● Hi. Ask me anything.\n\n❯ what does export do?\n\n● It writes CSV."
    );
    assert!(
        screen.closed_views,
        "the picker gives the screen to the conversation"
    );
}

/// A turn of a Claude Code session: it thought, read a file, said so, edited it, ran a failing
/// check, and answered.
fn worked() -> ReplayTurn {
    let read = CliCall {
        ok: Some(true),
        output: Some("fn total() {}".to_owned()),
        ..CliCall::new("t1", "Read", json!({"file_path": "src/tax.rs"}))
    };
    let edit = CliCall {
        ok: Some(true),
        output: Some("updated".to_owned()),
        patches: vec![CliPatch {
            path: "src/tax.rs".to_owned(),
            hunks: "@@ -7,1 +7,1 @@\n-    total.round()\n+    total.floor()\n".to_owned(),
            numbered: true,
        }],
        ..CliCall::new(
            "t2",
            "Edit",
            json!({"file_path": "src/tax.rs", "old_string": "round()", "new_string": "floor()"}),
        )
    };
    let check = CliCall {
        ok: Some(false),
        output: Some("1 test failed".to_owned()),
        ..CliCall::new("t3", "Bash", json!({"command": "cargo test tax"}))
    };
    ReplayTurn {
        steps: vec![
            CliStep::Thought {
                text: "Round down,\nnot half up.".to_owned(),
            },
            CliStep::Ran(read),
            CliStep::Wrote {
                text: "It rounds half up.".to_owned(),
            },
            CliStep::Ran(edit),
            CliStep::Ran(check),
            CliStep::Wrote {
                text: "One test still fails.".to_owned(),
            },
        ],
        ..agent("It rounds half up.\n\nOne test still fails.")
    }
}

fn resumed_with(turn: ReplayTurn) -> AppEvent {
    AppEvent::ConversationResumed {
        conversation: ConversationId::numbered(1),
        title: "Fix the rounding".to_owned(),
        worker: Some(ModelRef::new("claude-code", None)),
        turns: vec![you("fix the rounding"), turn],
        artifacts: Vec::new(),
    }
}

#[test]
fn a_cli_sessions_turn_reprints_its_calls_and_diffs_where_they_happened() {
    let mut screen = Screen::default();
    screen.settings.verbosity = Verbosity::Full;
    screen.printed();
    screen.send(resumed_with(worked()));
    let printed = screen.printed();
    assert_eq!(
        printed,
        "\
         ❯ fix the rounding\n\n\
         ● Read(src/tax.rs)\n  ⎿  Read 1 line\n\n\
         ● It rounds half up.\n\n\
         ● Update(src/tax.rs)\n  ⎿  Added 1 line, removed 1 line\n\
         \x20     7 -    total.round()\n\
         \x20     7 +    total.floor()\n\n\
         ● Bash(cargo test tax)\n  ⎿  1 test failed\n\n\
         ● One test still fails."
    );
    // What it thought never prints, and Ctrl+O has it.
    assert!(!printed.contains("Round down"), "{printed}");
    let theme = Theme::dark();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let thought = screen
        .model
        .history
        .cells()
        .iter()
        .map(|cell| plain_text(&cell.lines(&ctx)))
        .find(|text| text.contains("Round down"));
    assert_eq!(thought.as_deref(), Some("∴ Round down, not half up."));
}

#[test]
fn reprinted_calls_follow_the_verbosity_as_live_ones_do() {
    let mut screen = Screen::default();
    screen.settings.verbosity = Verbosity::Normal;
    screen.printed();
    screen.send(resumed_with(worked()));
    let normal = screen.printed();
    // The read folds into one line, and the edit and the failed check keep their rows.
    assert!(normal.contains("Read 1 file"), "{normal}");
    assert!(!normal.contains("● Read(src/tax.rs)"), "{normal}");
    assert!(normal.contains("● Update(src/tax.rs)"), "{normal}");

    let mut screen = Screen::default();
    screen.settings.verbosity = Verbosity::Quiet;
    screen.printed();
    screen.send(resumed_with(worked()));
    let quiet = screen.printed();
    assert!(!quiet.contains("src/tax.rs"), "{quiet}");
    assert!(
        quiet.ends_with("● It rounds half up.\n\n● One test still fails."),
        "{quiet}"
    );
}

/// A turn in which Claude Code sent a subagent to look, which searched and had one of its own
/// read the file.
fn delegated() -> ReplayTurn {
    let done = |call: CliCall, output: &str| CliCall {
        ok: Some(true),
        output: Some(output.to_owned()),
        ..call
    };
    let read = done(
        CliCall::new("t3", "Read", json!({"file_path": "src/tax.rs"})),
        "fn total() {}",
    );
    let reader = CliCall {
        steps: vec![CliStep::Ran(read)],
        ..done(
            CliCall::new("t2", "Agent", json!({"description": "Read it"})),
            "It rounds half up.",
        )
    };
    let search = done(
        CliCall::new("t1", "Bash", json!({"command": "rg round src"})),
        "src/tax.rs:7",
    );
    let explorer = CliCall {
        steps: vec![
            CliStep::Ran(search),
            CliStep::Ran(reader),
            CliStep::Wrote {
                text: "It is in src/tax.rs.".to_owned(),
            },
        ],
        ..done(
            CliCall::new("t0", "Agent", json!({"description": "Find the rounding"})),
            "It is in src/tax.rs.",
        )
    };
    ReplayTurn {
        steps: vec![
            CliStep::Ran(explorer),
            CliStep::Wrote {
                text: "In src/tax.rs.".to_owned(),
            },
        ],
        ..agent("In src/tax.rs.")
    }
}

#[test]
fn a_subagents_calls_fold_into_its_row_and_ctrl_o_shows_each() {
    let mut screen = Screen::default();
    screen.printed();
    screen.send(resumed_with(delegated()));
    let printed = screen.printed();
    assert_eq!(
        printed,
        "\
         ❯ fix the rounding\n\n\
         ● Task(Find the rounding)\n  ⎿  Done (3 tool uses)\n\n\
         ● In src/tax.rs."
    );
    let theme = Theme::dark();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now())).with_expanded(true);
    let whole = screen
        .model
        .history
        .cells()
        .iter()
        .map(|cell| plain_text(&cell.lines(&ctx)))
        .find(|text| text.contains("Find the rounding"))
        .expect("the subagent's row is kept");
    assert_eq!(
        whole,
        "● Task(Find the rounding)\n  ⎿  Bash(rg round src)\n        src/tax.rs:7\n\
         \x20    Task(Read it)\n        Read(src/tax.rs)\n           Read 1 line\n\
         \x20       Done (1 tool use)\n     Done (3 tool uses)"
    );
}

#[test]
fn a_patch_with_no_line_numbers_draws_without_them() {
    let patch = CliCall {
        ok: Some(true),
        patches: vec![CliPatch {
            path: "src/tax.rs".to_owned(),
            hunks: "@@ -1,1 +1,1 @@\n-    total.round()\n+    total.floor()\n".to_owned(),
            numbered: false,
        }],
        ..CliCall::new(
            "c1",
            "file_change",
            json!({"changes": [{"path": "src/tax.rs", "kind": "update"}]}),
        )
    };
    let turn = ReplayTurn {
        from: Party::Agent {
            session: SessionKey::parse("t-001").unwrap(),
            worker: ModelRef::new("codex", None),
        },
        text: String::new(),
        steps: vec![CliStep::Ran(patch)],
    };
    let mut screen = Screen::default();
    screen.printed();
    screen.send(resumed_with(turn));
    let printed = screen.printed();
    assert!(printed.contains("● Update(src/tax.rs)"), "{printed}");
    assert!(printed.contains("-    total.round()"), "{printed}");
    assert!(!printed.contains("1 -"), "{printed}");
}

#[test]
fn a_report_prints_even_when_quiet() {
    let mut screen = Screen::default();
    screen.settings.verbosity = Verbosity::Quiet;
    screen.printed();
    screen.send(AppEvent::Report {
        title: "Doctor".to_owned(),
        lines: vec![
            "claude-code  2.1.300  tested  logged in".to_owned(),
            "git 2.50.1".to_owned(),
        ],
    });
    let printed = screen.printed();
    assert!(
        printed.starts_with("● Doctor\n  ⎿  claude-code"),
        "{printed}"
    );
    assert!(printed.contains("git 2.50.1"), "{printed}");
}
