//! Example 2 of the four, a small fix that runs as Captain, through the real binary with fake
//! agents. Its screens are captured for the side-by-side review with Claude Code's.

use std::fs;
use std::path::Path;

use argonauts_scenarios::{
    Scenario, WAIT, as_set_up, on_the_default_workflow, past_the_question_guard, scenario_file,
};
use argonauts_testkit::{FakeScenario, FakeStep, Key, Pty, Screen};
use regex::Regex;

const FIX: &str = "Fix the install command in README.md: it should be npm install @acme/api";
const README: &str = "# acme-api\n\nInstall it with:\n\n    npm install acme-api\n";

const TOP_RULE: &str = r"(?m)^─+ \S.* ─$";
const BOTTOM_ROW: &str = r"(?m)^  ǁ supervised mode on";
const TURN_END: &str = r"(?m)^✻ \w+ for \d";

/// A global config means no guided setup; the scenario is about the run, not the first start.
/// The workflow is only the default, so the rules leave this small fix to the main agent.
/// The main agent names its model, as rows like `Haiku 5.5 on Claude Code · worker` show
/// it, and since merging is automatic by default, this run's merge is set to wait for you.
/// The request ends with the verdict, which a CLI that takes no tools keeps.
fn skip_setup(scenario: &Scenario) -> anyhow::Result<()> {
    let config = scenario.repo.global_config();
    if let Some(dir) = config.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        config,
        as_set_up(&on_the_default_workflow(
            "crew.main = \"claude-code/haiku\"\n\n[ui]\ntheme = \"dark\"\n\n\
             [approvals.merge]\nmode = \"supervised\"\n\n[main_session]\ntools = false\n",
        )),
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
    // The spend is a part you add to the bottom line, in its own file among the mods.
    let line = scenario.repo.home().join("mods/bottom-line.toml");
    std::fs::create_dir_all(line.parent().expect("the mods folder"))?;
    std::fs::write(&line, "right = [\"plan-usage\", \"spend\"]\n")?;
    let mut pty = scenario.launch(&[])?;
    pty.wait_for(TOP_RULE, WAIT)?;
    pty.wait_for(BOTTOM_ROW, WAIT)?;
    scenario.shot(&pty, "small-fix-start")?;

    pty.send(FIX)?;
    pty.press(Key::Enter)?;
    // The agent's row marks the start, headed by its label and the task it works.
    pty.wait_for(
        r"(?m)^[●⏺] build:[^\s(]+\(.+\) · Haiku 5\.5 on Claude Code",
        WAIT,
    )?;
    scenario.shot(&pty, "small-fix-working")?;

    // The merge question needs you, so it takes the keys as it shows and stands in for the input
    // under its own rule, as Claude Code's prompts do.
    pty.wait_for(r"(?m)^◆ Merge", WAIT)?;
    let question = pty.wait_for(r"(?m)^  Enter to choose.* · Esc back to the list", WAIT)?;
    scenario.shot(&pty, "small-fix-approval")?;
    let merge = row(&question, r"^◆ Merge").unwrap_or(0);
    let rule = row(&question, r"^─+$").unwrap_or(0);
    assert!(
        rule + 1 == merge && row(&question, r"^─+ \S+ ─$").is_none(),
        "the question stands in for the input:\n{}",
        question.visible_text()
    );
    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.wait_for(r"❯ 1\. Merge", WAIT)?;
    pty.press(Key::Char('1'))?;
    pty.wait_for("Enter to merge", WAIT)?;
    pty.press(Key::Enter)?;

    pty.wait_for(r"(?m)^[●⏺] Merged", WAIT)?;
    pty.wait_for(TURN_END, WAIT)?;
    let done = pty.wait_for(BOTTOM_ROW, WAIT)?;
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
    assert_eq!(count(&done, r"^─+ \S.* ─$"), 1, "{}", done.text());
    // The spend sits at the right end of the bottom line, after each CLI's plan usage, with no
    // status row of its own under the live region. The rule names the model, so the bottom line
    // never does.
    let bottom_row = row(&done, BOTTOM_ROW)
        .map(|at| done.visible[at].clone())
        .unwrap_or_default();
    assert!(
        bottom_row.contains('$'),
        "the spend isn't on the bottom line: {bottom_row:?}"
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
