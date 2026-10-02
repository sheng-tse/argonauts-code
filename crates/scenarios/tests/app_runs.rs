//! Runs from the chat: the small fix to its merge, closing and reattaching, pausing, stopping,
//! resuming and continuing, main sessions across runs, the gates offer, handoff checkpoints and
//! remembered picks, the lead's workflow design, `/context`, runs in other repos, and every
//! command.

mod app_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use app_support::*;
use argonauts_core::app::{AppChoice, AppEvent, AppMode, ChoiceKind, TraceLine};
use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::crew_panel::{CrewPanel, CrewState, CrewTarget};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointKind, CheckpointOpened, CheckpointPacket,
};
use argonauts_core::events::run::{ParkReason, RunLifecycle, RunOutcome};
use argonauts_core::events::session::{EndReason, SessionEvent};
use argonauts_core::events::{CardEvent, RunEventKind, WorkflowEvent};
use argonauts_core::input::{Handler, SLASH_COMMANDS};
use argonauts_core::layout::{run_files, user_files};
use argonauts_core::names::WorkerRef;
use argonauts_core::views::CheckpointRow;
use argonauts_core::workflow::{Count, StepKind};
use argonauts_core::{Paths, RepoId, RunId};
use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::{Invocation, Key, Pty};
use serde_json::{Value, json};

/// A solo change whose Claude owner commits, then hits its usage limit, so Codex finishes it.
const LIMITED_TASK: &str = "move the export job onto the worker queue";
/// A review, for which the lead picks the review panel.
const REVIEW_TASK: &str = "Review the retry change in src/retry.rs";
/// The workflow the lead picks for a review.
const REVIEW_PANEL: &str = r#"name: review-panel
description: Two reviewers from other families; an adjudicator only if they disagree
params: { target: diff, reviewers: 2 }
phases:
  - name: Review
    standard: check
    steps:
      - kind: fan_out
        role: reviewer
        count: "{{reviewers}}"
        blind: true
        different_family_from: author
  - name: Adjudicate
    standard: decide
    steps:
      - kind: judge
        role: adjudicator
        when: reviewers_disagree
"#;
/// The project file's first line, which every write must keep.
const KEPT_COMMENT: &str = "# Written by the scenario test, kept through every change.";

/// `run_config()` with merges waiting for you: by default they land on their own when every
/// check passes, and these scenarios answer the merge card.
fn merge_card_config() -> String {
    format!(
        "{}\n[approvals.merge]\nmode = \"supervised\"\n",
        run_config()
    )
}

/// The global config with a model for Codex, which every Codex session names since Codex never
/// reports which one ran.
fn codex_config(base: &str) -> String {
    format!("{base}\n[agents.codex]\nmodel = \"gpt-5.6-luna\"\n")
}

fn is_finished(outcome: RunOutcome) -> impl Fn(&argonauts_core::events::RunEvent) -> bool {
    move |event| matches!(&event.kind, RunEventKind::Run(RunLifecycle::Finished { outcome: o, .. }) if *o == outcome)
}

fn is_parked(event: &argonauts_core::events::RunEvent) -> bool {
    matches!(event.kind, RunEventKind::Run(RunLifecycle::Parked { .. }))
}

fn is_resumed(event: &argonauts_core::events::RunEvent) -> bool {
    matches!(event.kind, RunEventKind::Run(RunLifecycle::Resumed(_)))
}

#[test]
fn app_example_small_fix_solo() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_card_config())?;
    scenario.repo.write("notes.txt", "scratch notes\n")?;
    scenario.repo.write("todo.txt", "later\n")?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::SMALL_FIX)?;

    let run_dir = wait_run_dir(&scenario)?;
    let merge_card = wait_checkpoint(&scenario, CheckpointKind::Merge)?;
    let keys: Vec<char> = merge_card.options.iter().map(|o| o.key).collect();
    assert_eq!(keys[..3], ['y', 'p', 'b'], "{keys:?}");
    assert!(keys.contains(&'x') && keys.last() == Some(&'d'), "{keys:?}");

    // One gate passing reads `Passed`; several read `All 3 passed`.
    let screen = pty.wait_for(r"Gates\(tests\)\n.*Passed", WAIT)?;
    let text = screen.text();
    let order = [
        "Triage(solo)",
        "2 uncommitted files left out",
        "owner · card-001",
        "Gates(",
        "Passed",
        "Merge",
        "Open a pull request",
        "Request changes",
        "Abandon the run",
        "Show the diff",
    ];
    let found: Vec<usize> = order
        .iter()
        .map(|needle| text.find(needle).unwrap_or(usize::MAX))
        .collect();
    assert!(
        found
            .windows(2)
            .all(|pair| pair[0] < pair[1] && pair[1] != usize::MAX),
        "expected {order:?} in order:\n{text}"
    );

    merge(&mut pty, &run_dir)?;
    pty.wait_for(r"◆ Merged the run into main · you", WAIT)?;
    pty.wait_for(r"✻ \w+ for \d", WAIT)?;
    check_mode_line(&pty)?;
    let readme = scenario.repo.git(&["show", "main:README.md"])?;
    assert!(readme.contains("npm install @acme/api"), "{readme}");
    scenario.shot(&pty, "app_example_small_fix_solo")?;
    Ok(())
}

