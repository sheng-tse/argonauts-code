//! Screen-reader mode, `NO_COLOR`, and reduced motion, checked without a terminal.

mod pty_support;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use argonauts_core::app::{AnswerTarget, AppEvent, UiCommand};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::events::{RunEventKind, approval::ApprovalEvent};
use argonauts_core::names::ApprovalId;
use argonauts_core::settings::ui::{ColorChoice, Motion, UiSettings};
use argonauts_core::{RunId, Verbosity};
use argonauts_tui::a11y::{Reader, lines_for, run_lines};
use argonauts_tui::bottom::WorkingLine;
use argonauts_tui::bridge::settings::theme_for;
use argonauts_tui::bridge::tools::{finish_call, start_call};
use argonauts_tui::questions::approval_question;
use argonauts_tui::render::{Clock, RenderCtx};
use argonauts_tui::runtime::AppIo;
use argonauts_tui::state::AppState;
use argonauts_tui::terminal::write_line;
use argonauts_tui::theme::{ColorEnv, ColorLevel, Role, Theme};
use argonauts_tui::{Cell, HistoryCell, UiOptions};
use pty_support::*;
use ratatui::style::Color;
use ratatui::text::Line;
use serde_json::json;
use tokio::sync::mpsc;

fn read(reader: &mut Reader, events: &[AppEvent]) -> Vec<String> {
    events
        .iter()
        .flat_map(|event| reader.event(event).lines)
        .collect()
}

fn conversation(turn: &str, prompt: &str, answer: &str) -> Vec<AppEvent> {
    conversation_turn(turn, prompt, answer, 5)
        .into_iter()
        .map(|(_, event)| event)
        .collect()
}

fn approval_id(id: &str) -> ApprovalId {
    serde_json::from_value(json!(id)).expect("an approval id")
}

fn run_id() -> RunId {
    serde_json::from_value(json!(RUN)).expect("a run id")
}

#[test]
fn screen_reader_lines_for_a_crew_run() {
    let (mut run, crew, events) = crew_run(2);
    let mut reader = Reader::new(&UiOptions::default());
    let mut lines = read(&mut reader, &[welcome()]);
    lines.extend(read(
        &mut reader,
        &conversation("t-001", "hi", "Hello there.\nWhat should we build?"),
    ));
    lines.extend(read(&mut reader, &events));
    let rest = [
        run.text_delta(&crew[0], "The writer is in.\nNext, "),
        run.text_delta(&crew[0], "the flag."),
        run.message(&crew[0], "The writer is in.\nNext, the flag."),
        run.tool_finished(&crew[0], "s-001-edit", true, "Updated src/export.rs"),
        run.tool_finished(
            &crew[1],
            "s-002-edit",
            false,
            "error: old_string not found in src/cli.rs",
        ),
        narrator_line(RUN, "Both workers are on export.rs."),
        run.note("warn", "codex is at 91% of its window"),
        run.finished("merged", "Merged into main"),
    ];
    lines.extend(read(&mut reader, &rest));
    let text = lines.join("\n");
    insta::assert_snapshot!(text);

    for glyph in ['╭', '│', '╰', '─', '⎿', '●', '⏺', '·', '✢', '\x1b'] {
        assert!(!text.contains(glyph), "no {glyph:?} in:\n{text}");
    }
    // Streamed text is said once, a line at a time, and the whole message adds nothing.
    for said in ["The writer is in.", "Next, the flag.", "Hello there."] {
        let times = lines.iter().filter(|line| line.ends_with(said)).count();
        assert_eq!(times, 1, "{said:?} in:\n{text}");
    }
    for label in [
        "you: hi",
        "claude-code: Hello there.",
        "claude-code, worker, task-002: ",
        "codex, worker, task-003: ",
        "tool: ",
        "tool error: ",
        "narrator: ",
        "warning: ",
        "run: finished, merged",
    ] {
        assert!(
            lines.iter().any(|line| line.starts_with(label)),
            "a line starting {label:?} in:\n{text}"
        );
    }
}

