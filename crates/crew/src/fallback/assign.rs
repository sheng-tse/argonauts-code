//! Giving a task to a new worker: the checkpoint tag, the note the new worker starts from, the
//! reassignment, and quality debt when the new worker is weaker than the strongest one before it.

use std::path::PathBuf;
use std::slice;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::agent_status::FallbackEvent;
use argonauts_core::events::run::Party;
use argonauts_core::events::task::TaskEvent;
use argonauts_core::fallback::{CheckpointTag, QualityDebt};
use argonauts_core::limits::FallbackNote;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::fallback::{FallbackMode, UnknownTier};
use argonauts_core::steps::StepName;
use argonauts_core::{TaskId, sha256_hex};

use crate::engine::host::Host;
use crate::engine::job_fallback;
use crate::error::EngineResult;
use crate::fallback::{broken, debt, down, notes, rules, tags};
use crate::roles;
use crate::state::tasks::TaskEntry;

/// Gives the task to `to`: tags its committed work first when `tag` is set, then the note,
/// the reassignment, and any debt. The new worker continues at the task's next try.
pub async fn fallback(
    host: &Host,
    task: &TaskId,
    to: &ModelRef,
    note: &FallbackNote,
    tag: bool,
    by: Party,
) -> EngineResult<Option<CheckpointTag>> {
    let record = rules::record(host, task)?;
    let from = rules::worker(&record);
    let checkpoint =
        checkpoint(host, &record, from.as_ref(), slice::from_ref(to), note, tag).await?;
    let mut note = note.clone();
    if let Some(kept_on) = broken::set_aside(host, &record).await? {
        note.notes.push(format!(
            "Its work failed the checks, so it was kept on {kept_on} and the task starts again \
             from its base."
        ));
    }
    write_note(host, task, &note).await?;
    let since = checkpoint.as_ref().map_or(record.fallbacks, |tag| tag.n);
    settle(host, &record, from.as_ref(), to, by, since)?;
    Ok(checkpoint)
}

/// Tags the task's work before anyone else touches it, when checkpoints are on and there is
/// work on a branch to tag.
pub async fn checkpoint(
    host: &Host,
    record: &TaskEntry,
    from: Option<&ModelRef>,
    to: &[ModelRef],
    note: &FallbackNote,
    tag: bool,
) -> EngineResult<Option<CheckpointTag>> {
    notes::catch_up(host, &record.spec.id).await?;
    let (Some(from), true, Some(_)) = (from, tag, record.branch.as_ref()) else {
        return Ok(None);
    };
    let reason = if note.why_stopped.is_empty() {
        "fell back"
    } else {
        note.why_stopped.as_str()
    };
    tags::tag(host, &record.spec.id, from, to, reason)
        .await
        .map(Some)
}

/// Saves the note beside the run's log and records it, so the next try starts from it.
pub async fn write_note(host: &Host, task: &TaskId, note: &FallbackNote) -> EngineResult<()> {
    let record = rules::record(host, task)?;
    let text = serde_json::to_string_pretty(note)?;
    let path = note_path(host, task, record.reassigned + 1);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(&path, &text).await?;
    host.record(
        Some(task),
        None,
        RunEventKind::Fallback(FallbackEvent::Note {
            task: task.clone(),
            path,
            sha256: sha256_hex(text.as_bytes()),
        }),
    )?;
    Ok(())
}

fn note_path(host: &Host, task: &TaskId, n: u32) -> PathBuf {
    host.repo()
        .run_dir
        .join("fallbacks")
        .join(format!("{task}-{n}.json"))
}

/// Records the new worker and any debt, then puts the task back in line.
pub fn settle(
    host: &Host,
    record: &TaskEntry,
    from: Option<&ModelRef>,
    to: &ModelRef,
    by: Party,
    since: u32,
) -> EngineResult<()> {
    let task = &record.spec.id;
    let rules = rules::applied_for(host, &record.spec, from.unwrap_or(to));
    // Only your pick on its question was asked; rules, a timed-out ask, or the judge fall back
    // as auto does.
    let mode = match by {
        Party::User => rules.mode,
        _ => FallbackMode::Auto,
    };
    // The setting is why it stayed only where another CLI could have taken it, and never
    // where you picked the worker yourself.
    let kept_on_its_cli = rules.same_cli
        && by != Party::User
        && from.is_some_and(|from| from.agent_id() == to.agent_id())
        && !down::elsewhere(host, record).is_empty();
    host.record(
        Some(task),
        None,
        RunEventKind::Task(TaskEvent::Reassigned {
            from: from.cloned(),
            to: to.clone(),
            by: by.clone(),
            rule_override: None,
        }),
    )?;
    host.record(
        Some(task),
        None,
        RunEventKind::Fallback(FallbackEvent::FellBack {
            task: task.clone(),
            to: to.clone(),
            mode,
            by,
            context: Some(rules.context),
        }),
    )?;
    if let Some(from) = from {
        say_in_the_view(host, record, from, to, kept_on_its_cli)?;
    }
    if let Some(debt) = debt_for(host, record, from, to, since, rules.unknown_tier) {
        host.record(
            Some(task),
            None,
            RunEventKind::Task(TaskEvent::QualityDebt(debt)),
        )?;
    }
    rules::back_in_line(host, task, &format!("fell back to {to}"))
}

/// The task's workflow says the fallback in one line as well, which its view shows under its
/// header beside the fallback's row: who stopped, from how the task's last session with that
/// worker ended, and who the task fell back to.
fn say_in_the_view(
    host: &Host,
    record: &TaskEntry,
    from: &ModelRef,
    to: &ModelRef,
    kept_on_its_cli: bool,
) -> EngineResult<()> {
    let task = &record.spec.id;
    let (ended, step) = host.read(|state| {
        let stopped = state
            .sessions
            .by_key
            .values()
            .filter(|session| session.task.as_ref() == Some(task))
            .filter(|session| session.started.step == Some(StepName::Build))
            .filter(|session| session.started.worker == *from)
            .max_by_key(|session| session.since);
        let ended = stopped
            .and_then(|session| session.end.as_ref())
            .map(|end| end.reason.clone());
        let step = stopped
            .and_then(|session| session.started.workflow_step.clone())
            .or_else(|| rules::current_step(state).map(|at| at.step));
        (ended, step)
    });
    // A run with no step under way has no workflow's view to say it in.
    let Some(step) = step else {
        return Ok(());
    };
    // Named by the models they run, so a bare CLI that runs the stopped model reads as it.
    let line = job_fallback::task_in_the_view(
        &roles::runs(host, from),
        &roles::runs(host, to),
        ended.as_ref(),
        kept_on_its_cli,
    );
    job_fallback::say_in_the_view(host, &step.workflow, line)
}

/// Debt when `to` is weaker than the strongest worker the task had: its planned worker, the one
/// that stopped, and the one an earlier debt already waits for. An earlier debt keeps its start.
pub fn debt_for(
    host: &Host,
    record: &TaskEntry,
    from: Option<&ModelRef>,
    to: &ModelRef,
    since: u32,
    unknown: UnknownTier,
) -> Option<QualityDebt> {
    let previous = [
        record.spec.worker.as_ref(),
        from,
        record.debt.as_ref().map(|debt| &debt.needs),
    ];
    let needs = debt::strongest(host, &record.spec, previous.into_iter().flatten(), unknown)?;
    if !debt::weaker(host, &record.spec, to, &needs, unknown) {
        return None;
    }
    Some(QualityDebt {
        since: record.debt.as_ref().map_or(since, |debt| debt.since),
        by: to.clone(),
        needs,
    })
}
