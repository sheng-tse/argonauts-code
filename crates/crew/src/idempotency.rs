//! Proof that a side effect already happened, checked before doing it: a commit with the
//! session's key in its message, a merge commit naming the card, a `Delivered` event for a
//! message, a `SessionStarted` whose `Ended` is missing (resume it, don't restart). Replaying a
//! log never merges twice or sends a message twice.

use std::path::Path;

use argonauts_core::CardId;
use argonauts_core::names::{MessageId, SessionKey};
use argonauts_core::steps::StepName;

use crate::error::EngineResult;
use crate::git::Git;
use crate::state::RunState;

/// The trailer naming the session whose work a commit holds.
pub const SESSION_TRAILER: &str = "Argonauts-Session";
/// The trailer counting a resumed session's starts, left out on its first start.
pub const START_TRAILER: &str = "Argonauts-Start";
/// The trailer naming the card a merge commit brings in.
pub const CARD_TRAILER: &str = "Argonauts-Card";

pub fn session_line(session: &SessionKey) -> String {
    format!("{SESSION_TRAILER}: {session}")
}

/// The trailers of a commit holding one start's work of `session`.
pub fn session_lines(session: &SessionKey, start: u32) -> String {
    if start > 1 {
        format!("{}\n{START_TRAILER}: {start}", session_line(session))
    } else {
        session_line(session)
    }
}

/// Which start of its session a commit message holds: 1 unless a start trailer says more.
fn start_of(message: &str) -> u32 {
    let prefix = format!("{START_TRAILER}: ");
    message
        .lines()
        .find_map(|line| line.trim_end().strip_prefix(&prefix)?.parse().ok())
        .unwrap_or(1)
}

pub fn card_line(card: &CardId) -> String {
    format!("{CARD_TRAILER}: {card}")
}

fn has_line(message: &str, line: &str) -> bool {
    message.lines().any(|l| l.trim_end() == line)
}

/// Whether a commit message holds this start of `session`'s work.
pub fn holds_start(message: &str, session: &SessionKey, start: u32) -> bool {
    has_line(message, &session_line(session)) && start_of(message) == start
}

/// The commit on the card's branch that holds this start of the session, found by its
/// trailers. A resumed session's next start commits again.
pub async fn commit_for_session(
    git: &Git,
    dir: &Path,
    range: &str,
    session: &SessionKey,
    start: u32,
) -> EngineResult<Option<String>> {
    let log = git.log_messages(dir, range).await?;
    Ok(log
        .into_iter()
        .find(|(_, message)| holds_start(message, session, start))
        .map(|(sha, _)| sha))
}

/// The merge commit on the integration branch that names this card.
pub async fn merge_for_card(
    git: &Git,
    dir: &Path,
    range: &str,
    card: &CardId,
) -> EngineResult<Option<String>> {
    let line = card_line(card);
    for (sha, message) in git.log_messages(dir, range).await? {
        if has_line(&message, &line) && git.parents(dir, &sha).await?.len() > 1 {
            return Ok(Some(sha));
        }
    }
    Ok(None)
}

/// The merge commit that brought in exactly `card_head`, so a card sent back and changed merges
/// again while a replay of the same merge doesn't.
pub async fn merge_of_head(
    git: &Git,
    dir: &Path,
    range: &str,
    card: &CardId,
    card_head: &str,
) -> EngineResult<Option<String>> {
    let line = card_line(card);
    for (sha, message) in git.log_messages(dir, range).await? {
        if !has_line(&message, &line) {
            continue;
        }
        let parents = git.parents(dir, &sha).await?;
        if parents.iter().skip(1).any(|parent| parent == card_head) {
            return Ok(Some(sha));
        }
    }
    Ok(None)
}

pub fn delivered(state: &RunState, message: &MessageId, session: &SessionKey) -> bool {
    state
        .blackboard
        .delivered
        .iter()
        .any(|(m, s)| m == message && s == session)
}

