//! Runs from the conversation: the small fix to its merge, closing and reattaching, pausing,
//! stopping, resuming and continuing, main sessions across runs, the checks offer, fallback
//! checkpoints and remembered picks, the main agent's workflow design, `/context`, runs in other
//! repos, and every command.

use crate::app_support;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use app_support::*;
use argonauts_core::app::agent_panel::{AgentPanelRows, CrewState, CrewTarget};
use argonauts_core::app::views::CheckpointRow;
use argonauts_core::app::{AnswerTarget, UiCommand, ViewData};
use argonauts_core::app::{AppChoice, AppEvent, AppMode, ChoiceKind};
use argonauts_core::base::layout::{run_files, user_files};
use argonauts_core::base::names::ModelRef;
use argonauts_core::input::{Handler, SLASH_COMMANDS};
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
};
use argonauts_core::runs::events::run::{RunLifecycle, RunMode, RunOutcome, WaitReason};
use argonauts_core::runs::events::session::{EndReason, SessionEvent};
use argonauts_core::runs::events::{RunEventKind, TaskEvent, WorkflowEvent};
use argonauts_core::workflows::workflow::{Count, StepKind};
use argonauts_core::{Paths, RepoId, RunId};
use argonauts_scenarios::{
    Scenario, WAIT, on_the_default_workflow, past_the_question_guard, scenario_file,
};
use argonauts_testkit::{FakeScenario, Invocation, Key, Pty};
use serde_json::{Value, json};

/// A Captain change whose Claude worker commits, then hits its usage limit, so Codex finishes it.
const LIMITED_TASK: &str = "move the export job onto the worker queue";
/// A review, for which the main agent picks the review panel.
const REVIEW_TASK: &str = "Review the retry change in src/retry.rs";
/// The workflow the main agent picks for a review.
const REVIEW_PANEL: &str = r#"name: review-panel
description: Two reviewers from other providers; a judge only if they disagree
params: { target: diff, reviewers: 2 }
phases:
  - name: Review
    standard: check
    steps:
      - kind: fan_out
        role: reviewer
        count: "{{reviewers}}"
        blind: true
        different_provider_from: author
  - name: Settle
    standard: decide
    steps:
      - kind: judge
        role: judge
        when: reviewers_disagree
"#;
/// The project file's first line, which every write must keep.
const KEPT_COMMENT: &str = "# Written by the scenario test, kept through every change.";

/// `run_config()` with merges waiting for you: by default they land on their own when every
/// check passes, and these scenarios answer the merge question.
fn merge_question_config() -> String {
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

/// The goal's questions come to you, for the scenarios that walk through its question.
/// In auto the main agent would answer them otherwise.
const YOU_ANSWER: &str = "\n[approvals]\nanswers = \"you\"\n";

fn is_finished(outcome: RunOutcome) -> impl Fn(&argonauts_core::runs::events::RunEvent) -> bool {
    move |event| matches!(&event.kind, RunEventKind::Run(RunLifecycle::Finished { outcome: o, .. }) if *o == outcome)
}

fn is_waiting(event: &argonauts_core::runs::events::RunEvent) -> bool {
    matches!(event.kind, RunEventKind::Run(RunLifecycle::Waiting { .. }))
}

fn is_resumed(event: &argonauts_core::runs::events::RunEvent) -> bool {
    matches!(event.kind, RunEventKind::Run(RunLifecycle::Resumed(_)))
}

#[test]
fn app_example_small_fix_captain() -> anyhow::Result<()> {
    let scenario = scenario_with(&on_the_default_workflow(&merge_question_config()))?;
    scenario.repo.write("notes.txt", "scratch notes\n")?;
    scenario.repo.write("todo.txt", "later\n")?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::SMALL_FIX)?;

    let run_dir = wait_run_dir(&scenario)?;
    let merge_question = wait_approval(&scenario, ApprovalKind::Merge)?;
    let keys: Vec<char> = merge_question.options.iter().map(|o| o.key).collect();
    assert_eq!(keys[..3], ['y', 'p', 'b'], "{keys:?}");
    assert!(keys.contains(&'x') && keys.last() == Some(&'d'), "{keys:?}");

    // One check passing reads its command and `passed`; several read `All 3 passed`. The question
    // draws a moment after its approval opens, so the screen is read once both are on it.
    pty.wait_for(r"Checks\(tests\)\n.*true · passed", WAIT)?;
    let screen = pty.wait_for(r"Show the diff", WAIT)?;
    let text = screen.text();
    let order = [
        "2 uncommitted files left out",
        "build:",
        "Checks(",
        "true · passed",
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
    pty.wait_for(r"◆ Approved merging the run into main · you", WAIT)?;
    pty.wait_for(r"✻ \w+ for \d", WAIT)?;
    check_bottom_row(&pty)?;
    let readme = scenario.repo.git(&["show", "main:README.md"])?;
    assert!(readme.contains("npm install @acme/api"), "{readme}");
    scenario.shot(&pty, "app_example_small_fix_captain")?;
    Ok(())
}

#[test]
fn app_example_bigger_task_crew() -> anyhow::Result<()> {
    // The example runs the workflow as it is, without the main agent designing one.
    let workflows =
        config_with_workflows("orchestrator_designs = false\nadapt_at_phase_ends = false\n");
    // The crew's fixed phases, whose drafting agents the fakes play.
    let config = codex_config(&format!("{workflows}\n{YOU_ANSWER}"));
    let scenario = scenario_with(&format!("workflow = \"{FIXED_PHASES}\"\n{config}"))?;
    write_fixed_phases(&scenario.repo.global_config())?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::CREW_TASK)?;

    let run_dir = wait_run_dir(&scenario)?;
    let goal = wait_approval(&scenario, ApprovalKind::Understand)?;
    let recommended = goal
        .options
        .first()
        .expect("the Understand approval has no answers");
    assert!(
        recommended.key == 'a' && recommended.recommended,
        "{goal:?}"
    );
    let text = pty.wait_for(r"What should the goal say\?", WAIT)?.text();
    // The goal's `● Goal(…)` line waits for its approval; before the question, the screen shows
    // what the readings agree on and what they assumed.
    let order = [
        "What the readings agree on",
        "Assumed",
        "What should the goal say?",
    ];
    let found: Vec<Option<usize>> = order.iter().map(|needle| text.find(needle)).collect();
    assert!(
        found.iter().all(Option::is_some) && found.windows(2).all(|pair| pair[0] < pair[1]),
        "expected {order:?} in order:\n{text}"
    );

    // No agent works while the question waits, so the run waits and its run host exits.
    wait_journal(&run_dir, "the run to wait", is_waiting)?;
    wait_pid_gone(run_host_pid(&run_dir)?)?;

    // The recommended answer comes first.
    answer_question(&mut pty, '1')?;
    pty.wait_for("Goal question answered · Per API key", WAIT)?;
    pty.wait_for(
        r"[●⏺] Goal\(.*\n.*Agreed by 3 agents · 1 question answered",
        WAIT,
    )?;
    let drafting_agents = wait_until("three drafting agent blocks", || {
        let text = pty.screen().text();
        let drafts = text.matches("● draft:").count() + text.matches("⏺ draft:").count();
        Ok((drafts >= 3).then_some(text))
    })?;
    for agent in ["Claude Code", "Codex", "agy"] {
        assert!(
            drafting_agents.contains(agent),
            "no {agent} drafting agent:\n{drafting_agents}"
        );
    }
    scenario.shot(&pty, "app_example_bigger_task_crew")?;
    Ok(())
}

