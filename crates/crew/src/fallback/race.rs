//! Several fallbacks on one task at once (`together` of 2 or more), each in its own worktree,
//! sharing the task's remaining limit. `pick` says whose work is kept: the first try that
//! passes its checks and its review, and then the others stop; or the judge's choice among
//! all that passed, once every try has ended. The others' work is committed on their own
//! branches under their names.
//!
//! Tries take numbers no earlier try used, read from the log, so a tree or branch is
//! never reused. A race runs while the task stays in one state, so the tries since the
//! task's last state change are this race's: re-entered after a restart, the race finds the
//! same trees, and its sessions the slots replay matches.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio_util::sync::CancellationToken;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::task::TaskEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::FallbackNote;
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::SessionControl;
use argonauts_core::settings::fallback::Pick;
use argonauts_core::steps::StepName;
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::engine::host::Host;
use crate::engine::job_fallback;
use crate::error::{EngineError, EngineResult};
use crate::fallback::{assign, judge, notes, rules, upgrade};
use crate::labels;
use crate::roles::display_name;
use crate::state::RunState;
use crate::state::sessions::SessionRecord;
use crate::task::pipeline::{self, TryCall, TryEnd};
use crate::task::support::{newest_worker_session, task_events};
use crate::task::worktree::{self, TaskTree};
use crate::workflow::StepAt;

/// How often a decided race looks again for loser sessions to stop, since one may start late.
const STOP_POLL: Duration = Duration::from_millis(100);

/// One candidate and its own tree.
struct Entrant {
    worker: ModelRef,
    tree: TaskTree,
    first_slot: u32,
    /// What its first session is told, naming the transcript's copy in its own tree.
    note: FallbackNote,
}

/// Where this race sits in the task's log.
struct RaceLog {
    /// The seq of the task's last state change, which this race's events all come after.
    /// `None` when the log can't be read, and then every try starts fresh.
    since: Option<u64>,
    /// The highest try number any tree or branch of the task was given.
    highest: u32,
    /// Tries this race already started, by worker.
    started: BTreeMap<ModelRef, u32>,
}

/// Returns the fallback whose work the task keeps.
pub async fn run(
    host: &Host,
    task: &TaskId,
    to: &[ModelRef],
    pick: Pick,
    note: &FallbackNote,
) -> EngineResult<Option<ModelRef>> {
    let at = rules::step_now(host)?;
    // Every try starts from this note, built when the worker stopped. Built again later,
    // it could name a try that started first as the session that stopped.
    assign::write_note(host, task, note).await?;
    let record = rules::record(host, task)?;
    let start = match &record.branch {
        Some(branch) => Some(host.git().resolve(&host.repo().root, branch).await?),
        None => None,
    };
    let log = race_log(host, task)?;
    let entrants = entrants(host, task, to, &at, &log, note).await?;
    let stop = CancellationToken::new();
    let rounds = AtomicU32::new(first_round(host, task)?);
    // What a task builds has no findings to merge, so `merge` keeps the first to pass too.
    let until = match pick {
        Pick::Judge => Until::AllEnded,
        Pick::FirstPassing | Pick::Merge => Until::FirstPassed,
    };
    // Re-entered after a restart, its tries are under way and it was said then.
    if log.started.is_empty() {
        say_who_takes_it(host, task, &entrants, until)?;
    }
    let (passed, error) = race(host, task, &entrants, &at, &rounds, &stop, until).await;
    if matches!(error, Some(EngineError::Interrupted)) {
        return Err(EngineError::Interrupted);
    }
    let winner = match passed.as_slice() {
        [] => None,
        [only] => {
            say_who_passed(host, task, only, &entrants, until)?;
            Some(only.clone())
        }
        several => {
            let tries: Vec<judge::Passed<'_>> = several
                .iter()
                .filter_map(|worker| entrants.iter().find(|entrant| &entrant.worker == worker))
                .map(|entrant| judge::Passed {
                    worker: &entrant.worker,
                    tree: &entrant.tree,
                })
                .collect();
            Some(judge::pick(host, task, &tries).await?)
        }
    };
    for entrant in &entrants {
        if Some(&entrant.worker) != winner.as_ref() {
            keep(host, task, entrant, log.since).await?;
        }
    }
    let Some(winner) = winner else {
        notes::catch_up(host, task).await?;
        return error.map_or(Ok(None), Err);
    };
    if let Some(entrant) = entrants.iter().find(|entrant| entrant.worker == winner) {
        take_the_winners_work(host, task, &entrant.tree, start.as_deref()).await?;
    }
    notes::catch_up(host, task).await?;
    Ok(Some(winner))
}

