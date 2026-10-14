//! The message a run's change lands with, which the merge agent writes while the merge waits
//! for your approval: what the change does, never how it was made. It's kept with the change
//! it describes, so a landing takes it only for that same change, and the message plain code
//! writes stands in whenever there is none.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use argonauts_core::events::session::{EndReason, InputPart, InputPartKind};
use argonauts_core::hash::sha256_hex;
use argonauts_core::names::RoleName;
use argonauts_core::settings::delivery::DeliveryCommits;
use argonauts_core::steps::last_json_object;
use argonauts_core::worker::Access;

use crate::card::support::part;
use crate::engine::host::{Host, SessionPlan, StandIns};
use crate::error::{EngineError, EngineResult};
use crate::git::GitError;
use crate::integrate::{final_check, merge_packet, merger};
use crate::labels::card_label;
use crate::prompts;

/// A message the merge agent wrote, and the change it's for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Kept {
    /// The SHA-256 of the change's diff, from your branch to the run's. Your branch moving on
    /// leaves it the same while the run's own change is.
    change: String,
    subject: String,
    body: String,
}

/// What the merge agent answers.
#[derive(Debug, Deserialize)]
struct Answer {
    subject: String,
    #[serde(default)]
    body: String,
}

/// Has the merge agent write the message for the change as it stands, unless it already did.
/// A merge agent that can't, or answers with no subject, leaves the message to plain code.
pub async fn write(host: &Host) -> EngineResult<()> {
    // The run's history as it happened lands with the messages it has.
    if host.settings().delivery.commits == DeliveryCommits::AsRun {
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
    let change = change_of(host, &worktree, &main, &head).await?;
    if read(host).await.is_some_and(|kept| kept.change == change) {
        return Ok(());
    }
    let mut plan = SessionPlan {
        at: host.read(merger::merge_step),
        role: RoleName::Worker,
        worker: merger::seat(host.settings()),
        card: None,
        access: Access::ReadOnly,
        step: None,
        workdir: worktree.clone(),
        parts: parts(host, &worktree, &main, &head).await?,
        json_schema: Some(schema()),
        resume: None,
        slot: 0,
        label: Some(format!("merge:{target}")),
        reviews: false,
        fresh: true,
    };
    plan.slot = host.slot_for(&plan);
    let ran = match host.run_session(plan, StandIns::None).await {
        Ok(ran) => ran,
        // A merge agent that can't start leaves the message to code, like one that fails.
        Err(EngineError::Worker(_)) => return Ok(()),
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

/// The merge agent's message for the change from `main` to `head`, when it wrote one for
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

fn answer_of(end: &argonauts_core::events::session::SessionEnd) -> Option<Answer> {
    if end.reason != EndReason::Completed {
        return None;
    }
    let value = end
        .structured
        .clone()
        .filter(Value::is_object)
        .or_else(|| end.result_text.as_deref().and_then(last_json_object))?;
    serde_json::from_value::<Answer>(value)
        .ok()
        .filter(|answer| !answer.subject.trim().is_empty())
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "subject": {
                "type": "string",
                "description": "What the change does, in the imperative, under 72 characters."
            },
            "body": {
                "type": "string",
                "description": "What changed and why. Empty when the subject says it all."
            }
        },
        "required": ["subject", "body"],
        "additionalProperties": false
    })
}

/// The instructions, then what the change is: its title, the tasks in it, and its files.
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
            .cards
            .by_id
            .iter()
            .filter(|(_, record)| merge_packet::in_the_merge(record))
            .map(|(id, record)| format!("- {}: {}", card_label(state, id), record.spec.title))
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
    change.push_str(&format!("\nThe diff: git diff {main} {head}"));
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
