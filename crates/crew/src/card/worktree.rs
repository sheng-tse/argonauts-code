//! A card's worktree (`~/.argonauts/worktrees/<repo>/<run>/<card>`, branch
//! `argonauts/<run>/<card>`, from the integration branch), and the commit after every session
//! end.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use globset::Glob;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardEvent;
use argonauts_core::events::session::InputPart;
use argonauts_core::layout;
use argonauts_core::names::{SessionKey, WorkerRef};
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::{CardId, CommitInfo, DiffStat, Note, NoteLevel, RunId};

use crate::card::claims;
use crate::card::prompt;
use crate::card::support::{record_card, record_card_in, record_of};
use crate::engine::host::Host;
use crate::engine::shared::lock;
use crate::error::{EngineError, EngineResult};
use crate::git::{Git, LeftOut, LeftOutWhy};
use crate::handoff::notes;
use crate::idempotency;
use crate::integrate::branch::{
    attempt_branch, card_branch, ensure_integration, integration_branch,
};
use crate::integrate::conductor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardTree {
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
}

/// Creates the worktree, or finds the one the card already has.
pub async fn ensure(host: &Host, card: &CardId) -> EngineResult<CardTree> {
    let record = record_of(host, card)?;
    let branch = record
        .branch
        .clone()
        .unwrap_or_else(|| card_branch(host.run_id(), card));
    let path = record
        .worktree
        .clone()
        .unwrap_or_else(|| host.repo().worktrees.join(card.as_str()));
    let base = match &record.base {
        Some(base) => base.clone(),
        None => integration_head(host).await?,
    };
    let tree = CardTree { path, branch, base };
    let fresh = make_tree(host, &tree, &tree.base).await?;
    let recorded = record.worktree.as_ref() == Some(&tree.path)
        && record.branch.as_ref() == Some(&tree.branch)
        && record.base.as_ref() == Some(&tree.base);
    if !recorded {
        let event = CardEvent::Worktree {
            path: tree.path.clone(),
            branch: tree.branch.clone(),
            base: tree.base.clone(),
        };
        record_card(host, card, event)?;
    }
    if fresh {
        copy_untracked(host, &tree.path).await?;
    }
    Ok(tree)
}

/// A separate worktree and branch for one attempt at a card, such as a race candidate's
/// (`argonauts/<run>/<card>-attempt-<n>`). Its diffs are against the card's base; its branch
/// starts at the card's committed work, which is the base itself until the card has commits.
pub async fn attempt_tree(host: &Host, card: &CardId, attempt: u32) -> EngineResult<CardTree> {
    let own = ensure(host, card).await?;
    let start = host
        .git()
        .branch_head(&host.repo().root, &own.branch)
        .await?
        .unwrap_or_else(|| own.base.clone());
    let tree = CardTree {
        path: attempt_path(host, card, attempt),
        branch: attempt_branch(host.run_id(), card, attempt),
        base: own.base,
    };
    if make_tree(host, &tree, &start).await? {
        copy_untracked(host, &tree.path).await?;
    }
    Ok(tree)
}

/// Whether an attempt tree or branch with this number already exists, so a new attempt must
/// take another.
pub async fn attempt_taken(host: &Host, card: &CardId, attempt: u32) -> EngineResult<bool> {
    let path = attempt_path(host, card, attempt);
    if path.exists() {
        return Ok(true);
    }
    let branch = attempt_branch(host.run_id(), card, attempt);
    Ok(host
        .git()
        .branch_head(&host.repo().root, &branch)
        .await?
        .is_some())
}

/// The attempt number of one of this card's attempt branches, from its name.
pub fn attempt_of_branch(run: &RunId, card: &CardId, branch: &str) -> Option<u32> {
    let prefix = format!("{}-attempt-", card_branch(run, card));
    branch.strip_prefix(&prefix)?.parse().ok()
}

/// The attempt number of one of this card's attempt trees, from its name.
pub fn attempt_of_tree(card: &CardId, tree: &Path) -> Option<u32> {
    let name = tree.file_name()?.to_str()?;
    name.strip_prefix(&format!("{card}-attempt-"))?.parse().ok()
}

fn attempt_path(host: &Host, card: &CardId, attempt: u32) -> PathBuf {
    host.repo()
        .worktrees
        .join(format!("{card}-attempt-{attempt}"))
}

