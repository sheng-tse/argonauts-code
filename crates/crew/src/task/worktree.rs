//! A task's worktree (`~/.argonauts/worktrees/<repo>/<run>/<task>`, branch
//! `argonauts/<run>/<task>`, from the run branch), and the commit after every session
//! end.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use globset::Glob;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::InputPart;
use argonauts_core::events::task::TaskEvent;
use argonauts_core::layout;
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::{CommitInfo, DiffStat, Note, NoteLevel, RunId, TaskId};

use crate::engine::host::Host;
use crate::engine::shared::lock;
use crate::error::{EngineError, EngineResult};
use crate::fallback::notes;
use crate::git::{Git, LeftOut, LeftOutWhy};
use crate::idempotency;
use crate::merge::branch::{ensure_run_branch, run_branch, task_branch, try_branch};
use crate::merge::conductor;
use crate::task::claims;
use crate::task::prompt;
use crate::task::support::{record_of, record_task, record_task_in};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTree {
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
}

/// Creates the worktree, or finds the one the task already has.
pub async fn ensure(host: &Host, task: &TaskId) -> EngineResult<TaskTree> {
    let record = record_of(host, task)?;
    let branch = record
        .branch
        .clone()
        .unwrap_or_else(|| task_branch(host.run_id(), task));
    let path = record
        .worktree
        .clone()
        .unwrap_or_else(|| host.repo().worktrees.join(task.as_str()));
    let base = match &record.base {
        Some(base) => base.clone(),
        None => run_head(host).await?,
    };
    let tree = TaskTree { path, branch, base };
    let fresh = make_tree(host, &tree, &tree.base).await?;
    let recorded = record.worktree.as_ref() == Some(&tree.path)
        && record.branch.as_ref() == Some(&tree.branch)
        && record.base.as_ref() == Some(&tree.base);
    if !recorded {
        let event = TaskEvent::Worktree {
            path: tree.path.clone(),
            branch: tree.branch.clone(),
            base: tree.base.clone(),
        };
        record_task(host, task, event)?;
    }
    if fresh {
        copy_untracked(host, &tree.path).await?;
    }
    Ok(tree)
}

/// A separate worktree and branch for one try at a task, such as a race candidate's
/// (`argonauts/<run>/<task>-try-<n>`). Its diffs are against the task's base; its branch
/// starts at the task's committed work, which is the base itself until the task has commits.
pub async fn try_tree(host: &Host, task: &TaskId, try_number: u32) -> EngineResult<TaskTree> {
    let own = ensure(host, task).await?;
    let start = host
        .git()
        .branch_head(&host.repo().root, &own.branch)
        .await?
        .unwrap_or_else(|| own.base.clone());
    let tree = TaskTree {
        path: try_path(host, task, try_number),
        branch: try_branch(host.run_id(), task, try_number),
        base: own.base,
    };
    if make_tree(host, &tree, &start).await? {
        copy_untracked(host, &tree.path).await?;
    }
    Ok(tree)
}

/// Whether a try tree or branch with this number already exists, so a new try must
/// take another.
pub async fn try_taken(host: &Host, task: &TaskId, try_number: u32) -> EngineResult<bool> {
    let path = try_path(host, task, try_number);
    if path.exists() {
        return Ok(true);
    }
    let branch = try_branch(host.run_id(), task, try_number);
    Ok(host
        .git()
        .branch_head(&host.repo().root, &branch)
        .await?
        .is_some())
}

/// The try number of one of this task's try branches, from its name.
pub fn try_of_branch(run: &RunId, task: &TaskId, branch: &str) -> Option<u32> {
    let prefix = format!("{}-try-", task_branch(run, task));
    branch.strip_prefix(&prefix)?.parse().ok()
}

/// The try number of one of this task's try trees, from its name.
pub fn try_of_tree(task: &TaskId, tree: &Path) -> Option<u32> {
    let name = tree.file_name()?.to_str()?;
    name.strip_prefix(&format!("{task}-try-"))?.parse().ok()
}

fn try_path(host: &Host, task: &TaskId, try_number: u32) -> PathBuf {
    host.repo()
        .worktrees
        .join(format!("{task}-try-{try_number}"))
}

