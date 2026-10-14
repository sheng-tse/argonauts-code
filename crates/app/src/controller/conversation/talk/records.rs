//! What this repository's run records hold, read for a lookup: each review with what it sent
//! back and why, each task and how it ended, and each fallback. Only the kinds of event a
//! lookup needs are parsed, so a long journal reads quickly.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use argonauts_core::events::task::TaskState;
use argonauts_core::events::{
    DivideEvent, FallbackEvent, ReviewEvent, RunEvent, RunEventKind, TaskEvent,
};
use argonauts_core::layout::run_files;
use argonauts_core::names::ModelRef;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{RunId, TaskId};
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
    pub task_id: Option<TaskId>,
    pub task: Option<String>,
    pub reviewer: ModelRef,
    pub verdict: ReviewVerdict,
    pub summary: String,
    /// Each comment as `[high] src/task.rs:12 the reason`.
    pub comments: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct Task {
    pub run: RunId,
    pub task_id: TaskId,
    pub title: String,
    /// The last state the journal gave it.
    pub state: Option<TaskState>,
    /// Who built it last.
    pub worker: Option<ModelRef>,
}

#[derive(Debug, Clone)]
pub(super) struct Fallback {
    pub run: RunId,
    pub task_id: TaskId,
    pub task: String,
    pub stopped: ModelRef,
    pub reason: String,
    /// Who it fell back to. `None` while it waited for you or for the model to come back.
    pub to: Option<ModelRef>,
}

/// The kinds of event a lookup reads, by the envelope's `kind.type`.
const KINDS: [&[u8]; 4] = [b"task", b"review", b"fallback", b"divide"];

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
    let mut tasks: BTreeMap<TaskId, Task> = BTreeMap::new();
    let mut created: BTreeMap<TaskId, OffsetDateTime> = BTreeMap::new();
    let mut fallbacks: Vec<Fallback> = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if !kind_of(line).is_some_and(|kind| KINDS.contains(&kind)) {
            continue;
        }
        // A torn last line is skipped, as the journal's own reader skips it.
        let Ok(event) = serde_json::from_slice::<RunEvent>(line) else {
            continue;
        };
        let task_id = event.task_id.clone();
        match event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) => {
                created.insert(spec.id.clone(), event.ts);
                tasks.insert(
                    spec.id.clone(),
                    Task {
                        run: event.run_id.clone(),
                        task_id: spec.id.clone(),
                        title: spec.title.clone(),
                        state: None,
                        worker: spec.worker.clone(),
                    },
                );
            }
            RunEventKind::Task(TaskEvent::State { to, .. }) => {
                if let Some(task) = task_id.as_ref().and_then(|task_id| tasks.get_mut(task_id)) {
                    task.state = Some(to);
                }
            }
            RunEventKind::Task(TaskEvent::Try { worker, .. }) => {
                if let Some(task) = task_id.as_ref().and_then(|task_id| tasks.get_mut(task_id)) {
                    task.worker = Some(worker);
                }
            }
            RunEventKind::Divide(DivideEvent::Worker {
                task: task_id,
                worker,
                ..
            }) => {
                if let Some(task) = tasks.get_mut(&task_id) {
                    task.worker = Some(worker);
                }
            }
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer, review, ..
            }) if event.ts >= since => {
                let task = task_id
                    .as_ref()
                    .and_then(|task_id| tasks.get(task_id))
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
                    task_id,
                    task,
                    reviewer,
                    verdict: review.verdict,
                    summary: review.summary.trim().to_owned(),
                    comments,
                });
            }
            RunEventKind::Fallback(FallbackEvent::Needed(plan)) if event.ts >= since => {
                fallbacks.push(Fallback {
                    run: event.run_id.clone(),
                    task_id: plan.task.clone(),
                    task: plan.title.clone(),
                    stopped: plan.stopped.clone(),
                    reason: plan.reason.clone(),
                    to: None,
                });
            }
            RunEventKind::Fallback(FallbackEvent::FellBack {
                task: task_id, to, ..
            }) => {
                let waiting = fallbacks
                    .iter_mut()
                    .rev()
                    .find(|fallback| fallback.task_id == task_id && fallback.to.is_none());
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
            .filter(|task| created.get(&task.task_id).is_some_and(|at| *at >= since)),
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