#[test]
fn screen_reader_lines_stay_quiet_during_replay() {
    let (_, _, events) = crew_run(1);
    let mut reader = Reader::new(&UiOptions::default());
    let mut lines = read(&mut reader, &[attached(RUN, "Add a --json flag")]);
    lines.extend(read(&mut reader, &events));
    assert_eq!(lines.len(), 1, "only the attach line: {lines:?}");
    let replay = condensed_replay(
        RUN,
        &[("you", "add a --json flag")],
        &REPLAY_MILESTONES,
        REPLAY_RECAP,
    );
    let caught_up = read(&mut reader, &[replay, replay_done(RUN, 40)]);
    assert!(
        caught_up.contains(&format!("recap: {REPLAY_RECAP}")),
        "{caught_up:?}"
    );
    assert!(
        caught_up
            .iter()
            .any(|line| line == "you: add a --json flag")
    );
}

#[test]
fn screen_reader_question_as_numbered_list() {
    let (mut run, crew, events) = crew_run(1);
    let mut reader = Reader::new(&UiOptions::default());
    read(&mut reader, &events);

    let merge = run.merge_approval("approval-001");
    let said = reader.event(&merge);
    assert!(said.bell, "a task that needs you rings the bell");
    insta::assert_snapshot!("merge_question", said.lines.join("\n"));
    assert!(
        said.lines
            .iter()
            .any(|line| line == "  1. Merge, recommended")
    );
    assert!(
        said.lines
            .iter()
            .any(|line| line == "  2. Open a pull request")
    );
    // lines_for says the same, from the folded state alone.
    let mut state = AppState::default();
    for event in events.iter().chain([&merge]) {
        state.apply(event);
    }
    assert_eq!(lines_for(&merge, &state), said.lines);

    // By number, with or without a note, the answer carries the question's evidence hash.
    let answer = |action: AnswerAction, text: Option<&str>| UiCommand::Answer {
        to: AnswerTarget::Approval {
            run: run_id(),
            id: approval_id("approval-001"),
        },
        action,
        text: text.map(str::to_owned),
        evidence_sha256: Some(EVIDENCE.to_owned()),
        tasks: Vec::new(),
        remember: false,
    };
    let mut noted = Reader::new(&UiOptions::default());
    read(&mut noted, &events);
    noted.event(&merge);
    assert_eq!(
        noted.typed("3: keep the old flag").commands,
        vec![answer(
            AnswerAction::RequestChanges,
            Some("keep the old flag")
        )]
    );
    let picked = reader.typed("2");
    assert_eq!(
        picked.commands,
        vec![answer(AnswerAction::PullRequest, None)]
    );
    assert_eq!(picked.lines, vec!["you: 2. Open a pull request"]);
    // Answered here, the question no longer takes numbers.
    assert_eq!(
        reader.typed("1").commands,
        vec![UiCommand::Submit {
            text: "1".into(),
            workflow_requested: false,
            pictures: Vec::new(),
        }]
    );

    // A question with your own answer: the number, then the answer on the next line.
    let question = reader.event(&run.question_approval("approval-002", &crew[0]));
    assert!(
        question
            .lines
            .iter()
            .any(|line| line == "  3. Type my own answer")
    );
    assert!(!question.lines.iter().any(|line| line.starts_with("  4.")));
    let asked = reader.typed("3");
    assert!(asked.commands.is_empty());
    assert_eq!(
        asked.lines,
        vec!["answer: type your answer and press Enter."]
    );
    let own = reader.typed("Keep it for one release");
    assert_eq!(
        own.commands,
        vec![UiCommand::Answer {
            to: AnswerTarget::Approval {
                run: run_id(),
                id: approval_id("approval-002"),
            },
            action: AnswerAction::Answer,
            text: Some("Keep it for one release".into()),
            evidence_sha256: Some(EVIDENCE.to_owned()),
            tasks: Vec::new(),
            remember: false,
        }]
    );

    // An app choice is a numbered list too, answered without a hash.
    let choice = reader.event(&start_work_choice("ch-001"));
    assert!(
        choice.lines.iter().any(|line| line.starts_with("  1. ")),
        "{:?}",
        choice.lines
    );
    let chosen = reader.typed("1");
    assert!(matches!(
        chosen.commands.as_slice(),
        [UiCommand::Answer {
            to: AnswerTarget::Choice { .. },
            evidence_sha256: None,
            ..
        }]
    ));
}

