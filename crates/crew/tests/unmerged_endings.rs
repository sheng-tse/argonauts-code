//! A run that ends without merging: as a pull request, with its work kept on its branch, or
//! abandoned. What leaves this computer and what stays on the run's branch is the run's work
//! alone, a pull request counts only once one is open, and the words match what git holds.

mod support;

use argonauts_core::engine::{EngineCommand, EngineExit};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
};
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::task::MergeEvent;
use argonauts_core::events::{RunEvent, RunEventKind};

use support::{Crew, Gh, Outcome, answer, names, recommended};

const REQUEST: &str = "add a --verbose flag";
const EIGHT_LINES: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";
const WITH_YOUR_EDIT: &str = "a, yours\nb\nc\nd\ne\nf\ng\nh\n";
const WITH_BOTH: &str = "a, yours\nb\nc\nd\ne\nf\ng\nh, by the run\n";
const THE_RUNS_ALONE: &str = "a\nb\nc\nd\ne\nf\ng\nh, by the run";

/// A crew whose worker writes `verbose.txt` in its task, and a reviewer of another provider.
fn crew() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.building_worker("claude-code/opus", "anthropic", "verbose.txt", "on\n");
    crew.answering_worker("codex/gpt-x", "openai");
    Ok(crew)
}

/// A crew whose worker changes the last line of `cli.txt`, in a repository where you have
/// edited its first line, edited `notes.md` and added `todo.md`, all uncommitted.
fn crew_over_your_edits() -> anyhow::Result<Crew> {
    let mut crew = Crew::new()?;
    crew.building_worker("claude-code/opus", "anthropic", "cli.txt", WITH_BOTH);
    crew.answering_worker("codex/gpt-x", "openai");
    crew.repo.write("cli.txt", EIGHT_LINES)?;
    crew.repo.write("notes.md", "draft\n")?;
    crew.repo.commit_all("files")?;
    Ok(crew)
}

fn edit_without_committing(crew: &Crew) -> anyhow::Result<()> {
    crew.repo.write("cli.txt", WITH_YOUR_EDIT)?;
    crew.repo
        .write("notes.md", "draft, still being written\n")?;
    crew.repo.write("todo.md", "yours alone\n")?;
    Ok(())
}

/// Your checkout as a commit on top of its head that no branch points at, with the checkout
/// and its index left as they were: what a run starts from under `intake.include_uncommitted`.
fn snapshot_of(crew: &Crew) -> anyhow::Result<String> {
    let head = crew.repo.head()?;
    crew.repo.git(&["add", "--all"])?;
    crew.repo
        .git(&["commit", "-qm", "your files as the run starts"])?;
    let snapshot = crew.repo.head()?;
    crew.repo.git(&["reset", "-q", "--mixed", &head])?;
    Ok(snapshot)
}

fn started(crew: &Crew, snapshot: Option<String>) -> anyhow::Result<RunEvent> {
    let mut first = crew.run_started(RunMode::Captain, REQUEST)?;
    if let (Some(snapshot), RunEventKind::Run(RunLifecycle::Started(started))) =
        (snapshot, &mut first.kind)
    {
        started.base_commit = snapshot;
        started.uncommitted_included = true;
    }
    Ok(first)
}

/// Drives the run to its merge question, answering what comes before with the recommended
/// option, then answers that question with each of `answers` in turn, one each time it opens.
async fn answering_the_merge(
    crew: &Crew,
    first: RunEvent,
    answers: &[AnswerAction],
) -> anyhow::Result<Outcome> {
    let mut outcome = crew.resume(vec![first], Vec::new()).await?;
    let mut answers = answers.iter();
    for _ in 0..10 {
        if !matches!(outcome.exit, EngineExit::Waiting(_)) {
            break;
        }
        let command: EngineCommand = match answer(
            &outcome.events,
            ApprovalKind::Merge,
            AnswerAction::Merge,
            None,
        ) {
            Ok(_) => {
                let Some(action) = answers.next() else {
                    break;
                };
                answer(&outcome.events, ApprovalKind::Merge, action.clone(), None)?.0
            }
            Err(_) => match recommended(&outcome.events) {
                Some((command, _)) => command,
                None => break,
            },
        };
        outcome = crew.resume(outcome.events.clone(), vec![command]).await?;
    }
    Ok(outcome)
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

fn pull_requests(events: &[RunEvent]) -> Vec<Option<String>> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Merge(MergeEvent::PullRequest { url, .. }) => Some(url.clone()),
            _ => None,
        })
        .collect()
}

/// The run's branches still in the repository.
fn run_branches(crew: &Crew) -> anyhow::Result<Vec<String>> {
    let listed = crew.repo.git(&[
        "for-each-ref",
        "--format=%(refname:short)",
        "refs/heads/argonauts/",
    ])?;
    Ok(listed.lines().map(str::to_owned).collect())
}

fn status(crew: &Crew) -> anyhow::Result<Vec<String>> {
    let status = crew.repo.git(&["status", "--porcelain"])?;
    Ok(status.lines().map(|line| line.trim().to_owned()).collect())
}

