use std::path::Path;
use std::process::{Child, Command};

use argonauts_core::api::{CheckpointAnswer, DiffScope, RunState, RunSummary};
use argonauts_core::app::{
    AnswerTarget, AppEvent, ChoiceKind, RepoRuns, UiCommand, ViewData, ViewRequest,
};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText,
};
use argonauts_core::events::run::{Party, RunMode, RunOutcome};
use argonauts_core::events::session::SessionStarted;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::events::{RunEvent, RunEventKind, SessionEvent};
use argonauts_core::ids::RepoId;
use argonauts_core::input::SlashCommand;
use argonauts_core::layout::run_files;
use argonauts_core::names::{CheckpointId, MessageId, RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::handoff::HandoffMode;
use argonauts_core::views::ContextRow;
use argonauts_core::{AgentEvent, CardId, NoteLevel, RunId, Spend};
use time::OffsetDateTime;

use super::cards::handoff_rule;
use super::views::asks_for_status;
use super::{guidance, mention, take_back};
use crate::FirstView;
use crate::ancestry::Processes;
use crate::client::test_support::{agent, run_started, session_started, usage, write_journal};
use crate::controller::runs::rig::{Rig, event, finished, parked, question, run_id};
use crate::controller::test_support::{ApprovalCall, Call, Harness, HarnessOptions};
use crate::controller::{Opening, ScopeRequest};
use crate::live_registry::{LiveSupervisor, register};

fn attach_to(run: &str, interactive: bool) -> HarnessOptions {
    HarnessOptions {
        interactive,
        opening: Opening {
            guided_setup: false,
            first: FirstView::Attach(run_id(run)),
        },
        ..HarnessOptions::default()
    }
}

/// Commands run in order, so once `/status` answers every earlier command was handled.
async fn barrier(harness: &mut Harness) {
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Status {
                action: Default::default(),
            },
        })
        .await;
    harness
        .until(|event| matches!(event, AppEvent::View { .. } | AppEvent::Notice { .. }))
        .await;
}

#[tokio::test]
async fn guidance_ids_are_kept_and_take_back_answers_taken_back() {
    let mut rig = Rig::new();
    let run = rig.start("Rename the flag").await;
    guidance(
        &mut rig.ctx,
        &mut rig.runs,
        run.clone(),
        "use --json".into(),
        &[],
    )
    .await
    .expect("guidance is queued");
    let target = argonauts_core::input::MentionTarget::All;
    mention(
        &mut rig.ctx,
        &mut rig.runs,
        run.clone(),
        target,
        "and docs".into(),
        &[],
    )
    .await
    .expect("the mention is queued");
    let queued: Vec<MessageId> = rig.runs.queued[&run].iter().map(|m| m.id.clone()).collect();
    assert_eq!(queued, [MessageId::numbered(1), MessageId::numbered(2)]);

    take_back(&mut rig.ctx, &mut rig.runs, run.clone(), Vec::new())
        .await
        .expect("take back");
    assert!(rig.conductor.calls().contains(&Call::TakeBack {
        run: run.clone(),
        messages: queued,
    }));
    rig.until(|event| matches!(event, AppEvent::TakenBack { .. }))
        .await;
    assert!(
        rig.runs.queued[&run].is_empty(),
        "nothing is taken back twice"
    );
}

/// Esc on a run's question reaches the engine as "carry on without me", bound to no evidence,
/// so the run never waits on it.
#[tokio::test]
async fn esc_on_a_runs_question_tells_the_engine_to_carry_on() {
    let mut harness = Harness::start().await;
    let (run, id) = (run_id("run-005"), CheckpointId::numbered(3));
    harness
        .send(UiCommand::SetAside {
            to: AnswerTarget::Checkpoint {
                run: run.clone(),
                id: id.clone(),
            },
        })
        .await;
    barrier(&mut harness).await;
    assert_eq!(
        harness.approvals.calls(),
        [ApprovalCall::Answer {
            run,
            answer: CheckpointAnswer {
                id,
                action: AnswerAction::CarryOn,
                text: None,
                evidence_sha256: String::new(),
                cards: Vec::new(),
            },
        }]
    );
}

