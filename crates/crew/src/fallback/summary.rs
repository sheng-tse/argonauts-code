//! The fallback summary for `context = "note+summary"`: a read-only step with no tools that reads
//! the old session's transcript and writes what the fallback needs to know. It's metered and
//! shown like any other step, and saved as `summary.md` next to the transcript.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, PoisonError, Weak};

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::models::limits::{FallbackNote, TranscriptPaths};
use argonauts_core::models::registry::{ModelAccess, ModelEntry};
use argonauts_core::runs::events::session::InputPartKind;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::steps::fallback::{FallbackSummary, FallbackSummaryInput};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::fallback::rules;
use crate::prompts;
use crate::task::support::part;

pub const SUMMARY_FILE: &str = "summary.md";

/// Assumed when no model's window is known at all: the least a local server gives a model
/// with no cap of its own (Ollama's 4,096 on a machine with little memory), so a transcript
/// never overflows the model that reads it.
const LEAST_WINDOW: u64 = 4_096;
/// Kept free for the prompt, the question, and the answer.
const RESERVED_TOKENS: u64 = 8_000;
/// On the low side, so the tail that's kept fits.
const CHARS_PER_TOKEN: u64 = 3;

/// `fallback.summarizer` when set, else the fallback's own CLI with its cheapest model:
/// that agent is known to have plan usage, and the stopped one may have none.
pub fn summarizer(host: &Host, set: Option<&ModelRef>, fallback: &ModelRef) -> ModelRef {
    if let Some(worker) = set {
        return worker.clone();
    }
    let cheapest = host
        .services()
        .workers
        .get(fallback)
        .ok()
        .and_then(|worker| worker.cheapest_model());
    match cheapest {
        Some(model) => ModelRef::new(fallback.agent.clone(), Some(model)),
        None => fallback.clone(),
    }
}