#[test]
fn app_example_bigger_task_crew() -> anyhow::Result<()> {
    // The example runs the preset's own workflow, without the lead designing one.
    let scenario = scenario_with(&codex_config(&format!(
        "{GLOBAL_CONFIG}\n[workflows]\nlead_designs = false\nadapt_at_phase_ends = false\n"
    )))?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::CREW_TASK)?;

    let run_dir = wait_run_dir(&scenario)?;
    let goal = wait_checkpoint(&scenario, CheckpointKind::Goal)?;
    let recommended = goal.options.first().expect("the goal card has no answers");
    assert!(
        recommended.key == 'a' && recommended.recommended,
        "{goal:?}"
    );
    let text = pty.wait_for(r"What should the goal say\?", WAIT)?.text();
    // The goal's `● Goal(…)` line waits for its approval; before the question, the screen shows
    // what the restatements agree on and what they assumed.
    let order = [
        "Triage(crew)",
        "What the restatements agree on",
        "Assumed",
        "What should the goal say?",
    ];
    let found: Vec<Option<usize>> = order.iter().map(|needle| text.find(needle)).collect();
    assert!(
        found.iter().all(Option::is_some) && found.windows(2).all(|pair| pair[0] < pair[1]),
        "expected {order:?} in order:\n{text}"
    );

    // No agent works while the card waits, so the run parks and its supervisor exits.
    wait_journal(&run_dir, "the run to park", is_parked)?;
    wait_pid_gone(supervisor_pid(&run_dir)?)?;

    // The recommended answer comes first.
    answer_card(&mut pty, '1')?;
    pty.wait_for("Goal question answered · Per API key", WAIT)?;
    pty.wait_for(
        r"● Goal\(.*\n.*Agreed by 3 agents · 1 question answered",
        WAIT,
    )?;
    let drafters = wait_until("three drafter blocks", || {
        let text = pty.screen().text();
        Ok((text.matches("· drafter").count() >= 3).then_some(text))
    })?;
    for agent in ["Claude Code", "Codex", "Antigravity CLI"] {
        assert!(drafters.contains(agent), "no {agent} drafter:\n{drafters}");
    }
    scenario.shot(&pty, "app_example_bigger_task_crew")?;
    Ok(())
}

#[test]
fn app_stays_open_after_merge() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_card_config())?;
    let mut pty = start(&scenario)?;
    let (run_dir, _) = reach_merge_card(&scenario, &mut pty)?;
    merge(&mut pty, &run_dir)?;

    wait_mode(&scenario, "the prompt back", |m| matches!(m, AppMode::Idle))?;
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;
    // The turn-end verb is picked from the voyage set, such as `Sailed for 3s`.
    pty.wait_for(r"(?s)❯ hi\n.*Hi\. Ask me anything.*✻ \w+ for \d", WAIT)?;
    check_mode_line(&pty)?;
    scenario.shot(&pty, "app_stays_open_after_merge")?;
    Ok(())
}

#[test]
fn app_close_screen_run_continues_and_reattaches() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_solo(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    let owner = wait_owner_started(&scenario)?;
    let supervisor = supervisor_pid(&run_dir)?;

    pty.press(Key::Ctrl('c'))?;
    pty.wait_for("Press Ctrl-C again to exit · the run keeps going", WAIT)?;
    pty.press(Key::Ctrl('c'))?;
    pty.wait_for_exit(WAIT)?;
    let text = pty.screen().text();
    assert!(text.contains(&format!("{run} keeps going")), "{text}");
    assert!(text.contains("Reattach with: argonauts"), "{text}");
    assert!(
        pid_alive(supervisor),
        "closing the screen stopped the supervisor"
    );
    assert!(pid_alive(owner), "closing the screen stopped the owner");

    rotate_trace(&scenario, "first")?;
    let pty = start(&scenario)?;
    wait_event(
        &scenario,
        "the app to reattach",
        |event| matches!(event, AppEvent::Attached { summary, .. } if summary.run == run),
    )?;
    wait_event(
        &scenario,
        "the replay to end",
        |event| matches!(event, AppEvent::ReplayDone { run: done, .. } if *done == run),
    )?;
    pty.wait_for("owner · card-001", WAIT)?;
    assert!(pid_alive(owner), "reattaching restarted the owner");
    assert_eq!(
        claude_sessions(&scenario, "acceptEdits")?.len(),
        1,
        "reattaching started another owner"
    );
    scenario.shot(&pty, "app_close_screen_run_continues_and_reattaches")?;
    Ok(())
}

#[test]
fn app_resume_parked_run() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_card_config())?;
    let mut pty = start(&scenario)?;
    let (run_dir, _) = reach_merge_card(&scenario, &mut pty)?;
    let run = run_id(&run_dir)?;
    wait_journal(&run_dir, "the run to park", is_parked)?;
    wait_pid_gone(supervisor_pid(&run_dir)?)?;
    quit(&mut pty)?;

    rotate_trace(&scenario, "first")?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, &format!("/resume {run}"))?;
    wait_event(
        &scenario,
        "the app to attach",
        |event| matches!(event, AppEvent::Attached { summary, .. } if summary.run == run),
    )?;
    wait_checkpoint(&scenario, CheckpointKind::Merge)?;
    pty.wait_for("◆ Merge this run into main?", WAIT)?;
    merge(&mut pty, &run_dir)?;
    scenario.shot(&pty, "app_resume_parked_run")?;
    Ok(())
}

#[test]
fn app_continue_merged_run() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_card_config())?;
    let mut pty = start(&scenario)?;
    let (first_dir, _) = reach_merge_card(&scenario, &mut pty)?;
    let first = run_id(&first_dir)?;
    merge(&mut pty, &first_dir)?;

    submit(
        &mut pty,
        &format!("/follow-up {first} also mention yarn add @acme/api"),
    )?;
    let second_dir = wait_until("a follow-up run", || {
        let dirs = run_dirs(&scenario);
        Ok((dirs.len() >= 2).then(|| dirs[dirs.len() - 1].clone()))
    })?;
    let started = run_started(&second_dir)?;
    assert_eq!(started.continues.as_ref(), Some(&first), "{started:?}");
    assert!(
        started.request.contains("yarn add @acme/api"),
        "{started:?}"
    );
    scenario.shot(&pty, "app_continue_merged_run")?;
    Ok(())
}