fn ended(outcome: &Outcome, with: RunOutcome) -> anyhow::Result<()> {
    anyhow::ensure!(
        outcome.exit == EngineExit::Finished(with),
        "{:?}: {:?}\n{:#?}",
        outcome.exit,
        names(&outcome.events),
        notes(&outcome.events)
    );
    Ok(())
}

/// What `branch` holds, with nothing of what you hadn't committed: one commit on your branch's
/// tip, of the run's line alone.
fn holds_the_runs_work_alone(crew: &Crew, git_dir: &[&str], branch: &str, yours: &str) {
    let git = |args: &[&str]| {
        let all: Vec<&str> = git_dir.iter().chain(args).copied().collect();
        crew.repo.git(&all).expect("git reads the branch")
    };
    assert_eq!(git(&["rev-parse", &format!("{branch}^")]), yours);
    let subjects = git(&["log", "--format=%s", branch]);
    assert!(
        !subjects.contains("your files as the run starts"),
        "{subjects}"
    );
    assert_eq!(
        git(&["show", "--format=", "--name-only", branch]),
        "cli.txt"
    );
    assert_eq!(git(&["show", &format!("{branch}:cli.txt")]), THE_RUNS_ALONE);
    assert_eq!(git(&["show", &format!("{branch}:notes.md")]), "draft");
    let files = git(&["ls-tree", "-r", "--name-only", branch]);
    assert!(!files.lines().any(|file| file == "todo.md"), "{files}");
}

/// A run that started from your uncommitted edits goes up as a pull request of its own work:
/// the remote's branch holds neither your edits nor the snapshot the run grew from, and the
/// branch kept here is that same commit.
#[tokio::test]
async fn a_pull_request_never_carries_what_you_had_not_committed() -> anyhow::Result<()> {
    let mut crew = crew_over_your_edits()?;
    let remote = crew.with_remote()?;
    let gh = crew.with_tools(Some(Gh::Opens("https://example.invalid/you/repo/pull/7")))?;
    edit_without_committing(&crew)?;
    let yours = crew.repo.head()?;
    let before = status(&crew)?;
    let snapshot = snapshot_of(&crew)?;

    let first = started(&crew, Some(snapshot.clone()))?;
    let outcome = answering_the_merge(&crew, first, &[AnswerAction::PullRequest]).await?;
    ended(&outcome, RunOutcome::PullRequest)?;
    assert_eq!(
        pull_requests(&outcome.events),
        [Some("https://example.invalid/you/repo/pull/7".to_owned())]
    );

    let kept = run_branches(&crew)?;
    let [branch] = kept.as_slice() else {
        anyhow::bail!("one branch is kept: {kept:?}");
    };
    let remote = remote.to_string_lossy().into_owned();
    let on_the_remote = ["--git-dir", remote.as_str()];
    holds_the_runs_work_alone(&crew, &on_the_remote, branch, &yours);
    holds_the_runs_work_alone(&crew, &[], branch, &yours);
    let pushed = crew
        .repo
        .git(&["--git-dir", &remote, "rev-parse", branch])?;
    assert_eq!(crew.repo.git(&["rev-parse", branch])?, pushed);
    // The snapshot is on no branch, here or there.
    let reaching = crew
        .repo
        .git(&["branch", "--all", "--contains", &snapshot])?;
    assert_eq!(reaching, "", "branches that reach the snapshot");

    // `gh` was given the commit's own words, since the branch's history here isn't pushed.
    let asked = std::fs::read_to_string(gh)?;
    assert!(asked.lines().any(|arg| arg == "--body"), "{asked}");
    assert!(!asked.lines().any(|arg| arg == "--fill"), "{asked}");

    // Your files are as you left them.
    assert_eq!(status(&crew)?, before);
    let read = |file: &str| std::fs::read_to_string(crew.repo.root().join(file));
    assert_eq!(read("cli.txt")?, WITH_YOUR_EDIT);
    assert_eq!(read("todo.md")?, "yours alone\n");
    Ok(())
}

/// Kept on its branch, such a run leaves the same single commit, and none of its inner
/// branches, which grew from the snapshot.
#[tokio::test]
async fn a_kept_branch_never_carries_what_you_had_not_committed() -> anyhow::Result<()> {
    let crew = crew_over_your_edits()?;
    edit_without_committing(&crew)?;
    let yours = crew.repo.head()?;
    let before = status(&crew)?;
    let snapshot = snapshot_of(&crew)?;

    let first = started(&crew, Some(snapshot.clone()))?;
    let outcome = answering_the_merge(&crew, first, &[AnswerAction::KeepBranch]).await?;
    ended(&outcome, RunOutcome::BranchLeft)?;

    let kept = run_branches(&crew)?;
    let [branch] = kept.as_slice() else {
        anyhow::bail!("one branch is kept: {kept:?}");
    };
    holds_the_runs_work_alone(&crew, &[], branch, &yours);
    let reaching = crew
        .repo
        .git(&["branch", "--all", "--contains", &snapshot])?;
    assert_eq!(reaching, "", "branches that reach the snapshot");
    assert_eq!(status(&crew)?, before);
    Ok(())
}

