//! A repository where `git init` ran and nothing is committed yet. Your branch has nothing to
//! branch from, so a run starts from a snapshot of your files and says so, and its finished
//! work comes back as changes to your files. Nothing is committed or staged for you: your
//! first commit stays yours to make, unless `merge.unborn = "first_commit"` hands it to the
//! run.

mod policy_support;

use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_scenarios::WAIT;
use policy_support::{
    AUTO_MERGE_RULES, RUN_WAIT, launch_ready, run_finished, scenario, solo_config, start_solo,
    wait_for_events, write_global_config,
};

fn outcome(events: &[RunEvent]) -> Option<RunOutcome> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, .. }) => Some(*outcome),
        _ => None,
    })
}

fn notes(events: &[RunEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_run_starts_from_your_files_and_writes_its_work_back_uncommitted() -> anyhow::Result<()> {
    let scenario = scenario("owner-writes-hello.toml")?;
    write_global_config(&scenario, &solo_config(AUTO_MERGE_RULES))?;
    let repo = &scenario.repo;
    let root = repo.root().to_path_buf();
    std::fs::remove_dir_all(root.join(".git"))?;
    repo.git(&["init", "--quiet"])?;
    std::fs::write(root.join("notes.md"), "mine\n")?;
    repo.git(&["add", "notes.md"])?;
    let staged = repo.git(&["diff", "--cached", "--name-only"])?;

    let mut pty = launch_ready(&scenario, &[])?;
    start_solo(&mut pty)?;
    pty.wait_for("This repository has no commits yet", WAIT)?;

    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    let notes = notes(&events);
    assert_eq!(outcome(&events), Some(RunOutcome::Merged), "{notes:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("hello.txt"))?.trim_end(),
        "hi"
    );
    assert_eq!(std::fs::read_to_string(root.join("notes.md"))?, "mine\n");
    assert!(
        repo.head().is_err(),
        "a first commit was made on your branch"
    );
    assert_eq!(
        repo.git(&["diff", "--cached", "--name-only"])?,
        staged,
        "your staging area changed"
    );
    assert!(
        repo.git(&["status", "--porcelain"])?
            .contains("?? hello.txt"),
        "the run's file should be yours to commit"
    );
    assert!(
        notes
            .iter()
            .any(|note| note.starts_with("The run's changes are in your files for you to commit")),
        "{notes:?}"
    );
    // No branch moved, and the run says what its work went into.
    let landed_on = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Integration(IntegrationEvent::Landed { target, .. }) => Some(target.as_str()),
        _ => None,
    });
    assert_eq!(landed_on, Some("your files"));
    scenario.shot(&pty, "policy_runs_before_a_first_commit")
}

/// `merge.unborn = "first_commit"`: the run says from the start that its work becomes your
/// branch's first commit, and that commit holds your files with the run's. Afterwards git
/// shows nothing of them as a change.
#[test]
fn a_run_makes_your_first_commit_when_your_settings_ask() -> anyhow::Result<()> {
    let scenario = scenario("owner-writes-hello.toml")?;
    let config = format!("{AUTO_MERGE_RULES}\n[merge]\nunborn = \"first_commit\"\n");
    write_global_config(&scenario, &solo_config(&config))?;
    let repo = &scenario.repo;
    let root = repo.root().to_path_buf();
    std::fs::remove_dir_all(root.join(".git"))?;
    repo.git(&["init", "--quiet"])?;
    std::fs::write(root.join("notes.md"), "mine\n")?;
    repo.git(&["add", "notes.md"])?;
    let branch = repo.git(&["symbolic-ref", "--short", "HEAD"])?;

    let mut pty = launch_ready(&scenario, &[])?;
    start_solo(&mut pty)?;
    pty.wait_for("becomes your branch's first commit", WAIT)?;

    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    let notes = notes(&events);
    assert_eq!(outcome(&events), Some(RunOutcome::Merged), "{notes:?}");
    let landed = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Integration(IntegrationEvent::Landed { target, sha, .. }) => {
            Some((target.as_str(), sha.as_str()))
        }
        _ => None,
    });
    let first = repo.git(&["rev-list", "--max-parents=0", "HEAD"])?;
    assert_eq!(landed, Some((branch.as_str(), first.as_str())), "{notes:?}");
    let held = repo.git(&["ls-tree", "-r", "--name-only", &first])?;
    for file in ["hello.txt", "notes.md"] {
        assert!(held.lines().any(|path| path == file), "{file}: {held}");
    }
    assert_eq!(std::fs::read_to_string(root.join("notes.md"))?, "mine\n");
    let status = repo.git(&["status", "--porcelain"])?;
    assert!(
        !status.contains("hello.txt") && !status.contains("notes.md"),
        "committed files show as changes: {status}"
    );
    assert!(
        notes
            .iter()
            .any(|note| note.contains("had no commits, so the run made its first one")),
        "{notes:?}"
    );
    Ok(())
}