#[tokio::test]
async fn an_answer_passes_evidence_sha256_through() {
    let mut harness = Harness::start().await;
    let (run, id) = (run_id("run-005"), CheckpointId::numbered(3));
    let evidence = "ab".repeat(32);
    let card = CardId::new("task-002").expect("a task id");
    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Checkpoint {
                run: run.clone(),
                id: id.clone(),
            },
            action: AnswerAction::RequestChanges,
            text: Some("smaller diff".to_owned()),
            evidence_sha256: Some(evidence.clone()),
            cards: vec![card.clone()],
            remember: false,
        })
        .await;
    barrier(&mut harness).await;
    assert_eq!(
        harness.approvals.calls(),
        [ApprovalCall::Answer {
            run,
            answer: CheckpointAnswer {
                id,
                action: AnswerAction::RequestChanges,
                text: Some("smaller diff".to_owned()),
                evidence_sha256: evidence,
                cards: vec![card],
            },
        }]
    );
    harness.exit().await;
}

#[tokio::test]
async fn without_approvals_nothing_is_sent() {
    let mut harness = Harness::start_with(attach_to("run-001", false)).await;
    harness
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    let commands = [
        UiCommand::ApprovalMode {
            mode: ApprovalMode::Auto,
        },
        UiCommand::Answer {
            to: AnswerTarget::Checkpoint {
                run: run_id("run-001"),
                id: CheckpointId::numbered(1),
            },
            action: AnswerAction::Approve,
            text: None,
            evidence_sha256: Some("e".repeat(64)),
            cards: Vec::new(),
            remember: false,
        },
        UiCommand::TakeOver {
            run: run_id("run-001"),
            card: CardId::new("task-001").expect("a task id"),
        },
    ];
    for command in commands {
        harness.send(command).await;
        let refusal = harness
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
        let AppEvent::Notice { text, .. } = refusal else {
            unreachable!()
        };
        assert!(text.contains("can't approve anything"), "{text}");
    }
    assert!(harness.approvals.calls().is_empty());
    harness.exit().await;
}

#[tokio::test]
async fn status_and_every_view_start_no_model_session() {
    let mut harness = Harness::start_with(attach_to("run-001", true)).await;
    harness
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    let run = run_id("run-001");
    let session = SessionKey::numbered(1);
    let card = CardId::new("task-001").expect("a task id");
    let mut commands = vec![
        UiCommand::Slash {
            command: SlashCommand::Status {
                action: Default::default(),
            },
        },
        UiCommand::Slash {
            command: SlashCommand::Context {
                all: false,
                action: Default::default(),
            },
        },
        UiCommand::Slash {
            command: SlashCommand::Ask {
                question: "Where are we?".to_owned(),
            },
        },
    ];
    let views = [
        ViewRequest::Runs,
        ViewRequest::Status,
        ViewRequest::Context,
        ViewRequest::AllRepos,
        ViewRequest::Workflow {
            run: run.clone(),
            workflow: None,
        },
        ViewRequest::Agent {
            run: run.clone(),
            session: session.clone(),
        },
        ViewRequest::Diff {
            run: run.clone(),
            card,
            scope: DiffScope::Card,
        },
        ViewRequest::Raw { run, session },
    ];
    commands.extend(views.into_iter().map(|view| UiCommand::Request { view }));
    for command in commands {
        harness.send(command).await;
        harness
            .until(|event| {
                matches!(
                    event,
                    AppEvent::View { .. } | AppEvent::Notice { .. } | AppEvent::Hint { .. }
                )
            })
            .await;
    }
    assert!(harness.agent.specs().is_empty(), "a view started a session");
    let asked_status = harness
        .conductor
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::Status(_)))
        .count();
    assert_eq!(
        asked_status, 3,
        "/status, the status view, and \"where are we\""
    );
    harness.exit().await;
}

