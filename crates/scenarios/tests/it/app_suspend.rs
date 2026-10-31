//! Ctrl+Z stops the app and gives its terminal to the shell that started it, and `fg` brings
//! the full screen back with its pictures. Started by a parent that waits on it, as npm's
//! launcher starts it, the app is only one process of the shell's job, and the shell still gets
//! its prompt back.

use argonauts_scenarios::{WAIT, no_servers_of_yours};
use argonauts_testkit::{Key, Pty, PtyCommand, argonauts_bin};

use crate::app_support::*;

/// The sizes the screen is held to, as `cols × rows`.
const SIZES: [(u16, u16); 2] = [(120, 38), (177, 40)];

/// What the shell that keeps jobs prompts with.
const PROMPT: &str = "jobs$";

/// The line the app leaves on the shell's screen while it is stopped.
const SUSPENDED: &str = "Argonauts Code is suspended. Run fg to bring it back.";

/// The bottom line's hint, which the app's own screen always shows.
const THE_APP_SHOWS: &str = "shift+tab to cycle";

/// What the shell types to start the app, its path and arguments passed in its environment.
const STARTS_ALONE: &str = "\"$APP\" $APP_ARGS\r";

/// The same under a parent that waits on it, so the shell's job is the parent's process group
/// and the app isn't its first process.
const STARTS_UNDER_A_PARENT: &str = "sh -c '\"$APP\" $APP_ARGS; :'\r";

/// The app started from an interactive shell that keeps jobs, on a terminal that takes
/// pictures, by `starts`.
fn in_a_job_shell(
    scenario: &AppScenario,
    starts: &str,
    cols: u16,
    rows: u16,
) -> anyhow::Result<Pty> {
    let args = no_servers_of_yours(&["--set", "ui.screen=fullscreen"]).join(" ");
    let mut pty = Pty::spawn(
        PtyCommand::new("/bin/sh")
            .args(["-i"])
            .cwd(scenario.repo.root())
            .envs(scenario.envs())
            .env_remove("ARGONAUTS_SESSION")
            .env_remove("ENV")
            .env("PS1", format!("{PROMPT} "))
            .env("APP", argonauts_bin()?)
            .env("APP_ARGS", args)
            .env("TERM_PROGRAM", "ghostty")
            .takes_pictures()
            .size(rows, cols),
    )?;
    pty.wait_for_visible(&regex::escape(PROMPT), WAIT)?;
    pty.send(starts)?;
    Ok(pty)
}

/// Waits for the shell's prompt under the line the app leaves, with the full screen gone.
fn the_shell_has_the_terminal(pty: &Pty, size: &str) -> anyhow::Result<()> {
    let stopped = pty.wait_until(
        "the shell's prompt under the suspended line",
        WAIT,
        |screen| {
            let rows: Vec<&str> = screen
                .visible
                .iter()
                .map(|row| row.trim_end())
                .filter(|row| !row.is_empty())
                .collect();
            rows.last() == Some(&PROMPT) && rows.iter().any(|row| row.contains(SUSPENDED))
        },
    )?;
    assert!(
        !stopped.visible_text().contains(THE_APP_SHOWS),
        "{size}: the full screen is gone while the app is stopped:\n{}",
        stopped.visible_text()
    );
    Ok(())
}

fn stop_and_come_back(starts: &str) -> anyhow::Result<()> {
    for (cols, rows) in SIZES {
        let size = format!("{cols}x{rows}");
        let scenario = scenario()?;
        let mut pty = in_a_job_shell(&scenario, starts, cols, rows)?;
        wait_ready(&scenario, &pty)?;
        pty.wait_for_pictures(WAIT)?;

        pty.press(Key::Ctrl('z'))?;
        the_shell_has_the_terminal(&pty, &size)?;

        pty.send("fg\r")?;
        let back = pty.wait_for_visible("shift\\+tab to cycle", WAIT)?;
        assert!(
            !back.visible_text().contains(SUSPENDED),
            "{size}: the full screen is back over the shell:\n{}",
            back.visible_text()
        );
        pty.wait_for_pictures(WAIT)?;
    }
    Ok(())
}

#[test]
fn app_ctrl_z_gives_the_shell_its_prompt_and_fg_brings_the_screen_back() -> anyhow::Result<()> {
    stop_and_come_back(STARTS_ALONE)
}

#[test]
fn app_ctrl_z_under_a_waiting_parent_gives_the_shell_its_prompt_too() -> anyhow::Result<()> {
    stop_and_come_back(STARTS_UNDER_A_PARENT)
}
