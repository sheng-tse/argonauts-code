use std::time::Duration;

use argonauts_core::Paths;
use argonauts_core::api::{RunState, RunSummary, StatusSummary, TaskBrief};
use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, ReplayTurn, UiCommand};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::events::run::{Party, RunMode, RunOutcome, WaitReason};
use argonauts_core::events::task::TaskState;
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::{ApprovalId, ConversationId, TurnId};
use argonauts_core::policy::{NarratorLine, NarratorLineKind};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::{NoteLevel, Spend, TaskId};
use argonauts_testkit::FakeSession;
use serde_json::json;
use time::OffsetDateTime;

use super::rig::{Rig, finished, question, resumed, run_id, waiting};
use super::{RunNews, RunUpdate, attached, focus, on_update, open, status_parts};
use crate::FirstView;
use crate::client::test_support as log;
use crate::controller::header_and_bottom_line::header_and_bottom_line;
use crate::controller::recap::recap_from_summary;
use crate::controller::test_support::{ApprovalCall, Call, CliOptions, TestDriver, WAIT};
use crate::controller::{Effect, Opening, run_commands};
use crate::conversation_store::{ConversationRun, ConversationStore};
use crate::env::AppEnv;

fn is_mode(mode: AppMode) -> impl Fn(&AppEvent) -> bool {
    move |event| matches!(event, AppEvent::Mode { mode: seen } if *seen == mode)
}

async fn cli_on(run: &str) -> TestDriver {
    let mut driver = TestDriver::start_with(CliOptions {
        opening: Opening {
            guided_setup: false,
            first: FirstView::Attach(run_id(run)),
        },
        ..CliOptions::default()
    })
    .await;
    driver
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    driver
}

#[tokio::test]
async fn a_started_run_attaches_and_forwards_events() {
    let mut rig = Rig::new();
    let run = rig.start("Add --json to export").await;
    assert_eq!(run, run_id("run-001"));
    assert_eq!(
        rig.conductor.calls()[..2],
        [
            Call::Start(Box::new(super::rig::start_run("Add --json to export"))),
            Call::List,
        ]
    );
    assert!(rig.conductor.calls().contains(&Call::Subscribe {
        run: run.clone(),
        from_seq: 0,
    }));
    rig.until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    assert!(rig.conductor.push(&run, resumed(&run, 1)).await);
    let forwarded = rig
        .until(|event| matches!(event, AppEvent::Run { .. }))
        .await;
    assert_eq!(
        forwarded,
        AppEvent::Run {
            event: Box::new(resumed(&run, 1))
        }
    );
    assert!(rig.conductor.push(&run, waiting(&run, 2, Vec::new())).await);
    rig.until(is_mode(AppMode::Waiting {
        run: run.clone(),
        reason: WaitReason::Approval,
    }))
    .await;
    assert_eq!(status_parts(&rig.runs).run, Some(run));
}

/// A stopped run on screen goes on in place when you reopen it: a stream from the event after
/// its last, with no new header and no condensed replay.
#[tokio::test]
async fn a_reopened_stopped_run_goes_on_in_place() {
    let mut rig = Rig::new();
    let run = rig.start("Add --json to export").await;
    rig.until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    assert!(
        rig.conductor
            .push(&run, finished(&run, 1, RunOutcome::Stopped))
            .await
    );
    rig.until(is_mode(AppMode::Idle)).await;
    rig.conductor.end(&run, false);
    rig.settle().await;
    assert!(rig.runs.attached.as_ref().is_some_and(|a| a.ended));

    super::resume_and_attach(&mut rig.ctx, &mut rig.runs, &run)
        .await
        .expect("the run resumes");
    let calls = rig.conductor.calls();
    assert!(calls.contains(&Call::Resume(run.clone())), "{calls:?}");
    assert!(
        calls.contains(&Call::Subscribe {
            run: run.clone(),
            from_seq: 2,
        }),
        "{calls:?}"
    );
    assert!(!calls.contains(&Call::Reattach(run.clone())), "{calls:?}");
    assert!(rig.conductor.push(&run, resumed(&run, 2)).await);
    rig.until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    let after: Vec<String> = scrollback(rig.seen())
        .into_iter()
        .skip_while(|label| label != "event 1")
        .collect();
    assert_eq!(after, ["event 1", "event 2"]);
}

/// `/resume` right after a run merged opens the picker, where its agents' sessions carry on: a
/// merged run can't go on, so nothing tries to resume it.
#[tokio::test]
async fn resume_after_a_merged_run_opens_the_picker() {
    let mut rig = Rig::new();
    let run = rig.start("Add --json to export").await;
    rig.until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    assert!(
        rig.conductor
            .push(&run, finished(&run, 1, RunOutcome::Merged))
            .await
    );
    rig.until(is_mode(AppMode::Idle)).await;
    rig.conductor.end(&run, false);
    rig.settle().await;
    assert!(rig.runs.attached.as_ref().is_some_and(|a| a.ended));

    run_commands::resume(&mut rig.ctx, &mut rig.runs, None)
        .await
        .expect("the picker opens");
    let calls = rig.conductor.calls();
    assert!(!calls.contains(&Call::Resume(run.clone())), "{calls:?}");
    rig.until(|event| {
        matches!(
            event,
            AppEvent::View { data } if matches!(**data, argonauts_core::app::ViewData::Runs { .. })
        )
    })
    .await;
}

/// A message command that appends its text to `log`, so a test sees what was sent.
fn record_conversation_to(log: &std::path::Path) -> impl FnOnce(&std::path::Path) {
    let script = format!("printf '%s\\n' \"$0\" >> '{}'", log.display());
    move |config: &std::path::Path| {
        let text = format!(
            "[notifications]\nos = false\n\n[notifications.conversation]\ncommand = [\"/bin/sh\", \"-c\", \
             {script:?}, \"{{text}}\"]\n"
        );
        std::fs::create_dir_all(config.parent().expect("a parent")).expect("the home dir");
        std::fs::write(config, text).expect("the global config");
    }
}