#[tokio::test]
async fn revert_asks_first_then_sends_it_to_the_run() {
    let mut harness = Harness::start_with(attach_to("run-001", true)).await;
    harness
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    let card = CardId::new("task-002").expect("a task id");
    harness
        .send(UiCommand::Rewind {
            card: card.clone(),
            checkpoint: 1,
        })
        .await;
    let AppEvent::Choice { choice } = harness
        .until(|event| matches!(event, AppEvent::Choice { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(choice.kind, ChoiceKind::ConfirmRevert);
    assert!(
        choice
            .body
            .iter()
            .any(|line| line.contains("task-002-replaced-1"))
    );
    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice {
                id: choice.id.clone(),
            },
            action: AnswerAction::Approve,
            text: None,
            evidence_sha256: None,
            cards: Vec::new(),
            remember: false,
        })
        .await;
    let closed = harness
        .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
        .await;
    let AppEvent::ChoiceClosed { id, outcome } = closed else {
        unreachable!()
    };
    assert_eq!(id, choice.id);
    assert!(
        outcome.starts_with("Reverted task-002 to fallback-1.")
            && outcome.contains("argonauts/run-001/task-002-replaced-1"),
        "{outcome}"
    );
    assert!(harness.conductor.calls().contains(&Call::Revert {
        run: run_id("run-001"),
        card,
        checkpoint: 1,
    }));
    harness.exit().await;
}

#[tokio::test]
async fn a_spot_check_goes_to_the_run_and_needs_you_at_the_terminal() {
    let spot_check = |run: &str| UiCommand::SpotCheck {
        run: run_id(run),
        id: CheckpointId::numbered(3),
        correct: false,
    };
    let mut harness = Harness::start_with(attach_to("run-001", true)).await;
    harness
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    harness.send(spot_check("run-001")).await;
    let marked = harness
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("Marked")))
        .await;
    let AppEvent::Notice { text, .. } = marked else {
        unreachable!()
    };
    assert!(text.contains("cp-003 of run-001 wrong"), "{text}");
    assert!(harness.conductor.calls().contains(&Call::SpotCheck {
        run: run_id("run-001"),
        id: CheckpointId::numbered(3),
        correct: false,
    }));
    harness.exit().await;

    let mut unattended = Harness::start_with(attach_to("run-001", false)).await;
    unattended
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    unattended.send(spot_check("run-001")).await;
    unattended
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
    let calls = unattended.conductor.calls();
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::SpotCheck { .. })),
        "{calls:#?}"
    );
    unattended.exit().await;
}

/// `c` on an agent's goal reaches the run as its own action, and the notice says the checks
/// run beside the try and decide nothing.
#[tokio::test]
async fn running_a_goals_checks_now_goes_to_the_run() {
    use argonauts_core::events::goal::GoalAction;

    let check_now = GoalAction::CheckNow {
        task: Some(CardId::new("task-002").unwrap()),
    };
    let mut harness = Harness::start_with(attach_to("run-001", true)).await;
    harness
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    harness
        .send(UiCommand::Goal {
            run: Some(run_id("run-001")),
            action: check_now.clone(),
        })
        .await;
    let said = harness
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("checks")))
        .await;
    let AppEvent::Notice { text, .. } = said else {
        unreachable!()
    };
    assert!(
        text.starts_with("Running the goal's checks now, beside the try under way."),
        "{text}"
    );
    assert!(harness.conductor.calls().contains(&Call::Goal {
        run: run_id("run-001"),
        action: check_now,
    }));
    harness.exit().await;
}

fn handoff_opened(id: &CheckpointId, card: &CardId, stopped: &WorkerRef) -> RunEventKind {
    let plan = HandoffPlan {
        card: card.clone(),
        title: "Redis store".to_owned(),
        stopped: stopped.clone(),
        reason: "usage limit".to_owned(),
        resets_at: None,
        work: argonauts_core::DiffStat::default(),
        gates_ran: false,
        candidates: Vec::new(),
        mode: HandoffMode::Ask,
        strategy: Default::default(),
        race_count: 1,
        also_moving: Vec::new(),
    };
    let opened = CheckpointOpened {
        id: id.clone(),
        kind: CheckpointKind::Handoff,
        title: "Fallback · task-002".to_owned(),
        card: Some(card.clone()),
        packet: CheckpointPacket::Handoff(plan),
        evidence_sha256: "e".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened)))
}

#[test]
fn a_remembered_handoff_pick_asks_where_to_save_the_rule() {
    let run = run_id("run-001");
    let card = CardId::new("task-002").expect("a task id");
    let id = CheckpointId::numbered(4);
    let stopped = WorkerRef::new("claude-code", Some("opus".to_owned()));
    let pick = WorkerRef::new("codex", Some("gpt-5".to_owned()));
    let mut started = event(
        &run,
        1,
        RunEventKind::Session(SessionEvent::Started(Box::new(
            serde_json::from_value::<SessionStarted>(serde_json::json!({
                "worker": "claude-code/opus",
                "vendor": "claude-code",
                "family": "claude",
                "kind": "harness",
                "role": "reviewer",
                "step": null,
                "access": "build",
                "lifetime": "task",
                "resumed_from": null,
                "follow_key": 1,
            }))
            .expect("a session start"),
        ))),
    );
    started.card_id = Some(card.clone());
    let decided = CheckpointDecided {
        id: id.clone(),
        action: AnswerAction::Candidate {
            worker: pick.clone(),
        },
        text: None,
        by: Party::User,
        evidence_sha256: "e".repeat(64),
        shadow: None,
        cards: Vec::new(),
    };
    let events = vec![
        started,
        event(&run, 2, handoff_opened(&id, &card, &stopped)),
        event(
            &run,
            3,
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)),
        ),
    ];
    let expected = ScopeRequest {
        key: "fallback.reviewers.replace.claude-code.with".to_owned(),
        value: "[\"codex/gpt-5\"]".to_owned(),
        command: "fallback".to_owned(),
    };
    assert_eq!(handoff_rule(&events, &id, None), Some(expected.clone()));
    let other = WorkerRef::new("antigravity-cli", None);
    let answered = handoff_rule(&events, &id, Some(other)).expect("a rule");
    assert_eq!(answered.value, "[\"antigravity-cli\"]");
    assert_eq!(
        handoff_rule(&events[..1], &id, None),
        None,
        "no fallback question"
    );
    let owner = handoff_rule(&events[1..], &id, None).expect("a rule");
    assert_eq!(owner.key, "fallback.workers.replace.claude-code.with");
}