/// `Sonnet 5.5 on Claude Code and GPT 5.6 on Codex both take build:add-due`, and how their
/// results combine. After the worker went again and stopped again, the line opens with `Still
/// unreachable` and is a warning, as the retry line before it was.
fn say_who_takes_it(
    host: &Host,
    task: &TaskId,
    entrants: &[Entrant],
    until: Until,
) -> EngineResult<()> {
    let (label, stopped) = host.read(|state| {
        let stopped = newest_worker_session(state, task).cloned();
        (labels::task_label(state, task), stopped)
    });
    let still = stopped.and_then(|session| job_fallback::still_out(host, &session));
    let names: Vec<String> = entrants
        .iter()
        .map(|entrant| display_name(&entrant.worker))
        .collect();
    let together = match names.as_slice() {
        [one, other] => format!("{one} and {other} both take {label}"),
        names => format!("{} all take {label}", joined(names)),
    };
    let how = match until {
        Until::FirstPassed => "the first to pass the checks wins",
        Until::AllEnded => "the judge picks among those that pass the checks",
    };
    let text = format!("{together} · {how} · /fallback changes this");
    match still {
        Some(still) => note(host, task, NoteLevel::Warn, format!("{still} · {text}")),
        None => note(host, task, NoteLevel::Info, text),
    }
}

/// `build:add-due · GPT 5.6 on Codex passed first · Sonnet 5.5's try stopped`, or that
/// only one passed when each ran to its end.
fn say_who_passed(
    host: &Host,
    task: &TaskId,
    winner: &ModelRef,
    entrants: &[Entrant],
    until: Until,
) -> EngineResult<()> {
    let label = host.read(|state| labels::task_label(state, task));
    // The line that said who takes it named each in full, so the model alone names a loser.
    let names = DisplayNames::default();
    let others: Vec<String> = entrants
        .iter()
        .filter(|entrant| &entrant.worker != winner)
        .map(|entrant| names.short(&entrant.worker))
        .collect();
    let winner = display_name(winner);
    let text = match until {
        Until::FirstPassed => format!(
            "{label} · {winner} passed first · {} stopped",
            tries_of(&others)
        ),
        Until::AllEnded => format!("{label} · only {winner}'s try passed the checks"),
    };
    note(host, task, NoteLevel::Info, text)
}

/// `Sonnet 5.5 on Claude Code's try`, or `the tries of A and B`.
fn tries_of(names: &[String]) -> String {
    match names {
        [one] => format!("{one}'s try"),
        names => format!("the tries of {}", joined(names)),
    }
}

