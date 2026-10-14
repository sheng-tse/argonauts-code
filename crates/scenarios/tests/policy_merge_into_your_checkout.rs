//! The sample run in a Python repo with no `.gitignore`: the agent's tests left bytecode in its
//! worktree, Argonauts Code committed it, and the fast-forward into your checkout failed on your
//! own untracked bytecode at the same paths, after the transcript had said "Merged". Now a file
//! of yours in the way makes the run wait to merge and ask you, never fail. And the run's
//! commits take only the new files its agent meant to create, the ones its edit tools wrote
//! or its report lists, so what the tests left behind stays out with no list of file types.

mod policy_support;

use argonauts_core::events::approval::{ApprovalEvent, ApprovalKind};
use argonauts_core::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::events::task::MergeEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_scenarios::{WAIT, past_the_question_guard};
use argonauts_testkit::Key;
use policy_support::{
    AUTO_MERGE_RULES, RUN_WAIT, captain_config, launch_ready, run_finished, start_captain,
    trusted_scenario, wait_for_events,
};

const SCENARIO: &str = "worker-runs-python-tests.toml";
const YOUR_BYTECODE: &str = "your own bytecode";
const YOURS: &str = "__pycache__/textkit.cpython-312.pyc";
/// Every new file is committed, whoever made it, as before the claims rule.
const KEEP_EVERY_NEW_FILE: &str = "\n[commit]\nunclaimed = \"keep\"\n";

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

fn waiting_to_merge(events: &[RunEvent]) -> Option<&Vec<String>> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Merge(MergeEvent::WaitingToMerge { in_the_way, .. }) => Some(in_the_way),
        _ => None,
    })
}

#[test]
fn what_the_tests_left_stays_out_and_the_merge_lands_beside_yours() -> anyhow::Result<()> {
    let scenario = trusted_scenario(SCENARIO, &captain_config(AUTO_MERGE_RULES))?;
    scenario.repo.write(YOURS, YOUR_BYTECODE)?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_captain(&mut pty)?;

    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    assert_eq!(outcome(&events), Some(RunOutcome::Merged));
    let merged = scenario
        .repo
        .git(&["ls-tree", "-r", "--name-only", "main"])?;
    assert!(merged.lines().any(|path| path == "textkit.py"), "{merged}");
    assert!(
        !merged.contains("__pycache__") && !merged.contains(".pytest_cache"),
        "build output merged:\n{merged}"
    );
    assert_eq!(
        std::fs::read_to_string(scenario.repo.root().join(YOURS))?,
        YOUR_BYTECODE
    );
    // Each commit names what it left out, each folder once, so nothing goes missing unsaid.
    let told: Vec<&str> = notes(&events)
        .into_iter()
        .filter(|note| note.starts_with("Left out of task-001's commit"))
        .collect();
    let pycache = told
        .iter()
        .filter(|note| note.contains("__pycache__/"))
        .count();
    assert_eq!(pycache, 1, "named once: {told:?}");
    assert!(
        told.iter().any(|note| note.contains(".pytest_cache/")),
        "{told:?}"
    );
    assert!(waiting_to_merge(&events).is_none());
    pty.wait_for("Merged the run into main", WAIT)?;
    scenario.shot(&pty, "policy_build_output_stays_out_of_the_merge")
}

/// The sample case as it ran, with `commit.unclaimed = "keep"`: the run's commit takes the
/// bytecode its tests left, yours is at the same path, and the run waits to merge on its
/// approval's choices instead of failing, its work kept on its branch.
#[test]
fn with_every_new_file_committed_your_bytecode_in_the_way_waits_to_merge() -> anyhow::Result<()> {
    let config = captain_config(&format!("{AUTO_MERGE_RULES}{KEEP_EVERY_NEW_FILE}"));
    let scenario = trusted_scenario(SCENARIO, &config)?;
    scenario.repo.write(YOURS, YOUR_BYTECODE)?;
    let main = scenario.repo.head()?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_captain(&mut pty)?;

    let events = wait_for_events(&scenario, "the merge to wait", RUN_WAIT, |events| {
        waiting_to_merge(events).is_some()
    })?;
    assert_eq!(
        waiting_to_merge(&events).map(Vec::as_slice),
        Some([YOURS.to_owned()].as_slice())
    );
    assert_eq!(outcome(&events), None, "waiting to merge, not failed");
    pty.wait_for("Waiting to merge into main", WAIT)?;
    pty.wait_for("Move them aside and merge", WAIT)?;
    let screen = pty.screen().text();
    assert!(!screen.contains("The run failed"), "{screen}");
    assert_eq!(scenario.repo.head()?, main, "nothing merged");
    assert_eq!(
        std::fs::read_to_string(scenario.repo.root().join(YOURS))?,
        YOUR_BYTECODE
    );
    let branches = scenario.repo.git(&[
        "for-each-ref",
        "--format=%(refname:short)",
        "refs/heads/argonauts/",
    ])?;
    let run_branch = branches
        .lines()
        .find(|branch| branch.ends_with("/merged"))
        .ok_or_else(|| anyhow::anyhow!("no run branch in {branches:?}"))?;
    let kept = scenario
        .repo
        .git(&["ls-tree", "-r", "--name-only", run_branch])?;
    assert!(kept.lines().any(|path| path == "textkit.py"), "{kept}");
    assert!(kept.lines().any(|path| path == YOURS), "{kept}");
    Ok(())
}

