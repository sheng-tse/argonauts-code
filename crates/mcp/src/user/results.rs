//! Results written by code: ids, states, counts, spend, and needs-you flags. Agent text goes
//! only in fields named `agent_text`, cut short, stripped of control sequences, and redacted,
//! and `mcp.share = "status-only"` leaves it out. Diffs, file contents, prompts, and raw logs
//! never leave the machine.

use std::collections::BTreeMap;

use argonauts_core::api::{ApiError, RunSummary, StatusSummary};
use argonauts_core::events::checkpoint::{CheckpointEvent, CheckpointOpened};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::redact::redact_secrets;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::agents::Share;
use argonauts_core::{RunId, Spend};
use serde::Serialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Agent text is cut to this many characters.
pub const AGENT_TEXT_MAX: usize = 280;
/// Runs and cards past these counts are left out, and a `more_*` count says how many.
pub const MAX_RUNS: usize = 50;
pub const MAX_CARDS: usize = 50;
/// Characters of a checkpoint's evidence hash shown, enough to tell two apart.
pub const EVIDENCE_PREFIX: usize = 12;
/// Error text from the conductor can quote agent text, so it's cut like it.
const ERROR_MAX: usize = 300;

/// Agent text as a front end may see it, or `None` when `share` keeps it home.
pub fn agent_text(text: &str, share: Share) -> Option<String> {
    match share {
        Share::StatusOnly => None,
        Share::Summaries => Some(clean(text, AGENT_TEXT_MAX)),
    }
}

