//! The handoff summary for `context = "note+summary"`: a read-only step with no tools that reads
//! the old session's transcript and writes what the replacement needs to know. It's metered and
//! shown like any other step, and saved as `summary.md` next to the transcript.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, PoisonError, Weak};

use argonauts_core::CardId;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::limits::{HandoffPacket, TranscriptPaths};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::StepName;
use argonauts_core::steps::handoff::{HandoffSummary, HandoffSummaryInput};
use argonauts_core::worker::Access;

use crate::card::support::part;
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::handoff::rules;
use crate::prompts;

pub const SUMMARY_FILE: &str = "summary.md";

/// Assumed when the registry doesn't know the summarizer's context window.
const DEFAULT_WINDOW: u64 = 128_000;
/// Kept free for the prompt, the card, and the answer.
const RESERVED_TOKENS: u64 = 8_000;
/// On the low side, so the tail that's kept fits.
const CHARS_PER_TOKEN: u64 = 3;

/// `handoff.summarizer` when set, else the replacement's own harness with its cheapest model:
/// that vendor is known to have quota, and the stopped one may have none.
pub fn summarizer(host: &Host, set: Option<&WorkerRef>, replacement: &WorkerRef) -> WorkerRef {
    if let Some(worker) = set {
        return worker.clone();
    }
    let cheapest = host
        .services()
        .workers
        .get(replacement)
        .ok()
        .and_then(|worker| worker.cheapest_model());
    match cheapest {
        Some(model) => WorkerRef::new(replacement.worker.clone(), Some(model)),
        None => replacement.clone(),
    }
}

/// The summary of the packet's transcript by `summarizer`. `Ok(Err(why))` when none could be
/// written, and the handoff goes on without one.
pub async fn summarize(
    host: &Host,
    card: &CardId,
    packet: &HandoffPacket,
    summarizer: &WorkerRef,
) -> EngineResult<Result<String, String>> {
    let Some(paths) = &packet.transcript else {
        return Ok(Err("there's no transcript to summarize".to_owned()));
    };
    // A race's attempts start at once, and the first one's summary serves them all.
    let turn = turn(&paths.run);
    let _turn = turn.lock().await;
    if let Some(summary) = saved(paths).await {
        return Ok(Ok(summary));
    }
    if let Some(why) = unusable(host, summarizer) {
        return Ok(Err(why));
    }
    let transcript = match read(paths).await {
        Ok(text) => text,
        Err(err) => return Ok(Err(format!("its transcript can't be read: {err}"))),
    };
    let (transcript, cut) = tail(&transcript, budget(host, summarizer));
    let input = HandoffSummaryInput {
        card: card.to_string(),
        title: packet.card.title.clone(),
        why_stopped: packet.why_stopped.clone(),
        cut,
    };
    let at = host.read(rules::current_step);
    let mut ask = Ask::new(
        at,
        RoleName::Default,
        summarizer.clone(),
        prompts::handoff::SUMMARY,
        input,
    );
    ask.card = Some(card.clone());
    ask.access = Access::NoTools;
    let source = Some(paths.run.display().to_string());
    ask.parts = vec![part(
        InputPartKind::Handoff,
        "transcript",
        transcript,
        source,
    )];
    ask.slot = slot(host, card);
    let summary = match host.ask::<HandoffSummary>(ask).await {
        Ok(answered) => answered.output.summary.trim().to_owned(),
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(err) => return Ok(Err(format!("the summary by {summarizer} failed: {err}"))),
    };
    if summary.is_empty() {
        return Ok(Err(format!("{summarizer} wrote an empty summary")));
    }
    save(paths, &summary).await;
    Ok(Ok(summary))
}

/// Why `summarizer` can't run now, if it can't.
fn unusable(host: &Host, summarizer: &WorkerRef) -> Option<String> {
    let vendor = summarizer.vendor();
    let out = host
        .services()
        .quota
        .get(&vendor)
        .is_some_and(|reading| rules::quota_out(&reading, host.now()));
    if out {
        return Some(format!("{summarizer} is out of quota"));
    }
    (!rules::vendor_usable(host, &vendor)).then(|| format!("{summarizer} isn't working right now"))
}

