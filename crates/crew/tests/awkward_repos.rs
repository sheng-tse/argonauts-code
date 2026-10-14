//! Runs in repositories in awkward states: `.argonauts/` ignored, files you are editing, a main
//! that moves while the run works, untracked build files, and a detached head. In each one the
//! task's change lands exactly as made, and nothing of yours, and nothing of Argonauts Code's own,
//! is touched or committed.

mod support;

use std::time::Duration;

use anyhow::Context;
use argonauts_core::api::ApprovalAnswer;
use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
};
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::task::MergeEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::log::COPIES_REMOVED;
use argonauts_core::names::SessionKey;
use argonauts_core::session_runner::SessionRunner;
use argonauts_core::settings::Span;
use argonauts_core::settings::delivery::DeliveryWrite;
use argonauts_crew::engine::shared::with_git_env;
use argonauts_crew::merge::{detached, your_folder};
use serde_json::json;

use argonauts_core::steps::StepName;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::{Crew, Outcome, names, recommended, smallest_answer, wrote};

const REQUEST: &str = "add a --verbose flag";

/// A crew whose worker writes `verbose.txt` in its task, and a reviewer of another provider.
fn crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.building_worker("claude-code/opus", "anthropic", "verbose.txt", "on\n");
    crew.answering_worker("codex/gpt-x", "openai");
    Ok(crew)
}

async fn run(crew: &Crew) -> anyhow::Result<Outcome> {
    let outcomes = crew.run_to_end(RunMode::Captain, REQUEST, 8).await?;
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
    let merged = crew.repo.git(&["show", "HEAD:verbose.txt"])?;
    anyhow::ensure!(merged == "on", "{merged}");
    Ok(())
}

/// No goal, note or decision of Argonauts Code's in any commit on any branch, and nothing under
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

/// The first event of a run started from `snapshot`, your branch's tip with what you hadn't
/// committed on top, as `intake.include_uncommitted` starts one.
fn started_from(crew: &Crew, snapshot: String) -> anyhow::Result<RunEvent> {
    let mut first = crew.run_started(RunMode::Captain, REQUEST)?;
    if let RunEventKind::Run(RunLifecycle::Started(started)) = &mut first.kind {
        started.base_commit = snapshot;
        started.uncommitted_included = true;
    }
    Ok(first)
}

/// A run started from `snapshot`, driven to its end.
async fn run_from_snapshot(crew: &Crew, snapshot: String) -> anyhow::Result<Outcome> {
    let first = started_from(crew, snapshot)?;
    let mut outcome = crew.resume(vec![first], Vec::new()).await?;
    for _ in 0..8 {
        if !matches!(outcome.exit, EngineExit::Waiting(_)) {
            break;
        }
        let Some((command, _reply)) = recommended(&outcome.events) else {
            break;
        };
        outcome = crew.resume(outcome.events.clone(), vec![command]).await?;
    }
    Ok(outcome)
}

/// Your checkout as a commit on top of its head that no branch points at, with the checkout
/// and its index left as they were.
fn snapshot_of(crew: &Crew) -> anyhow::Result<String> {
    let head = crew.repo.head()?;
    crew.repo.git(&["add", "--all"])?;
    crew.repo
        .git(&["commit", "-qm", "your files as the run starts"])?;
    let snapshot = crew.repo.head()?;
    crew.repo.git(&["reset", "-q", "--mixed", &head])?;
    Ok(snapshot)
}

const EIGHT_LINES: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";
const WITH_YOUR_EDIT: &str = "a, yours\nb\nc\nd\ne\nf\ng\nh\n";
const WITH_BOTH: &str = "a, yours\nb\nc\nd\ne\nf\ng\nh, by the run\n";