#[test]
fn app_resume_stopped_run() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_card_config())?;
    let mut pty = start(&scenario)?;
    let run_dir = start_solo(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    let owner = wait_owner_started(&scenario)?;

    submit(&mut pty, "/stop")?;
    wait_journal(
        &run_dir,
        "the run to stop",
        is_finished(RunOutcome::Stopped),
    )?;
    wait_pid_gone(owner)?;

    // A stopped run can be resumed: the owner's vendor session picks up where it was.
    submit(&mut pty, &format!("/resume {run}"))?;
    wait_journal(&run_dir, "the run to resume", is_resumed)?;
    wait_until("the owner's session to resume", || {
        Ok(claude_sessions(&scenario, "acceptEdits")?
            .into_iter()
            .find(|i| has_flag(&i.args, "--resume", "sess-owner-slow")))
    })?;

    // A merged one refuses.
    wait_checkpoint(&scenario, CheckpointKind::Merge)?;
    merge(&mut pty, &run_dir)?;
    let resumes_before = run_events(&run_dir)?.into_iter().filter(is_resumed).count();
    let notices_before = notices(&scenario)?.len();
    submit(&mut pty, &format!("/resume {run}"))?;
    let refusal = wait_until("the refusal", || {
        Ok(notices(&scenario)?.into_iter().nth(notices_before))
    })?;
    assert!(refusal.contains("merged"), "{refusal}");
    thread::sleep(Duration::from_millis(500));
    let resumes_after = run_events(&run_dir)?.into_iter().filter(is_resumed).count();
    assert_eq!(resumes_after, resumes_before, "a merged run resumed");
    scenario.shot(&pty, "app_resume_stopped_run")?;
    Ok(())
}

#[test]
fn app_resume_cli_opens_picker() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let parked = fabricate_parked_run(&scenario, 1)?;
    let journal_before = run_events(&parked.dir)?.len();

    let mut pty = scenario.launch(&["resume"])?;
    // The picker can take the screen before the mode line draws, which shows again after Esc.
    wait_event(&scenario, "the app's welcome", |event| {
        matches!(event, AppEvent::Welcome { .. })
    })?;
    let listed = wait_event(
        &scenario,
        "the runs picker",
        |event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::Runs { .. })),
    )?;
    if let AppEvent::View { data } = listed
        && let ViewData::Runs { runs, .. } = *data
    {
        assert!(
            runs.iter().any(|summary| summary.run == parked.run),
            "{runs:?}"
        );
    }
    pty.wait_for(parked.run.as_str(), WAIT)?;
    pty.press(Key::Esc)?;
    check_mode_line(&pty)?;
    assert_eq!(
        run_events(&parked.dir)?.len(),
        journal_before,
        "opening the picker changed the run"
    );
    scenario.shot(&pty, "app_resume_cli_opens_picker")?;
    Ok(())
}

#[test]
fn app_project_main_session_across_runs() -> anyhow::Result<()> {
    let scenario = scenario_with(&format!(
        "{}\n[roles.owner]\nlifetime = \"project\"\n",
        merge_card_config()
    ))?;
    let mut pty = start(&scenario)?;
    let (first_dir, _) = reach_merge_card(&scenario, &mut pty)?;
    merge(&mut pty, &first_dir)?;

    start_solo(&scenario, &mut pty, requests::TIMED_TASK)?;
    let owners = wait_until("the second run's owner", || {
        let owners = claude_sessions(&scenario, "acceptEdits")?;
        Ok((owners.len() >= 2).then_some(owners))
    })?;
    assert!(
        !owners[0].args.iter().any(|a| a == "--resume"),
        "the first owner resumed something"
    );
    assert!(
        has_flag(&owners[1].args, "--resume", "sess-owner-readme"),
        "the second run didn't resume the project's owner session: {:?}",
        owners[1].args
    );
    scenario.shot(&pty, "app_project_main_session_across_runs")?;
    Ok(())
}

#[test]
fn app_pause_at_safe_point_and_resume() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_card_config())?;
    let mut pty = start(&scenario)?;
    let run_dir = start_solo(&scenario, &mut pty, requests::TIMED_TASK)?;
    let owner = wait_owner_started(&scenario)?;

    pty.press(Key::Esc)?;
    pty.wait_for(r"Pausing… \(esc again to stop now\)", WAIT)?;
    wait_journal(&run_dir, "the pause request", |event| {
        matches!(
            event.kind,
            RunEventKind::Run(RunLifecycle::PauseRequested { now: false, .. })
        )
    })?;
    let paused = wait_journal(&run_dir, "the run to pause", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::Paused { .. }))
    })?;
    // A safe point stops each session after its current tool call. The owner had none open,
    // so it stopped at once, and its work was committed.
    if let RunEventKind::Run(RunLifecycle::Paused {
        sessions_stopped,
        committed,
    }) = paused.kind
    {
        assert_eq!(sessions_stopped, 1, "the pause didn't stop the owner");
        assert!(
            committed.iter().any(|card| card.as_str() == "card-001"),
            "{committed:?}"
        );
    }
    wait_journal(&run_dir, "the run to park", |event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::Parked {
                reason: ParkReason::Paused,
                ..
            })
        )
    })?;
    // Paused after its tool call, which a stop-now would have cut short.
    let owner_end = run_events(&run_dir)?
        .into_iter()
        .find_map(|event| match event.kind {
            RunEventKind::Session(SessionEvent::Ended(end)) => Some(end.reason),
            _ => None,
        })
        .context("the owner's session never ended")?;
    assert_eq!(owner_end, EndReason::Paused, "the owner was cut off");
    wait_pid_gone(owner)?;
    wait_mode(&scenario, "the parked mode", |m| {
        matches!(m, AppMode::Parked { .. })
    })?;

    submit(&mut pty, "/resume")?;
    wait_journal(&run_dir, "the run to resume", is_resumed)?;
    wait_checkpoint(&scenario, CheckpointKind::Merge)?;
    scenario.shot(&pty, "app_pause_at_safe_point_and_resume")?;
    Ok(())
}

