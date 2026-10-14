//! Full-screen views: the crew view and its tabs, one agent's session with its input side,
//! steering, follow mode, and the resume picker.

mod pty_support;

use pty_support::*;
use serde_json::{Value, json};

/// A crew of three at work, with the first agent's question waiting.
fn crew_needing_you() -> (RunScript, Vec<Agent>, Script) {
    let (mut run, crew, events) = crew_run(3);
    let question = run.question_checkpoint("cp-001", &crew[0]);
    let script = Script::idle().all(0, events).at(0, question);
    (run, crew, script)
}

fn has_command(harness: &Harness, check: impl Fn(&Value) -> bool) -> bool {
    harness.command_values().iter().any(check)
}

#[test]
fn crew_view_tabs_and_quotas() {
    let (_, _, script) = crew_needing_you();
    let mut harness = Harness::start(script);
    harness.wait_visible("Keep a CSV fallback\\?");
    // The question has the keys until it's handed on.
    past_the_card_guard();
    harness.press(Key::Esc);
    harness.wait_visible("◆ Question left to the agent · you, ");
    harness.press(Key::Left);
    let view = harness.wait_gone(TOP_RULE);
    let title = expect_row(&view, "argonauts › crew");
    assert!(
        view.visible[title].contains("Claude 71%"),
        "{}",
        view.visible_text()
    );
    let tabs = expect_row(&view, "\\[ All \\d+ \\]");
    for tab in ["Needs you 1", "Working", "Ready", "Done"] {
        assert!(
            view.visible[tabs].contains(tab),
            "{tab}:\n{}",
            view.visible_text()
        );
    }
    assert_order(
        &view,
        &[
            "Add a --json flag to export",
            "Opus on Claude Code",
            "GPT 5\\.6 on Codex",
        ],
    );

    harness.press(Key::Tab);
    let needs_you = harness.wait_visible("\\[ Needs you 1 \\]");
    assert!(
        row_of(&needs_you, "GPT 5\\.6 on Codex").is_none(),
        "{}",
        needs_you.visible_text()
    );
    harness.press(Key::ShiftTab);
    harness.wait_visible("GPT 5\\.6 on Codex");
    harness.press(Key::Esc);
    harness.wait_visible(TOP_RULE);
}

#[test]
fn crew_view_lists_conversations_and_other_repos() {
    let (_, _, script) = crew_needing_you();
    let rows = vec![
        json!({"target": {"kind": "conversation", "chat": "chat-002"}, "state": "ready",
               "title": "How does the export pick a format?", "repo": REPO,
               "worker": "claude-code/opus", "latest": "It reads --format first.",
               "updated": "2026-09-30T11:00:00Z"}),
        json!({"target": {"kind": "run", "run": "run-07"}, "state": "needs_you",
               "title": "Fix the login redirect", "repo": "~/code/web",
               "run": {"id": "run-07", "title": "Fix the login redirect",
                       "stands": {"state": "needs_you", "word": "plan approval",
                                  "since": "2026-09-29T06:00:00Z"}},
               "latest": "plan approval", "updated": "2026-09-29T06:00:00Z"}),
    ];
    let script = script.await_command("request").at(0, crew_view_data(rows));
    let mut harness = Harness::start(script);
    harness.wait_visible("Keep a CSV fallback\\?");
    // The question has the keys until it's handed on.
    past_the_card_guard();
    harness.press(Key::Esc);
    harness.wait_visible("◆ Question left to the agent · you, ");
    harness.press(Key::Left);
    let request = harness.wait_command("request");
    assert_eq!(request["view"], json!({"view": "crew"}));
    let view = harness.wait_visible("~/code/web");
    assert_order(
        &view,
        &[
            REPO,
            "Sessions",
            "How does the",
            "~/code/web",
            "Fix the login redirect · plan approval",
        ],
    );
    let chat = &view.visible[expect_row(&view, "How does the")];
    assert!(chat.contains("Claude Code · enter continues"), "{chat}");
    assert!(view.visible[expect_row(&view, "plan approval")].contains('◆'));
}