/// The log's lines once it holds `count`, or after `wait`.
async fn lines_after(log: &std::path::Path, count: usize, wait: Duration) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        if lines.len() >= count || tokio::time::Instant::now() > deadline {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn replayed_approvals_before_replay_done_do_not_notify() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let log = dir.path().join("conversation.log");
    let mut rig = Rig::with(true, record_conversation_to(&log));
    let run = rig.start("Fix the flaky test").await;
    rig.settle().await;
    focus(&mut rig.runs, false);
    rig.runs
        .attached
        .as_mut()
        .expect("attached")
        .replay_history_until(2);

    rig.conductor.push(&run, resumed(&run, 0)).await;
    rig.conductor
        .push(&run, question(&run, 1, "approval-001"))
        .await;
    rig.conductor.push(&run, waiting(&run, 2, vec![])).await;
    let done = rig
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    assert_eq!(
        done,
        AppEvent::ReplayDone {
            run: run.clone(),
            seq: 2
        }
    );
    rig.until(is_mode(AppMode::Waiting {
        run: run.clone(),
        reason: WaitReason::Approval,
    }))
    .await;
    rig.settle().await;
    let quiet = Duration::from_millis(300);
    assert!(
        lines_after(&log, 1, quiet).await.is_empty(),
        "history notified"
    );

    rig.conductor.push(&run, resumed(&run, 3)).await;
    rig.conductor
        .push(&run, question(&run, 4, "approval-002"))
        .await;
    rig.settle().await;
    let lines = lines_after(&log, 1, WAIT).await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(&format!("{run} waits for you at approval-002")),
        "{lines:?}"
    );
    assert!(
        !lines[0].contains("Which store"),
        "agent text leaked: {lines:?}"
    );

    focus(&mut rig.runs, true);
    rig.conductor
        .push(&run, question(&run, 5, "approval-003"))
        .await;
    rig.settle().await;
    assert_eq!(
        lines_after(&log, 2, quiet).await.len(),
        1,
        "notified while focused"
    );
}

#[tokio::test]
async fn a_stream_that_loses_its_run_host_says_what_resume_does() {
    let mut driver = cli_on("run-004").await;
    let run = run_id("run-004");
    driver.conductor.push(&run, resumed(&run, 7)).await;
    driver
        .until(|event| matches!(event, AppEvent::Run { .. }))
        .await;
    driver.conductor.end(&run, true);
    let notice = driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Notice {
                    level: NoteLevel::Warn,
                    ..
                }
            )
        })
        .await;
    let AppEvent::Notice { text, .. } = notice else {
        unreachable!()
    };
    assert!(
        text.contains("run-004 stopped when its run host exited")
            && text.contains("/resume run-004 carries it on"),
        "{text}"
    );
    driver.until(is_mode(AppMode::Idle)).await;

    driver.send(UiCommand::Resume { run: None }).await;
    driver
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let calls = driver.conductor.calls();
    assert!(calls.contains(&Call::Resume(run.clone())), "{calls:?}");
    assert!(calls.contains(&Call::Reattach(run)), "{calls:?}");
    driver.exit().await;
}

#[tokio::test]
async fn a_waiting_run_whose_run_host_exits_is_not_crashed() {
    let mut driver = cli_on("run-002").await;
    let run = run_id("run-002");
    driver.conductor.push(&run, waiting(&run, 3, vec![])).await;
    driver
        .until(is_mode(AppMode::Waiting {
            run: run.clone(),
            reason: WaitReason::Approval,
        }))
        .await;
    driver.conductor.end(&run, true);
    let events = driver.exit().await;
    let crashed = events
        .iter()
        .any(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("its run host exited")));
    assert!(!crashed, "{events:#?}");
}

#[tokio::test]
async fn the_bottom_row_shows_the_settings_mode_with_no_run_attached() {
    let line =
        |rig: &Rig| header_and_bottom_line(&rig.ctx, "", status_parts(&rig.runs)).approval_mode;
    assert_eq!(
        line(&Rig::new()),
        ApprovalMode::Auto,
        "the built-in default"
    );

    let supervised = Rig::with(true, |global| {
        std::fs::create_dir_all(global.parent().expect("a config dir")).expect("the dir");
        std::fs::write(global, "[approvals.merge]\nmode = \"supervised\"\n").expect("the config");
    });
    assert_eq!(
        line(&supervised),
        ApprovalMode::Supervised,
        "one approval that waits for you"
    );
}

#[tokio::test]
async fn shift_tab_sets_the_mode_now_and_after_the_next_start() {
    let mut rig = Rig::new();
    let first = rig.start("First change").await;
    assert!(
        rig.approvals.calls().is_empty(),
        "settings decide until Shift+Tab"
    );

    run_commands::approval_mode(&mut rig.ctx, &mut rig.runs, ApprovalMode::Auto)
        .await
        .expect("the mode changes");
    assert_eq!(
        rig.approvals.calls(),
        [ApprovalCall::Mode {
            run: first,
            mode: ApprovalMode::Auto
        }]
    );
    assert_eq!(
        status_parts(&rig.runs).approval_mode,
        Some(ApprovalMode::Auto)
    );

    let second = rig.start("Second change").await;
    assert_eq!(
        rig.approvals.calls().last(),
        Some(&ApprovalCall::Mode {
            run: second,
            mode: ApprovalMode::Auto
        })
    );
    let starts = rig.conductor.starts();
    assert_eq!(starts.len(), 2);
    assert!(
        starts.iter().all(|start| start.approval_mode.is_none()),
        "start never takes more than supervised: {starts:?}"
    );
    assert_eq!(
        status_parts(&rig.runs).approval_mode,
        Some(ApprovalMode::Auto)
    );
}

/// Autopilot is the mode after auto in the Shift+Tab cycle and a setting too: chosen either
/// way, the bottom line says so, and every run started after Shift+Tab is raised to it.
#[tokio::test]
async fn autopilot_comes_from_shift_tab_or_the_settings() {
    let set = Rig::with(true, |global| {
        let points = [
            "understand",
            "plan",
            "merge",
            "workflow_promotion",
            "stuck",
            "limit",
            "change_request",
        ];
        let file: String = points
            .iter()
            .map(|point| format!("[approvals.{point}]\nmode = \"autopilot\"\n"))
            .collect();
        std::fs::create_dir_all(global.parent().expect("a config dir")).expect("the dir");
        std::fs::write(global, file).expect("the config");
    });
    assert_eq!(
        header_and_bottom_line(&set.ctx, "", status_parts(&set.runs)).approval_mode,
        ApprovalMode::Autopilot
    );

    let mut rig = Rig::new();
    let first = rig.start("First change").await;
    run_commands::approval_mode(&mut rig.ctx, &mut rig.runs, ApprovalMode::Autopilot)
        .await
        .expect("the mode changes");
    let second = rig.start("Second change").await;
    assert_eq!(
        rig.approvals.calls(),
        [
            ApprovalCall::Mode {
                run: first,
                mode: ApprovalMode::Autopilot
            },
            ApprovalCall::Mode {
                run: second,
                mode: ApprovalMode::Autopilot
            }
        ]
    );
    assert_eq!(
        status_parts(&rig.runs).approval_mode,
        Some(ApprovalMode::Autopilot)
    );
}