/// Ultracode through the fake agents: the one-file fix that the rules give one agent goes to the
/// crew while it's on, as the input rule's label says, and Captain while it's off.
#[test]
fn app_ultracode_sends_a_small_task_to_the_crew() -> anyhow::Result<()> {
    for (on, mode) in [(true, RunMode::Crew), (false, RunMode::Captain)] {
        let config = if on {
            format!("ultracode = true\n{GLOBAL_CONFIG}\n")
        } else {
            GLOBAL_CONFIG.to_owned()
        };
        let scenario = scenario_with(&on_the_default_workflow(&config))?;
        let mut pty = start(&scenario)?;
        if on {
            pty.wait_for("  ultracode ─", WAIT)?;
        }
        submit(&mut pty, requests::SMALL_FIX)?;

        let run_dir = wait_run_dir(&scenario)?;
        let started = run_started(&run_dir)?;
        assert_eq!(started.mode, mode);
        assert!(!started.forced);
        let rule = started
            .captain_or_crew
            .map(|captain_or_crew| captain_or_crew.rule)
            .unwrap_or_default();
        assert_eq!(rule == "ultracode", on, "{rule}");
        if on {
            scenario.shot(&pty, "app_ultracode_sends_a_small_task_to_the_crew")?;
        }
    }
    Ok(())
}

#[test]
fn app_stays_open_after_merge() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_question_config())?;
    let mut pty = start(&scenario)?;
    let (run_dir, _) = reach_merge_question(&scenario, &mut pty)?;
    merge(&mut pty, &run_dir)?;

    wait_mode(&scenario, "the prompt back", |m| matches!(m, AppMode::Idle))?;
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;
    // The turn-end verb is picked from the voyage set, such as `Sailed for 3s`.
    pty.wait_for(r"(?s)❯ hi\n.*Hi\. Ask me anything.*✻ \w+ for \d", WAIT)?;
    check_bottom_row(&pty)?;
    scenario.shot(&pty, "app_stays_open_after_merge")?;
    Ok(())
}

#[test]
fn app_close_screen_run_continues_and_reattaches() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_captain(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    let worker = wait_worker_started(&scenario)?;
    let run_host = run_host_pid(&run_dir)?;

    // Both presses in one write: a busy machine can stall longer than the second the first
    // press gives the second. The screen's own tests check the hint between them.
    pty.send_at_once("\x03\x03")?;
    pty.wait_for_exit(WAIT)?;
    let text = pty.screen().text();
    assert!(text.contains(&format!("{run} keeps going")), "{text}");
    let reattach = format!("Reattach with: argonauts resume {run}");
    assert!(text.contains(&reattach), "{text}");
    assert!(
        pid_alive(run_host),
        "closing the screen stopped the run host"
    );
    assert!(pid_alive(worker), "closing the screen stopped the worker");

    rotate_trace(&scenario, "first")?;
    let pty = start_with(&scenario, &["resume", run.as_str()])?;
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
    // The run is drawn as it printed live: so far, the working line of its build.
    pty.wait_for(r"Build · \d+/\d+ tasks", WAIT)?;
    assert!(pid_alive(worker), "reattaching restarted the worker");
    assert_eq!(
        claude_sessions(&scenario, "acceptEdits")?.len(),
        1,
        "reattaching started another worker"
    );
    scenario.shot(&pty, "app_close_screen_run_continues_and_reattaches")?;
    Ok(())
}

/// The panel row of a run that waits on you, where the screen holds the run: its mark, ◆, two
/// blanks and its name, picked or not. A question's own heading has one blank after its ◆.
const NEEDS_YOU_ROW: &str = "(?m)^[ ❯] ◆  \\S";

/// Fails when a run waiting on you has a panel row, marked ◆, or counts in the bottom line,
/// `← 1 needs you`, or when the app attached to it or listed rows for the panel. The line under
/// the header says `1 run needs you`.
fn check_off_the_panel(scenario: &Scenario, pty: &Pty) -> anyhow::Result<()> {
    let text = pty.screen().visible_text();
    assert!(
        !regex::Regex::new(NEEDS_YOU_ROW)?.is_match(&text),
        "the run got a panel row:\n{text}"
    );
    assert!(
        !text.contains(" 1 needs you"),
        "the bottom line counted the run:\n{text}"
    );
    let events = events(scenario)?;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AppEvent::Attached { .. } | AppEvent::BackgroundRuns { .. }
        )),
        "{events:#?}"
    );
    Ok(())
}

/// `argonauts` opens a new conversation, as `claude` does, and its panel holds only the runs
/// started in it. A run waiting on a question in an earlier session gets one line
/// under the header, and no panel row or bottom line count; what you type goes to the new
/// conversation, and the question waits until you open the run. `argonauts -c` carries on a
/// conversation that never opened the run, so it stays off that panel too.
#[test]
fn app_start_leaves_a_waiting_question_for_you_to_open() -> anyhow::Result<()> {
    const QUESTION: &str = "Keep the old name as an alias?";
    const TITLE: &str = "Rename the config loader";
    let scenario = scenario()?;
    let waiting = fabricate_waiting_run(&scenario, 0)?;
    let journal_before = run_events(&waiting.dir)?.len();
    let mut pty = start(&scenario)?;
    pty.wait_for("1 run needs you · /resume", WAIT)?;
    check_bottom_row(&pty)?;
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;
    let text = pty.screen().visible_text();
    assert!(
        !text.contains(QUESTION) && !text.contains(TITLE),
        "the task or the run showed at start:\n{text}"
    );
    check_off_the_panel(&scenario, &pty)?;
    assert_eq!(
        run_events(&waiting.dir)?.len(),
        journal_before,
        "the waiting run changed while you chatted"
    );
    scenario.shot(&pty, "app_start_leaves_a_waiting_question_for_you_to_open")?;
    quit(&mut pty)?;

    rotate_trace(&scenario, "first")?;
    let mut pty = start_with(&scenario, &["-c"])?;
    wait_event(&scenario, "the conversation to carry on", |event| {
        matches!(event, AppEvent::ConversationResumed { .. })
    })?;
    pty.wait_for("1 run needs you · /resume", WAIT)?;
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;
    let text = pty.screen().visible_text();
    assert!(
        !text.contains(QUESTION) && !text.contains(TITLE),
        "the task or the run showed after -c:\n{text}"
    );
    check_off_the_panel(&scenario, &pty)?;
    assert_eq!(run_events(&waiting.dir)?.len(), journal_before);
    scenario.shot(
        &pty,
        "app_continue_leaves_the_waiting_question_for_you_to_open",
    )?;
    Ok(())
}