/// Sanitized first, so an escape sequence can't split a secret past the redactor, then
/// redacted before the cut, so a cut can't leave half a secret the redactor misses.
fn clean(text: &str, max: usize) -> String {
    let sanitized = sanitize_for_terminal(text);
    let one_line = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    cut(&redact_secrets(&one_line), max)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// An error the client can read, cut and cleaned since it can quote agent text.
pub fn api_error(err: &ApiError) -> String {
    clean(&err.to_string(), ERROR_MAX)
}

/// Adds `agent_text` to `object` when `share` lets it out and there is some.
fn with_agent_text(mut object: Map<String, Value>, text: &str, share: Share) -> Value {
    if let Some(text) = agent_text(text, share).filter(|text| !text.is_empty()) {
        object.insert("agent_text".to_owned(), Value::String(text));
    }
    Value::Object(object)
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// An enum's serde name, such as `running`.
fn tag<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn timestamp(at: OffsetDateTime) -> Value {
    at.format(&Rfc3339).map_or(Value::Null, Value::String)
}

pub fn spend(spent: &Spend) -> Value {
    json!({
        "usd": spent.cost_usd,
        "usd_partial": spent.cost_partial,
        "usd_estimated": spent.cost_estimated,
        "input_tokens": spent.input_tokens,
        "output_tokens": spent.output_tokens,
        "cached_tokens": spent.cached_tokens,
    })
}

pub fn run_summary(run: &RunSummary, share: Share) -> Value {
    let fields = json!({
        "run": run.run.as_str(),
        "state": tag(&run.state),
        "mode": tag(&run.mode),
        "live": run.live,
        "needs_you": !run.waiting.is_empty(),
        "waiting": run.waiting.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
        "sessions_to_resume": run.sessions_to_resume,
        "remind": run.remind,
        "updated": timestamp(run.updated),
        "spent": spend(&run.spent),
    });
    with_agent_text(object(fields), &run.title, share)
}

/// Newest first, as the conductor lists them.
pub fn run_list(runs: &[RunSummary], share: Share) -> Value {
    json!({
        "runs": runs.iter().take(MAX_RUNS).map(|run| run_summary(run, share)).collect::<Vec<_>>(),
        "more_runs": runs.len().saturating_sub(MAX_RUNS),
    })
}

/// One run's status. `summary` is its row in the run list, for its state and spend. The plan,
/// decisions, and recent lines stay home; only goal lines and card titles go out as agent text.
pub fn run_status(
    run: &RunId,
    status: &StatusSummary,
    summary: Option<&RunSummary>,
    share: Share,
) -> Value {
    let mut fields = match summary {
        Some(summary) => object(run_summary(summary, Share::StatusOnly)),
        None => {
            let spent = status.budget.as_ref().map(|budget| spend(&budget.spent));
            object(json!({
                "run": run.as_str(),
                "needs_you": !status.waiting.is_empty(),
                "spent": spent,
            }))
        }
    };
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for card in &status.cards {
        if let Value::String(state) = tag(&card.state) {
            *counts.entry(state).or_default() += 1;
        }
    }
    let cards: Vec<Value> = status
        .cards
        .iter()
        .take(MAX_CARDS)
        .map(|card| {
            let fields = json!({
                "card": card.card.as_str(),
                "state": tag(&card.state),
                "owner": card.owner.as_ref().map(|owner| clean(&owner.worker, 64)),
            });
            with_agent_text(object(fields), &card.title, share)
        })
        .collect();
    fields.insert("cards".to_owned(), Value::Array(cards));
    fields.insert("card_counts".to_owned(), json!(counts));
    fields.insert(
        "more_cards".to_owned(),
        json!(status.cards.len().saturating_sub(MAX_CARDS)),
    );
    fields.insert("decisions".to_owned(), json!(status.decisions.len()));
    with_agent_text(fields, &status.goal.join(" / "), share)
}

/// One event: its position, time, type, and ids. Only a checkpoint's title goes out as agent
/// text; messages, reports, and tool output stay home.
pub fn event(event: &RunEvent, share: Share) -> Value {
    let (kind, name) = event_names(&event.kind);
    let mut fields = object(json!({
        "seq": event.seq,
        "ts": timestamp(event.ts),
        "type": kind,
    }));
    if let Some(name) = name {
        fields.insert("event".to_owned(), Value::String(name));
    }
    if let Some(card) = &event.card_id {
        fields.insert("card".to_owned(), Value::String(card.as_str().to_owned()));
    }
    if let Some(session) = &event.session {
        fields.insert(
            "session".to_owned(),
            Value::String(session.as_str().to_owned()),
        );
    }
    let RunEventKind::Checkpoint(checkpoint) = &event.kind else {
        return Value::Object(fields);
    };
    let id = match checkpoint {
        CheckpointEvent::Opened(opened) => {
            fields.insert("checkpoint_kind".to_owned(), tag(&opened.kind));
            fields.insert("needs_you".to_owned(), Value::Bool(opened.needs_you));
            fields.insert(
                "checkpoint".to_owned(),
                Value::String(opened.id.as_str().to_owned()),
            );
            return with_agent_text(fields, &opened.title, share);
        }
        CheckpointEvent::Checked(check) => &check.id,
        CheckpointEvent::Decided(decided) => &decided.id,
        CheckpointEvent::Withdrawn { id, .. } | CheckpointEvent::SpotCheckQueued { id } => id,
    };
    fields.insert(
        "checkpoint".to_owned(),
        Value::String(id.as_str().to_owned()),
    );
    Value::Object(fields)
}

/// The event's type and, for types tagged by event name, that name, such as `checkpoint` and
/// `opened`.
fn event_names(kind: &RunEventKind) -> (&'static str, Option<String>) {
    use RunEventKind as Kind;
    match kind {
        Kind::Run(inner) => ("run", inner_tag(inner)),
        Kind::Phase(_) => ("phase", None),
        Kind::Goal(inner) => ("goal", inner_tag(inner)),
        Kind::Plan(inner) => ("plan", inner_tag(inner)),
        Kind::Divide(inner) => ("divide", inner_tag(inner)),
        Kind::Card(inner) => ("card", inner_tag(inner)),
        Kind::Session(inner) => ("session", inner_tag(inner)),
        Kind::Agent(_) => ("agent", None),
        Kind::Gate(_) => ("gate", None),
        Kind::Review(inner) => ("review", inner_tag(inner)),
        Kind::Integration(inner) => ("integration", inner_tag(inner)),
        Kind::Checkpoint(inner) => ("checkpoint", inner_tag(inner)),
        Kind::Handoff(inner) => ("handoff", inner_tag(inner)),
        Kind::Vendor(inner) => ("vendor", inner_tag(inner)),
        Kind::Budget(inner) => ("budget", inner_tag(inner)),
        Kind::Blackboard(inner) => ("blackboard", inner_tag(inner)),
        Kind::Retro(inner) => ("retro", inner_tag(inner)),
        Kind::Note(_) => ("note", None),
        Kind::Workflow(inner) => ("workflow", inner_tag(inner)),
    }
}

/// The `event` tag serde writes for these enums. It's a variant name, but it's checked to be
/// one, so no field that happens to share the name can carry text out.
fn inner_tag<T: Serialize>(inner: &T) -> Option<String> {
    let value = serde_json::to_value(inner).ok()?;
    let name = value.get("event")?.as_str()?;
    let plain = !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    plain.then(|| name.to_owned())
}

/// The first characters of an evidence hash, hex digits only.
pub fn evidence_prefix(evidence_sha256: &str) -> String {
    evidence_sha256
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(EVIDENCE_PREFIX)
        .collect()
}

/// The shell command that answers `checkpoint`, run by you in a terminal.
pub fn approve_command(run: &RunId, checkpoint: &CheckpointOpened) -> String {
    format!("argonauts approve {run} {}", checkpoint.id)
}

/// What's waiting in `run`, as text for the client to show and as fields. It says where to
/// answer and never answers anything.
pub fn decision(run: &RunId, waiting: &[CheckpointOpened], share: Share) -> (String, Value) {
    let how = format!(
        "Answer it in the Argonauts app, or run `argonauts approve {run} <approval>` in a \
         terminal. This server can't answer it."
    );
    let items: Vec<Value> = waiting
        .iter()
        .map(|checkpoint| {
            let fields = json!({
                "checkpoint": checkpoint.id.as_str(),
                "kind": tag(&checkpoint.kind),
                "evidence": evidence_prefix(&checkpoint.evidence_sha256),
                "needs_you": checkpoint.needs_you,
                "answer_with": approve_command(run, checkpoint),
            });
            with_agent_text(object(fields), &checkpoint.title, share)
        })
        .collect();
    let fields = json!({
        "run": run.as_str(),
        "needs_you": waiting.iter().any(|checkpoint| checkpoint.needs_you),
        "waiting": items,
        "how_to_answer": how,
    });
    (decision_text(run, waiting), fields)
}

fn decision_text(run: &RunId, waiting: &[CheckpointOpened]) -> String {
    if waiting.is_empty() {
        return format!("Nothing in run {run} is waiting for a decision.");
    }
    let mut lines = vec![match waiting.len() {
        1 => format!("Run {run} is waiting for a decision."),
        n => format!("Run {run} is waiting for {n} decisions."),
    }];
    for checkpoint in waiting {
        let kind = tag(&checkpoint.kind);
        let kind = kind.as_str().unwrap_or("checkpoint");
        let deciding = if checkpoint.needs_you {
            ""
        } else {
            " Its approval rules are still deciding."
        };
        lines.push(format!(
            "- {} ({kind}, evidence {}): answer it in the Argonauts app, or run `{}`.{deciding}",
            checkpoint.id,
            evidence_prefix(&checkpoint.evidence_sha256),
            approve_command(run, checkpoint),
        ));
    }
    lines.push("This server can say what's waiting but never answer it.".to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built at runtime so no string in this file looks like a real credential.
    fn fake(prefix: &str, body: char, len: usize) -> String {
        format!("{prefix}{}", body.to_string().repeat(len))
    }

    #[test]
    fn agent_text_is_cleaned_redacted_and_cut() {
        let secret = fake("sk-ant-", 'A', 28);
        let text = format!("\x1b[31mred\x1b[0m\n{secret} {}", "word ".repeat(200));
        let shown = agent_text(&text, Share::Summaries).unwrap_or_default();
        assert!(!shown.contains('\x1b'), "{shown:?}");
        assert!(!shown.contains('\n'), "{shown:?}");
        assert!(!shown.contains(&secret), "{shown:?}");
        assert!(shown.contains("[redacted"), "{shown:?}");
        assert!(shown.starts_with("red "), "{shown:?}");
        assert_eq!(shown.chars().count(), AGENT_TEXT_MAX);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn a_split_secret_is_still_redacted() {
        let text = format!(
            "key {}\x1b[0m{} end",
            fake("sk-ant-", 'A', 4),
            "A".repeat(24)
        );
        let shown = agent_text(&text, Share::Summaries).unwrap_or_default();
        assert!(shown.contains("[redacted"), "{shown:?}");
    }

    #[test]
    fn status_only_keeps_agent_text_home() {
        assert_eq!(agent_text("the goal", Share::StatusOnly), None);
        assert_eq!(
            agent_text("the goal", Share::Summaries).as_deref(),
            Some("the goal")
        );
    }

    #[test]
    fn short_text_is_kept_whole() {
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcd", 3), "ab…");
    }

    #[test]
    fn evidence_shows_twelve_hex_digits() {
        assert_eq!(
            evidence_prefix("3fa2c1d0e9ab77aa0011223344556677"),
            "3fa2c1d0e9ab"
        );
        assert_eq!(evidence_prefix("zz-3f!"), "3f");
    }
}