/// `git add -A` and a commit with the session key in its message, hooks off; records
/// `Task::Committed` and `Task::Diff`. Nothing happens when that commit already exists.
///
/// A merge the session left open is not finished here: it waits for the session to resume,
/// since finishing it would commit whatever conflict markers are still in it.
pub async fn commit_after_end(
    host: &Host,
    task: &TaskId,
    session: &SessionKey,
) -> EngineResult<Option<CommitInfo>> {
    let tree = ensure(host, task).await?;
    if tree_git(host, &tree.path)
        .await?
        .merge_in_progress(&tree.path)
        .await?
    {
        return Ok(None);
    }
    let worker = host.read(|state| {
        state
            .sessions
            .get(session)
            .map(|record| record.started.worker.clone())
    });
    commit_in(host, task, &tree, Some(session), worker).await
}

/// Commits everything in `tree` for `session` (or, with no session, as your own work) and records
/// it. Commits the session's agent made itself come first: each is recorded as its work, and only
/// what it left uncommitted goes in Argonauts Code's commit. That commit also takes back out the
/// new files the agent committed without meaning to create them. Every new commit gets its author's
/// note. A commit already made for the session's current start is recorded if the log missed it,
/// and never made twice. A start that changed nothing is recorded too, with no commit, so a replay
/// finds its end commit done in the log and doesn't ask git again. A merge with conflict markers
/// left in it stays open, and the checks report it.
///
/// Returns the newest commit recorded here.
pub(crate) async fn commit_in(
    host: &Host,
    task: &TaskId,
    tree: &TaskTree,
    session: Option<&SessionKey>,
    by: Option<ModelRef>,
) -> EngineResult<Option<CommitInfo>> {
    let git = tree_git(host, &tree.path).await?;
    let session = session.map(|key| {
        let start = host.read(|state| state.sessions.get(key).map_or(1, |record| record.starts));
        (key, start)
    });
    // A resumed session can change files after its last commit, so a clean tree is what
    // shows there's nothing left to commit.
    if let Some((key, start)) = session
        && commit_recorded(host, task, key, start)?
        && git.is_clean(&tree.path).await?
    {
        return Ok(None);
    }
    let range = format!("{}..HEAD", tree.base);
    let mut tree = tree.clone();
    let mut made = Vec::new();
    let mut own = Vec::new();
    if by.is_some() {
        let key = session.map(|(key, _)| key);
        for sha in agents_commits(host, task, &git, &tree.path, &range, session).await? {
            let (info, moved) =
                record_commit(host, task, &git, &tree, &sha, key, by.as_ref()).await?;
            tree = moved;
            made.push(info);
            own.push(sha);
        }
    }
    let existing = match session {
        Some((key, start)) => {
            idempotency::commit_for_session(&git, &tree.path, &range, key, start).await?
        }
        None => None,
    };
    let leftover = match existing {
        Some(sha) if record_of(host, task)?.has_commit(&sha) => None,
        Some(sha) => Some(sha),
        None => {
            let message = commit_message(host.run_id(), task, session, by.is_some());
            // An agent's commit takes the new files it meant to create. What you left in the
            // worktree yourself, with no agent named, is all yours and all committed.
            let claims = by
                .as_ref()
                .and_then(|_| claims::of_task(host, task, &tree.path));
            let committed = if git.merge_in_progress(&tree.path).await? {
                if !git.leftover_markers(&tree.path).await?.is_empty() {
                    return finish(host, task, &git, &tree, made).await;
                }
                let (sha, left_out) = git
                    .finish_merge_work(&tree.path, &message, claims.as_ref())
                    .await?;
                tell_left_out(host, task, &left_out)?;
                Some(sha)
            } else {
                // What the agent's own commits swept in is sorted like what it left behind,
                // so the branch holds the same files whoever ran `git commit`.
                git.unstage_left_out(&tree.path, &own, claims.as_ref())
                    .await?;
                let committed = git
                    .commit_claimed(&tree.path, &message, claims.as_ref())
                    .await?;
                tell_left_out(host, task, &committed.left_out)?;
                committed.sha
            };
            if let (None, Some((key, _))) = (&committed, session) {
                let nothing = CommitInfo {
                    branch: tree.branch.clone(),
                    sha: None,
                    message,
                    stat: DiffStat::default(),
                    by: by.clone(),
                };
                record_task_in(host, task, key, TaskEvent::Committed(nothing))?;
            }
            committed
        }
    };
    if let Some(sha) = leftover {
        let key = session.map(|(key, _)| key);
        let (info, moved) = record_commit(host, task, &git, &tree, &sha, key, by.as_ref()).await?;
        tree = moved;
        made.push(info);
    }
    finish(host, task, &git, &tree, made).await
}