/// The summary of the note's transcript by `summarizer`. `Ok(Err(why))` when none could be
/// written, and the fallback goes on without one.
pub async fn summarize(
    host: &Host,
    task: &TaskId,
    note: &FallbackNote,
    summarizer: &ModelRef,
) -> EngineResult<Result<String, String>> {
    let Some(paths) = &note.transcript else {
        return Ok(Err("there's no transcript to summarize".to_owned()));
    };
    // A race's tries start at once, and the first one's summary serves them all.
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
    let (transcript, cut) = tail(&transcript, limit(host, summarizer));
    let input = FallbackSummaryInput {
        task: task.to_string(),
        title: note.task.title.clone(),
        why_stopped: note.why_stopped.clone(),
        cut,
    };
    let at = host.read(rules::current_step);
    let mut ask = Ask::new(
        at,
        RoleName::Main,
        summarizer.clone(),
        prompts::fallback::SUMMARY,
        input,
    );
    ask.task = Some(task.clone());
    ask.access = Access::NoTools;
    let source = Some(paths.run.display().to_string());
    ask.parts = vec![part(
        InputPartKind::Fallback,
        "transcript",
        transcript,
        source,
    )];
    ask.slot = slot(host, task);
    let summary = match host.ask::<FallbackSummary>(ask).await {
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
fn unusable(host: &Host, summarizer: &ModelRef) -> Option<String> {
    let agent_id = summarizer.agent_id();
    let out = host
        .services()
        .plan_usage
        .get(&agent_id)
        .is_some_and(|reading| rules::plan_usage_out(&reading, host.now()));
    if out {
        return Some(format!("{summarizer} is out of plan usage"));
    }
    (!rules::agent_usable(host, &agent_id)).then(|| format!("{summarizer} isn't working right now"))
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

/// Each summary is a session of its own, since earlier ones were for other fallbacks: a slot
/// past every earlier summary of the task never matches one of them.
fn slot(host: &Host, task: &TaskId) -> u32 {
    let earlier = host.read(|state| {
        state
            .sessions
            .by_key
            .values()
            .filter(|session| {
                session.task.as_ref() == Some(task)
                    && session.started.step == Some(StepName::FallbackSummary)
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

/// A summary written earlier for this transcript, such as by another try of a race or
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
/// failed write only costs a later try its reuse.
async fn save(paths: &TranscriptPaths, summary: &str) {
    let text = format!("{summary}\n");
    for copy in copies(paths) {
        let Some(dir) = copy.parent().filter(|dir| dir.is_dir()) else {
            continue;
        };
        let path = dir.join(SUMMARY_FILE);
        if let Err(error) = tokio::fs::write(&path, &text).await {
            tracing::warn!(path = %path.display(), %error, "the fallback summary wasn't saved");
        }
    }
}

/// How much transcript the summarizer's context holds, in bytes.
fn limit(host: &Host, summarizer: &ModelRef) -> usize {
    let registry = &host.services().registry;
    // An alias such as `claude-code/haiku` has no window of its own; the model it runs has.
    let runs = registry
        .catalog()
        .runs(summarizer)
        .map(|model| ModelRef::new(summarizer.agent.clone(), Some(model)))
        .filter(|runs| runs != summarizer)
        .and_then(|runs| registry.get(&runs));
    let window = summary_window(
        summarizer,
        registry.get(summarizer).as_ref(),
        runs.as_ref(),
        &registry.list(),
    );
    bytes_for(window)
}

/// The summarizer's real window: the smaller of the most its model takes and the cap its
/// server holds it to, from its own entry or that of the model an alias of it runs. Where
/// neither knows it, the smallest window known beside it stands in, of its CLI's models on the
/// same server or route. With none known, a local model gets the least a local server gives,
/// and a hosted one the smallest window known of a hosted model, so a summary never assumes
/// more room than its model has, nor sizes a hosted model by a small local one.
fn summary_window(
    summarizer: &ModelRef,
    own: Option<&ModelEntry>,
    runs: Option<&ModelEntry>,
    known: &[ModelEntry],
) -> u64 {
    if let Some(window) = own
        .and_then(held_window)
        .or_else(|| runs.and_then(held_window))
    {
        return window;
    }
    let smallest = |keep: &dyn Fn(&ModelEntry) -> bool| {
        known
            .iter()
            .filter(|entry| keep(entry))
            .filter_map(held_window)
            .min()
    };
    if let Some(window) = smallest(&|entry| beside(summarizer, &entry.id)) {
        return window;
    }
    let local = match own.or(runs) {
        Some(entry) => entry.access == ModelAccess::Local,
        None => known
            .iter()
            .any(|entry| beside(summarizer, &entry.id) && entry.access == ModelAccess::Local),
    };
    if local {
        return LEAST_WINDOW;
    }
    smallest(&|entry| entry.access != ModelAccess::Local).unwrap_or(LEAST_WINDOW)
}

/// Whether `id` runs where `summarizer` does: on the same CLI, and for a model the CLI reaches
/// through a server, such as OpenCode's `ollama/…`, on the same one.
fn beside(summarizer: &ModelRef, id: &ModelRef) -> bool {
    let route = |id: &ModelRef| {
        id.model
            .as_deref()
            .and_then(|model| model.split_once('/'))
            .map(|(route, _)| route.to_owned())
    };
    id.agent == summarizer.agent && route(id) == route(summarizer)
}

/// The window a session on the model is held to, as its entry says. A local model with no cap
/// of its own has none until its server loads it, so the summary doesn't count on its most.
fn held_window(entry: &ModelEntry) -> Option<u64> {
    entry.session_window().filter(|window| *window > 0)
}

/// The transcript's bytes that fit in `window` tokens, with room kept for the rest.
fn bytes_for(window: u64) -> usize {
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
        let (text, cut) = tail("# task-001\n\nRead src/cli.rs\n", 100);
        assert_eq!(text, "# task-001\n\nRead src/cli.rs\n");
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

    fn entry(id: &str, context_window: Option<u64>, window: Option<u64>) -> ModelEntry {
        let access = if id.starts_with("ollama/") || id.contains("/ollama/") {
            "local"
        } else {
            "subscription"
        };
        let mut entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "id": id,
            "provider": "local",
            "kind": "cli",
            "access": access,
            "cost": { "unit": "none" },
        }))
        .expect("a registry entry");
        entry.context_window = context_window;
        entry.window = window;
        entry
    }

    fn model(id: &str) -> ModelRef {
        id.parse().expect("a model id")
    }

    /// Gemma 3 takes 131,072 tokens, but its 32k tag holds it to 32,768, and that is what the
    /// summary must fit in.
    #[test]
    fn the_summary_fits_the_window_the_server_holds_the_model_to() {
        let window = |entry: &ModelEntry| summary_window(&entry.id, Some(entry), None, &[]);
        let gemma = entry("ollama/gemma3:12b", Some(131_072), Some(32_768));
        assert_eq!(window(&gemma), 32_768);
        let opus = entry("claude-code/claude-opus-5-5", Some(1_000_000), None);
        assert_eq!(window(&opus), 1_000_000);
        // A tag with no cap of its own is sized as it loads, so its most isn't counted on.
        let mut uncapped = entry("ollama/qwen3-coder:30b", Some(262_144), None);
        uncapped.discovered = true;
        assert_eq!(window(&uncapped), LEAST_WINDOW);
    }

    /// Haiku named by its alias reads the window of the model the alias runs, never that of a
    /// small local model beside it in the registry.
    #[test]
    fn an_alias_takes_the_window_of_the_model_it_runs() {
        let alias = entry("claude-code/haiku", None, None);
        let haiku = entry("claude-code/claude-haiku-5-5", Some(1_000_000), None);
        let known = [
            alias.clone(),
            haiku.clone(),
            entry("ollama/gemma2:9b", Some(8_192), Some(8_192)),
        ];
        let window = summary_window(&alias.id, Some(&alias), Some(&haiku), &known);
        assert_eq!(window, 1_000_000);
    }

    /// A hosted model the registry knows no window of takes the smallest known beside it on
    /// its CLI, and with none there the smallest of a hosted model; a local one the smallest
    /// on its own server, and with none there the least a local server gives. Never a made-up
    /// 128k, and never a hosted model sized by a local one.
    #[test]
    fn a_model_with_no_window_known_is_sized_by_those_beside_it() {
        let mut uncapped = entry("opencode/ollama/qwen3-coder:30b", Some(262_144), None);
        uncapped.discovered = true;
        let known = [
            entry("claude-code/claude-opus-5-5", Some(1_000_000), None),
            entry("codex/gpt-6-luna", Some(400_000), None),
            entry("antigravity-cli/gemini-3.8-flash-low", None, None),
            entry("ollama/gemma2:9b", Some(8_192), Some(8_192)),
            entry("opencode/ollama/gemma3:12b", Some(131_072), Some(32_768)),
            uncapped,
            entry("opencode/opencode/glm-5.3-flash-free", Some(200_000), None),
        ];
        let window = |id: &str| {
            let own = known.iter().find(|entry| entry.id == model(id));
            summary_window(&model(id), own, None, &known)
        };
        assert_eq!(window("antigravity-cli/gemini-3.8-flash-low"), 200_000);
        assert_eq!(window("codex/gpt-6-sol"), 400_000);
        assert_eq!(window("opencode/ollama/qwen3-coder:30b"), 32_768);
        assert_eq!(window("ollama/llama3.2:latest"), 8_192);
        let mut local_alone = [entry("ollama/qwen3-coder:30b", Some(262_144), None)];
        local_alone[0].discovered = true;
        let own = &local_alone[0];
        assert_eq!(
            summary_window(&own.id, Some(own), None, &local_alone),
            LEAST_WINDOW
        );
        assert_eq!(
            summary_window(&model("codex/gpt-6-luna"), None, None, &[]),
            LEAST_WINDOW
        );
        assert!(bytes_for(32_768) < bytes_for(65_536));
        assert_eq!(bytes_for(32_768), (32_768 - 8_000) * 3);
    }

    #[test]
    fn the_cut_never_splits_a_character() {
        let transcript = "ééééé\nok\n";
        let (text, cut) = tail(transcript, 5);
        assert!(cut);
        assert!(text.ends_with("ok\n"), "{text}");
    }
}
