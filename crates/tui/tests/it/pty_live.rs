//! The live region under the conversation: live blocks, the narrator's line, the working line, the
//! input between its rules, and the bottom line.

use crate::pty_support;
use std::time::{Duration, Instant};

use pty_support::*;
use serde_json::json;

fn working(agents: usize) -> (RunScript, Vec<Agent>, Script) {
    let (run, crew, events) = crew_run(agents);
    (run, crew, Script::idle().all(0, events))
}

#[test]
fn live_block_updates_in_place() {
    let (mut run, crew, script) = working(1);
    let agent = &crew[0];
    let more = vec![
        run.tool_finished(agent, "s-001-edit", true, "Updated src/export.rs"),
        run.tool_started(
            agent,
            "s-001-bash",
            "Bash",
            json!({"command": "cargo test export"}),
        ),
        run.usage(agent, 24_300),
    ];
    // The block that updates in place is the one an agent at work has when asked for.
    let driver = TestDriver::launch(script.await_signal("more").all(0, more))
        .ui("agents_in_conversation = \"as_they_work\"")
        .start();
    // A working agent's bullet blinks, so its header is counted in either phase.
    let header = format!("^({BULLET}| ) Opus on Claude Code · worker · task-002");
    let before = driver.wait_visible("src/export.rs");
    assert_eq!(count_rows(&before, &header), 1, "{}", before.text());
    driver.signal("more");
    let after = driver.wait_visible("cargo test export");
    driver.wait_visible("24\\.3k");
    assert_eq!(
        count_rows(&after, &header),
        1,
        "one block, redrawn:\n{}",
        after.text()
    );
    assert!(
        after
            .scrollback
            .iter()
            .all(|row| !row.contains("task-002 (Add")),
        "a live block never goes to the scrollback while it runs:\n{}",
        after.text()
    );
}

#[test]
fn live_region_working_line_rules_bottom_row() {
    let (_, _, script) = working(2);
    let driver = TestDriver::start(script);
    let screen = driver.wait_visible(WORKING);
    let working = expect_row(&screen, WORKING_LINE);
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        screen.visible[working + 1].is_empty(),
        "{}",
        screen.visible_text()
    );
    assert_eq!(rule, working + 2, "{}", screen.visible_text());
    assert!(
        screen.visible[rule + 1].starts_with('❯'),
        "{}",
        screen.visible_text()
    );
    assert!(
        is_rule(&screen.visible[rule + 2]),
        "{}",
        screen.visible_text()
    );
    // At 100 columns the chips take the room of the mode's first two hints, and Esc leaves a
    // run's agents going, so the line doesn't offer to interrupt.
    let mode = &screen.visible[rule + 3];
    assert!(
        mode.starts_with("  ǁ supervised mode on")
            && mode.contains("←  2 agents")
            && !mode.contains("esc to interrupt"),
        "{}",
        screen.visible_text()
    );
}

#[test]
fn live_region_keeps_its_order() {
    let (mut run, crew, events) = crew_run(2);
    let script = Script::idle()
        .all(0, events)
        .at(
            0,
            status_with(json!({"run": RUN, "narrator": "live", "line": line_with_spend()})),
        )
        .at(0, narrator_status(0.06, "$0.50"))
        .at(
            0,
            narrator_line(RUN, "Both agents are writing the JSON writer."),
        )
        .at(0, run.question_approval("approval-001", &crew[0]))
        .await_command("submit")
        .at(0, run.queued(&crew[1], "m-001", "keep the CSV path"));
    // Wide enough for the narrator's spend to fit on the bottom line beside its hints.
    let mut driver = TestDriver::launch(script)
        .ui("narrator = \"live\"\nagents_in_conversation = \"as_they_work\"\n[panel]\nagents = true")
        .size(ROWS, 200)
        .start();
    driver.wait_visible("※ narrator:");
    driver.wait_visible("◆");
    // The question takes the keys, so guiding the run hands it on first.
    past_the_question_guard();
    carry_on(&mut driver);
    driver.wait_visible("◆ Question left to the agent · you, ");
    driver.submit("keep the CSV path");
    driver.wait_visible("keep the CSV path");
    // A working agent's bullet blinks, so the order is read from a frame that shows it.
    let opus = format!("^{BULLET} Opus on Claude Code · worker · task-002");
    let screen = driver.wait_visible(&opus);
    assert_order(
        &screen,
        &[
            &opus,
            "※ narrator: Both agents",
            WORKING_LINE,
            "keep the CSV path",
            TOP_RULE,
            INPUT_ROW,
            "ǁ supervised mode on .* narrator \\$0\\.06/\\$0\\.50",
            // Agent rows sit indented under their run's row.
            "^    ○ task-002 ",
            "^    ○ task-003 ",
        ],
    );
    let last = expect_row(&screen, "^    ○ task-003 ");
    assert_eq!(
        last + 1 + blank_rows_at_bottom(&screen),
        screen.visible.len(),
        "nothing under the panel:\n{}",
        screen.visible_text()
    );
}