#[test]
fn where_are_we_is_a_status_question() {
    for question in [
        "Where are we?",
        "status",
        "What's the status of task-002",
        "how is it going",
    ] {
        assert!(asks_for_status(question), "{question}");
    }
    for question in [
        "Why did the reviewer reject task-002?",
        "What does the plan change?",
    ] {
        assert!(!asks_for_status(question), "{question}");
    }
}

#[tokio::test]
async fn a_handoff_answer_with_remember_saves_the_rule_where_you_say() {
    let mut harness = Harness::start_with(attach_to("run-001", true)).await;
    harness
        .until(|event| matches!(event, AppEvent::ReplayDone { .. }))
        .await;
    let run = run_id("run-001");
    let card = CardId::new("task-002").expect("a task id");
    let id = CheckpointId::numbered(4);
    let owner = SessionKey::numbered(1);
    let started = session_started(1, &owner, Some(&card), RoleName::Worker, "claude-code");
    let stopped = WorkerRef::new("claude-code", None);
    for event in [
        RunEvent {
            run_id: run.clone(),
            ..started
        },
        event(&run, 2, handoff_opened(&id, &card, &stopped)),
    ] {
        assert!(harness.conductor.push(&run, event).await);
    }
    harness
        .until(|event| matches!(event, AppEvent::Run { event } if event.seq == 2))
        .await;

    let pick = WorkerRef::new("codex", Some("gpt-5".to_owned()));
    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Checkpoint {
                run: run.clone(),
                id: id.clone(),
            },
            action: AnswerAction::Candidate {
                worker: pick.clone(),
            },
            text: None,
            evidence_sha256: Some("e".repeat(64)),
            cards: Vec::new(),
            remember: true,
        })
        .await;
    let AppEvent::Choice { choice } = harness
        .until(|event| matches!(event, AppEvent::Choice { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(choice.kind, ChoiceKind::SettingScope);
    assert_eq!(
        choice.body.first().map(String::as_str),
        Some("fallback.workers.replace.claude-code.with = [\"codex/gpt-5\"]"),
        "{choice:?}"
    );
    let sent = harness.approvals.calls();
    assert!(
        matches!(&sent[..], [ApprovalCall::Answer { answer, .. }]
            if answer.action == AnswerAction::Candidate { worker: pick.clone() }),
        "the pick goes to the engine before the rule is saved: {sent:?}"
    );

    let project = choice
        .options
        .iter()
        .find(|option| option.key == 'p')
        .expect("a project option");
    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice {
                id: choice.id.clone(),
            },
            action: project.action.clone(),
            text: None,
            evidence_sha256: None,
            cards: Vec::new(),
            remember: false,
        })
        .await;
    let changed = harness
        .until(|event| matches!(event, AppEvent::SettingChanged { .. }))
        .await;
    let AppEvent::SettingChanged { key, value, .. } = changed else {
        unreachable!()
    };
    assert_eq!(
        (key.as_str(), value.as_str()),
        (
            "fallback.workers.replace.claude-code.with",
            "[\"codex/gpt-5\"]"
        )
    );
    let text = std::fs::read_to_string(harness.env.project_config()).expect("the project file");
    let written = toml::Value::Table(text.parse().expect("the project file is TOML"));
    let with = ["fallback", "workers", "replace", "claude-code", "with"]
        .iter()
        .try_fold(&written, |node, part| node.get(part));
    assert_eq!(
        with.and_then(toml::Value::as_array).map(|picks| picks
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>()),
        Some(vec!["codex/gpt-5"]),
        "{text}"
    );
    harness.exit().await;
}