/// A run you resume after Shift+Tab chose autopilot runs in autopilot, as one started then
/// does, and the bottom line goes by the run's own mode, not this session's pick, while it shows.
#[tokio::test]
async fn a_resumed_run_takes_the_mode_shift_tab_chose() {
    let mut rig = Rig::new();
    let run = rig.start("Add --json to export").await;
    rig.until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    assert!(
        rig.conductor
            .push(&run, finished(&run, 1, RunOutcome::Stopped))
            .await
    );
    rig.until(is_mode(AppMode::Idle)).await;
    rig.conductor.end(&run, false);
    rig.settle().await;

    run_commands::approval_mode(&mut rig.ctx, &mut rig.runs, ApprovalMode::Autopilot)
        .await
        .expect("the mode changes");
    assert_eq!(rig.approvals.calls(), [], "the stopped run isn't asked");
    assert_eq!(
        status_parts(&rig.runs).approval_mode,
        Some(ApprovalMode::Autopilot),
        "with the run over, the bottom line says what the next one gets"
    );

    super::resume_and_attach(&mut rig.ctx, &mut rig.runs, &run)
        .await
        .expect("the run resumes");
    assert_eq!(
        rig.approvals.calls(),
        [ApprovalCall::Mode {
            run: run.clone(),
            mode: ApprovalMode::Autopilot
        }]
    );
    assert_eq!(
        status_parts(&rig.runs).approval_mode,
        Some(ApprovalMode::Autopilot)
    );
}

/// The bottom line shows the run on screen's own mode: one that never changed it shows your
/// settings', whatever Shift+Tab picked for runs not yet on screen.
#[tokio::test]
async fn the_bottom_row_goes_by_the_run_on_screen() {
    let mut rig = Rig::new();
    let run = rig.start("Add --json to export").await;
    rig.until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    rig.runs.approval_mode = Some(ApprovalMode::Autopilot);
    if let Some(attached) = rig.runs.attached.as_mut() {
        attached.approval_mode = None;
    }
    assert_eq!(status_parts(&rig.runs).approval_mode, None);
}

/// A failed run is said once, in its own block: no recap follows to say it again.
#[tokio::test]
async fn a_failed_run_sends_no_recap() {
    let mut driver = cli_on("run-004").await;
    let run = run_id("run-004");
    driver.conductor.push(&run, resumed(&run, 1)).await;
    driver
        .until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    driver
        .conductor
        .push(&run, finished(&run, 2, RunOutcome::Failed))
        .await;
    driver.until(is_mode(AppMode::Idle)).await;
    driver.until(|event| matches!(event, AppEvent::Exit)).await;
    let recaps = driver
        .seen()
        .iter()
        .filter(|event| matches!(event, AppEvent::Recap { .. }))
        .count();
    assert_eq!(recaps, 0, "{:#?}", driver.seen());
}

/// `argonauts run` opens the app on its run, and the app closes once that run finishes.
#[tokio::test]
async fn a_finished_run_sends_a_recap_and_closes_the_app() {
    let mut driver = cli_on("run-003").await;
    let run = run_id("run-003");
    driver.conductor.push(&run, resumed(&run, 1)).await;
    driver
        .until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    driver
        .conductor
        .push(&run, finished(&run, 2, RunOutcome::Merged))
        .await;
    let recap = driver
        .until(|event| matches!(event, AppEvent::Recap { .. }))
        .await;
    let AppEvent::Recap { text, model } = recap else {
        unreachable!()
    };
    assert!(!text.is_empty());
    assert_eq!(model, None, "built by code");
    driver.until(is_mode(AppMode::Idle)).await;
    driver.until(|event| matches!(event, AppEvent::Exit)).await;
    let crashed = driver
        .seen()
        .iter()
        .any(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("its run host exited")));
    assert!(!crashed, "{:#?}", driver.seen());
}

fn exits(effects: &[Effect]) -> bool {
    effects.iter().any(|effect| matches!(effect, Effect::Exit))
}

#[tokio::test]
async fn only_the_run_argonauts_run_opened_on_closes_the_app() {
    let mut rig = Rig::new();
    let run = rig.start("Add a flag").await;
    rig.settle().await;
    rig.conductor
        .push(&run, finished(&run, 1, RunOutcome::Merged))
        .await;
    rig.until(|event| matches!(event, AppEvent::Recap { .. }))
        .await;
    rig.settle().await;
    assert!(!exits(&rig.effects), "a run started from the conversation");

    // This one finished before the app attached, so its history holds the end.
    let done = run_id("run-009");
    let opened = open(&mut rig.ctx, &mut rig.runs, FirstView::Attach(done.clone()))
        .await
        .expect("the app opens on the run");
    rig.effects.extend(opened);
    rig.settle().await;
    rig.runs
        .attached
        .as_mut()
        .expect("attached")
        .replay_history_until(1);
    rig.conductor.push(&done, resumed(&done, 0)).await;
    rig.conductor
        .push(&done, finished(&done, 1, RunOutcome::Failed))
        .await;
    rig.until(|event| matches!(event, AppEvent::ReplayDone { seq: 1, .. }))
        .await;
    assert!(matches!(rig.effects.last(), Some(Effect::Exit)));
}

fn summary(run: &str, state: RunState, live: bool) -> RunSummary {
    RunSummary {
        run: run_id(run),
        title: format!("{run} title"),
        mode: RunMode::Crew,
        workflow: "council".to_owned(),
        state,
        stopped_at: String::new(),
        updated: OffsetDateTime::UNIX_EPOCH,
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live,
        remind: false,
        name: None,
    }
}

/// A run whose run host died, with the words the list gives it.
fn stopped(run: &str) -> RunSummary {
    RunSummary {
        stopped_at: "stopped at 3m10s when its run host exited · /resume carries it on from \
                     Review"
            .to_owned(),
        ..summary(run, RunState::Crashed, false)
    }
}

fn waiting_on_you(run: &str) -> RunSummary {
    RunSummary {
        waiting: vec![ApprovalId::numbered(1)],
        stopped_at: "question, waiting for you".to_owned(),
        ..summary(run, RunState::Waiting, false)
    }
}

