//! `/runs archive`: moves a run that can't go on out of the way. Its folder goes whole from
//! the runs of this project to `runs-archived` beside them, so Status and `/resume` stop
//! listing it and nothing of it is deleted.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::{NoteLevel, RunId};

use crate::client::day;
use crate::controller::{Ctx, Effect};

/// Beside a project's `runs`.
const ARCHIVED: &str = "runs-archived";

/// Archives `run`, or with none every run another Argonauts Code wrote.
pub(crate) async fn command(ctx: &mut Ctx, run: Option<RunId>) -> anyhow::Result<Vec<Effect>> {
    let list = ctx
        .services
        .conductor
        .list()
        .await
        .context("listing this folder's runs")?;
    let chosen: Vec<&RunSummary> = match &run {
        Some(run) => {
            let Some(found) = list.iter().find(|listed| listed.run == *run) else {
                bail!("There is no run {run} in this folder.");
            };
            if let Some(why) = keeps(found) {
                bail!("{run} {why}, so it stays. /runs archive takes a run that has ended.");
            }
            vec![found]
        }
        None => list
            .iter()
            .filter(|run| run.state == RunState::FromAnotherBuild)
            .collect(),
    };
    if chosen.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "No run here was written by an older Argonauts. /runs archive <run> moves one \
             that has ended.",
        );
        return Ok(Vec::new());
    }
    let to = ctx.env.state_dir.join(ARCHIVED);
    for run in &chosen {
        move_run(&ctx.env.runs_dir().join(run.run.as_str()), &to)?;
    }
    let named = match chosen.as_slice() {
        [one] => format!(
            "\"{}\" ({})",
            one.name.as_deref().unwrap_or(&one.title),
            day(one.updated)
        ),
        many => format!("{} runs", many.len()),
    };
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Archived {named} · moved to {} and out of Status and /resume",
            ctx.env.display(&to)
        ),
    );
    Ok(vec![Effect::Status])
}

/// Why a run can't be archived: it still goes, or it waits to go on.
fn keeps(run: &RunSummary) -> Option<&'static str> {
    match run.state {
        _ if run.live => Some("is still going"),
        RunState::Running | RunState::Pausing => Some("is still going"),
        RunState::Waiting => Some("waits to go on"),
        _ => None,
    }
}

/// Moves the run's folder into `to`, under a name no archived run has yet.
fn move_run(run_dir: &Path, to: &Path) -> anyhow::Result<PathBuf> {
    create_private_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    let name = run_dir
        .file_name()
        .with_context(|| format!("{} has no name", run_dir.display()))?;
    let mut target = to.join(name);
    let mut copy = 1;
    while target.exists() {
        copy += 1;
        target = to.join(format!("{}-{copy}", name.to_string_lossy()));
    }
    std::fs::rename(run_dir, &target)
        .with_context(|| format!("moving {} to {}", run_dir.display(), target.display()))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archived_run_moves_whole_and_never_over_another() {
        let state = tempfile::tempdir().unwrap();
        let runs = state.path().join("runs");
        let archived = state.path().join(ARCHIVED);
        for round in 0..2 {
            let run = runs.join("run-01");
            std::fs::create_dir_all(run.join("sessions")).unwrap();
            std::fs::write(run.join("events.jsonl"), format!("round {round}\n")).unwrap();
            let moved = move_run(&run, &archived).unwrap();
            assert!(!run.exists());
            assert!(moved.join("sessions").is_dir());
            assert_eq!(
                std::fs::read_to_string(moved.join("events.jsonl")).unwrap(),
                format!("round {round}\n")
            );
        }
        assert!(archived.join("run-01").is_dir() && archived.join("run-01-2").is_dir());
    }
}