#[test]
fn app_second_esc_stops_now_and_commits() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_solo(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    let owner = wait_owner_started(&scenario)?;
    // The owner writes its file before it says this.
    pty.wait_for("Renaming the config loader", WAIT)?;

    pty.press(Key::Esc)?;
    pty.wait_for(r"Pausing… \(esc again to stop now\)", WAIT)?;
    pty.press(Key::Esc)?;
    wait_journal(&run_dir, "stop now", |event| {
        matches!(
            event.kind,
            RunEventKind::Run(RunLifecycle::PauseRequested { now: true, .. })
        )
    })?;
    let paused = wait_journal(&run_dir, "the run to pause", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::Paused { .. }))
    })?;
    if let RunEventKind::Run(RunLifecycle::Paused { committed, .. }) = paused.kind {
        assert!(
            committed.iter().any(|card| card.as_str() == "card-001"),
            "{committed:?}"
        );
    }
    wait_pid_gone(owner)?;
    let committed_work = run_branches(&scenario, &run)?.into_iter().any(|branch| {
        branch_files(&scenario, &branch)
            .is_ok_and(|files| files.contains(&"loader-notes.md".to_owned()))
    });
    assert!(committed_work, "the owner's work wasn't committed");
    scenario.shot(&pty, "app_second_esc_stops_now_and_commits")?;
    Ok(())
}

#[test]
fn app_guidance_while_parked_delivered_on_resume() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_solo(&scenario, &mut pty, requests::SLOW_TASK)?;
    wait_owner_started(&scenario)?;
    pty.wait_for("Renaming the config loader", WAIT)?;
    pty.press(Key::Esc)?;
    pty.wait_for(r"Pausing… \(esc again to stop now\)", WAIT)?;
    pty.press(Key::Esc)?;
    wait_journal(&run_dir, "the run to park", is_parked)?;
    wait_mode(&scenario, "the parked mode", |m| {
        matches!(m, AppMode::Parked { .. })
    })?;

    submit(&mut pty, requests::GUIDANCE)?;
    wait_journal(&run_dir, "the guidance in the journal", |event| {
        serde_json::to_string(event).is_ok_and(|line| line.contains(requests::GUIDANCE))
    })?;
    if !run_events(&run_dir)?.iter().any(is_resumed) {
        submit(&mut pty, "/resume")?;
    }
    let resumed = wait_until("the resumed owner", || {
        Ok(claude_sessions(&scenario, "acceptEdits")?
            .into_iter()
            .find(|i| i.args.iter().any(|a| a == "--resume")))
    })?;
    assert!(
        resumed
            .stdin
            .as_deref()
            .is_some_and(|prompt| prompt.contains(requests::GUIDANCE)),
        "the resumed owner's prompt lacks the guidance: {:?}",
        resumed.stdin
    );
    scenario.shot(&pty, "app_guidance_while_parked_delivered_on_resume")?;
    Ok(())
}

#[test]
fn app_offers_detected_gates_once() -> anyhow::Result<()> {
    let package = "{\"scripts\": {\"test\": \"node test.js\", \"lint\": \"eslint .\"}}\n";

    // Skip means never again in this repo.
    let skipping = scenario()?;
    skipping.repo.write("package.json", package)?;
    skipping.repo.commit_all("add package.json")?;
    let mut pty = start(&skipping)?;
    start_solo(&skipping, &mut pty, requests::SMALL_FIX)?;
    let offer = wait_open_choice(&skipping, argonauts_core::app::ChoiceKind::DetectedGates)?;
    let body = offer.body.join("\n");
    assert!(body.contains("test") && body.contains("lint"), "{body}");
    let keys: Vec<char> = offer.options.iter().map(|o| o.key).collect();
    assert_eq!(keys, ['y', 'e', 'n'], "{offer:?}");
    answer_card(&mut pty, number_of(&offer.options, 'n'))?;
    wait_choice_closed(&skipping, &offer.id)?;
    start_solo(&skipping, &mut pty, requests::TIMED_TASK)?;
    wait_owner_started(&skipping)?;
    thread::sleep(Duration::from_millis(500));
    let offers = events(&skipping)?
        .into_iter()
        .filter(|event| {
            matches!(event, AppEvent::Choice { choice }
                if choice.kind == argonauts_core::app::ChoiceKind::DetectedGates)
        })
        .count();
    assert_eq!(offers, 1, "the gates were offered again after Skip");
    skipping.shot(&pty, "app_offers_detected_gates_once_skip")?;
    quit(&mut pty)?;

    // Use these writes them to the project file.
    let using = scenario()?;
    using.repo.write("package.json", package)?;
    using.repo.commit_all("add package.json")?;
    let mut pty = start(&using)?;
    start_solo(&using, &mut pty, requests::SMALL_FIX)?;
    let offer = wait_open_choice(&using, argonauts_core::app::ChoiceKind::DetectedGates)?;
    answer_card(&mut pty, number_of(&offer.options, 'y'))?;
    wait_choice_closed(&using, &offer.id)?;
    let written = wait_until("the gates in the project file", || {
        Ok(read(&project_config(&using)).ok().filter(|text| {
            toml_entries(text)
                .iter()
                .any(|e| e.key.starts_with("gates.run"))
        }))
    })?;
    assert!(written.contains("npm"), "{written}");
    using.shot(&pty, "app_offers_detected_gates_once_use")?;
    Ok(())
}

