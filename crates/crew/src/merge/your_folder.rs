//! Writing a run into a folder that has no git of its own. Argonauts Code keeps that folder's
//! history itself, on one branch that follows the folder, so nothing here is a branch of
//! yours: the merge question asks to write the run into your folder, and offers no pull
//! request and no branch to keep.
//!
//! What you changed while the run worked can't be committed or stashed there. So when it's in
//! the way, it's merged with the run's work the way git merges two branches, and nothing is
//! asked while the changes are in different lines. Where you and the run changed the same
//! lines, you pick the version that goes in. Either way your folder as it was is a commit of
//! the history first, so `/rewind` gets it back.

use std::path::PathBuf;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, BothChanged, FreeText,
};
use argonauts_core::names::ApprovalId;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::settings::protocol::MergeMode;
use argonauts_core::{Note, NoteLevel, hidden_git};

use crate::approvals::{self, ApprovalRequest};
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::git::FolderMerge;
use crate::merge::deliver;
use crate::merge::in_the_way::{self, MERGE_LATER};
use crate::state::RunState;

pub const TITLE: &str = "Write this run into your folder?";
pub const WRITE_IT: &str = "Write it into my folder";
pub const KEEP_MINE: &str = "Keep my version";
pub const TAKE_THE_RUNS: &str = "Take the run's version";
pub const SHOW_BOTH: &str = "Show both";

/// Whether the run works on the history Argonauts Code keeps of a folder without git. That
/// was settled when the run started: a `git init` in the folder meanwhile doesn't move a run
/// halfway from one history to another.
pub fn is(host: &Host) -> bool {
    host.git().hidden_history().is_some()
}

/// Says, as a run on the hidden history ends, that the folder became a repository while it
/// worked, so nothing about where its work went is left to guess.
pub fn say_if_it_has_git_now(host: &Host) {
    if is(host) && host.repo().root.join(".git").exists() {
        let note = Note {
            level: NoteLevel::Info,
            text: "This folder has git now. This run kept to the hidden history it started \
                   with, and the next workflow uses your repository."
                .to_owned(),
        };
        let _ = host.record(None, None, RunEventKind::Note(note));
    }
}

/// How the run's work reaches you. A folder without git has no branch to leave the work on
/// and no remote to open a pull request at, so there you're always asked.
pub fn merge_mode(host: &Host) -> MergeMode {
    if is(host) {
        MergeMode::Ask
    } else {
        host.settings().merge.mode
    }
}

/// Clears a lock that a stop in the middle of a write left on the folder's history, and says
/// so. Without this the next write fails in git's own words, with nothing you can do in a
/// folder that has no `.git` to look into.
pub fn clear_stale_lock(host: &Host) {
    let Some(hidden) = host.git().hidden_history() else {
        return;
    };
    // Other runs of this folder may be writing, which nothing here can tell.
    let now = std::time::SystemTime::now();
    if hidden_git::clear_stale_locks(hidden, now, hidden_git::Writers::Maybe) {
        let note = Note {
            level: NoteLevel::Warn,
            text: hidden_git::CLEARED_STALE_LOCK.to_owned(),
        };
        let _ = host.record(None, None, RunEventKind::Note(note));
    }
}

/// What the merge question offers in a folder without git. `None` in a repository, which
/// keeps the question's own options.
pub fn merge_options(host: &Host) -> Option<Vec<AnswerOption>> {
    let option = |key: char, label: &str, action: AnswerAction| AnswerOption {
        key,
        label: label.to_owned(),
        action,
        recommended: false,
        detail: None,
    };
    is(host).then(|| {
        vec![
            option('y', WRITE_IT, AnswerAction::Merge),
            option(
                'b',
                "Request changes on some tasks",
                AnswerAction::RequestChanges,
            ),
            option('x', "Abandon the run", AnswerAction::Abandon),
            option('d', "Show the diff", AnswerAction::ShowDiff),
        ]
    })
}

/// `options` without the answers that need a branch of yours, in a folder without git.
pub fn without_branch_answers(host: &Host, mut options: Vec<AnswerOption>) -> Vec<AnswerOption> {
    if is(host) {
        options.retain(|option| !needs_a_branch(&option.action));
    }
    options
}

/// Whether an answer leaves the work on a branch or opens a pull request from one.
pub fn needs_a_branch(action: &AnswerAction) -> bool {
    matches!(action, AnswerAction::PullRequest | AnswerAction::KeepBranch)
}

