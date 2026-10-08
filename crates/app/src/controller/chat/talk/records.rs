//! What this repository's run records hold, read for a lookup: each review with what it sent
//! back and why, each task and how it ended, and each fallback. Only the kinds of event a
//! lookup needs are parsed, so a long journal reads quickly.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use argonauts_core::events::card::CardState;
use argonauts_core::events::{
    CardEvent, DivideEvent, HandoffEvent, ReviewEvent, RunEvent, RunEventKind,
};
use argonauts_core::layout::run_files;
use argonauts_core::names::WorkerRef;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{CardId, RunId};
use time::OffsetDateTime;

#[derive(Debug, Default)]
pub(super) struct Records {
    pub reviews: Vec<Review>,
    pub tasks: Vec<Task>,
    pub fallbacks: Vec<Fallback>,
}

#[derive(Debug, Clone)]
pub(super) struct Review {
    pub run: RunId,
    pub card: Option<CardId>,
    pub task: Option<String>,
    pub reviewer: WorkerRef,
    pub verdict: ReviewVerdict,
    pub summary: String,
    /// Each comment as `[high] src/task.rs:12 the reason`.
    pub comments: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct Task {
    pub run: RunId,
    pub card: CardId,
    pub title: String,
    /// The last state the journal gave it.
    pub state: Option<CardState>,
    /// Who built it last.
    pub owner: Option<WorkerRef>,
}

#[derive(Debug, Clone)]
pub(super) struct Fallback {
    pub run: RunId,
    pub card: CardId,
    pub task: String,
    pub stopped: WorkerRef,
    pub reason: String,
    /// Who took over. `None` while it waited for you or for the model to come back.
    pub to: Option<WorkerRef>,
}

/// The kinds of event a lookup reads, by the envelope's `kind.type`.
const KINDS: [&[u8]; 4] = [b"card", b"review", b"handoff", b"divide"];

/// Every run's records since `since`, oldest run first. A journal last written before then
/// holds nothing newer, so it isn't opened.
pub(super) fn read(runs_dir: &Path, since: OffsetDateTime) -> Records {
    let mut records = Records::default();
    let Ok(entries) = std::fs::read_dir(runs_dir) else {
        return records;
    };
    let mut journals: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(run_files::EVENTS))
        .filter(|path| written_since(path, since))
        .collect();
    journals.sort();
    for journal in journals {
        if let Ok(bytes) = std::fs::read(&journal) {
            read_run(&bytes, since, &mut records);
        }
    }
    records
}

fn written_since(path: &Path, since: OffsetDateTime) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
        return false;
    };
    let since = SystemTime::from(since);
    modified >= since
}

/// One run's journal into `records`.
fn read_run(bytes: &[u8], since: OffsetDateTime, records: &mut Records) {
    let mut tasks: BTreeMap<CardId, Task> = BTreeMap::new();
    let mut created: BTreeMap<CardId, OffsetDateTime> = BTreeMap::new();
    let mut fallbacks: Vec<Fallback> = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if !kind_of(line).is_some_and(|kind| KINDS.contains(&kind)) {
            continue;
        }
        // A torn last line is skipped, as the journal's own reader skips it.
        let Ok(event) = serde_json::from_slice::<RunEvent>(line) else {
            continue;
        };
        let card = event.card_id.clone();
        match event.kind {
            RunEventKind::Card(CardEvent::Created(spec)) => {
                created.insert(spec.id.clone(), event.ts);
                tasks.insert(
                    spec.id.clone(),
                    Task {
                        run: event.run_id.clone(),
                        card: spec.id.clone(),
                        title: spec.title.clone(),
                        state: None,
                        owner: spec.owner.clone(),
                    },
                );
            }
            RunEventKind::Card(CardEvent::State { to, .. }) => {
                if let Some(task) = card.as_ref().and_then(|card| tasks.get_mut(card)) {
                    task.state = Some(to);
                }
            }
            RunEventKind::Card(CardEvent::Attempt { worker, .. }) => {
                if let Some(task) = card.as_ref().and_then(|card| tasks.get_mut(card)) {
                    task.owner = Some(worker);
                }
            }
            RunEventKind::Divide(DivideEvent::Owner { card, worker, .. }) => {
                if let Some(task) = tasks.get_mut(&card) {
                    task.owner = Some(worker);
                }
            }
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer, review, ..
            }) if event.ts >= since => {
                let task = card
                    .as_ref()
                    .and_then(|card| tasks.get(card))
                    .map(|task| task.title.clone());
                let comments = review
                    .comments
                    .iter()
                    .map(|comment| {
                        let place = match comment.line {
                            Some(line) => format!("{}:{line}", comment.path),
                            None => comment.path.clone(),
                        };
                        let severity = format!("{:?}", comment.severity).to_lowercase();
                        format!("[{severity}] {place} {}", comment.text.trim())
                    })
                    .collect();
                records.reviews.push(Review {
                    run: event.run_id.clone(),
                    card,
                    task,
                    reviewer,
                    verdict: review.verdict,
                    summary: review.summary.trim().to_owned(),
                    comments,
                });
            }
            RunEventKind::Handoff(HandoffEvent::Needed(plan)) if event.ts >= since => {
                fallbacks.push(Fallback {
                    run: event.run_id.clone(),
                    card: plan.card.clone(),
                    task: plan.title.clone(),
                    stopped: plan.stopped.clone(),
                    reason: plan.reason.clone(),
                    to: None,
                });
            }
            RunEventKind::Handoff(HandoffEvent::HandedOff { card, to, .. }) => {
                let waiting = fallbacks
                    .iter_mut()
                    .rev()
                    .find(|fallback| fallback.card == card && fallback.to.is_none());
                if let Some(fallback) = waiting {
                    fallback.to = Some(to);
                }
            }
            _ => {}
        }
    }
    records.tasks.extend(
        tasks
            .into_values()
            .filter(|task| created.get(&task.card).is_some_and(|at| *at >= since)),
    );
    records.fallbacks.extend(fallbacks);
}

/// The envelope's `kind.type`. Every field before it is a number or a string, and a JSON
/// string can't hold an unescaped quote, so the first match is the envelope's own.
fn kind_of(line: &[u8]) -> Option<&[u8]> {
    const TAG: &[u8] = br#""kind":{"type":""#;
    let start = line.windows(TAG.len()).position(|window| window == TAG)? + TAG.len();
    let len = line[start..].iter().position(|byte| *byte == b'"')?;
    Some(&line[start..start + len])
}
