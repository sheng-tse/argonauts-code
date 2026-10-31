//! Accessibility: the screen-reader mode's flat lines, `NO_COLOR`, and reduced motion.

use crate::pty_support;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use pty_support::*;
use serde_json::json;

#[test]
fn screen_reader_flat_lines() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .await_command("submit")
        .timed(conversation_turn("t-001", "hi", "Hello there.", 5))
        .all(0, events)
        .at(
            0,
            run.tool_finished(&crew[0], "s-001-edit", true, "Updated src/export.rs"),
        )
        .at(0, run.merge_approval("approval-001"));
    let mut driver = TestDriver::launch(script)
        .ui("screen_reader = true")
        .start();
    driver.wait_for("(?m)^.*Argonauts Code");
    driver.submit("hi");
    driver.wait_for("(?m)^you: hi$");
    driver.wait_for("(?m)^claude-code(/haiku)?: Hello there\\.");
    driver.wait_for("(?m)^claude-code(/opus)?, worker, task-002: ");
    driver.wait_for("(?m)^tool: ");
    let text = driver.wait_for("(?m)^\\s*1\\. Merge").text();
    driver.wait_for("(?m)^\\s*2\\. Open a pull request");
    for glyph in ['╭', '│', '╰', '─', '⎿'] {
        assert!(
            !text.contains(glyph),
            "no {glyph} in screen-reader mode:\n{text}"
        );
    }
    let drawn = plain(&driver.raw());
    let spinner = regex::Regex::new(r"[∙✢✲✶✻✼] [\w'-]+…").unwrap();
    assert!(
        !spinner.is_match(&drawn),
        "no spinner is ever drawn:\n{drawn}"
    );
    driver.wait_until("the bell for the task", |_| driver.bells() > 0);
}

/// Screen-reader mode reads Ultracode with the main agent's effort and flips it when you type
/// `a`, since a typed line never carries Tab.
#[test]
fn screen_reader_flips_ultracode_by_typing_a() {
    use argonauts_core::app::model_picker::EffortSlider;
    use argonauts_core::app::{AppEvent, ViewData};

    let slider =
        EffortSlider::without_levels(None, "claude-code/haiku".parse().unwrap(), "Haiku 4.5");
    let view = AppEvent::View {
        data: Box::new(ViewData::Effort {
            slider: slider.with_ultracode(false),
        }),
    };
    let script = Script::idle().await_command("slash").at(0, view);
    let mut driver = TestDriver::launch(script)
        .ui("screen_reader = true")
        .start();
    driver.wait_for("(?m)^.*Argonauts Code");
    driver.send("/effort");
    driver.press(Key::Enter);
    driver.wait_for("(?m)^effort for Haiku 4.5: none, it takes no effort setting$");
    driver.wait_for("(?m)^ultracode: off$");
    let text = driver
        .wait_for("(?m)^ultracode: type a to turn it on\\.$")
        .text();
    assert!(!text.to_lowercase().contains("tab to toggle"), "{text}");
    driver.send("a");
    driver.press(Key::Enter);
    let flipped = driver.wait_command_where("slash", |value| {
        value["command"]["ultracode"] == json!(true)
    });
    assert_eq!(flipped["command"]["command"], "effort", "{flipped}");
    assert!(flipped["command"]["level"].is_null(), "{flipped}");
}

#[test]
fn no_color_env() {
    let script = Script::idle().await_command("submit").all(
        0,
        [
            mode_answering("t-001"),
            turn_started("t-001", "show it"),
            conversation_tool_started("t-001", "c1", "Bash", json!({"command": "cat missing.txt"})),
            conversation_agent(
                "t-001",
                json!({"type": "tool_finished", "id": "c1", "ok": false,
                       "output": "cat: missing.txt: No such file or directory", "parent": null}),
            ),
            turn_finished("t-001", "That file doesn't exist.", 1500),
            mode_idle(),
        ],
    );
    let mut driver = TestDriver::launch(script).env("NO_COLOR", "1").start();
    driver.wait_visible(MODE_IDLE);
    driver.submit("show it");
    let screen = driver.wait_visible("That file doesn't exist\\.");
    // At the default verbosity the call shows whole, and its header says it failed in words.
    let call = expect_row(&screen, "Bash\\(cat missing\\.txt\\)");
    assert!(
        screen.visible[call].contains("failed"),
        "without color a failed call says so:\n{}",
        screen.visible_text()
    );
    assert!(!sets_color(&driver.raw()), "NO_COLOR sets no color");
}

/// The working line's glyphs seen over `window`, or until `enough` holds for them.
fn glyphs_until(
    driver: &TestDriver,
    window: Duration,
    enough: impl Fn(&BTreeSet<char>) -> bool,
) -> BTreeSet<char> {
    let started = Instant::now();
    let mut seen = BTreeSet::new();
    while started.elapsed() < window && !enough(&seen) {
        let screen = driver.screen();
        if let Some(row) = row_of(&screen, WORKING_LINE) {
            seen.extend(screen.visible[row].chars().next());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    seen
}

fn working_turn(driver: &mut TestDriver) {
    driver.wait_visible(MODE_IDLE);
    driver.submit("check the build");
    driver.wait_visible(WORKING_LINE);
}

fn thinking_script() -> Script {
    Script::idle().await_command("submit").all(
        0,
        [
            mode_answering("t-001"),
            turn_started("t-001", "check the build"),
            conversation_tool_started("t-001", "c1", "Bash", json!({"command": "cargo build"})),
        ],
    )
}

#[test]
fn reduced_motion_static_spinner() {
    let mut moving = TestDriver::start(thinking_script());
    working_turn(&mut moving);
    // A stalled machine only makes the next glyph come later, so this looks until it does.
    let normal = glyphs_until(&moving, WAIT, |seen| seen.len() > 1);
    assert!(
        normal.len() > 1,
        "the glyph animates by default: {normal:?}"
    );
    drop(moving);

    let mut still = TestDriver::launch(thinking_script())
        .ui("motion = \"reduced\"")
        .start();
    working_turn(&mut still);
    let reduced = glyphs_until(&still, Duration::from_millis(1500), |_| false);
    assert_eq!(reduced.len(), 1, "one glyph, held still: {reduced:?}");
}

/// An answer that stops mid line until the test signals, then ends.
fn half_line_script() -> Script {
    Script::idle()
        .await_command("submit")
        .all(
            0,
            [
                mode_answering("t-001"),
                turn_started("t-001", "explain"),
                text_delta("t-001", "First line.\nSecond li"),
            ],
        )
        .await_signal("rest")
        .all(
            0,
            [
                text_delta("t-001", "ne.\n"),
                turn_finished("t-001", "First line.\nSecond line.\n", 1500),
                mode_idle(),
            ],
        )
}

#[test]
fn reduced_motion_streams_whole_lines() {
    let mut moving = TestDriver::start(half_line_script());
    moving.wait_visible(MODE_IDLE);
    moving.submit("explain");
    moving.wait_visible("Second li");
    drop(moving);

    let mut still = TestDriver::launch(half_line_script())
        .ui("motion = \"reduced\"")
        .start();
    still.wait_visible(MODE_IDLE);
    still.submit("explain");
    // The first line arrives in the same delta as the half line, so once it shows, the half
    // line would have too.
    still.wait_visible("First line\\.");
    let early = still.poll_visible("Second li", Duration::from_millis(500));
    assert!(
        early.is_none(),
        "a half line stays hidden under reduced motion:\n{}",
        still.visible()
    );
    assert!(!plain(&still.raw()).contains("Second li"));
    still.signal("rest");
    still.wait_for("Second line\\.");
}