#[test]
fn app_every_slash_command_handled() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    for spec in SLASH_COMMANDS {
        match spec.name {
            "exit" => continue,
            "vim" => {
                toggle_vim_on_and_off(&scenario, &mut pty)?;
                continue;
            }
            _ => {}
        }
        // Bare, so no command reaches a model: one that needs an argument must say so.
        let line = format!("/{}", spec.name);
        let before = scenario.trace.lines()?.len();
        let screen_before = pty.screen().text();
        submit(&mut pty, &line)?;
        let needs_argument = spec.needs_argument();
        // The screen answers some itself, such as `/theme`, which asks for its rows.
        if spec.handler != Handler::App || needs_argument {
            // Handled here means something answered: the app or the screen.
            wait_until(&format!("an answer to {line}"), || {
                let answered =
                    scenario.trace.lines()?.len() > before || pty.screen().text() != screen_before;
                Ok(answered.then_some(()))
            })?;
        } else {
            wait_until(&format!("the app's answer to {line}"), || {
                let lines = scenario.trace.lines()?;
                let after = lines.get(before..).unwrap_or_default();
                let got_it = after.iter().any(|l| {
                    matches!(
                        l,
                        TraceLine::In {
                            command: UiCommand::Slash { .. },
                            ..
                        }
                    )
                });
                let answered = after.iter().any(|l| matches!(l, TraceLine::Out { .. }));
                Ok((got_it && answered).then_some(()))
            })?;
        }
        thread::sleep(Duration::from_millis(200));
        pty.press(Key::Esc)?;
        thread::sleep(Duration::from_millis(200));
        // The menu completes a command that needs an argument rather than running it, so
        // its text is still in the prompt.
        if needs_argument {
            pty.press(Key::Ctrl('c'))?;
            thread::sleep(Duration::from_millis(200));
        }
        check_mode_line(&pty).map_err(|err| err.context(format!("after {line}")))?;
    }

    // `/setup` checks each CLI's login, which calls no model.
    let sessions: Vec<_> = all_sessions(&scenario)?
        .into_iter()
        .filter(|invocation| invocation.args != ["auth", "status"])
        .collect();
    assert!(
        sessions.is_empty(),
        "a bare command started a session: {sessions:?}"
    );
    let unhandled: Vec<String> = notices(&scenario)?
        .into_iter()
        .filter(|text| {
            let text = text.to_lowercase();
            text.contains("unknown command") || text.contains("not implemented")
        })
        .collect();
    assert!(unhandled.is_empty(), "{unhandled:?}");
    scenario.shot(&pty, "app_every_slash_command_handled")?;

    submit(&mut pty, "/exit")?;
    pty.wait_for_exit(WAIT)?;
    Ok(())
}

/// Everything a fake session was told: its arguments, then what it read on stdin.
fn prompt_of(invocation: &Invocation) -> String {
    format!(
        "{}\n{}",
        invocation.args.join("\n"),
        invocation.stdin.as_deref().unwrap_or_default()
    )
}

/// Sends `/rewind card-001` and returns the notice that answers it.
/// `/rewind card-001` opens its picker; this returns the picker's newest rows, still open.
fn open_rewind(scenario: &Scenario, pty: &mut Pty) -> anyhow::Result<Vec<CheckpointRow>> {
    let is_picker = |event: &AppEvent| matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Checkpoints { .. }));
    let before = events(scenario)?
        .into_iter()
        .filter(|e| is_picker(e))
        .count();
    submit(pty, "/rewind card-001")?;
    let shown = wait_events(scenario, "the /rewind picker", before + 1, is_picker)?;
    match shown.last() {
        Some(AppEvent::View { data }) => match data.as_ref() {
            ViewData::Checkpoints { checkpoints, .. } => Ok(checkpoints.clone()),
            other => bail!("not the /rewind picker: {other:?}"),
        },
        _ => bail!("no /rewind picker"),
    }
}

fn list_checkpoints(scenario: &Scenario, pty: &mut Pty) -> anyhow::Result<Vec<CheckpointRow>> {
    let rows = open_rewind(scenario, pty)?;
    pty.wait_for("Rewind", WAIT)?;
    pty.press(Key::Esc)?;
    Ok(rows)
}

/// Picks handoff-1, the only checkpoint, so the newest is the one picked.
fn ask_revert(scenario: &Scenario, pty: &mut Pty) -> anyhow::Result<AppChoice> {
    open_rewind(scenario, pty)?;
    pty.wait_for("handoff-1", WAIT)?;
    pty.press(Key::Enter)?;
    wait_open_choice(scenario, ChoiceKind::ConfirmRevert)
}