#[test]
fn screen_reader_questions_follow_one_another() {
    let (mut run, crew, events) = crew_run(1);
    let mut reader = Reader::new(&UiOptions::default());
    read(&mut reader, &events);
    reader.event(&run.merge_approval("approval-001"));
    reader.event(&run.question_approval("approval-002", &crew[0]));
    // Numbers answer the question read out last.
    let picked = reader.typed("1");
    assert!(matches!(
        picked.commands.as_slice(),
        [UiCommand::Answer {
            action: AnswerAction::Choose { .. },
            ..
        }]
    ));
    // Then the one still waiting is read out again, and numbers answer it.
    assert!(
        picked.lines.contains(&"still waiting:".to_owned()),
        "{:?}",
        picked.lines
    );
    assert!(
        picked
            .lines
            .iter()
            .any(|line| line.contains("Merge approval"))
    );
    // Merge asks again before it's sent, and a letter is only a message.
    assert_eq!(
        reader.typed("y").commands,
        vec![UiCommand::Submit {
            text: "y".into(),
            workflow_requested: false,
            pictures: Vec::new(),
        }]
    );
    assert!(reader.typed("1").commands.is_empty());
    assert!(matches!(
        reader.typed("1").commands.as_slice(),
        [UiCommand::Answer {
            action: AnswerAction::Merge,
            ..
        }]
    ));
}

#[test]
fn screen_reader_commands_and_quiet_verbosity() {
    let (mut run, crew, events) = crew_run(1);
    let mut reader = Reader::new(&UiOptions::default());
    read(&mut reader, &events);
    let quiet = reader.typed("/verbosity quiet");
    assert_eq!(
        quiet.commands,
        vec![UiCommand::Verbosity {
            level: Verbosity::Quiet
        }]
    );
    // Quiet keeps failures and drops the rest.
    let ok = reader.event(&run.tool_finished(&crew[0], "s-001-edit", true, "done"));
    assert!(ok.lines.is_empty(), "{:?}", ok.lines);
    let failed = reader.event(&run.tool_finished(&crew[0], "s-001-read", false, "denied"));
    assert_eq!(failed.lines, vec!["tool error: denied"]);

    let help = reader.typed("/help");
    let at = |start: &str| {
        help.lines
            .iter()
            .position(|line| line.starts_with(start))
            .unwrap_or_else(|| panic!("{start} in {:?}", help.lines))
    };
    assert!(at("help: Session commands:") < at("help: /exit"));
    assert!(at("help: /exit") < at("help: Work commands:"));
    assert!(at("help: Setup and help commands:") < at("help: /help"));
    assert!(matches!(
        reader.typed("/workflows").commands.as_slice(),
        [UiCommand::Request { .. }]
    ));
    let exit = reader.typed("/exit");
    assert!(exit.exit);
    assert_eq!(exit.commands, vec![UiCommand::Exit]);
}

fn io_pair() -> (AppIo, mpsc::Sender<AppEvent>, mpsc::Receiver<UiCommand>) {
    let (commands, sent) = mpsc::channel(64);
    let (events_in, events) = mpsc::channel(64);
    (AppIo { commands, events }, events_in, sent)
}

