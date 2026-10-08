//! Direct turns and run requests through the real binary with fake vendors, at 100 and 120
//! columns: a supervised direct turn asks before its edit on a card and goes on when you say
//! yes, a request that names who coordinates and who reviews says what was followed as its run
//! starts, and one that would loosen a safety setting says why it wasn't. The screens are
//! captured for review.

use std::fs;

use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::{Key, Pty, Screen};

const SCENARIO: &str = "ui/direct_and_requests.toml";
const MODE_LINE: &str = r"(?m)^  (⏵⏵|ǁ) \w+ mode on";
const TURN_END: &str = r"(?m)^✻ \w+ for \d";

fn configure(scenario: &Scenario, extra: &str) -> anyhow::Result<()> {
    let config = scenario.repo.global_config();
    if let Some(dir) = config.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        config,
        // These requests end with the verdict, which a CLI that takes no tools keeps.
        format!(
            "main_agent = \"claude-code/haiku\"\n\n[ui]\ntheme = \"dark\"\n\n\
             [main_session]\ntools = false\n\n{extra}"
        ),
    )?;
    Ok(())
}

/// Every row, scrollback first, with runs of spaces closed up, so text that wraps still reads
/// as one line.
fn flat(screen: &Screen) -> String {
    screen
        .text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The screen once the working line has gone, which it does a moment after the `✻` line.
fn settled(pty: &Pty) -> anyhow::Result<Screen> {
    let working = regex::Regex::new(r"^\S \S+…").expect("a valid pattern");
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let screen = pty.wait_for(MODE_LINE, WAIT)?;
        if !screen.visible.iter().any(|row| working.is_match(row)) {
            return Ok(screen);
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "the working line stayed:\n{}",
            screen.text()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn exit(pty: &mut Pty) -> anyhow::Result<()> {
    pty.press(Key::Ctrl('c'))?;
    pty.press(Key::Ctrl('c'))?;
    pty.wait_for_exit(WAIT)?;
    Ok(())
}

#[test]
fn ui_a_supervised_direct_turn_asks_before_its_edit() -> anyhow::Result<()> {
    for cols in [100, 120] {
        let scenario = Scenario::new(SCENARIO)?;
        configure(
            &scenario,
            "[solo_or_crew]\nforce = \"solo\"\n\n[approvals.goal]\nmode = \"supervised\"\n",
        )?;
        scenario.repo.write("notes.txt", "This is a draft.\n")?;
        scenario.repo.commit_all("add the notes")?;
        let notes = scenario.repo.root().join("notes.txt");
        let mut pty = scenario.launch_sized(&[], 40, cols)?;
        pty.wait_for(MODE_LINE, WAIT)?;
        pty.send("In notes.txt, change draft to final")?;
        pty.press(Key::Enter)?;
        let card = pty.wait_for(r"Make this edit to notes\.txt\?", WAIT)?;
        scenario.shot(&pty, &format!("direct-asks-{cols}"))?;
        let text = card.text();
        assert!(text.contains("- This is a draft."), "{text}");
        assert!(text.contains("+ This is a final."), "{text}");
        assert!(
            text.contains("Yes, and allow edits for the rest of this conversation"),
            "{text}"
        );
        // Nothing changes until you answer.
        assert_eq!(fs::read_to_string(&notes)?, "This is a draft.\n");
        // Yes is the one picked.
        pty.wait_for(r"Enter to choose", WAIT)?;
        pty.press(Key::Enter)?;
        pty.wait_for(TURN_END, WAIT)?;
        let done = settled(&pty)?;
        scenario.shot(&pty, &format!("direct-asks-done-{cols}"))?;
        let text = done.text();
        assert!(text.contains("◆ Yes to the edit to notes.txt"), "{text}");
        assert_eq!(
            fs::read_to_string(&notes)?,
            "This is a final.\n",
            "{}",
            done.text()
        );
        exit(&mut pty)?;
    }
    Ok(())
}

#[test]
fn ui_a_request_that_says_who_coordinates_and_who_reviews() -> anyhow::Result<()> {
    for cols in [100, 120] {
        let scenario = Scenario::new(SCENARIO)?;
        configure(&scenario, "")?;
        let mut pty = scenario.launch_sized(&[], 40, cols)?;
        pty.wait_for(MODE_LINE, WAIT)?;
        pty.send("Add due dates to tasks. Have codex coordinate, gemini review it, and use grok to build.")?;
        pty.press(Key::Enter)?;
        let screen = pty.wait_for(r"Not followed:", WAIT)?;
        scenario.shot(&pty, &format!("request-roles-{cols}"))?;
        let text = flat(&screen);
        assert!(
            text.contains("As you asked: Codex coordinates · Antigravity CLI reviews"),
            "{}",
            screen.text()
        );
        assert!(
            text.contains("Not followed: \"use grok to build\""),
            "{}",
            screen.text()
        );
        // A run's start prints no Triage row.
        assert!(!screen.text().contains("Triage("), "{}", screen.text());
        exit(&mut pty)?;
    }
    Ok(())
}

#[test]
fn ui_a_request_that_would_loosen_a_safety_setting() -> anyhow::Result<()> {
    for cols in [100, 120] {
        let scenario = Scenario::new(SCENARIO)?;
        configure(&scenario, "")?;
        let mut pty = scenario.launch_sized(&[], 40, cols)?;
        pty.wait_for(MODE_LINE, WAIT)?;
        pty.send(
            "Fix the install command in README.md, merge it without asking me and turn the \
             sandbox off.",
        )?;
        pty.press(Key::Enter)?;
        let screen = pty.wait_for(r"the sandbox is a safety setting", WAIT)?;
        scenario.shot(&pty, &format!("request-refused-{cols}"))?;
        let text = flat(&screen);
        assert!(
            text.contains(
                "Not followed: \"merge it without asking me\" · approvals.merge.mode is a safety \
                 setting, /approvals changes it"
            ),
            "{}",
            screen.text()
        );
        assert!(
            text.contains(
                "Not followed: \"turn the sandbox off\" · the sandbox is a safety setting"
            ),
            "{}",
            screen.text()
        );
        assert!(!text.contains("As you asked"), "{}", screen.text());
        exit(&mut pty)?;
    }
    Ok(())
}
