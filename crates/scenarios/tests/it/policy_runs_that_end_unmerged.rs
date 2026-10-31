//! A run that ends without merging, on the real binary: what its last lines say is what git
//! holds. A pull request carries the run's work alone, one that couldn't be opened is said
//! and asked about again, and an abandoned run's branches go with it.

use crate::policy_support;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use argonauts_core::runs::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_scenarios::{Scenario, WAIT, past_the_question_guard};
use argonauts_testkit::{Key, Pty};
use policy_support::{
    HELLO_CHECK, IDLE, RUN_WAIT, SUPERVISED_MERGE, captain_config, merge_opened, run_finished,
    scenario, start_captain, wait_for_events, write_global_config,
};

const COLS: u16 = 118;
const ROWS: u16 = 60;

fn launch(scenario: &Scenario) -> anyhow::Result<Pty> {
    let pty = scenario.launch_sized(&[], ROWS, COLS)?;
    pty.wait_for(IDLE, WAIT)?;
    Ok(pty)
}

/// Moves the pointer to option `digit` of the question on screen, which answers it unless it
/// asks to be confirmed.
fn pick(pty: &mut Pty, digit: char) -> anyhow::Result<()> {
    pty.wait_for(r"❯ \d+\. ", WAIT)?;
    past_the_question_guard();
    pty.press(Key::Char(digit))
}

fn outcome(events: &[RunEvent]) -> Option<RunOutcome> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => Some(*outcome),
        _ => None,
    })
}

