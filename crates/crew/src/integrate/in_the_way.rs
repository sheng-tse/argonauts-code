//! What stops landing in your checkout. Landing moves your checkout of the target forward, and
//! git won't overwrite your untracked files at paths the run brings, or merge over your
//! uncommitted edits to files it changes, so landing looks first. They're yours, so only you
//! decide: move the untracked ones aside and merge, try again once you've dealt with them,
//! merge later, or end the run without merging, on its branch or as a pull request. When git
//! refuses for a reason of its own, the same card says so in git's words. Until then the run
//! waits to merge; it hasn't failed.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointKind, CheckpointPacket, FreeText, MergePacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::names::CheckpointId;
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::{Note, NoteLevel};

use crate::checkpoints::{self, CheckpointRequest};
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::git::GitError;
use crate::integrate::unborn;

pub const MOVE_ASIDE: &str = "Move them aside and merge";
pub const TRY_AGAIN: &str = "Try again";
pub const MERGE_LATER: &str = "Merge later";

/// The most files a note names before it says how many more.
const NAMED: usize = 5;

/// What's in the way of moving your checkout from main to the run's head, and what of yours
/// already holds what it brings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blocked {
    /// Your untracked files, ignored ones too, at paths the run brings. A folder ends in `/`.
    pub untracked: Vec<String>,
    /// Your uncommitted edits to files the merge changes.
    pub edited: Vec<String>,
    /// Git's words, when it refused with nothing of yours found in the way.
    pub refused: Option<String>,
    /// Your files at paths the run writes that hold what it brings there already, so they
    /// aren't in its way. Landing takes them as they are.
    pub holding: Holding,
}

/// Your files that already hold what the run brings: the same bytes, or nothing where it
/// removes a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holding {
    /// Untracked, or ignored, at paths the run adds.
    pub untracked: Vec<String>,
    /// Edits, and deletions, of files the run changes.
    pub edited: Vec<String>,
}

impl Holding {
    pub fn all(&self) -> Vec<String> {
        let mut all: Vec<String> = self.untracked.iter().chain(&self.edited).cloned().collect();
        all.sort();
        all
    }
}

impl Blocked {
    pub fn is_empty(&self) -> bool {
        self.untracked.is_empty() && self.edited.is_empty() && self.refused.is_none()
    }

    /// Your files git won't overwrite, in one list.
    fn files(&self) -> Vec<String> {
        let mut files: Vec<String> = self.untracked.iter().chain(&self.edited).cloned().collect();
        files.sort();
        files
    }
}

/// Your files in the way of moving your checkout from `main` to `head`. A file that holds
/// what `head` has there already isn't, and is listed apart.
pub async fn find(host: &Host, main: &str, head: &str) -> Result<Blocked, GitError> {
    let git = host.git();
    let root = &host.repo().root;
    let unborn = unborn::base(host).await?.is_some();
    let edited = if unborn {
        unborn::edited_in_the_way(host, main, head).await?
    } else {
        git.edited_in_the_way(root, main, head).await?
    };
    let untracked: Vec<String> = git
        .in_the_way(root, main, head)
        .await?
        .into_iter()
        .filter(|path| !edited.contains(path))
        .collect();
    let mut holding = git.holding(root, head, &edited).await?;
    // Taking an edit as it is stages it, which would lose a version you staged on the way.
    // Before a first commit your index is never touched, so it can't lose anything there.
    if !unborn {
        let apart = git.staged_apart(root, main, head, &holding).await?;
        holding.retain(|path| !apart.contains(path));
    }
    let holding = Holding {
        untracked: git.holding(root, head, &untracked).await?,
        edited: holding,
    };
    let held = |path: &String| !holding.untracked.contains(path) && !holding.edited.contains(path);
    Ok(Blocked {
        untracked: untracked.into_iter().filter(held).collect(),
        edited: edited.into_iter().filter(held).collect(),
        refused: None,
        holding,
    })
}

/// What you chose on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// Go on: your checkout may have changed while the card waited, so landing looks again.
    Again,
    /// You chose to merge later.
    Later,
    /// The run ends with its work on its branch, not merged.
    KeepBranch,
    /// The run ends as a pull request, `yours` when you chose it rather than your rules.
    PullRequest { yours: bool },
}

