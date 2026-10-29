//! The folder each seat works in, from its first answer to the end of the task it builds.
//!
//! A CLI keeps a session with the folder it began in. So each seat talks in a worktree of its
//! own at the run's base, read-only, and the task the seat then builds takes that same folder
//! over: the seat's session carries on there instead of starting fresh somewhere else.

use std::path::PathBuf;

use argonauts_core::TaskId;
use argonauts_core::events::approval::TaskSeat;
use argonauts_core::events::council::TextPart;
use argonauts_core::events::task::{TaskEvent, TaskSpec};
use argonauts_core::names::{ModelRef, SeatId, SessionKey};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::workflow::StepRef;

use crate::council::seats;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::merge::branch::{ensure_run_branch, run_base, run_branch, task_branch};
use crate::task::support::{record_of, record_task};

/// Where `seat` of the council at `talk` works: beside the tasks' worktrees, named for the
/// council as well as the seat, since every council numbers its seats from one.
pub fn path(host: &Host, talk: &StepRef, seat: &SeatId) -> PathBuf {
    let place: Vec<String> = talk.path.iter().map(u32::to_string).collect();
    host.repo().worktrees.join(format!(
        "council-{}-{}-{}-{seat}",
        talk.workflow,
        talk.phase,
        place.join(".")
    ))
}

/// The seat's folder, made at the run's base the first time the seat is asked anything.
pub async fn ensure(host: &Host, talk: &StepRef, seat: &SeatId) -> EngineResult<PathBuf> {
    let folder = path(host, talk, seat);
    if folder.join(".git").exists() {
        return Ok(folder);
    }
    let base = run_base(host)?;
    if let Some(parent) = folder.parent() {
        create_private_dir_all(parent)?;
    }
    host.git()
        .add_detached_worktree(&host.repo().root, &folder, &base)
        .await?;
    Ok(folder)
}

/// Hands the folder of `seat` to `task`, the task the seat builds: the folder becomes the task's
/// worktree, on the task's branch from the run's head, in the very same place, so the seat's
/// session carries on there. True when it did.
///
/// A folder that is gone, that a talk turn left changes in, or that another task already took
/// stays as it is, and a task that already has a worktree keeps it. Such a task gets a worktree
/// of its own, as any task does.
pub async fn hand_to_task(
    host: &Host,
    talk: &StepRef,
    seat: &SeatId,
    task: &TaskId,
) -> EngineResult<bool> {
    let folder = path(host, talk, seat);
    if !folder.join(".git").exists() || record_of(host, task)?.worktree.is_some() {
        return Ok(false);
    }
    let taken = host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .any(|other| other.worktree.as_ref() == Some(&folder))
    });
    let git = host.git();
    if taken || !git.is_clean(&folder).await? {
        return Ok(false);
    }
    ensure_run_branch(host).await?;
    let run = run_branch(host.run_id());
    let base = git
        .branch_head(&host.repo().root, &run)
        .await?
        .ok_or_else(|| EngineError::Refused(format!("{run} has no commits")))?;
    // A clean read-only worktree holds nothing to lose, so the task's worktree is made again in
    // its place, on the task's branch, just as any task's is.
    git.remove_worktree(&host.repo().root, &folder).await?;
    record_task(
        host,
        task,
        TaskEvent::Worktree {
            path: folder,
            branch: task_branch(host.run_id(), task),
            base,
        },
    )?;
    Ok(true)
}

/// [`hand_to_task`] for the seat of the newest council, the one whose plan the tasks come from.
pub async fn hand_newest_to_task(host: &Host, seat: &SeatId, task: &TaskId) -> EngineResult<bool> {
    let talk = host.read(|state| state.councils.newest().map(|council| council.talk.clone()));
    match talk {
        Some(talk) => hand_to_task(host, &talk, seat, task).await,
        None => Ok(false),
    }
}