/// The review's probe: an uncommitted edit of yours to a file the run leaves alone, which a
/// fast-forward carries along, made each landing go back to your rules, and the third
/// approval took the day's last auto-merge. Now it lands at once, on one approval.
#[test]
fn an_edit_of_yours_the_run_leaves_alone_rides_along() -> anyhow::Result<()> {
    let scenario = trusted_scenario(SCENARIO, &captain_config(AUTO_MERGE_RULES))?;
    let notes_of_yours = "# Test repo\n\nMy notes, not committed yet.\n";
    scenario.repo.write("README.md", notes_of_yours)?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_captain(&mut pty)?;

    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    assert_eq!(outcome(&events), Some(RunOutcome::Merged));
    let merge_questions = events
        .iter()
        .filter(|event| {
            matches!(&event.kind,
                RunEventKind::Approval(ApprovalEvent::Opened(opened))
                    if opened.kind == ApprovalKind::Merge)
        })
        .count();
    assert_eq!(merge_questions, 1, "asked once");
    assert_eq!(
        std::fs::read_to_string(scenario.repo.root().join("README.md"))?,
        notes_of_yours
    );
    pty.wait_for("Merged the run into main", WAIT)?;
    Ok(())
}

#[test]
fn a_file_of_yours_in_the_way_waits_to_merge_and_moves_aside() -> anyhow::Result<()> {
    let scenario = trusted_scenario(SCENARIO, &captain_config(AUTO_MERGE_RULES))?;
    scenario.repo.write("textkit.py", "# my own draft\n")?;
    let mut pty = launch_ready(&scenario, &[])?;
    start_captain(&mut pty)?;

    let events = wait_for_events(&scenario, "the merge to wait", RUN_WAIT, |events| {
        waiting_to_merge(events).is_some()
    })?;
    assert_eq!(
        waiting_to_merge(&events).map(Vec::as_slice),
        Some(["textkit.py".to_owned()].as_slice())
    );
    assert_eq!(outcome(&events), None, "waiting to merge, not failed");
    pty.wait_for("Waiting to merge into main", WAIT)?;
    pty.wait_for("Move them aside and merge", WAIT)?;
    let screen = pty.screen().text();
    assert!(
        screen.contains("Approved merging the run into main"),
        "{screen}"
    );
    assert!(!screen.contains("Merged the run into main"), "{screen}");
    assert!(!screen.contains("The run failed"), "{screen}");
    scenario.shot(&pty, "policy_merge_waits_on_your_files")?;

    // The question opens waiting, or already focused on its first option.
    pty.wait_for(r"❯ \d+\. ", WAIT)?;
    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char('1'))?;
    let events = wait_for_events(&scenario, "the run to finish", RUN_WAIT, run_finished)?;
    assert_eq!(outcome(&events), Some(RunOutcome::Merged));
    assert_eq!(
        std::fs::read_to_string(scenario.repo.root().join("textkit.py"))?,
        "def slugify(text):\n    return text.lower()\n"
    );
    let said = "Moved textkit.py out of the way, into ";
    let folder = notes(&events)
        .into_iter()
        .find_map(|note| note.strip_prefix(said))
        .expect("a note says where your file went");
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(folder).join("textkit.py"))?,
        "# my own draft\n"
    );
    pty.wait_for("Merged the run into main", WAIT)?;
    scenario.shot(&pty, "policy_merge_after_moving_your_files_aside")
}
