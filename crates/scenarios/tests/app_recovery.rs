//! When a run host dies or waits: rebuilding from the journal, one process per agent
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

/// The exit watchers the run host forked, one per agent, each still running its binary.
fn exit_watchers(run_host: u32) -> anyhow::Result<Vec<u32>> {
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
        .find(|(pid, _, _)| *pid == run_host)
        .map(|(_, _, command)| command.clone())
        .context("the run host isn't running")?;
    Ok(rows
        .into_iter()
        .filter(|(_, ppid, command)| *ppid == run_host && *command == binary)
        .map(|(pid, _, _)| pid)
        .collect())
}

#[test]
fn app_crash_recovery_rebuilds_from_log() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_captain(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    let orphan = wait_worker_started(&scenario)?;
    pty.wait_for("Renaming the config loader", WAIT)?;

    let run_host = run_host_pid(&run_dir)?;
    // When only the run host dies, its exit watcher stops the worker. A kill that
    // takes the watcher too, such as the OOM killer's, leaves the worker for recovery to find.
    let watchers = exit_watchers(run_host)?;
    assert!(!watchers.is_empty(), "no exit watcher guards the worker");
    for watcher in watchers {
        kill_hard(watcher)?;
    }
    kill_hard(run_host)?;
    wait_pid_gone(run_host)?;
    assert!(pid_alive(orphan), "the worker died with its run host");
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
                session.resumed && session.cli_session.as_deref() == Some("sess-worker-slow")
            }),
            "{resumed:?}"
        );
    }

    let workers = wait_until("the worker's session to resume", || {
        let workers = claude_sessions(&scenario, "acceptEdits")?;
        Ok((workers.len() >= 2).then_some(workers))
    })?;
    assert!(has_flag(&workers[1].args, "--resume", "sess-worker-slow"));
    let live: Vec<u32> = workers
        .iter()
        .map(|worker| worker.pid)
        .filter(|pid| pid_alive(*pid))
        .collect();
    assert!(
        live.len() <= 1,
        "more than one process for one agent session: {live:?}"
    );
    scenario.shot(&pty, "app_crash_recovery_rebuilds_from_log")?;
    Ok(())
}

#[test]
fn app_waiting_run_exits_and_restarts_on_answer() -> anyhow::Result<()> {
    let scenario = scenario_with(&run_config())?;
    let mut pty = start(&scenario)?;
    let (run_dir, _) = reach_merge_question(&scenario, &mut pty)?;
    wait_journal(&run_dir, "the run to wait", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::Waiting { .. }))
    })?;
    let waiting_run_host = run_host_pid(&run_dir)?;
    // The scenario's wait grace is one second.
    wait_pid_gone(waiting_run_host)?;

    // Merge comes first.
    answer_question_and_confirm(&mut pty, '1')?;
    wait_journal(
        &run_dir,
        "a new run host to take the answer",
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
    if let Ok(pid) = run_host_pid(&run_dir) {
        assert_ne!(pid, waiting_run_host, "the waiting run host never exited");
    }
    scenario.shot(&pty, "app_waiting_run_exits_and_restarts_on_answer")?;
    Ok(())
}

#[test]
fn app_run_host_crash_while_attached_says_what_resume_does() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_captain(&scenario, &mut pty, requests::SLOW_TASK)?;
    wait_worker_started(&scenario)?;

    kill_hard(run_host_pid(&run_dir)?)?;
    let screen = pty.wait_for(r"/resume\s+carries\s+it\s+on", WAIT)?;
    // One sentence, however the screen's width breaks it.
    let text = screen.text();
    let said = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        said.contains("when its run host exited · /resume carries it on"),
        "the notice doesn't say what happened and how to go on:\n{text}"
    );
    assert!(!said.contains("crashed:"), "{text}");
    check_bottom_row(&pty)?;
    scenario.shot(
        &pty,
        "app_run_host_crash_while_attached_says_what_resume_does",
    )?;
    Ok(())
}

#[test]
fn app_doctor_lists_long_waiting_runs() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let long = fabricate_waiting_run(&scenario, 9)?;
    let recent = fabricate_waiting_run(&scenario, 1)?;

    let output = run_cli(&scenario, &["doctor"])?;
    let text = output_text(&output);
    assert!(
        text.contains(&format!("! {} has been waiting for 9 days", long.run)),
        "{text}"
    );
    assert!(text.contains("Waiting runs never expire."), "{text}");
    assert!(
        !text.contains(recent.run.as_str()),
        "a run waiting for a day is listed:\n{text}"
    );
    // Reading journals starts nothing.
    let sessions = all_sessions(&scenario)?;
    assert!(sessions.is_empty(), "{sessions:#?}");
    Ok(())
}