/// What became of your changes in the way of the run's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// The run is in your folder, with your changes merged in or the version you picked. The
    /// history's branch is at this commit.
    Written(String),
    /// You chose to merge later.
    Later,
    /// Nothing the history records has changed, so what's in the way is a file it leaves out.
    InTheWay,
}

/// The version of a file both changed that goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Yours,
    Runs,
}

enum Asked {
    Side(Side),
    Later,
    /// The question went away without an answer of yours.
    Withdrawn,
}

/// Merges what you changed in your folder since `main`, the history's newest commit, with the
/// run's `head`, and writes the result into your folder. `approved` is the merge question
/// you answered, whose evidence the next question shows again.
pub async fn settle(
    host: &Host,
    approved: &ApprovalId,
    main: &str,
    head: &str,
) -> EngineResult<Settled> {
    let git = host.git();
    let repo = host.repo();
    let root = &repo.root;
    let scratch = repo.run_dir.join("your-folder.index");
    // Your pick, with the files it was about: a folder that changed under the question is
    // asked about again. An answer wakes the run in a new run host, so the pick is read
    // from the run's record.
    let mut picked: Option<(Side, Vec<String>)> = host.read(recorded_pick);
    loop {
        let yours = git.folder_as_commit(root, &scratch, main).await?;
        if git.tree_of(root, &yours).await? == git.tree_of(root, main).await? {
            return Ok(Settled::InTheWay);
        }
        let merge = git.merge_folder(root, &yours, head).await?;
        let side = match &picked {
            _ if merge.both_changed.is_empty() => None,
            Some((side, files)) if *files == merge.both_changed => Some(*side),
            _ => match ask(host, approved, &merge, (main, &yours, head)).await? {
                Asked::Later => return Ok(Settled::Later),
                Asked::Withdrawn => continue,
                Asked::Side(side) => {
                    picked = Some((side, merge.both_changed.clone()));
                    // The folder is read again, since it may have changed while you chose.
                    continue;
                }
            },
        };
        let tree = match side {
            None => merge.tree.clone(),
            Some(side) => {
                let version = match side {
                    Side::Yours => yours.as_str(),
                    Side::Runs => head,
                };
                git.tree_with(root, &scratch, &merge.tree, version, &merge.both_changed)
                    .await?
            }
        };
        let message = deliver::plain_message(host);
        let written = git
            .commit_tree_as_argonauts(root, &tree, &yours, &message)
            .await?;
        // All or nothing: git refuses while a file it would write changed since the snapshot.
        git.write_files(root, &scratch, &yours, &written, &[])
            .await?;
        git.follow_folder(root, &written).await?;
        say(host, &merge, side, (main, &yours, head)).await;
        return Ok(Settled::Written(written));
    }
}

/// The newest version you picked in this run, with the files the question was about.
fn recorded_pick(state: &RunState) -> Option<(Side, Vec<String>)> {
    state
        .approvals
        .by_id
        .values()
        .filter_map(|record| {
            let ApprovalPacket::Merge(packet) = &record.opened.packet else {
                return None;
            };
            let side = match &record.decided.as_ref()?.action {
                AnswerAction::Choose { value } if value == KEEP_MINE => Side::Yours,
                AnswerAction::Choose { value } if value == TAKE_THE_RUNS => Side::Runs,
                _ => return None,
            };
            let files: Vec<String> = packet
                .both_changed
                .iter()
                .map(|file| file.path.clone())
                .collect();
            (!files.is_empty()).then_some((record.since, side, files))
        })
        .max_by_key(|(since, ..)| *since)
        .map(|(_, side, files)| (side, files))
}

/// Asks which version of the files both changed goes in.
async fn ask(
    host: &Host,
    approved: &ApprovalId,
    merge: &FolderMerge,
    (main, yours, head): (&str, &str, &str),
) -> EngineResult<Asked> {
    let git = host.git();
    let root = &host.repo().root;
    let mut both_changed = Vec::new();
    for path in &merge.both_changed {
        let one = std::slice::from_ref(path);
        both_changed.push(BothChanged {
            path: path.clone(),
            yours: git.diff_stat_of(root, main, yours, one).await?,
            runs: git.diff_stat_of(root, main, head, one).await?,
        });
    }
    let beside = git.diff_of(root, yours, head, &merge.both_changed).await?;
    let request = request(host, approved, both_changed, &beside);
    let decided = approvals::open(host, request)?
        .decided_or_withdrawn(host)
        .await?;
    Ok(match decided.map(|decided| decided.action) {
        Ok(AnswerAction::Choose { value }) if value == KEEP_MINE => Asked::Side(Side::Yours),
        Ok(AnswerAction::Choose { value }) if value == TAKE_THE_RUNS => Asked::Side(Side::Runs),
        Ok(AnswerAction::Choose { value }) if value == MERGE_LATER => Asked::Later,
        _ => Asked::Withdrawn,
    })
}

