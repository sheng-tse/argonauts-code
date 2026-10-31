//! What a fallback is told when a task changes hands or its session rotates
//! (`fallback.context`): the task only, the fallback note, the note and the old session's
//! transcript, or the note with a summary of that transcript written into it.

use std::fmt::Write as _;

use argonauts_core::base::names::ModelRef;
use argonauts_core::models::limits::FallbackNote;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::task::TryCause;
use argonauts_core::settings::fallback::{AppliedFallback, FallbackContext};
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::fallback::{rules, summary};
use crate::state::tasks::{TaskEntry, TaskProgress};
use crate::task::support::newest_worker_session;

/// The note a fresh session starts from: the note as it's told, saved as JSON, and the text
/// the session reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Told {
    pub note: FallbackNote,
    pub json: String,
    pub text: String,
}

/// The worker whose stop this fresh session of `to` carries on from. Its agent picks the
/// `fallback.replace` table that applies. `last` ran the task's newest build session, the one
/// the transcript is of.
pub fn stopped(
    record: &TaskEntry,
    progress: &TaskProgress,
    last: Option<&ModelRef>,
    to: &ModelRef,
    cause: TryCause,
) -> ModelRef {
    if cause == TryCause::Rotation {
        return to.clone();
    }
    match rules::worker(record) {
        // A race try: the task stays with the one that stopped until a try wins.
        Some(worker) if worker != *to => worker,
        // Who had the task before its newest reassignment may not be who stopped last, as
        // when the new worker tries again after a failure of its own.
        _ => last
            .or(progress.reassigned_from.as_ref())
            .cloned()
            .unwrap_or_else(|| to.clone()),
    }
}

/// The fallback rules for a fresh session of `to`, after `cause`.
pub fn applied(
    host: &Host,
    record: &TaskEntry,
    progress: &TaskProgress,
    to: &ModelRef,
    cause: TryCause,
) -> AppliedFallback {
    let last = host.read(|state| {
        newest_worker_session(state, &record.spec.id).map(|session| session.started.worker.clone())
    });
    let stopped = stopped(record, progress, last.as_ref(), to, cause);
    rules::applied_for(host, &record.spec, &stopped)
}

/// Fills in what the context needs and returns the one that holds. For `note+summary` that's
/// the summary, and when none can be written the fallback goes on as `note+transcript`, with a
/// notice saying why.
pub async fn prepare(
    host: &Host,
    task: &TaskId,
    to: &ModelRef,
    rules: &AppliedFallback,
    note: &mut FallbackNote,
) -> EngineResult<FallbackContext> {
    if rules.context != FallbackContext::NoteAndSummary || note.summary.is_some() {
        return Ok(rules.context);
    }
    let summarizer = summary::summarizer(host, rules.summarizer.as_ref(), to);
    match summary::summarize(host, task, note, &summarizer).await? {
        Ok(text) => {
            note.summary = Some(text);
            Ok(FallbackContext::NoteAndSummary)
        }
        Err(why) => {
            let fallback = FallbackContext::NoteAndTranscript;
            let text = format!("{task}'s fallback goes on as {}: {why}", fallback.label());
            host.record(
                Some(task),
                None,
                RunEventKind::Note(Note {
                    level: NoteLevel::Warn,
                    text,
                }),
            )?;
            Ok(fallback)
        }
    }
}

