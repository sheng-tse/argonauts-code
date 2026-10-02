//! `argonauts approve <run> <checkpoint>`: approving from a shell. Only the app holds the
//! approval path, so this checks that a person can be at this terminal, then opens the app on
//! the run with the checkpoint's card, and you answer it there.

use std::io::IsTerminal as _;
use std::process::ExitCode;

use anyhow::{anyhow, bail};
use argonauts_app::{FirstView, InteractiveOptions};
use argonauts_core::RunId;
use argonauts_core::api::RunSummary;
use argonauts_core::names::CheckpointId;

use crate::usage::usage;

pub async fn run(
    run: &str,
    checkpoint: &str,
    options: InteractiveOptions,
) -> anyhow::Result<ExitCode> {
    let run = RunId::new(run).map_err(usage)?;
    let checkpoint = CheckpointId::parse(checkpoint).ok_or_else(|| {
        usage(anyhow!(
            "{checkpoint:?} isn't a checkpoint id, such as cp-001"
        ))
    })?;
    // An agent can fake a terminal, so its markers and ancestry count first.
    argonauts_app::refuse_inside_agent()?;
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!("approving needs you at a terminal; run it there, or answer in the app");
    }
    let root = argonauts_app::repo_root(options.repo.as_deref())?;
    let runs = argonauts_app::conductor(&root)?.list().await?;
    waiting_at(&runs, &run, &checkpoint)?;
    argonauts_app::run_interactive_with(options, FirstView::Resume(run)).await?;
    Ok(ExitCode::SUCCESS)
}

/// The run is this repo's, and the checkpoint waits for you on it.
fn waiting_at(runs: &[RunSummary], run: &RunId, checkpoint: &CheckpointId) -> anyhow::Result<()> {
    let Some(summary) = runs.iter().find(|summary| &summary.run == run) else {
        bail!("there's no {run} in this repo; `argonauts inbox` lists what waits for you");
    };
    if summary.waiting.contains(checkpoint) {
        return Ok(());
    }
    if summary.waiting.is_empty() {
        bail!("{run} isn't waiting for you");
    }
    let waiting: Vec<&str> = summary.waiting.iter().map(CheckpointId::as_str).collect();
    bail!(
        "{run} isn't waiting at {checkpoint}; it waits at {}",
        waiting.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::api::RunState;
    use argonauts_core::events::run::RunMode;

    use super::*;

    fn summary(run: &str, waiting: &[u32]) -> RunSummary {
        RunSummary {
            run: RunId::new(run).unwrap(),
            title: "Add a flag".to_owned(),
            mode: RunMode::Crew,
            preset: "peer-crew".to_owned(),
            state: RunState::Parked,
            stopped_at: String::new(),
            updated: time::OffsetDateTime::UNIX_EPOCH,
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: waiting.iter().map(|n| CheckpointId::numbered(*n)).collect(),
            live: false,
            remind: false,
        }
    }

    fn check(runs: &[RunSummary], run: &str, checkpoint: u32) -> Result<(), String> {
        waiting_at(
            runs,
            &RunId::new(run).unwrap(),
            &CheckpointId::numbered(checkpoint),
        )
        .map_err(|err| err.to_string())
    }

    #[test]
    fn only_a_checkpoint_waiting_on_the_run_opens() {
        let runs = [summary("run-01", &[]), summary("run-02", &[1, 3])];
        assert_eq!(check(&runs, "run-02", 3), Ok(()));
        assert_eq!(
            check(&runs, "run-02", 2),
            Err("run-02 isn't waiting at cp-002; it waits at cp-001, cp-003".to_owned())
        );
        assert_eq!(
            check(&runs, "run-01", 1),
            Err("run-01 isn't waiting for you".to_owned())
        );
        assert!(check(&runs, "run-09", 1).unwrap_err().contains("no run-09"));
    }
}
