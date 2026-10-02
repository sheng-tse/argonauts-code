//! When a supervisor dies or parks: rebuilding from the journal, one process per vendor
//! session, restarting on an answer, what the attached app shows, and doctor's reminders.

mod app_support;

use std::fs::OpenOptions;
use std::io::Write as _;
use std::process::Command;

use anyhow::Context as _;

use app_support::*;
use argonauts_core::events::run::{ResumeCause, RunLifecycle, RunOutcome};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::run_files;
use argonauts_scenarios::WAIT;

fn resumed_by(cause: ResumeCause) -> impl Fn(&RunEvent) -> bool {
    move |event| matches!(&event.kind, RunEventKind::Run(RunLifecycle::Resumed(resumed)) if resumed.cause == cause)
}

/// The exit watchers `supervisor` forked, one per agent, each still running its binary.
fn exit_watchers(supervisor: u32) -> anyhow::Result<Vec<u32>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()?;
    let rows: Vec<(u32, u32, String)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            Some((pid, ppid, words.collect::<Vec<_>>().join(" ")))
        })
        .collect();
    let binary = rows
        .iter()
        .find(|(pid, _, _)| *pid == supervisor)
        .map(|(_, _, command)| command.clone())
        .context("the supervisor isn't running")?;
    Ok(rows
        .into_iter()
        .filter(|(_, ppid, command)| *ppid == supervisor && *command == binary)
        .map(|(pid, _, _)| pid)
        .collect())
}

#[test]
fn app_crash_recovery_rebuilds_from_log() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_solo(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    let orphan = wait_owner_started(&scenario)?;
    pty.wait_for("Renaming the config loader", WAIT)?;

    let supervisor = supervisor_pid(&run_dir)?;
    // When only the supervisor dies, its exit watcher stops the owner. A kill that
    // takes the watcher too, such as the OOM killer's, leaves the owner for recovery to find.
    let watchers = exit_watchers(supervisor)?;
    assert!(!watchers.is_empty(), "no exit watcher guards the owner");
    for watcher in watchers {
        kill_hard(watcher)?;
    }
    kill_hard(supervisor)?;
    wait_pid_gone(supervisor)?;
    assert!(pid_alive(orphan), "the owner died with its supervisor");
    let before = run_events(&run_dir)?;
    // A crash can tear the last line; recovery skips it.
    let mut journal = OpenOptions::new()
        .append(true)
        .open(run_dir.join(run_files::EVENTS))?;
    write!(journal, "{{\"seq\":{},\"ts\":", before.len())?;
    drop(journal);

    submit(&mut pty, &format!("/resume {run}"))?;
    let resumed = wait_journal(&run_dir, "the run to resume", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::Resumed(_)))
    })?;
    wait_pid_gone(orphan)?;

    let after = run_events(&run_dir)?;
    assert_eq!(
        after[..before.len()],
        before[..],
        "recovery rewrote the history it rebuilt from"
    );
    if let RunEventKind::Run(RunLifecycle::Resumed(resumed)) = resumed.kind {
        assert!(
            resumed.sessions.iter().any(|session| {
                session.resumed && session.vendor_session.as_deref() == Some("sess-owner-slow")
            }),
            "{resumed:?}"
        );
    }

    let owners = wait_until("the owner's session to resume", || {
        let owners = claude_sessions(&scenario, "acceptEdits")?;
        Ok((owners.len() >= 2).then_some(owners))
    })?;
    assert!(has_flag(&owners[1].args, "--resume", "sess-owner-slow"));
    let live: Vec<u32> = owners
        .iter()
        .map(|owner| owner.pid)
        .filter(|pid| pid_alive(*pid))
        .collect();
    assert!(
        live.len() <= 1,
        "more than one process for one vendor session: {live:?}"
    );
    scenario.shot(&pty, "app_crash_recovery_rebuilds_from_log")?;
    Ok(())
}

#[test]
fn app_parked_run_exits_and_restarts_on_answer() -> anyhow::Result<()> {
    let scenario = scenario_with(&run_config())?;
    let mut pty = start(&scenario)?;
    let (run_dir, _) = reach_merge_card(&scenario, &mut pty)?;
    wait_journal(&run_dir, "the run to park", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::Parked { .. }))
    })?;
    let parked_supervisor = supervisor_pid(&run_dir)?;
    // The scenario's park grace is one second.
    wait_pid_gone(parked_supervisor)?;

    // Merge comes first.
    answer_card_and_confirm(&mut pty, '1')?;
    wait_journal(
        &run_dir,
        "a new supervisor to take the answer",
        resumed_by(ResumeCause::Answer),
    )?;
    wait_journal(&run_dir, "the run to finish merged", |event| {
        matches!(
            event.kind,
            RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::Merged,
                ..
            })
        )
    })?;
    if let Ok(pid) = supervisor_pid(&run_dir) {
        assert_ne!(pid, parked_supervisor, "the parked supervisor never exited");
    }
    scenario.shot(&pty, "app_parked_run_exits_and_restarts_on_answer")?;
    Ok(())
}

#[test]
fn app_supervisor_crash_while_attached_shows_crashed() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_solo(&scenario, &mut pty, requests::SLOW_TASK)?;
    wait_owner_started(&scenario)?;

    kill_hard(supervisor_pid(&run_dir)?)?;
    let screen = pty.wait_for(r"(?i)crashed", WAIT)?;
    let text = screen.text();
    assert!(
        text.contains("/resume"),
        "the notice doesn't say how to recover:\n{text}"
    );
    check_mode_line(&pty)?;
    scenario.shot(&pty, "app_supervisor_crash_while_attached_shows_crashed")?;
    Ok(())
}

#[test]
fn app_doctor_lists_long_parked_runs() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let long = fabricate_parked_run(&scenario, 9)?;
    let recent = fabricate_parked_run(&scenario, 1)?;

    let output = run_cli(&scenario, &["doctor"])?;
    let text = output_text(&output);
    assert!(
        text.contains(&format!("! {} has been parked for 9 days", long.run)),
        "{text}"
    );
    assert!(text.contains("Parked runs never expire."), "{text}");
    assert!(
        !text.contains(recent.run.as_str()),
        "a run parked for a day is listed:\n{text}"
    );
    // Reading journals starts nothing.
    let sessions = all_sessions(&scenario)?;
    assert!(sessions.is_empty(), "{sessions:#?}");
    Ok(())
}