/// A crew whose worker changes the last line of `cli.txt`, in a repository where you have
/// edited its first line, edited `notes.md` and added `todo.md`, all uncommitted.
fn crew_over_your_edits() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.building_worker("claude-code/opus", "anthropic", "cli.txt", WITH_BOTH);
    crew.answering_worker("codex/gpt-x", "openai");
    crew.repo.write("cli.txt", EIGHT_LINES)?;
    crew.repo.write("notes.md", "draft\n")?;
    crew.repo.commit_all("files")?;
    crew.repo.write("cli.txt", WITH_YOUR_EDIT)?;
    crew.repo
        .write("notes.md", "draft, still being written\n")?;
    crew.repo.write("todo.md", "yours alone\n")?;
    Ok(crew)
}

fn status(crew: &Crew) -> anyhow::Result<Vec<String>> {
    let status = crew.repo.git(&["status", "--porcelain"])?;
    Ok(status.lines().map(|line| line.trim().to_owned()).collect())
}

#[tokio::test]
async fn your_uncommitted_edits_stay_out_of_the_runs_commit() -> anyhow::Result<()> {
    let crew = crew_over_your_edits()?;
    let yours = crew.repo.head()?;
    let before = status(&crew)?;
    let snapshot = snapshot_of(&crew)?;
    assert_eq!(status(&crew)?, before);

    let outcome = run_from_snapshot(&crew, snapshot).await?;
    anyhow::ensure!(
        outcome.exit == EngineExit::Finished(RunOutcome::Merged),
        "{:?}: {:?}",
        outcome.exit,
        names(&outcome.events)
    );

    // One commit on your branch, of the run's line alone.
    assert_eq!(crew.repo.git(&["rev-parse", "HEAD^"])?, yours);
    let in_the_commit = crew
        .repo
        .git(&["show", "--format=", "--name-only", "HEAD"])?;
    assert_eq!(in_the_commit, "cli.txt");
    let committed = crew.repo.git(&["show", "HEAD:cli.txt"])?;
    assert_eq!(committed, "a\nb\nc\nd\ne\nf\ng\nh, by the run");
    assert_eq!(crew.repo.git(&["show", "HEAD:notes.md"])?, "draft");

    // Your files hold your edits with the run's work, and the edits are still uncommitted.
    let read = |file: &str| std::fs::read_to_string(crew.repo.root().join(file));
    assert_eq!(read("cli.txt")?, WITH_BOTH);
    assert_eq!(read("notes.md")?, "draft, still being written\n");
    assert_eq!(read("todo.md")?, "yours alone\n");
    assert_eq!(status(&crew)?, before);
    let still_yours = crew.repo.git(&["diff", "--unified=0", "cli.txt"])?;
    assert!(still_yours.contains("+a, yours"), "{still_yours}");
    assert!(!still_yours.contains("by the run"), "{still_yours}");

    assert!(
        notes(&outcome.events)
            .iter()
            .any(|note| note.contains("holds only its own work")),
        "{:?}",
        notes(&outcome.events)
    );

    // The question showed what would land: one file, one line changed.
    assert_eq!(counted(&outcome.events)?, (1, 1, 1));
    nothing_of_argonauts(&crew)?;
    Ok(())
}

#[tokio::test]
async fn an_edit_you_committed_meanwhile_is_not_undone() -> anyhow::Result<()> {
    let crew = crew_over_your_edits()?;
    let snapshot = snapshot_of(&crew)?;
    // While the run would work, you commit the edits it started from.
    crew.repo.git(&["add", "--all"])?;
    crew.repo.git(&["commit", "-qm", "my edits"])?;
    let yours = crew.repo.head()?;

    let outcome = run_from_snapshot(&crew, snapshot).await?;
    anyhow::ensure!(
        outcome.exit == EngineExit::Finished(RunOutcome::Merged),
        "{:?}: {:?}",
        outcome.exit,
        names(&outcome.events)
    );
    assert_eq!(crew.repo.git(&["rev-parse", "HEAD^"])?, yours);
    assert_eq!(
        crew.repo.git(&["show", "HEAD:cli.txt"])?,
        WITH_BOTH.trim_end()
    );
    assert_eq!(
        crew.repo.git(&["show", "HEAD:notes.md"])?,
        "draft, still being written"
    );
    assert_eq!(status(&crew)?, Vec::<String>::new());
    assert!(
        !notes(&outcome.events)
            .iter()
            .any(|note| note.contains("holds only its own work")),
        "{:?}",
        notes(&outcome.events)
    );
    Ok(())
}