fn notices(events: &[AppEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The rows of the last `BackgroundRuns`, or `None` when none was sent.
fn panel_rows(events: &[AppEvent]) -> Option<Vec<&str>> {
    events.iter().rev().find_map(|event| match event {
        AppEvent::BackgroundRuns { runs } => {
            Some(runs.iter().map(|run| run.run.as_str()).collect())
        }
        _ => None,
    })
}

/// `argonauts` opens a new conversation, as `claude` does, and its panel holds only the runs
/// started in it. Runs from earlier conversations get no rows; the header counts
/// them, /resume lists them, and a change in one is said in one line. Nothing on them is
/// resumed, watched, or answered.
#[tokio::test]
async fn a_plain_start_gives_earlier_runs_no_panel_rows() {
    let mut rig = Rig::new();
    rig.conductor.set_runs(vec![
        waiting_on_you("run-009"),
        summary("run-008", RunState::Merged, false),
        summary("run-007", RunState::Running, true),
        stopped("run-006"),
    ]);
    open(&mut rig.ctx, &mut rig.runs, FirstView::Conversation)
        .await
        .expect("the app opens");
    assert_eq!(attached(&rig.runs), None);
    assert_eq!(rig.conductor.calls(), [Call::List]);
    let events = rig.settle().await;
    assert_eq!(panel_rows(&events), None, "{events:#?}");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AppEvent::Attached { .. } | AppEvent::Mode { .. } | AppEvent::Run { .. }
        )),
        "{events:#?}"
    );
    let texts = notices(&events);
    assert!(
        texts.contains(
            &"\"run-006 title\" stopped at 3m10s when its run host exited · /resume carries \
              it on from Review"
        ),
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.contains("is on screen")),
        "{texts:?}"
    );

    rig.conductor.set_runs(vec![
        waiting_on_you("run-009"),
        summary("run-007", RunState::Merged, false),
        waiting_on_you("run-005"),
    ]);
    super::refresh_background(&rig.ctx);
    let later = rig.settle().await;
    assert_eq!(panel_rows(&later), None, "{later:#?}");
    // run-009 waited from the start, as the header said, so only what changed is said.
    assert_eq!(
        notices(&later),
        [
            "run-007 title is merged. /resume run-007 shows it.",
            "run-005 title needs you · /resume run-005"
        ]
    );
}

/// A run that isn't this conversation's, from an earlier one, another terminal, or an outside agent
/// over MCP that started it after the app opened, notifies you while you're away when it needs
/// you or ends, so a decision it waits on or a merge into your checkout is never missed. It
/// still gets no panel row, so the bottom line doesn't count it.
#[tokio::test]
async fn a_run_from_elsewhere_notifies_you_while_you_are_away() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let log = dir.path().join("conversation.log");
    let mut rig = Rig::with(true, record_conversation_to(&log));
    rig.conductor
        .set_runs(vec![summary("run-007", RunState::Running, true)]);
    open(&mut rig.ctx, &mut rig.runs, FirstView::Conversation)
        .await
        .expect("the app opens");
    rig.settle().await;
    focus(&mut rig.runs, false);

    rig.conductor.set_runs(vec![
        summary("run-007", RunState::Merged, false),
        waiting_on_you("run-012"),
    ]);
    super::refresh_background(&rig.ctx);
    let events = rig.settle().await;
    assert_eq!(panel_rows(&events), None, "{events:#?}");
    assert_eq!(
        notices(&events),
        [
            "run-007 title is merged. /resume run-007 shows it.",
            "run-012 title needs you · /resume run-012"
        ]
    );
    let lines = lines_after(&log, 2, WAIT).await;
    assert_eq!(lines.len(), 2, "{lines:?}");
    // Each notification goes out on its own, so they may land in either order.
    for said in [
        "run-007 finished (merged)",
        "run-012 waits for you at approval-001",
    ] {
        assert!(lines.iter().any(|line| line.contains(said)), "{lines:?}");
    }
}

/// A run started in another window after the app opened shows one line here and gets no panel
/// row, so the bottom line doesn't count it. It's said once: the next listing
/// knows it, and what it does later is said as for any run off screen.
#[tokio::test]
async fn a_run_started_in_another_window_shows_one_line_and_gets_no_row() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let log = dir.path().join("conversation.log");
    // Its config turns OS notifications off, so the test raises none on this desktop.
    let mut rig = Rig::with(true, record_conversation_to(&log));
    rig.conductor
        .set_runs(vec![summary("run-007", RunState::Running, true)]);
    open(&mut rig.ctx, &mut rig.runs, FirstView::Conversation)
        .await
        .expect("the app opens");
    let first = rig.settle().await;
    // The run that was going at the start is in the header's count, not a new line.
    assert!(
        !notices(&first)
            .iter()
            .any(|text| text.contains("another window")),
        "{first:#?}"
    );
    focus(&mut rig.runs, false);

    let mut named = summary("run-012", RunState::Running, true);
    named.title = "Add due dates to the tasks".to_owned();
    named.name = Some("add-due-dates".to_owned());
    rig.conductor.set_runs(vec![
        summary("run-007", RunState::Running, true),
        named.clone(),
        summary("run-013", RunState::Running, true),
    ]);
    super::refresh_background(&rig.ctx);
    let events = rig.settle().await;
    assert_eq!(panel_rows(&events), None, "{events:#?}");
    let said = notices(&events);
    assert_eq!(said.len(), 2, "{said:?}");
    assert_eq!(
        said[0],
        "A run started in another window: add-due-dates · /resume run-012 shows it"
    );
    assert!(
        said[1].starts_with("A run started in another window: ")
            && said[1].ends_with(" · /resume run-013 shows it"),
        "{said:?}"
    );

    // Said once. When it merges, that's its next line.
    super::refresh_background(&rig.ctx);
    assert!(notices(&rig.settle().await).is_empty());
    rig.conductor.set_runs(vec![
        summary("run-007", RunState::Running, true),
        RunSummary {
            state: RunState::Merged,
            live: false,
            ..named
        },
    ]);
    super::refresh_background(&rig.ctx);
    let later = rig.settle().await;
    assert_eq!(panel_rows(&later), None, "{later:#?}");
    assert_eq!(
        notices(&later),
        ["add-due-dates is merged. /resume run-012 shows it."]
    );
}

/// `argonauts -c` carries a conversation on and brings back its own runs as panel rows, leaving
/// them where they are: a run opens only from its row or /resume. Runs from other conversations
/// stay off the panel.
#[tokio::test]
async fn continuing_brings_back_the_conversations_own_runs() {
    let mut rig = Rig::new();
    rig.conductor.set_runs(vec![
        waiting_on_you("run-009"),
        summary("run-008", RunState::Merged, false),
        summary("run-007", RunState::Pausing, true),
        stopped("run-006"),
        waiting_on_you("run-005"),
    ]);
    super::adopt(&mut rig.runs, ["run-009", "run-008", "run-007"].map(run_id));
    open(&mut rig.ctx, &mut rig.runs, FirstView::Continue)
        .await
        .expect("the app opens");
    assert_eq!(attached(&rig.runs), None);
    assert_eq!(rig.conductor.calls(), [Call::List]);
    let events = rig.settle().await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AppEvent::Attached { .. } | AppEvent::Mode { .. } | AppEvent::Run { .. }
        )),
        "{events:#?}"
    );
    let texts = notices(&events);
    assert!(
        texts.contains(
            &"\"run-006 title\" stopped at 3m10s when its run host exited · /resume carries \
              it on from Review"
        ),
        "{texts:?}"
    );
    assert_eq!(panel_rows(&events), Some(vec!["run-009", "run-007"]));

    // The rows go again only when they change.
    super::refresh_background(&rig.ctx);
    let again = rig.settle().await;
    assert_eq!(panel_rows(&again), None, "{again:#?}");
    rig.conductor
        .set_runs(vec![summary("run-007", RunState::Running, true)]);
    super::refresh_background(&rig.ctx);
    let changed = rig.settle().await;
    assert_eq!(panel_rows(&changed), Some(vec!["run-007"]));
}

