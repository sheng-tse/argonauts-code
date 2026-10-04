//! Example 2 of the four, a small fix that runs solo, through the real binary with fake
//! vendors. Its screens are captured for the side-by-side review with Claude Code's.

use std::fs;
use std::path::Path;

use argonauts_scenarios::{Scenario, WAIT, scenario_file};
use argonauts_testkit::{FakeScenario, FakeStep, Key, Pty, Screen};
use regex::Regex;

const FIX: &str = "Fix the install command in README.md: it should be npm install @acme/api";
const README: &str = "# acme-api\n\nInstall it with:\n\n    npm install acme-api\n";

const TOP_RULE: &str = r"(?m)^─+ \S+ ─$";
const MODE_LINE: &str = r"(?m)^  ǁ supervised mode on";
const TURN_END: &str = r"(?m)^✻ \w+ for \d";

/// A global config means no guided setup; the scenario is about the run, not the first start.
/// The default agent names its model, as rows like `Haiku 4.5 on Claude Code · owner` show
/// it, and since merging is automatic by default, this run's merge is set to wait for you.
fn skip_setup(scenario: &Scenario) -> anyhow::Result<()> {
    let config = scenario.repo.global_config();
    if let Some(dir) = config.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        config,
        "default_agent = \"claude-code/haiku\"\n\n[ui]\ntheme = \"dark\"\n\n\
         [approvals.merge]\nmode = \"supervised\"\n",
    )?;
    Ok(())
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

fn row(screen: &Screen, pattern: &str) -> Option<usize> {
    let regex = Regex::new(pattern).expect("a valid pattern");
    screen.visible.iter().position(|row| regex.is_match(row))
}

#[test]
fn ui_small_fix_looks_like_claude_code() -> anyhow::Result<()> {
    let scenario = Scenario::new("ui/small_fix.toml")?;
    skip_setup(&scenario)?;
    scenario.repo.write("README.md", README)?;
    scenario.repo.commit_all("add the readme")?;
    let mut pty = scenario.launch(&[])?;
    pty.wait_for(TOP_RULE, WAIT)?;
    pty.wait_for(MODE_LINE, WAIT)?;
    scenario.shot(&pty, "small-fix-start")?;

    pty.send(FIX)?;
    pty.press(Key::Enter)?;
    pty.wait_for(r"(?m)^[●⏺] Triage\(solo, checked\)", WAIT)?;
    pty.wait_for(r"(?m)^[●⏺] build:\S+ · Haiku 4\.5 on Claude Code", WAIT)?;
    scenario.shot(&pty, "small-fix-working")?;

    let card = pty.wait_for(r"(?m)^◆ Merge", WAIT)?;
    scenario.shot(&pty, "small-fix-checkpoint")?;
    let merge = row(&card, r"^◆ Merge").unwrap_or(0);
    let rule = row(&card, r"^─+ \S+ ─$").unwrap_or(0);
    assert!(
        merge < rule,
        "the card sits above the input:\n{}",
        card.visible_text()
    );
    pty.press(Key::Up)?;
    pty.wait_for(r"❯ 1\. Merge", WAIT)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for("Enter to merge", WAIT)?;
    pty.press(Key::Enter)?;

    pty.wait_for(r"(?m)^[●⏺] Merged", WAIT)?;
    pty.wait_for(TURN_END, WAIT)?;
    let done = pty.wait_for(MODE_LINE, WAIT)?;
    scenario.shot(&pty, "small-fix-done")?;

    assert_eq!(
        count(&done, &format!("^❯ {}", regex::escape(FIX))),
        1,
        "{}",
        done.text()
    );
    assert_eq!(
        count(&done, r"^  ǁ supervised mode on"),
        1,
        "one live region:\n{}",
        done.text()
    );
    assert_eq!(count(&done, r"^─+ \S+ ─$"), 1, "{}", done.text());
    // The spend sits at the right end of the mode line, with no status row of its own under
    // the live region. The default agent is in the header, so the mode line leaves it out.
    let mode_line = row(&done, MODE_LINE)
        .map(|at| done.visible[at].clone())
        .unwrap_or_default();
    assert!(
        mode_line.contains('$'),
        "the spend isn't on the mode line: {mode_line:?}"
    );
    let merged = scenario.repo.git(&["show", "HEAD:README.md"])?;
    assert!(merged.contains("npm install @acme/api"), "{merged}");
    exit(&mut pty)
}

fn exit(pty: &mut Pty) -> anyhow::Result<()> {
    pty.press(Key::Ctrl('c'))?;
    pty.press(Key::Ctrl('c'))?;
    pty.wait_for_exit(WAIT)?;
    Ok(())
}

/// Every UI scenario parses, and every recorded fixture it plays is where it says.
#[test]
fn ui_scenario_files_load() {
    for name in ["ui/small_fix.toml", "ui/four_states.toml"] {
        let path = scenario_file(name);
        let scenario = FakeScenario::load(&path).unwrap();
        assert!(!scenario.rules.is_empty(), "{name}");
        let folder = path.parent().unwrap();
        for step in scenario.rules.iter().flat_map(|rule| &rule.steps) {
            if let FakeStep::EmitFile(file) = step {
                assert!(Path::new(folder).join(file).is_file(), "{name}: {file}");
            }
        }
    }
}