#[tokio::test]
async fn screen_reader_rings_and_leaves() {
    let (mut run, _, events) = crew_run(1);
    let (io, events_in, mut sent) = io_pair();
    let (typed_in, typed) = mpsc::unbounded_channel();
    for event in events {
        events_in
            .send(event)
            .await
            .expect("the reader is listening");
    }
    events_in
        .send(run.merge_approval("approval-001"))
        .await
        .expect("the reader is listening");
    typed_in
        .send("/exit".to_owned())
        .expect("the reader is listening");
    let out = run_lines(io, &UiOptions::default(), Vec::new(), typed, true)
        .await
        .expect("the reader runs");
    let text = String::from_utf8(out).expect("utf-8");
    assert_eq!(text.matches('\x07').count(), 1, "one bell:\n{text}");
    assert!(text.contains("  1. Merge, recommended\n"));
    assert!(!text.contains('\x1b'), "no escape sequences:\n{text}");
    assert_eq!(sent.recv().await, Some(UiCommand::Exit));

    // Ctrl+D ends the input, which leaves too. No bell off a terminal.
    let (io, events_in, mut sent) = io_pair();
    let (typed_in, typed) = mpsc::unbounded_channel::<String>();
    let mut run = RunScript::new(RUN);
    events_in
        .send(run.merge_approval("approval-001"))
        .await
        .expect("the reader is listening");
    drop(typed_in);
    let out = run_lines(io, &UiOptions::default(), Vec::new(), typed, false)
        .await
        .expect("the reader runs");
    assert!(!out.contains(&0x07));
    assert_eq!(sent.recv().await, Some(UiCommand::Exit));
    drop(events_in);
}

fn no_color_env() -> ColorEnv {
    ColorEnv {
        no_color: true,
        colorterm: Some("truecolor".into()),
        term: Some("xterm-256color".into()),
        colorfgbg: None,
    }
}

fn is_plain(color: Option<Color>) -> bool {
    matches!(color, None | Some(Color::Reset))
}

#[test]
fn no_color_has_no_color_codes() {
    let theme = theme_for(&UiSettings::default(), &no_color_env(), None);
    assert_eq!(theme.level, ColorLevel::None);
    for role in Role::ALL {
        let style = theme.style(role);
        assert!(
            is_plain(style.fg) && is_plain(style.bg),
            "{role:?}: {style:?}"
        );
    }
    let dumb = ColorEnv {
        term: Some("dumb".into()),
        ..ColorEnv::default()
    };
    assert_eq!(
        theme_for(&UiSettings::default(), &dumb, None).level,
        ColorLevel::None
    );
    // `ui.color = "always"` overrides both, as the standard allows.
    let always = UiSettings {
        color: ColorChoice::Always,
        ..UiSettings::default()
    };
    assert_ne!(
        theme_for(&always, &no_color_env(), None).level,
        ColorLevel::None
    );
    assert_ne!(theme_for(&always, &dumb, None).level, ColorLevel::None);

    // A failed call and a question, written out as the terminal gets them, set no color.
    let mut call = start_call(
        "claude-code",
        "Bash",
        &json!({"command": "cat missing.txt"}),
    );
    finish_call(
        &mut call,
        false,
        Some("cat: missing.txt: No such file or directory"),
    );
    let mut run = RunScript::new(RUN);
    let AppEvent::Run { event } = run.merge_approval("approval-001") else {
        unreachable!("a run event")
    };
    let RunEventKind::Approval(ApprovalEvent::Opened(opened)) = event.kind else {
        unreachable!("an opened approval")
    };
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let mut lines: Vec<Line<'static>> = Cell::Tool(call).lines(&ctx);
    lines.extend(approval_question(1, &opened, None).lines(&ctx));
    let mut raw = Vec::new();
    for line in &lines {
        write_line(&mut raw, line, 80).expect("writing to memory");
    }
    assert!(!sets_color(&raw), "{}", String::from_utf8_lossy(&raw));
    let text: String = lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
        .collect();
    assert!(
        text.contains("No such file"),
        "the failure still says so:\n{text}"
    );

    // The same theme with color on does set it, so the check can fail.
    let colored = theme_for(&always, &no_color_env(), None);
    let ctx = RenderCtx::new(80, &colored, Clock::still(Instant::now()));
    let mut raw = Vec::new();
    for line in approval_question(1, &opened, None).lines(&ctx) {
        write_line(&mut raw, &line, 80).expect("writing to memory");
    }
    assert!(sets_color(&raw));
}