/// A run that started from a commit of yours keeps its branches as they are.
#[tokio::test]
async fn a_kept_branch_keeps_the_runs_own_history() -> anyhow::Result<()> {
    let crew = crew()?;
    let first = started(&crew, None)?;
    let outcome = answering_the_merge(&crew, first, &[AnswerAction::KeepBranch]).await?;
    ended(&outcome, RunOutcome::BranchLeft)?;
    let kept = run_branches(&crew)?;
    assert!(!kept.is_empty(), "the run's branches stay");
    let on_it = crew
        .repo
        .git(&["show", &format!("{}:verbose.txt", kept[0])])?;
    assert_eq!(on_it, "on");
    Ok(())
}

/// With no remote there is nowhere to push: the transcript says so, the question opens again
/// with the reason on it, and the run hasn't ended as a pull request.
#[tokio::test]
async fn a_pull_request_with_no_remote_is_said_and_asked_again() -> anyhow::Result<()> {
    let mut crew = crew()?;
    crew.with_tools(Some(Gh::Opens("https://example.invalid/pull/1")))?;
    let first = started(&crew, None)?;
    let outcome = answering_the_merge(&crew, first, &[AnswerAction::PullRequest]).await?;
    anyhow::ensure!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert!(pull_requests(&outcome.events).is_empty());
    let said = notes(&outcome.events);
    assert!(
        said.iter()
            .any(|note| note
                .starts_with("No pull request was opened: this repository has no remote")),
        "{said:#?}"
    );
    let asked = merge_questions(&outcome.events);
    let ApprovalPacket::Merge(again) = &asked[asked.len() - 1].packet else {
        anyhow::bail!("the merge question opened again");
    };
    assert_eq!(asked.len(), 2);
    let why = again.again.as_deref().unwrap_or_default();
    assert!(why.starts_with("this repository has no remote"), "{why}");

    // Keeping the branch then ends the run with its work there.
    let (keep, _) = answer(
        &outcome.events,
        ApprovalKind::Merge,
        AnswerAction::KeepBranch,
        None,
    )?;
    let outcome = crew.resume(outcome.events.clone(), vec![keep]).await?;
    ended(&outcome, RunOutcome::BranchLeft)
}

/// `gh` failing after the push says what it said and where the branch is, and asks again.
#[tokio::test]
async fn a_pull_request_gh_could_not_open_is_said_and_asked_again() -> anyhow::Result<()> {
    let mut crew = crew()?;
    let remote = crew.with_remote()?;
    crew.with_tools(Some(Gh::Fails("pull request create failed: no commits")))?;
    let first = started(&crew, None)?;
    let outcome = answering_the_merge(&crew, first, &[AnswerAction::PullRequest]).await?;
    anyhow::ensure!(
        matches!(outcome.exit, EngineExit::Waiting(_)),
        "{:?}",
        outcome.exit
    );
    assert!(pull_requests(&outcome.events).is_empty());
    let said = notes(&outcome.events);
    let line = said
        .iter()
        .find(|note| note.starts_with("No pull request was opened: "))
        .unwrap_or_else(|| panic!("{said:#?}"));
    assert!(line.contains("gh pr create failed: pull request create failed: no commits"));
    assert!(line.contains("is pushed to origin"), "{line}");
    let remote = remote.to_string_lossy().into_owned();
    let pushed = crew
        .repo
        .git(&["--git-dir", &remote, "branch", "--list", "argonauts/*"])?;
    assert!(!pushed.is_empty(), "the branch is on the remote");
    Ok(())
}

/// Without `gh` the branch is pushed and the pull request is yours to open, so the run ends
/// with its work on its branch and never as a pull request.
#[tokio::test]
async fn without_gh_the_pushed_branch_is_left_for_you() -> anyhow::Result<()> {
    let mut crew = crew()?;
    let remote = crew.with_remote()?;
    crew.with_tools(None)?;
    let first = started(&crew, None)?;
    let outcome = answering_the_merge(&crew, first, &[AnswerAction::PullRequest]).await?;
    ended(&outcome, RunOutcome::BranchLeft)?;
    assert!(pull_requests(&outcome.events).is_empty());
    let said = notes(&outcome.events);
    assert!(
        said.iter()
            .any(|note| note.contains("gh isn't installed, so the pull request is yours to open")),
        "{said:#?}"
    );
    let remote = remote.to_string_lossy().into_owned();
    let pushed = crew
        .repo
        .git(&["--git-dir", &remote, "branch", "--list", "argonauts/*"])?;
    assert!(!pushed.is_empty(), "the branch is on the remote");
    Ok(())
}

/// An abandoned run's branches go with it, as its last line says.
#[tokio::test]
async fn an_abandoned_run_leaves_no_branch() -> anyhow::Result<()> {
    let crew = crew()?;
    let first = started(&crew, None)?;
    let outcome = answering_the_merge(&crew, first, &[AnswerAction::Abandon]).await?;
    ended(&outcome, RunOutcome::Abandoned)?;
    assert_eq!(run_branches(&crew)?, Vec::<String>::new());
    assert!(!crew.repo.root().join("verbose.txt").exists());
    Ok(())
}