/// A `gh` first on the scenario's `PATH`, in place of the machine's own: it prints `says` and
/// leaves with `status`.
fn install_gh(scenario: &Scenario, says: &str, status: i32) -> anyhow::Result<()> {
    let gh = scenario.clis.bin_dir().join("gh");
    let out = if status == 0 { "" } else { " >&2" };
    std::fs::write(
        &gh,
        format!("#!/bin/sh\necho '{says}'{out}\nexit {status}\n"),
    )?;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// A bare repository added as `origin`, with `main` pushed to it.
fn add_remote(scenario: &Scenario) -> anyhow::Result<PathBuf> {
    let remote = scenario.repo.scratch().join("origin.git");
    let path = remote.to_string_lossy().into_owned();
    scenario.repo.git(&["init", "--bare", "--quiet", &path])?;
    scenario.repo.git(&["remote", "add", "origin", &path])?;
    scenario.repo.git(&["push", "--quiet", "origin", "main"])?;
    Ok(remote)
}

fn in_remote(remote: &std::path::Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .args(args)
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[test]
fn an_abandoned_run_says_its_work_is_removed_and_git_agrees() -> anyhow::Result<()> {
    let scenario = scenario("worker-writes-hello.toml")?;
    write_global_config(
        &scenario,
        &captain_config(&format!("{SUPERVISED_MERGE}\n{HELLO_CHECK}")),
    )?;
    let mut pty = launch(&scenario)?;
    start_captain(&mut pty)?;
    wait_for_events(&scenario, "the merge question", RUN_WAIT, merge_opened)?;
    let before = scenario.repo.git(&["branch", "--list", "argonauts/*"])?;
    anyhow::ensure!(!before.is_empty(), "the run has branches while it waits");

    let abandon = pty
        .wait_for(r"\d+\. Abandon the run", WAIT)?
        .text()
        .lines()
        .find_map(|row| {
            let row = row.trim().trim_start_matches('❯').trim();
            row.ends_with(". Abandon the run")
                .then(|| row.chars().next())
                .flatten()
        })
        .ok_or_else(|| anyhow::anyhow!("the merge question offers to abandon the run"))?;
    pick(&mut pty, abandon)?;
    // Said before it is done, where it can still be taken back.
    pty.wait_for(
        "Enter to abandon the run, its work is removed · Esc to cancel",
        WAIT,
    )?;
    pty.press(Key::Enter)?;
    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    assert_eq!(outcome(&events), Some(RunOutcome::Abandoned));
    pty.wait_for("Abandoned the run · its work is removed", WAIT)?;

    let after = scenario.repo.git(&["branch", "--list", "argonauts/*"])?;
    assert_eq!(after, "", "the run's branches after it was abandoned");
    assert!(!scenario.repo.root().join("hello.txt").exists());
    Ok(())
}

#[test]
fn a_pull_request_carries_none_of_your_uncommitted_edits() -> anyhow::Result<()> {
    let scenario = scenario("worker-writes-hello.toml")?;
    write_global_config(
        &scenario,
        &captain_config(&format!(
            "{SUPERVISED_MERGE}\n{HELLO_CHECK}\n[intake]\ninclude_uncommitted = true\n"
        )),
    )?;
    install_gh(&scenario, "https://example.invalid/you/repo/pull/7", 0)?;
    scenario.repo.write("notes.md", "one\n")?;
    scenario.repo.commit_all("notes")?;
    let remote = add_remote(&scenario)?;
    scenario
        .repo
        .write("notes.md", "one\nmine, not committed\n")?;
    scenario.repo.write("scratch.md", "mine, never added\n")?;

    let mut pty = launch(&scenario)?;
    start_captain(&mut pty)?;
    wait_for_events(&scenario, "the merge question", RUN_WAIT, merge_opened)?;
    pick(&mut pty, '2')?;
    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    assert_eq!(outcome(&events), Some(RunOutcome::PullRequest));
    pty.wait_for("Opened a pull request for the run", WAIT)?;
    pty.wait_for("https://example.invalid/you/repo/pull/7", WAIT)?;

    let branches = in_remote(&remote, &["branch", "--format=%(refname:short)"])?;
    let pushed: Vec<&str> = branches
        .lines()
        .filter(|branch| branch.starts_with("argonauts/"))
        .collect();
    let [branch] = pushed.as_slice() else {
        anyhow::bail!("one branch of the run is on the remote: {branches}");
    };
    // One commit on what main holds there, with the run's file and nothing of yours.
    let main = in_remote(&remote, &["rev-parse", "main"])?;
    assert_eq!(
        in_remote(&remote, &["rev-parse", &format!("{branch}^")])?,
        main
    );
    let files = in_remote(&remote, &["ls-tree", "-r", "--name-only", branch])?;
    assert!(files.lines().any(|file| file == "hello.txt"), "{files}");
    assert!(!files.lines().any(|file| file == "scratch.md"), "{files}");
    let notes = in_remote(&remote, &["show", &format!("{branch}:notes.md")])?;
    assert_eq!(notes, "one");
    let subjects = in_remote(&remote, &["log", "--format=%s", branch])?;
    assert!(!subjects.contains("uncommitted"), "{subjects}");

    // What you hadn't committed is still yours, and still uncommitted.
    let status = scenario.repo.git(&["status", "--porcelain"])?;
    assert!(
        status.contains("notes.md") && status.contains("scratch.md"),
        "{status}"
    );
    Ok(())
}

#[test]
fn a_pull_request_gh_could_not_open_is_said_and_the_question_asks_again() -> anyhow::Result<()> {
    let scenario = scenario("worker-writes-hello.toml")?;
    write_global_config(
        &scenario,
        &captain_config(&format!("{SUPERVISED_MERGE}\n{HELLO_CHECK}")),
    )?;
    install_gh(&scenario, "pull request create failed: no commits", 1)?;
    add_remote(&scenario)?;
    let mut pty = launch(&scenario)?;
    start_captain(&mut pty)?;
    wait_for_events(&scenario, "the merge question", RUN_WAIT, merge_opened)?;
    pick(&mut pty, '2')?;

    pty.wait_for("Asked for a pull request for the run", WAIT)?;
    pty.wait_for("No pull request was opened: gh pr create failed", WAIT)?;
    let again = pty
        .wait_for("Asked again: gh pr create failed", WAIT)?
        .text();
    assert!(!again.contains("Opened a pull request"), "{again}");
    std::thread::sleep(Duration::from_millis(500));
    let events = scenario.latest_run_events()?;
    assert_eq!(outcome(&events), None, "the run still waits for an answer");
    Ok(())
}
