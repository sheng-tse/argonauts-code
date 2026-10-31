//! Direct turns and run requests through the real binary with fake agents, at 100 and 120
//! columns: a supervised direct turn asks before its edit on a question and goes on when you say
//! yes, a request that names who orchestrates and who reviews says what was followed as its run
//! starts, and one that would loosen a safety setting says why it wasn't. The screens are
//! captured for review.

use std::fs;

use argonauts_scenarios::{Scenario, WAIT, as_set_up, past_the_question_guard};
use argonauts_testkit::{Key, Pty, Screen};

const SCENARIO: &str = "ui/direct_and_requests.toml";
const BOTTOM_ROW: &str = r"(?m)^  (⏵⏵|ǁ) \w+ mode on";
const TURN_END: &str = r"(?m)^✻ \w+ for \d";

fn configure(scenario: &Scenario, extra: &str) -> anyhow::Result<()> {
    let config = scenario.repo.global_config();
    if let Some(dir) = config.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        config,
        // These requests end with the verdict, which a CLI that takes no tools keeps.
        as_set_up(&format!(
            "crew.main = \"claude-code/haiku\"\n\n[ui]\ntheme = \"dark\"\n\n\
             [main_session]\ntools = false\n\n{extra}"
        )),
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
    let deadline = argonauts_testkit::pty::deadline::after(WAIT);
    loop {
        let screen = pty.wait_for(BOTTOM_ROW, WAIT)?;
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
            "[captain_or_crew]\nforce = \"captain\"\n\n[approvals.understand]\nmode = \"supervised\"\n",
        )?;
        scenario.repo.write("notes.txt", "This is a draft.\n")?;
        scenario.repo.commit_all("add the notes")?;
        let notes = scenario.repo.root().join("notes.txt");
        let mut pty = scenario.launch_sized(&[], 40, cols)?;
        pty.wait_for(BOTTOM_ROW, WAIT)?;
        pty.send("In notes.txt, change draft to final")?;
        pty.press(Key::Enter)?;
        pty.wait_for(r"Make this edit to notes\.txt\?", WAIT)?;
        // The answers may show a frame after the title and the change, later on a busy machine.
        let question = pty.wait_for(r"Yes, and allow edits", WAIT)?;
        scenario.shot(&pty, &format!("direct-asks-{cols}"))?;
        let text = question.text();
        assert!(text.contains("Make this edit to notes.txt?"), "{text}");
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
        past_the_question_guard();
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
fn ui_a_request_that_says_who_orchestrates_and_who_reviews() -> anyhow::Result<()> {
    for cols in [100, 120] {
        let scenario = Scenario::new(SCENARIO)?;
        configure(&scenario, "")?;
        let mut pty = scenario.launch_sized(&[], 40, cols)?;
        pty.wait_for(BOTTOM_ROW, WAIT)?;
        pty.send("Add due dates to tasks. Have codex orchestrate, gemini review it, and use grok to build.")?;
        pty.press(Key::Enter)?;
        let screen = pty.wait_for(r"Not followed:", WAIT)?;
        scenario.shot(&pty, &format!("request-roles-{cols}"))?;
        let text = flat(&screen);
        // The crew note names the reviewers you asked for, so the line leaves them out.
        assert!(
            text.contains("As you asked: Codex orchestrates")
                && !text.contains("As you asked: Codex orchestrates · agy reviews"),
            "{}",
            screen.text()
        );
        assert!(
            screen.text().lines().any(|line| line.contains("reviewers")
                && line.contains("agy")
                && line.contains("as you asked")),
            "{}",
            screen.text()
        );
        assert!(
            text.contains("Not followed: \"use grok to build\""),
            "{}",
            screen.text()
        );
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
        pty.wait_for(BOTTOM_ROW, WAIT)?;
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