/// How often a waiting merge looks whether your files are still in the way.
const LOOK_AGAIN: Duration = Duration::from_secs(3);

/// Asks you what to do about `blocked`, once, and does the moving you chose. `approved` is the
/// merge card you or your rules answered, whose evidence the question shows again. With
/// `range`, the main and head the files are in the way of, the wait also ends by itself once
/// you've moved them: the question is withdrawn, and landing looks again.
pub async fn ask(
    host: &Host,
    approved: &CheckpointId,
    target: &str,
    blocked: &Blocked,
    range: Option<(&str, &str)>,
) -> EngineResult<Way> {
    let files = blocked.files();
    if !files.is_empty() && host.read(|state| state.integration.waiting_to_merge != files) {
        host.record(
            None,
            None,
            RunEventKind::Integration(IntegrationEvent::WaitingToMerge {
                target: target.to_owned(),
                in_the_way: files,
            }),
        )?;
    }
    let opening = checkpoints::open(host, request(host, approved, target, blocked))?;
    let id = opening.id().clone();
    let answer = opening.decided_or_withdrawn(host);
    tokio::pin!(answer);
    let answered = loop {
        tokio::select! {
            answered = &mut answer => break answered?,
            () = tokio::time::sleep(LOOK_AGAIN), if range.is_some() => {
                if let Some((main, head)) = range
                    && find(host, main, head).await.is_ok_and(|now| now.is_empty())
                {
                    // Answered in the same moment: the answer stands, and nothing is lost.
                    let _ = checkpoints::withdraw(host, &id, "nothing is in the way anymore");
                }
            }
        }
    };
    // Withdrawn because what's in the way changed: landing looks again.
    let Ok(decided) = answered else {
        return Ok(Way::Again);
    };
    match &decided.action {
        AnswerAction::Choose { value } if value == MERGE_LATER => return Ok(Way::Later),
        AnswerAction::KeepBranch => return Ok(Way::KeepBranch),
        AnswerAction::PullRequest => {
            let yours = matches!(decided.by, Party::User);
            return Ok(Way::PullRequest { yours });
        }
        AnswerAction::Choose { value } if value == MOVE_ASIDE => {
            move_aside(host, &host.repo().root, &blocked.untracked)?;
        }
        _ => {}
    }
    Ok(Way::Again)
}

/// The card asking what to do: the merge's evidence again, with what's in the way.
fn request(
    host: &Host,
    approved: &CheckpointId,
    target: &str,
    blocked: &Blocked,
) -> CheckpointRequest {
    let shown = host.read(|state| {
        state
            .checkpoints
            .by_id
            .get(approved)
            .and_then(|record| match &record.opened.packet {
                CheckpointPacket::Merge(packet) => Some(packet.clone()),
                _ => None,
            })
    });
    let mut packet = shown.unwrap_or_else(|| MergePacket {
        target: Some(target.to_owned()),
        branch: String::new(),
        rows: Vec::new(),
        suite_passed_on_latest_main: false,
        gates: Vec::new(),
        combined: Default::default(),
        conductor: Default::default(),
        diff_path: PathBuf::new(),
        tree_sha: String::new(),
        spent: Default::default(),
        limit: None,
        partial: false,
        in_the_way: Vec::new(),
        edited: Vec::new(),
        refused: None,
    });
    packet.in_the_way = blocked.untracked.clone();
    packet.edited = blocked.edited.clone();
    packet.refused = blocked.refused.clone();
    // The work was approved already, so nothing here waits on a stronger review.
    for row in &mut packet.rows {
        row.debt.clear();
    }
    let run = host.run_id();
    let choice = |key: char, label: &str, recommended: bool, detail: String| AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: label.to_owned(),
        },
        recommended,
        detail: Some(detail),
    };
    let mut options = Vec::new();
    if !blocked.untracked.is_empty() {
        options.push(choice(
            'm',
            MOVE_ASIDE,
            true,
            "your untracked files, into the run's folder, nothing deleted".to_owned(),
        ));
    }
    let once = if !blocked.edited.is_empty() {
        "once you've committed or stashed your edits"
    } else if blocked.untracked.is_empty() {
        "once git can, after what it says is dealt with"
    } else {
        "once you've moved or deleted them"
    };
    options.push(choice('t', TRY_AGAIN, options.is_empty(), once.to_owned()));
    options.push(choice(
        'l',
        MERGE_LATER,
        false,
        format!("the run pauses here; /resume {run} merges it"),
    ));
    // The history Argonauts keeps of a folder without git has no branch you'd look at, and
    // no remote.
    if host.repo().hidden_git().is_none() {
        let ending = |key: char, label: &str, action: AnswerAction, detail: String| AnswerOption {
            key,
            label: label.to_owned(),
            action,
            recommended: false,
            detail: Some(detail),
        };
        let branch = if packet.branch.is_empty() {
            "its branch".to_owned()
        } else {
            packet.branch.clone()
        };
        options.push(ending(
            'p',
            "Open a pull request",
            AnswerAction::PullRequest,
            format!("the run ends, and {branch} goes up for review"),
        ));
        options.push(ending(
            'k',
            "Keep the branch",
            AnswerAction::KeepBranch,
            format!("the run ends with its work on {branch}, not merged"),
        ));
    }
    let mut documents = Vec::new();
    if !blocked.untracked.is_empty() {
        documents.push(document(
            "your untracked files in the way",
            &blocked.untracked,
        ));
    }
    if !blocked.edited.is_empty() {
        documents.push(document("your edits in the way", &blocked.edited));
    }
    if let Some(refused) = &blocked.refused {
        documents.push(document("what git said", std::slice::from_ref(refused)));
    }
    CheckpointRequest {
        at: None,
        kind: CheckpointKind::Merge,
        title: format!("Waiting to merge into {target}"),
        card: None,
        packet: CheckpointPacket::Merge(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: plainly(target, blocked).into_iter().collect(),
        documents,
        options: Some(options),
        free_text: FreeText::None,
        authors: Vec::new(),
    }
}

