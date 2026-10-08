//! Argonauts works in a folder that has no git. The app opens there and says once how it
//! works; a run keeps its branches in a history under Argonauts' home, and its finished work
//! is written back into your files. Your folder never gets a `.git`, and a file of yours the
//! run doesn't touch is left as it was.

mod policy_support;

use argonauts_core::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::Key;
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
fn a_run_keeps_a_hidden_history_and_writes_its_work_back() -> anyhow::Result<()> {
    let scenario = scenario("owner-writes-hello.toml")?;
    write_global_config(&scenario, &solo_config(AUTO_MERGE_RULES))?;
    let root = scenario.repo.root().to_path_buf();
    std::fs::remove_dir_all(root.join(".git"))?;
    std::fs::write(root.join("notes.md"), "mine\n")?;

    let mut pty = launch_ready(&scenario, &[])?;
    pty.wait_for("This folder has no git.", WAIT)?;
    start_solo(&mut pty)?;

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
    scenario.shot(&pty, "policy_runs_in_a_folder_without_git")
}

/// Solo is the CLI itself in your folder, and needs no git: the turn edits your files as it
/// would anywhere, nothing warns about commits, and no history is kept that nobody asked for.
#[test]
fn solo_works_in_the_folder_directly_and_keeps_no_history() -> anyhow::Result<()> {
    const TASK: &str = "In notes.txt, change draft to final, then run echo hi > shell.txt";
    let scenario = Scenario::new("ui/direct_solo.toml")?;
    std::fs::create_dir_all(scenario.repo.home())?;
    std::fs::write(
        scenario.repo.global_config(),
        "main_agent = \"claude-code/haiku\"\n\n[solo_or_crew]\nforce = \"solo\"\n",
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
    scenario.shot(&pty, "solo_in_a_folder_without_git")
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
