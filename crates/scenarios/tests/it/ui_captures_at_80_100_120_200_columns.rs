//! The four states a session shows, a conversation answer, a run at work, an approval, and a
//! finished run, captured at 80, 100, 120, and 200 columns on dark and light themes and with
//! `NO_COLOR`, for the side-by-side review with Claude Code's screens.

use std::ffi::OsString;
use std::fs;

use argonauts_scenarios::{
    ROWS, Scenario, WAIT, as_set_up, on_the_default_workflow, past_the_question_guard,
};
use argonauts_testkit::{Key, Pty, PtyCommand, Screen, argonauts_bin};
use regex::Regex;

const WIDTHS: [u16; 4] = [80, 100, 120, 200];
const FIX: &str = "Fix the install command in README.md: it should be npm install @acme/api";
const README: &str = "# acme-api\n\nInstall it with:\n\n    npm install acme-api\n";

/// The mode by its sign and first word, which stay when a narrow terminal takes the rest of
/// its words for the CLIs' plan usage.
const BOTTOM_ROW: &str = r"(?m)^  ǁ supervised";

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
    // Merging is automatic by default, and the approval state needs the merge to wait. The
    // request ends with the verdict, which a CLI that takes no tools keeps. The workflow is
    // only the default, so the rules leave the small request to the main agent.
    fs::write(
        &config,
        as_set_up(&on_the_default_workflow(&format!(
            "[ui]\ntheme = \"{}\"\n\n[approvals.merge]\nmode = \"supervised\"\n\n\
             [main_session]\ntools = false\n",
            look.theme
        ))),
    )?;
    let mut envs: Vec<(String, OsString)> = scenario.envs();
    if look.no_color {
        envs.push(("NO_COLOR".to_owned(), "1".into()));
    }
    let mut command = PtyCommand::new(argonauts_bin()?)
        .args(argonauts_scenarios::no_servers_of_yours(&[]))
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

/// The live region fits the terminal, and the scrollback holds each item once. A question that
/// holds the keys stands in for the input, so its rules are gone then.
fn check(screen: &Screen, cols: u16, state: &str) {
    check_with(screen, cols, state, true);
}

fn check_with(screen: &Screen, cols: u16, state: &str, input: bool) {
    for row in &screen.visible {
        let cells = row.chars().count();
        assert!(
            cells <= usize::from(cols),
            "{state} at {cols}: {row:?} is too wide"
        );
    }
    assert_eq!(
        count(screen, r"^  ǁ supervised"),
        1,
        "{state} at {cols}:\n{}",
        screen.text()
    );
    assert_eq!(
        // The rule's label is the workflow and the model, so it has spaces.
        count(screen, r"^─+ \S.* ─$"),
        usize::from(input),
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
    pty.wait_for(BOTTOM_ROW, WAIT)?;

    pty.send("hi")?;
    pty.press(Key::Enter)?;
    pty.wait_for(r"(?m)^✻ \w+ for \d", WAIT)?;
    let screen = pty.wait_for(BOTTOM_ROW, WAIT)?;
    check(&screen, cols, "conversation answer");
    shot(&pty, "conversation-answer")?;

    pty.send(FIX)?;
    pty.press(Key::Enter)?;
    // The working line says the run is going. The mode's `esc to interrupt` hint is the
    // first thing a narrow terminal takes for the chips, so it's no sign of that here.
    pty.wait_for_visible(r"^[∙✢✲✶✻✼*] [\w'-]+… \(", WAIT)?;
    // Esc leaves a run's agents going, so the bottom line counts them and makes
    // no offer to interrupt.
    let screen = pty.wait_for_visible(r"←  \d+ agents?", WAIT)?;
    check(&screen, cols, "busy");
    shot(&pty, "busy")?;
    scenario.clis.signal("busy-captured")?;

    pty.wait_for(r"(?m)^◆ Merge", WAIT)?;
    // The merge question needs you, so it takes the keys as it shows.
    let screen = pty.wait_for(r"(?m)^  Enter to \w+.* · Esc back to the list", WAIT)?;
    check_with(&screen, cols, "approval", false);
    shot(&pty, "approval")?;

    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for("Enter to merge", WAIT)?;
    pty.press(Key::Enter)?;
    pty.wait_for(r"(?m)^[●⏺] Merged", WAIT)?;
    let screen = pty.wait_for(BOTTOM_ROW, WAIT)?;
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