/// `/rewind` at the merge card: the card's branch goes back to `handoff-1`, and the run's
/// branch drops the work after it, which stays on the replaced branch.
#[test]
fn app_checkpoints_and_revert_commands() -> anyhow::Result<()> {
    let scenario = scenario_with(&codex_config(&format!(
        "{}\n[handoff]\nmode = \"auto\"\nfallback = [\"codex\"]\n",
        merge_card_config()
    )))?;
    let mut pty = start(&scenario)?;
    let run_dir = start_solo(&scenario, &mut pty, LIMITED_TASK)?;
    let run = run_id(&run_dir)?;
    let tagged = wait_journal(&run_dir, "the handoff checkpoint", |event| {
        matches!(
            event.kind,
            RunEventKind::Card(CardEvent::CheckpointTagged { n: 1, .. })
        )
    })?;
    let RunEventKind::Card(CardEvent::CheckpointTagged { commit, .. }) = tagged.kind else {
        unreachable!("matched a tag")
    };
    // Codex's commit is in once the merge card opens.
    let merge_card = wait_checkpoint(&scenario, CheckpointKind::Merge)?;
    let CheckpointPacket::Merge(merge_packet) = merge_card.packet else {
        bail!("the merge card has no merge packet: {merge_card:?}");
    };
    let run_branch = merge_packet.branch;
    let card_branch = format!("argonauts/{run}/card-001");
    let tag = format!("argonauts/{run}/card-001/handoff-1");
    let kept_on = format!("argonauts/{run}/card-001-replaced-1");

    let listed = list_checkpoints(&scenario, &mut pty)?;
    let [row] = listed.as_slice() else {
        bail!("one checkpoint, not {listed:?}");
    };
    assert_eq!((row.card.as_str(), row.n), ("card-001", 1), "{row:?}");
    assert_eq!(row.from.worker, "claude-code", "{row:?}");
    assert!(row.to.iter().any(|to| to.worker == "codex"), "{row:?}");
    assert_eq!(row.commit, commit, "{row:?}");
    assert!(!row.reverted_here, "{row:?}");

    // Set aside, the confirm reverts nothing.
    let heads = || -> anyhow::Result<[String; 2]> {
        Ok([
            scenario.repo.git(&["rev-parse", &card_branch])?,
            scenario.repo.git(&["rev-parse", &run_branch])?,
        ])
    };
    let before = heads()?;
    let confirm = ask_revert(&scenario, &mut pty)?;
    let body = confirm.body.join("\n");
    assert!(body.contains(&tag) && body.contains(&kept_on), "{body}");
    pty.press(Key::Esc)?;
    assert_eq!(wait_choice_closed(&scenario, &confirm.id)?, "Not reverted.");
    assert_eq!(heads()?, before);

    let confirm = ask_revert(&scenario, &mut pty)?;
    answer_card(&mut pty, number_of(&confirm.options, 'y'))?;
    wait_choice_closed(&scenario, &confirm.id)?;
    wait_journal(&run_dir, "the revert", |event| {
        matches!(
            &event.kind,
            RunEventKind::Card(CardEvent::Reverted { to: 1, kept_on: kept })
                if *kept == kept_on
        )
    })?;
    assert_eq!(
        scenario.repo.git(&["rev-parse", &card_branch])?,
        scenario
            .repo
            .git(&["rev-parse", &format!("{tag}^{{commit}}")])?,
        "the card branch is back at handoff-1"
    );
    let card_files = branch_files(&scenario, &card_branch)?;
    assert!(
        card_files.contains(&"export-job.md".to_owned())
            && !card_files.contains(&"export-callers.md".to_owned()),
        "{card_files:?}"
    );
    let kept_files = branch_files(&scenario, &kept_on)?;
    assert!(
        kept_files.contains(&"export-callers.md".to_owned()),
        "the replaced work is lost: {kept_files:?}"
    );
    let run_files = branch_files(&scenario, &run_branch)?;
    assert!(
        run_files.contains(&"export-job.md".to_owned())
            && !run_files.contains(&"export-callers.md".to_owned()),
        "the run's branch still has the work after handoff-1: {run_files:?}"
    );

    let listed = list_checkpoints(&scenario, &mut pty)?;
    assert!(listed.iter().all(|row| row.reverted_here), "{listed:?}");
    scenario.shot(&pty, "app_checkpoints_and_revert_commands")?;
    Ok(())
}

#[test]
fn app_lead_picks_review_panel_and_fills_params() -> anyhow::Result<()> {
    let scenario = scenario_with(&codex_config(GLOBAL_CONFIG))?;
    // The review panel is a project's own workflow, offered to the lead beside the built-ins.
    scenario
        .repo
        .write(".argonauts/workflows/review-panel.yaml", REVIEW_PANEL)?;
    scenario.repo.commit_all("add the review panel workflow")?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, &format!("/crew {REVIEW_TASK}"))?;
    let run_dir = wait_run_dir(&scenario)?;
    // The restatements disagree on scope, so the goal card asks first.
    let goal = wait_checkpoint(&scenario, CheckpointKind::Goal)?;
    answer_card(&mut pty, number_of(&goal.options, 'a'))?;

    let design = wait_until("the lead's design session", || {
        Ok(sessions_of(&scenario, "claude")?
            .into_iter()
            .find(|invocation| prompt_of(invocation).contains("\"composed\"")))
    })?;
    assert!(
        design.args.iter().any(|arg| arg == "dontAsk")
            && !design.args.iter().any(|arg| arg == "acceptEdits"),
        "the design session could edit: {:?}",
        design.args
    );
    let prompt = prompt_of(&design);
    assert!(prompt.contains("### review-panel"), "{prompt}");
    assert!(
        prompt.contains("\"why\"") && prompt.contains("\"params\""),
        "the prompt lacks the schema: {prompt}"
    );

    let started = wait_journal(&run_dir, "the run's workflow", |event| {
        matches!(
            &event.kind,
            RunEventKind::Workflow(WorkflowEvent::Started(started)) if started.parent.is_none()
        )
    })?;
    let RunEventKind::Workflow(WorkflowEvent::Started(started)) = started.kind else {
        unreachable!("matched a workflow start")
    };
    assert_eq!(
        started.from.as_ref().map(|from| from.as_str()),
        Some("review-panel")
    );
    let params = &started.workflow.params.0;
    assert_eq!(
        (params.get("target"), params.get("reviewers")),
        (Some(&json!("diff")), Some(&json!(2))),
        "{params:?}"
    );
    let review = started
        .workflow
        .phases
        .iter()
        .find(|phase| phase.name == "Review")
        .context("the workflow has no Review phase")?;
    let fan_out = review
        .steps
        .iter()
        .find(|step| step.kind == StepKind::FanOut)
        .context("the Review phase has no fan_out")?;
    assert_eq!(fan_out.count, Some(Count::Fixed(2)), "{fan_out:?}");

    wait_checkpoint(&scenario, CheckpointKind::Plan)?;
    // The lead's own message says why before the plan's item does, so this waits for the
    // item's workflow in plain words, which ends with its last phase, and reads the item alone.
    // The steps' conditions are for Ctrl+O.
    let text = pty.wait_for(r"adjudicate \(if needed\)", WAIT)?.text();
    let plan = &text[text.rfind("The plan").context("no plan item")?..];
    for shown in [
        "A review: two reviewers from other families",
        "review-panel",
        "review → adjudicate (if needed)",
    ] {
        assert!(plan.contains(shown), "the plan card lacks {shown}:\n{text}");
    }
    assert!(!plan.contains("reviewers_disagree"), "{text}");
    scenario.shot(&pty, "app_lead_picks_review_panel_and_fills_params")?;
    Ok(())
}

/// The replacement rule a remembered handoff pick saves.
const REMEMBERED_RULE: &str = "roles.owner.handoff.replace.claude-code.with";

