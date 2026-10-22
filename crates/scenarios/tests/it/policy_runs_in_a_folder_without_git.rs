//! Argonauts Code works in a folder that has no git. The app opens there and says once how it
//! works; a run keeps its branches in a history under Argonauts Code's home, and its finished work
//! is written back into your files. Your folder never gets a `.git`, and a file of yours the
//! run doesn't touch is left as it was.

use crate::policy_support;
use argonauts_core::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::Key;
use policy_support::{
    AUTO_MERGE_RULES, RUN_WAIT, captain_config, launch_ready, run_finished, scenario,
    start_captain, wait_for_events, write_global_config,
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
fn a_run_keeps_a_hidden_history_and_writes_its_work_back() -> anyhow::Result<()> {
    let scenario = scenario("worker-writes-hello.toml")?;
    write_global_config(&scenario, &captain_config(AUTO_MERGE_RULES))?;
    let root = scenario.repo.root().to_path_buf();
    std::fs::remove_dir_all(root.join(".git"))?;
    std::fs::write(root.join("notes.md"), "mine\n")?;

    let mut pty = launch_ready(&scenario, &[])?;
    pty.wait_for("This folder has no git.", WAIT)?;
    start_captain(&mut pty)?;

    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    assert_eq!(
        outcome(&events),
        Some(RunOutcome::Merged),
        "{:?}",
        notes(&events)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("hello.txt"))?.trim_end(),
        "hi"
    );
    assert_eq!(std::fs::read_to_string(root.join("notes.md"))?, "mine\n");
    assert!(!root.join(".git").exists(), "the folder got a .git");
    assert!(
        notes(&events)
            .iter()
            .any(|note| note.starts_with("This folder has no git, so this workflow keeps")),
        "{:?}",
        notes(&events)
    );
    scenario.shot(&pty, "policy_runs_in_a_folder_without_git")?;

    // The conversation says where the work went and what it changed, and the run that ended
    // keeps its row under the input for a while, its name in words, saying the same of where.
    pty.wait_for(
        r"Written into your folder · 1 file \+1 \u{2212}0 · history kept by Argonauts",
        WAIT,
    )?;
    pty.wait_for(
        r"\u{2714} \S+(?: \S+)* +\u{25b0}+  2/2 · \S+ · \u{2193}  \S+ tokens · written into your folder",
        WAIT,
    )?;
    Ok(())
}

/// Captain is the CLI itself in your folder, and needs no git: the turn edits your files as it
/// would anywhere, nothing warns about commits, and no history is kept that nobody asked for.
#[test]
fn captain_works_in_the_folder_directly_and_keeps_no_history() -> anyhow::Result<()> {
    const TASK: &str = "In notes.txt, change draft to final, then run echo hi > shell.txt";
    let scenario = Scenario::new("ui/direct_captain.toml")?;
    std::fs::create_dir_all(scenario.repo.home())?;
    std::fs::write(
        scenario.repo.global_config(),
        "crew.main = \"claude-code/haiku\"\n\n[captain_or_crew]\nforce = \"captain\"\n",
    )?;
    let root = scenario.repo.root().to_path_buf();
    std::fs::remove_dir_all(root.join(".git"))?;
    std::fs::write(root.join("notes.txt"), "This is a draft.\n")?;

    let mut pty = scenario.launch(&[])?;
    pty.wait_for("This folder has no git.", WAIT)?;
    pty.send(TASK)?;
    pty.press(Key::Enter)?;
    let done = pty.wait_for(r"(?m)^✻ \w+ for \d", WAIT)?.text();

    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt"))?,
        "This is a final.\n",
        "{done}"
    );
    for trouble in ["not a git repository", "without a commit", "can't put back"] {
        assert!(!done.contains(trouble), "{trouble}:\n{done}");
    }
    assert!(!root.join(".git").exists(), "the folder got a .git");
    let histories = walk_for(scenario.repo.home(), "history.git");
    assert!(histories.is_empty(), "a history was kept: {histories:?}");
    scenario.shot(&pty, "captain_in_a_folder_without_git")
}