/// The most a note names before it says how many more.
const LEFT_OUT_NAMED: usize = 6;

/// Names what a commit of `task` left out, each folder or file once per task, so a file left
/// out by mistake shows while it's still in the worktree, before the merge. Otherwise those
/// files would go missing without a word.
fn tell_left_out(host: &Host, task: &TaskId, left_out: &[LeftOut]) -> EngineResult<()> {
    for why in [
        LeftOutWhy::BuildOutput,
        LeftOutWhy::Unclaimed,
        LeftOutWhy::TooBig,
        LeftOutWhy::Ignored,
    ] {
        let new: Vec<&str> = {
            let mut told = lock(&host.shared().told_left_out);
            left_out
                .iter()
                .filter(|left| left.why == why)
                .filter(|left| told.insert(format!("{task}:{}", left.because)))
                .map(|left| left.because.as_str())
                .collect()
        };
        if new.is_empty() {
            continue;
        }
        let shown = new.len().min(LEFT_OUT_NAMED);
        let mut named = new[..shown].join(", ");
        if new.len() > shown {
            named.push_str(&format!(" and {} more", new.len() - shown));
        }
        let text = match why {
            LeftOutWhy::BuildOutput => format!(
                "Left out of {task}'s commit as build and cache output, and kept in its \
                 worktree: {named}. merge.leave_out and merge.leave_out_builtin set what's \
                 left out."
            ),
            LeftOutWhy::Unclaimed => format!(
                "Left out of {task}'s commit, and kept in its worktree: {named}. No edit tool \
                 wrote them and no report lists them, so they read as something a command \
                 left behind. commit.unclaimed = \"keep\" commits every new file."
            ),
            LeftOutWhy::TooBig => format!(
                "Left out of {task}'s commit as bigger than commit.max_new_file_mb allows, \
                 and kept in its worktree: {named}."
            ),
            LeftOutWhy::Ignored => format!(
                "Left out of {task}'s commit because the repository's ignore rules match \
                 them, and kept in its worktree: {named}. A file that belongs in the change \
                 needs another name, or a change to the rule."
            ),
        };
        host.record(
            Some(task),
            None,
            RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text,
            }),
        )?;
    }
    Ok(())
}

/// What to tell `task`'s worker when checks fail: the new files in its worktree that its
/// commit left out. The checks ran on the commit without them, so one of them may be what's
/// missing. `None` when nothing was left out.
pub(crate) async fn left_out_part(host: &Host, task: &TaskId) -> EngineResult<Option<InputPart>> {
    let tree = ensure(host, task).await?;
    let Some(claims) = claims::of_task(host, task, &tree.path) else {
        return Ok(None);
    };
    let git = tree_git(host, &tree.path).await?;
    let mut text = Vec::new();
    if let Some(named) = named(&git.unclaimed_files(&tree.path, &claims).await?) {
        text.push(format!(
            "These new files in your worktree are not in your commit, because no edit tool \
             wrote them and your report didn't list them: {named}. If the change needs one of \
             them, list it in your report's files_changed. Leave caches, build output and \
             test leftovers out."
        ));
    }
    if let Some(named) = named(&git.claimed_but_ignored(&tree.path, &claims).await?) {
        text.push(format!(
            "These files you meant to create are not in your commit, because the \
             repository's ignore rules match them: {named}. If the change needs one, give it \
             another name, or change the rule in an edit of the .gitignore file."
        ));
    }
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(prompt::message_part(
        "left out of your commit",
        text.join("\n\n"),
    )))
}

