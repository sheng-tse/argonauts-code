//! What a task's journal says: its events, read cheaply from a long run's log, how many times it
//! fell back, whether checks ran on its latest commit, and where the run's other tasks stand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use argonauts_core::events::agent_status::FallbackEvent;
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::task::{TaskEvent, TaskState, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{AgentId, ModelRef};
use argonauts_core::{ErrorCategory, TaskId};
use tokio::io::{AsyncBufReadExt, BufReader};

/// What the task's own history says about earlier fallbacks and checks.
pub struct History {
    pub fallbacks: u32,
    /// Checks ran on the task's latest commit.
    pub checks_ran: bool,
    /// Its sessions that ended without the answer its format asked for, the stopped one too.
    pub missed_answers: u32,
}

impl History {
    /// Checkpoint tags number fallbacks exactly, so the highest one counts whenever the task has
    /// any. Without tags, fallback tries count, where the racers of one race share a try
    /// number and count once.
    pub fn of(events: &[RunEvent], task: &TaskId) -> Self {
        let mut tagged = 0;
        let mut tries = BTreeSet::new();
        let mut last_commit = None;
        let mut last_check = None;
        let mut missed_answers = 0;
        for event in events
            .iter()
            .filter(|event| event.task_id.as_ref() == Some(task))
        {
            match &event.kind {
                RunEventKind::Task(TaskEvent::CheckpointTagged { n, .. }) => {
                    tagged = tagged.max(*n)
                }
                RunEventKind::Task(TaskEvent::Try {
                    try_number,
                    cause: TryCause::Fallback,
                    ..
                }) => {
                    tries.insert(*try_number);
                }
                RunEventKind::Task(TaskEvent::Committed(info)) if info.sha.is_some() => {
                    last_commit = Some(event.seq);
                }
                RunEventKind::Check(_) => last_check = Some(event.seq),
                RunEventKind::Session(SessionEvent::Ended(end))
                    if matches!(
                        end.reason,
                        EndReason::CliError {
                            category: ErrorCategory::AnswerMissed,
                            ..
                        }
                    ) =>
                {
                    missed_answers += 1;
                }
                _ => {}
            }
        }
        let fallbacks = if tagged > 0 {
            tagged
        } else {
            u32::try_from(tries.len()).unwrap_or(u32::MAX)
        };
        Self {
            fallbacks,
            checks_ran: last_check
                .is_some_and(|check| last_commit.is_none_or(|commit| check > commit)),
            missed_answers,
        }
    }
}

/// What a stop reads from the run's log.
#[derive(Debug, Default)]
pub struct TaskLog {
    /// The task's events and the run-wide ones its fallback note reads.
    pub events: Vec<RunEvent>,
    pub placements: Placements,
}

/// Who holds each of the run's tasks, for `spread`.
#[derive(Debug, Clone, Default)]
pub struct Placements {
    tasks: BTreeMap<TaskId, Placement>,
    /// The newest event read, so a pick made after the read can tell when the log catches up.
    pub last_seq: u64,
}

#[derive(Debug, Clone, Default)]
struct Placement {
    worker: Option<ModelRef>,
    /// A fallback gave the task to `worker`.
    fell_back: bool,
    state: Option<TaskState>,
    /// The event that last placed the task.
    seq: u64,
}

impl Placements {
    fn observe(&mut self, event: &RunEvent) {
        self.last_seq = self.last_seq.max(event.seq);
        match (&event.kind, &event.task_id) {
            (RunEventKind::Task(TaskEvent::Try { worker, cause, .. }), Some(task)) => {
                self.place(task, worker, *cause == TryCause::Fallback, event.seq);
            }
            (RunEventKind::Task(TaskEvent::Reassigned { to, .. }), Some(task)) => {
                self.place(task, to, false, event.seq);
            }
            (RunEventKind::Fallback(FallbackEvent::FellBack { task, to, .. }), _) => {
                self.place(task, to, true, event.seq);
            }
            (RunEventKind::Task(TaskEvent::State { to, .. }), Some(task)) => {
                self.tasks.entry(task.clone()).or_default().state = Some(*to);
            }
            _ => {}
        }
    }

    /// A retry on the same model keeps how the task got there.
    fn place(&mut self, task: &TaskId, worker: &ModelRef, fell_back: bool, seq: u64) {
        let placement = self.tasks.entry(task.clone()).or_default();
        if placement.worker.as_ref() == Some(worker) {
            placement.fell_back |= fell_back;
        } else {
            placement.worker = Some(worker.clone());
            placement.fell_back = fell_back;
        }
        placement.seq = seq;
    }

    /// A fallback decided after reading up to `seen`, which the log may not show yet. Returns
    /// false once the log has placed the task since, so the pick can be forgotten.
    pub fn pick(&mut self, task: &TaskId, worker: &ModelRef, seen: u64) -> bool {
        if self
            .tasks
            .get(task)
            .is_some_and(|placement| placement.seq > seen)
        {
            return false;
        }
        self.place(task, worker, true, seen);
        true
    }

    /// The run's tasks a fallback gave to `worker`.
    pub fn fell_back_to(&self, worker: &ModelRef) -> u32 {
        let count = self
            .tasks
            .values()
            .filter(|placement| placement.fell_back && placement.worker.as_ref() == Some(worker))
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// The tasks other than `except` whose session on `agent` is running or just stopped.
    pub fn on_agent(&self, agent_id: &AgentId, except: &TaskId) -> Vec<TaskId> {
        self.tasks
            .iter()
            .filter(|(task, placement)| {
                *task != except
                    && placement
                        .worker
                        .as_ref()
                        .is_some_and(|worker| &worker.agent_id() == agent_id)
                    && placement.state.is_none_or(|state| {
                        matches!(
                            state,
                            TaskState::Running | TaskState::Blocked | TaskState::NeedsDecision
                        )
                    })
            })
            .map(|(task, _)| task.clone())
            .collect()
    }
}

/// Reads the task's events and the run's placements in one pass. Lines that can't be about
/// either are skipped before parsing, so a long run stays cheap, and a line that doesn't parse
/// is skipped too, since a crash can leave the last one half written.
pub async fn task_log(path: &Path, task: &TaskId) -> std::io::Result<TaskLog> {
    const RUN_WIDE: [&str; 3] = ["\"goal\"", "\"plan\"", "\"blackboard\""];
    const PLACING: [&str; 4] = ["\"try\"", "\"reassigned\"", "\"fell_back\"", "\"state\""];
    let file = tokio::fs::File::open(path).await?;
    let mut lines = BufReader::new(file).lines();
    let mut sessions: Vec<String> = Vec::new();
    let mut log = TaskLog::default();
    while let Some(line) = lines.next_line().await? {
        let mine = line.contains(task.as_str())
            || RUN_WIDE.iter().any(|tag| line.contains(tag))
            || sessions
                .iter()
                .any(|session| line.contains(session.as_str()));
        let placing = PLACING.iter().any(|tag| line.contains(tag));
        if !mine && !placing {
            continue;
        }
        let Ok(event) = serde_json::from_str::<RunEvent>(&line) else {
            continue;
        };
        log.placements.observe(&event);
        if !mine {
            continue;
        }
        if event.task_id.as_ref() == Some(task)
            && let Some(session) = &event.session
        {
            let key = session.to_string();
            if !sessions.contains(&key) {
                sessions.push(key);
            }
        }
        log.events.push(event);
    }
    Ok(log)
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::SessionKey;
    use argonauts_core::{AgentEvent, Note, NoteLevel};
    use time::OffsetDateTime;

    use super::*;
    use crate::fallback::testing::{TASK, agent_id, run, worker};

    fn event(task: Option<&str>, session: Option<u32>, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: run(),
            task_id: task.map(|id| TaskId::new(id).expect("a valid task id")),
            session: session.map(SessionKey::numbered),
            source: "argonauts-code".to_owned(),
            kind,
        }
    }

    fn message(text: &str) -> RunEventKind {
        RunEventKind::Agent(AgentEvent::Message {
            text: text.to_owned(),
            parent: None,
        })
    }

    #[tokio::test]
    async fn task_log_keeps_the_tasks_sessions_and_skips_the_rest() {
        let task = TaskId::new(TASK).expect("a valid task id");
        let first_try = RunEventKind::Task(TaskEvent::Try {
            try_number: 1,
            worker: worker("codex/gpt-5"),
            cause: TryCause::First,
        });
        let events = [
            event(Some(TASK), Some(1), first_try.clone()),
            event(None, Some(1), message("mine")),
            event(Some("task-004"), Some(2), first_try),
            event(None, Some(2), message("theirs")),
            event(
                None,
                None,
                RunEventKind::Note(Note {
                    level: NoteLevel::Info,
                    text: "run-wide".to_owned(),
                }),
            ),
        ];
        let mut text: String = events
            .iter()
            .map(|event| serde_json::to_string(event).expect("an event serializes") + "\n")
            .collect();
        text.push_str("{\"seq\": 9, \"half written");
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, text).expect("the journal is written");

        let read = task_log(&path, &task).await.expect("the journal reads");
        assert_eq!(read.events, events[..2]);
        // Another task's try still places that task.
        assert_eq!(
            read.placements.on_agent(&agent_id("codex"), &task),
            [TaskId::new("task-004").expect("a valid task id")]
        );
        assert!(
            task_log(&dir.path().join("missing.jsonl"), &task)
                .await
                .is_err()
        );
    }

    fn try_number(n: u32, to: &str, cause: TryCause) -> RunEventKind {
        RunEventKind::Task(TaskEvent::Try {
            try_number: n,
            worker: worker(to),
            cause,
        })
    }

    fn state(to: TaskState) -> RunEventKind {
        RunEventKind::Task(TaskEvent::State {
            from: None,
            to,
            reason: String::new(),
        })
    }

    #[test]
    fn placements_follow_fallbacks_and_live_tasks() {
        let at = |seq: u64, task: &str, kind: RunEventKind| RunEvent {
            seq,
            ..event(Some(task), None, kind)
        };
        let id = |task: &str| TaskId::new(task).expect("a valid task id");
        let events = [
            at(
                1,
                "task-004",
                try_number(1, "claude-code/opus", TryCause::First),
            ),
            at(2, "task-004", state(TaskState::Running)),
            at(
                3,
                "task-005",
                try_number(1, "claude-code/opus", TryCause::First),
            ),
            at(4, "task-005", state(TaskState::Merged)),
            at(
                5,
                "task-006",
                RunEventKind::Fallback(FallbackEvent::FellBack {
                    task: id("task-006"),
                    to: worker("codex/gpt-5"),
                    mode: argonauts_core::settings::fallback::FallbackMode::Auto,
                    by: argonauts_core::events::run::Party::ArgonautsCode,
                    context: None,
                }),
            ),
            at(
                6,
                "task-006",
                try_number(2, "codex/gpt-5", TryCause::Fallback),
            ),
            at(
                7,
                "task-006",
                try_number(3, "codex/gpt-5", TryCause::CheckRetry),
            ),
        ];
        let mut placements = Placements::default();
        for event in &events {
            placements.observe(event);
        }
        let claude = agent_id("claude-code");
        assert_eq!(placements.last_seq, 7);
        // A finished task has no session to move.
        assert_eq!(placements.on_agent(&claude, &id(TASK)), [id("task-004")]);
        assert!(placements.on_agent(&claude, &id("task-004")).is_empty());
        // A check retry on the same model keeps the fallback that put the task there.
        assert_eq!(placements.fell_back_to(&worker("codex/gpt-5")), 1);

        let gemini = worker("antigravity-cli/gemini-3");
        assert!(placements.pick(&id("task-004"), &gemini, 7));
        assert_eq!(placements.fell_back_to(&gemini), 1);
        assert!(placements.on_agent(&claude, &id(TASK)).is_empty());

        // Once the log places the task after the pick, the log wins.
        placements.observe(&at(
            8,
            "task-004",
            try_number(2, "codex/gpt-5", TryCause::Fallback),
        ));
        assert!(!placements.pick(&id("task-004"), &gemini, 7));
        assert_eq!(placements.fell_back_to(&gemini), 0);
        assert_eq!(placements.fell_back_to(&worker("codex/gpt-5")), 2);
    }

    #[test]
    fn a_race_counts_as_one_fallback() {
        let task = TaskId::new(TASK).expect("a valid task id");
        let racers = ["codex/gpt-5", "antigravity-cli/gemini-3", "opencode/glm"];
        let at = |seq: u64, kind: RunEventKind| RunEvent {
            seq,
            ..event(Some(TASK), None, kind)
        };
        let tag = at(
            2,
            RunEventKind::Task(TaskEvent::CheckpointTagged {
                n: 1,
                commit: "c1".to_owned(),
                from: worker("claude-code/opus"),
                to: racers.iter().map(|racer| worker(racer)).collect(),
                reason: "usage limit".to_owned(),
            }),
        );
        // Each racer logs its own try, even with its own number.
        let mut checkpointed = vec![
            at(1, try_number(1, "claude-code/opus", TryCause::First)),
            tag,
        ];
        checkpointed.extend((0u32..).zip(racers).map(|(n, racer)| {
            at(
                u64::from(3 + n),
                try_number(2 + n, racer, TryCause::Fallback),
            )
        }));
        assert_eq!(History::of(&checkpointed, &task).fallbacks, 1);

        // Without checkpoints, racers that share a try number count once.
        let untagged: Vec<RunEvent> = (0u32..)
            .zip(racers)
            .map(|(n, racer)| at(u64::from(n), try_number(2, racer, TryCause::Fallback)))
            .collect();
        assert_eq!(History::of(&untagged, &task).fallbacks, 1);
    }

    #[test]
    fn checks_count_only_after_the_latest_commit() {
        let task = TaskId::new(TASK).expect("a valid task id");
        let at = |seq: u64, kind: RunEventKind| RunEvent {
            seq,
            ..event(Some(TASK), None, kind)
        };
        let check = || {
            RunEventKind::Check(argonauts_core::events::task::CheckOutcome {
                name: "tests".to_owned(),
                kind: argonauts_core::events::task::CheckKind::Command,
                passed: true,
                command: None,
                exit_code: Some(0),
                duration_ms: 1,
                summary: "ok".to_owned(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            })
        };
        let commit = RunEventKind::Task(TaskEvent::Committed(argonauts_core::CommitInfo {
            branch: "argonauts/run-01/task-003".to_owned(),
            sha: Some("abc".to_owned()),
            message: "work".to_owned(),
            stat: argonauts_core::DiffStat::default(),
            by: None,
        }));
        let before = [at(1, check()), at(2, commit.clone())];
        assert!(!History::of(&before, &task).checks_ran);
        let after = [at(1, commit), at(2, check())];
        assert!(History::of(&after, &task).checks_ran);
        assert_eq!(History::of(&after, &task).fallbacks, 0);
    }
}