/// The working line's first glyph at each of `times`, in milliseconds after it started.
fn working_glyphs(theme: &Theme, times: impl Iterator<Item = u64>) -> BTreeSet<String> {
    let start = Instant::now();
    let line = WorkingLine::new("Checking", start);
    times
        .map(|ms| {
            let clock = Clock::new(start, start + Duration::from_millis(ms));
            let ctx = RenderCtx::new(80, theme, clock);
            let drawn = line.line(&ctx);
            drawn
                .spans
                .first()
                .map(|span| span.content.to_string())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn reduced_motion_spinner_is_still() {
    let env = ColorEnv {
        term: Some("xterm-256color".into()),
        ..ColorEnv::default()
    };
    let moving = theme_for(&UiSettings::default(), &env, None);
    assert!(!moving.reduced_motion);
    let seen = working_glyphs(&moving, (0..30).map(|n| n * 70));
    assert!(seen.len() > 1, "the glyph turns by default: {seen:?}");

    let reduced = UiSettings {
        motion: Motion::Reduced,
        ..UiSettings::default()
    };
    let still = theme_for(&reduced, &env, None);
    assert!(still.reduced_motion);
    let seen = working_glyphs(&still, (0..30).map(|n| n * 70));
    assert_eq!(seen.len(), 1, "one glyph, held still: {seen:?}");
}

#[test]
fn screen_reader_says_nothing_of_argonauts_own_calls() {
    let (mut run, crew, events) = crew_run(1);
    let mut reader = Reader::new(&UiOptions::default());
    let mut lines = read(&mut reader, &[welcome()]);
    let turn = "t-001";
    lines.extend(read(
        &mut reader,
        &[
            turn_started(turn, "hi"),
            conversation_tool_started(turn, "c1", "Read", json!({"file_path": "src/cli.rs"})),
            conversation_tool_finished(turn, "c1", "fn main() {}"),
            conversation_tool_started(
                turn,
                "c2",
                "mcp__argonauts_status__argonauts_status",
                json!({}),
            ),
            conversation_tool_finished(turn, "c2", "no run is going"),
            text_delta(turn, "Hello there."),
            conversation_tool_started(turn, "c3", "StructuredOutput", json!({"status": "done"})),
            conversation_tool_finished(turn, "c3", "Structured output provided successfully"),
            turn_finished(turn, "Hello there.", 3_000),
        ],
    ));
    lines.extend(read(&mut reader, &events));
    let rest = [
        run.tool_started(&crew[0], "s-001-status", "argonauts_status", json!({})),
        run.tool_finished(&crew[0], "s-001-status", true, "task-002 running"),
        run.tool_started(
            &crew[0],
            "s-001-report",
            "argonauts_report",
            json!({"status": "done"}),
        ),
        run.tool_finished(&crew[0], "s-001-report", true, "Report saved"),
    ];
    lines.extend(read(&mut reader, &rest));
    let text = lines.join("\n");
    assert!(text.contains("claude-code: Read src/cli.rs"), "{text}");
    assert!(text.contains("claude-code: Hello there."), "{text}");
    for unsaid in [
        "argonauts_status",
        "Report",
        "no run is going",
        "Structured output",
        "task-002 running",
    ] {
        assert!(!text.contains(unsaid), "{unsaid:?} in:\n{text}");
    }
}