#[test]
fn working_line_elapsed_tokens_esc_hint() {
    let script = Script::idle().await_command("submit").all(
        0,
        [
            mode_answering("t-001"),
            turn_started("t-001", "check the build"),
            conversation_tool_started("t-001", "c1", "Bash", json!({"command": "cargo build"})),
            conversation_usage("t-001", 3600),
        ],
    );
    let mut driver = TestDriver::start(script);
    driver.wait_for(TOP_RULE);
    driver.submit("check the build");
    let screen = driver.wait_visible(r"^[∙✢✲✶✻✼*] [\w'-]+… \(\d+s · ↓  3\.6k tokens");
    let working = expect_row(&screen, WORKING_LINE);
    assert!(
        !screen.visible[working].contains('$'),
        "{}",
        screen.visible_text()
    );
    driver.wait_visible(WORKING);
    let later = driver.wait_visible(r"… \([1-9]\d*s ");
    assert!(row_of(&later, WORKING_LINE).is_some());
}

#[test]
fn queued_message_gray_and_taken_back() {
    let (mut run, crew, script) = working(1);
    let queued = run.queued(&crew[0], "m-001", "keep a fallback");
    let script = script
        .await_command("submit")
        .at(0, queued)
        .await_command("take_back")
        .at(0, taken_back(&["keep a fallback"]));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    driver.submit("keep a fallback");
    let screen = driver.wait_visible("^❯ keep a fallback");
    let queued_row = expect_row(&screen, "^❯ keep a fallback");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        queued_row < rule,
        "queued above the input:\n{}",
        screen.visible_text()
    );
    driver.press(Key::Up);
    let take_back = driver.wait_command("take_back");
    assert_eq!(take_back["run"], RUN);
    assert_eq!(take_back["messages"], json!(["m-001"]));
    let screen = driver.wait_until("the text back in the input", |screen| {
        row_of(screen, TOP_RULE).is_some_and(|rule| {
            screen
                .visible
                .get(rule + 1)
                .is_some_and(|row| row.contains("keep a fallback"))
        })
    });
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        screen.visible[..rule]
            .iter()
            .all(|row| !row.contains("keep a fallback")),
        "no longer queued:\n{}",
        screen.visible_text()
    );
}

#[test]
fn shift_tab_cycles_approval_mode() {
    // Shift+Tab's cycle: supervised, auto, autopilot.
    let script = Script::idle()
        .await_command("approval_mode")
        .at(0, status_with(json!({"approval_mode": "auto"})))
        .await_command("approval_mode")
        .at(0, status_with(json!({"approval_mode": "autopilot"})))
        .await_command("approval_mode")
        .at(0, status_with(json!({"approval_mode": "supervised"})));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MODE_IDLE);
    driver.press(Key::ShiftTab);
    assert_eq!(driver.wait_command("approval_mode")["mode"], "auto");
    driver.wait_visible("^  ⏵⏵ auto mode on \\(shift\\+tab to cycle\\)");
    driver.press(Key::ShiftTab);
    assert_eq!(driver.wait_command("approval_mode")["mode"], "autopilot");
    driver.wait_visible("^  ⏵⏵ autopilot on \\(shift\\+tab to cycle\\)");
    driver.press(Key::ShiftTab);
    assert_eq!(driver.wait_command("approval_mode")["mode"], "supervised");
    driver.wait_visible(MODE_IDLE);
}