/// The first few of what was left out, then how many more.
fn named(left_out: &[LeftOut]) -> Option<String> {
    let mut named: Vec<&str> = left_out.iter().map(|left| left.because.as_str()).collect();
    named.dedup();
    if named.is_empty() {
        return None;
    }
    let more = named.len().saturating_sub(LEFT_OUT_NAMED);
    named.truncate(LEFT_OUT_NAMED);
    let mut text = named.join(", ");
    if more > 0 {
        text.push_str(&format!(" and {more} more"));
    }
    Some(text)
}

/// The commits on the branch's own line in `range` that the log hasn't recorded, oldest
/// first: the agent's own. One holding this start's session lines is Argonauts Code's commit
/// for the session, recorded as that.
async fn agents_commits(
    host: &Host,
    task: &TaskId,
    git: &Git,
    dir: &Path,
    range: &str,
    session: Option<(&SessionKey, u32)>,
) -> EngineResult<Vec<String>> {
    let record = record_of(host, task)?;
    let mut shas: Vec<String> = git
        .first_parent_log(dir, range)
        .await?
        .into_iter()
        .filter(|(sha, message)| {
            let ours =
                session.is_some_and(|(key, start)| idempotency::holds_start(message, key, start));
            !record.has_commit(sha) && !ours
        })
        .map(|(sha, _)| sha)
        .collect();
    shas.reverse();
    Ok(shas)
}

/// Records `sha` as `by`'s work in the session, or as your own with no session, and moves the
/// tree's base past a merge it holds.
async fn record_commit(
    host: &Host,
    task: &TaskId,
    git: &Git,
    tree: &TaskTree,
    sha: &str,
    session: Option<&SessionKey>,
    by: Option<&ModelRef>,
) -> EngineResult<(CommitInfo, TaskTree)> {
    let message = git
        .log_messages(&tree.path, &format!("{sha}^!"))
        .await?
        .into_iter()
        .next()
        .map(|(_, message)| message)
        .unwrap_or_default();
    let parents = git.parents(&tree.path, sha).await?;
    let stat = match parents.first() {
        Some(parent) => git.diff_stat(&tree.path, parent, sha).await?,
        None => Default::default(),
    };
    let info = CommitInfo {
        branch: tree.branch.clone(),
        sha: Some(sha.to_owned()),
        message,
        stat,
        by: by.cloned(),
    };
    let committed = TaskEvent::Committed(info.clone());
    match session {
        Some(key) => record_task_in(host, task, key, committed)?,
        None => record_task(host, task, committed)?,
    };
    let tree = rebase_on_merge(host, task, git, tree, &parents).await?;
    Ok((info, tree))
}

/// Notes each new commit's author, saves the task's diff up to the newest, and returns it.
async fn finish(
    host: &Host,
    task: &TaskId,
    git: &Git,
    tree: &TaskTree,
    mut made: Vec<CommitInfo>,
) -> EngineResult<Option<CommitInfo>> {
    let root = host.repo().root.clone();
    for info in &made {
        if let (Some(sha), Some(author)) = (&info.sha, &info.by) {
            notes::record_author(host, &root, sha, author).await?;
        }
    }
    let Some(newest) = made.pop() else {
        return Ok(None);
    };
    if let Some(sha) = &newest.sha {
        save_diff(host, task, git, tree, sha).await?;
    }
    Ok(Some(newest))
}

/// After the run branch is merged into the task's own branch, the merge head
/// it brought in becomes the task's base, so the task's diffs stop counting other tasks'
/// merged work as its own.
async fn rebase_on_merge(
    host: &Host,
    task: &TaskId,
    git: &Git,
    tree: &TaskTree,
    parents: &[String],
) -> EngineResult<TaskTree> {
    let [_, merged] = parents else {
        return Ok(tree.clone());
    };
    let own = record_of(host, task)?.branch.as_ref() == Some(&tree.branch);
    if !own || *merged == tree.base {
        return Ok(tree.clone());
    }
    let run_branch = run_branch(host.run_id());
    let Some(head) = git.branch_head(&tree.path, &run_branch).await? else {
        return Ok(tree.clone());
    };
    let forward = git.is_ancestor(&tree.path, merged, &head).await?
        && git.is_ancestor(&tree.path, &tree.base, merged).await?;
    if !forward {
        return Ok(tree.clone());
    }
    let moved = TaskTree {
        base: merged.clone(),
        ..tree.clone()
    };
    let event = TaskEvent::Worktree {
        path: moved.path.clone(),
        branch: moved.branch.clone(),
        base: moved.base.clone(),
    };
    record_task(host, task, event)?;
    Ok(moved)
}