/// `A, B and C`.
fn joined(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn note(host: &Host, task: &TaskId, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(Some(task), None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// When the tries stop running side by side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Until {
    /// At the first that passes: the others are stopped.
    FirstPassed,
    /// Each runs to its own end.
    AllEnded,
}

/// Runs every try at once and waits for each to end: all of them on their own, or the
/// rest stopped once one passes its checks and review. Returns those that passed, in the
/// order they did, and the first error, for when none passed.
async fn race(
    host: &Host,
    task: &TaskId,
    entrants: &[Entrant],
    at: &StepAt,
    rounds: &AtomicU32,
    stop: &CancellationToken,
    until: Until,
) -> (Vec<ModelRef>, Option<EngineError>) {
    let mut running: FuturesUnordered<_> = entrants
        .iter()
        .map(|entrant| async move {
            let call = TryCall {
                task,
                worker: &entrant.worker,
                tree: &entrant.tree,
                at,
                first_slot: entrant.first_slot,
                note: &entrant.note,
                rounds,
                stop,
            };
            (&entrant.worker, pipeline::run_try(host, call).await)
        })
        .collect();
    let mut passed: Vec<ModelRef> = Vec::new();
    let mut error = None;
    let mut stopped: Vec<SessionKey> = Vec::new();
    loop {
        let next = if stop.is_cancelled() {
            stop_losers(host, task, entrants, passed.first(), &mut stopped);
            tokio::select! {
                next = running.next() => next,
                () = tokio::time::sleep(STOP_POLL) => continue,
            }
        } else {
            running.next().await
        };
        let Some((worker, end)) = next else {
            break;
        };
        match end {
            // After the first to pass, a stopped try can't have passed too.
            Ok(TryEnd::Passed) if !stop.is_cancelled() => {
                passed.push(worker.clone());
                if until == Until::FirstPassed {
                    stop.cancel();
                }
            }
            Ok(_) => {}
            Err(EngineError::Interrupted) => return (passed, Some(EngineError::Interrupted)),
            Err(err) => {
                error.get_or_insert(err);
            }
        }
    }
    (passed, error)
}

/// Stops the worker sessions of the tries that lost, each once.
fn stop_losers(
    host: &Host,
    task: &TaskId,
    entrants: &[Entrant],
    winner: Option<&ModelRef>,
    stopped: &mut Vec<SessionKey>,
) {
    let keys: Vec<SessionKey> = host.read(|state| {
        state
            .sessions
            .running()
            .filter(|session| {
                session.task.as_ref() == Some(task)
                    && session.started.role == RoleName::Worker
                    && Some(&session.started.worker) != winner
                    && entrants
                        .iter()
                        .any(|entrant| entrant.worker == session.started.worker)
                    && !stopped.contains(&session.key)
            })
            .map(|session| session.key.clone())
            .collect()
    });
    for key in keys {
        // A session that ended meanwhile refuses the control, and one with a full control
        // queue is tried again at the next poll; either way its try still ends.
        if host.control(&key, SessionControl::Stop).is_ok() {
            stopped.push(key);
        }
    }
}

/// Commits what a losing try left uncommitted, as its newest session's work. Its sessions
/// commit when they end, so only a try that broke off leaves anything.
async fn keep(
    host: &Host,
    task: &TaskId,
    entrant: &Entrant,
    since: Option<u64>,
) -> EngineResult<()> {
    let session = host.read(|state| {
        worker_sessions(state, task, &entrant.worker)
            .filter(|session| since.is_some_and(|since| session.since > since))
            .max_by_key(|session| session.since)
            .map(|session| session.key.clone())
    });
    let by = Some(entrant.worker.clone());
    worktree::commit_in(host, task, &entrant.tree, session.as_ref(), by)
        .await
        .map(drop)
}

/// A tree for each distinct candidate: the one this race already gave it, else a number no
/// tree or branch of the task has had.
async fn entrants(
    host: &Host,
    task: &TaskId,
    to: &[ModelRef],
    at: &StepAt,
    log: &RaceLog,
    note: &FallbackNote,
) -> EngineResult<Vec<Entrant>> {
    let task_tree = rules::record(host, task)?.worktree;
    let mut next = log.started.values().copied().fold(log.highest, u32::max) + 1;
    let mut entrants: Vec<Entrant> = Vec::new();
    for worker in to {
        if entrants.iter().any(|entrant| entrant.worker == *worker) {
            continue;
        }
        let try_number = match log.started.get(worker) {
            Some(try_number) => *try_number,
            None => {
                while worktree::try_taken(host, task, next).await? {
                    next += 1;
                }
                next += 1;
                next - 1
            }
        };
        // A new tree starts at the task's committed work; one this race made keeps its own.
        let tree = worktree::try_tree(host, task, try_number).await?;
        let first_slot = host.read(|state| sessions_before(state, task, worker, at, log.since));
        let note = own_worktree(note, task_tree.as_deref(), &tree.path).await;
        entrants.push(Entrant {
            worker: worker.clone(),
            tree,
            first_slot,
            note,
        });
    }
    Ok(entrants)
}

/// The note naming a copy of the transcript in the try's own tree, since a session reads
/// no worktree but its own. The git exclude that keeps the task's copy out of commits covers
/// every worktree of the repository. Without a copy, it still names the task's.
async fn own_worktree(note: &FallbackNote, task_tree: Option<&Path>, tree: &Path) -> FallbackNote {
    let mut note = note.clone();
    let Some(paths) = note.transcript.as_mut() else {
        return note;
    };
    let Some(inside) = task_tree.and_then(|task_tree| paths.worktree.strip_prefix(task_tree).ok())
    else {
        return note;
    };
    let own = tree.join(inside);
    let copied = async {
        if let Some(dir) = own.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::copy(&paths.worktree, &own).await
    };
    match copied.await {
        Ok(_) => paths.worktree = own,
        Err(error) => tracing::warn!(
            path = %own.display(),
            %error,
            "a race try is told of the task's transcript, not a copy in its own tree"
        ),
    }
    note
}

/// Reads the task's tries from the log: every try tree a session worked in and every
/// try branch a commit went to.
fn race_log(host: &Host, task: &TaskId) -> EngineResult<RaceLog> {
    let events = task_events(host, task)?;
    let run = host.run_id().clone();
    // The task's commits are in the state too, for a run that keeps no log on disk.
    let committed = host.read(|state| {
        state.tasks.get(task).map_or(0, |record| {
            record
                .commits
                .iter()
                .filter_map(|commit| worktree::try_of_branch(&run, task, &commit.branch))
                .max()
                .unwrap_or(0)
        })
    });
    let ours = |event: &&RunEvent| event.task_id.as_ref() == Some(task);
    let since = events
        .iter()
        .filter(ours)
        .filter(|event| matches!(event.kind, RunEventKind::Task(TaskEvent::State { .. })))
        .map(|event| event.seq)
        .max();
    let mut log = RaceLog {
        since,
        highest: committed,
        started: BTreeMap::new(),
    };
    for event in events.iter().filter(ours) {
        let (try_number, tree) = match &event.kind {
            RunEventKind::Task(TaskEvent::Committed(commit)) => {
                (worktree::try_of_branch(&run, task, &commit.branch), false)
            }
            RunEventKind::Session(SessionEvent::Input(input)) => {
                (worktree::try_of_tree(task, &input.launch.cwd), true)
            }
            _ => (None, false),
        };
        let Some(try_number) = try_number else {
            continue;
        };
        log.highest = log.highest.max(try_number);
        let this_race = since.is_some_and(|since| event.seq > since);
        if !(tree && this_race) {
            continue;
        }
        let worker = event.session.as_ref().and_then(|key| {
            host.read(|state| {
                state
                    .sessions
                    .get(key)
                    .map(|session| session.started.worker.clone())
            })
        });
        if let Some(worker) = worker {
            log.started.entry(worker).or_insert(try_number);
        }
    }
    Ok(log)
}

/// How many worker sessions `worker` had on the task in this step's round before the race,
/// which is the slot of its try's first session: replay finds a session by its place
/// among the worker's sessions of the same task, role, step, and round.
fn sessions_before(
    state: &RunState,
    task: &TaskId,
    worker: &ModelRef,
    at: &StepAt,
    since: Option<u64>,
) -> u32 {
    let round_start = state
        .workflows
        .by_id
        .get(&at.step.workflow)
        .and_then(|workflow| {
            workflow
                .steps
                .iter()
                .filter(|progress| progress.step == at.step && progress.round == at.round)
                .map(|progress| progress.since)
                .min()
        })
        .unwrap_or(0);
    let before = worker_sessions(state, task, worker)
        .filter(|session| {
            session.started.workflow_step.as_ref() == Some(&at.step)
                && session.since >= round_start
                && since.is_none_or(|since| session.since < since)
        })
        .count();
    u32::try_from(before).unwrap_or(u32::MAX)
}

fn worker_sessions<'a>(
    state: &'a RunState,
    task: &'a TaskId,
    worker: &'a ModelRef,
) -> impl Iterator<Item = &'a SessionRecord> {
    state.sessions.by_key.values().filter(move |session| {
        session.task.as_ref() == Some(task)
            && session.started.worker == *worker
            && session.started.role == RoleName::Worker
            && session.started.step == Some(StepName::Build)
    })
}