#[test]
fn bottom_line_changed_agent_plan_usage_spend() {
    let status = status_with(json!({
        "main_agent": "claude-code/opus",
        "plan_chips": [chip("Claude", Some(plan_usage("claude-code", 0.71)), "healthy"),
                   chip("Codex", Some(plan_usage("codex", 0.38)), "healthy"),
                   chip("agy", None, "healthy")],
        "spent": spend(9000, 0.21),
        "line": {"settings": {"right": ["plan-usage", "spend"], "plan-usage": {"show": "all"}}}}));
    let driver = TestDriver::launch(Script::idle().at(0, status))
        .size(ROWS, 200)
        .start();
    // The rule names the model, so the bottom line never does. It marks the CLI in use, and
    // keeps a CLI nothing was read for yet by its name.
    let chips = r"^  ǁ supervised mode on .*  ● Claude 5h .{6}  71%   ○ Codex 5h .{6}  38%   ○ agy  \$0\.21$";
    let screen = driver.wait_visible(chips);
    let status = expect_row(&screen, chips);
    assert_eq!(
        status + 1 + blank_rows_at_bottom(&screen),
        screen.visible.len()
    );
    let rule = expect_row(&screen, "^─+ council ─$");
    assert!(screen.visible[rule].ends_with(" council ─"));
}

#[test]
fn bottom_line_plan_usage_chips() {
    // Out until its window resets, three hours from now.
    let mut out = plan_usage("claude-code", 1.0);
    out["status"] = json!("rejected");
    let status = status_with(json!({
        "plan_chips": [chip("Claude", Some(out), "healthy"),
                   chip("Codex", Some(plan_usage("codex", 0.38)), "down"),
                   chip("agy", None, "healthy")],
        "line": {"settings": {"plan-usage": {"show": "all"}}}}));
    let driver = TestDriver::launch(Script::idle().at(0, status))
        .size(ROWS, 200)
        .start();
    let out_until = r"● Claude out until (\w{3} )?\d{1,2}:\d{2}[ap]m   ○ Codex down   ○ agy$";
    let screen = driver.wait_visible(out_until);
    let row = &screen.visible[expect_row(&screen, out_until)];
    // agy's plan usage isn't known yet, and it still has its place by name.
    assert!(row.ends_with("○ agy"), "{row}");
    assert!(
        !row.contains("38%"),
        "a down agent shows down, not its reading: {row}"
    );

    let fine = status_with(json!({
        "plan_chips": [chip("Claude", Some(plan_usage("claude-code", 0.92)), "healthy"),
                   chip("Codex", Some(plan_usage("codex", 0.38)), "healthy")]}));
    let driver = TestDriver::launch(Script::idle().at(0, fine))
        .size(ROWS, 200)
        .start();
    // From 90% the window shows when it resets.
    let near = r"● Claude 5h .{6}  92% resets (\w{3} )?\d{1,2}:\d{2}[ap]m   ○ Codex 5h .{6}  38%$";
    let screen = driver.wait_visible(near);
    assert!(row_of(&screen, near).is_some());
}

/// With `run` among its parts, the bottom line names the workflow under way and counts its
/// tasks as they merge.
#[test]
fn the_bottom_line_names_the_workflow_under_way() {
    let (mut run, crew, events) = crew_run(5);
    let named = json!({"type": "run", "data": {"event": "named", "name": "add-json-flag",
        "description": "Add a --json flag to export", "style": "workflow"}});
    let line = json!({"settings": {"right": ["run", "plan-usage"]}});
    let mut script = Script::idle()
        .all(0, events)
        .at(0, status_with(json!({"run": RUN, "line": line})))
        .at(0, run.event(None, None, named));
    for agent in &crew[..3] {
        script = script.at(0, run.task_state(agent, Some("running"), "merged"));
    }
    let driver = TestDriver::launch(script).size(ROWS, 120).start();
    let screen = driver.wait_visible(r"  add-json-flag .{6} 3/5  ● Claude 5h .{6}  71%");
    assert!(
        row_of(&screen, "▄▄▄▄▁▁ 3/5").is_some(),
        "a small bar, three fifths full to the nearest cell: {:?}",
        screen.visible
    );
}

#[test]
fn bottom_line_shows_narrator_spend() {
    let script = Script::idle()
        .at(
            0,
            status_with(json!({"narrator": "live", "line": line_with_spend()})),
        )
        .at(0, narrator_status(0.06, "$0.50"));
    let driver = TestDriver::launch(script)
        .ui("narrator = \"live\"")
        .size(ROWS, 200)
        .start();
    let screen = driver.wait_visible("narrator \\$0\\.06/\\$0\\.50");
    let row = expect_row(&screen, "narrator \\$0\\.06/\\$0\\.50");
    assert!(
        screen.visible[row].contains("● Claude 5h ")
            && screen.visible[row].contains("ǁ supervised mode on"),
        "on the bottom line"
    );
}