/// The card's newest session of `step` that started and never ended: resume it, don't start
/// another.
pub fn unfinished_session(state: &RunState, card: &CardId, step: StepName) -> Option<SessionKey> {
    state
        .sessions
        .by_key
        .values()
        .filter(|s| s.card.as_ref() == Some(card) && s.started.step == Some(step))
        .filter(|s| s.end.is_none())
        .max_by_key(|s| s.since)
        .map(|s| s.key.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::MergeOutcome;
    use crate::testutil::{TestRepo, git_test_env};

    fn git() -> Git {
        Git::with_env(git_test_env())
    }

    #[tokio::test]
    async fn a_session_commit_is_found_by_its_key() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let key = SessionKey::numbered(7);
        let range = format!("{base}..HEAD");
        assert_eq!(
            commit_for_session(&git, &repo.root, &range, &key, 1)
                .await
                .unwrap(),
            None
        );
        repo.write("a.txt", "a");
        let message = format!("card-001: work\n\n{}", session_line(&key));
        let sha = git.commit_all(&repo.root, &message).await.unwrap().unwrap();
        assert_eq!(
            commit_for_session(&git, &repo.root, &range, &key, 1)
                .await
                .unwrap(),
            Some(sha)
        );
        let other = SessionKey::numbered(70);
        assert_eq!(
            commit_for_session(&git, &repo.root, &range, &other, 1)
                .await
                .unwrap(),
            None,
            "s-070 is not s-007"
        );
        assert_eq!(
            commit_for_session(&git, &repo.root, &range, &key, 2)
                .await
                .unwrap(),
            None,
            "a resumed start commits again"
        );
        repo.write("b.txt", "b");
        let message = format!("card-001: work\n\n{}", session_lines(&key, 2));
        let resumed = git.commit_all(&repo.root, &message).await.unwrap().unwrap();
        assert_eq!(
            commit_for_session(&git, &repo.root, &range, &key, 2)
                .await
                .unwrap(),
            Some(resumed)
        );
    }

    #[tokio::test]
    async fn a_merge_naming_the_card_is_found_and_plain_commits_are_not() {
        let repo = TestRepo::new();
        let git = git();
        let base = git.head_commit(&repo.root).await.unwrap().unwrap();
        let card = CardId::new("card-001").unwrap();
        let range = format!("{base}..HEAD");

        // A plain commit that mentions the card is not a merge.
        repo.write("x.txt", "x");
        git.commit_all(&repo.root, &format!("Note\n\n{}", card_line(&card)))
            .await
            .unwrap();
        assert_eq!(
            merge_for_card(&git, &repo.root, &range, &card)
                .await
                .unwrap(),
            None
        );

        let tree = repo.dir.path().join("tree");
        git.add_worktree(&repo.root, &tree, "argonauts/r/card-001", &base)
            .await
            .unwrap();
        std::fs::write(tree.join("a.txt"), "a").unwrap();
        let head = git.commit_all(&tree, "work").await.unwrap().unwrap();
        let message = format!("Merge card-001\n\n{}", card_line(&card));
        let MergeOutcome::Merged(merge) = git
            .merge_no_ff(&repo.root, "argonauts/r/card-001", &message)
            .await
            .unwrap()
        else {
            panic!("expected a merge");
        };
        assert_eq!(
            merge_for_card(&git, &repo.root, &range, &card)
                .await
                .unwrap(),
            Some(merge.clone())
        );
        assert_eq!(
            merge_of_head(&git, &repo.root, &range, &card, &head)
                .await
                .unwrap(),
            Some(merge)
        );

        // New work on the card is not merged yet.
        std::fs::write(tree.join("b.txt"), "b").unwrap();
        let newer = git.commit_all(&tree, "more").await.unwrap().unwrap();
        assert_eq!(
            merge_of_head(&git, &repo.root, &range, &card, &newer)
                .await
                .unwrap(),
            None
        );
    }

    #[test]
    fn delivered_needs_the_same_message_and_session() {
        let mut state = RunState::default();
        let (m1, s1) = (MessageId::numbered(1), SessionKey::numbered(1));
        state.blackboard.delivered.push((m1.clone(), s1.clone()));
        assert!(delivered(&state, &m1, &s1));
        assert!(!delivered(&state, &m1, &SessionKey::numbered(2)));
        assert!(!delivered(&state, &MessageId::numbered(2), &s1));
    }
}