/// The engine's own commits on the task's branches: your changes after a take-over and the
/// merges it made cleanly. Matched by SHA, never by message, since an agent can
/// write any message.
pub(crate) fn conductor_commits(host: &Host, task: &TaskId) -> EngineResult<BTreeSet<String>> {
    Ok(record_of(host, task)?
        .commits
        .iter()
        .filter(|commit| commit.by.is_none())
        .filter_map(|commit| commit.sha.clone())
        .collect())
}

/// Whether the log holds a `Task::Committed` for this start of `session`, with a commit or
/// with nothing to commit.
fn commit_recorded(
    host: &Host,
    task: &TaskId,
    session: &SessionKey,
    start: u32,
) -> EngineResult<bool> {
    Ok(record_of(host, task)?
        .commits
        .iter()
        .any(|commit| idempotency::holds_start(&commit.message, session, start)))
}
/// Saves the task's whole diff against its base and records `Task::Diff`.
pub(crate) async fn save_diff(
    host: &Host,
    task: &TaskId,
    git: &Git,
    tree: &TaskTree,
    head: &str,
) -> EngineResult<()> {
    let text = git.diff(&tree.path, &tree.base, head).await?;
    let stat = git.diff_stat(&tree.path, &tree.base, head).await?;
    let dir = layout::task_dir(&host.repo().run_dir, task).join("diffs");
    create_private_dir_all(&dir)?;
    let short: String = head.chars().take(12).collect();
    let path = dir.join(format!("{short}.diff"));
    write_private_file(&path, text.as_bytes())?;
    record_task(host, task, TaskEvent::Diff { stat, path })?;
    Ok(())
}

/// Git bound to the worktree's own administrative directory, read from the main repository,
/// so a `.git` file the agent rewrote can't point our commands at another repository.
pub(crate) async fn tree_git(host: &Host, tree: &Path) -> EngineResult<Git> {
    let git = host.git();
    match git.worktree_git_dir(&host.repo().root, tree).await? {
        Some(admin) => Ok(git.bound_to(admin, tree.to_path_buf())),
        None => Err(EngineError::Refused(format!(
            "{} is not a worktree of this repository",
            tree.display()
        ))),
    }
}

/// An agent's session commit names the session, and its start after the first, so a resumed
/// session commits again. It never names the agent or its CLI: a check that reads the history
/// must not learn who made the change. Only your own changes after a take-over carry Argonauts
/// Code's trailer.
fn commit_message(
    run: &RunId,
    task: &TaskId,
    session: Option<(&SessionKey, u32)>,
    an_agents: bool,
) -> String {
    match (session, an_agents) {
        (Some((session, start)), _) => format!(
            "{task}: work from {session}\n\n{}",
            idempotency::session_lines(session, start)
        ),
        (None, true) => format!("{task}: work an agent left"),
        (None, false) => conductor::message(run, &format!("{task}: your changes")),
    }
}

/// The run branch's head, where new task worktrees start.
async fn run_head(host: &Host) -> EngineResult<String> {
    ensure_run_branch(host).await?;
    let branch = run_branch(host.run_id());
    host.git()
        .branch_head(&host.repo().root, &branch)
        .await?
        .ok_or_else(|| EngineError::Refused(format!("{branch} has no commits")))
}

/// Adds the worktree when it's missing. True when it was just made.
async fn make_tree(host: &Host, tree: &TaskTree, start: &str) -> EngineResult<bool> {
    if tree.path.join(".git").exists() {
        return Ok(false);
    }
    if let Some(parent) = tree.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let git = host.git();
    let root = &host.repo().root;
    if git.branch_head(root, &tree.branch).await?.is_some() {
        git.add_worktree_on(root, &tree.path, &tree.branch).await?;
    } else {
        git.add_worktree(root, &tree.path, &tree.branch, start)
            .await?;
    }
    Ok(true)
}