#[test]
fn narrator_lane() {
    let (_, _, events) = crew_run(2);
    let script = Script::idle()
        .all(0, events)
        .at(0, status_with(json!({"run": RUN, "narrator": "live"})))
        .at(
            0,
            narrator_line(RUN, "Two agents are adding the JSON writer and its tests."),
        );
    let driver = TestDriver::launch(script).ui("narrator = \"live\"").start();
    let screen = driver.wait_visible("※ narrator: Two agents");
    let narrator = expect_row(&screen, "^※ narrator: Two agents");
    let working = expect_row(&screen, WORKING_LINE);
    assert_eq!(
        narrator + 1,
        working,
        "just above the working line:\n{}",
        screen.visible_text()
    );
    for row in &screen.visible[narrator.saturating_sub(1)..=narrator] {
        assert!(
            !row.chars().any(|c| "╭╮╰╯│┌┐└┘".contains(c)),
            "no box around the narrator: {row:?}"
        );
    }
}

#[test]
fn paused_record_line() {
    let (mut run, _, script) = working(2);
    let script = script
        .await_signal("paused")
        .at(0, mode_pausing(RUN))
        .at(0, run.paused(2))
        .at(0, run.waiting("paused"))
        .at(0, mode_waiting(RUN, "paused"));
    let driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    driver.signal("paused");
    let screen = driver.wait_visible("(?i)paused");
    let record = expect_row(&screen, "(?i)paused");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        record < rule,
        "the record goes into the conversation:\n{}",
        screen.visible_text()
    );
    let conversation = screen.scrollback.iter().chain(&screen.visible[..rule]);
    let records = conversation
        .filter(|row| row.to_lowercase().contains("paused"))
        .count();
    assert_eq!(records, 1, "one record line:\n{}", screen.text());
}

#[test]
fn verbose_twice_returns_to_normal() {
    // A conversation turn's calls always fold into one line, as Claude Code folds them, so
    // the levels show on an agent's calls in a run.
    let mut run = RunScript::default();
    let agent = Agent::numbered(1);
    let reads = |run: &mut RunScript, level: &str| {
        let (a, b) = (format!("{level}-a"), format!("{level}-b"));
        vec![
            run.tool_started(&agent, &a, "Read", json!({"file_path": "src/a.rs"})),
            run.tool_finished(&agent, &a, true, "fn a() {}"),
            run.tool_started(&agent, &b, "Read", json!({"file_path": "src/b.rs"})),
            run.tool_finished(&agent, &b, true, "fn b() {}"),
            run.message(&agent, &format!("Both read at {level}.")),
        ]
    };
    let start = vec![
        mode_working(RUN),
        run.started("Add a --json flag to export", "crew"),
        run.task_created(&agent),
        run.session_started(&agent, 1),
    ];
    let full = reads(&mut run, "full");
    let normal = reads(&mut run, "normal");
    let script = Script::idle()
        .all(0, start)
        .await_command("verbosity")
        .all(0, full)
        .await_command("verbosity")
        .all(0, normal);
    let mut driver = TestDriver::launch(script)
        .ui("verbosity = \"normal\"\nagents_in_conversation = \"as_they_work\"")
        .start();
    driver.wait_visible(WORKING);
    driver.send("/verbosity full");
    driver.press(Key::Enter);
    let full = driver.wait_visible("Both read at full\\.");
    assert!(
        row_of(&full, "Read\\(src/a\\.rs\\)").is_some()
            && row_of(&full, "Read\\(src/b\\.rs\\)").is_some(),
        "full shows every call:\n{}",
        full.visible_text()
    );
    driver.send("/verbosity normal");
    driver.press(Key::Enter);
    let normal = driver.wait_visible("Both read at normal\\.");
    let before = last_row_of(&normal, "Both read at full\\.").unwrap_or(0);
    assert!(
        normal.visible[before..]
            .iter()
            .any(|row| row.contains("Read 2 files")),
        "normal folds the calls into one line:\n{}",
        normal.visible_text()
    );
    let levels: Vec<String> = driver
        .command_values()
        .iter()
        .filter(|c| command_type(c) == "verbosity")
        .map(|c| c["level"].as_str().unwrap_or_default().to_owned())
        .collect();
    let slashes = driver
        .command_values()
        .iter()
        .filter(|c| command_type(c) == "slash" && c["command"]["command"] == "verbose")
        .count();
    assert!(
        levels == ["full", "normal"] || slashes == 2,
        "the screen tells the app each change: {:?}",
        driver.command_values()
    );
}