/// `git add -A` and a commit with the session key in its message, hooks off; records
/// `Card::Committed` and `Card::Diff`. Nothing happens when that commit already exists.
///
/// A merge the session left open is not finished here: it waits for the session to resume,
/// since finishing it would commit whatever conflict markers are still in it.
pub async fn commit_after_end(
    host: &Host,
    card: &CardId,
    session: &SessionKey,
) -> EngineResult<Option<CommitInfo>> {
    let tree = ensure(host, card).await?;
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
    commit_in(host, card, &tree, Some(session), worker).await
}

/// Commits everything in `tree` for `session` (or, with no session, as your own work) and
/// records it. Commits the session's agent made itself come first: each is recorded as its
/// work, and only what it left uncommitted goes in the conductor's commit. That commit also
/// takes back out the new files the agent committed without meaning to create them. Every new commit
/// gets its author's note. A commit already made for the session's current start is recorded if
/// the log missed it, and never made twice. A start that changed nothing is recorded too, with
/// no commit, so a replay finds its end commit done in the log and doesn't ask git again. A
/// merge with conflict markers left in it stays open, and the gates report it.
///
/// Returns the newest commit recorded here.
pub(crate) async fn commit_in(
    host: &Host,
    card: &CardId,
    tree: &CardTree,
    session: Option<&SessionKey>,
    by: Option<WorkerRef>,
) -> EngineResult<Option<CommitInfo>> {
    let git = tree_git(host, &tree.path).await?;
    let session = session.map(|key| {
        let start = host.read(|state| state.sessions.get(key).map_or(1, |record| record.starts));
        (key, start)
    });
    // A resumed session can change files after its last commit, so a clean tree is what
    // shows there's nothing left to commit.
    if let Some((key, start)) = session
        && commit_recorded(host, card, key, start)?
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
        for sha in agents_commits(host, card, &git, &tree.path, &range, session).await? {
            let (info, moved) =
                record_commit(host, card, &git, &tree, &sha, key, by.as_ref()).await?;
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
        Some(sha) if record_of(host, card)?.has_commit(&sha) => None,
        Some(sha) => Some(sha),
        None => {
            let message = commit_message(host.run_id(), card, session, by.as_ref());
            // An agent's commit takes the new files it meant to create. What you left in the
            // worktree yourself, with no agent named, is all yours and all committed.
            let claims = by
                .as_ref()
                .and_then(|_| claims::of_card(host, card, &tree.path));
            let committed = if git.merge_in_progress(&tree.path).await? {
                if !git.leftover_markers(&tree.path).await?.is_empty() {
                    return finish(host, card, &git, &tree, made).await;
                }
                let (sha, left_out) = git
                    .finish_merge_work(&tree.path, &message, claims.as_ref())
                    .await?;
                tell_left_out(host, card, &left_out)?;
                Some(sha)
            } else {
                // What the agent's own commits swept in is sorted like what it left behind,
                // so the branch holds the same files whoever ran `git commit`.
                git.unstage_left_out(&tree.path, &own, claims.as_ref())
                    .await?;
                let committed = git
                    .commit_claimed(&tree.path, &message, claims.as_ref())
                    .await?;
                tell_left_out(host, card, &committed.left_out)?;
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
                record_card_in(host, card, key, CardEvent::Committed(nothing))?;
            }
            committed
        }
    };
    if let Some(sha) = leftover {
        let key = session.map(|(key, _)| key);
        let (info, moved) = record_commit(host, card, &git, &tree, &sha, key, by.as_ref()).await?;
        tree = moved;
        made.push(info);
    }
    finish(host, card, &git, &tree, made).await
}

/// The most a note names before it says how many more.
const LEFT_OUT_NAMED: usize = 6;

/// Names what a commit of `card` left out, each folder or file once per card, so a file left
/// out by mistake shows while it's still in the worktree, before the merge. Otherwise those
/// files would go missing without a word.
fn tell_left_out(host: &Host, card: &CardId, left_out: &[LeftOut]) -> EngineResult<()> {
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
                .filter(|left| told.insert(format!("{card}:{}", left.because)))
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
                "Left out of {card}'s commit as build and cache output, and kept in its \
                 worktree: {named}. merge.leave_out and merge.leave_out_builtin set what's \
                 left out."
            ),
            LeftOutWhy::Unclaimed => format!(
                "Left out of {card}'s commit, and kept in its worktree: {named}. No edit tool \
                 wrote them and no report lists them, so they read as something a command \
                 left behind. commit.unclaimed = \"keep\" commits every new file."
            ),
            LeftOutWhy::TooBig => format!(
                "Left out of {card}'s commit as bigger than commit.max_new_file_mb allows, \
                 and kept in its worktree: {named}."
            ),
            LeftOutWhy::Ignored => format!(
                "Left out of {card}'s commit because the repository's ignore rules match \
                 them, and kept in its worktree: {named}. A file that belongs in the change \
                 needs another name, or a change to the rule."
            ),
        };
        host.record(
            Some(card),
            None,
            RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text,
            }),
        )?;
    }
    Ok(())
}

