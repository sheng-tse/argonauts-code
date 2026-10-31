//! The start's first frame: the header and the start's notices come with the input, while the
//! CLI checks are still out, and the header stays one header once they answer. The first start's
//! greeting and the start `/cd` opens come the same way.

use std::path::Path;

use crate::app_support;

use app_support::*;
use argonauts_core::app::ChoiceKind;
use argonauts_scenarios::{WAIT, as_set_up, launch_defaults, scenario_file};
use argonauts_testkit::screen::Grid;
use argonauts_testkit::{
    FakeRule, FakeScenario, FakeStep, LogLine, Pty, PtyCommand, argonauts_bin,
};

/// The size the frames are read at, as `cols × rows`.
const COLS: u16 = 120;
const ROWS: u16 = 38;

/// Where each of the app's frames ends: it draws every frame as one synchronized update.
const FRAME_END: &[u8] = b"\x1b[?2026l";

/// Where a screen starts: it turns bracketed paste on before its first frame.
const SCREEN_START: &[u8] = b"\x1b[?2004h";

/// Lets the held `claude --version` answer.
const VERSION_MAY_ANSWER: &str = "version-may-answer";

/// The greeting's own line, under the name in block letters.
const GREETING: &str = "Every coding agent, one orchestrator, you at the helm.";

/// The start notice a folder without git gets.
const NO_GIT: &str = "This folder has no git";

#[test]
fn app_header_and_notices_come_before_the_cli_checks() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let scenario = with_the_version_held(dir.path())?;
    std::fs::write(scenario.repo.global_config(), as_set_up(GLOBAL_CONFIG))?;
    scenario.repo.write(
        ".argonauts/config.local.toml",
        "# only mine, or so I thought\n",
    )?;
    scenario.repo.commit_all("track the local settings file")?;

    let pty = scenario.launch_sized(&[], ROWS, COLS)?;
    pty.wait_for("Git tracks .argonauts/config.local.toml", WAIT)?;
    let drawn = frames(&pty.raw(), COLS, ROWS);
    let first = drawn
        .iter()
        .find(|frame| has_input(frame))
        .expect("a frame drew the input");
    let text = text_of(first);
    assert!(
        text.contains("Argonauts Code") && text.contains("Git tracks .argonauts/config.local.toml"),
        "the first frame with the input has the header and the notice:\n{text}"
    );
    let screen = pty.screen().text();
    let header = screen.find("Argonauts Code").expect("the header is up");
    let notice = screen
        .find("Git tracks .argonauts/config.local.toml")
        .expect("the notice");
    assert!(
        header < notice,
        "the header sits above the notice:\n{screen}"
    );
    assert!(
        screen.contains("shift+tab to cycle"),
        "the input came with them:\n{screen}"
    );

    scenario.clis.signal(VERSION_MAY_ANSWER)?;
    wait_start_through(&scenario)?;
    let screen = pty.screen().text();
    assert_eq!(
        screen.matches("Argonauts Code").count(),
        1,
        "the checks change the header in place:\n{screen}"
    );
    Ok(())
}

#[test]
fn app_greeting_comes_with_the_first_frame() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let scenario = with_the_version_held(dir.path())?;
    let servers = no_local_servers()?;
    let args: Vec<&str> = servers.iter().map(String::as_str).collect();
    // The first start greets only in a terminal outside CI.
    let pty = Pty::spawn(
        PtyCommand::new(argonauts_bin()?)
            .args(launch_defaults(&args))
            .cwd(scenario.repo.root())
            .envs(scenario.envs())
            .env_remove("ARGONAUTS_SESSION")
            .env("CI", "false")
            .size(ROWS, COLS),
    )?;
    pty.wait_for(GREETING, WAIT)?;
    // The greeting is up while every CLI check is still out. They answer now: held, the setup
    // would open only once the start's check and the setup's own each wait out the app's
    // version timeout, 20 seconds that would take a test's whole wait.
    scenario.clis.signal(VERSION_MAY_ANSWER)?;
    wait_open_choice(&scenario, ChoiceKind::TermsNote)?;
    assert!(
        held_check_answered(&scenario)?,
        "the setup opened on the held check's answer, not on the app's version timeout"
    );
    let drawn = frames(&pty.raw(), COLS, ROWS);
    let first = drawn
        .iter()
        .find(|frame| !text_of(frame).trim().is_empty())
        .expect("a frame drew something");
    let text = text_of(first);
    assert!(
        text.contains(GREETING),
        "the first frame has the greeting:\n{text}"
    );
    for frame in &drawn {
        let text = text_of(frame);
        assert!(
            text.trim().is_empty() || text.contains(GREETING),
            "no frame draws the setup without the greeting:\n{text}"
        );
    }
    Ok(())
}