/// A run belongs to the conversation that started or opened it. Started now, it gets a panel row.
/// After a restart it shows only under the header, with no row and no count in the bottom line,
/// until `/resume <run>` opens it, question and row. `argonauts -c` carries on the newest
/// conversation, one with no turn that only opened the run, and brings its run back with it,
/// row and question.
#[test]
fn app_waiting_run_belongs_to_the_conversation_that_started_it() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_question_config())?;
    let mut pty = start(&scenario)?;
    let (run_dir, _) = reach_merge_question(&scenario, &mut pty)?;
    let run = run_id(&run_dir)?;
    pty.wait_for(NEEDS_YOU_ROW, WAIT)?;
    scenario.shot(&pty, "app_run_started_now_gets_a_row")?;
    wait_journal(&run_dir, "the run to wait", is_waiting)?;
    wait_pid_gone(run_host_pid(&run_dir)?)?;
    quit(&mut pty)?;

    rotate_trace(&scenario, "first")?;
    let mut pty = start(&scenario)?;
    pty.wait_for("1 run needs you · /resume", WAIT)?;
    check_bottom_row(&pty)?;
    check_off_the_panel(&scenario, &pty)?;
    scenario.shot(&pty, "app_restart_leaves_an_earlier_run_off_the_panel")?;
    submit(&mut pty, &format!("/resume {run}"))?;
    wait_event(
        &scenario,
        "the run to open",
        |event| matches!(event, AppEvent::Attached { summary, .. } if summary.run == run),
    )?;
    wait_approval(&scenario, ApprovalKind::Merge)?;
    pty.wait_for("◆ Merge this run into main?", WAIT)?;
    pty.wait_for(NEEDS_YOU_ROW, WAIT)?;
    scenario.shot(&pty, "app_resume_brings_a_run_into_the_panel")?;
    quit(&mut pty)?;

    // The newest conversation carries on, though it only opened the run, and brings its row
    // back.
    rotate_trace(&scenario, "second")?;
    let pty = start_with(&scenario, &["-c"])?;
    wait_event(&scenario, "the conversation to carry on", |event| {
        matches!(event, AppEvent::ConversationResumed { .. })
    })?;
    // Its run comes back with it, drawn as it printed, with the question it waits on.
    pty.wait_for(NEEDS_YOU_ROW, WAIT)?;
    check_bottom_row(&pty)?;
    pty.wait_for("◆ Merge this run into main?", WAIT)?;
    // The question takes the keys.
    pty.wait_for(r"(?m)^  Enter to \w+.* · Esc (to|back) ", WAIT)?;
    scenario.shot(&pty, "app_continue_brings_back_the_conversations_run")?;
    Ok(())
}

/// A start says each one-time thing once, the CLIs first: a newer version of a CLI that is
/// out, then a run an older Argonauts Code wrote. The start after it says neither again.
#[test]
fn app_start_names_a_newer_cli_then_an_older_run_once_each() -> anyhow::Result<()> {
    use argonauts_conductor::boards::cli_versions::CliVersions;

    const NEWER: &str = "Claude Code 2.1.999 is out";
    const SHOWN_ONCE: &str = "shown once for this version";
    const OLDER: &str = "was written by an older Argonauts and can't be opened";

    let scenario = scenario()?;
    let paths = Paths::new(scenario.repo.home());
    // Where Claude Code was installed from said a moment ago what its newest version is.
    let seen_at = paths.cli_versions();
    let mut seen = CliVersions::load(&seen_at);
    let now = time::OffsetDateTime::now_utc();
    seen.asked_newest("claude-code", Some("2.1.999".to_owned()), now);
    fs::create_dir_all(seen_at.parent().context("the versions file has a folder")?)?;
    seen.save(&seen_at)?;
    // A run of this folder that an older build wrote.
    let repo = RepoId::for_repo(scenario.repo.root()).context("naming the test repo")?;
    let run = RunId::new("run-20261004-072329-63c3")?;
    let run_dir = paths.run_dir(&repo, &run);
    fs::create_dir_all(&run_dir)?;
    let older = Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/older-run");
    for file in [run_files::START, run_files::EVENTS] {
        fs::copy(older.join(file), run_dir.join(file))
            .with_context(|| format!("copying the older run's {file}"))?;
    }

    let mut pty = start(&scenario)?;
    pty.wait_for(OLDER, WAIT)?;
    let said = pty.screen().text();
    for line in [NEWER, SHOWN_ONCE, OLDER] {
        assert_eq!(said.matches(line).count(), 1, "{line}:\n{said}");
    }
    assert!(said.find(NEWER) < said.find(OLDER), "{said}");
    quit(&mut pty)?;

    rotate_trace(&scenario, "first")?;
    let mut pty = start(&scenario)?;
    check_bottom_row(&pty)?;
    // Through its start, the app has sent every line it says once.
    wait_start_through(&scenario)?;
    let notices = notices(&scenario)?;
    let said = pty.screen().text();
    for line in [NEWER, SHOWN_ONCE, OLDER] {
        assert!(
            !notices.iter().any(|notice| notice.contains(line)),
            "{line} was said again: {notices:?}"
        );
        assert!(!said.contains(line), "{line} was said again:\n{said}");
    }
    quit(&mut pty)
}

#[test]
fn app_resume_waiting_run() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_question_config())?;
    let mut pty = start(&scenario)?;
    let (run_dir, _) = reach_merge_question(&scenario, &mut pty)?;
    let run = run_id(&run_dir)?;
    wait_journal(&run_dir, "the run to wait", is_waiting)?;
    wait_pid_gone(run_host_pid(&run_dir)?)?;
    quit(&mut pty)?;

    rotate_trace(&scenario, "first")?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, &format!("/resume {run}"))?;
    wait_event(
        &scenario,
        "the app to attach",
        |event| matches!(event, AppEvent::Attached { summary, .. } if summary.run == run),
    )?;
    wait_approval(&scenario, ApprovalKind::Merge)?;
    pty.wait_for("◆ Merge this run into main?", WAIT)?;
    merge(&mut pty, &run_dir)?;
    scenario.shot(&pty, "app_resume_waiting_run")?;
    Ok(())
}

#[test]
fn app_continue_merged_run() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_question_config())?;
    let mut pty = start(&scenario)?;
    let (first_dir, _) = reach_merge_question(&scenario, &mut pty)?;
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
    let scenario = scenario_with(&merge_question_config())?;
    let mut pty = start(&scenario)?;
    let run_dir = start_captain(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    let worker = wait_worker_started(&scenario)?;

    submit(&mut pty, "/stop")?;
    wait_journal(
        &run_dir,
        "the run to stop",
        is_finished(RunOutcome::Stopped),
    )?;
    wait_pid_gone(worker)?;

    // A stopped run can be resumed: the worker's CLI session picks up where it was.
    submit(&mut pty, &format!("/resume {run}"))?;
    wait_journal(&run_dir, "the run to resume", is_resumed)?;
    wait_until("the worker's session to resume", || {
        Ok(claude_sessions(&scenario, "acceptEdits")?
            .into_iter()
            .find(|i| has_flag(&i.args, "--resume", "sess-worker-slow")))
    })?;

    // A merged one refuses.
    wait_approval(&scenario, ApprovalKind::Merge)?;
    merge(&mut pty, &run_dir)?;
    let resumes_before = run_events(&run_dir)?.into_iter().filter(is_resumed).count();
    let notices_before = notices(&scenario)?.len();
    submit(&mut pty, &format!("/resume {run}"))?;
    let refusal = wait_until("the refusal", || {
        Ok(notices(&scenario)?.into_iter().nth(notices_before))
    })?;
    // The refusal is the command's whole answer: a merged run gets no run host to resume it.
    assert!(refusal.contains("merged"), "{refusal}");
    let resumes_after = run_events(&run_dir)?.into_iter().filter(is_resumed).count();
    assert_eq!(resumes_after, resumes_before, "a merged run resumed");
    scenario.shot(&pty, "app_resume_stopped_run")?;
    Ok(())
}