/// A run started now, or one you opened, is this conversation's: once another takes the screen
/// it keeps a panel row, and the conversation saves it for `argonauts -c`.
#[tokio::test]
async fn a_run_started_or_opened_here_keeps_its_row_off_screen() {
    let mut rig = Rig::new();
    rig.conductor.set_runs(vec![waiting_on_you("run-009")]);
    open(&mut rig.ctx, &mut rig.runs, FirstView::Conversation)
        .await
        .expect("the app opens");
    rig.settle().await;

    let started = rig.start("add --json to export").await;
    super::resume_and_attach(&mut rig.ctx, &mut rig.runs, &run_id("run-009"))
        .await
        .expect("run-009 opens");
    let opened: Vec<_> = super::take_opened(&mut rig.runs)
        .into_iter()
        .map(|kept| (kept.run, kept.title))
        .collect();
    assert_eq!(
        opened,
        [
            (started.clone(), "add --json to export".to_owned()),
            (run_id("run-009"), "run-009 title".to_owned())
        ]
    );
    assert!(super::take_opened(&mut rig.runs).is_empty());

    rig.conductor.set_runs(vec![
        waiting_on_you("run-009"),
        summary(started.as_str(), RunState::Running, true),
        summary("run-008", RunState::Running, true),
    ]);
    super::refresh_background(&rig.ctx);
    let events = rig.settle().await;
    assert_eq!(panel_rows(&events), Some(vec![started.as_str()]));
}

/// The header counts this repo's runs going or waiting on you in any conversation, which
/// /resume lists, but not a run waiting with nothing for you.
#[tokio::test]
async fn the_header_counts_every_run_going_or_waiting_on_you() {
    let rig = Rig::new();
    rig.conductor.set_runs(vec![
        waiting_on_you("run-009"),
        summary("run-008", RunState::Waiting, false),
        summary("run-007", RunState::Running, true),
        summary("run-006", RunState::Merged, false),
    ]);
    let counts = crate::startup::run_counts(rig.conductor.as_ref()).await;
    assert_eq!(
        counts,
        crate::startup::RunCounts {
            going: 1,
            need_you: 1,
        }
    );
}

/// One of this conversation's runs off screen that starts waiting on you, or finishes, says so
/// once in the transcript, and its row changes with it.
#[tokio::test]
async fn a_run_off_screen_says_when_it_needs_you_and_when_it_ends() {
    let mut rig = Rig::new();
    rig.conductor.set_runs(vec![
        summary("run-009", RunState::Running, true),
        summary("run-008", RunState::Running, true),
    ]);
    super::adopt(&mut rig.runs, ["run-009", "run-008"].map(run_id));
    open(&mut rig.ctx, &mut rig.runs, FirstView::Continue)
        .await
        .expect("the app opens");
    rig.settle().await;

    rig.conductor.set_runs(vec![
        waiting_on_you("run-009"),
        summary("run-008", RunState::Merged, false),
    ]);
    super::refresh_background(&rig.ctx);
    let events = rig.settle().await;
    assert_eq!(
        notices(&events),
        [
            "run-009 title needs you · ↓ to manage",
            "run-008 title is merged. /resume shows it."
        ]
    );
    assert_eq!(panel_rows(&events), Some(vec!["run-009"]));

    // Said once: the same list again says nothing.
    super::refresh_background(&rig.ctx);
    assert!(notices(&rig.settle().await).is_empty());
}

/// A run waiting on a question at start waits while you talk: what you type goes to the new
/// conversation, the run gets no panel row, and only opening it from /resume brings its question
/// back and makes it this conversation's.
#[tokio::test]
async fn a_question_waiting_at_start_waits_while_you_conversation() {
    let mut driver = TestDriver::start_with(CliOptions {
        runs: vec![waiting_on_you("run-009")],
        ..CliOptions::default()
    })
    .await;
    driver.agent.push(FakeSession::structured(
        json!({"kind": "answer", "answer_markdown": "Hi.", "estimate": null}),
    ));
    driver.submit("hi").await;
    let mode = driver
        .until(|event| matches!(event, AppEvent::Mode { .. }))
        .await;
    assert!(
        matches!(
            mode,
            AppEvent::Mode {
                mode: AppMode::Answering { .. }
            }
        ),
        "{mode:?}"
    );
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Idle
                }
            )
        })
        .await;
    let calls = driver.conductor.calls();
    assert!(
        calls.iter().all(|call| *call == Call::List),
        "the waiting run was touched: {calls:?}"
    );
    assert!(driver.approvals.calls().is_empty());
    assert!(
        !driver.seen().iter().any(|event| matches!(
            event,
            AppEvent::Attached { .. } | AppEvent::BackgroundRuns { .. }
        )),
        "{:#?}",
        driver.seen()
    );

    // Enter on it in /resume, or `/resume run-009`, opens it.
    driver
        .send(UiCommand::Resume {
            run: Some(run_id("run-009")),
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::Attached { summary, .. } if summary.run == run_id("run-009")))
        .await;
    assert!(
        driver
            .conductor
            .calls()
            .contains(&Call::Resume(run_id("run-009")))
    );
    // The conversation saves it once the step that opened it is done.
    let conversation =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(1));
    let deadline = tokio::time::Instant::now() + WAIT;
    while conversation.runs().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(conversation.runs(), [run_id("run-009")]);
    driver.exit().await;
}

