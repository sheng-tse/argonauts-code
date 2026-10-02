//! What a replacement is told when a card changes hands or its session rotates
//! (`handoff.context`): the card only, the handoff note, the note and the old session's
//! transcript, or the note with a summary of that transcript written into it.

use std::fmt::Write as _;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::AttemptCause;
use argonauts_core::limits::HandoffPacket;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::handoff::{AppliedHandoff, HandoffContext};
use argonauts_core::{CardId, Note, NoteLevel};

use crate::card::support::newest_owner_session;
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::handoff::{rules, summary};
use crate::state::cards::{CardProgress, CardRecord};

/// The note a fresh session starts from: the packet as it's told, saved as JSON, and the text
/// the session reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Told {
    pub packet: HandoffPacket,
    pub json: String,
    pub text: String,
}

/// The worker whose stop this fresh session of `to` carries on from. Its vendor picks the
/// `handoff.replace` table that applies. `last` ran the card's newest build session, the one
/// the transcript is of.
pub fn stopped(
    record: &CardRecord,
    progress: &CardProgress,
    last: Option<&WorkerRef>,
    to: &WorkerRef,
    cause: AttemptCause,
) -> WorkerRef {
    if cause == AttemptCause::Rotation {
        return to.clone();
    }
    match rules::owner(record) {
        // A race attempt: the card stays with the one that stopped until an attempt wins.
        Some(owner) if owner != *to => owner,
        // Who had the card before its newest reassignment may not be who stopped last, as
        // when the new owner tries again after a failure of its own.
        _ => last
            .or(progress.handed_from.as_ref())
            .cloned()
            .unwrap_or_else(|| to.clone()),
    }
}

/// The handoff rules for a fresh session of `to`, after `cause`.
pub fn applied(
    host: &Host,
    record: &CardRecord,
    progress: &CardProgress,
    to: &WorkerRef,
    cause: AttemptCause,
) -> AppliedHandoff {
    let last = host.read(|state| {
        newest_owner_session(state, &record.spec.id).map(|session| session.started.worker.clone())
    });
    let stopped = stopped(record, progress, last.as_ref(), to, cause);
    rules::applied_for(host, &record.spec, &stopped)
}