fn document(label: &str, lines: &[String]) -> EvidenceDocument {
    EvidenceDocument {
        label: label.to_owned(),
        text: lines.join("\n"),
        path: None,
        sha256: None,
    }
}

/// A card still open from before, now that nothing is in the way.
pub fn withdraw_open(host: &Host) -> EngineResult<()> {
    let open = host.read(|state| {
        state
            .checkpoints
            .open_for(CheckpointKind::Merge, None)
            .filter(|record| checkpoints::only_yours(&record.opened.packet))
            .map(|record| record.opened.id.clone())
    });
    if let Some(id) = open {
        checkpoints::withdraw(host, &id, "nothing is in the way anymore")?;
    }
    Ok(())
}

/// Moves `files` out of your checkout into a folder of the run's, keeping their paths, and
/// says where. A file it can't move is named, and the card asks again.
fn move_aside(host: &Host, root: &Path, files: &[String]) -> EngineResult<()> {
    let into = free_folder(&host.repo().run_dir.join("set-aside"));
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    for file in files {
        let rel = file.trim_end_matches('/');
        if !is_inside(rel) {
            failed.push(format!("{file} (not a path in the repo)"));
            continue;
        }
        let to = into.join(rel);
        let result = to
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| move_path(&root.join(rel), &to));
        match result {
            Ok(()) => moved.push(file.clone()),
            Err(err) => failed.push(format!("{file} ({err})")),
        }
    }
    if !moved.is_empty() {
        let what = match moved.as_slice() {
            [one] => one.clone(),
            many => format!("{} files ({})", many.len(), named(many)),
        };
        // The folder goes last, where a long path can wrap.
        let text = format!("Moved {what} out of the way, into {}", into.display());
        note(host, NoteLevel::Info, text)?;
    }
    if !failed.is_empty() {
        let text = format!(
            "Couldn't move {}. Move them yourself, then choose {TRY_AGAIN}",
            named(&failed)
        );
        note(host, NoteLevel::Warn, text)?;
    }
    Ok(())
}

/// `base`, or `base-2`, `base-3`, … when the run set files aside before.
fn free_folder(base: &Path) -> PathBuf {
    let mut folder = base.to_path_buf();
    let mut n = 1;
    while folder.exists() {
        n += 1;
        let name = base.file_name().unwrap_or_default().to_string_lossy();
        folder = base.with_file_name(format!("{name}-{n}"));
    }
    folder
}