/// A first run that keeps a hidden history for the scenario's folder, driven to its end, and
/// a lock on that history's index dated `taken`, as a stop in the middle of a write leaves.
fn after_a_run_with_a_lock(
    taken: impl Fn() -> std::time::SystemTime,
) -> anyhow::Result<(Scenario, argonauts_testkit::Pty, std::path::PathBuf)> {
    let scenario = scenario("worker-writes-hello.toml")?;
    // No spot check of the first run's merge, which would take the keys from the next request.
    let config = format!("{AUTO_MERGE_RULES}\n[approvals]\nspot_check_sample = 0.0\n");
    write_global_config(&scenario, &captain_config(&config))?;
    std::fs::remove_dir_all(scenario.repo.root().join(".git"))?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_captain(&mut pty)?;
    wait_for_events(&scenario, "the first run to finish", RUN_WAIT, run_finished)?;
    pty.wait_for("Run finished", WAIT)?;
    let histories = walk_for(scenario.repo.home(), "history.git");
    let [history] = histories.as_slice() else {
        anyhow::bail!("one hidden history: {histories:?}");
    };
    let lock = history.join("index.lock");
    std::fs::write(&lock, "")?;
    std::fs::File::options()
        .write(true)
        .open(&lock)?
        .set_modified(taken())?;
    Ok((scenario, pty, lock))
}

/// A hard stop leaves git's lock on the hidden history. Long after, nobody holds it: the
/// next run clears it, says so, and goes on.
#[test]
fn a_lock_a_stop_left_on_the_history_is_cleared_and_said() -> anyhow::Result<()> {
    let quarter_hour = std::time::Duration::from_secs(15 * 60);
    let (scenario, mut pty, lock) =
        after_a_run_with_a_lock(|| std::time::SystemTime::now() - quarter_hour)?;
    let first = scenario.latest_run_events()?.len();
    pty.send("/captain Create bye.txt containing bye\r")?;
    pty.wait_for("Cleared a lock on this folder's hidden history", RUN_WAIT)?;
    assert!(!lock.exists(), "the lock is gone");
    let started = |events: &[RunEvent]| {
        events
            .iter()
            .any(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Started(_))))
    };
    let events = wait_for_events(&scenario, "the second run to start", RUN_WAIT, started)?;
    assert!(events.len() != first || started(&events));
    let screen = pty.screen().text();
    assert!(!screen.contains("didn't start"), "{screen}");
    assert!(!screen.contains("its run host exited"), "{screen}");
    Ok(())
}

/// A lock taken this moment may belong to a git still writing, so it stays, and the run that
/// would fail on it says why it didn't start, in words.
#[test]
fn a_lock_just_taken_keeps_a_run_from_starting_and_says_why() -> anyhow::Result<()> {
    // Dated ahead, so it is new whenever the run's start looks at it.
    let minute = std::time::Duration::from_secs(60);
    let (_scenario, mut pty, lock) =
        after_a_run_with_a_lock(|| std::time::SystemTime::now() + minute)?;
    pty.send("/captain Create bye.txt containing bye\r")?;
    let screen = pty.wait_for("in a moment", RUN_WAIT)?.text();
    // One sentence, however the screen's width breaks it.
    let said = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        said.contains(
            "\"Create bye.txt containing bye\" didn't start: this folder's hidden history is \
             being written, or a stop left a lock 0s ago · try again in a moment"
        ),
        "{screen}"
    );
    assert!(!said.contains("run host"), "{screen}");
    assert!(lock.exists(), "a lock that may be held stays");
    Ok(())
}

/// Every folder named `name` under `dir`.
fn walk_for(dir: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|file| file == name) {
                found.push(path);
            } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                folders.push(path);
            }
        }
    }
    found
}
