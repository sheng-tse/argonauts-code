//! The opened agent as a person meets it: tabs that Tab goes through, a plan drawn as a table,
//! the rest of an agent's talk with Argonauts Code left to the raw log, and keys that say what they
//! do before they do it.

use std::time::Instant;

use argonauts_core::RunId;
use std::path::PathBuf;

use argonauts_core::api::RawView;
use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::names::SessionKey;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;

use super::{SessionView, Tab};
use crate::mentions::fixture::working_state;
use crate::stability::buffer_text;
use crate::state::{AppState, SessionState, ToolRecord};
use crate::theme::{Glyphs, Theme};
use crate::views::{View, ViewCtx, ViewOutcome};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn first() -> SessionKey {
    SessionKey::numbered(1)
}

/// A crew at work, and a view on its first agent.
fn opened() -> (AppState, RunId, SessionView) {
    let state = working_state();
    let run = state.attached.clone().unwrap();
    let view = SessionView::new(run.clone(), first());
    (state, run, view)
}

fn agent<'a>(state: &'a mut AppState, run: &RunId) -> &'a mut SessionState {
    let run = state.runs.get_mut(run).unwrap();
    run.sessions.get_mut(&first()).unwrap()
}

fn press(view: &mut SessionView, state: &AppState, code: KeyCode) -> ViewOutcome {
    let theme = theme();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
}

fn screen(view: &mut SessionView, state: &AppState, width: u16) -> String {
    screen_of(view, state, width, 24)
}

fn screen_of(view: &mut SessionView, state: &AppState, width: u16, height: u16) -> String {
    let theme = theme();
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.render(area, &mut buf, &ctx);
    buffer_text(&buf)
}

fn footer(text: &str) -> &str {
    text.lines().last().unwrap_or_default().trim_end()
}

fn reported(name: &str, input: serde_json::Value) -> ToolRecord {
    ToolRecord {
        id: name.to_owned(),
        name: name.to_owned(),
        input,
        parent: None,
        ok: Some(true),
        output: None,
    }
}

fn plan() -> serde_json::Value {
    json!({
        "plan": {"summary": "Two tasks."},
        "tasks": [
            {"key": "iface", "title": "Due-date stubs", "task": "Define the Due type.",
             "owned_paths": ["src/due.rs"], "depends_on": []},
            {"key": "impl", "title": "Store, list, overdue", "task": "Store and list them.",
             "owned_paths": [], "depends_on": ["iface"]}
        ],
        "assignments": [
            {"task": "iface", "worker": "claude-code", "reviewers": ["antigravity-cli/gemini"]},
            {"task": "impl", "worker": "codex", "reviewers": ["antigravity-cli/gemini"]}
        ]
    })
}

#[test]
fn tab_goes_through_the_tabs_and_the_raw_log_is_the_last() {
    let (state, _, mut view) = opened();
    let text = screen(&mut view, &state, 100);
    assert!(
        text.lines()
            .next()
            .unwrap()
            .contains(" Output  Input  Raw "),
        "{text}"
    );
    assert!(footer(&text).contains("tab input"), "{text}");

    assert!(matches!(
        press(&mut view, &state, KeyCode::Tab),
        ViewOutcome::Consumed
    ));
    assert_eq!(view.tab, Tab::Input);
    assert!(footer(&screen(&mut view, &state, 100)).contains("tab next tab"));

    // The last tab is the raw log, read as it stands each time it opens.
    let raw = press(&mut view, &state, KeyCode::Tab);
    assert!(
        matches!(&raw, ViewOutcome::Request(ViewRequest::Raw { session, .. }) if *session == first()),
        "{raw:?}"
    );
    assert_eq!(view.tab, Tab::Raw);
    // From there the tabs go round.
    press(&mut view, &state, KeyCode::Tab);
    assert_eq!(view.tab, Tab::Output);

    // Shift+Tab goes the other way round.
    let raw = press(&mut view, &state, KeyCode::BackTab);
    assert!(matches!(raw, ViewOutcome::Request(ViewRequest::Raw { .. })));
    assert_eq!(view.tab, Tab::Raw);
    press(&mut view, &state, KeyCode::BackTab);
    assert_eq!(view.tab, Tab::Input);
}