/// What the merge question counted: files, lines added and lines removed.
fn counted(events: &[RunEvent]) -> anyhow::Result<(u32, u32, u32)> {
    let asked = merge_questions(events);
    let Some(ApprovalPacket::Merge(packet)) = asked.first().map(|opened| &opened.packet) else {
        anyhow::bail!("no merge question with a merge packet");
    };
    Ok((
        packet.combined.files_changed,
        packet.combined.insertions,
        packet.combined.deletions,
    ))
}

/// Whether `session` has ended in `events`.
fn ended(events: &[RunEvent], session: &SessionKey) -> bool {
    events.iter().any(|event| {
        event.session.as_ref() == Some(session)
            && matches!(event.kind, RunEventKind::Session(SessionEvent::Ended(_)))
    })
}

/// The merger writes its message for what lands. The files it is told of hold none of your
/// uncommitted edits, and the commit takes the message.
#[tokio::test]
async fn the_mergers_message_is_for_the_runs_work_alone() -> anyhow::Result<()> {
    let mut crew = crew_over_your_edits()?;
    let merger =
        crew.services
            .workers
            .add(
                FakeRunner::cli("opencode/merger", "opencode").respond_with(|_| {
                    FakeSession::structured(json!({
                        "subject": "Mark the last line",
                        "body": "Says who wrote it.",
                    }))
                }),
            );
    crew.settings.crew.merger = vec![merger.model_ref().clone()];
    // The merger writes while the merge question waits, so the run stays up for its answer.
    crew.settings.run_host.wait_grace = Span::new("60s");
    let snapshot = snapshot_of(&crew)?;

    let (context, sink, commands) = crew.context(vec![started_from(&crew, snapshot)?])?;
    let engine = argonauts_crew::engine_for(&crew.settings, RunMode::Captain);
    let run = with_git_env(crew.git_env(), engine.run(context));
    let answer = async {
        let asked = async {
            loop {
                let events = sink.events();
                let written = merger.specs().first().map(|spec| ended(&events, &spec.key));
                if written == Some(true) {
                    break recommended(&events);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let (merge, reply) = tokio::time::timeout(Duration::from_secs(30), asked)
            .await
            .context("the merger never wrote a message")?
            .context("the merge question")?;
        anyhow::ensure!(commands.send(merge).await.is_ok(), "the run's inbox closed");
        Ok(reply)
    };
    let (exit, answered) = tokio::join!(run, answer);
    let _reply = answered?;
    anyhow::ensure!(
        exit == EngineExit::Finished(RunOutcome::Merged),
        "{exit:?}: {:?}",
        names(&sink.events())
    );

    let specs = merger.specs();
    assert_eq!(specs.len(), 1, "one message, written once");
    let change = specs[0]
        .prompt
        .iter()
        .find(|part| part.label == "change")
        .context("the change the merger is told of")?;
    assert!(change.text.contains("- cli.txt (+1 -1)"), "{}", change.text);
    for yours in ["notes.md", "todo.md"] {
        assert!(!change.text.contains(yours), "{}", change.text);
    }
    let message = crew.repo.git(&["log", "-1", "--format=%B", "HEAD"])?;
    assert!(
        message.starts_with("Mark the last line\n\nSays who wrote it.\n\nArgonauts-Run: "),
        "{message}"
    );
    Ok(())
}

/// `delivery.write = "files"` commits nothing, and a run that started from your uncommitted
/// edits still asks about, and writes, its own work alone.
#[tokio::test]
async fn writing_files_over_your_uncommitted_edits_counts_the_runs_work() -> anyhow::Result<()> {
    let mut crew = crew_over_your_edits()?;
    crew.settings.delivery.write = DeliveryWrite::Files;
    let yours = crew.repo.head()?;
    let before = status(&crew)?;
    let snapshot = snapshot_of(&crew)?;

    let outcome = run_from_snapshot(&crew, snapshot).await?;
    anyhow::ensure!(
        outcome.exit == EngineExit::Finished(RunOutcome::Merged),
        "{:?}: {:?}",
        outcome.exit,
        names(&outcome.events)
    );

    assert_eq!(crew.repo.head()?, yours, "your branch didn't move");
    let read = |file: &str| std::fs::read_to_string(crew.repo.root().join(file));
    assert_eq!(read("cli.txt")?, WITH_BOTH);
    assert_eq!(read("notes.md")?, "draft, still being written\n");
    assert_eq!(read("todo.md")?, "yours alone\n");
    assert_eq!(status(&crew)?, before);
    assert_eq!(counted(&outcome.events)?, (1, 1, 1));
    Ok(())
}

/// Only a commit can bring the run's work without an edit you took back while it worked.
/// Under `delivery.write = "files"` none is made, so that file is in the way and stays as
/// you left it until you say what to do.
#[tokio::test]
async fn writing_files_asks_about_an_edit_you_took_back() -> anyhow::Result<()> {
    let mut crew = crew_over_your_edits()?;
    crew.settings.delivery.write = DeliveryWrite::Files;
    let snapshot = snapshot_of(&crew)?;
    crew.repo.write("cli.txt", EIGHT_LINES)?;

    let waiting = crew
        .resume(vec![started_from(&crew, snapshot)?], Vec::new())
        .await?;
    let (merge, _reply) = recommended(&waiting.events).context("the merge question")?;
    let outcome = crew.resume(waiting.events, vec![merge]).await?;

    anyhow::ensure!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}: {:?}",
        outcome.exit,
        names(&outcome.events)
    );
    let in_the_way = outcome
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            RunEventKind::Merge(MergeEvent::WaitingToMerge { in_the_way, .. }) => {
                Some(in_the_way.clone())
            }
            _ => None,
        });
    assert_eq!(in_the_way, Some(vec!["cli.txt".to_owned()]));
    let yours = std::fs::read_to_string(crew.repo.root().join("cli.txt"))?;
    assert_eq!(yours, EIGHT_LINES);
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
    let fake = argonauts_testkit::runner::FakeRunner::cli("claude-code/opus", "anthropic")
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
    crew.answering_worker("codex/gpt-x", "openai");
    let outcome = run(&crew).await?;
    merged(&crew, &outcome)?;
    let main_work = crew.repo.git(&["show", "HEAD:changelog.md"])?;
    assert_eq!(main_work, "moved");
    nothing_of_argonauts(&crew)?;
    Ok(())
}