/// One lock per transcript, dropped once nobody waits on it.
fn turn(transcript: &Path) -> Arc<tokio::sync::Mutex<()>> {
    type Turns = HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>;
    static TURNS: LazyLock<std::sync::Mutex<Turns>> = LazyLock::new(Default::default);
    let mut turns = TURNS.lock().unwrap_or_else(PoisonError::into_inner);
    turns.retain(|_, turn| turn.strong_count() > 0);
    if let Some(turn) = turns.get(transcript).and_then(Weak::upgrade) {
        return turn;
    }
    let turn = Arc::new(tokio::sync::Mutex::new(()));
    turns.insert(transcript.to_owned(), Arc::downgrade(&turn));
    turn
}

/// Each summary is a session of its own, since earlier ones were for other handoffs: a slot
/// past every earlier summary of the card never matches one of them.
fn slot(host: &Host, card: &CardId) -> u32 {
    let earlier = host.read(|state| {
        state
            .sessions
            .by_key
            .values()
            .filter(|session| {
                session.card.as_ref() == Some(card)
                    && session.started.step == Some(StepName::HandoffSummary)
            })
            .count()
    });
    u32::try_from(earlier).unwrap_or(u32::MAX)
}

fn copies(paths: &TranscriptPaths) -> [&Path; 2] {
    [paths.run.as_path(), paths.worktree.as_path()]
}

async fn read(paths: &TranscriptPaths) -> std::io::Result<String> {
    match tokio::fs::read_to_string(&paths.run).await {
        Ok(text) => Ok(text),
        Err(_) => tokio::fs::read_to_string(&paths.worktree).await,
    }
}

/// A summary written earlier for this transcript, such as by another attempt of a race or
/// before a restart.
async fn saved(paths: &TranscriptPaths) -> Option<String> {
    for copy in copies(paths) {
        if let Ok(text) = tokio::fs::read_to_string(copy.with_file_name(SUMMARY_FILE)).await
            && !text.trim().is_empty()
        {
            return Some(text.trim().to_owned());
        }
    }
    None
}

/// Next to each copy of the transcript there is. The summary is in the note either way, so a
/// failed write only costs a later attempt its reuse.
async fn save(paths: &TranscriptPaths, summary: &str) {
    let text = format!("{summary}\n");
    for copy in copies(paths) {
        let Some(dir) = copy.parent().filter(|dir| dir.is_dir()) else {
            continue;
        };
        let path = dir.join(SUMMARY_FILE);
        if let Err(error) = tokio::fs::write(&path, &text).await {
            tracing::warn!(path = %path.display(), %error, "the handoff summary wasn't saved");
        }
    }
}

/// How much transcript the summarizer's context holds, in bytes.
fn budget(host: &Host, summarizer: &WorkerRef) -> usize {
    let window = host
        .services()
        .registry
        .get(summarizer)
        .and_then(|entry| entry.context_window)
        .unwrap_or(DEFAULT_WINDOW);
    let tokens = window.saturating_sub(RESERVED_TOKENS.min(window / 4));
    usize::try_from(tokens.saturating_mul(CHARS_PER_TOKEN)).unwrap_or(usize::MAX)
}

/// The transcript's tail that fits in `max` bytes, from the start of a line, and whether
/// anything was cut.
fn tail(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    if let Some(newline) = text[start..].find('\n') {
        start += newline + 1;
    }
    let lines = text[..start].lines().count();
    let kept = &text[start..];
    (
        format!("… the first {lines} lines are cut, so this is the transcript's tail\n\n{kept}"),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transcript_that_fits_is_whole() {
        let (text, cut) = tail("# card-001\n\nRead src/cli.rs\n", 100);
        assert_eq!(text, "# card-001\n\nRead src/cli.rs\n");
        assert!(!cut);
    }

    #[test]
    fn a_long_transcript_keeps_its_tail_from_a_line() {
        let transcript = "first line\nsecond line\nthird line\nlast line\n";
        let (text, cut) = tail(transcript, 18);
        assert!(cut);
        assert!(text.starts_with("… the first 3 lines are cut"), "{text}");
        assert!(text.ends_with("\n\nlast line\n"), "{text}");
    }

    #[test]
    fn the_cut_never_splits_a_character() {
        let transcript = "ééééé\nok\n";
        let (text, cut) = tail(transcript, 5);
        assert!(cut);
        assert!(text.ends_with("ok\n"), "{text}");
    }
}