#[test]
fn r_opens_the_raw_tab_from_any_tab() {
    let (state, _, mut view) = opened();
    press(&mut view, &state, KeyCode::Tab);
    let raw = press(&mut view, &state, KeyCode::Char('r'));
    assert!(matches!(raw, ViewOutcome::Request(ViewRequest::Raw { .. })));
    assert_eq!(view.tab, Tab::Raw);
    // On the Raw tab, r reads it again.
    let again = press(&mut view, &state, KeyCode::Char('r'));
    assert!(matches!(
        again,
        ViewOutcome::Request(ViewRequest::Raw { .. })
    ));
}

fn raw_of(session: SessionKey, lines: usize) -> ViewData {
    ViewData::Raw {
        raw: RawView {
            session,
            lines: (0..lines)
                .map(|n| format!(r#"{{"type":"assistant","n":{n}}}"#))
                .collect(),
            truncated: false,
            path: PathBuf::from("raw.jsonl"),
            cli_transcript: None,
        },
    }
}

#[test]
fn the_raw_log_draws_inside_the_view_under_its_header_and_tabs() {
    let (state, _, mut view) = opened();
    press(&mut view, &state, KeyCode::Char('r'));
    let text = screen(&mut view, &state, 100);
    assert!(text.contains("Reading the raw log…"), "{text}");
    // Another agent's log is not this tab's.
    assert!(!view.accept(&raw_of(SessionKey::numbered(2), 3)));
    assert!(view.accept(&raw_of(first(), 40)));

    let text = screen(&mut view, &state, 100);
    let header = text.lines().next().unwrap();
    assert!(header.contains(" Output  Input  Raw "), "{text}");
    assert!(
        text.contains(r#"assistant  {"type":"assistant","n":0}"#),
        "{text}"
    );
    let footer = footer(&text);
    assert!(footer.contains("enter unfolds"), "{footer}");
    assert!(footer.contains("tab output"), "{footer}");
    assert!(footer.contains("1-21 of 40"), "{footer}");

    // The keys move through the log by event, and Enter unfolds the one under the cursor.
    press(&mut view, &state, KeyCode::End);
    let text = screen(&mut view, &state, 100);
    assert!(text.contains(r#""n":39"#), "{text}");
    press(&mut view, &state, KeyCode::Enter);
    let text = screen(&mut view, &state, 100);
    assert!(text.contains(r#"  "n": 39"#), "{text}");

    // A search takes every key, Esc included, until it is done.
    press(&mut view, &state, KeyCode::Char('/'));
    for c in "\"n\":3}".chars() {
        press(&mut view, &state, KeyCode::Char(c));
    }
    press(&mut view, &state, KeyCode::Enter);
    assert!(screen(&mut view, &state, 100).contains(r#"{"type":"assistant","n":3}"#));
    press(&mut view, &state, KeyCode::Char('/'));
    assert!(matches!(
        press(&mut view, &state, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert!(matches!(
        press(&mut view, &state, KeyCode::Esc),
        ViewOutcome::Close
    ));

    // Read again, the log keeps its cursor and what was open.
    press(&mut view, &state, KeyCode::Char('r'));
    assert!(view.accept(&raw_of(first(), 41)));
    let text = screen(&mut view, &state, 100);
    assert!(text.contains(r#"{"type":"assistant","n":3}"#), "{text}");
    press(&mut view, &state, KeyCode::End);
    let text = screen(&mut view, &state, 100);
    assert!(text.contains(r#"  "n": 39"#), "{text}");
    assert!(text.contains(r#""n":40}"#), "{text}");
}

#[test]
fn a_reported_plan_is_a_table_and_the_right_arrow_opens_its_tasks() {
    let (mut state, run, mut view) = opened();
    agent(&mut state, &run).tools = vec![reported("StructuredOutput", plan())];
    let text = screen(&mut view, &state, 100);
    assert!(text.contains("● Plan · 2 tasks"), "{text}");
    assert!(
        text.contains("  task   what it does          worker       reviewed by"),
        "{text}"
    );
    assert!(
        text.contains("  impl   Store, list, overdue  Codex        Gemini on Antigravity CLI"),
        "{text}"
    );
    assert!(
        text.contains("⎿  impl waits on iface · → expands the tasks"),
        "{text}"
    );
    // The JSON it was reported as stays in the raw log.
    assert!(
        !text.contains('{') && !text.contains("assignments"),
        "{text}"
    );
    assert!(!text.contains("StructuredOutput"), "{text}");
    assert!(footer(&text).contains("→ expand"), "{text}");

    press(&mut view, &state, KeyCode::Right);
    let text = screen(&mut view, &state, 100);
    assert!(text.contains("    Store and list them."), "{text}");
    assert!(text.contains("    owns src/due.rs"), "{text}");
    assert!(footer(&text).contains("→ fold"), "{text}");
    press(&mut view, &state, KeyCode::Right);
    assert!(!screen(&mut view, &state, 100).contains("Store and list them."));
}

#[test]
fn a_result_written_at_the_end_of_a_message_reads_as_words_too() {
    let (mut state, run, mut view) = opened();
    agent(&mut state, &run).messages = vec![format!(
        "The change is close.\n\n```json\n{}\n```",
        json!({"verdict": "request_changes", "summary": "One date is compared wrong.",
               "comments": [], "tamper_suspected": false})
    )];
    let text = screen(&mut view, &state, 100);
    assert!(text.contains("● The change is close."), "{text}");
    assert!(text.contains("● Review · request changes"), "{text}");
    assert!(text.contains("  One date is compared wrong."), "{text}");
    assert!(!text.contains("```") && !text.contains("verdict"), "{text}");
    // Nothing folded, so no key offers to open it.
    assert!(!footer(&text).contains("expand"), "{text}");
}

/// An edit reads as its patch alone, as Claude Code shows it, and a Codex edit, whose call
/// carries no patch, takes the change Argonauts Code saw, so it shows once.
#[test]
fn an_edit_shows_once_with_its_patch_and_nothing_after_it() {
    let (mut state, run, mut view) = opened();
    let session = agent(&mut state, &run);
    session.agent_id = "claude-code".to_owned();
    let mut edit = reported(
        "Edit",
        json!({"file_path": "src/due.rs", "old_string": "a", "new_string": "b"}),
    );
    edit.id = "t1".to_owned();
    edit.output = Some("The file src/due.rs has been updated successfully.".to_owned());
    session.tools = vec![edit];
    session.patches = vec![(
        "t1".to_owned(),
        "src/due.rs".to_owned(),
        "@@ -1,1 +1,1 @@\n-a\n+b".to_owned(),
    )];
    let text = screen(&mut view, &state, 100);
    assert!(text.contains("Update(src/due.rs)"), "{text}");
    assert!(!text.contains("updated successfully"), "{text}");

    let (mut state, run, mut view) = opened();
    let session = agent(&mut state, &run);
    session.agent_id = "codex".to_owned();
    let mut change = reported(
        "file_change",
        json!({"changes": [{"path": "/w/src/due.rs", "kind": "update"}]}),
    );
    change.id = "c1".to_owned();
    session.tools = vec![change];
    session.diffs = vec![argonauts_core::events::session::FileDiff {
        path: "src/due.rs".to_owned(),
        added: 1,
        removed: 1,
        hunk: "@@ -1,1 +1,1 @@\n-a\n+b".to_owned(),
        truncated: false,
    }];
    let text = screen(&mut view, &state, 100);
    assert_eq!(text.matches("src/due.rs").count(), 1, "{text}");
    assert!(
        text.contains("+b"),
        "the diff hangs under the call:\n{text}"
    );
}

#[test]
fn an_agents_own_talk_with_argonauts_is_left_out_but_its_work_is_not() {
    let (mut state, run, mut view) = opened();
    agent(&mut state, &run).tools = vec![
        reported("mcp__argonauts__argonauts_status", json!({})),
        reported("Bash", json!({"command": "cargo test overdue"})),
    ];
    let text = screen(&mut view, &state, 100);
    assert!(text.contains("Bash(cargo test overdue)"), "{text}");
    assert!(!text.contains("status"), "{text}");
    assert!(!text.contains("Result"), "{text}");
}

/// As Claude Code draws an agent's work: a read says how many lines it read, never the
/// file, and a command's output folds to its first lines until → opens it.
#[test]
fn the_output_tab_reads_like_claude_codes() {
    let (mut state, run, mut view) = opened();
    let file: String = (1..=23)
        .map(|n| format!("line {n} of the file\n"))
        .collect();
    let printed: String = (1..=30).map(|n| format!("test {n} ok\n")).collect();
    let mut read = reported("Read", json!({"file_path": "src/due.py"}));
    read.output = Some(file);
    let mut ran = reported("Bash", json!({"command": "python3 -m unittest"}));
    ran.output = Some(printed);
    agent(&mut state, &run).tools = vec![read, ran];
    let text = screen_of(&mut view, &state, 100, 40);
    assert!(text.contains("Read 23 lines"), "{text}");
    assert!(!text.contains("line 1 of the file"), "{text}");
    assert!(text.contains("test 1 ok"), "{text}");
    assert!(!text.contains("test 30 ok"), "{text}");
    assert!(text.contains("+"), "{text}");
    press(&mut view, &state, KeyCode::Right);
    let text = screen_of(&mut view, &state, 100, 60);
    assert!(text.contains("test 30 ok"), "{text}");
}

/// A report Claude Code couldn't read is no report: its raw text isn't drawn.
#[test]
fn an_unreadable_report_is_left_out() {
    let (mut state, run, mut view) = opened();
    agent(&mut state, &run).tools = vec![reported(
        "StructuredOutput",
        json!({"UnparsedToolInput": "{\"status\": \"blocked\", \"summary\": \"still runn"}),
    )];
    let text = screen(&mut view, &state, 100);
    assert!(!text.contains("UnparsedToolInput"), "{text}");
    assert!(!text.contains("still runn"), "{text}");
}

#[test]
fn x_asks_before_it_stops_the_agent_and_any_other_key_keeps_it() {
    let (state, run, mut view) = opened();
    assert!(footer(&screen(&mut view, &state, 120)).contains("x stop it"));
    assert!(matches!(
        press(&mut view, &state, KeyCode::Char('x')),
        ViewOutcome::Consumed
    ));
    assert_eq!(
        footer(&screen(&mut view, &state, 120)),
        "x again to stop it · esc to keep"
    );
    // Esc keeps it, and the view stays open.
    assert!(matches!(
        press(&mut view, &state, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert!(footer(&screen(&mut view, &state, 120)).contains("x stop it"));

    press(&mut view, &state, KeyCode::Char('x'));
    let stop = press(&mut view, &state, KeyCode::Char('x'));
    assert!(
        matches!(&stop, ViewOutcome::Send(UiCommand::StopAgent { run: of, session })
            if *of == run && *session == first()),
        "{stop:?}"
    );
}

#[test]
fn a_finished_agent_has_nothing_to_stop() {
    let (mut state, run, mut view) = opened();
    agent(&mut state, &run).status = crate::state::SessionStatus::Done;
    assert!(!footer(&screen(&mut view, &state, 120)).contains("x stop"));
    let why = press(&mut view, &state, KeyCode::Char('x'));
    assert!(matches!(why, ViewOutcome::Notice(_)), "{why:?}");
}

#[test]
fn only_a_paused_output_says_so_under_the_header() {
    let (mut state, run, mut view) = opened();
    agent(&mut state, &run).messages = (1..=60).map(|n| format!("line {n}")).collect();
    let live = screen(&mut view, &state, 100);
    assert!(live.lines().nth(1).unwrap().trim().is_empty(), "{live}");
    press(&mut view, &state, KeyCode::PageUp);
    let paused = screen(&mut view, &state, 100);
    assert!(
        paused
            .lines()
            .nth(1)
            .unwrap()
            .trim_end()
            .ends_with("paused · End follows"),
        "{paused}"
    );
    assert!(footer(&paused).starts_with("End follow"), "{paused}");
    // The Input tab has no live end to follow.
    press(&mut view, &state, KeyCode::Tab);
    assert!(!screen(&mut view, &state, 100).contains("paused"));
}

#[test]
fn a_narrow_header_keeps_the_tabs_and_cuts_the_name() {
    let (mut state, run, mut view) = opened();
    agent(&mut state, &run).label = Some("build:a-very-long-label-for-one-agent".into());
    for width in [40, 60, 80, 120] {
        let text = screen(&mut view, &state, width);
        let header = text.lines().next().unwrap();
        assert!(header.contains(" Output  Input  Raw "), "{width}: {header}");
        assert!(header.starts_with("◐ build:a-very"), "{width}: {header}");
    }
}

/// The first agent of the sample run, working toward its goal.
fn with_goal() -> (AppState, RunId, SessionView) {
    let (mut state, run, view) = opened();
    let mut goal = crate::views::goal_panel::fixture::build_cli("s-001");
    goal.label = "build:cli".into();
    state.goals.remember(&run, &[goal]);
    agent(&mut state, &run).label = Some("build:cli".into());
    (state, run, view)
}

#[test]
fn an_agent_on_a_goal_has_a_goal_tab_with_the_four_parts_at_full_size() {
    let (state, _, mut view) = with_goal();
    let text = screen(&mut view, &state, 120);
    let header = text.lines().next().unwrap();
    assert!(header.contains(" Output  Input  Goal  Raw "), "{text}");
    // The try it is on comes before its time.
    assert!(header.contains("try 3 · "), "{text}");

    press(&mut view, &state, KeyCode::Tab);
    press(&mut view, &state, KeyCode::Tab);
    assert_eq!(view.tab, Tab::Goal);
    let text = screen(&mut view, &state, 120);
    let rows: Vec<&str> = text.lines().collect();
    assert!(rows[1].trim().is_empty(), "{text}");
    assert!(rows[2].starts_with('╭'), "{text}");
    assert!(rows[3].starts_with("│ ◆ the goal"), "{text}");
    assert!(
        rows[3]
            .trim_end()
            .ends_with("kept by Codex's own goal · set 6m ago · 2 of 3 lines met │"),
        "{text}"
    );
    for part in ["◆ tries", "◆ checks", "◆ Codex says"] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    assert!(
        text.contains("cargo test overdue · 1 of 12 fails"),
        "{text}"
    );
    assert!(text.contains("2m17s · running"), "{text}");
    assert!(
        footer(&text).starts_with("↑↓ scroll · m message it · x stop it · tab next tab · esc back"),
        "{text}"
    );

    // Past Goal is the raw log, and then the tabs go round to Output.
    let raw = press(&mut view, &state, KeyCode::Tab);
    assert!(matches!(raw, ViewOutcome::Request(ViewRequest::Raw { .. })));
    assert_eq!(view.tab, Tab::Raw);
    press(&mut view, &state, KeyCode::Tab);
    assert_eq!(view.tab, Tab::Output);
}

#[test]
fn an_agent_without_a_goal_has_no_goal_tab_and_one_opened_on_it_falls_back() {
    let (state, run, _) = opened();
    let mut view = SessionView::new(run, first()).on(Tab::Goal);
    let text = screen(&mut view, &state, 120);
    assert!(!text.lines().next().unwrap().contains("Goal"), "{text}");
    assert_eq!(view.tab, Tab::Output);
}

#[test]
fn a_newer_report_of_the_goal_is_what_the_tab_draws_next() {
    let (mut state, run, view) = with_goal();
    let mut view = view.on(Tab::Goal);
    assert!(screen(&mut view, &state, 120).contains("2 of 3 lines met"));
    let mut goal = state.goals.of(&run, &first()).unwrap().clone();
    goal.lines[2].met = Some(true);
    goal.lines[2].met_on_try = Some(3);
    goal.state = argonauts_core::agent_goal::GoalState::Met;
    state.goals.remember(&run, &[goal]);
    let text = screen(&mut view, &state, 120);
    assert!(text.contains("3 of 3 lines met"), "{text}");
    assert!(text.contains("cargo test overdue · met on try 3"), "{text}");
}

#[test]
fn the_goal_tab_scrolls_when_the_screen_is_short() {
    let (state, _, view) = with_goal();
    let mut view = view.on(Tab::Goal);
    let theme = theme();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let draw = |view: &mut SessionView| {
        let area = Rect::new(0, 0, 120, 12);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        buffer_text(&buf)
    };
    let top = draw(&mut view);
    assert!(
        top.contains("◆ the goal") && !top.contains("◆ checks"),
        "{top}"
    );
    press(&mut view, &state, KeyCode::End);
    let end = draw(&mut view);
    assert!(
        end.contains("◆ checks") && !end.contains("◆ the goal"),
        "{end}"
    );
    press(&mut view, &state, KeyCode::Home);
    assert!(draw(&mut view).contains("◆ the goal"));
}

/// The same agent, its goal one task's, so the keys on it have something to name.
fn with_a_tasks_goal() -> (AppState, RunId, SessionView) {
    let (mut state, run, view) = with_goal();
    let mut goal = state.goals.of(&run, &first()).unwrap().clone();
    goal.task = Some(argonauts_core::TaskId::new("task-002").unwrap());
    state.goals.remember(&run, &[goal]);
    (state, run, view.on(Tab::Goal))
}

fn goal_action(outcome: ViewOutcome) -> argonauts_core::events::goal::GoalAction {
    match outcome {
        ViewOutcome::Send(UiCommand::Goal {
            run: Some(_),
            action,
        }) => action,
        _ => panic!("the key sends a word about the goal"),
    }
}

/// The Goal tab's footer offers the goal's keys, and `p` and `c` send what
/// they say for the goal's task.
#[test]
fn the_goal_tab_holds_the_goal_and_runs_its_checks() {
    use argonauts_core::events::goal::GoalAction;

    let (mut state, run, mut view) = with_a_tasks_goal();
    // With room for every task, the footer holds every key.
    let text = screen_of(&mut view, &state, 120, 40);
    assert_eq!(
        footer(&text),
        "p pause the goal · e edit its lines · c run the checks now · m message it · x stop it · \
         tab next tab · esc back"
    );
    // On a short screen the place in the page takes room, and the keys on the goal stay.
    let text = screen(&mut view, &state, 120);
    assert!(
        footer(&text).starts_with(
            "p pause the goal · e edit its lines · c run the checks now · m message it · \
             x stop it · esc back"
        ),
        "{text}"
    );
    let task = Some(argonauts_core::TaskId::new("task-002").unwrap());
    assert_eq!(
        goal_action(press(&mut view, &state, KeyCode::Char('p'))),
        GoalAction::Pause { task: task.clone() }
    );
    assert_eq!(
        goal_action(press(&mut view, &state, KeyCode::Char('c'))),
        GoalAction::CheckNow { task }
    );

    // While the checks you asked for run, `c` isn't offered and does nothing.
    let mut goal = state.goals.of(&run, &first()).unwrap().clone();
    goal.checking = true;
    state.goals.remember(&run, &[goal]);
    let text = screen(&mut view, &state, 120);
    assert!(!footer(&text).contains("c run the checks now"), "{text}");
    assert!(text.contains("running now, as you asked"), "{text}");
    assert!(matches!(
        press(&mut view, &state, KeyCode::Char('c')),
        ViewOutcome::Ignored
    ));
}

/// `e` opens the goal's lines on one line, where the cursor moves and text goes in, Enter
/// rewrites them and Esc leaves them as they were.
#[test]
fn e_opens_the_goals_lines_to_change_and_enter_rewrites_them() {
    use argonauts_core::events::goal::GoalAction;

    let (state, _, mut view) = with_a_tasks_goal();
    assert!(matches!(
        press(&mut view, &state, KeyCode::Char('e')),
        ViewOutcome::Consumed
    ));
    let text = screen(&mut view, &state, 120);
    assert!(
        footer(&text).starts_with("its lines, with ; between ❯ "),
        "{text}"
    );
    assert!(
        footer(&text).contains("every test in tests/overdue.rs passes▌"),
        "{text}"
    );
    // Keys that act elsewhere are text here: `x` stops nothing and `p` holds nothing.
    for c in "; px".chars() {
        assert!(matches!(
            press(&mut view, &state, KeyCode::Char(c)),
            ViewOutcome::Consumed
        ));
    }
    let GoalAction::EditLines { task, lines } =
        goal_action(press(&mut view, &state, KeyCode::Enter))
    else {
        panic!("enter rewrites the lines");
    };
    assert_eq!(task.unwrap().as_str(), "task-002");
    assert_eq!(
        lines,
        [
            "tasks overdue lists tasks due before today",
            "tasks add --due rejects a date that isn't ISO",
            "every test in tests/overdue.rs passes",
            "px"
        ]
    );
    assert!(!footer(&screen(&mut view, &state, 120)).contains("with ; between"));

    // Esc leaves the lines alone, and an emptied line is refused rather than sent.
    press(&mut view, &state, KeyCode::Char('e'));
    assert!(matches!(
        press(&mut view, &state, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert!(footer(&screen(&mut view, &state, 120)).starts_with("p pause the goal"));
    press(&mut view, &state, KeyCode::Char('e'));
    view.handle_key(
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        &ViewCtx {
            theme: &theme(),
            state: &state,
            now: Instant::now(),
        },
    );
    assert!(matches!(
        press(&mut view, &state, KeyCode::Enter),
        ViewOutcome::Notice(_)
    ));
}

/// A goal that is over keeps its lines: `e` says so, and the footer doesn't offer it.
#[test]
fn a_goal_that_is_over_offers_no_keys_on_it() {
    let (mut state, run, mut view) = with_a_tasks_goal();
    let mut goal = state.goals.of(&run, &first()).unwrap().clone();
    goal.state = argonauts_core::agent_goal::GoalState::Met;
    state.goals.remember(&run, &[goal]);
    let text = screen(&mut view, &state, 120);
    for key in ["p pause", "e edit", "c run"] {
        assert!(!footer(&text).contains(key), "{key}: {text}");
    }
    assert!(matches!(
        press(&mut view, &state, KeyCode::Char('e')),
        ViewOutcome::Notice(_)
    ));
}

/// A worker that ran Claude Code's own workflow: the call, the workflow's report, and the
/// workflow as Claude Code also lists it among the subagents.
fn with_its_own_workflow() -> (AppState, RunId, SessionView) {
    use argonauts_core::events::session::{
        CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentKind, SubagentStatus,
        SubagentUpdate,
    };
    let (mut state, run, view) = opened();
    let session = agent(&mut state, &run);
    session.agent_id = "claude-code".to_owned();
    session.tools.push(ToolRecord {
        id: "toolu_wf".to_owned(),
        name: "Workflow".to_owned(),
        input: json!({"script": "export const meta = {name: 'add-due-dates'}"}),
        parent: None,
        ok: Some(true),
        output: None,
    });
    let phase = |index: u32, title: &str| CliPhase {
        index,
        title: title.to_owned(),
        detail: String::new(),
    };
    let worker = |index: u32, state: CliAgentState| CliAgent {
        index,
        label: format!("step:{index}"),
        phase: Some(index),
        model: None,
        state,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        started_at_ms: None,
        try_number: 1,
        prompt: None,
        result: None,
        ..CliAgent::default()
    };
    session.workflows.push(CliWorkflowRun {
        id: "w7k2".to_owned(),
        call: Some("toolu_wf".to_owned()),
        name: "add-due-dates".to_owned(),
        description: String::new(),
        phases: vec![phase(1, "build"), phase(2, "review"), phase(3, "judge")],
        agents: vec![
            worker(1, CliAgentState::Done),
            worker(2, CliAgentState::Running),
            worker(3, CliAgentState::Queued),
        ],
        status: SubagentStatus::Running,
        tokens: 0,
        tool_calls: 0,
        duration_ms: 0,
        summary: Some("Build, review, and judge".to_owned()),
        started_at_ms: None,
    });
    session.subagents.push(SubagentUpdate {
        id: "toolu_wf".to_owned(),
        parent: None,
        kind: SubagentKind::Workflow,
        label: Some("add-due-dates".to_owned()),
        prompt: None,
        status: SubagentStatus::Running,
        last_message: Some("Review: step:2".to_owned()),
        log_uri: None,
        steps_visible: false,
        tokens: None,
    });
    (state, run, view)
}

/// While Claude Code still writes the script there is no workflow: nothing of the call shows
/// until its first report. A call that failed before that says why.
#[test]
fn a_workflow_call_shows_nothing_until_its_script_reports() {
    let (mut state, run, mut view) = with_its_own_workflow();
    let session = agent(&mut state, &run);
    session.workflows.clear();
    session.subagents.clear();
    session.tools[0].ok = None;
    let text = screen_of(&mut view, &state, 100, 40);
    assert!(!text.contains("Workflow"), "{text}");

    let session = agent(&mut state, &run);
    session.tools[0].ok = Some(false);
    session.tools[0].output =
        Some("Workflow script has a syntax error and was not launched:\nUnexpected".to_owned());
    let text = screen_of(&mut view, &state, 100, 40);
    assert!(text.contains("● Workflow(workflow)"), "{text}");
    assert!(
        text.contains("Workflow script has a syntax error"),
        "{text}"
    );
}

#[test]
fn claude_codes_own_workflow_is_its_call_and_where_to_watch_it() {
    let (state, _, mut view) = with_its_own_workflow();
    let text = screen_of(&mut view, &state, 100, 40);
    let rows: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("Workflow(") || line.contains("Running in background"))
        .map(str::trim)
        .collect();
    assert_eq!(
        rows,
        [
            "● Workflow(add-due-dates)",
            "⎿  Running in background · /workflows to monitor and save"
        ],
        "{text}"
    );
    assert!(!text.contains("subagent"), "{text}");
}

#[test]
fn an_edit_is_drawn_from_the_patch_it_reported_with_the_files_line_numbers() {
    let (mut state, run, mut view) = opened();
    let session = agent(&mut state, &run);
    session.tools = vec![reported(
        "Edit",
        json!({"file_path": "/w/src/a.rs", "old_string": "x", "new_string": "y"}),
    )];
    session.patches = vec![(
        "Edit".to_owned(),
        "/w/src/a.rs".to_owned(),
        "@@ -41,3 +41,3 @@\n a\n-x\n+y\n b\n".to_owned(),
    )];
    let text = screen(&mut view, &state, 100);
    assert!(text.contains("41  a"), "{text}");
    assert!(text.contains("42 -x"), "{text}");
    assert!(text.contains("42 +y"), "{text}");
}

/// Argonauts Code also notes how each file stands after the session's calls. A file whose call drew
/// its own patch is shown once, from the patch; one no call drew, as a shell command's, still
/// shows.
#[test]
fn a_file_its_call_drew_is_not_drawn_again_from_the_change_seen_after() {
    let (mut state, run, mut view) = opened();
    let session = agent(&mut state, &run);
    session.tools = vec![reported(
        "Edit",
        json!({"file_path": "/w/src/a.rs", "old_string": "x", "new_string": "y"}),
    )];
    session.patches = vec![(
        "Edit".to_owned(),
        "/w/src/a.rs".to_owned(),
        "@@ -41,3 +41,3 @@\n a\n-x\n+y\n b\n".to_owned(),
    )];
    let seen = |path: &str, line: &str| argonauts_core::events::session::FileDiff {
        path: path.to_owned(),
        added: 1,
        removed: 1,
        hunk: format!("@@ -1,1 +1,1 @@\n-old {line}\n+new {line}\n"),
        truncated: false,
    };
    session.diffs = vec![seen("src/a.rs", "a"), seen("src/b.rs", "b")];
    let text = screen(&mut view, &state, 100);
    assert_eq!(text.matches("42 +y").count(), 1, "{text}");
    assert!(!text.contains("new a"), "{text}");
    assert!(text.contains("Update(src/b.rs)"), "{text}");
    assert!(text.contains("new b"), "{text}");
}

/// A call its model is still writing shows in the Output tab as the conversation shows it, by its
/// name alone and with a steady dim bullet; filled in, it reads as any call, and one that
/// never lands is gone.
#[test]
fn a_call_being_written_shows_by_its_name_and_goes_if_it_never_lands() {
    let (mut state, run, mut view) = opened();
    let pending = |id: &str, name: &str| ToolRecord {
        id: id.to_owned(),
        name: name.to_owned(),
        input: serde_json::Value::Null,
        parent: None,
        ok: None,
        output: None,
    };
    agent(&mut state, &run).tools = vec![pending("e1", "Edit"), pending("w1", "Write")];
    let text = screen_of(&mut view, &state, 100, 30);
    let alone = |name: &str| {
        text.lines()
            .any(|line| line.trim_end() == format!("● {name}"))
    };
    assert!(alone("Update") && alone("Write"), "{text}");

    let theme = theme();
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.render(area, &mut buf, &ctx);
    let row = (0..30)
        .find(|y| {
            let line: String = (0..100).map(|x| buf[(x, *y)].symbol()).collect();
            line.contains("● Update")
        })
        .expect("the call's row");
    let bullet = (0..100)
        .find(|x| buf[(*x, row)].symbol() == "●")
        .expect("its bullet");
    let dim = theme.style(crate::theme::Role::Dim).fg;
    assert_eq!(Some(buf[(bullet, row)].fg), dim, "a steady dim bullet");

    let tools = &mut agent(&mut state, &run).tools;
    tools[0].input = json!({"file_path": "src/app.rs", "old_string": "a", "new_string": "b"});
    tools.retain(|tool| tool.id != "w1");
    let text = screen_of(&mut view, &state, 100, 30);
    assert!(text.contains("● Update(src/app.rs)"), "{text}");
    assert_eq!(text.matches("● Update").count(), 1, "{text}");
    assert!(!text.contains("Write"), "{text}");
}