fn listed(run: &str, live: bool) -> RunSummary {
    RunSummary {
        run: run_id(run),
        title: format!("{run} title"),
        mode: RunMode::Solo,
        workflow: "solo".to_owned(),
        state: if live {
            RunState::Running
        } else {
            RunState::Parked
        },
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

/// A live session of `run`, with its context fill when `window` is known.
fn live_session(run: &RunId, seq: u64, key: &SessionKey, window: Option<u64>) -> Vec<RunEvent> {
    let card = CardId::new("task-001").expect("a task id");
    let mut events = vec![session_started(
        seq,
        key,
        Some(&card),
        RoleName::Worker,
        "claude-code",
    )];
    if let Some(size) = window {
        let fill = AgentEvent::ContextFill {
            used: size / 10,
            size: Some(size),
            sent: None,
        };
        events.push(agent(seq + 1, key, fill));
        events.push(usage(seq + 2, key, 1_000, 200, 0.25));
    }
    events
        .into_iter()
        .map(|event| RunEvent {
            run_id: run.clone(),
            ..event
        })
        .collect()
}

fn write_run(rig: &Rig, run: &RunId, events: &[RunEvent]) {
    let dir = rig.ctx.env.run_dir(run);
    std::fs::create_dir_all(&dir).expect("the run directory");
    write_journal(&dir.join(run_files::EVENTS), events);
}

async fn context_rows(rig: &mut Rig, chat: &ContextRow) -> Vec<ContextRow> {
    super::context(&mut rig.ctx, &mut rig.runs, Some(chat.clone()))
        .await
        .expect("/context");
    let AppEvent::View { data } = rig
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await
    else {
        unreachable!()
    };
    match *data {
        ViewData::Context { view } => view.rows,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn context_lists_the_chat_then_live_sessions_each_capped_by_its_window() {
    let mut rig = Rig::with(true, |global| {
        std::fs::write(global, "[sessions]\nrotate_at = 0.9\n").expect("the global config");
    });
    let run = rig.start("Rename the flag").await;
    let (owner, other_owner, starting) = (
        SessionKey::numbered(1),
        SessionKey::numbered(2),
        SessionKey::numbered(3),
    );
    for event in live_session(&run, 1, &owner, Some(200_000)) {
        assert!(rig.conductor.push(&run, event).await);
    }
    rig.settle().await;
    let other = run_id("run-002");
    let mut other_events = live_session(&other, 1, &other_owner, Some(1_000_000));
    other_events.extend(live_session(&other, 4, &starting, None));
    write_run(&rig, &other, &other_events);
    let not_live = run_id("run-003");
    write_run(
        &rig,
        &not_live,
        &live_session(&not_live, 1, &owner, Some(200_000)),
    );
    rig.conductor.set_runs(vec![
        listed("run-001", true),
        listed("run-002", true),
        listed("run-003", false),
    ]);
    let chat = ContextRow {
        run: None,
        session: SessionKey::numbered(9),
        label: "chat".to_owned(),
        worker: WorkerRef::new("claude-code", None),
        used: 3_000,
        window: Some(200_000),
        cap: None,
        tool_calls: 0,
        spent: Spend::default(),
        rotations: 0,
    };

    let rows = context_rows(&mut rig, &chat).await;
    let shown: Vec<(Option<&str>, &SessionKey, Option<u64>)> = rows
        .iter()
        .map(|row| (row.run.as_ref().map(RunId::as_str), &row.session, row.cap))
        .collect();
    assert_eq!(
        shown,
        [
            (None, &chat.session, None),
            (Some("run-001"), &owner, Some(180_000)),
            (Some("run-002"), &other_owner, Some(900_000)),
            (Some("run-002"), &starting, None),
        ],
        "the chat, the run on screen, then other live runs, each capped at 0.9 of its window"
    );
    assert_eq!(rows[0], chat, "the chat's row is shown as given");
    assert_eq!((rows[1].used, rows[1].spent.input_tokens), (20_000, 1_000));

    assert!(
        rig.conductor
            .push(&run, finished(&run, 4, RunOutcome::Merged))
            .await
    );
    rig.settle().await;
    let rows = context_rows(&mut rig, &chat).await;
    let runs: Vec<Option<&str>> = rows
        .iter()
        .map(|row| row.run.as_ref().map(RunId::as_str))
        .collect();
    assert_eq!(
        runs,
        [None, Some("run-002"), Some("run-002")],
        "a finished run has no live sessions"
    );
    assert!(rig.agent.specs().is_empty(), "/context started a session");
}

/// A child standing in for a live supervisor, killed when it drops.
struct StandIn(Child);

impl StandIn {
    fn spawn() -> Self {
        Self(
            Command::new("sleep")
                .arg("60")
                .spawn()
                .expect("sleep starts"),
        )
    }

    fn entry(&self, repo_root: &Path, run: &RunId) -> LiveSupervisor {
        let pid = self.0.id();
        LiveSupervisor {
            pid,
            start_time: Processes::new().start_time(pid).expect("the fallback runs"),
            repo_root: repo_root.to_owned(),
            run: run.clone(),
            sessions: 1,
        }
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A run in the repo at `root` that waits on an agent's question since `at`.
fn waiting_run(rig: &Rig, root: &Path, run: &str, at: OffsetDateTime) -> RunId {
    std::fs::create_dir_all(root).expect("the other repo");
    let run = run_id(run);
    let repo = RepoId::for_repo(root).expect("a repo id");
    let dir = rig.ctx.env.paths.run_dir(&repo, &run);
    std::fs::create_dir_all(&dir).expect("the run directory");
    let checkpoint = CheckpointId::numbered(1);
    let events = [
        run_started(0, "Rename the loader", None),
        question(&run, 1, checkpoint.as_str()),
        parked(&run, 2, vec![checkpoint]),
    ]
    .map(|event| RunEvent {
        run_id: run.clone(),
        ts: at,
        ..event
    });
    write_journal(&dir.join(run_files::EVENTS), &events);
    run
}

async fn all_repos(rig: &mut Rig) -> Vec<RepoRuns> {
    super::view(&mut rig.ctx, &mut rig.runs, ViewRequest::AllRepos)
        .await
        .expect("the all repos view");
    let AppEvent::View { data } = rig
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await
    else {
        unreachable!()
    };
    match *data {
        ViewData::AllRepos { repos } => repos,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn all_repos_lists_this_repo_then_live_ones_newest_first() {
    let mut rig = Rig::new();
    rig.conductor.set_runs(vec![listed("run-001", true)]);
    let here = rig.ctx.env.repo_root.clone();
    let base = here.parent().expect("the repo's parent").to_owned();
    let (older, newer) = (base.join("older"), base.join("newer"));
    let at = OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);
    let older_run = waiting_run(&rig, &older, "run-101", at);
    let newer_run = waiting_run(&rig, &newer, "run-102", at + time::Duration::hours(1));
    let mut stand_ins: Vec<StandIn> = (0..4).map(|_| StandIn::spawn()).collect();
    for entry in [
        stand_ins[0].entry(&here, &run_id("run-001")),
        stand_ins[1].entry(&older, &older_run),
        stand_ins[2].entry(&newer, &newer_run),
        stand_ins[3].entry(&base.join("moved-away"), &run_id("run-103")),
    ] {
        register(&rig.ctx.env.paths, &entry).expect("the fallback registers");
    }

    let repos = all_repos(&mut rig).await;
    let names: Vec<String> = repos.iter().map(|repos| repos.repo.clone()).collect();
    assert_eq!(
        names,
        [
            rig.ctx.env.display_repo(),
            newer.display().to_string(),
            older.display().to_string()
        ],
        "this repo once and first, a repo that moved away left out"
    );
    assert_eq!(repos[0].runs, [listed("run-001", true)]);
    for (repo, run) in [(&repos[1], &newer_run), (&repos[2], &older_run)] {
        let [summary] = &repo.runs[..] else {
            panic!("{repo:?}")
        };
        assert_eq!(&summary.run, run);
        assert_eq!(summary.waiting, [CheckpointId::numbered(1)], "{summary:?}");
    }

    // Only live supervisors are listed, so a waiting run whose supervisor exited after the
    // park grace drops out of the tab.
    drop(stand_ins.remove(2));
    let names: Vec<String> = all_repos(&mut rig)
        .await
        .into_iter()
        .map(|repos| repos.repo)
        .collect();
    assert_eq!(
        names,
        [rig.ctx.env.display_repo(), older.display().to_string()]
    );
}

#[tokio::test]
async fn rewind_lists_a_cards_checkpoints_for_the_picker() {
    use argonauts_core::events::card::CardEvent;
    let mut rig = Rig::new();
    let run = rig.start("Rename the flag").await;
    let (export, docs) = (
        CardId::new("task-001").expect("a task id"),
        CardId::new("task-002").expect("a task id"),
    );
    let tagged = |seq, card: &CardId, n| RunEvent {
        card_id: Some(card.clone()),
        ..event(
            &run,
            seq,
            RunEventKind::Card(CardEvent::CheckpointTagged {
                n,
                commit: "0123456789abcdef".to_owned(),
                from: WorkerRef::new("claude-code", None),
                to: vec![WorkerRef::new("codex", None)],
                reason: "usage limit".to_owned(),
            }),
        )
    };
    for event in [
        tagged(10, &export, 1),
        tagged(11, &docs, 1),
        tagged(12, &export, 2),
    ] {
        assert!(rig.conductor.push(&run, event).await);
    }
    rig.settle().await;

    super::checkpoints(&mut rig.ctx, &mut rig.runs, Some(export.clone()))
        .await
        .expect("/rewind task-001");
    let AppEvent::View { data } = rig
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await
    else {
        unreachable!()
    };
    let ViewData::Checkpoints {
        run: shown,
        card,
        checkpoints,
    } = *data
    else {
        panic!("/rewind answers with its picker");
    };
    assert_eq!((shown, card), (run.clone(), Some(export.clone())));
    let picked: Vec<(String, u32)> = checkpoints
        .iter()
        .map(|row| (row.card.to_string(), row.n))
        .collect();
    assert_eq!(
        picked,
        [("task-001".to_owned(), 1), ("task-001".to_owned(), 2)]
    );

    // A card with none says so instead of an empty picker.
    let none = CardId::new("task-009").expect("a task id");
    super::checkpoints(&mut rig.ctx, &mut rig.runs, Some(none))
        .await
        .expect("/rewind task-009");
    let AppEvent::Notice { text, .. } = rig
        .until(|event| matches!(event, AppEvent::Notice { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(
        text.contains("task-009 has no fallback checkpoints"),
        "{text}"
    );
}

#[tokio::test]
async fn follow_up_builds_on_a_merged_run_and_says_why_not_otherwise() {
    let mut rig = Rig::new();
    let merged = RunSummary {
        state: RunState::Merged,
        ..listed("run-001", false)
    };
    rig.conductor
        .set_runs(vec![listed("run-002", false), merged]);
    let follow_up = |run: Option<&str>| (run.map(run_id), "Add a CSV export too".to_owned());

    // A parked run can't be followed up; /resume carries it on.
    let (run, request) = follow_up(Some("run-002"));
    super::continue_run(&mut rig.ctx, &mut rig.runs, run, request)
        .await
        .expect("/follow-up run-002");
    let AppEvent::Notice { text, .. } = rig
        .until(|event| matches!(event, AppEvent::Notice { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(text.contains("run-002 isn't merged"), "{text}");

    let (run, request) = follow_up(Some("run-404"));
    super::continue_run(&mut rig.ctx, &mut rig.runs, run, request)
        .await
        .expect("/follow-up run-404");
    let AppEvent::Notice { text, .. } = rig
        .until(|event| matches!(event, AppEvent::Notice { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(text.contains("no run run-404"), "{text}");
    assert!(
        !rig.conductor
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Continue { .. }))
    );

    // With no run named, the newest merged one.
    let (run, request) = follow_up(None);
    super::continue_run(&mut rig.ctx, &mut rig.runs, run, request)
        .await
        .expect("/follow-up");
    assert!(rig.conductor.calls().iter().any(|call| matches!(
        call,
        Call::Continue { merged, .. } if *merged == merged_id()
    )));
}

fn merged_id() -> RunId {
    run_id("run-001")
}

/// A follow-up to a solo run stays solo, unless All hands is on, which sends it to the crew as
/// it does every task.
#[tokio::test]
async fn follow_up_goes_to_the_crew_while_ultracode_is_on() {
    for on in [false, true] {
        let mut rig = Rig::with(true, |global| {
            std::fs::write(global, format!("ultracode = {on}\n")).unwrap();
        });
        let merged = RunSummary {
            state: RunState::Merged,
            ..listed("run-001", false)
        };
        rig.conductor.set_runs(vec![merged]);
        super::continue_run(
            &mut rig.ctx,
            &mut rig.runs,
            None,
            "Add a CSV export too".into(),
        )
        .await
        .expect("/follow-up");
        let calls = rig.conductor.calls();
        let Some(Call::Continue { merged, request }) = calls
            .iter()
            .find(|call| matches!(call, Call::Continue { .. }))
        else {
            panic!("no follow-up started: {calls:?}");
        };
        assert_eq!(*merged, merged_id());
        let rule = request.triage.as_ref().map(|triage| triage.rule.as_str());
        if on {
            assert_eq!((request.mode, rule), (RunMode::Crew, Some("ultracode")));
        } else {
            assert_eq!((request.mode, rule), (RunMode::Solo, None));
        }
        assert!(!request.forced);
    }
}

#[tokio::test]
async fn the_workflow_view_before_a_workflow_starts_says_so_plainly() {
    let mut rig = Rig::new();
    let run = rig.start("Rename the flag").await;
    rig.settle().await;
    let request = ViewRequest::Workflow {
        run: run.clone(),
        workflow: None,
    };
    super::view(&mut rig.ctx, &mut rig.runs, request)
        .await
        .expect("the workflow view");
    // The key that asked gets its answer on the notice line, never in the transcript.
    let AppEvent::Hint { text } = rig
        .until(|event| matches!(event, AppEvent::Hint { .. } | AppEvent::Notice { .. }))
        .await
    else {
        panic!("a key's answer went into the transcript")
    };
    assert_eq!(text, "This run hasn't started a workflow yet.");
    assert!(!text.contains(run.as_str()), "{text}");
}

/// A steer that can't be sent answers text you typed, not a key, so it stays in the transcript
/// beside it rather than passing on the notice line.
#[tokio::test]
async fn a_steer_that_isnt_sent_says_so_in_the_transcript() {
    let mut rig = Rig::new();
    let run = rig.start("Rename the flag").await;
    rig.settle().await;
    let session = SessionKey::numbered(1);
    super::control::steer(
        &mut rig.ctx,
        &mut rig.runs,
        run,
        session,
        "keep the old name".to_owned(),
        false,
    )
    .await
    .expect("the steer is answered");
    let AppEvent::Notice { text, .. } = rig
        .until(|event| matches!(event, AppEvent::Hint { .. } | AppEvent::Notice { .. }))
        .await
    else {
        panic!("a typed steer's answer went to the notice line")
    };
    assert_eq!(
        text,
        "Nothing was sent, since the fake conductor doesn't steer."
    );
}

/// Refusals you meet by acting on a run that ended read as what happened and what you can do,
/// never as an internal error line.
#[test]
fn refusals_from_an_ended_run_read_in_plain_words() {
    use argonauts_core::api::ApiError;

    let run = RunId::new("run-20261004-043029-d8bb").unwrap();
    let merged = super::plain_refusal(&ApiError::Merged(run.clone())).unwrap();
    assert_eq!(
        merged,
        "That run already merged, so its agents are done. To build on its result, type \
         /follow-up and what you want next."
    );
    let gone = super::plain_refusal(&ApiError::NoRun(run.clone())).unwrap();
    assert!(!gone.contains(run.as_str()), "{gone}");
    assert_eq!(
        super::plain_refusal(&ApiError::NotNow("the run isn't running".into())).as_deref(),
        Some("The run isn't running.")
    );
    // A supervisor's own failure keeps its detail, under a plain verb at the call.
    assert_eq!(
        super::plain_refusal(&ApiError::Supervisor("socket closed".into())),
        None
    );
}

/// `/stop` on a run that merged and isn't on screen says so plainly, as `t` on its agent does,
/// never `stopping run-…: run run-… is merged`.
#[tokio::test]
async fn stopping_a_merged_run_says_so_plainly() {
    let mut rig = Rig::new();
    let run = RunId::new("run-20261004-043029-d8bb").unwrap();
    rig.conductor.merged(&run);
    super::control::stop(&mut rig.ctx, &mut rig.runs, Some(run.clone()))
        .await
        .expect("a refusal is a notice");
    let AppEvent::Notice { text, .. } = rig
        .until(|event| matches!(event, AppEvent::Notice { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(
        text,
        "That run already merged, so its agents are done. To build on its result, type \
         /follow-up and what you want next."
    );
}

/// A failure the run explains reads as what you were doing and why, without "refused:" or the
/// run's id, and a run that won't resume says why the same way.
#[test]
fn failures_read_as_one_plain_line() {
    use argonauts_core::api::ApiError;

    let refused = ApiError::Refused("task-001 has no session to steer".into());
    assert_eq!(
        super::failure(&refused, "Couldn't steer task-001"),
        "Couldn't steer task-001: task-001 has no session to steer."
    );
    let down = ApiError::Supervisor("socket closed".into());
    assert_eq!(
        super::failure(&down, "Couldn't pause the run"),
        "Couldn't pause the run: its run didn't answer (socket closed)."
    );
    let run = RunId::new("run-20261004-043029-d8bb").unwrap();
    let gone = super::failure(&ApiError::NoRun(run.clone()), "Couldn't resume that run");
    assert!(!gone.contains(run.as_str()), "{gone}");
    assert!(!gone.contains("refused:"), "{gone}");
}