#[test]
fn app_resume_cli_opens_picker() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let waiting = fabricate_waiting_run(&scenario, 1)?;
    let journal_before = run_events(&waiting.dir)?.len();

    let mut pty = scenario.launch(&["resume"])?;
    // The picker can take the screen before the bottom line draws, which shows again after Esc.
    wait_event(&scenario, "the app's welcome", |event| {
        matches!(event, AppEvent::Welcome { .. })
    })?;
    let listed = wait_event(
        &scenario,
        "the resume picker",
        |event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::ResumeList { .. })),
    )?;
    let AppEvent::View { data } = listed else {
        unreachable!("matched a view")
    };
    let ViewData::ResumeList { conversations, .. } = *data else {
        unreachable!("matched the resume picker")
    };
    // A run comes back with the conversation that started it, so one started without a
    // conversation isn't listed on its own.
    assert!(conversations.is_empty(), "{conversations:?}");
    pty.wait_for("Resume a conversation", WAIT)?;
    pty.press(Key::Esc)?;
    check_bottom_row(&pty)?;
    assert_eq!(
        run_events(&waiting.dir)?.len(),
        journal_before,
        "opening the picker changed the run"
    );
    scenario.shot(&pty, "app_resume_cli_opens_picker")?;
    Ok(())
}

#[test]
fn app_project_main_session_across_runs() -> anyhow::Result<()> {
    let scenario = scenario_with(&format!(
        "{}\n[roles.worker]\nlifetime = \"project\"\n",
        merge_question_config()
    ))?;
    let mut pty = start(&scenario)?;
    let (first_dir, _) = reach_merge_question(&scenario, &mut pty)?;
    merge(&mut pty, &first_dir)?;

    start_captain(&scenario, &mut pty, requests::TIMED_TASK)?;
    let workers = wait_until("the second run's worker", || {
        let workers = claude_sessions(&scenario, "acceptEdits")?;
        Ok((workers.len() >= 2).then_some(workers))
    })?;
    assert!(
        !workers[0].args.iter().any(|a| a == "--resume"),
        "the first worker resumed something"
    );
    assert!(
        has_flag(&workers[1].args, "--resume", "sess-worker-readme"),
        "the second run didn't resume the project's worker session: {:?}",
        workers[1].args
    );
    scenario.shot(&pty, "app_project_main_session_across_runs")?;
    Ok(())
}

#[test]
fn app_pause_at_safe_point_and_resume() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_question_config())?;
    let mut pty = start(&scenario)?;
    let run_dir = start_captain(&scenario, &mut pty, requests::HELD_TASK)?;
    let worker = wait_worker_started(&scenario)?;

    scenario.pause_run(&mut pty)?;
    wait_journal(&run_dir, "the pause request", |event| {
        matches!(
            event.kind,
            RunEventKind::Run(RunLifecycle::PauseRequested { now: false, .. })
        )
    })?;
    let paused = wait_journal(&run_dir, "the run to pause", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::Paused { .. }))
    })?;
    // A safe point stops each session after its current tool call. The worker had none open,
    // so it stopped at once, and its work was committed.
    if let RunEventKind::Run(RunLifecycle::Paused {
        sessions_stopped,
        committed,
    }) = paused.kind
    {
        assert_eq!(sessions_stopped, 1, "the pause didn't stop the worker");
        assert!(
            committed.iter().any(|task| task.as_str() == "task-001"),
            "{committed:?}"
        );
    }
    wait_journal(&run_dir, "the run to wait", |event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::Waiting {
                reason: WaitReason::Paused,
                ..
            })
        )
    })?;
    // Paused after its tool call, which a stop-now would have cut short.
    let worker_end = run_events(&run_dir)?
        .into_iter()
        .find_map(|event| match event.kind {
            RunEventKind::Session(SessionEvent::Ended(end)) => Some(end.reason),
            _ => None,
        })
        .context("the worker's session never ended")?;
    assert_eq!(worker_end, EndReason::Paused, "the worker was cut off");
    wait_pid_gone(worker)?;
    wait_mode(&scenario, "the waiting mode", |m| {
        matches!(m, AppMode::Waiting { .. })
    })?;

    scenario.clis.signal("worker-may-finish")?;
    submit(&mut pty, "/resume")?;
    wait_journal(&run_dir, "the run to resume", is_resumed)?;
    wait_approval(&scenario, ApprovalKind::Merge)?;
    scenario.shot(&pty, "app_pause_at_safe_point_and_resume")?;
    Ok(())
}

/// The workflow view's own `p`: Ctrl+T opens the run's view, `p` pauses it at its next safe
/// point and the keys say `p resume`, nothing starts while it waits, and `p` again carries it
/// on, back to `p pause`.
#[test]
fn app_workflow_view_p_pauses_and_resumes() -> anyhow::Result<()> {
    let scenario = scenario_with(&merge_question_config())?;
    let mut pty = start(&scenario)?;
    let run_dir = start_captain(&scenario, &mut pty, requests::HELD_TASK)?;
    let worker = wait_worker_started(&scenario)?;

    pty.press(Key::Ctrl('t'))?;
    pty.wait_for_visible(r"p pause · esc back", WAIT)?;
    pty.press(Key::Char('p'))?;
    wait_journal(&run_dir, "the pause request", |event| {
        matches!(
            event.kind,
            RunEventKind::Run(RunLifecycle::PauseRequested { now: false, .. })
        )
    })?;
    wait_journal(&run_dir, "the run to pause", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::Paused { .. }))
    })?;
    wait_journal(&run_dir, "the run to wait", |event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::Waiting {
                reason: WaitReason::Paused,
                ..
            })
        )
    })?;
    wait_pid_gone(worker)?;
    pty.wait_for_visible(r"p resume · esc back", WAIT)?;
    let started = |run_dir: &Path| -> anyhow::Result<usize> {
        Ok(run_events(run_dir)?
            .iter()
            .filter(|event| matches!(event.kind, RunEventKind::Session(SessionEvent::Started(_))))
            .count())
    };
    let before = started(&run_dir)?;

    pty.press(Key::Char('p'))?;
    wait_journal(&run_dir, "the run to resume", is_resumed)?;
    // While it waited, from its wait to its resume, no session started.
    let events = run_events(&run_dir)?;
    let waited = events
        .iter()
        .position(|event| {
            matches!(
                &event.kind,
                RunEventKind::Run(RunLifecycle::Waiting {
                    reason: WaitReason::Paused,
                    ..
                })
            )
        })
        .context("the run's wait")?;
    let resumed = events
        .iter()
        .position(is_resumed)
        .context("the run's resume")?;
    assert!(
        !events[waited..resumed]
            .iter()
            .any(|event| matches!(event.kind, RunEventKind::Session(SessionEvent::Started(_)))),
        "a session started while paused"
    );
    pty.wait_for_visible(r"p pause · esc back", WAIT)?;
    // Only now may the worker finish: a fast one would otherwise reach the merge question
    // before the keys go back to `p pause`. The signal waits for the step that reads it.
    scenario.clis.signal("worker-may-finish")?;
    wait_approval(&scenario, ApprovalKind::Merge)?;
    assert!(
        started(&run_dir)? > before,
        "nothing started once it went on"
    );
    scenario.shot(&pty, "app_workflow_view_p_pauses_and_resumes")?;
    Ok(())
}