/// Fills in what the context needs and returns the one that holds. For `note+summary` that's
/// the summary, and when none can be written the handoff goes on as `note+transcript`, with a
/// notice saying why.
pub async fn prepare(
    host: &Host,
    card: &CardId,
    to: &WorkerRef,
    rules: &AppliedHandoff,
    packet: &mut HandoffPacket,
) -> EngineResult<HandoffContext> {
    if rules.context != HandoffContext::NoteAndSummary || packet.summary.is_some() {
        return Ok(rules.context);
    }
    let summarizer = summary::summarizer(host, rules.summarizer.as_ref(), to);
    match summary::summarize(host, card, packet, &summarizer).await? {
        Ok(text) => {
            packet.summary = Some(text);
            Ok(HandoffContext::NoteAndSummary)
        }
        Err(why) => {
            let fallback = HandoffContext::NoteAndTranscript;
            let text = format!("{card}'s handoff goes on as {}: {why}", fallback.label());
            host.record(
                Some(card),
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

/// The note for `context`, or `None` for `fresh`, which gives the card only. The note names
/// the transcript only for `note+transcript` and carries the summary only for `note+summary`.
pub fn tell(context: HandoffContext, mut packet: HandoffPacket) -> EngineResult<Option<Told>> {
    let transcript = packet.transcript.take();
    match context {
        HandoffContext::Fresh => return Ok(None),
        HandoffContext::Note | HandoffContext::NoteAndTranscript => packet.summary = None,
        HandoffContext::NoteAndSummary => {}
    }
    let json = serde_json::to_string_pretty(&packet)?;
    let mut text = format!(
        "You're taking over this card from another session. It stopped because: {}. \
         Its work so far is committed on your branch. Here is what it did:\n\n{json}",
        packet.why_stopped
    );
    if context == HandoffContext::NoteAndTranscript
        && let Some(paths) = transcript
    {
        let _ = write!(
            text,
            "\n\nThe old session's whole transcript is at {}. Read it only if you need more \
             detail than this note gives.",
            paths.worktree.display()
        );
    }
    Ok(Some(Told { packet, json, text }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::DiffStat;
    use argonauts_core::events::card::{CardBudget, CardKind, CardSpec};
    use argonauts_core::limits::{TranscriptPaths, WorkSoFar};

    use super::*;

    fn worker(name: &str) -> WorkerRef {
        name.parse().expect("a valid worker")
    }

    fn spec() -> CardSpec {
        CardSpec {
            id: CardId::numbered(3),
            title: "Middleware".to_owned(),
            task: "Add the rate limiter".to_owned(),
            kind: CardKind::Build,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            owner: Some(worker("claude-code/opus")),
            reviewers: Vec::new(),
            budget: CardBudget::default(),
            predictions: None,
        }
    }

    fn packet() -> HandoffPacket {
        HandoffPacket {
            card: spec(),
            goal_sha256: None,
            notes: Vec::new(),
            work: WorkSoFar {
                branch: "argonauts/run/card-003".to_owned(),
                commit: None,
                stat: DiffStat::default(),
            },
            happened: vec!["Edited src/limit.rs (+14 −2)".to_owned()],
            last_todo: Vec::new(),
            gates: Vec::new(),
            why_stopped: "usage limit".to_owned(),
            last_words: None,
            last_message: None,
            budget_left: None,
            transcript: Some(TranscriptPaths {
                worktree: PathBuf::from("/tree/.argonauts/handoff/1/transcript.md"),
                run: PathBuf::from("/runs/run/cards/card-003/handoff-1/transcript.md"),
            }),
            summary: Some("It tried a token bucket.".to_owned()),
        }
    }

    fn record() -> CardRecord {
        CardRecord {
            spec: spec(),
            state: argonauts_core::events::card::CardState::NeedsDecision,
            attempts: 1,
            owner: None,
            reassigned: 0,
            worktree: None,
            branch: None,
            base: None,
            commits: Vec::new(),
            reports: Vec::new(),
            gates: Vec::new(),
            reviews: Vec::new(),
            handoffs: 0,
            debt: None,
        }
    }

    #[test]
    fn fresh_tells_nothing() {
        assert_eq!(tell(HandoffContext::Fresh, packet()).unwrap(), None);
    }

    #[test]
    fn note_leaves_out_the_transcript_and_the_summary() {
        let told = tell(HandoffContext::Note, packet()).unwrap().unwrap();
        assert_eq!(told.packet.transcript, None);
        assert_eq!(told.packet.summary, None);
        assert!(told.text.contains("Edited src/limit.rs"));
        assert!(!told.text.contains("transcript"), "{}", told.text);
        assert!(!told.text.contains("token bucket"));
    }

    #[test]
    fn note_and_transcript_names_the_worktree_copy() {
        let told = tell(HandoffContext::NoteAndTranscript, packet())
            .unwrap()
            .unwrap();
        assert!(told.text.ends_with(
            "The old session's whole transcript is at /tree/.argonauts/handoff/1/transcript.md. \
             Read it only if you need more detail than this note gives."
        ));
        assert!(!told.text.contains("/runs/"));
        assert!(!told.text.contains("token bucket"));
    }

    #[test]
    fn note_and_summary_carries_the_summary_in_the_note() {
        let told = tell(HandoffContext::NoteAndSummary, packet())
            .unwrap()
            .unwrap();
        assert_eq!(
            told.packet.summary.as_deref(),
            Some("It tried a token bucket.")
        );
        assert!(told.json.contains("It tried a token bucket."));
        assert!(!told.text.contains("transcript.md"));
    }

    #[test]
    fn the_stopped_worker_is_the_one_before_the_new_owner() {
        let opus = worker("claude-code/opus");
        let codex = worker("codex/gpt-5.6");
        let mut record = record();
        let mut progress = CardProgress::default();
        // A race attempt, before any attempt won, even once another attempt's session started.
        assert_eq!(
            stopped(&record, &progress, None, &codex, AttemptCause::Handoff),
            opus
        );
        assert_eq!(
            stopped(
                &record,
                &progress,
                Some(&codex),
                &codex,
                AttemptCause::Handoff
            ),
            opus
        );
        record.owner = Some(codex.clone());
        progress.handed_from = Some(opus.clone());
        for last in [None, Some(&opus)] {
            assert_eq!(
                stopped(&record, &progress, last, &codex, AttemptCause::Reassigned),
                opus
            );
        }
        // A full context rotates to the same worker, whose own rules apply.
        assert_eq!(
            stopped(
                &record,
                &progress,
                Some(&codex),
                &codex,
                AttemptCause::Rotation
            ),
            codex
        );
        // The new owner tries again after its own session failed.
        assert_eq!(
            stopped(
                &record,
                &progress,
                Some(&codex),
                &codex,
                AttemptCause::AfterFailure
            ),
            codex
        );
    }
}
