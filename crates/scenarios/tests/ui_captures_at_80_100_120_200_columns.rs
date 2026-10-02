//! The four states a session shows, a chat answer, a run at work, a checkpoint, and a
//! finished run, captured at 80, 100, 120, and 200 columns on dark and light themes and with
//! `NO_COLOR`, for the side-by-side review with Claude Code's screens.

use std::ffi::OsString;
use std::fs;

use argonauts_scenarios::{ROWS, Scenario, WAIT};
use argonauts_testkit::{Key, Pty, PtyCommand, Screen, argonauts_bin};
use regex::Regex;

const WIDTHS: [u16; 4] = [80, 100, 120, 200];
const FIX: &str = "Fix the install command in README.md: it should be npm install @acme/api";
const README: &str = "# acme-api\n\nInstall it with:\n\n    npm install acme-api\n";

const MODE_LINE: &str = r"(?m)^  ǁ supervised mode on";

struct Look {
    name: &'static str,
    theme: &'static str,
    no_color: bool,
}

const LOOKS: [Look; 3] = [
    Look {
        name: "dark",
        theme: "dark",
        no_color: false,
    },
    Look {
        name: "light",
        theme: "light",
        no_color: false,
    },
    Look {
        name: "no-color",
        theme: "dark",
        no_color: true,
    },
];

fn launch(scenario: &Scenario, look: &Look, cols: u16) -> anyhow::Result<Pty> {
    let config = scenario.repo.global_config();
    if let Some(dir) = config.parent() {
        fs::create_dir_all(dir)?;
    }
    // Merging is automatic by default, and the checkpoint state needs the merge to wait.
    fs::write(
        &config,
        format!(
            "[ui]\ntheme = \"{}\"\n\n[approvals.merge]\nmode = \"supervised\"\n",
            look.theme
        ),
    )?;
    let mut envs: Vec<(String, OsString)> = scenario.envs();
    if look.no_color {
        envs.push(("NO_COLOR".to_owned(), "1".into()));
    }
    let mut command = PtyCommand::new(argonauts_bin()?)
        .cwd(scenario.repo.root())
        .envs(envs)
        .env_remove("ARGONAUTS_SESSION")
        .size(ROWS, cols);
    if !look.no_color {
        command = command.env_remove("NO_COLOR");
    }
    Pty::spawn(command)
}

fn count(screen: &Screen, pattern: &str) -> usize {
    let regex = Regex::new(pattern).expect("a valid pattern");
    screen
        .scrollback
        .iter()
        .chain(&screen.visible)
        .filter(|row| regex.is_match(row))
        .count()
}

/// The live region fits the terminal, and the scrollback holds each item once.
fn check(screen: &Screen, cols: u16, state: &str) {
    for row in &screen.visible {
        let cells = row.chars().count();
        assert!(
            cells <= usize::from(cols),
            "{state} at {cols}: {row:?} is too wide"
        );
    }
    assert_eq!(
        count(screen, r"^  ǁ supervised mode on"),
        1,
        "{state} at {cols}:\n{}",
        screen.text()
    );
    assert_eq!(
        count(screen, r"^─+ \S+ ─$"),
        1,
        "{state} at {cols}:\n{}",
        screen.text()
    );
    assert!(
        count(screen, r"^❯ hi$") <= 1,
        "{state} at {cols}:\n{}",
        screen.text()
    );
}

fn capture(scenario: &Scenario, look: &Look, cols: u16) -> anyhow::Result<()> {
    let shot =
        |pty: &Pty, state: &str| scenario.shot(pty, &format!("{}-{cols}-{state}", look.name));
    let mut pty = launch(scenario, look, cols)?;
    pty.wait_for(MODE_LINE, WAIT)?;

    pty.send("hi")?;
    pty.press(Key::Enter)?;
    pty.wait_for(r"(?m)^✻ \w+ for \d", WAIT)?;
    let screen = pty.wait_for(MODE_LINE, WAIT)?;
    check(&screen, cols, "chat answer");
    shot(&pty, "chat-answer")?;

    pty.send(FIX)?;
    pty.press(Key::Enter)?;
    let screen = pty.wait_for(r"(?m)^[∙✢✲✶✻✼*] [\w'-]+… \(", WAIT)?;
    pty.wait_for("esc to interrupt", WAIT)?;
    check(&screen, cols, "busy");
    shot(&pty, "busy")?;

    let screen = pty.wait_for(r"(?m)^◆ Merge", WAIT)?;
    check(&screen, cols, "checkpoint");
    shot(&pty, "checkpoint")?;

    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for("Enter to merge", WAIT)?;
    pty.press(Key::Enter)?;
    pty.wait_for(r"(?m)^[●⏺] Merged", WAIT)?;
    let screen = pty.wait_for(MODE_LINE, WAIT)?;
    check(&screen, cols, "finished");
    shot(&pty, "finished")?;

    pty.press(Key::Ctrl('c'))?;
    pty.press(Key::Ctrl('c'))?;
    pty.wait_for_exit(WAIT)?;
    Ok(())
}

#[test]
fn ui_captures_at_80_100_120_200_columns() -> anyhow::Result<()> {
    for look in &LOOKS {
        for cols in WIDTHS {
            let scenario = Scenario::new("ui/four_states.toml")?;
            scenario.repo.write("README.md", README)?;
            scenario.repo.commit_all("add the readme")?;
            capture(&scenario, look, cols)?;
        }
    }
    Ok(())
}