/// A handoff that asks, with Codex to pick.
fn handoff_card_config() -> String {
    codex_config(&format!(
        "{}\n[handoff]\nmode = \"ask\"\nfallback = [\"codex\"]\n",
        merge_card_config()
    ))
}

/// The handoff card's Codex candidate: its key and worker.
/// The Codex candidate's number on the card, and the worker.
fn codex_candidate(card: &CheckpointOpened) -> anyhow::Result<(char, WorkerRef)> {
    card.options
        .iter()
        .find_map(|option| match &option.action {
            AnswerAction::Candidate { worker } if worker.worker == "codex" => {
                Some((number_of(&card.options, option.key), worker.clone()))
            }
            _ => None,
        })
        .with_context(|| format!("no Codex candidate on {card:?}"))
}

/// Remember this pick, the toggle after the handoff card's own options.
fn remember_toggle(card: &CheckpointOpened) -> char {
    char::from_digit(card.options.len() as u32 + 1, 10).expect("at most 9 options")
}

/// Asks to remember an answered handoff card's pick: `m` on the answered card's record line.
fn remember_answered_pick(pty: &mut Pty) -> anyhow::Result<()> {
    pty.press(Key::Up)?;
    pty.press(Key::Char('m'))
}

#[test]
fn app_remember_pick_saves_replacement_rule() -> anyhow::Result<()> {
    let key = REMEMBERED_RULE;

    // Remembered as you pick, and saved for the project.
    let scenario = scenario_with(&handoff_card_config())?;
    scenario
        .repo
        .write(".argonauts/config.toml", &format!("{KEPT_COMMENT}\n"))?;
    scenario.repo.commit_all("add the project settings")?;
    let mut pty = start(&scenario)?;
    start_solo(&scenario, &mut pty, LIMITED_TASK)?;
    let card = wait_checkpoint(&scenario, CheckpointKind::Handoff)?;
    let (pick_key, pick) = codex_candidate(&card)?;
    let rule = format!("[\"{pick}\"]");
    answer_card(&mut pty, remember_toggle(&card))?;
    pty.press(Key::Char(pick_key))?;
    answer_scope(&scenario, &mut pty, 'p', None)?;
    let changed = wait_setting_changed(&scenario, key, 1)?;
    assert!(same_toml(&changed, &rule), "{changed}");
    let project = read(&project_config(&scenario))?;
    assert!(
        same_toml(&toml_value(&project, key).unwrap_or_default(), &rule),
        "{project}"
    );
    assert!(project.contains(KEPT_COMMENT), "{project}");
    assert!(
        commands(&scenario)?
            .iter()
            .any(|command| matches!(command, UiCommand::Answer { remember: true, .. })),
        "the pick wasn't sent with remember"
    );
    wait_until("Codex to take the card", || {
        Ok(sessions_of(&scenario, "codex")?
            .into_iter()
            .find(|invocation| prompt_of(invocation).contains(LIMITED_TASK)))
    })?;
    let next_dir = start_solo(&scenario, &mut pty, requests::TIMED_TASK)?;
    let settings = wait_until("the next run's settings", || {
        Ok(run_settings(&next_dir).ok())
    })?;
    // `settings.json` keeps the resolved settings under `settings`, beside their hash.
    assert_eq!(
        json_at(&settings, &format!("settings.{key}")),
        Some(&json!([pick.to_string()])),
        "{settings}"
    );
    scenario.shot(&pty, "app_remember_pick_saves_replacement_rule_project")?;
    Ok(())
}

#[test]
fn app_remember_answered_pick_saves_replacement_rule() -> anyhow::Result<()> {
    let key = REMEMBERED_RULE;

    // Remembered after the answer, and saved for just you.
    let scenario = scenario_with(&handoff_card_config())?;
    let mut pty = start(&scenario)?;
    start_solo(&scenario, &mut pty, LIMITED_TASK)?;
    let card = wait_checkpoint(&scenario, CheckpointKind::Handoff)?;
    let (pick_key, pick) = codex_candidate(&card)?;
    answer_card(&mut pty, pick_key)?;
    remember_answered_pick(&mut pty)?;
    wait_until("the remembered pick", || {
        Ok(commands(&scenario)?.into_iter().find(
            |command| matches!(command, UiCommand::RememberPick { id, .. } if *id == card.id),
        ))
    })?;
    answer_scope(&scenario, &mut pty, 'm', None)?;
    wait_setting_changed(&scenario, key, 1)?;
    let local = read(&project_local_config(&scenario))?;
    assert!(
        same_toml(
            &toml_value(&local, key).unwrap_or_default(),
            &format!("[\"{pick}\"]")
        ),
        "{local}"
    );
    assert!(
        !project_config(&scenario).exists(),
        "just you went to the shared file"
    );
    scenario.shot(&pty, "app_remember_pick_saves_replacement_rule_local")?;
    Ok(())
}

#[test]
fn app_context_command_lists_sessions() -> anyhow::Result<()> {
    let scenario = scenario_with(&format!(
        "{GLOBAL_CONFIG}\n[sessions]\nrotate_at = \"50k\"\n"
    ))?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;
    let run_dir = start_solo(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    wait_owner_started(&scenario)?;
    // The owner reports its usage with this message.
    pty.wait_for("Renaming the config loader", WAIT)?;
    let sessions = all_sessions(&scenario)?.len();

    submit(&mut pty, "/context")?;
    let shown = wait_event(
        &scenario,
        "the context view",
        |event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::Context { .. })),
    )?;
    let AppEvent::View { data } = shown else {
        unreachable!("matched a view")
    };
    let ViewData::Context { view } = *data else {
        unreachable!("matched the context view")
    };
    let rows = view.rows;
    let chat = rows.first().context("the context view has no rows")?;
    assert!(chat.run.is_none() && chat.label == "chat", "{rows:#?}");
    let owners: Vec<_> = rows
        .iter()
        .filter(|row| row.run.as_ref() == Some(&run))
        .collect();
    let [owner] = owners.as_slice() else {
        bail!("expected one owner row: {rows:#?}");
    };
    assert_eq!(owner.cap, Some(50_000), "{owner:?}");
    assert_eq!(
        (owner.spent.input_tokens, owner.spent.output_tokens),
        (1_200, 300),
        "{owner:?}"
    );
    assert!(owner.used >= 1_200, "{owner:?}");

    let text = pty.wait_for("owner · card-001", WAIT)?.text();
    assert!(text.contains("chat"), "{text}");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        all_sessions(&scenario)?.len(),
        sessions,
        "/context started a session"
    );
    scenario.shot(&pty, "app_context_command_lists_sessions")?;
    Ok(())
}