/// Copies the untracked files `checks.copy_untracked` names from your checkout into a new
/// worktree. Nothing else untracked is copied, and nothing already there is overwritten.
async fn copy_untracked(host: &Host, tree: &Path) -> EngineResult<()> {
    let patterns = &host.settings().checks.copy_untracked;
    if patterns.is_empty() {
        return Ok(());
    }
    let root = &host.repo().root;
    let untracked = host.git().untracked_files(root).await?;
    for rel in files_to_copy(patterns, &untracked, root) {
        let (from, to) = (root.join(&rel), tree.join(&rel));
        if to.exists() || !from.is_file() {
            continue;
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&from, &to)?;
    }
    Ok(())
}

/// Relative paths inside the repository that `patterns` pick: a plain path names itself (so an
/// ignored file such as `.env.test` can be named), and a glob matches untracked files.
fn files_to_copy(patterns: &[String], untracked: &[String], root: &Path) -> Vec<String> {
    let mut picked = Vec::new();
    for pattern in patterns {
        let is_glob = pattern.contains(['*', '?', '[', '{']);
        if !is_glob {
            if is_inside(pattern) && root.join(pattern).is_file() {
                picked.push(pattern.clone());
            }
            continue;
        }
        let Ok(glob) = Glob::new(pattern) else {
            continue;
        };
        let matcher = glob.compile_matcher();
        picked.extend(
            untracked
                .iter()
                .filter(|path| is_inside(path) && matcher.is_match(path.as_str()))
                .cloned(),
        );
    }
    picked.sort();
    picked.dedup();
    picked
}

/// A relative path that stays inside the repository.
fn is_inside(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_untracked_takes_plain_paths_and_globs_inside_the_repo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".env.test"), "A=1").unwrap();
        let patterns = vec![
            ".env.test".to_owned(),
            "fixtures/*.json".to_owned(),
            "../outside".to_owned(),
            "missing.txt".to_owned(),
        ];
        let untracked = vec![
            "fixtures/a.json".to_owned(),
            "fixtures/b.txt".to_owned(),
            "../escape.json".to_owned(),
        ];
        assert_eq!(
            files_to_copy(&patterns, &untracked, root),
            [".env.test", "fixtures/a.json"]
        );
    }

    #[test]
    fn try_numbers_come_from_tree_and_branch_names() {
        let run = RunId::new("run-01").unwrap();
        let task = TaskId::new("task-002").unwrap();
        let branch = try_branch(&run, &task, 7);
        assert_eq!(try_of_branch(&run, &task, &branch), Some(7));
        assert_eq!(try_of_branch(&run, &task, &task_branch(&run, &task)), None);
        let other = TaskId::new("task-003").unwrap();
        assert_eq!(try_of_branch(&run, &other, &branch), None);
        let tree = Path::new("/worktrees/run-01/task-002-try-3");
        assert_eq!(try_of_tree(&task, tree), Some(3));
        assert_eq!(try_of_tree(&other, tree), None);
        assert_eq!(try_of_tree(&task, Path::new("/worktrees/task-002")), None);
    }

    #[test]
    fn commit_messages_carry_the_session_key_and_never_who_made_it() {
        let run = RunId::new("run-01").unwrap();
        let task = TaskId::new("task-002").unwrap();
        let key = SessionKey::numbered(4);
        let text = commit_message(&run, &task, Some((&key, 1)), true);
        assert_eq!(
            text,
            "task-002: work from s-004\n\nArgonauts-Session: s-004"
        );
        let resumed = commit_message(&run, &task, Some((&key, 2)), true);
        assert!(resumed.ends_with("Argonauts-Session: s-004\nArgonauts-Start: 2"));
        let yours = commit_message(&run, &task, None, false);
        assert!(conductor::is_conductor_message(&run, &yours), "{yours}");
        let left = commit_message(&run, &task, None, true);
        assert_eq!(left, "task-002: work an agent left");
        assert!(!conductor::is_conductor_message(&run, &left), "{left}");
    }
}
