//! Direct solo through the real binary with fake vendors, at 100 and 120 columns: in the solo
//! preset your message goes to Claude Code in your checkout, its own stream shows as Claude
//! Code shows it, and the turn ends with its last message and the `✻` line. No Triage item,
//! card, report, merge, or recap. The screens are captured for the side-by-side review with
//! Claude Code's own (`ref/claude-code/25-edit-done.txt`).

use std::fs;

use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::{Key, Pty, Screen};
use regex::Regex;

const TASK: &str = "In notes.txt, change draft to final, then run echo hi > shell.txt";

const MODE_LINE: &str = r"(?m)^  ⏵⏵ auto mode on";
const TURN_END: &str = r"(?m)^✻ \w+ for \d";

fn solo_preset(scenario: &Scenario) -> anyhow::Result<()> {
    let config = scenario.repo.global_config();
    if let Some(dir) = config.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        config,
        "main_agent = \"claude-code/haiku\"\n\n[ui]\ntheme = \"dark\"\n\n\
         [solo_or_crew]\nforce = \"solo\"\n",
    )?;
    Ok(())
}

fn rows(screen: &Screen) -> Vec<String> {
    screen
        .scrollback
        .iter()
        .chain(&screen.visible)
        .cloned()
        .collect()
}

fn count(screen: &Screen, pattern: &str) -> usize {
    let regex = Regex::new(pattern).expect("a valid pattern");
    rows(screen)
        .iter()
        .filter(|row| regex.is_match(row))
        .count()
}

/// The screen once the working line has gone, which it does a moment after the `✻` line.
fn settled(pty: &Pty) -> anyhow::Result<Screen> {
    let working = Regex::new(r"^\S \S+…").expect("a valid pattern");
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
fn ui_direct_solo_turn_looks_like_claude_code() -> anyhow::Result<()> {
    for cols in [100, 120] {
        let scenario = Scenario::new("ui/direct_solo.toml")?;
        solo_preset(&scenario)?;
        scenario.repo.write("notes.txt", "This is a draft.\n")?;
        scenario.repo.commit_all("add the notes")?;
        let mut pty = scenario.launch_sized(&[], 40, cols)?;
        pty.wait_for(MODE_LINE, WAIT)?;

        pty.send(TASK)?;
        pty.press(Key::Enter)?;
        pty.wait_for(TURN_END, WAIT)?;
        let done = settled(&pty)?;
        scenario.shot(&pty, &format!("direct-solo-{cols}"))?;
        let text = done.text();

        assert_eq!(
            count(&done, &format!("^❯ {}", regex::escape(TASK))),
            1,
            "{text}"
        );
        // The recording names the file under its sanitized folder.
        assert_eq!(
            count(&done, r"^[●⏺] Update\((<workdir>/)?notes\.txt\)"),
            1,
            "{text}"
        );
        assert_eq!(
            count(&done, r"^[●⏺] Bash\(echo hi > shell\.txt\)"),
            1,
            "{text}"
        );
        assert_eq!(count(&done, r"^[●⏺] done$"), 1, "{text}");
        for run_only in [r"Triage\(", "task-001", r"^◆ ", r"Merged", "worker"] {
            assert_eq!(count(&done, run_only), 0, "{run_only}:\n{text}");
        }
        // Nothing after the agent's last message but the `✻` line.
        let all = rows(&done);
        let answer = all
            .iter()
            .rposition(|row| row.starts_with("● done") || row.starts_with("⏺ done"))
            .expect("the agent's answer");
        let after: Vec<&String> = all[answer + 1..]
            .iter()
            .take_while(|row| !row.starts_with('─'))
            .filter(|row| !row.trim().is_empty())
            .collect();
        assert!(
            after.len() == 1 && after[0].starts_with('✻'),
            "after the answer: {after:?}\n{text}"
        );
        let notes = fs::read_to_string(scenario.repo.root().join("notes.txt"))?;
        assert_eq!(notes, "This is a final.\n", "the edit is in your checkout");
        exit(&mut pty)?;
    }
    Ok(())
}