/// A second git repo beside the scenario's, under the same Argonauts home.
fn other_repo(scenario: &Scenario) -> anyhow::Result<PathBuf> {
    let root = scenario
        .repo
        .root()
        .parent()
        .context("the repo has no parent")?
        .join("other-repo");
    fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let at = root.to_string_lossy().into_owned();
    scenario.repo.git(&["-C", &at, "init", "--quiet"])?;
    fs::write(root.join("README.md"), "# Other repo\n").context("writing its README")?;
    scenario.repo.git(&["-C", &at, "add", "-A"])?;
    scenario
        .repo
        .git(&["-C", &at, "commit", "--quiet", "-m", "initial commit"])?;
    Ok(root)
}

/// A run in the repo at `root` that waits on an agent's question.
fn waiting_run_in(scenario: &Scenario, root: &Path) -> anyhow::Result<ParkedRun> {
    let parked = fabricate_parked_run(scenario, 0)?;
    let head = scenario
        .repo
        .git(&["-C", &root.to_string_lossy(), "rev-parse", "HEAD"])?;
    let repo = RepoId::for_repo(root).context("naming the other repo")?;
    let dir = Paths::new(scenario.repo.home()).run_dir(&repo, &parked.run);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut journal = String::new();
    for mut event in run_events(&parked.dir)? {
        if let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind {
            started.repo_root = root.to_owned();
            started.base_commit = head.clone();
        }
        journal.push_str(&serde_json::to_string(&event)?);
        journal.push('\n');
    }
    fs::write(dir.join(run_files::EVENTS), journal).context("writing the moved journal")?;
    fs::remove_dir_all(&parked.dir).context("removing the fabricated run")?;
    Ok(ParkedRun {
        run: parked.run,
        dir,
        checkpoint: parked.checkpoint,
    })
}

/// A sleeping child registered as the live supervisor of `run`, killed when it drops.
struct StandInSupervisor(Child);

impl StandInSupervisor {
    fn register(scenario: &Scenario, repo_root: &Path, run: &RunId) -> anyhow::Result<Self> {
        let now = || -> anyhow::Result<u64> {
            Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
        };
        let before = now()?;
        let child = Command::new("sleep")
            .arg("600")
            .spawn()
            .context("starting a stand-in supervisor")?;
        let after = now()?;
        let stand_in = Self(child);
        let path = scenario.repo.home().join(user_files::REGISTRY);
        let mut registry: Value = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).context("reading the registry")?,
            Err(_) => json!({ "supervisors": [] }),
        };
        let supervisors = registry["supervisors"]
            .as_array_mut()
            .context("the registry has no supervisors list")?;
        // Entries are keyed by the start time sysinfo reads, which can land a second either
        // side of this clock. Readers drop the entries whose time is wrong.
        for start_time in before.saturating_sub(1)..=after + 1 {
            supervisors.push(json!({
                "pid": stand_in.0.id(),
                "start_time": start_time,
                "repo_root": repo_root,
                "run": run,
                "sessions": 0,
            }));
        }
        fs::write(&path, serde_json::to_vec_pretty(&registry)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(stand_in)
    }
}

impl Drop for StandInSupervisor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Opens the crew panel with ← and returns the `n`th list the controller sent for it, which
/// holds the runs of every repo with a live supervisor.
fn open_crew_panel(scenario: &Scenario, pty: &mut Pty, n: usize) -> anyhow::Result<CrewPanel> {
    check_mode_line(pty)?;
    pty.press(Key::Left)?;
    let panels = wait_events(
        scenario,
        "the crew panel",
        n,
        |event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::Crew { .. })),
    )?;
    match panels.into_iter().nth(n - 1) {
        Some(AppEvent::View { data }) => match *data {
            ViewData::Crew { panel } => Ok(panel),
            _ => unreachable!("matched the crew panel"),
        },
        _ => unreachable!("waited for {n} panels"),
    }
}

#[test]
fn app_all_repos_shows_other_repo_waiting() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let other = other_repo(&scenario)?;
    let waiting = waiting_run_in(&scenario, &other)?;
    let supervisor = StandInSupervisor::register(&scenario, &other, &waiting.run)?;
    let mut pty = start(&scenario)?;

    let panel = open_crew_panel(&scenario, &mut pty, 1)?;
    let there = other.display().to_string();
    let theirs: Vec<_> = panel.rows.iter().filter(|row| row.repo == there).collect();
    let [row] = theirs.as_slice() else {
        bail!("expected the waiting run alone: {panel:#?}");
    };
    assert_eq!(
        row.target,
        CrewTarget::Run {
            run: waiting.run.clone()
        }
    );
    assert_eq!(
        row.state,
        CrewState::NeedsYou,
        "it waits on its card: {row:?}"
    );
    pty.wait_for("other-repo", WAIT)?;
    scenario.shot(&pty, "app_all_repos_shows_other_repo_waiting")?;

    // Only repos with a live supervisor are listed, so a run parked past the park grace, whose
    // supervisor exited, drops out of the panel.
    drop(supervisor);
    pty.press(Key::Esc)?;
    let panel = open_crew_panel(&scenario, &mut pty, 2)?;
    assert!(
        !panel.rows.iter().any(|row| row.repo == there),
        "{panel:#?}"
    );
    Ok(())
}