/// The note for `context`, or `None` for `fresh`, which gives the task only. The note names
/// the transcript only for `note+transcript` and carries the summary only for `note+summary`.
pub fn tell(context: FallbackContext, mut note: FallbackNote) -> EngineResult<Option<Told>> {
    let transcript = note.transcript.take();
    match context {
        FallbackContext::Fresh => return Ok(None),
        FallbackContext::Note | FallbackContext::NoteAndTranscript => note.summary = None,
        FallbackContext::NoteAndSummary => {}
    }
    let json = serde_json::to_string_pretty(&note)?;
    let mut text = format!(
        "This task fell back to you from another session. It stopped because: {}. \
         Its work so far is committed on your branch. Here is what it did:\n\n{json}",
        note.why_stopped
    );
    if context == FallbackContext::NoteAndTranscript
        && let Some(paths) = transcript
    {
        let _ = write!(
            text,
            "\n\nThe old session's whole transcript is at {}. Read it only if you need more \
             detail than this note gives.",
            paths.worktree.display()
        );
    }
    Ok(Some(Told { note, json, text }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::DiffStat;
    use argonauts_core::models::limits::{TranscriptPaths, WorkSoFar};
    use argonauts_core::runs::events::task::{TaskKind, TaskLimit, TaskSpec};

    use super::*;

    fn worker(name: &str) -> ModelRef {
        name.parse().expect("a valid model")
    }

    fn spec() -> TaskSpec {
        TaskSpec {
            id: TaskId::numbered(3),
            title: "Middleware".to_owned(),
            task: "Add the rate limiter".to_owned(),
            kind: TaskKind::Build,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some(worker("claude-code/opus")),
            reviewers: Vec::new(),
            limit: TaskLimit::default(),
            predictions: None,
            item: None,
            way: None,
        }
    }

    fn note() -> FallbackNote {
        FallbackNote {
            task: spec(),
            goal_sha256: None,
            notes: Vec::new(),
            work: WorkSoFar {
                branch: "argonauts/run/task-003".to_owned(),
                commit: None,
                stat: DiffStat::default(),
            },
            happened: vec!["Edited src/limit.rs (+14 −2)".to_owned()],
            last_todo: Vec::new(),
            checks: Vec::new(),
            why_stopped: "usage limit".to_owned(),
            last_words: None,
            last_message: None,
            limit_left: None,
            transcript: Some(TranscriptPaths {
                worktree: PathBuf::from("/tree/.argonauts/fallback/1/transcript.md"),
                run: PathBuf::from("/runs/run/tasks/task-003/fallback-1/transcript.md"),
            }),
            summary: Some("It tried a token bucket.".to_owned()),
        }
    }

    fn record() -> TaskEntry {
        TaskEntry {
            spec: spec(),
            state: argonauts_core::runs::events::task::TaskState::NeedsDecision,
            tries: 1,
            worker: None,
            reassigned: 0,
            worktree: None,
            branch: None,
            base: None,
            commits: Vec::new(),
            reports: Vec::new(),
            checks: Vec::new(),
            reviews: Vec::new(),
            fallbacks: 0,
            debt: None,
        }
    }

    #[test]
    fn fresh_tells_nothing() {
        assert_eq!(tell(FallbackContext::Fresh, note()).unwrap(), None);
    }

    #[test]
    fn note_leaves_out_the_transcript_and_the_summary() {
        let told = tell(FallbackContext::Note, note()).unwrap().unwrap();
        assert_eq!(told.note.transcript, None);
        assert_eq!(told.note.summary, None);
        assert!(told.text.contains("Edited src/limit.rs"));
        assert!(!told.text.contains("transcript"), "{}", told.text);
        assert!(!told.text.contains("token bucket"));
    }

    #[test]
    fn note_and_transcript_names_the_worktree_copy() {
        let told = tell(FallbackContext::NoteAndTranscript, note())
            .unwrap()
            .unwrap();
        assert!(told.text.ends_with(
            "The old session's whole transcript is at /tree/.argonauts/fallback/1/transcript.md. \
             Read it only if you need more detail than this note gives."
        ));
        assert!(!told.text.contains("/runs/"));
        assert!(!told.text.contains("token bucket"));
    }

    #[test]
    fn note_and_summary_carries_the_summary_in_the_note() {
        let told = tell(FallbackContext::NoteAndSummary, note())
            .unwrap()
            .unwrap();
        assert_eq!(
            told.note.summary.as_deref(),
            Some("It tried a token bucket.")
        );
        assert!(told.json.contains("It tried a token bucket."));
        assert!(!told.text.contains("transcript.md"));
    }

    #[test]
    fn the_stopped_worker_is_the_one_before_the_new_worker() {
        let opus = worker("claude-code/opus");
        let codex = worker("codex/gpt-5.6");
        let mut record = record();
        let mut progress = TaskProgress::default();
        // A race try, before any try won, even once another try's session started.
        assert_eq!(
            stopped(&record, &progress, None, &codex, TryCause::Fallback),
            opus
        );
        assert_eq!(
            stopped(&record, &progress, Some(&codex), &codex, TryCause::Fallback),
            opus
        );
        record.worker = Some(codex.clone());
        progress.reassigned_from = Some(opus.clone());
        for last in [None, Some(&opus)] {
            assert_eq!(
                stopped(&record, &progress, last, &codex, TryCause::Reassigned),
                opus
            );
        }
        // A full context rotates to the same worker, whose own rules apply.
        assert_eq!(
            stopped(&record, &progress, Some(&codex), &codex, TryCause::Rotation),
            codex
        );
        // The new worker tries again after its own session failed.
        assert_eq!(
            stopped(
                &record,
                &progress,
                Some(&codex),
                &codex,
                TryCause::AfterFailure
            ),
            codex
        );
    }
}
