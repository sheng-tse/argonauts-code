//! The harness and the pty helpers themselves, so a failing screen test points at the screen.

mod pty_support;

use argonauts_core::app::AppEvent;
use argonauts_testkit::{Pty, PtyCommand};
use pty_support::*;
use serde_json::json;

fn round_trips(event: &AppEvent) {
    let value = serde_json::to_value(event).expect("an AppEvent writes as JSON");
    let again: AppEvent = serde_json::from_value(value).expect("and reads back");
    assert_eq!(&again, event);
}

#[test]
fn every_builder_makes_a_contract_event() {
    let (mut run, crew, mut events) = crew_run(7);
    let agent = &crew[0];
    events.extend([
        welcome(),
        welcome_with_runs(2),
        status_line(),
        status_with(json!({"approval_mode": "auto"})),
        mode_idle(),
        mode_answering("t-001"),
        mode_pausing(RUN),
        mode_parked(RUN, "paused"),
        turn_cancelled("t-001"),
        chat_tool_started("t-001", "c1", "Bash", json!({"command": "ls"})),
        chat_tool_finished("t-001", "c1", "src"),
        notice("warn", "careful"),
        recap("All done."),
        narrator_line(RUN, "Two agents are writing tests."),
        narrator_status(0.06, "$0.50"),
        taken_back(&["keep a fallback"]),
        exit(),
        ui_settings(json!({"keys": "vim", "theme": "light"})),
        start_work_choice("ch-001"),
        scope_choice(
            "ch-002",
            "Save where?",
            "global",
            Some("A project file sets this."),
        ),
        choice_closed("ch-001", "No run started."),
        run.session_input(agent, &[("card", "Add the JSON writer")]),
        run.text_delta(agent, "Adding"),
        run.message(agent, "Added."),
        run.file_diff(agent, "src/export.rs", "@@ -41,2 +41,3 @@"),
        run.queued(agent, "m-001", "keep a fallback"),
        run.session_ended(agent, 4),
        run.merge_checkpoint("cp-001"),
        run.question_checkpoint("cp-002", agent),
        run.plan_checkpoint("cp-003"),
        run.take_over_return("cp-004", agent),
        run.decided(
            "cp-001",
            json!({"action": "merge"}),
            json!({"party": "user"}),
        ),
        run.spot_check_queued("cp-001"),
        run.parked("checkpoint"),
        run.paused(2),
        run.finished("merged", "landed on main"),
        run.note("info", "hello"),
        workflow_view(&crew[..3], Some(nested_workflow_row("wf-002", "review"))),
        agent_view(agent),
        settings_view(
            "theme",
            vec![setting_row("ui.theme", "\"dark\"", "global", &[])],
        ),
        config_view(&[("ui.keys", "\"vim\"", "global")]),
        runs_view(vec![run_summary("run-02", "Old run", "parked", true)]),
        all_repos_view(vec![(
            "~/code/web",
            vec![run_summary("run-03", "Web", "parked", true)],
        )]),
        raw_view(agent, &["{}"]),
        diff_view(agent, "diff --git"),
        take_over(agent, &["sh", "-c", "exit 0"], "/wt/card-002"),
        take_over(&Agent::endpoint(4), &[], "/wt/card-005"),
    ]);
    events.extend(
        chat_turn("t-002", "hi", "Hi there.", 10)
            .into_iter()
            .map(|(_, e)| e),
    );
    events.extend(long_replayed_run(2).2);
    for event in &events {
        round_trips(event);
    }
}

#[test]
fn words_keep_their_spaces() {
    assert_eq!(words("one two  three"), ["one ", "two  ", "three"]);
    assert_eq!(words("one two").concat(), "one two");
}

#[test]
fn scripts_are_one_json_line_each() {
    let script = Script::idle()
        .await_command("submit")
        .await_signal("typed")
        .at(50, exit());
    let text = script.to_jsonl();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[3], json!({"await_command": "submit"}));
    assert_eq!(lines[4], json!({"await_signal": "typed"}));
    assert_eq!(lines[5]["at_ms"], 50);
    assert_eq!(lines[5]["event"]["type"], "exit");
}

#[test]
fn a_bell_ending_a_title_is_not_rung() {
    let raw = b"\x1b]0;\xe2\x9c\xbb Argonauts\x07text\x07\x1b]2;two\x1b\\\x07";
    assert_eq!(bells(raw), 2);
    assert_eq!(titles(raw), ["✻ Argonauts", "two"]);
}

#[test]
fn colors_are_told_from_attributes() {
    assert!(!sets_color(b"\x1b[1m\x1b[2mdim\x1b[0m"));
    assert!(sets_color(b"\x1b[38;5;244mgrey"));
    assert!(sets_color(b"\x1b[1;31mred"));
    assert!(!sets_color(b"\x1b[38;2;1;2;3"));
}

#[test]
fn the_frame_checker_catches_a_jagged_edge() {
    let straight = Screen {
        scrollback: vec![],
        visible: vec![
            "╭──┬──╮".to_owned(),
            "│ab│cd│".to_owned(),
            "╰──┴──╯".to_owned(),
        ],
    };
    assert_frames(&straight, 10);
    let jagged = Screen {
        scrollback: vec![],
        visible: vec!["╭──┬──╮".to_owned(), "│ab│c│".to_owned()],
    };
    let caught = std::panic::catch_unwind(|| assert_frames(&jagged, 10));
    assert!(caught.is_err());
}

/// The harness with its folder, which has to outlive it.
fn harness_with(env: &[(&str, &str)], script: &str) -> (Pty, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.jsonl");
    std::fs::write(&path, script).unwrap();
    let mut command = PtyCommand::new(env!("CARGO_BIN_EXE_argonauts-tui-harness"))
        .args([path.as_os_str()])
        .env("ARGONAUTS_HOME", dir.path().join("home").as_os_str())
        .size(ROWS, COLS);
    for (name, value) in env {
        command = command.env(name, value);
    }
    (Pty::spawn(command).unwrap(), dir)
}

#[test]
fn a_misspelled_ui_setting_is_refused() {
    let (mut pty, _dir) = harness_with(&[("HARNESS_UI", "kyes = \"vim\"")], "");
    pty.wait_for("unknown key \"kyes\"", WAIT).unwrap();
    assert_ne!(pty.wait_for_exit(WAIT).unwrap(), 0);
}

#[test]
fn a_bad_script_line_is_refused_with_its_number() {
    let script = format!(
        "{}\n{{\"at_ms\": 5}}\n",
        Script::idle().to_jsonl().lines().next().unwrap()
    );
    let (mut pty, _dir) = harness_with(&[], &script);
    pty.wait_for("line 2", WAIT).unwrap();
    assert_ne!(pty.wait_for_exit(WAIT).unwrap(), 0);
}
