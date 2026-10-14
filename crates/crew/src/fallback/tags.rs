//! Checkpoint tags at each fallback. A tag stays put while the task's branch moves. Its message
//! records the time, the original worker, why it stopped, and who it fell back to.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::task::TaskEvent;
use argonauts_core::fallback::CheckpointTag;
use argonauts_core::names::ModelRef;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::fallback::rules;

/// Tags the task's last commit as the next `fallback-<n>` and records `Task::CheckpointTagged`.
pub async fn tag(
    host: &Host,
    task: &TaskId,
    from: &ModelRef,
    to: &[ModelRef],
    reason: &str,
) -> EngineResult<CheckpointTag> {
    let record = rules::record(host, task)?;
    let branch = record
        .branch
        .ok_or_else(|| EngineError::Refused(format!("{task} has no branch to tag yet")))?;
    let root = host.repo().root.clone();
    let commit = host.git().resolve(&root, &branch).await?;
    let tag = CheckpointTag {
        run: host.run_id().clone(),
        task: task.clone(),
        n: record.fallbacks + 1,
    };
    let text = message(host.now(), from, to, reason);
    host.git()
        .tag(&root, &tag.tag_name(), &commit, &text)
        .await?;
    host.record(
        Some(task),
        None,
        RunEventKind::Task(TaskEvent::CheckpointTagged {
            n: tag.n,
            commit,
            from: from.clone(),
            to: to.to_vec(),
            reason: reason.to_owned(),
        }),
    )?;
    Ok(tag)
}

/// The tag's message: when, who stopped, why, and who it fell back to.
pub fn message(at: OffsetDateTime, from: &ModelRef, to: &[ModelRef], reason: &str) -> String {
    let when = at.format(&Rfc3339).unwrap_or_else(|_| at.to_string());
    let to = to
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("Fallback checkpoint\n\ntime: {when}\nfrom: {from}\nreason: {reason}\nto: {to}\n")
}

/// The task's checkpoints and the commit each one marks, oldest first, for `/rewind`.
pub async fn list(host: &Host, task: &TaskId) -> EngineResult<Vec<(CheckpointTag, String)>> {
    let record = rules::record(host, task)?;
    let root = host.repo().root.clone();
    let mut found = Vec::new();
    for n in 1..=record.fallbacks {
        let tag = CheckpointTag {
            run: host.run_id().clone(),
            task: task.clone(),
            n,
        };
        if let Some(commit) = host.git().tag_target(&root, &tag.tag_name()).await? {
            found.push((tag, commit));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn the_message_says_when_who_why_and_to_whom() {
        let text = message(
            datetime!(2026-09-30 15:45 UTC),
            &ModelRef::new("claude-code", Some("opus".into())),
            &[
                ModelRef::new("codex", Some("gpt-5.6".into())),
                ModelRef::new("agy", None),
            ],
            "usage limit, resets at 3:45 PM",
        );
        assert!(text.contains("time: 2026-09-30T15:45:00Z"), "{text}");
        assert!(text.contains("from: claude-code/opus"));
        assert!(text.contains("reason: usage limit, resets at 3:45 PM"));
        assert!(text.contains("to: codex/gpt-5.6, agy"));
    }
}