#[test]
fn app_second_esc_stops_now_and_commits() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    // A worker in a tool call keeps the pause pending, so the second Esc finds it pausing.
    let run_dir = start_captain(&scenario, &mut pty, requests::TASK_IN_A_TOOL_CALL)?;
    let run = run_id(&run_dir)?;
    let worker = wait_worker_started(&scenario)?;
    // The worker writes its file before it starts the build.
    pty.wait_for("cargo build", WAIT)?;

    // Esc leaves a working run going. Once it is pausing, Esc stops it now.
    scenario.pause_run(&mut pty)?;
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
            committed.iter().any(|task| task.as_str() == "task-001"),
            "{committed:?}"
        );
    }
    wait_pid_gone(worker)?;
    let committed_work = run_branches(&scenario, &run)?.into_iter().any(|branch| {
        branch_files(&scenario, &branch)
            .is_ok_and(|files| files.contains(&"loader-notes.md".to_owned()))
    });
    assert!(committed_work, "the worker's work wasn't committed");
    scenario.shot(&pty, "app_second_esc_stops_now_and_commits")?;
    Ok(())
}

#[test]
fn app_guidance_while_waiting_delivered_on_resume() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    let run_dir = start_captain(&scenario, &mut pty, requests::SLOW_TASK)?;
    wait_worker_started(&scenario)?;
    pty.wait_for("Renaming the config loader", WAIT)?;
    scenario.pause_run(&mut pty)?;
    pty.press(Key::Esc)?;
    wait_journal(&run_dir, "the run to wait", is_waiting)?;
    wait_mode(&scenario, "the waiting mode", |m| {
        matches!(m, AppMode::Waiting { .. })
    })?;

    submit(&mut pty, requests::GUIDANCE)?;
    wait_journal(&run_dir, "the guidance in the journal", |event| {
        serde_json::to_string(event).is_ok_and(|line| line.contains(requests::GUIDANCE))
    })?;
    if !run_events(&run_dir)?.iter().any(is_resumed) {
        submit(&mut pty, "/resume")?;
    }
    let resumed = wait_until("the resumed worker", || {
        Ok(claude_sessions(&scenario, "acceptEdits")?
            .into_iter()
            .find(|i| i.args.iter().any(|a| a == "--resume")))
    })?;
    assert!(
        resumed
            .stdin
            .as_deref()
            .is_some_and(|prompt| prompt.contains(requests::GUIDANCE)),
        "the resumed worker's prompt lacks the guidance: {:?}",
        resumed.stdin
    );
    scenario.shot(&pty, "app_guidance_while_waiting_delivered_on_resume")?;
    Ok(())
}

#[test]
fn app_offers_detected_checks_once() -> anyhow::Result<()> {
    let package = "{\"scripts\": {\"test\": \"node test.js\", \"lint\": \"eslint .\"}}\n";

    // Skip means never again in this repo.
    let skipping = scenario()?;
    skipping.repo.write("package.json", package)?;
    skipping.repo.commit_all("add package.json")?;
    let mut pty = start(&skipping)?;
    let first = start_captain(&skipping, &mut pty, requests::SMALL_FIX)?;
    let offer = wait_open_choice(&skipping, argonauts_core::app::ChoiceKind::DetectedChecks)?;
    let body = offer.body.join("\n");
    assert!(body.contains("test") && body.contains("lint"), "{body}");
    let keys: Vec<char> = offer.options.iter().map(|o| o.key).collect();
    assert_eq!(keys, ['y', 'e', 'n'], "{offer:?}");
    answer_question(&mut pty, number_of(&offer.options, 'n'))?;
    wait_choice_closed(&skipping, &offer.id)?;
    // The run's merge question takes the keys as it shows, so the next run starts once this
    // one has merged.
    wait_approval(&skipping, ApprovalKind::Merge)?;
    merge(&mut pty, &first)?;
    // The checks are offered before a run starts, so once its worker has started, an offer
    // for it would be in the trace.
    start_captain(&skipping, &mut pty, requests::TIMED_TASK)?;
    wait_worker_started(&skipping)?;
    let offers = events(&skipping)?
        .into_iter()
        .filter(|event| {
            matches!(event, AppEvent::Choice { choice }
                if choice.kind == argonauts_core::app::ChoiceKind::DetectedChecks)
        })
        .count();
    assert_eq!(offers, 1, "the checks were offered again after Skip");
    skipping.shot(&pty, "app_offers_detected_checks_once_skip")?;
    quit(&mut pty)?;

    // Use these writes them to the project file.
    let using = scenario()?;
    using.repo.write("package.json", package)?;
    using.repo.commit_all("add package.json")?;
    let mut pty = start(&using)?;
    start_captain(&using, &mut pty, requests::SMALL_FIX)?;
    let offer = wait_open_choice(&using, argonauts_core::app::ChoiceKind::DetectedChecks)?;
    answer_question(&mut pty, number_of(&offer.options, 'y'))?;
    wait_choice_closed(&using, &offer.id)?;
    let written = wait_until("the checks in the project file", || {
        Ok(read(&project_config(&using)).ok().filter(|text| {
            toml_entries(text)
                .iter()
                .any(|e| e.key.starts_with("checks.run"))
        }))
    })?;
    assert!(written.contains("npm"), "{written}");
    using.shot(&pty, "app_offers_detected_checks_once_use")?;
    Ok(())
}