#[test]
fn always_on_agent_warning_once() {
    let warning = "openclaw is always on · approvals refuse its shell · see argonauts doctor";
    let script = Script::idle()
        .at(0, notice("warn", warning))
        .await_command("submit")
        .at(0, notice("warn", warning))
        .at(0, bottom_line());
    let mut driver = TestDriver::start(script);
    driver.wait_for("openclaw is always on");
    driver.submit("hello");
    driver.wait_for("❯ hello");
    let screen = driver.screen();
    assert_eq!(
        count_rows(&screen, "openclaw is always on"),
        1,
        "{}",
        screen.text()
    );
}

/// 300 events a second for two seconds at 120 columns, spread over three agents. A frame is
/// dropped when the screen stays the same for longer than `STALL` while events keep coming.
/// A machine pauses now and then, which drops a frame or two the renderer never caused, so a
/// few short stalls pass; a renderer that falls behind stalls again and again, or for long.
#[test]
#[ignore = "timed: run alone on an optimized build"]
fn renders_300_events_per_second_at_120_columns() {
    const EVENTS: u64 = 600;
    const STALL: Duration = Duration::from_millis(100);
    let (mut run, crew, events) = crew_run(3);
    let mut stream = Vec::new();
    for i in 0..EVENTS {
        let agent = &crew[(i % 3) as usize];
        let at = i * 10 / 3;
        let id = format!("{}-{i}", agent.session);
        let event = match i % 3 {
            0 => run.tool_started(
                agent,
                &id,
                "Read",
                json!({"file_path": format!("src/file_{i}.rs")}),
            ),
            1 => run.text_delta(agent, &format!("step {i} ")),
            _ => run.usage(agent, 1000 + i * 10),
        };
        stream.push((at, event));
    }
    let last = format!("src/file_{}\\.rs", (EVENTS - 1) / 3 * 3);
    let script = Script::idle()
        .all(0, events)
        .await_signal("go")
        .timed(stream);
    let driver = TestDriver::launch(script)
        .size(40, 120)
        .ui("[panel]\nagents = true")
        .start();
    driver.wait_visible(WORKING);
    let frames_before = sync_updates(&driver.raw());
    driver.signal("go");
    let started = Instant::now();
    let last_event = regex::Regex::new(&last).unwrap();
    let mut previous = driver.visible();
    let mut changed_at = Instant::now();
    let mut dropped = 0;
    let mut longest = Duration::ZERO;
    loop {
        let now = Instant::now();
        let current = driver.visible();
        if current != previous {
            let gap = now - changed_at;
            longest = longest.max(gap);
            if gap > STALL {
                dropped += 1;
            }
            previous = current;
            changed_at = now;
        }
        if last_event.is_match(&previous) {
            break;
        }
        assert!(
            now - started < WAIT,
            "the last event never showed:\n{previous}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let elapsed = started.elapsed();
    let frames = sync_updates(&driver.raw()) - frames_before;
    eprintln!(
        "{EVENTS} events in {elapsed:?}: {frames} frames, {dropped} dropped, longest gap {longest:?}"
    );
    assert!(
        dropped <= 4 && longest < Duration::from_millis(500),
        "{dropped} dropped, longest gap {longest:?}"
    );
    assert!(frames > 0, "each frame is one synchronized update");
    let cap = (elapsed.as_secs_f64() * 120.0).ceil() as usize + 2;
    assert!(
        frames <= cap,
        "{frames} frames in {elapsed:?}, over 120 a second"
    );
    assert_frames(&driver.screen(), 120);
}

/// Frames drawn as synchronized updates (`CSI ? 2026 h`).
fn sync_updates(raw: &[u8]) -> usize {
    raw.windows(8).filter(|w| w == b"\x1b[?2026h").count()
}