fn session_script(agent_parts: &[(&str, &str)]) -> (RunScript, Vec<Agent>, Script) {
    let (mut run, crew, events) = crew_run(2);
    let agent = crew[0].clone();
    let extra = vec![
        run.session_input(&agent, agent_parts),
        run.message(&agent, "The JSON writer goes beside the CSV one."),
    ];
    let script = Script::idle().all(0, events).all(0, extra);
    (run, crew, script)
}

const PARTS: [(&str, &str); 3] = [
    (
        "task",
        "You own task-002: Add the JSON writer.\nKeep the CSV path working.",
    ),
    ("goal", "Export takes --json and prints valid JSON."),
    ("notes", "Use serde_json.\nNo new dependencies."),
];

fn open_first_session(harness: &mut Harness) -> Screen {
    // The run's row comes first, so the first agent is two rows down.
    harness.press(Key::Down);
    harness.press(Key::Down);
    harness.press(Key::Enter);
    harness.wait_gone(TOP_RULE)
}

#[test]
fn session_view_output_and_input() {
    let (_, _, script) = session_script(&PARTS);
    let mut harness = Harness::launch(script).ui("[panel]\nagents = true").start();
    harness.wait_visible("↓ to manage");
    open_first_session(&mut harness);
    let output = harness.wait_visible("JSON writer goes beside");
    assert!(
        row_of(&output, "Read").is_some(),
        "tool calls:\n{}",
        output.visible_text()
    );
    // A read says how many lines it read, as Claude Code's does, and never shows the file.
    assert!(
        row_of(&output, "fn write").is_none(),
        "without the file:\n{}",
        output.visible_text()
    );
    // Tab goes to what the agent was asked, each part under its own heading, and the launch
    // folded to one line under them.
    harness.press(Key::Tab);
    let input = harness.wait_visible("Launched as .* exec --json");
    // The goal file is what you asked for, so it reads as the request.
    for heading in ["Task", "Request", "Notes"] {
        assert!(
            row_of(&input, heading).is_some(),
            "{heading}:\n{}",
            input.visible_text()
        );
    }
    harness.press(Key::ShiftTab);
    harness.wait_visible("JSON writer goes beside");
}

#[test]
fn session_input_side_exact() {
    let (_, _, script) = session_script(&PARTS);
    let mut harness = Harness::launch(script).ui("[panel]\nagents = true").start();
    harness.wait_visible("↓ to manage");
    open_first_session(&mut harness);
    harness.press(Key::Tab);
    let input = harness.wait_visible("No new dependencies\\.");
    let lines: Vec<&str> = PARTS.iter().flat_map(|(_, text)| text.lines()).collect();
    let mut last = 0;
    for line in lines {
        let row = input
            .visible
            .iter()
            .position(|row| row.trim_end().ends_with(line))
            .unwrap_or_else(|| panic!("{line:?} word for word:\n{}", input.visible_text()));
        assert!(row >= last, "{line:?} in order:\n{}", input.visible_text());
        last = row;
    }
}

#[test]
fn session_view_steer_raw_diff() {
    let (_, crew, script) = session_script(&PARTS);
    let agent = crew[0].clone();
    let script = script
        .await_command("request")
        .at(0, raw_view(&agent, &["{\"type\":\"raw agent line\"}"]))
        .await_command("request")
        .at(
            0,
            diff_view(
                &agent,
                "-        self.write_csv(out)\n+        match self.format {",
            ),
        );
    let mut harness = Harness::launch(script).ui("[panel]\nagents = true").start();
    harness.wait_visible("↓ to manage");
    open_first_session(&mut harness);
    harness.press(Key::Char('m'));
    harness.send("use serde_json");
    harness.press(Key::Enter);
    let steer = harness.wait_command("steer");
    assert_eq!(
        steer,
        json!({"type": "steer", "run": RUN, "session": "s-001", "text": "use serde_json"})
    );
    harness.press(Key::Char('r'));
    let raw = harness.wait_command("request");
    assert_eq!(
        raw["view"],
        json!({"view": "raw", "run": RUN, "session": "s-001"})
    );
    // The raw log is the view's Raw tab, and the view's keys still work there.
    harness.wait_visible("raw agent line");
    harness.wait_visible(" Output  Input  Raw ");
    harness.press(Key::Char('d'));
    let diff = harness.wait_command("request");
    assert_eq!(diff["view"]["view"], "diff");
    assert_eq!(diff["view"]["card"], agent.card);
    harness.wait_visible("match self\\.format");
}