/// A run that would use a Codex older than the oldest version supported waits on one question,
/// which says beside each answer what it does. Esc starts nothing; Run with it anyway starts
/// the run with Codex as it is.
#[test]
fn app_asks_before_a_run_on_a_cli_that_must_be_updated() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut fakes = FakeScenario::load(&scenario_file(AGENTS))?;
    fakes
        .version
        .insert("codex".to_owned(), "codex-cli 0.1.0".to_owned());
    let outdated = dir.path().join("outdated-codex.toml");
    fakes.save(&outdated)?;
    let scenario = scenario_playing(&outdated)?;
    fs::write(scenario.repo.global_config(), run_config())?;
    let mut pty = start(&scenario)?;

    submit(&mut pty, &format!("/captain {}", requests::SMALL_FIX))?;
    let asked = wait_open_choice(&scenario, ChoiceKind::NeededUpdate)?;
    assert!(
        asked
            .title
            .starts_with("Codex 0.1.0 is older than Argonauts Code supports ("),
        "{}",
        asked.title
    );
    pty.wait_for(
        r"1\. Update it now {18}codex update, then the run starts",
        WAIT,
    )?;
    let screen = pty.screen().text();
    for row in [
        "◆ Codex 0.1.0 is older than Argonauts Code supports (",
        "  Runs with it may misread what it does. Nothing has started yet.",
        "2. Leave Codex out of this run    its roles go to the next model in your fallback order",
        "3. Run with it anyway             and ask again before the next run",
    ] {
        assert!(screen.contains(row), "no {row:?} in\n{screen}");
    }
    assert!(run_dirs(&scenario).is_empty(), "nothing has started yet");
    scenario.shot(&pty, "app_asks_before_a_run_on_a_cli_that_must_be_updated")?;

    argonauts_scenarios::past_the_question_guard();
    pty.press(Key::Esc)?;
    let outcome = wait_choice_closed(&scenario, &asked.id)?;
    assert_eq!(outcome, "Not started · Codex 0.1.0 still needs an update");
    assert!(run_dirs(&scenario).is_empty(), "Esc started a run");

    submit(&mut pty, &format!("/captain {}", requests::SMALL_FIX))?;
    let again = wait_until("the question again", || {
        Ok(open_choices(&scenario)?
            .into_iter()
            .find(|choice| choice.kind == ChoiceKind::NeededUpdate && choice.id != asked.id))
    })?;
    answer_question(&mut pty, number_of(&again.options, 'r'))?;
    assert_eq!(
        wait_choice_closed(&scenario, &again.id)?,
        "Running with Codex 0.1.0."
    );
    wait_run_dir(&scenario)?;
    quit(&mut pty)
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
        // Bare, `/config` and `/settings` open the Config tab on the screen, which asks for its
        // rows; with a key the app answers them.
        let opens_config = matches!(spec.name, "config" | "settings");
        // The screen answers some itself, such as `/theme`, which asks for its rows.
        if spec.handler != Handler::App || needs_argument || opens_config {
            // Handled here means something answered: the app or the screen.
            wait_until(&format!("an answer to {line}"), || {
                let answered =
                    scenario.trace.lines()?.len() > before || pty.screen().text() != screen_before;
                Ok(answered.then_some(()))
            })?;
        } else {
            wait_answered(&scenario, before, &line)?;
        }
        pty.press(Key::Esc)?;
        // The menu completes a command that needs an argument rather than running it, so
        // its text is still in the prompt.
        if needs_argument {
            pty.press(Key::Ctrl('c'))?;
        }
        back_to_the_input(&mut pty).map_err(|err| err.context(format!("after {line}")))?;
    }

    // `/setup` checks each CLI's login and `/update` runs each CLI's updater (`update`, or
    // OpenCode's `upgrade`), and neither calls a model.
    const NO_MODEL_CALL: [&[&str]; 3] = [&["auth", "status"], &["update"], &["upgrade"]];
    let sessions: Vec<_> = all_sessions(&scenario)?
        .into_iter()
        .filter(|invocation| !NO_MODEL_CALL.iter().any(|args| invocation.args == *args))
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

/// The rewind picker's own key, which nothing else on the screen shows. The transcript already
/// names `fallback-1`, so waiting for the row's text could press a key before the picker opens.
const REWIND_PICKER_KEY: &str = "go back to it";

/// `/rewind task-001` opens its picker; this returns the picker's newest rows, still open and
/// drawn.
fn open_rewind(scenario: &Scenario, pty: &mut Pty) -> anyhow::Result<Vec<CheckpointRow>> {
    let is_picker = |event: &AppEvent| matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Checkpoints { .. }));
    let before = events(scenario)?
        .into_iter()
        .filter(|e| is_picker(e))
        .count();
    submit(pty, "/rewind task-001")?;
    let shown = wait_events(scenario, "the /rewind picker", before + 1, is_picker)?;
    let rows = match shown.last() {
        Some(AppEvent::View { data }) => match data.as_ref() {
            ViewData::Checkpoints { checkpoints, .. } => checkpoints.clone(),
            other => bail!("not the /rewind picker: {other:?}"),
        },
        _ => bail!("no /rewind picker"),
    };
    pty.wait_for_visible(REWIND_PICKER_KEY, WAIT)?;
    Ok(rows)
}

fn list_checkpoints(scenario: &Scenario, pty: &mut Pty) -> anyhow::Result<Vec<CheckpointRow>> {
    let rows = open_rewind(scenario, pty)?;
    pty.press(Key::Esc)?;
    pty.wait_until("the /rewind picker to close", WAIT, |screen| {
        !screen.visible_text().contains(REWIND_PICKER_KEY)
    })?;
    Ok(rows)
}

/// Picks fallback-1, the only checkpoint, so the newest is the one picked.
fn ask_rewind(scenario: &Scenario, pty: &mut Pty) -> anyhow::Result<AppChoice> {
    open_rewind(scenario, pty)?;
    pty.press(Key::Enter)?;
    wait_shown_choice(scenario, pty, ChoiceKind::ConfirmRewind)
}

/// `/rewind` at the merge approval: the task's branch goes back to `fallback-1`, and the run's
/// branch drops the work after it, which stays on the replaced branch.
#[test]
fn app_checkpoints_and_rewind_commands() -> anyhow::Result<()> {
    let scenario = scenario_with(&codex_config(&format!(
        "{}\n[fallback]\nmode = \"auto\"\norder = [\"codex\"]\n",
        merge_question_config()
    )))?;
    let mut pty = start(&scenario)?;
    let run_dir = start_captain(&scenario, &mut pty, LIMITED_TASK)?;
    let run = run_id(&run_dir)?;
    let tagged = wait_journal(&run_dir, "the fallback checkpoint", |event| {
        matches!(
            event.kind,
            RunEventKind::Task(TaskEvent::CheckpointTagged { n: 1, .. })
        )
    })?;
    let RunEventKind::Task(TaskEvent::CheckpointTagged { commit, .. }) = tagged.kind else {
        unreachable!("matched a tag")
    };
    // Codex's commit is in once the merge question opens. No agent works while it waits, so the
    // run waits; the commands below wait for that, since one sent as it waits is refused.
    let merge_question = wait_approval(&scenario, ApprovalKind::Merge)?;
    wait_journal(&run_dir, "the run to wait", is_waiting)?;
    wait_pid_gone(run_host_pid(&run_dir)?)?;
    let ApprovalPacket::Merge(merge_packet) = merge_question.packet else {
        bail!("the merge approval has no merge packet: {merge_question:?}");
    };
    let run_branch = merge_packet.branch;
    let task_branch = format!("argonauts/{run}/task-001");
    let tag = format!("argonauts/{run}/task-001/fallback-1");
    let kept_on = format!("argonauts/{run}/task-001-replaced-1");

    set_question_aside(&mut pty)?;
    // Set aside, the merge question still waits: Esc sent no answer for it.
    let answered = commands(&scenario)?.into_iter().any(|command| {
        matches!(
            command,
            UiCommand::Answer {
                to: AnswerTarget::Approval { .. },
                ..
            } | UiCommand::SetAside {
                to: AnswerTarget::Approval { .. }
            }
        )
    });
    assert!(!answered, "Esc answered the merge question");
    let listed = list_checkpoints(&scenario, &mut pty)?;
    let [row] = listed.as_slice() else {
        bail!("one approval, not {listed:?}");
    };
    assert_eq!((row.task.as_str(), row.n), ("task-001", 1), "{row:?}");
    assert_eq!(row.from.agent, "claude-code", "{row:?}");
    assert!(row.to.iter().any(|to| to.agent == "codex"), "{row:?}");
    assert_eq!(row.commit, commit, "{row:?}");
    assert!(!row.rewound_here, "{row:?}");

    // Left with Esc, the confirm rewinds nothing.
    let heads = || -> anyhow::Result<[String; 2]> {
        Ok([
            scenario.repo.git(&["rev-parse", &task_branch])?,
            scenario.repo.git(&["rev-parse", &run_branch])?,
        ])
    };
    let before = heads()?;
    let confirm = ask_rewind(&scenario, &mut pty)?;
    let body = confirm.body.join("\n");
    assert!(body.contains(&tag) && body.contains(&kept_on), "{body}");
    pty.press(Key::Esc)?;
    assert_eq!(wait_choice_closed(&scenario, &confirm.id)?, "Not rewound.");
    assert_eq!(heads()?, before);

    let confirm = ask_rewind(&scenario, &mut pty)?;
    answer_question(&mut pty, number_of(&confirm.options, 'y'))?;
    let outcome = wait_choice_closed(&scenario, &confirm.id)?;
    assert!(
        outcome.starts_with("Rewound"),
        "{outcome}\n{}",
        pty.screen().visible.join("\n")
    );
    wait_journal(&run_dir, "the rewind", |event| {
        matches!(
            &event.kind,
            RunEventKind::Task(TaskEvent::Rewound { to: 1, kept_on: kept })
                if *kept == kept_on
        )
    })?;
    assert_eq!(
        scenario.repo.git(&["rev-parse", &task_branch])?,
        scenario
            .repo
            .git(&["rev-parse", &format!("{tag}^{{commit}}")])?,
        "the task branch is back at fallback-1"
    );
    let task_files = branch_files(&scenario, &task_branch)?;
    assert!(
        task_files.contains(&"export-job.md".to_owned())
            && !task_files.contains(&"export-callers.md".to_owned()),
        "{task_files:?}"
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
        "the run's branch still has the work after fallback-1: {run_files:?}"
    );

    // The rewind opens a fresh merge question, which takes the keys as it shows.
    set_question_aside(&mut pty)?;
    let listed = list_checkpoints(&scenario, &mut pty)?;
    assert!(listed.iter().all(|row| row.rewound_here), "{listed:?}");
    scenario.shot(&pty, "app_checkpoints_and_rewind_commands")?;
    Ok(())
}