/// The first review round of a race: above every round the task's own review can reach, so
/// that review never takes a try's verdict for its own.
fn first_round(host: &Host, task: &TaskId) -> EngineResult<u32> {
    let record = rules::record(host, task)?;
    let above_own = host.settings().review.max_change_rounds + 2;
    Ok(upgrade::next_round(&record.reviews).max(above_own))
}

/// Moves the task's branch to the winning try, which started from the task's head.
async fn take_the_winners_work(
    host: &Host,
    task: &TaskId,
    tree: &TaskTree,
    start: Option<&str>,
) -> EngineResult<()> {
    let git = host.git();
    let record = rules::record(host, task)?;
    let head = git.resolve(&tree.path, "HEAD").await?;
    match (&record.worktree, &record.branch) {
        (Some(path), _) => {
            if !git.fast_forward(path, &head).await? {
                git.reset_hard(path, &head).await?;
            }
            // The losers' commits came last, so the task's saved diff is the winner's again.
            let own = worktree::ensure(host, task).await?;
            let own_git = worktree::tree_git(host, &own.path).await?;
            worktree::save_diff(host, task, &own_git, &own, &head).await?;
        }
        (None, Some(branch)) => {
            let root = host.repo().root.clone();
            let old = match start {
                Some(start) => start.to_owned(),
                None => git.resolve(&root, branch).await?,
            };
            git.update_branch(&root, branch, &head, &old).await?;
        }
        (None, None) => {}
    }
    Ok(())
}
