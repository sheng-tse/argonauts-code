//! Runs in repositories in awkward states: `.argonauts/` ignored, files you are editing, a main
//! that moves while the run works, untracked build files, and a detached head. In each one the
//! task's change lands exactly as made, and nothing of yours, and nothing of Argonauts' own, is
//! touched or committed.

mod support;

use anyhow::Context;
use argonauts_core::engine::EngineExit;
use argonauts_core::events::run::{RunMode, RunOutcome};

use support::{Crew, Outcome, names};

const REQUEST: &str = "add a --verbose flag";

/// A crew whose worker writes `verbose.txt` in its task, and a reviewer of another provider.
fn crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.building_worker("claude-code/opus", "claude", "verbose.txt", "on\n");
    crew.answering_worker("codex/gpt-x", "gpt");
    Ok(crew)
}

async fn run(crew: &Crew) -> anyhow::Result<Outcome> {
    let outcomes = crew.run_to_end(RunMode::Solo, REQUEST, 8).await?;
    outcomes
        .into_iter()
        .last()
        .ok_or_else(|| anyhow::anyhow!("the run never started"))
}

fn merged(crew: &Crew, outcome: &Outcome) -> anyhow::Result<()> {
    anyhow::ensure!(
        outcome.exit == EngineExit::Finished(RunOutcome::Merged),
        "{:?}: {:?}",
        outcome.exit,
        names(&outcome.events)
    );
    let landed = crew.repo.git(&["show", "HEAD:verbose.txt"])?;
    anyhow::ensure!(landed == "on", "{landed}");
    Ok(())
}

/// No goal, note or decision of Argonauts' in any commit on any branch, and nothing under
/// `.argonauts/` on yours.
fn nothing_of_argonauts(crew: &Crew) -> anyhow::Result<()> {
    let own = [
        ".argonauts/goal",
        ".argonauts/notes",
        ".argonauts/decisions",
    ];
    let mut args = vec!["log", "--all", "--name-only", "--format=", "--"];
    args.extend(own);
    let touched = crew.repo.git(&args)?;
    anyhow::ensure!(touched.trim().is_empty(), "{touched}");
    let yours = crew.repo.git(&[
        "log",
        "--name-only",
        "--format=",
        "main",
        "--",
        ".argonauts",
    ])?;
    anyhow::ensure!(yours.trim().is_empty(), "{yours}");
    Ok(())
}

#[tokio::test]
async fn an_ignored_argonauts_folder() -> anyhow::Result<()> {
    let crew = crew()?;
    crew.repo.write(".gitignore", ".argonauts/\n")?;
    crew.repo.commit_all("ignore .argonauts/")?;
    let outcome = run(&crew).await?;
    merged(&crew, &outcome)?;
    nothing_of_argonauts(&crew)?;
    Ok(())
}

#[tokio::test]
async fn files_you_are_editing_stay_as_they_are() -> anyhow::Result<()> {
    let crew = crew()?;
    crew.repo.write("notes.md", "draft\n")?;
    crew.repo.commit_all("notes")?;
    crew.repo
        .write("notes.md", "draft, still being written\n")?;
    let outcome = run(&crew).await?;
    merged(&crew, &outcome)?;
    let notes = std::fs::read_to_string(crew.repo.root().join("notes.md"))?;
    assert_eq!(notes, "draft, still being written\n");
    let committed = crew.repo.git(&["show", "HEAD:notes.md"])?;
    assert_eq!(committed, "draft");
    nothing_of_argonauts(&crew)?;
    Ok(())
}

#[tokio::test]
async fn untracked_build_files_stay_yours() -> anyhow::Result<()> {
    let crew = crew()?;
    crew.repo
        .write("__pycache__/app.cpython-312.pyc", "bytecode")?;
    crew.repo.write("build/out.o", "object")?;
    let outcome = run(&crew).await?;
    merged(&crew, &outcome)?;
    for yours in ["__pycache__/app.cpython-312.pyc", "build/out.o"] {
        assert!(
            crew.repo.root().join(yours).exists(),
            "{yours} went missing"
        );
        let tracked = crew.repo.git(&["ls-files", "--", yours])?;
        assert!(tracked.is_empty(), "{yours} was committed");
    }
    Ok(())
}

#[tokio::test]
async fn a_main_that_moved_while_the_run_worked() -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    let root = crew.repo.root().to_path_buf();
    let envs = crew.repo.envs();
    let fake = argonauts_testkit::worker::FakeWorker::harness("claude-code/opus", "claude")
        .respond_with(move |spec| {
            if spec.step == Some(argonauts_core::steps::StepName::Build) {
                // Someone commits on main while the task works.
                std::fs::write(root.join("changelog.md"), "moved\n").expect("main moves");
                for args in [
                    &["add", "changelog.md"][..],
                    &["commit", "-qm", "moved"][..],
                ] {
                    let status = std::process::Command::new("git")
                        .args(args)
                        .current_dir(&root)
                        .envs(envs.iter().map(|(name, value)| (name, value)))
                        .status()
                        .expect("git runs");
                    assert!(status.success());
                }
                std::fs::write(spec.workdir.join("verbose.txt"), "on\n").expect("the task writes");
                return support::smallest_answer(spec).with_event(support::wrote("verbose.txt"));
            }
            support::smallest_answer(spec)
        });
    crew.enlist(fake);
    crew.answering_worker("codex/gpt-x", "gpt");
    let outcome = run(&crew).await?;
    merged(&crew, &outcome)?;
    let main_work = crew.repo.git(&["show", "HEAD:changelog.md"])?;
    assert_eq!(main_work, "moved");
    nothing_of_argonauts(&crew)?;
    Ok(())
}

/// With your checkout detached, the run lands on the branch it started from and leaves your
/// checkout where it is, files included.
#[tokio::test]
async fn a_detached_head_stays_where_it_is() -> anyhow::Result<()> {
    let crew = crew()?;
    let before = crew.repo.head()?;
    crew.repo.git(&["checkout", "--quiet", "--detach"])?;
    let outcome = run(&crew).await?;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:?}",
        names(&outcome.events)
    );
    let landed = crew.repo.git(&["show", "main:verbose.txt"])?;
    assert_eq!(landed, "on");
    assert_eq!(crew.repo.git(&["rev-parse", "HEAD"])?, before);
    assert!(crew.repo.git(&["symbolic-ref", "-q", "HEAD"]).is_err());
    assert!(!crew.repo.root().join("verbose.txt").exists());
    nothing_of_argonauts(&crew)?;
    Ok(())
}

/// A folder with no git of its own works through the history Argonauts keeps for it under its
/// home: the change lands in your files, and the folder never gets a `.git`.
#[tokio::test]
async fn a_folder_with_no_git() -> anyhow::Result<()> {
    let crew = crew()?;
    let root = crew.repo.root().to_path_buf();
    let hidden = argonauts_core::hidden_git::dir(crew.repo_context()?.state_dir());
    std::fs::create_dir_all(hidden.parent().context("a state folder")?)?;
    std::fs::rename(root.join(".git"), &hidden)?;
    let outcome = run(&crew).await?;
    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:?}",
        names(&outcome.events)
    );
    assert_eq!(std::fs::read_to_string(root.join("verbose.txt"))?, "on\n");
    assert!(!root.join(".git").exists());
    assert!(!root.join(".argonauts").exists());
    Ok(())
}
