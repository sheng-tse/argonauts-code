//! The live region under the chat: live blocks, the narrator's line, the working line, the
//! input between its rules, the mode line, and the status line.

mod pty_support;

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
    let harness = Harness::start(script.await_signal("more").all(0, more));
    let header = format!("^{BULLET} Opus on Claude Code · owner · card-002");
    let before = harness.wait_visible("src/export.rs");
    assert_eq!(count_rows(&before, &header), 1, "{}", before.text());
    harness.signal("more");
    let after = harness.wait_visible("cargo test export");
    harness.wait_visible("24\\.3k");
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
            .all(|row| !row.contains("card-002 (Add")),
        "a live block never goes to the scrollback while it runs:\n{}",
        after.text()
    );
}

#[test]
fn live_region_working_line_rules_mode_line() {
    let (_, _, script) = working(2);
    let harness = Harness::start(script);
    let screen = harness.wait_visible("esc to interrupt");
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
    assert!(
        screen.visible[rule + 3]
            .starts_with("  ǁ supervised mode on (shift+tab to cycle) · esc to interrupt"),
        "{}",
        screen.visible_text()
    );
}

#[test]
fn live_region_keeps_its_order() {
    let (mut run, crew, events) = crew_run(2);
    let script = Script::idle()
        .all(0, events)
        .at(0, status_with(json!({"run": RUN, "narrator": "live"})))
        .at(0, narrator_status(0.06, "$0.50"))
        .at(
            0,
            narrator_line(RUN, "Both agents are writing the JSON writer."),
        )
        .at(0, run.question_checkpoint("cp-001", &crew[0]))
        .await_command("submit")
        .at(0, run.queued(&crew[1], "m-001", "keep the CSV path"));
    // Wide enough for the narrator's spend to fit on the mode line beside its hints.
    let mut harness = Harness::launch(script)
        .ui("narrator = \"live\"\n[panel]\nagents = true")
        .size(ROWS, 200)
        .start();
    harness.wait_visible("※ narrator:");
    harness.wait_visible("◆");
    // The question takes the keys, so guiding the run sets it aside first.
    harness.press(Key::Esc);
    harness.wait_visible("· needs you · ↑ to answer");
    harness.submit("keep the CSV path");
    let screen = harness.wait_visible("keep the CSV path");
    assert_order(
        &screen,
        &[
            &format!("^{BULLET} Opus on Claude Code · owner · card-002"),
            "※ narrator: Both agents",
            WORKING_LINE,
            "◆",
            "keep the CSV path",
            TOP_RULE,
            INPUT_ROW,
            "ǁ supervised mode on .* narrator \\$0\\.06/\\$0\\.50",
            // Agent rows sit indented under their run's row.
            "^ +◐ Opus on Claude Code",
            "^ +◐ GPT 5\\.6 on Codex",
        ],
    );
    let last = expect_row(&screen, "^ +◐ GPT 5\\.6 on Codex");
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
            chat_tool_started("t-001", "c1", "Bash", json!({"command": "cargo build"})),
            chat_usage("t-001", 3600),
        ],
    );
    let mut harness = Harness::start(script);
    harness.wait_for(TOP_RULE);
    harness.submit("check the build");
    let screen = harness.wait_visible(r"^[∙✢✲✶✻✼*] [\w'-]+… \(\d+s · ↓ 3\.6k tokens");
    let working = expect_row(&screen, WORKING_LINE);
    assert!(
        !screen.visible[working].contains('$'),
        "{}",
        screen.visible_text()
    );
    harness.wait_visible("esc to interrupt");
    let later = harness.wait_visible(r"… \([1-9]\d*s ");
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
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    harness.submit("keep a fallback");
    let screen = harness.wait_visible("^❯ keep a fallback");
    let queued_row = expect_row(&screen, "^❯ keep a fallback");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        queued_row < rule,
        "queued above the input:\n{}",
        screen.visible_text()
    );
    harness.press(Key::Up);
    let take_back = harness.wait_command("take_back");
    assert_eq!(take_back["run"], RUN);
    assert_eq!(take_back["messages"], json!(["m-001"]));
    let screen = harness.wait_until("the text back in the input", |screen| {
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
    // Shift+Tab's cycle: supervised, auto, full auto.
    let script = Script::idle()
        .await_command("approval_mode")
        .at(0, status_with(json!({"approval_mode": "auto"})))
        .await_command("approval_mode")
        .at(0, status_with(json!({"approval_mode": "full_auto"})))
        .await_command("approval_mode")
        .at(0, status_with(json!({"approval_mode": "supervised"})));
    let mut harness = Harness::start(script);
    harness.wait_visible(MODE_IDLE);
    harness.press(Key::ShiftTab);
    assert_eq!(harness.wait_command("approval_mode")["mode"], "auto");
    harness.wait_visible("^  ⏵⏵ auto mode on \\(shift\\+tab to cycle\\)");
    harness.press(Key::ShiftTab);
    assert_eq!(harness.wait_command("approval_mode")["mode"], "full_auto");
    harness.wait_visible("^  ⏵⏵ full auto mode on \\(shift\\+tab to cycle\\)");
    harness.press(Key::ShiftTab);
    assert_eq!(harness.wait_command("approval_mode")["mode"], "supervised");
    harness.wait_visible(MODE_IDLE);
}

#[test]
fn status_line_changed_agent_quota_spend() {
    let status = status_with(json!({
        "default_agent": "claude-code/opus",
        "quotas": [chip("Claude", Some(quota("claude-code", 0.71)), "healthy"),
                   chip("Codex", Some(quota("codex", 0.38)), "healthy"),
                   chip("agy", None, "healthy")],
        "spent": spend(9000, 0.21)}));
    let harness = Harness::launch(Script::idle().at(0, status))
        .size(ROWS, 200)
        .start();
    // The header names haiku and the preset, so only the agent that differs comes back here,
    // at the mode line's right end.
    let screen = harness.wait_visible(
        r"^  ǁ supervised mode on .*  Opus on Claude Code · Claude 5h 71% used · Codex 5h 38% used · \$0\.21$",
    );
    let status = expect_row(&screen, r"Opus on Claude Code · Claude");
    assert_eq!(
        status + 1 + blank_rows_at_bottom(&screen),
        screen.visible.len()
    );
    let rule = expect_row(&screen, TOP_RULE);
    assert!(screen.visible[rule].ends_with(" peer-crew ─"));
}

#[test]
fn status_line_quota_chips() {
    let mut out = quota("claude-code", 1.0);
    out["status"] = json!("rejected");
    // Today, in the harness's UTC clock, so the chip reads a time without a weekday.
    let today = time::OffsetDateTime::now_utc().date();
    out["resets_at"] = json!(format!("{today}T15:45:00Z"));
    let status = status_with(json!({
        "quotas": [chip("Claude", Some(out), "healthy"),
                   chip("Codex", Some(quota("codex", 0.38)), "down"),
                   chip("agy", None, "healthy")]}));
    let harness = Harness::launch(Script::idle().at(0, status))
        .size(ROWS, 200)
        .start();
    let screen = harness.wait_visible("Claude out until 3:45 PM");
    let row = &screen.visible[expect_row(&screen, "Claude out until")];
    assert!(row.contains("Codex down"), "{row}");
    // agy's quota isn't known yet, so it has no chip.
    assert!(!row.contains("agy"), "{row}");
    assert!(
        !row.contains("38%"),
        "a down vendor shows down, not its reading: {row}"
    );

    let fine = status_with(json!({
        "quotas": [chip("Claude", Some(quota("claude-code", 0.92)), "healthy"),
                   chip("Codex", Some(quota("codex", 0.38)), "healthy")]}));
    let harness = Harness::launch(Script::idle().at(0, fine))
        .size(ROWS, 200)
        .start();
    let screen = harness.wait_visible("Claude 5h 92% used");
    let row = &screen.visible[expect_row(&screen, "Claude 5h 92% used")];
    assert!(
        row.contains("Codex 5h 38% used"),
        "every chip says used: {row}"
    );
}

#[test]
fn status_line_shows_narrator_spend() {
    let script = Script::idle()
        .at(0, status_with(json!({"narrator": "live"})))
        .at(0, narrator_status(0.06, "$0.50"));
    let harness = Harness::launch(script)
        .ui("narrator = \"live\"")
        .size(ROWS, 200)
        .start();
    let screen = harness.wait_visible("narrator \\$0\\.06/\\$0\\.50");
    let row = expect_row(&screen, "narrator \\$0\\.06/\\$0\\.50");
    assert!(
        screen.visible[row].contains("Claude 5h 71% used")
            && screen.visible[row].contains("ǁ supervised mode on"),
        "on the mode line"
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
    let harness = Harness::launch(script).ui("narrator = \"live\"").start();
    let screen = harness.wait_visible("※ narrator: Two agents");
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
        .await_command("pause")
        .at(0, mode_pausing(RUN))
        .at(0, run.paused(2))
        .at(0, run.parked("paused"))
        .at(0, mode_parked(RUN, "paused"));
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    harness.press(Key::Esc);
    assert_eq!(harness.wait_command("pause")["now"], false);
    let screen = harness.wait_visible("(?i)paused");
    let record = expect_row(&screen, "(?i)paused");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        record < rule,
        "the record goes into the chat:\n{}",
        screen.visible_text()
    );
    let chat = screen.scrollback.iter().chain(&screen.visible[..rule]);
    let records = chat
        .filter(|row| row.to_lowercase().contains("paused"))
        .count();
    assert_eq!(records, 1, "one record line:\n{}", screen.text());
}

#[test]
fn verbose_twice_returns_to_normal() {
    // A chat turn's calls always fold into one line, as Claude Code folds them, so the
    // levels show on an agent's calls in a run.
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
        run.card_created(&agent),
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
    let mut harness = Harness::launch(script).ui("verbosity = \"normal\"").start();
    harness.wait_visible("esc to interrupt");
    harness.send("/verbosity full");
    harness.press(Key::Enter);
    let full = harness.wait_visible("Both read at full\\.");
    assert!(
        row_of(&full, "Read\\(src/a\\.rs\\)").is_some()
            && row_of(&full, "Read\\(src/b\\.rs\\)").is_some(),
        "full shows every call:\n{}",
        full.visible_text()
    );
    harness.send("/verbosity normal");
    harness.press(Key::Enter);
    let normal = harness.wait_visible("Both read at normal\\.");
    let before = last_row_of(&normal, "Both read at full\\.").unwrap_or(0);
    assert!(
        normal.visible[before..]
            .iter()
            .any(|row| row.contains("Read 2 files")),
        "normal folds the calls into one line:\n{}",
        normal.visible_text()
    );
    let levels: Vec<String> = harness
        .command_values()
        .iter()
        .filter(|c| command_type(c) == "verbosity")
        .map(|c| c["level"].as_str().unwrap_or_default().to_owned())
        .collect();
    let slashes = harness
        .command_values()
        .iter()
        .filter(|c| command_type(c) == "slash" && c["command"]["command"] == "verbose")
        .count();
    assert!(
        levels == ["full", "normal"] || slashes == 2,
        "the screen tells the app each change: {:?}",
        harness.command_values()
    );
}

#[test]
fn always_on_agent_warning_once() {
    let warning = "openclaw is always on · approvals refuse its shell · see argonauts doctor";
    let script = Script::idle()
        .at(0, notice("warn", warning))
        .await_command("submit")
        .at(0, notice("warn", warning))
        .at(0, status_line());
    let mut harness = Harness::start(script);
    harness.wait_for("openclaw is always on");
    harness.submit("hello");
    harness.wait_for("❯ hello");
    let screen = harness.screen();
    assert_eq!(
        count_rows(&screen, "openclaw is always on"),
        1,
        "{}",
        screen.text()
    );
}

/// 300 events a second for two seconds at 120 columns, spread over three agents. A frame is
/// dropped when the screen stays the same for longer than `STALL` while events keep coming.
#[test]
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
    let harness = Harness::launch(script)
        .size(40, 120)
        .ui("[panel]\nagents = true")
        .start();
    harness.wait_visible("esc to interrupt");
    let frames_before = sync_updates(&harness.raw());
    harness.signal("go");
    let started = Instant::now();
    let last_event = regex::Regex::new(&last).unwrap();
    let mut previous = harness.visible();
    let mut changed_at = Instant::now();
    let mut dropped = 0;
    let mut longest = Duration::ZERO;
    loop {
        let now = Instant::now();
        let current = harness.visible();
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
    let frames = sync_updates(&harness.raw()) - frames_before;
    eprintln!(
        "{EVENTS} events in {elapsed:?}: {frames} frames, {dropped} dropped, longest gap {longest:?}"
    );
    assert_eq!(dropped, 0, "longest gap {longest:?}");
    assert!(frames > 0, "each frame is one synchronized update");
    let cap = (elapsed.as_secs_f64() * 120.0).ceil() as usize + 2;
    assert!(
        frames <= cap,
        "{frames} frames in {elapsed:?}, over 120 a second"
    );
    assert_frames(&harness.screen(), 120);
}

/// Frames drawn as synchronized updates (`CSI ? 2026 h`).
fn sync_updates(raw: &[u8]) -> usize {
    raw.windows(8).filter(|w| w == b"\x1b[?2026h").count()
}
