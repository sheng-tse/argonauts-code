//! A check by the checker the ladder picks, from another maker than the task's worker
//! whenever one is set up, never another model of the worker's maker. In a council's run every
//! task's check is one, whatever way the task was done: the pick is recorded with its rung and
//! the check reads the council's frame. With no other maker set up, the Same maker line goes
//! under it.

use std::path::{Path, PathBuf};
use std::slice;

use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::council::{CheckPick, CouncilEvent, CouncilEventKind};
use argonauts_core::runs::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::{Note, NoteLevel, TaskId};

use super::{Reader, Verdict, review_in};
use crate::council::seats;
use crate::council::ways::{self, Checker};
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::plan::context::part;

/// Who checks `task` in a run where a council talked, by the ladder. `None` in any other
/// run, and before the task has a worker.
pub(crate) fn checker(host: &Host, task: &TaskId) -> EngineResult<Option<Checker>> {
    if !council_talked(host) {
        return Ok(None);
    }
    ways::checker_of_task(host, task, CrewRole::Reviewer)
}

/// The run's plan is a council's: the workflow is a council, and its seats talked.
fn council_talked(host: &Host) -> bool {
    seats::in_council(host.settings()) && host.read(|state| state.councils.newest().is_some())
}

/// `checker`'s check of `task`'s work in `tree`, or in the task's own worktree.
pub(crate) async fn take(
    host: &Host,
    task: &TaskId,
    round: u32,
    checker: &Checker,
    tree: Option<&Path>,
) -> EngineResult<Verdict> {
    let council = council_talked(host);
    let reader = if council {
        record_pick(host, task, checker)?;
        Reader::CouncilCheck
    } else {
        Reader::Reviewer
    };
    let neutral = if council {
        nameless_copy(host, task).await?
    } else {
        None
    };
    let tree = neutral.as_deref().or(tree);
    let checkers = slice::from_ref(&checker.model);
    let verdict = review_in(host, task, round, checkers, tree, reader).await?;
    if checker.same_maker {
        let note = Note {
            level: NoteLevel::Info,
            text: ways::same_maker_line(host.names(), &checker.model),
        };
        host.record(Some(task), None, RunEventKind::Note(note))?;
    }
    Ok(verdict)
}

/// A copy of the task's work in a folder named for the task alone, where its check reads it,
/// when the task works in the folder a seat handed it, whose name says which seat made it.
/// `None` for a task in a worktree of its own, which names nobody.
async fn nameless_copy(host: &Host, task: &TaskId) -> EngineResult<Option<PathBuf>> {
    if crate::council::talk::folder::seat_of(host, task).is_none() {
        return Ok(None);
    }
    let worktree = host.read(|state| state.tasks.get(task)?.worktree.clone());
    let Some(worktree) = worktree else {
        return Ok(None);
    };
    let Some(head) = host.git().head_commit(&worktree).await? else {
        return Ok(None);
    };
    let folder = host.repo().worktrees.join(format!("check-{task}"));
    if folder.join(".git").exists() {
        host.git().restore_clean(&folder, &head).await?;
    } else {
        host.git()
            .add_detached_worktree(&host.repo().root, &folder, &head)
            .await?;
    }
    Ok(Some(folder))
}

/// The council's frame, after the reviewers' own that the prompt opens with.
pub(super) fn council_frame() -> InputPart {
    let text = argonauts_core::prompts::prompt_files::in_use(
        "council/check",
        crate::prompts::council::CHECK,
    );
    part(InputPartKind::Instructions, "The council's check", text)
}

/// The pick, once per task however often it's checked or the run resumes, in the talk of the
/// council whose plan the task comes from.
fn record_pick(host: &Host, task: &TaskId, checker: &Checker) -> EngineResult<()> {
    let talk = host.read(|state| {
        let council = state.councils.newest()?;
        (!council.checked.contains(task)).then(|| council.talk.clone())
    });
    let Some(talk) = talk else {
        return Ok(());
    };
    let pick = CheckPick {
        task: task.clone(),
        checker: Some(checker.model.clone()),
        rung: checker.rung(),
    };
    let event = CouncilEvent {
        talk,
        kind: CouncilEventKind::Check(pick),
    };
    host.record(Some(task), None, RunEventKind::Council(event))?;
    Ok(())
}