/// `argonauts -c` brings back the runs the conversation started or opened as panel rows, and
/// only those.
#[tokio::test]
async fn continuing_puts_the_conversations_runs_back_in_the_panel() {
    let mut driver = TestDriver::start_with(CliOptions {
        opening: Opening {
            guided_setup: false,
            first: FirstView::Continue,
        },
        prepare: Some(Box::new(|repo| {
            let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap();
            let conversation =
                ConversationStore::open(&env.conversation_dir(), &ConversationId::numbered(1));
            conversation
                .append(&ConversationEvent::TurnStarted {
                    turn: TurnId::numbered(1),
                    text: "rename the config loader".to_owned(),
                    worker: "claude-code".parse().unwrap(),
                    read_only: true,
                    commit: "abc".to_owned(),
                    uncommitted: Vec::new(),
                    settings_sha256: None,
                    continues: false,
                })
                .unwrap();
            conversation
                .keep_run(&kept("run-009", "run-009 title"))
                .unwrap();
        })),
        runs: vec![waiting_on_you("run-009"), waiting_on_you("run-008")],
        ..CliOptions::default()
    })
    .await;
    let rows = driver
        .until(|event| matches!(event, AppEvent::BackgroundRuns { .. }))
        .await;
    assert_eq!(panel_rows(&[rows]), Some(vec!["run-009"]));
    let seen = driver.exit().await;
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, AppEvent::Attached { .. })),
        "{seen:#?}"
    );
}

/// A conversation that only started or opened runs, with `/crew` or `/resume <run>`, is carried
/// on like one with turns: `argonauts -c` picks it when it's the newest, named after its first
/// run, and brings back its runs rather than an older conversation's. `/resume` takes it by id.
#[tokio::test]
async fn continuing_carries_on_a_conversation_that_only_opened_runs() {
    let mut driver = TestDriver::start_with(CliOptions {
        opening: Opening {
            guided_setup: false,
            first: FirstView::Continue,
        },
        prepare: Some(Box::new(|repo| {
            let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap();
            let older =
                ConversationStore::open(&env.conversation_dir(), &ConversationId::numbered(1));
            older.append(&asked("an older unrelated question")).unwrap();
            older
                .keep_run(&kept("run-008", "rename the loader"))
                .unwrap();
            std::thread::sleep(Duration::from_millis(20));
            ConversationStore::open(&env.conversation_dir(), &ConversationId::numbered(2))
                .keep_run(&kept("run-009", "fix the median"))
                .unwrap();
        })),
        runs: vec![waiting_on_you("run-009"), waiting_on_you("run-008")],
        ..CliOptions::default()
    })
    .await;
    let AppEvent::ConversationResumed {
        conversation,
        title,
        turns,
        ..
    } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(conversation, ConversationId::numbered(2));
    assert_eq!(title, "fix the median");
    assert!(turns.is_empty(), "{turns:?}");
    let rows = driver
        .until(|event| matches!(event, AppEvent::BackgroundRuns { .. }))
        .await;
    assert_eq!(panel_rows(&[rows]), Some(vec!["run-009"]));

    for (conversation, title) in [
        ("conversation-001", "an older unrelated question"),
        ("conversation-002", "fix the median"),
    ] {
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Resume {
                    target: Some(ResumeTarget::Conversation(
                        ConversationId::parse(conversation).unwrap(),
                    )),
                },
            })
            .await;
        let resumed = driver
            .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
            .await;
        assert!(
            matches!(&resumed, AppEvent::ConversationResumed { conversation: id, title: named, .. }
                if id.as_str() == conversation && named == title),
            "{resumed:?}"
        );
    }
    driver.exit().await;
}

/// The panel follows the conversation on screen, the way `argonauts -c` would bring it back
///: `/resume <conversation>` shows that conversation's runs in place of the ones it left,
/// `/branch` keeps them in the copy, and `/clear` starts with none. Each conversation keeps its
/// own runs on disk.
#[tokio::test]
async fn the_panel_follows_the_conversation_on_screen() {
    let mut driver = TestDriver::start_with(CliOptions {
        opening: Opening {
            guided_setup: false,
            first: FirstView::Continue,
        },
        prepare: Some(Box::new(|repo| {
            let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap();
            let conversations = [
                (1, "rename the loader", "run-008"),
                (2, "fix the median", "run-009"),
            ];
            for (n, request, run) in conversations {
                let conversation =
                    ConversationStore::open(&env.conversation_dir(), &ConversationId::numbered(n));
                conversation.append(&asked(request)).unwrap();
                conversation.keep_run(&kept(run, request)).unwrap();
                // So conversation-002 is the newest, for -c.
                std::thread::sleep(Duration::from_millis(20));
            }
        })),
        runs: vec![waiting_on_you("run-009"), waiting_on_you("run-008")],
        ..CliOptions::default()
    })
    .await;
    let rows = driver
        .until(|event| matches!(event, AppEvent::BackgroundRuns { .. }))
        .await;
    assert_eq!(panel_rows(&[rows]), Some(vec!["run-009"]));

    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Resume {
                target: Some(ResumeTarget::Conversation(ConversationId::numbered(1))),
            },
        })
        .await;
    let rows = driver
        .until(|event| matches!(event, AppEvent::BackgroundRuns { .. }))
        .await;
    assert_eq!(panel_rows(&[rows]), Some(vec!["run-008"]));

    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Branch { name: None },
        })
        .await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. } if text.starts_with("Branched into conversation-003"))
        })
        .await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Clear,
        })
        .await;
    let rows = driver
        .until(|event| matches!(event, AppEvent::BackgroundRuns { .. }))
        .await;
    assert_eq!(panel_rows(&[rows]), Some(vec![]));

    // The copy kept run-008's row, so the panel changed only for /resume and /clear.
    let sent: Vec<Vec<&str>> = driver
        .seen()
        .iter()
        .filter_map(|event| panel_rows(std::slice::from_ref(event)))
        .collect();
    assert_eq!(sent, [vec!["run-009"], vec!["run-008"], vec![]]);
    let runs_of = |n| {
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(n)).runs()
    };
    assert_eq!(runs_of(1), [run_id("run-008")]);
    assert_eq!(runs_of(2), [run_id("run-009")]);
    assert_eq!(runs_of(3), [run_id("run-008")], "the branch keeps its runs");
    assert!(runs_of(4).is_empty(), "/clear starts with none");
    driver.exit().await;
}

fn asked(text: &str) -> ConversationEvent {
    ConversationEvent::TurnStarted {
        turn: TurnId::numbered(1),
        text: text.to_owned(),
        worker: "claude-code".parse().unwrap(),
        read_only: true,
        commit: "abc".to_owned(),
        uncommitted: Vec::new(),
        settings_sha256: None,
        continues: false,
    }
}

fn kept(run: &str, title: &str) -> ConversationRun {
    ConversationRun {
        run: run_id(run),
        title: title.to_owned(),
    }
}