/// What to tell `card`'s worker when checks fail: the new files in its worktree that its
/// commit left out. The checks ran on the commit without them, so one of them may be what's
/// missing. `None` when nothing was left out.
pub(crate) async fn left_out_part(host: &Host, card: &CardId) -> EngineResult<Option<InputPart>> {
    let tree = ensure(host, card).await?;
    let Some(claims) = claims::of_card(host, card, &tree.path) else {
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
/// first: the agent's own. One holding this start's session lines is the conductor's commit
/// for the session, recorded as that.
async fn agents_commits(
    host: &Host,
    card: &CardId,
    git: &Git,
    dir: &Path,
    range: &str,
    session: Option<(&SessionKey, u32)>,
) -> EngineResult<Vec<String>> {
    let record = record_of(host, card)?;
    let mut shas: Vec<String> = git
        .first_parent_log(dir, range)
        .await?
        .into_iter()
        .filter(|(sha, message)| {
            let conductors =
                session.is_some_and(|(key, start)| idempotency::holds_start(message, key, start));
            !record.has_commit(sha) && !conductors
        })
        .map(|(sha, _)| sha)
        .collect();
    shas.reverse();
    Ok(shas)
}

/// Records `sha` as `by`'s work in the session, or as your own with no session, and moves the
/// tree's base past an integration merge it holds.
async fn record_commit(
    host: &Host,
    card: &CardId,
    git: &Git,
    tree: &CardTree,
    sha: &str,
    session: Option<&SessionKey>,
    by: Option<&WorkerRef>,
) -> EngineResult<(CommitInfo, CardTree)> {
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
    let committed = CardEvent::Committed(info.clone());
    match session {
        Some(key) => record_card_in(host, card, key, committed)?,
        None => record_card(host, card, committed)?,
    };
    let tree = rebase_on_merge(host, card, git, tree, &parents).await?;
    Ok((info, tree))
}

/// Notes each new commit's author, saves the card's diff up to the newest, and returns it.
async fn finish(
    host: &Host,
    card: &CardId,
    git: &Git,
    tree: &CardTree,
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
        save_diff(host, card, git, tree, sha).await?;
    }
    Ok(Some(newest))
}

/// After the integration branch is merged into the card's own branch, the integration head
/// it brought in becomes the card's base, so the card's diffs stop counting other cards'
/// integrated work as its own.
async fn rebase_on_merge(
    host: &Host,
    card: &CardId,
    git: &Git,
    tree: &CardTree,
    parents: &[String],
) -> EngineResult<CardTree> {
    let [_, merged] = parents else {
        return Ok(tree.clone());
    };
    let own = record_of(host, card)?.branch.as_ref() == Some(&tree.branch);
    if !own || *merged == tree.base {
        return Ok(tree.clone());
    }
    let integration = integration_branch(host.run_id());
    let Some(head) = git.branch_head(&tree.path, &integration).await? else {
        return Ok(tree.clone());
    };
    let forward = git.is_ancestor(&tree.path, merged, &head).await?
        && git.is_ancestor(&tree.path, &tree.base, merged).await?;
    if !forward {
        return Ok(tree.clone());
    }
    let moved = CardTree {
        base: merged.clone(),
        ..tree.clone()
    };
    let event = CardEvent::Worktree {
        path: moved.path.clone(),
        branch: moved.branch.clone(),
        base: moved.base.clone(),
    };
    record_card(host, card, event)?;
    Ok(moved)
}

/// The engine's own commits on the card's branches: your changes after a take-over and the
/// integration merges it made cleanly. Matched by SHA, never by message, since an agent can
/// write any message.
pub(crate) fn conductor_commits(host: &Host, card: &CardId) -> EngineResult<BTreeSet<String>> {
    Ok(record_of(host, card)?
        .commits
        .iter()
        .filter(|commit| commit.by.is_none())
        .filter_map(|commit| commit.sha.clone())
        .collect())
}

/// Whether the log holds a `Card::Committed` for this start of `session`, with a commit or
/// with nothing to commit.
fn commit_recorded(
    host: &Host,
    card: &CardId,
    session: &SessionKey,
    start: u32,
) -> EngineResult<bool> {
    Ok(record_of(host, card)?
        .commits
        .iter()
        .any(|commit| idempotency::holds_start(&commit.message, session, start)))
}
/// Saves the card's whole diff against its base and records `Card::Diff`.
pub(crate) async fn save_diff(
    host: &Host,
    card: &CardId,
    git: &Git,
    tree: &CardTree,
    head: &str,
) -> EngineResult<()> {
    let text = git.diff(&tree.path, &tree.base, head).await?;
    let stat = git.diff_stat(&tree.path, &tree.base, head).await?;
    let dir = layout::card_dir(&host.repo().run_dir, card).join("diffs");
    create_private_dir_all(&dir)?;
    let short: String = head.chars().take(12).collect();
    let path = dir.join(format!("{short}.diff"));
    write_private_file(&path, text.as_bytes())?;
    record_card(host, card, CardEvent::Diff { stat, path })?;
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
/// session commits again. Work with no session carries its author, and only your own
/// changes after a take-over carry the conductor trailer.
fn commit_message(
    run: &RunId,
    card: &CardId,
    session: Option<(&SessionKey, u32)>,
    by: Option<&WorkerRef>,
) -> String {
    match (session, by) {
        (Some((session, start)), Some(by)) => format!(
            "{card}: work from {session} ({by})\n\n{}",
            idempotency::session_lines(session, start)
        ),
        (Some((session, start)), None) => format!(
            "{card}: work from {session}\n\n{}",
            idempotency::session_lines(session, start)
        ),
        (None, Some(by)) => format!("{card}: work left by {by}"),
        (None, None) => conductor::message(run, &format!("{card}: your changes")),
    }
}

/// The integration branch's head, where new card worktrees start.
async fn integration_head(host: &Host) -> EngineResult<String> {
    ensure_integration(host).await?;
    let branch = integration_branch(host.run_id());
    host.git()
        .branch_head(&host.repo().root, &branch)
        .await?
        .ok_or_else(|| EngineError::Refused(format!("{branch} has no commits")))
}

/// Adds the worktree when it's missing. True when it was just made.
async fn make_tree(host: &Host, tree: &CardTree, start: &str) -> EngineResult<bool> {
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

/// Copies the untracked files `gates.copy_untracked` names from your checkout into a new
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
    fn attempt_numbers_come_from_tree_and_branch_names() {
        let run = RunId::new("run-01").unwrap();
        let card = CardId::new("task-002").unwrap();
        let branch = attempt_branch(&run, &card, 7);
        assert_eq!(attempt_of_branch(&run, &card, &branch), Some(7));
        assert_eq!(
            attempt_of_branch(&run, &card, &card_branch(&run, &card)),
            None
        );
        let other = CardId::new("task-003").unwrap();
        assert_eq!(attempt_of_branch(&run, &other, &branch), None);
        let tree = Path::new("/worktrees/run-01/task-002-attempt-3");
        assert_eq!(attempt_of_tree(&card, tree), Some(3));
        assert_eq!(attempt_of_tree(&other, tree), None);
        assert_eq!(
            attempt_of_tree(&card, Path::new("/worktrees/task-002")),
            None
        );
    }

    #[test]
    fn commit_messages_carry_the_session_key() {
        let run = RunId::new("run-01").unwrap();
        let card = CardId::new("task-002").unwrap();
        let key = SessionKey::numbered(4);
        let by: WorkerRef = "codex".parse().unwrap();
        let text = commit_message(&run, &card, Some((&key, 1)), Some(&by));
        assert_eq!(
            text,
            "task-002: work from s-004 (codex)\n\nArgonauts-Session: s-004"
        );
        let resumed = commit_message(&run, &card, Some((&key, 2)), Some(&by));
        assert!(resumed.ends_with("Argonauts-Session: s-004\nArgonauts-Start: 2"));
        let yours = commit_message(&run, &card, None, None);
        assert!(conductor::is_conductor_message(&run, &yours), "{yours}");
        let left = commit_message(&run, &card, None, Some(&by));
        assert!(!conductor::is_conductor_message(&run, &left), "{left}");
    }
}