/// The files as a question names them: the one file, or how many.
fn named(files: &[String]) -> String {
    match files {
        [one] => one.clone(),
        files => format!("{} files", files.len()),
    }
}

fn request(
    host: &Host,
    approved: &ApprovalId,
    both_changed: Vec<BothChanged>,
    beside: &str,
) -> ApprovalRequest {
    let files: Vec<String> = both_changed.iter().map(|file| file.path.clone()).collect();
    let one = files.len() == 1;
    let name = named(&files);
    let run = host.run_id();
    let mut packet = in_the_way::packet_shown(host, approved, hidden_git::BRANCH);
    packet.diff_path = beside_file(host, beside).unwrap_or_default();
    packet.both_changed = both_changed;
    let choice = |key: char, label: &str, detail: String| AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: label.to_owned(),
        },
        recommended: false,
        detail: Some(detail),
    };
    let options = vec![
        choice(
            'k',
            KEEP_MINE,
            if one {
                format!("your edit stays, and the run's change to {name} is left out")
            } else {
                format!("your edits stay, and the run's changes to those {name} are left out")
            },
        ),
        choice(
            't',
            TAKE_THE_RUNS,
            if one {
                format!("the run's {name} goes in; yours stays in the history for /rewind")
            } else {
                format!("the run's {name} go in; yours stay in the history for /rewind")
            },
        ),
        AnswerOption {
            key: 'd',
            label: SHOW_BOTH.to_owned(),
            action: AnswerAction::ShowDiff,
            recommended: false,
            detail: Some(format!(
                "your {name} beside the run's, then these choices again"
            )),
        },
        choice(
            'l',
            MERGE_LATER,
            format!("the run pauses here; /resume {run} writes it in"),
        ),
    ];
    let title = if one {
        format!("You and the run both changed {name}. Which version goes in?")
    } else {
        format!("You and the run both changed {name}. Which versions go in?")
    };
    ApprovalRequest {
        at: None,
        kind: ApprovalKind::Merge,
        title,
        task: None,
        packet: ApprovalPacket::Merge(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: vec![EvidenceDocument {
            label: format!("your {name} beside the run's"),
            text: beside.to_owned(),
            path: None,
            sha256: None,
        }],
        options: Some(options),
        free_text: FreeText::Guidance,
        authors: Vec::new(),
    }
}

/// The diff from your versions to the run's, in a file "Show both" opens.
fn beside_file(host: &Host, beside: &str) -> Option<PathBuf> {
    let folder = host.repo().run_dir.join("merge");
    let file = folder.join("both-changed.diff");
    let written =
        create_private_dir_all(&folder).and_then(|()| write_private_file(&file, beside.as_bytes()));
    match written {
        Ok(()) => Some(file),
        Err(err) => {
            tracing::warn!("writing {}: {err}", file.display());
            None
        }
    }
}

/// Says what became of your changes, since the checks ran on the run's work without them.
async fn say(host: &Host, merge: &FolderMerge, side: Option<Side>, range: (&str, &str, &str)) {
    let (main, yours, head) = range;
    let git = host.git();
    let root = &host.repo().root;
    let name = named(&merge.both_changed);
    let text = match side {
        Some(Side::Yours) => format!(
            "Kept your version of {name}, and left the run's change to it out. The run's \
             checks ran with its own version."
        ),
        Some(Side::Runs) => format!(
            "Took the run's version of {name}. Yours is in the history, and /rewind gets it \
             back."
        ),
        None => {
            // The files both changed, in different lines.
            let (Ok(mine), Ok(runs)) = (
                git.diff_files(root, main, yours).await,
                git.diff_files(root, main, head).await,
            ) else {
                return;
            };
            let both: Vec<String> = mine
                .into_iter()
                .map(|(path, ..)| path)
                .filter(|path| runs.iter().any(|(theirs, ..)| theirs == path))
                .collect();
            if both.is_empty() {
                return;
            }
            format!(
                "You changed {} while the run worked, in other lines than it did, so your \
                 edits and the run's are both in. The run's checks ran without yours.",
                named(&both)
            )
        }
    };
    let note = Note {
        level: NoteLevel::Info,
        text,
    };
    // The files are written either way, so a note that can't be recorded doesn't undo that.
    let _ = host.record(None, None, RunEventKind::Note(note));
}
