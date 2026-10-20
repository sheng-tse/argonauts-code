//! `argonauts run`, for scripts and CI: the same settings and engine as a run from the app.

use crate::app_support;
use anyhow::Context as _;
use app_support::*;
use argonauts_core::events::approval::{ApprovalEvent, ApprovalKind};
use argonauts_core::events::{RunEvent, RunEventKind};
use std::path::Path;

/// The run's phases and tasks in order, which timing doesn't change.
fn milestones(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Phase(change) => Some(format!("{:?} {:?}", change.phase, change.status)),
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                Some(format!("task {:?}", opened.kind))
            }
            _ => None,
        })
        .collect()
}

fn milestones_to_merge(run_dir: &Path) -> anyhow::Result<Vec<String>> {
    wait_journal(run_dir, "the merge approval", |event| {
        matches!(&event.kind, RunEventKind::Approval(ApprovalEvent::Opened(opened))
            if opened.kind == ApprovalKind::Merge)
    })?;
    Ok(milestones(&run_events(run_dir)?))
}

#[test]
fn app_run_command_same_settings_and_engine() -> anyhow::Result<()> {
    let scenario = scenario_with(&run_config())?;

    // Without a terminal it prints plain lines, and a run waiting at an approval says how to go on.
    let output = run_cli(
        &scenario,
        &["run", "--captain", "--plain", requests::SMALL_FIX],
    )?;
    let said = output_text(&output);
    assert!(!output.status.success(), "a waiting run exited 0:\n{said}");
    let cli_dir = run_dirs(&scenario)
        .pop()
        .context("argonauts run started no run")?;
    let cli_run = run_id(&cli_dir)?;
    assert!(said.contains("waiting for you:"), "{said}");
    assert!(
        said.contains(&format!("argonauts resume {cli_run}")),
        "{said}"
    );
    assert!(
        !said.contains('\x1b'),
        "plain lines hold escape sequences:\n{said}"
    );
    let cli_started = run_started(&cli_dir)?;
    let cli_milestones = milestones_to_merge(&cli_dir)?;

    // The same task from the app.
    let mut pty = start(&scenario)?;
    let app_dir = start_captain(&scenario, &mut pty, requests::SMALL_FIX)?;
    let app_started = run_started(&app_dir)?;
    assert_eq!(
        cli_started.settings_sha256, app_started.settings_sha256,
        "argonauts run resolved other settings than the app"
    );
    assert_eq!(cli_started.mode, app_started.mode);
    assert_eq!(milestones_to_merge(&app_dir)?, cli_milestones);
    scenario.shot(&pty, "app_run_command_same_settings_and_engine")?;
    quit(&mut pty)?;

    // With no settings file it runs on the built-in defaults and never opens the setup.
    let fresh = first_run_scenario()?;
    let output = run_cli(
        &fresh,
        &["run", "--captain", "--plain", requests::SMALL_FIX],
    )?;
    let said = output_text(&output);
    assert!(
        run_dirs(&fresh).pop().is_some(),
        "no run without a settings file:\n{said}"
    );
    assert!(
        !fresh.repo.global_config().exists(),
        "argonauts run wrote a global config, as the setup would"
    );
    assert!(!fresh.repo.home().join("terms-note-shown").exists());
    Ok(())
}