#[test]
fn app_cd_brings_the_header_and_notices_with_the_first_frame() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let scenario = with_the_version_held(dir.path())?;
    std::fs::write(scenario.repo.global_config(), as_set_up(GLOBAL_CONFIG))?;
    let mut pty = scenario.launch_sized(&[], ROWS, COLS)?;
    scenario.clis.signal(VERSION_MAY_ANSWER)?;
    wait_start_through(&scenario)?;
    let notes = dir.path().join("notes");
    std::fs::create_dir(&notes)?;

    let before = pty.raw().len();
    submit(&mut pty, &format!("/cd {}", notes.display()))?;
    pty.wait_for(NO_GIT, WAIT)?;
    let raw = pty.raw();
    let reopened = raw[before..]
        .windows(SCREEN_START.len())
        .rposition(|bytes| bytes == SCREEN_START)
        .map(|at| before + at)
        .expect("the app opened its screen again");
    // The reopened screen's frames alone, so nothing the first screen left is read as its.
    let drawn = frames(&raw[reopened..], COLS, ROWS);
    let first = drawn
        .iter()
        .find(|frame| has_input(frame))
        .expect("the reopened screen drew the input");
    let text = text_of(first);
    assert!(
        text.contains("Argonauts Code") && text.contains(NO_GIT),
        "the first frame with the input has the header and the notice:\n{text}"
    );
    Ok(())
}

/// The sample agents, with `claude --version` held until the test signals
/// [`VERSION_MAY_ANSWER`], so every check the start makes is still out.
fn with_the_version_held(dir: &Path) -> anyhow::Result<AppScenario> {
    let mut fakes = FakeScenario::load(&scenario_file(AGENTS))?;
    fakes.rules.insert(
        0,
        FakeRule::for_program("claude")
            .args(&["--version"])
            .step(FakeStep::AwaitSignal(VERSION_MAY_ANSWER.to_owned()))
            .step(FakeStep::Emit("2.1.284 (Claude Code)".to_owned())),
    );
    let held = dir.join("held-version.toml");
    fakes.save(&held)?;
    scenario_playing(&held)
}

/// Whether the first `claude --version` the app ran, the one [`with_the_version_held`] holds,
/// answered. The app stops a check at its version timeout, and a fake stopped that way logs no
/// exit.
fn held_check_answered(scenario: &AppScenario) -> anyhow::Result<bool> {
    let log = scenario.clis.log()?;
    let held = log.iter().find_map(|line| match line {
        LogLine::Invocation(ran) if ran.argv0 == "claude" && ran.args == ["--version"] => {
            Some(ran.pid)
        }
        _ => None,
    });
    Ok(held.is_some_and(|held| {
        log.iter()
            .any(|line| matches!(line, LogLine::Exit { pid, code: 0 } if *pid == held))
    }))
}

/// The screen after each frame the app drew from its start, as the terminal shows it once that
/// frame's update ends.
fn frames(raw: &[u8], cols: u16, rows: u16) -> Vec<Grid> {
    raw.windows(FRAME_END.len())
        .enumerate()
        .filter(|(_, bytes)| *bytes == FRAME_END)
        .map(|(at, _)| Grid::from_bytes(cols, rows, &raw[..at + FRAME_END.len()]))
        .collect()
}

/// Whether a frame draws the input: a row that starts with `❯` right under a rule.
fn has_input(grid: &Grid) -> bool {
    (1..grid.rows()).any(|row| {
        grid.text(row - 1, 0, grid.cols()).starts_with('─')
            && grid.text(row, 0, grid.cols()).starts_with('❯')
    })
}

fn text_of(grid: &Grid) -> String {
    (0..grid.rows())
        .map(|row| grid.text(row, 0, grid.cols()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// On the inline screen the header prints into the terminal's history, where the rows under it
/// can't move, so a CLI the checks find not ready still gets its row, under them.
#[test]
fn app_inline_header_keeps_a_cli_not_ready() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let scenario = with_the_version_held(dir.path())?;
    let config = format!(
        "{}\n[agents.antigravity-cli]\nenabled = false\n",
        GLOBAL_CONFIG.replace("[ui]\n", "[ui]\nscreen = \"inline\"\n")
    );
    std::fs::write(scenario.repo.global_config(), as_set_up(&config))?;

    let pty = scenario.launch(&[])?;
    pty.wait_for("shift\\+tab to cycle", WAIT)?;
    let screen = pty.screen().text();
    assert!(
        !screen.contains("turned off"),
        "the checks haven't answered yet:\n{screen}"
    );

    scenario.clis.signal(VERSION_MAY_ANSWER)?;
    wait_start_through(&scenario)?;
    let screen = pty.wait_for(": turned off", WAIT)?.text();
    assert_eq!(
        screen.matches("Argonauts Code").count(),
        1,
        "one header, its new row under the rows printed since:\n{screen}"
    );
    Ok(())
}