/// With your checkout detached, the run has no branch of its own to merge into, so it asks
/// once which branch takes it, and leaves your checkout where it is, files included.
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
    let merged = crew.repo.git(&["show", "main:verbose.txt"])?;
    assert_eq!(merged, "on");
    let asked: Vec<ApprovalOpened> = merge_questions(&outcome.events)
        .into_iter()
        .filter(|asked| asked.title == detached::TITLE)
        .collect();
    assert_eq!(asked.len(), 1, "asked once which branch takes the run");
    assert_eq!(asked[0].options[0].label, "main");
    assert_eq!(crew.repo.git(&["rev-parse", "HEAD"])?, before);
    assert!(crew.repo.git(&["symbolic-ref", "-q", "HEAD"]).is_err());
    assert!(!crew.repo.root().join("verbose.txt").exists());
    nothing_of_argonauts(&crew)?;
    Ok(())
}

/// A folder with no git of its own works through the history Argonauts Code keeps for it under its
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

    // The question asks to write the run into the folder, and offers nothing that needs a
    // branch of yours.
    let asked = merge_questions(&outcome.events);
    let asked = asked.first().context("a merge question")?;
    assert_eq!(asked.title, your_folder::TITLE);
    let labels: Vec<&str> = asked
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            your_folder::WRITE_IT,
            "Request changes on some tasks",
            "Abandon the run",
            "Show the diff"
        ]
    );
    // The run's copies went with it, and its last note names them.
    let context = crew.repo_context()?;
    assert!(
        !context.worktrees.exists(),
        "{}",
        context.worktrees.display()
    );
    let removed = notes(&outcome.events)
        .into_iter()
        .find(|note| note.starts_with(COPIES_REMOVED))
        .context("a note naming the copies removed")?;
    assert_eq!(removed, "the run's copies removed: base, 1 task, merge");
    Ok(())
}