#[test]
fn app_main_agent_picks_review_panel_and_fills_params() -> anyhow::Result<()> {
    // Picking another workflow than the one in use takes `orchestrator_designs = "any"`.
    let workflows = config_with_workflows("orchestrator_designs = \"any\"\n");
    let scenario = scenario_with(&codex_config(&format!("{workflows}{YOU_ANSWER}")))?;
    // The review panel is a project's own workflow, offered to the main agent beside the built-ins.
    scenario
        .repo
        .write(".argonauts/workflows/review-panel.yaml", REVIEW_PANEL)?;
    scenario.repo.commit_all("add the review panel workflow")?;
    let mut pty = start(&scenario)?;
    // A workflow that came with the repository waits until you trust it, once.
    pty.wait_for(r"Use these instructions\?", WAIT)?;
    past_the_question_guard();
    pty.send("1")?;
    pty.wait_for(r"Trusted ", WAIT)?;
    submit(&mut pty, &format!("/crew {REVIEW_TASK}"))?;
    let run_dir = wait_run_dir(&scenario)?;
    // The readings disagree on scope, so the goal question asks first.
    let goal = wait_approval(&scenario, ApprovalKind::Understand)?;
    answer_question(&mut pty, number_of(&goal.options, 'a'))?;

    let design = wait_until("the main agent's design session", || {
        Ok(sessions_of(&scenario, "claude")?
            .into_iter()
            .find(|invocation| prompt_of(invocation).contains("\"done_when\"")))
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

    // Understand opens the run and its question is the one go-ahead: a review writes nothing, so no
    // plan task follows, and the review phase runs.
    assert_eq!(
        started
            .workflow
            .phases
            .first()
            .map(|phase| phase.name.as_str()),
        Some("Understand")
    );
    wait_journal(&run_dir, "the Review phase", |event| {
        matches!(
            &event.kind,
            RunEventKind::Workflow(WorkflowEvent::PhaseStarted(mark)) if mark.name == "Review"
        )
    })?;
    let plans = run_events(&run_dir)?
        .into_iter()
        .filter(|event| {
            matches!(
                &event.kind,
                RunEventKind::Approval(ApprovalEvent::Opened(opened))
                    if opened.kind == ApprovalKind::Plan
            )
        })
        .count();
    assert_eq!(plans, 0, "a review needs no plan approval");
    scenario.shot(&pty, "app_main_agent_picks_review_panel_and_fills_params")?;
    Ok(())
}

/// The fallback rule a remembered fallback pick saves.
const REMEMBERED_RULE: &str = "fallback.worker.replace.claude-code.with";

/// A fallback that asks, with Codex to pick.
fn fallback_question_config() -> String {
    codex_config(&format!(
        "{}\n[fallback]\nmode = \"ask\"\norder = [\"codex\"]\n",
        merge_question_config()
    ))
}

/// The fallback question's Codex candidate: its key and worker.
/// The Codex candidate's number on the question, and the worker.
fn codex_candidate(question: &ApprovalOpened) -> anyhow::Result<(char, ModelRef)> {
    question
        .options
        .iter()
        .find_map(|option| match &option.action {
            AnswerAction::Candidate { worker } if worker.agent == "codex" => {
                Some((number_of(&question.options, option.key), worker.clone()))
            }
            _ => None,
        })
        .with_context(|| format!("no Codex candidate on {question:?}"))
}

/// Remember this pick, the toggle after the fallback question's own options.
fn remember_toggle(question: &ApprovalOpened) -> char {
    char::from_digit(question.options.len() as u32 + 1, 10).expect("at most 9 options")
}

/// Asks to remember an answered fallback question's pick: `m` on the question's record line.
fn remember_answered_pick(pty: &mut Pty) -> anyhow::Result<()> {
    pty.press(Key::Up)?;
    pty.press(Key::Char('m'))
}

#[test]
fn app_remember_pick_saves_fallback_rule() -> anyhow::Result<()> {
    let key = REMEMBERED_RULE;

    // Remembered as you pick, and saved for the project, where your changes go.
    let config = format!(
        "save_changes_to = \"this_project\"\n{}",
        fallback_question_config()
    );
    let scenario = scenario_with(&config)?;
    scenario
        .repo
        .write(".argonauts/config.toml", &format!("{KEPT_COMMENT}\n"))?;
    scenario.repo.commit_all("add the project settings")?;
    let mut pty = start(&scenario)?;
    let first = start_captain(&scenario, &mut pty, LIMITED_TASK)?;
    let question = wait_approval(&scenario, ApprovalKind::Fallback)?;
    let (pick_key, pick) = codex_candidate(&question)?;
    let rule = format!("[\"{pick}\"]");
    answer_question(&mut pty, remember_toggle(&question))?;
    pty.press(Key::Char(pick_key))?;
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
    wait_until("Codex to take the task", || {
        Ok(sessions_of(&scenario, "codex")?
            .into_iter()
            .find(|invocation| prompt_of(invocation).contains(LIMITED_TASK)))
    })?;
    // The run's merge question takes the keys as it shows, so the next run starts once this
    // one has merged.
    wait_approval(&scenario, ApprovalKind::Merge)?;
    merge(&mut pty, &first)?;
    let next_dir = start_captain(&scenario, &mut pty, requests::TIMED_TASK)?;
    let settings = wait_until("the next run's settings", || {
        Ok(run_settings(&next_dir).ok())
    })?;
    // `settings.json` keeps the resolved settings under `settings`, beside their hash.
    assert_eq!(
        json_at(&settings, &format!("settings.{key}")),
        Some(&json!([pick.to_string()])),
        "{settings}"
    );
    scenario.shot(&pty, "app_remember_pick_saves_fallback_rule_project")?;
    Ok(())
}

#[test]
fn app_remember_answered_pick_saves_fallback_rule() -> anyhow::Result<()> {
    let key = REMEMBERED_RULE;

    // Remembered after the answer, and saved for just you, where your changes go.
    let config = format!(
        "save_changes_to = \"just_you\"\n{}",
        fallback_question_config()
    );
    let scenario = scenario_with(&config)?;
    let mut pty = start(&scenario)?;
    start_captain(&scenario, &mut pty, LIMITED_TASK)?;
    let question = wait_approval(&scenario, ApprovalKind::Fallback)?;
    let (pick_key, pick) = codex_candidate(&question)?;
    answer_question(&mut pty, pick_key)?;
    remember_answered_pick(&mut pty)?;
    wait_until("the remembered pick", || {
        Ok(commands(&scenario)?.into_iter().find(
            |command| matches!(command, UiCommand::RememberPick { id, .. } if *id == question.id),
        ))
    })?;
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
    scenario.shot(&pty, "app_remember_pick_saves_fallback_rule_local")?;
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
    let run_dir = start_captain(&scenario, &mut pty, requests::SLOW_TASK)?;
    let run = run_id(&run_dir)?;
    wait_worker_started(&scenario)?;
    // The worker reports its usage with this message.
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
    let conversation = rows.first().context("the context view has no rows")?;
    assert!(
        conversation.run.is_none() && conversation.label == "conversation",
        "{rows:#?}"
    );
    let workers: Vec<_> = rows
        .iter()
        .filter(|row| row.run.as_ref() == Some(&run))
        .collect();
    let [worker] = workers.as_slice() else {
        bail!("expected one worker row: {rows:#?}");
    };
    assert_eq!(worker.cap, Some(50_000), "{worker:?}");
    assert_eq!(
        (worker.spent.input_tokens, worker.spent.output_tokens),
        (1_200, 300),
        "{worker:?}"
    );
    assert!(worker.used >= 1_200, "{worker:?}");

    // The worker goes by its label, not its task's id. Wait for the view's own count of its
    // rows: the main agent's window comes first, under the title Claude Code gives it, then the
    // run's crew folded to one line.
    assert!(worker.label.starts_with("build:"), "{worker:?}");
    let text = pty.wait_for(r"\d+ sessions", WAIT)?.text();
    assert!(text.contains("Context Usage"), "{text}");
    // At this size the main agent's window fills the screen, so the crew comes into view as
    // the keys go down past it, a still screen after each.
    let mut text = String::new();
    for _ in 0..40 {
        pty.send("\x1b[B")?;
        text = pty
            .wait_until("a still screen", WAIT, |_| true)?
            .visible_text();
        if text.contains("Crew agents") {
            break;
        }
    }
    assert!(
        text.contains("Crew agents") && text.contains("1 agent"),
        "{text}"
    );
    // The view is the command's answer, drawn from what it counted, so a session it started
    // would be among them by now.
    assert_eq!(
        all_sessions(&scenario)?.len(),
        sessions,
        "/context started a session"
    );
    scenario.shot(&pty, "app_context_command_lists_sessions")?;
    Ok(())
}

/// A second git repo beside the scenario's, under the same Argonauts Code home.
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
fn waiting_run_in(scenario: &Scenario, root: &Path) -> anyhow::Result<WaitingRun> {
    let waiting = fabricate_waiting_run(scenario, 0)?;
    let head = scenario
        .repo
        .git(&["-C", &root.to_string_lossy(), "rev-parse", "HEAD"])?;
    let repo = RepoId::for_repo(root).context("naming the other repo")?;
    let dir = Paths::new(scenario.repo.home()).run_dir(&repo, &waiting.run);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut journal = String::new();
    for mut event in run_events(&waiting.dir)? {
        if let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind {
            started.repo_root = root.to_owned();
            started.base_commit = head.clone();
        }
        journal.push_str(&serde_json::to_string(&event)?);
        journal.push('\n');
    }
    fs::write(dir.join(run_files::EVENTS), journal).context("writing the moved journal")?;
    fs::remove_dir_all(&waiting.dir).context("removing the fabricated run")?;
    Ok(WaitingRun {
        run: waiting.run,
        dir,
        approval: waiting.approval,
    })
}

/// A sleeping child registered as the live run host of `run`, killed when it drops.
struct SleepingRunHost(Child);

impl SleepingRunHost {
    fn register(scenario: &Scenario, repo_root: &Path, run: &RunId) -> anyhow::Result<Self> {
        let now = || -> anyhow::Result<u64> {
            Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
        };
        let before = now()?;
        let child = Command::new("sleep")
            .arg("600")
            .spawn()
            .context("starting a fallback run host")?;
        let after = now()?;
        let fake = Self(child);
        let path = scenario.repo.home().join(user_files::REGISTRY);
        let mut registry: Value = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).context("reading the registry")?,
            Err(_) => json!({ "run_hosts": [] }),
        };
        let run_hosts = registry["run_hosts"]
            .as_array_mut()
            .context("the registry has no run hosts list")?;
        // Entries are keyed by the start time sysinfo reads, which can land a second either
        // side of this clock. Readers drop the entries whose time is wrong.
        for start_time in before.saturating_sub(1)..=after + 1 {
            run_hosts.push(json!({
                "pid": fake.0.id(),
                "start_time": start_time,
                "repo_root": repo_root,
                "run": run,
                "sessions": 0,
            }));
        }
        fs::write(&path, serde_json::to_vec_pretty(&registry)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(fake)
    }
}

impl Drop for SleepingRunHost {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Opens the agent panel with ← and returns the `n`th list the controller sent for it, which
/// holds the runs of every repo with a live run host.
fn open_agent_panel(
    scenario: &Scenario,
    pty: &mut Pty,
    n: usize,
) -> anyhow::Result<AgentPanelRows> {
    check_bottom_row(pty)?;
    pty.press(Key::Left)?;
    let panels = wait_events(
        scenario,
        "the agent panel",
        n,
        |event| matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::Crew { .. })),
    )?;
    match panels.into_iter().nth(n - 1) {
        Some(AppEvent::View { data }) => match *data {
            ViewData::Crew { panel } => Ok(panel),
            _ => unreachable!("matched the agent panel"),
        },
        _ => unreachable!("waited for {n} panels"),
    }
}

#[test]
fn app_all_repos_shows_other_repo_waiting() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let other = other_repo(&scenario)?;
    let waiting = waiting_run_in(&scenario, &other)?;
    let run_host = SleepingRunHost::register(&scenario, &other, &waiting.run)?;
    let mut pty = start(&scenario)?;

    let panel = open_agent_panel(&scenario, &mut pty, 1)?;
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
        "it waits on its task: {row:?}"
    );
    pty.wait_for("other-repo", WAIT)?;
    scenario.shot(&pty, "app_all_repos_shows_other_repo_waiting")?;

    // Only repos with a live run host are listed, so a run waiting past the wait grace, whose
    // run host exited, drops out of the panel.
    drop(run_host);
    pty.press(Key::Esc)?;
    let panel = open_agent_panel(&scenario, &mut pty, 2)?;
    assert!(
        !panel.rows.iter().any(|row| row.repo == there),
        "{panel:#?}"
    );
    Ok(())
}