#[tokio::test]
async fn narrator_lines_reach_the_screen_and_its_journal() {
    let mut rig = Rig::new();
    let run = rig.start("Tidy the docs").await;
    let run_dir = rig.ctx.env.run_dir(&run);
    std::fs::create_dir_all(&run_dir).expect("the run dir");
    let line = NarratorLine {
        ts: OffsetDateTime::UNIX_EPOCH,
        run: Some(run.clone()),
        kind: NarratorLineKind::Update,
        text: "One worker is writing tests.".to_owned(),
        model: argonauts_core::names::ModelRef::new("claude-code", None),
        spend: Spend::default(),
    };
    let update = RunUpdate::Narrator {
        run: run.clone(),
        line: line.clone(),
    };
    on_update(&mut rig.ctx, &mut rig.runs, update)
        .await
        .expect("the line applies");
    rig.until(|event| matches!(event, AppEvent::Narrator { line: seen } if *seen == line))
        .await;
    let journal = std::fs::read_to_string(run_dir.join("narrator.jsonl")).expect("the journal");
    assert_eq!(journal.lines().count(), 1);

    let stale = RunUpdate::Narrator {
        run: run_id("run-099"),
        line,
    };
    on_update(&mut rig.ctx, &mut rig.runs, stale)
        .await
        .expect("a stale line is dropped");
    let events = rig.settle().await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AppEvent::Narrator { .. })),
        "{:#?}",
        rig.seen()
    );
}

/// A waiting run that wakes, or that couldn't be woken, is named as every line about a run
/// names it: by its title, never by its number.
#[tokio::test]
async fn a_run_that_wakes_is_named_as_its_row_names_it() {
    use argonauts_core::api::ApiError;

    use crate::wake::WakeCause;

    let mut rig = Rig::new();
    rig.conductor
        .set_runs(vec![summary("run-009", RunState::Waiting, false)]);
    let woke = |started| RunUpdate::Woke {
        run: run_id("run-009"),
        cause: WakeCause::AgentBack,
        started,
    };
    on_update(&mut rig.ctx, &mut rig.runs, woke(Ok(())))
        .await
        .expect("the wake applies");
    assert_eq!(
        notices(&rig.settle().await),
        ["run-009 title woke up: the agent it waited for is tried again."]
    );

    let unanswered = ApiError::RunHost("it didn't answer".to_owned());
    on_update(&mut rig.ctx, &mut rig.runs, woke(Err(unanswered)))
        .await
        .expect("the failed wake applies");
    assert_eq!(
        notices(&rig.settle().await),
        ["Couldn't wake run-009 title: run host: it didn't answer"]
    );
}

fn task(id: &str, state: TaskState) -> TaskBrief {
    TaskBrief {
        task: TaskId::new(id).expect("a task id"),
        title: format!("{id} title"),
        state,
        worker: None,
        note: String::new(),
    }
}

/// The resume recap comes from the code-built digest alone: no model session, and no agent
/// text from the history.
#[tokio::test]
async fn resume_recap_code_built_from_digest() {
    let mut rig = Rig::new();
    let run = run_id("run-009");
    rig.conductor
        .set_runs(vec![summary("run-009", RunState::Waiting, true)]);
    let digest = StatusSummary {
        run: Some(run.clone()),
        goal: vec!["export now takes --json.".to_owned()],
        tasks: vec![
            task("task-001", TaskState::Merged),
            task("task-002", TaskState::Running),
        ],
        waiting: vec!["the merge approval".to_owned()],
        ..StatusSummary::default()
    };
    rig.conductor.set_status(digest.clone());
    open(&mut rig.ctx, &mut rig.runs, FirstView::Resume(run.clone()))
        .await
        .expect("the app opens");
    assert_eq!(attached(&rig.runs), Some(&run));
    rig.settle().await;
    rig.runs
        .attached
        .as_mut()
        .expect("attached")
        .replay_history_until(2);

    rig.conductor.push(&run, resumed(&run, 0)).await;
    rig.conductor
        .push(&run, question(&run, 1, "approval-001"))
        .await;
    rig.conductor.push(&run, waiting(&run, 2, vec![])).await;
    rig.until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    let after_history: Vec<&AppEvent> = rig
        .seen()
        .iter()
        .skip_while(|event| !matches!(event, AppEvent::CondensedReplay { .. }))
        .collect();
    let [
        AppEvent::CondensedReplay {
            recap, recap_model, ..
        },
        done,
    ] = after_history.as_slice()
    else {
        panic!("{:#?}", rig.seen());
    };
    assert_eq!(*recap, recap_from_summary(&digest, None, None));
    assert_eq!(*recap_model, None);
    assert!(!recap.contains("Which store?"), "{recap}");
    assert_eq!(
        **done,
        AppEvent::ReplayDone {
            run: run.clone(),
            seq: 2
        }
    );
    assert!(rig.conductor.calls().contains(&Call::Status(run)));
    assert!(rig.agent.specs().is_empty());
}

/// Short labels for what a run puts in the scrollback, in order.
fn scrollback(events: &[AppEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Attached { .. } => Some("header".to_owned()),
            AppEvent::Run { event } => Some(format!("event {}", event.seq)),
            AppEvent::CondensedReplay { .. } => Some("condensed replay".to_owned()),
            AppEvent::ReplayDone { seq, .. } => Some(format!("replay done at {seq}")),
            _ => None,
        })
        .collect()
}

fn is_condensed_replay(event: &AppEvent) -> bool {
    matches!(event, AppEvent::CondensedReplay { .. })
}