/// The merge questions a run asked, oldest first.
fn merge_questions(events: &[RunEvent]) -> Vec<ApprovalOpened> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Merge =>
            {
                Some((**opened).clone())
            }
            _ => None,
        })
        .collect()
}

fn notes(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.clone()),
            _ => None,
        })
        .collect()
}

/// A crew in a folder whose `.git` became the history Argonauts Code keeps for it, with
/// `notes.txt` in the folder. Its worker adds a last line to that file in its task's copy,
/// and, as you would while a run works, `your_edit` is saved over the folder's own file.
fn crew_without_git(your_edit: &'static str) -> anyhow::Result<(Crew, std::path::PathBuf)> {
    let mut crew = Crew::new()?;
    crew.repo
        .write("notes.txt", "one\ntwo\nthree\nfour\nfive\n")?;
    crew.repo.git(&["add", "notes.txt"])?;
    crew.repo.git(&["commit", "--quiet", "-m", "notes"])?;
    let root = crew.repo.root().to_path_buf();
    let folder = root.clone();
    let worker = FakeRunner::cli("claude-code/opus", "anthropic").respond_with(move |spec| {
        if spec.step != Some(StepName::Build) {
            return smallest_answer(spec);
        }
        std::fs::write(
            spec.workdir.join("notes.txt"),
            "one\ntwo\nthree\nfour\nfive\nsix\n",
        )
        .expect("the build session writes its file");
        std::fs::write(folder.join("notes.txt"), your_edit).expect("your edit is saved");
        smallest_answer(spec).with_event(wrote("notes.txt"))
    });
    crew.enlist(worker);
    crew.answering_worker("codex/gpt-x", "openai");
    let hidden = argonauts_core::hidden_git::dir(crew.repo_context()?.state_dir());
    std::fs::create_dir_all(hidden.parent().context("a state folder")?)?;
    std::fs::rename(root.join(".git"), &hidden)?;
    Ok((crew, root))
}

/// Drives the run, answering each question with its recommended option, until it asks which
/// version of a file both changed goes in, and returns it waiting there.
async fn until_asked_for_a_version(crew: &Crew) -> anyhow::Result<(Outcome, ApprovalOpened)> {
    let mut last = crew.start(RunMode::Captain, REQUEST).await?;
    for _ in 0..8 {
        anyhow::ensure!(
            matches!(last.exit, EngineExit::Waiting(_)),
            "{:?}: {:?}",
            last.exit,
            names(&last.events)
        );
        let asked = merge_questions(&last.events)
            .pop()
            .filter(|asked| asked.title.starts_with("You and the run both changed"));
        if let Some(asked) = asked {
            return Ok((last, asked));
        }
        let (command, _reply) = recommended(&last.events).context("an open question")?;
        last = crew.resume(last.events.clone(), vec![command]).await?;
    }
    anyhow::bail!("never asked: {:?}", names(&last.events))
}