/// Hands each seat of the newest council the folder of a task it builds, before any task opens
/// a worktree: the task the plan says the seat does, else the first of the plan's tasks, in
/// order, that the seat's own model works. A session carries on only in the CLI that began
/// it, so a seat builds no task of another model's. Returns which seat took which task.
pub async fn hand_to_builders(host: &Host) -> EngineResult<Vec<(SeatId, TaskId)>> {
    let found = host.read(|state| {
        let council = state.councils.newest()?;
        // A talk still going has no plan to build yet.
        council.ending?;
        let seating = council.seating.clone()?;
        let graph = state.divide.graph.as_ref()?;
        let open: Vec<(TaskId, ModelRef)> = graph
            .tasks
            .iter()
            .filter(|spec| {
                state
                    .tasks
                    .get(&spec.id)
                    .is_some_and(|task| task.worktree.is_none())
            })
            .filter_map(|spec| Some((spec.id.clone(), spec.worker.clone()?)))
            .collect();
        let planned = planned_seats(council, &graph.tasks);
        Some((
            council.talk.clone(),
            seating,
            graph.tasks.len(),
            open,
            planned,
        ))
    });
    let Some((talk, seating, tasks, mut open, planned)) = found else {
        return Ok(Vec::new());
    };
    let mut handed: Vec<(SeatId, TaskId)> = Vec::new();
    let builders = seats::builders(&seating, tasks);
    // The plan says which seat does which task, which matters where two seats share a model.
    for (seat, task) in &planned {
        let Some(sitting) = builders.iter().find(|sitting| &sitting.id == seat) else {
            continue;
        };
        let Some(at) = open
            .iter()
            .position(|(id, worker)| id == task && worker == &sitting.model)
        else {
            continue;
        };
        if hand_to_task(host, &talk, seat, task).await? {
            open.remove(at);
            handed.push((seat.clone(), task.clone()));
        }
    }
    for seat in builders {
        if handed.iter().any(|(taken, _)| *taken == seat.id) {
            continue;
        }
        let Some(at) = open.iter().position(|(_, worker)| worker == &seat.model) else {
            continue;
        };
        // A seat whose folder can't be handed leaves the task to another seat of its model.
        if hand_to_task(host, &talk, &seat.id, &open[at].0).await? {
            let (task, _) = open.remove(at);
            handed.push((seat.id.clone(), task));
        }
    }
    Ok(handed)
}

/// Which seat builds each of `tasks` that the newest council's plan gives one, for the plan you
/// approve to name it. Nothing for a plan no council wrote.
pub fn planned(host: &Host, tasks: &[TaskSpec]) -> Vec<TaskSeat> {
    host.read(|state| {
        let Some(council) = state.councils.newest() else {
            return Vec::new();
        };
        planned_seats(council, tasks)
            .into_iter()
            .map(|(seat, task)| TaskSeat { task, seat })
            .collect()
    })
}

/// Which seat the note keeper's plan says does which task: each sentence of its plan message
/// that names one seat, by letter, and one task.
fn planned_seats(
    council: &crate::council::talk::Council,
    tasks: &[TaskSpec],
) -> Vec<(SeatId, TaskId)> {
    let Some(plan) = council
        .messages
        .iter()
        .rev()
        .find(|said| council.plans.contains(&said.message.id))
    else {
        return Vec::new();
    };
    let mut planned: Vec<(SeatId, TaskId)> = Vec::new();
    let mut named: Vec<SeatId> = Vec::new();
    let mut sentence = String::new();
    // A sentence that names two seats says nothing of who does the task.
    let mut close = |named: &mut Vec<SeatId>, sentence: &mut String| {
        if let [seat] = named.as_slice()
            && let Some(at) = crate::council::ways::task_named(tasks, sentence)
            && !planned.iter().any(|(_, task)| *task == tasks[at].id)
        {
            planned.push((seat.clone(), tasks[at].id.clone()));
        }
        named.clear();
        sentence.clear();
    };
    for part in &plan.message.text {
        match part {
            TextPart::Seat(seat) => {
                if !named.contains(seat) {
                    named.push(seat.clone());
                }
            }
            TextPart::Words(words) => {
                for piece in words.split_inclusive(['.', '\n', ';']) {
                    sentence.push_str(piece);
                    if piece.ends_with(['.', '\n', ';']) {
                        close(&mut named, &mut sentence);
                    }
                }
            }
        }
    }
    close(&mut named, &mut sentence);
    planned
}