#[tokio::test]
async fn resume_replay_condensed_from_events() {
    let mut rig = Rig::new();
    let run = log::run_id();
    rig.conductor
        .set_runs(vec![summary(run.as_ref(), RunState::Waiting, false)]);
    super::resume_and_attach(&mut rig.ctx, &mut rig.runs, &run)
        .await
        .expect("the run resumes");
    // The fake conductor's streams open on an empty log, which has nothing to condense.
    let opened = rig.settle().await;
    assert_eq!(scrollback(&opened), ["header", "replay done at 0"]);

    // Now the stream holds history up to seq 8, as one opened on a longer log does.
    rig.runs
        .attached
        .as_mut()
        .expect("attached")
        .replay_history_until(8);
    let plan = ApprovalId::numbered(1);
    let mut history = vec![log::run_started(0, "Add --json to export", None)];
    for seq in 1..=5 {
        history.push(log::queued(seq, &format!("note {seq}"), Party::User));
    }
    history.push(log::plan_approval(6, &plan, 3));
    history.push(log::approval_decided(
        7,
        &plan,
        AnswerAction::Approve,
        Party::User,
        Some("ship it"),
    ));
    history.push(waiting(&run, 8, Vec::new()));
    let mark = rig.seen().len();
    for event in history {
        assert!(rig.conductor.push(&run, event).await);
    }
    assert!(rig.conductor.push(&run, resumed(&run, 9)).await);
    rig.until(|event| matches!(event, AppEvent::Run { event } if event.seq == 9))
        .await;
    let replayed = &rig.seen()[mark..];
    let mut order: Vec<String> = (0..=8).map(|seq| format!("event {seq}")).collect();
    order.extend(["condensed replay", "replay done at 8", "event 9"].map(str::to_owned));
    assert_eq!(scrollback(replayed), order);

    let Some(AppEvent::CondensedReplay {
        run: replay_run,
        turns,
        milestones,
        ..
    }) = replayed.iter().find(|event| is_condensed_replay(event))
    else {
        unreachable!()
    };
    assert_eq!(replay_run, &run);
    let said = ["note 1", "note 2", "note 3", "note 4", "note 5", "ship it"];
    let expected: Vec<ReplayTurn> = said
        .iter()
        .map(|text| ReplayTurn {
            from: Party::User,
            text: (*text).to_owned(),
            steps: Vec::new(),
        })
        .collect();
    assert_eq!(turns, &expected, "the last turns, word for word");
    assert_eq!(
        milestones,
        &["Plan approved · 3 tasks", "Waiting · waiting for you"]
    );

    // A run this app just started shows its own events and needs no condensed replay.
    let mut rig = Rig::new();
    let run = rig.start("Fix the flaky test").await;
    rig.settle().await;
    rig.runs
        .attached
        .as_mut()
        .expect("attached")
        .replay_history_until(1);
    rig.conductor.push(&run, resumed(&run, 0)).await;
    rig.conductor.push(&run, waiting(&run, 1, Vec::new())).await;
    rig.until(|event| matches!(event, AppEvent::ReplayDone { seq: 1, .. }))
        .await;
    assert!(
        !rig.seen().iter().any(is_condensed_replay),
        "{:#?}",
        rig.seen()
    );
}

/// A run that won't resume says why in plain words, without its id or the error's own prefix.
#[test]
fn a_run_that_wont_resume_says_why_without_its_id() {
    use argonauts_core::api::ApiError;

    let run = argonauts_core::RunId::new("run-20261004-043029-d8bb").unwrap();
    let merged = super::resume_error(ApiError::Merged(run.clone())).to_string();
    assert!(
        merged.starts_with("That run already merged, so it can't resume."),
        "{merged}"
    );
    let refused =
        super::resume_error(ApiError::Refused("the run's log can't be read".into())).to_string();
    assert_eq!(
        refused,
        "Couldn't resume that run: the run's log can't be read."
    );
    let gone = super::resume_error(ApiError::NoRun(run.clone())).to_string();
    assert!(!gone.contains(run.as_str()), "{gone}");
}

/// A run off screen that fails, or whose run host goes, is told to the conversation that
/// started it once, as a run on screen is.
#[tokio::test]
async fn a_run_off_screen_that_fails_is_heard_once() {
    for run_host_went in [false, true] {
        let mut rig = Rig::new();
        open(&mut rig.ctx, &mut rig.runs, FirstView::Conversation)
            .await
            .expect("the app opens");
        rig.settle().await;
        let first = rig.start("add due dates").await;
        rig.conductor.push(&first, resumed(&first, 1)).await;
        rig.settle().await;
        let _ = super::take_news(&mut rig.runs);
        let second = rig.start("fix the median").await;
        rig.settle().await;
        assert!(super::alongside::streams(&rig.runs, &first));
        assert_eq!(attached(&rig.runs), Some(&second));
        let _ = super::take_news(&mut rig.runs);

        if run_host_went {
            rig.conductor.end(&first, true);
        } else {
            rig.conductor
                .push(&first, finished(&first, 2, RunOutcome::Failed))
                .await;
        }
        rig.settle().await;
        let ended: Vec<(argonauts_core::RunId, RunNews)> = super::take_news(&mut rig.runs)
            .into_iter()
            .filter(|(run, _)| *run == first)
            .collect();
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert!(
            matches!(
                &ended[0].1,
                RunNews::Ended {
                    outcome: RunOutcome::Failed,
                    ..
                }
            ),
            "{ended:?}"
        );
    }
}

/// A second workflow takes the screen while the first works: the first goes on streaming into
/// the conversation beside it, and its finish still brings its recap there, while the mode stays
/// with the run on screen. The listing then doesn't say again how it ended.
#[tokio::test]
async fn two_workflows_both_stream_and_the_one_off_screen_ends_in_the_conversation() {
    let mut rig = Rig::new();
    open(&mut rig.ctx, &mut rig.runs, FirstView::Conversation)
        .await
        .expect("the app opens");
    rig.settle().await;
    let first = rig.start("add due dates").await;
    rig.conductor.push(&first, resumed(&first, 1)).await;
    rig.settle().await;
    rig.conductor
        .set_runs(vec![summary(first.as_str(), RunState::Running, true)]);
    super::refresh_background(&rig.ctx);
    rig.settle().await;

    let second = rig.start("fix the median").await;
    assert_ne!(first, second);
    let left = rig.settle().await;
    assert!(
        !left
            .iter()
            .any(|event| matches!(event, AppEvent::Detached { run } if *run == first)),
        "{left:#?}"
    );
    assert!(super::alongside::streams(&rig.runs, &first));
    assert_eq!(attached(&rig.runs), Some(&second));

    // An event the old stream sent again is shown once.
    rig.conductor.push(&first, resumed(&first, 1)).await;
    rig.conductor
        .push(&first, finished(&first, 2, RunOutcome::Merged))
        .await;
    let ended = rig.settle().await;
    let shown: Vec<u64> = ended
        .iter()
        .filter_map(|event| match event {
            AppEvent::Run { event } if event.run_id == first => Some(event.seq),
            _ => None,
        })
        .collect();
    assert_eq!(shown, [2]);
    assert!(
        ended
            .iter()
            .any(|event| matches!(event, AppEvent::Recap { text, .. } if !text.is_empty())),
        "{ended:#?}"
    );
    assert!(
        !ended.iter().any(|event| matches!(
            event,
            AppEvent::Mode {
                mode: AppMode::Idle
            }
        )),
        "the run on screen keeps the mode: {ended:#?}"
    );

    rig.conductor.end(&first, false);
    rig.settle().await;
    assert!(!super::alongside::streams(&rig.runs, &first));
    rig.conductor.set_runs(vec![
        summary(first.as_str(), RunState::Merged, false),
        summary(second.as_str(), RunState::Running, true),
    ]);
    super::refresh_background(&rig.ctx);
    let listed = rig.settle().await;
    assert!(notices(&listed).is_empty(), "{:?}", notices(&listed));
}