/// In a folder without git nothing can be committed or stashed, so an edit of yours to a file
/// the run also changed is merged with the run's: here in other lines, with nothing asked.
#[tokio::test]
async fn without_git_your_edit_in_other_lines_merges_with_the_runs() -> anyhow::Result<()> {
    let (crew, root) = crew_without_git("zero\none\ntwo\nthree\nfour\nfive\n")?;
    std::fs::write(root.join("mine.txt"), "untouched by the run\n")?;
    let outcome = run(&crew).await?;

    assert_eq!(
        outcome.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:?}",
        names(&outcome.events)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt"))?,
        "zero\none\ntwo\nthree\nfour\nfive\nsix\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("mine.txt"))?,
        "untouched by the run\n"
    );
    assert!(
        merge_questions(&outcome.events)
            .iter()
            .all(|asked| asked.title == your_folder::TITLE),
        "nothing was asked about your edit"
    );
    assert!(
        notes(&outcome.events)
            .iter()
            .any(|note| note.starts_with("You changed notes.txt while the run worked")),
        "{:?}",
        notes(&outcome.events)
    );
    assert!(!root.join(".git").exists());
    Ok(())
}

/// Where you and the run changed the same lines, you pick the version, and yours stays in
/// the history either way.
#[tokio::test]
async fn without_git_you_pick_the_version_of_a_file_both_changed() -> anyhow::Result<()> {
    for (pick, lands) in [
        (
            your_folder::KEEP_MINE,
            "one\ntwo\nthree\nfour\nfive\nmine\n",
        ),
        (
            your_folder::TAKE_THE_RUNS,
            "one\ntwo\nthree\nfour\nfive\nsix\n",
        ),
    ] {
        let (crew, root) = crew_without_git("one\ntwo\nthree\nfour\nfive\nmine\n")?;
        let (asked, question) = until_asked_for_a_version(&crew).await?;
        assert_eq!(
            question.title,
            "You and the run both changed notes.txt. Which version goes in?"
        );
        let labels: Vec<&str> = question
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                your_folder::KEEP_MINE,
                your_folder::TAKE_THE_RUNS,
                your_folder::SHOW_BOTH,
                "Merge later"
            ]
        );
        let ApprovalPacket::Merge(packet) = &question.packet else {
            anyhow::bail!("not a merge packet");
        };
        assert_eq!(packet.both_changed.len(), 1);
        assert_eq!(packet.both_changed[0].path, "notes.txt");
        // Nothing is written until you choose.
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt"))?,
            "one\ntwo\nthree\nfour\nfive\nmine\n"
        );

        let (reply, _replied) = tokio::sync::oneshot::channel();
        let answer = ApprovalAnswer {
            id: question.id.clone(),
            action: AnswerAction::Choose {
                value: pick.to_owned(),
            },
            text: None,
            evidence_sha256: question.evidence_sha256.clone(),
            tasks: Vec::new(),
        };
        let command = EngineCommand::Answer { answer, reply };
        let outcome = crew.resume(asked.events.clone(), vec![command]).await?;
        assert_eq!(
            outcome.exit,
            EngineExit::Finished(RunOutcome::Merged),
            "{pick}: {:?}",
            merge_questions(&outcome.events)
                .iter()
                .map(|asked| (asked.title.clone(), asked.packet.clone()))
                .next_back()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt"))?,
            lands,
            "{pick}"
        );
        // Your version is a commit of the history, whichever went in.
        let hidden = argonauts_core::hidden_git::dir(crew.repo_context()?.state_dir());
        let log = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&hidden)
            .args(["log", "--format=%s", "main"])
            .output()?;
        let log = String::from_utf8_lossy(&log.stdout).into_owned();
        assert!(
            log.contains("Your folder while a run waited to be written in"),
            "{pick}: {log}"
        );
    }
    Ok(())
}