/// [`hand_to_builders`] for a task a script starts, which the plan's graph doesn't hold: the
/// folder of a seat of the newest council that builds, works `worker`'s model, and hasn't
/// handed its folder to another task. True when it did.
pub async fn hand_to_script_task(
    host: &Host,
    task: &TaskId,
    worker: &ModelRef,
) -> EngineResult<bool> {
    let found = host.read(|state| {
        let council = state.councils.newest()?;
        // A talk still going has no plan to build yet.
        council.ending?;
        let seating = council.seating.clone()?;
        let tasks = state
            .divide
            .graph
            .as_ref()
            .map_or(0, |graph| graph.tasks.len());
        Some((council.talk.clone(), seating, tasks))
    });
    let Some((talk, seating, tasks)) = found else {
        return Ok(false);
    };
    for seat in seats::builders(&seating, tasks) {
        if &seat.model == worker && hand_to_task(host, &talk, &seat.id, task).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The seat whose folder `task` took over, and the council it sits in, whose own session the
/// task's turns carry on.
pub fn seat_of(host: &Host, task: &TaskId) -> Option<(StepRef, SeatId)> {
    let (worktree, councils) = host.read(|state| {
        let worktree = state.tasks.get(task)?.worktree.clone()?;
        let councils: Vec<(StepRef, Vec<SeatId>)> = state
            .councils
            .iter()
            .filter_map(|council| {
                let seats = council.seating.as_ref()?.seats.iter();
                Some((
                    council.talk.clone(),
                    seats.map(|seat| seat.id.clone()).collect(),
                ))
            })
            .collect();
        Some((worktree, councils))
    })?;
    councils.into_iter().find_map(|(talk, seats)| {
        let seat = seats
            .into_iter()
            .find(|seat| path(host, &talk, seat) == worktree)?;
        Some((talk, seat))
    })
}

/// The session `task`'s first try carries on: the talk session of the seat whose folder the
/// task took over, while the task's worker is still the seat's model.
pub fn session_to_carry_on(host: &Host, task: &TaskId, worker: &ModelRef) -> Option<SessionKey> {
    let (talk, seat) = seat_of(host, task)?;
    host.read(|state| {
        let key = crate::lifetime::seat_session(state, &talk, &seat)?;
        let session = state.sessions.get(&key)?;
        (session.asked_worker() == worker && session.cli_session.is_some()).then_some(key)
    })
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::council::{Seat, SeatRole, Seating};

    use super::*;

    fn seating(roles: &[SeatRole]) -> Seating {
        Seating {
            seats: roles
                .iter()
                .zip(1..)
                .map(|(role, n)| Seat {
                    id: SeatId::numbered(n),
                    model: "codex/gpt-5".parse().expect("a model"),
                    role: *role,
                    number: Some(u8::try_from(n).expect("a few seats")),
                })
                .collect(),
            notes: None,
        }
    }

    /// The seats offered their folder for a task: a checker only when the tasks outnumber the
    /// other seats.
    #[test]
    fn a_checker_builds_only_when_the_tasks_outnumber_the_other_seats() {
        let seats = seating(&[SeatRole::Proposer, SeatRole::Checker, SeatRole::Skeptic]);
        let ids = |tasks| -> Vec<String> {
            seats::builders(&seats, tasks)
                .iter()
                .map(|seat| seat.id.to_string())
                .collect()
        };
        assert_eq!(ids(2), ["seat-001", "seat-003"]);
        assert_eq!(ids(3), ["seat-001", "seat-002", "seat-003"]);
    }
}