/// A rename, or a copy and delete when the run's folder is on another file system.
pub(crate) fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => {
            copy_all(from, to)?;
            if std::fs::symlink_metadata(from)?.is_dir() {
                std::fs::remove_dir_all(from)
            } else {
                std::fs::remove_file(from)
            }
        }
        result => result,
    }
}

fn copy_all(from: &Path, to: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    #[cfg(unix)]
    if meta.file_type().is_symlink() {
        return std::os::unix::fs::symlink(std::fs::read_link(from)?, to);
    }
    std::fs::copy(from, to).map(drop)
}

/// A relative path that stays inside the repository.
pub(crate) fn is_inside(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// Which of your files are in the way and why, file by file, and that none of them changed.
fn plainly(target: &str, blocked: &Blocked) -> Option<String> {
    let each: Vec<String> = blocked
        .edited
        .iter()
        .map(|file| format!("{file} (your uncommitted edit, where the run changes it too)"))
        .chain(blocked.untracked.iter().map(|file| {
            let what = if file.ends_with('/') {
                "files"
            } else {
                "a file"
            };
            format!("{file} (not committed, where the run adds {what})")
        }))
        .collect();
    let count = match each.len() {
        0 => return None,
        1 => "1 file of yours is".to_owned(),
        n => format!("{n} files of yours are"),
    };
    Some(format!(
        "{count} in the way of {target}: {}. Nothing of yours was changed.",
        named(&each)
    ))
}

/// Says which of your files landing took as they were, since they already held what the run
/// brings there. Nothing when there were none.
pub fn tell_held(host: &Host, holding: &Holding) -> EngineResult<()> {
    let held = holding.all();
    let text = match held.as_slice() {
        [] => return Ok(()),
        [one] => {
            format!("Your {one} already held what the run brings, so it stayed as you left it")
        }
        many => format!(
            "{} files of yours already held what the run brings, so they stayed as you left \
             them: {}",
            many.len(),
            named(many)
        ),
    };
    note(host, NoteLevel::Info, text)
}

fn named(files: &[String]) -> String {
    let shown = files.len().min(NAMED);
    let mut text = files[..shown].join(", ");
    if files.len() > shown {
        text.push_str(&format!(" and {} more", files.len() - shown));
    }
    text
}

fn note(host: &Host, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(None, None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_inside_the_repo_move() {
        assert!(is_inside("stats/__pycache__/core.pyc"));
        assert!(!is_inside("../outside"));
        assert!(!is_inside("/etc/hosts"));
        assert!(!is_inside(""));
    }

    #[test]
    fn a_second_set_aside_gets_a_folder_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("set-aside");
        assert_eq!(free_folder(&base), base);
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(free_folder(&base), dir.path().join("set-aside-2"));
    }

    #[test]
    fn a_move_across_file_systems_copies_then_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        std::fs::create_dir_all(from.join("deep")).unwrap();
        std::fs::write(from.join("deep/a.txt"), "a").unwrap();
        let to = dir.path().join("to");
        copy_all(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("deep/a.txt")).unwrap(), "a");
        move_path(&from.join("deep/a.txt"), &dir.path().join("b.txt")).unwrap();
        assert!(!from.join("deep/a.txt").exists());
    }

    #[test]
    fn the_card_says_which_files_are_in_the_way_and_why() {
        let blocked = Blocked {
            untracked: vec!["notes.txt".into(), "build/".into()],
            edited: vec!["README.md".into()],
            ..Blocked::default()
        };
        assert_eq!(
            plainly("main", &blocked).unwrap(),
            "3 files of yours are in the way of main: README.md (your uncommitted edit, where \
             the run changes it too), notes.txt (not committed, where the run adds a file), \
             build/ (not committed, where the run adds files). Nothing of yours was changed."
        );
        let refused = Blocked {
            refused: Some("git said no".into()),
            ..Blocked::default()
        };
        assert_eq!(plainly("main", &refused), None);
    }

    #[test]
    fn notes_name_a_few_files() {
        let files: Vec<String> = (1..=7).map(|n| format!("f{n}")).collect();
        assert_eq!(named(&files), "f1, f2, f3, f4, f5 and 2 more");
        assert_eq!(named(&files[..1]), "f1");
    }
}
