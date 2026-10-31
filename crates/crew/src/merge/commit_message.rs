//! The message a run's change lands with, which the merger writes while the merge waits
//! for your approval: what the change does, never how it was made. It's kept with the change
//! it describes, so a landing takes it only for that same change, and the message plain code
//! writes is used whenever there is none.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use argonauts_core::agents::session_runner::{Access, SessionControl};
use argonauts_core::app::views::COMMIT_MESSAGE;
use argonauts_core::base::hash::sha256_hex;
use argonauts_core::base::names::{RoleName, SessionKey};
use argonauts_core::runs::events::session::{EndReason, InputPart, InputPartKind};
use argonauts_core::settings::delivery::DeliveryCommits;
use argonauts_core::workflows::steps::task::CommitMessage;
use argonauts_core::workflows::steps::{StepName, last_json_object};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Fallbacks, Host, SessionPlan};
use crate::engine::shared::lock;
use crate::git::GitError;
use crate::merge::{final_check, merge_packet, merger, your_folder};
use crate::prompts;
use crate::sessions::labels::task_label;
use crate::task::support::part;

/// A message the merger wrote, and the change it's for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Kept {
    /// The SHA-256 of the change's diff, from your branch to the run's. Your branch moving on
    /// leaves it the same while the run's own change is.
    change: String,
    subject: String,
    body: String,
}

/// The most of a diff the merger is given, in characters. A longer one is cut there, and the
/// file list before it stays whole.
const DIFF_CHARS: usize = 60_000;

/// Lets a landing go ahead without the message. Once told to stop, the merger's session ends,
/// now or as soon as it starts, and the change lands with the message plain code writes.
#[derive(Debug, Default)]
pub struct Writing {
    stopped: AtomicBool,
    session: Mutex<Option<SessionKey>>,
}

impl Writing {
    pub fn stop(&self, host: &Host) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(session) = lock(&self.session).as_ref() {
            // It may have ended by itself meanwhile, which is as good.
            let _ = host.control(session, SessionControl::Stop);
        }
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn started(&self, host: &Host, session: &SessionKey) {
        *lock(&self.session) = Some(session.clone());
        if self.stopped() {
            let _ = host.control(session, SessionControl::Stop);
        }
    }
}

/// Has the merger write the message for the change as it stands, unless it already did.
/// A merger that can't, answers with no subject, or is stopped through `writing` leaves the
/// message to plain code.
pub async fn write(host: &Host, writing: &Writing) -> EngineResult<()> {
    // The run's history as it happened lands with the messages it has.
    if host.settings().delivery.commits == DeliveryCommits::AsRun {
        return Ok(());
    }
    // Only /rewind reads the history of a folder without git, so plain code's line does.
    if your_folder::is(host) {
        return Ok(());
    }
    let Some(target) = final_check::lands_on(host).await? else {
        return Ok(());
    };
    let git = host.git();
    let (_, worktree) = merge_packet::landing_branch(host).await?;
    let main = final_check::main_commit(host, &target).await?;
    let head = git.resolve(&worktree, "HEAD").await?;
    // Until your branch's newer commits are in, the diff would read as undoing them.
    if main == head || !git.is_ancestor(&worktree, &main, &head).await? {
        return Ok(());
    }
    // What lands of a run that started from your uncommitted edits holds none of them, and
    // the message is for what lands.
    let (main, head) =
        merge_packet::shown_range(host, &worktree, Some(&main), &main, &head).await?;
    let change = change_of(host, &worktree, &main, &head).await?;
    if read(host).await.is_some_and(|kept| kept.change == change) {
        return Ok(());
    }
    let plan = SessionPlan {
        without_status: false,
        at: host.read(merger::merge_step),
        role: RoleName::Worker,
        worker: merger::model(host.settings()),
        task: None,
        // The change is in its prompt, so it reads nothing and runs nothing.
        access: Access::NoTools,
        step: Some(StepName::CommitMessage),
        workdir: worktree.clone(),
        parts: parts(host, &worktree, &main, &head).await?,
        json_schema: StepName::CommitMessage.output_schema(),
        resume: None,
        slot: 0,
        label: Some(COMMIT_MESSAGE.to_owned()),
        reviews: false,
        fresh: true,
        independent_of: Vec::new(),
        in_place_of: None,
        fork_of: None,
    };
    if writing.stopped() {
        return Ok(());
    }
    let started = |session: &SessionKey| {
        writing.started(host, session);
        Ok(())
    };
    let ran = host
        .run_session_in_turn(plan, None, Fallbacks::None, &started)
        .await;
    let ran = match ran {
        Ok(ran) => ran,
        // A merger that can't start leaves the message to code, like one that fails.
        Err(EngineError::Runner(_)) => return Ok(()),
        Err(err) => return Err(err),
    };
    let Some(answer) = answer_of(&ran.end) else {
        return Ok(());
    };
    let kept = Kept {
        change,
        subject: answer.subject.trim().to_owned(),
        body: answer.body.trim().to_owned(),
    };
    let path = path(host);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(&path, serde_json::to_vec_pretty(&kept)?).await?;
    Ok(())
}

