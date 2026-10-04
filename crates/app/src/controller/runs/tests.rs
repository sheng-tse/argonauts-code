use std::time::Duration;

use argonauts_core::api::{CardBrief, RunState, RunSummary, StatusSummary};
use argonauts_core::app::{AppEvent, AppMode, ReplayTurn, UiCommand};
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::{ParkReason, Party, RunMode, RunOutcome};
use argonauts_core::names::CheckpointId;
use argonauts_core::policy::{NarratorLine, NarratorLineKind};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::{CardId, NoteLevel, Spend};
use time::OffsetDateTime;

use super::rig::{Rig, finished, parked, question, resumed, run_id};
use super::{RunUpdate, attached, focus, on_update, open, status_parts};
use crate::FirstView;
use crate::client::test_support as log;
use crate::controller::recap::recap_from_summary;
use crate::controller::status_line::status_line;
use crate::controller::test_support::{ApprovalCall, Call, Harness, HarnessOptions, WAIT};
use crate::controller::{Effect, Opening, run_commands};

fn is_mode(mode: AppMode) -> impl Fn(&AppEvent) -> bool {
    move |event| matches!(event, AppEvent::Mode { mode: seen } if *seen == mode)
}

async fn harness_on(run: &str) -> Harness {
    let mut harness = Harness::start_with(HarnessOptions {
        opening: Opening {
            guided_setup: false,
            first: FirstView::Attach(run_id(run)),
        },
        ..HarnessOptions::default()
    })
    .await;
    harness
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    harness
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
    assert!(rig.conductor.push(&run, parked(&run, 2, Vec::new())).await);
    rig.until(is_mode(AppMode::Parked {
        run: run.clone(),
        reason: ParkReason::Checkpoint,
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

/// A chat command that appends its text to `log`, so a test sees what was sent.
fn record_chat_to(log: &std::path::Path) -> impl FnOnce(&std::path::Path) {
    let script = format!("printf '%s\\n' \"$0\" >> '{}'", log.display());
    move |config: &std::path::Path| {
        let text = format!(
            "[notifications]\nos = false\n\n[notifications.chat]\ncommand = [\"/bin/sh\", \"-c\", \
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
async fn replayed_checkpoints_before_replay_done_do_not_notify() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let log = dir.path().join("chat.log");
    let mut rig = Rig::with(true, record_chat_to(&log));
    let run = rig.start("Fix the flaky test").await;
    rig.settle().await;
    focus(&mut rig.runs, false);
    rig.runs
        .attached
        .as_mut()
        .expect("attached")
        .replay_history_until(2);

    rig.conductor.push(&run, resumed(&run, 0)).await;
    rig.conductor.push(&run, question(&run, 1, "cp-001")).await;
    rig.conductor.push(&run, parked(&run, 2, vec![])).await;
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
    rig.until(is_mode(AppMode::Parked {
        run: run.clone(),
        reason: ParkReason::Checkpoint,
    }))
    .await;
    rig.settle().await;
    let quiet = Duration::from_millis(300);
    assert!(
        lines_after(&log, 1, quiet).await.is_empty(),
        "history notified"
    );

    rig.conductor.push(&run, resumed(&run, 3)).await;
    rig.conductor.push(&run, question(&run, 4, "cp-002")).await;
    rig.settle().await;
    let lines = lines_after(&log, 1, WAIT).await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(&format!("{run} waits for you at cp-002")),
        "{lines:?}"
    );
    assert!(
        !lines[0].contains("Which store"),
        "agent text leaked: {lines:?}"
    );

    focus(&mut rig.runs, true);
    rig.conductor.push(&run, question(&run, 5, "cp-003")).await;
    rig.settle().await;
    assert_eq!(
        lines_after(&log, 2, quiet).await.len(),
        1,
        "notified while focused"
    );
}

#[tokio::test]
async fn a_crashed_stream_shows_the_run_as_crashed() {
    let mut harness = harness_on("run-004").await;
    let run = run_id("run-004");
    harness.conductor.push(&run, resumed(&run, 7)).await;
    harness
        .until(|event| matches!(event, AppEvent::Run { .. }))
        .await;
    harness.conductor.end(&run, true);
    let notice = harness
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
        text.contains("run-004 crashed") && text.contains("/resume run-004"),
        "{text}"
    );
    harness.until(is_mode(AppMode::Idle)).await;

    harness.send(UiCommand::Resume { run: None }).await;
    harness
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let calls = harness.conductor.calls();
    assert!(calls.contains(&Call::Resume(run.clone())), "{calls:?}");
    assert!(calls.contains(&Call::Reattach(run)), "{calls:?}");
    harness.exit().await;
}

#[tokio::test]
async fn a_parked_run_whose_supervisor_exits_is_not_crashed() {
    let mut harness = harness_on("run-002").await;
    let run = run_id("run-002");
    harness.conductor.push(&run, parked(&run, 3, vec![])).await;
    harness
        .until(is_mode(AppMode::Parked {
            run: run.clone(),
            reason: ParkReason::Checkpoint,
        }))
        .await;
    harness.conductor.end(&run, true);
    let events = harness.exit().await;
    let crashed = events
        .iter()
        .any(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("crashed")));
    assert!(!crashed, "{events:#?}");
}

#[tokio::test]
async fn the_mode_line_shows_the_settings_mode_with_no_run_attached() {
    let line = |rig: &Rig| status_line(&rig.ctx, "", status_parts(&rig.runs)).approval_mode;
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
        "one checkpoint that waits for you"
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

/// `argonauts run` opens the app on its run, and the app closes once that run finishes.
#[tokio::test]
async fn a_finished_run_sends_a_recap_and_closes_the_app() {
    let mut harness = harness_on("run-003").await;
    let run = run_id("run-003");
    harness.conductor.push(&run, resumed(&run, 1)).await;
    harness
        .until(is_mode(AppMode::Working { run: run.clone() }))
        .await;
    harness
        .conductor
        .push(&run, finished(&run, 2, RunOutcome::Merged))
        .await;
    let recap = harness
        .until(|event| matches!(event, AppEvent::Recap { .. }))
        .await;
    let AppEvent::Recap { text, model } = recap else {
        unreachable!()
    };
    assert!(!text.is_empty());
    assert_eq!(model, None, "built by code");
    harness.until(is_mode(AppMode::Idle)).await;
    harness.until(|event| matches!(event, AppEvent::Exit)).await;
    let crashed = harness
        .seen()
        .iter()
        .any(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("crashed")));
    assert!(!crashed, "{:#?}", harness.seen());
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
    assert!(!exits(&rig.effects), "a run started from the chat");

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
        preset: "peer-crew".to_owned(),
        state,
        stopped_at: String::new(),
        updated: OffsetDateTime::UNIX_EPOCH,
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live,
        remind: false,
    }
}

#[tokio::test]
async fn opening_reattaches_the_newest_going_run_and_names_the_rest() {
    let mut rig = Rig::new();
    rig.conductor.set_runs(vec![
        summary("run-009", RunState::Running, true),
        summary("run-008", RunState::Merged, false),
        summary("run-007", RunState::Pausing, true),
        summary("run-006", RunState::Crashed, false),
    ]);
    open(&mut rig.ctx, &mut rig.runs, FirstView::Chat)
        .await
        .expect("the app opens");
    assert_eq!(attached(&rig.runs), Some(&run_id("run-009")));
    assert!(
        rig.conductor
            .calls()
            .contains(&Call::Reattach(run_id("run-009")))
    );
    let events = rig.settle().await;
    let texts: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts
            .iter()
            .any(|text| text.starts_with("2 runs are going")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains("run-006 crashed")),
        "{texts:?}"
    );
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
        text: "One owner is writing tests.".to_owned(),
        model: argonauts_core::names::WorkerRef::new("claude-code", None),
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

fn card(id: &str, state: CardState) -> CardBrief {
    CardBrief {
        card: CardId::new(id).expect("a card id"),
        title: format!("{id} title"),
        state,
        owner: None,
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
        .set_runs(vec![summary("run-009", RunState::Parked, true)]);
    let digest = StatusSummary {
        run: Some(run.clone()),
        goal: vec!["export now takes --json.".to_owned()],
        cards: vec![
            card("card-001", CardState::Integrated),
            card("card-002", CardState::Running),
        ],
        waiting: vec!["the merge checkpoint".to_owned()],
        ..StatusSummary::default()
    };
    rig.conductor.set_status(digest.clone());
    open(&mut rig.ctx, &mut rig.runs, FirstView::Chat)
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
    rig.conductor.push(&run, question(&run, 1, "cp-001")).await;
    rig.conductor.push(&run, parked(&run, 2, vec![])).await;
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
        .set_runs(vec![summary(run.as_ref(), RunState::Parked, false)]);
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
    let plan = CheckpointId::numbered(1);
    let mut history = vec![log::run_started(0, "Add --json to export", None)];
    for seq in 1..=5 {
        history.push(log::queued(seq, &format!("note {seq}"), Party::User));
    }
    history.push(log::plan_checkpoint(6, &plan, 3));
    history.push(log::checkpoint_decided(
        7,
        &plan,
        AnswerAction::Approve,
        Party::User,
        Some("ship it"),
    ));
    history.push(parked(&run, 8, Vec::new()));
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
        })
        .collect();
    assert_eq!(turns, &expected, "the last turns, word for word");
    assert_eq!(
        milestones,
        &["Plan approved · 3 cards", "Parked · waiting for you"]
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
    rig.conductor.push(&run, parked(&run, 1, Vec::new())).await;
    rig.until(|event| matches!(event, AppEvent::ReplayDone { seq: 1, .. }))
        .await;
    assert!(
        !rig.seen().iter().any(is_condensed_replay),
        "{:#?}",
        rig.seen()
    );
}