#[test]
fn follow_mode_1_to_9_end_esc() {
    let (mut run, crew, events) = crew_run(3);
    let agent = crew[1].clone();
    let mut history = Vec::new();
    for n in 0..30 {
        let id = format!("follow-{n}");
        history.push(run.tool_started(
            &agent,
            &id,
            "Read",
            json!({"file_path": format!("src/part_{n}.rs")}),
        ));
        history.push(run.tool_finished(&agent, &id, true, "ok"));
    }
    let latest = run.message(&agent, "Streamed while you follow.");
    let script = Script::idle()
        .all(0, events)
        .all(0, history)
        .await_signal("more")
        .at(0, latest);
    let mut harness = Harness::launch(script).ui("[panel]\nagents = true").start();
    harness.wait_visible("↓ to manage");
    harness.press(Key::Down);
    harness.press(Key::Char('2'));
    let follow = harness.wait_gone(TOP_RULE);
    assert!(
        follow.visible_text().contains("GPT 5.6 on Codex"),
        "{}",
        follow.visible_text()
    );
    harness.wait_visible("part_29");
    harness.signal("more");
    harness.wait_visible("Streamed while you follow\\.");
    harness.press(Key::PageUp);
    harness.wait_gone("Streamed while you follow\\.");
    harness.press(Key::End);
    harness.wait_visible("Streamed while you follow\\.");
    harness.press(Key::Esc);
    harness.wait_visible(TOP_RULE);
}

#[test]
fn resume_picker() {
    let runs = vec![
        run_summary("run-03", "Add a --json flag to export", "parked", true),
        run_summary("run-02", "Fix the flaky export test", "crashed", false),
        run_summary("run-01", "Rename the CSV writer", "merged", false),
    ];
    let script = Script::idle().await_signal("runs").at(0, runs_view(runs));
    let mut harness = Harness::start(script);
    harness.wait_visible(MODE_IDLE);
    harness.send("/resume");
    harness.press(Key::Enter);
    harness.wait_until("the screen to ask for the runs", |_| {
        has_command(&harness, |c| {
            c["view"]["view"] == "runs" || c["command"]["command"] == "resume"
        })
    });
    harness.signal("runs");
    let picker = harness.wait_visible("Fix the flaky export test");
    assert_order(
        &picker,
        &[
            "Add a --json flag to export",
            "Fix the flaky export test",
            "Rename the CSV writer",
        ],
    );
    let reminded = expect_row(&picker, "Add a --json flag to export");
    assert!(
        picker.visible[reminded].contains('◆'),
        "{}",
        picker.visible_text()
    );
    let crashed = expect_row(&picker, "Fix the flaky export test");
    assert!(!picker.visible[crashed].contains('◆'));
    let hints = last_row_of(&picker, "\\bx\\b").unwrap_or(0);
    for key in ["\\br\\b", "\\bd\\b", "\\bx\\b"] {
        assert!(
            regex::Regex::new(key)
                .unwrap()
                .is_match(&picker.visible[hints]),
            "{key} in the key line:\n{}",
            picker.visible_text()
        );
    }
    // Enter resumes; `r` is the run's raw log.
    harness.press(Key::Down);
    harness.press(Key::Enter);
    harness.wait_until("the resume", |_| {
        has_command(&harness, |c| {
            c["run"] == "run-02" || c["command"]["run"] == "run-02"
        })
    });
}