/// The merger's message for the change from `main` to `head`, when it wrote one for
/// exactly that change: its subject, then its body after a blank line.
pub async fn for_change(
    host: &Host,
    dir: &Path,
    main: &str,
    head: &str,
) -> Result<Option<String>, GitError> {
    let Some(kept) = read(host).await else {
        return Ok(None);
    };
    if kept.change != change_of(host, dir, main, head).await? {
        return Ok(None);
    }
    Ok(Some(if kept.body.is_empty() {
        kept.subject
    } else {
        format!("{}\n\n{}", kept.subject, kept.body)
    }))
}

async fn change_of(host: &Host, dir: &Path, main: &str, head: &str) -> Result<String, GitError> {
    let diff = host.git().diff(dir, main, head).await?;
    Ok(sha256_hex(diff.as_bytes()))
}

/// A missing or unreadable message only means plain code writes it.
async fn read(host: &Host) -> Option<Kept> {
    let bytes = tokio::fs::read(path(host)).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn path(host: &Host) -> PathBuf {
    host.repo().run_dir.join("merge").join("message.json")
}

fn answer_of(end: &argonauts_core::runs::events::session::SessionEnd) -> Option<CommitMessage> {
    if end.reason != EndReason::Completed {
        return None;
    }
    let value = end
        .structured
        .clone()
        .filter(Value::is_object)
        .or_else(|| end.result_text.as_deref().and_then(last_json_object))?;
    serde_json::from_value::<CommitMessage>(value)
        .ok()
        .filter(|answer| !answer.subject.trim().is_empty())
}

/// The instructions, then what the change is: its title, the tasks in it, its files, and its
/// diff.
async fn parts(
    host: &Host,
    worktree: &Path,
    main: &str,
    head: &str,
) -> Result<Vec<InputPart>, GitError> {
    let files = host.git().diff_files(worktree, main, head).await?;
    let (title, tasks) = host.read(|state| {
        let title = state
            .run
            .started
            .as_ref()
            .map(|started| started.title.clone())
            .unwrap_or_default();
        let tasks: Vec<String> = state
            .tasks
            .by_id
            .iter()
            .filter(|(_, record)| merge_packet::in_the_merge(record))
            .map(|(id, record)| format!("- {}: {}", task_label(state, id), record.spec.title))
            .collect();
        (title, tasks)
    });
    let mut change = format!("Asked for: {title}\n");
    if !tasks.is_empty() {
        change.push_str(&format!("\nWhat it holds:\n{}\n", tasks.join("\n")));
    }
    change.push_str("\nFiles:\n");
    for (path, insertions, deletions) in &files {
        change.push_str(&format!("- {path} (+{insertions} -{deletions})\n"));
    }
    let diff = host.git().diff(worktree, main, head).await?;
    change.push_str("\nThe diff:\n");
    match diff.char_indices().nth(DIFF_CHARS) {
        Some((cut, _)) => {
            change.push_str(&diff[..cut]);
            change.push_str(&format!(
                "\n\nThe diff goes on. It is cut here, after {DIFF_CHARS} characters; the file \
                 list above is whole."
            ));
        }
        None => change.push_str(&diff),
    }
    Ok(vec![
        part(
            InputPartKind::Prompt,
            "merger",
            prompts::files::text(host, prompts::merge::COMMIT_MESSAGE),
            None,
        ),
        part(InputPartKind::Message, "change", change, None),
    ])
}
