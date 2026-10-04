use argonauts_core::api::{CardBrief, RunSummary, StatusSummary};
use argonauts_core::app::{AppChoice, ChatEvent, ChoiceKind, QuotaChip, TurnReply};
use argonauts_core::events::card::{
    AttemptCause, CardBudget, CardEvent, CardKind, CardSpec, CardState, GateKind, GateOutcome,
};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, PolicyCheck, QuestionPacket,
};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DivideEvent, PlanEvent, TaskGraph};
use argonauts_core::events::run::{
    ParkReason, Party, PhaseChange, RunLifecycle, RunMode, RunOutcome, RunPhase, RunSnapshot,
    RunStarted,
};
use argonauts_core::events::session::{
    Delivery, EndReason, FileDiff, QueuedMessage, SessionEnd, SessionEvent, SessionStarted,
    SubagentKind, SubagentStatus, SubagentUpdate,
};
use argonauts_core::events::shared::{BlackboardEvent, RetroEvent};
use argonauts_core::events::vendor::{BudgetEvent, HandoffEvent, RunBudget, VendorEvent};
use argonauts_core::events::workflow::{
    StepEnd, StepFinished, StepStarted, WorkflowEvent, WorkflowStarted,
};
use argonauts_core::events::{IntegrationEvent, ReviewEvent, RunEvent, RunEventKind};
use argonauts_core::handoff::QualityDebt;
use argonauts_core::input::MentionTarget;
use argonauts_core::limits::{HealthState, QuotaReading, QuotaSource, QuotaStatus, VendorHealth};
use argonauts_core::names::{
    ChoiceId, MessageId, RoleName, TurnId, VendorId, WorkerRef, WorkflowId,
};
use argonauts_core::policy::{NarratorLineKind, NarratorStatus};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::worker::{Access, WorkerKind};
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{AgentEvent, CardId, DiffStat, Note, NoteLevel, PhaseStatus, Spend};
use crossterm::event::{KeyCode, KeyEvent};
use time::macros::datetime;
use time::{Duration, OffsetDateTime, UtcOffset};

use super::*;
use crate::app::UiAction;
use crate::bridge::status::{ChipFacts, quota_part};
use crate::bridge::{Cx, Effects};
use crate::cards::{CardTarget, Cards};
use crate::cell::Tone;
use crate::model::ViewModel;
use crate::theme::Theme;
use crate::views::settings::approvals::ApprovalsView;
use crate::views::{View, ViewCtx, ViewOutcome};

const T0: OffsetDateTime = datetime!(2026-09-30 10:00 UTC);

fn run_id() -> RunId {
    RunId::new("run-20260930-100000-a1b2").unwrap()
}

fn card_id(n: u32) -> CardId {
    CardId::new(format!("card-{n:03}")).unwrap()
}

fn key(n: u32) -> SessionKey {
    SessionKey::numbered(n)
}

fn worker(name: &str, model: &str) -> WorkerRef {
    WorkerRef::new(name, Some(model.to_owned()))
}

/// Events of the one test run, numbered as they're made.
struct Log {
    seq: u64,
}

impl Log {
    fn new() -> Self {
        Self { seq: 0 }
    }

    fn event(&mut self, kind: RunEventKind) -> AppEvent {
        self.at(None, None, kind)
    }

    fn at(
        &mut self,
        card: Option<CardId>,
        session: Option<SessionKey>,
        kind: RunEventKind,
    ) -> AppEvent {
        let seq = self.seq;
        self.seq += 1;
        AppEvent::Run {
            event: Box::new(RunEvent {
                seq,
                ts: T0 + Duration::seconds(seq as i64),
                run_id: run_id(),
                card_id: card,
                session,
                source: "conductor".into(),
                kind,
            }),
        }
    }

    fn agent(&mut self, session: u32, event: AgentEvent) -> AppEvent {
        self.at(None, Some(key(session)), RunEventKind::Agent(event))
    }

    fn session(&mut self, session: u32, card: Option<u32>, event: SessionEvent) -> AppEvent {
        self.at(
            card.map(card_id),
            Some(key(session)),
            RunEventKind::Session(event),
        )
    }

    fn card(&mut self, card: u32, event: CardEvent) -> AppEvent {
        self.at(Some(card_id(card)), None, RunEventKind::Card(event))
    }
}

fn started(title: &str, mode: RunMode) -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: title.into(),
        title: title.into(),
        mode,
        forced: false,
        preset: "peer-crew".into(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: "/repo".into(),
        base_branch: Some("main".into()),
        base_commit: "abc123".into(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: Vec::new(),
        harness_versions: Default::default(),
        version: "0.1.0".into(),
        front_end: Some("slack".into()),
        instructions: Vec::new(),
    })))
}

fn session_started(who: WorkerRef, follow: u8) -> SessionEvent {
    SessionEvent::Started(Box::new(SessionStarted {
        vendor: VendorId(who.worker.clone()),
        worker: who,
        family: "claude".into(),
        kind: WorkerKind::Harness,
        role: RoleName::Owner,
        step: None,
        access: Access::Build,
        lifetime: SessionLifetime::Card,
        resumed_from: None,
        follow_key: Some(follow),
        workflow_step: None,
        label: None,
    }))
}

fn spec(n: u32, title: &str) -> CardSpec {
    CardSpec {
        id: card_id(n),
        title: title.into(),
        task: title.into(),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        owner: Some(worker("claude-code", "opus")),
        reviewers: Vec::new(),
        budget: CardBudget {
            limit: None,
            max_minutes: None,
            max_tool_calls: None,
            inactivity_min: None,
        },
        predictions: None,
        item: None,
    }
}

fn question(id: u32, title: &str, needs_you: bool) -> CheckpointOpened {
    CheckpointOpened {
        id: CheckpointId::numbered(id),
        kind: CheckpointKind::Question,
        title: title.into(),
        card: Some(card_id(1)),
        packet: CheckpointPacket::Question(QuestionPacket {
            from: worker("codex", "gpt-5.6"),
            card: card_id(1),
            text: title.into(),
            options: Vec::new(),
        }),
        evidence_sha256: String::new(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::Answer,
        needs_you,
    }
}

fn choice(n: u32, title: &str) -> AppChoice {
    AppChoice {
        id: ChoiceId::numbered(n),
        kind: ChoiceKind::StartWork,
        title: title.into(),
        body: Vec::new(),
        options: Vec::new(),
        free_text: FreeText::None,
        bell: true,
    }
}

fn reading(vendor: &str, window: &str, used: f64, status: QuotaStatus) -> QuotaReading {
    QuotaReading {
        vendor: VendorId(vendor.into()),
        status,
        used: Some(used),
        resets_at: None,
        window: Some(window.into()),
        source: QuotaSource::RateLimitEvent,
        observed_at: T0,
        detail: None,
    }
}

fn summary() -> RunSummary {
    RunSummary {
        run: run_id(),
        title: "Fix the export race".into(),
        mode: RunMode::Crew,
        preset: "peer-crew".into(),
        state: argonauts_core::api::RunState::Running,
        stopped_at: String::new(),
        updated: T0,
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: true,
        remind: false,
        name: None,
    }
}

fn fold(events: &[AppEvent]) -> AppState {
    let mut state = AppState::default();
    for event in events {
        state.apply(event);
    }
    state
}

/// A crew run with one Claude Code session on card-001.
fn crew(log: &mut Log) -> Vec<AppEvent> {
    vec![
        log.event(started("Fix the export race", RunMode::Crew)),
        log.card(1, CardEvent::Created(Box::new(spec(1, "Middleware")))),
        log.session(
            1,
            Some(1),
            session_started(worker("claude-code", "opus"), 1),
        ),
    ]
}

fn run(state: &AppState) -> &RunState {
    &state.runs[&run_id()]
}

fn session(state: &AppState) -> &SessionState {
    &run(state).sessions[&key(1)]
}

#[test]
fn state_folds_run() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let folded = run(&state);
    assert_eq!(folded.title, "Fix the export race");
    assert_eq!(folded.mode, Some(RunMode::Crew));
    assert_eq!(folded.front_end.as_deref(), Some("slack"));
    assert_eq!(folded.preset.as_deref(), Some("peer-crew"));
    assert_eq!(folded.started, Some(T0));

    let pause = RunLifecycle::PauseRequested {
        by: Party::User,
        now: false,
    };
    state.apply(&log.event(RunEventKind::Run(pause)));
    assert!(run(&state).pausing);
    let approval = RunLifecycle::ApprovalModeChanged {
        mode: ApprovalMode::Auto,
        by: Party::User,
    };
    state.apply(&log.event(RunEventKind::Run(approval)));
    assert_eq!(run(&state).approval, Some(ApprovalMode::Auto));
    let parked = RunLifecycle::Parked {
        reason: ParkReason::Paused,
        waiting_on: Vec::new(),
    };
    state.apply(&log.event(RunEventKind::Run(parked)));
    assert!(!run(&state).pausing);
    assert_eq!(run(&state).parked, Some(ParkReason::Paused));
    assert!(!run(&state).is_active());

    let finished = RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    };
    state.apply(&log.event(RunEventKind::Run(finished)));
    assert_eq!(run(&state).finished, Some(RunOutcome::Merged));
    assert_eq!(session(&state).status, SessionStatus::Stopped);
    assert_eq!(run(&state).seq, log.seq - 1);
}

#[test]
fn state_folds_phase() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let phase = |phase, status| {
        RunEventKind::Phase(PhaseChange {
            phase,
            status,
            summary: None,
        })
    };
    state.apply(&log.event(phase(RunPhase::Build, PhaseStatus::Started)));
    assert_eq!(run(&state).phase, Some(RunPhase::Build));
    // Finishing a phase leaves it named until the next one starts.
    state.apply(&log.event(phase(RunPhase::Build, PhaseStatus::Finished)));
    assert_eq!(run(&state).phase, Some(RunPhase::Build));
}

/// Kinds the live region only needs to know arrived: they advance the run and the session's
/// stall clock and change nothing else.
fn folds_as_progress(kind: RunEventKind) {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let cards = run(&state).cards.clone();
    state.apply(&log.at(None, Some(key(1)), kind));
    assert_eq!(run(&state).seq, 3);
    assert_eq!(run(&state).updated, Some(T0 + Duration::seconds(3)));
    assert_eq!(session(&state).last_event, T0 + Duration::seconds(3));
    assert_eq!(run(&state).cards, cards);
}

#[test]
fn state_folds_goal() {
    folds_as_progress(RunEventKind::Goal(GoalEvent::Compared {
        worker: worker("codex", "gpt-5.6"),
        differences: Vec::new(),
    }));
}

#[test]
fn state_folds_plan() {
    folds_as_progress(RunEventKind::Plan(PlanEvent::FastPath {
        merged_by: worker("claude-code", "opus"),
    }));
}

#[test]
fn state_folds_retro() {
    folds_as_progress(RunEventKind::Retro(RetroEvent::Committed {
        sha: "def456".into(),
        paths: Vec::new(),
    }));
}

#[test]
fn state_folds_note() {
    folds_as_progress(RunEventKind::Note(Note {
        level: NoteLevel::Info,
        text: "main moved".into(),
    }));
}

#[test]
fn state_folds_divide() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let graph = TaskGraph {
        cards: vec![spec(2, "Tests"), spec(3, "Docs")],
        critical_path: Vec::new(),
    };
    state.apply(&log.event(RunEventKind::Divide(DivideEvent::Graph(graph))));
    assert_eq!(run(&state).cards.len(), 3);
    assert_eq!(run(&state).cards[&card_id(3)].title, "Docs");
    let owner = DivideEvent::Owner {
        card: card_id(2),
        worker: worker("codex", "gpt-5.6"),
        algorithm: argonauts_core::settings::protocol::RoutingAlgorithm::RoundRobin,
        explored: false,
        reason: String::new(),
    };
    state.apply(&log.event(RunEventKind::Divide(owner)));
    assert_eq!(
        run(&state).cards[&card_id(2)].owner,
        Some(worker("codex", "gpt-5.6"))
    );
}

#[test]
fn state_folds_card() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.apply(&log.card(
        1,
        CardEvent::State {
            from: None,
            to: CardState::Running,
            reason: String::new(),
        },
    ));
    state.apply(&log.card(
        1,
        CardEvent::Attempt {
            attempt: 2,
            worker: worker("claude-code", "opus"),
            cause: AttemptCause::Rotation,
        },
    ));
    let card = &run(&state).cards[&card_id(1)];
    assert_eq!(card.state, Some(CardState::Running));
    assert_eq!((card.attempt, card.rotations), (2, 1));
    assert_eq!(session(&state).rotations, 1);

    // A replacement stands in for the first owner until work goes back to it.
    state.apply(&log.card(
        1,
        CardEvent::Reassigned {
            from: Some(worker("claude-code", "opus")),
            to: worker("codex", "gpt-5.6"),
            by: Party::Rules,
            rule_override: None,
        },
    ));
    state.apply(&log.card(
        1,
        CardEvent::CheckpointTagged {
            n: 1,
            commit: "abc".into(),
            from: worker("claude-code", "opus"),
            to: vec![worker("codex", "gpt-5.6")],
            reason: "limit".into(),
        },
    ));
    let debt = QualityDebt {
        since: 1,
        by: worker("codex", "gpt-5.6"),
        needs: worker("claude-code", "opus"),
    };
    state.apply(&log.card(1, CardEvent::QualityDebt(debt.clone())));
    let stat = DiffStat {
        files_changed: 2,
        insertions: 10,
        deletions: 3,
    };
    state.apply(&log.card(
        1,
        CardEvent::Diff {
            stat,
            path: "d.patch".into(),
        },
    ));
    let card = &run(&state).cards[&card_id(1)];
    assert_eq!(card.standing_in_for, Some(worker("claude-code", "opus")));
    assert_eq!(card.owner, Some(worker("codex", "gpt-5.6")));
    assert_eq!(card.checkpoints, 1);
    assert_eq!(card.debt, vec![debt]);
    assert_eq!(card.diff, Some(stat));

    state.apply(&log.card(
        1,
        CardEvent::DebtCleared {
            by: worker("claude-code", "opus"),
        },
    ));
    state.apply(&log.card(
        1,
        CardEvent::Reassigned {
            from: Some(worker("codex", "gpt-5.6")),
            to: worker("claude-code", "opus"),
            by: Party::Rules,
            rule_override: None,
        },
    ));
    let card = &run(&state).cards[&card_id(1)];
    assert!(card.debt.is_empty());
    assert_eq!(card.standing_in_for, None);
}

#[test]
fn state_folds_session() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let folded = session(&state);
    assert_eq!(folded.follow, Some(1));
    assert_eq!(folded.vendor, "claude-code");
    assert_eq!(folded.card, Some(card_id(1)));
    assert_eq!(folded.status, SessionStatus::Starting);

    let message = QueuedMessage {
        id: MessageId::numbered(1),
        from: Party::User,
        to: MentionTarget::Number(1),
        text: "use the new helper".into(),
        queued_at: T0,
        delivery: Delivery::Steer,
    };
    state.apply(&log.session(1, None, SessionEvent::Queued(message)));
    assert_eq!(run(&state).queued.len(), 1);
    assert_eq!(session(&state).queued.len(), 1);
    let diff = FileDiff {
        path: "src/lib.rs".into(),
        added: 3,
        removed: 1,
        hunk: String::new(),
        truncated: false,
    };
    state.apply(&log.session(1, None, SessionEvent::FileDiff(diff.clone())));
    state.apply(&log.session(1, None, SessionEvent::FileDiff(diff)));
    assert_eq!(session(&state).diffs.len(), 1);
    state.apply(&log.session(
        1,
        None,
        SessionEvent::Delivered {
            message: MessageId::numbered(1),
            via: Delivery::Steer,
        },
    ));
    assert!(run(&state).queued.is_empty());
    assert!(session(&state).queued.is_empty());

    let end = SessionEnd {
        vendor_session: None,
        exit_code: Some(0),
        signal: None,
        duration_ms: 1_000,
        reason: EndReason::Completed,
        spend: Spend {
            cost_usd: 0.12,
            ..Spend::default()
        },
        tool_calls: 3,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log: None,
    };
    state.apply(&log.session(1, None, SessionEvent::Ended(Box::new(end))));
    let folded = session(&state);
    assert_eq!(folded.status, SessionStatus::Done);
    assert_eq!(folded.cost_usd, Some(0.12));
    assert!(folded.ended.is_some());
}

#[test]
fn a_stall_marks_the_session_until_it_is_heard_from() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let stall = AgentEvent::Error {
        category: argonauts_core::ErrorCategory::Stalled,
        message: "Claude Code has sent nothing for 45 seconds, not even its heartbeat".into(),
        retryable: true,
    };
    state.apply(&log.agent(1, stall));
    assert!(session(&state).stalled);
    let usage = AgentEvent::Usage {
        input_tokens: 10,
        output_tokens: 10,
        cached_tokens: 0,
        cost_usd: None,
        cumulative: true,
    };
    state.apply(&log.agent(1, usage));
    assert!(!session(&state).stalled);
}

#[test]
fn state_folds_agent() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let events = [
        AgentEvent::Reasoning { text: "hm".into() },
        AgentEvent::Retry {
            reason: argonauts_core::RetryReason::RateLimit,
            delay_ms: Some(12_000),
        },
        AgentEvent::Retry {
            reason: argonauts_core::RetryReason::RateLimit,
            delay_ms: Some(12_000),
        },
    ];
    for event in events {
        state.apply(&log.agent(1, event));
    }
    assert_eq!(session(&state).doing, Doing::Thinking);
    assert_eq!(session(&state).retry.as_ref().map(|r| r.attempt), Some(2));

    let events = [
        AgentEvent::ToolStarted {
            id: "t1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
            parent: None,
        },
        AgentEvent::Usage {
            input_tokens: 1_000,
            output_tokens: 200,
            cached_tokens: 0,
            cost_usd: Some(0.01),
            cumulative: false,
        },
        AgentEvent::Usage {
            input_tokens: 500,
            output_tokens: 100,
            cached_tokens: 0,
            cost_usd: Some(0.02),
            cumulative: false,
        },
        AgentEvent::ContextFill {
            used: 150_000,
            size: Some(1_000_000),
        },
    ];
    for event in events {
        state.apply(&log.agent(1, event));
    }
    let folded = session(&state);
    assert_eq!(folded.retry, None);
    assert_eq!(folded.doing, Doing::RunningTool);
    assert_eq!((folded.tokens_in, folded.tokens_out), (1_500, 300));
    assert!((folded.cost_usd.unwrap() - 0.03).abs() < 1e-9);
    assert_eq!(
        folded.context,
        Some(argonauts_core::views::ContextUse {
            used: 150_000,
            cap: 200_000
        })
    );

    let events = [
        AgentEvent::ToolFinished {
            id: "t1".into(),
            ok: true,
            output: Some("a.rs".into()),
            parent: None,
        },
        AgentEvent::TextDelta {
            text: "Done ".into(),
            parent: None,
        },
        AgentEvent::TextDelta {
            text: "here".into(),
            parent: None,
        },
        AgentEvent::Todo {
            items: vec![argonauts_core::TodoItem {
                text: "tests".into(),
                status: argonauts_core::TodoStatus::Pending,
            }],
        },
        AgentEvent::Subagent {
            update: SubagentUpdate {
                id: "sub".into(),
                parent: None,
                kind: SubagentKind::Subagent,
                label: None,
                prompt: None,
                status: SubagentStatus::Running,
                last_message: None,
                log_uri: None,
                steps_visible: true,
            },
        },
        AgentEvent::Unknown {
            raw: serde_json::json!({"type": "mystery"}),
        },
    ];
    for event in events {
        state.apply(&log.agent(1, event));
    }
    let folded = session(&state);
    assert_eq!(folded.tools[0].ok, Some(true));
    assert_eq!(folded.streaming, "Done here");
    assert_eq!(folded.doing, Doing::Writing);
    assert_eq!(folded.todos.len(), 1);
    assert_eq!(folded.subagents.len(), 1);
    assert_eq!(folded.unknown.len(), 1);
    assert_eq!(folded.status, SessionStatus::Working);

    state.apply(&log.agent(
        1,
        AgentEvent::Message {
            text: "Done here".into(),
            parent: None,
        },
    ));
    let folded = session(&state);
    assert_eq!(folded.messages, vec!["Done here".to_owned()]);
    assert!(folded.streaming.is_empty());
}

#[test]
fn state_folds_gate() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let gate = GateOutcome {
        name: "tests".into(),
        kind: GateKind::Command,
        passed: false,
        command: None,
        exit_code: Some(1),
        duration_ms: 10,
        summary: String::new(),
        output_tail: String::new(),
        violations: Vec::new(),
        sandboxed: true,
        skipped: false,
    };
    state.apply(&log.at(Some(card_id(1)), None, RunEventKind::Gate(gate)));
    assert_eq!(
        run(&state).cards[&card_id(1)].last_gate,
        Some(GateBrief {
            name: "tests".into(),
            passed: false
        })
    );
}

#[test]
fn state_folds_review() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let tests = ReviewEvent::TestsWritten {
        reviewer: worker("codex", "gpt-5.6"),
        for_card: card_id(1),
        files: Vec::new(),
    };
    state.apply(&log.at(Some(card_id(1)), None, RunEventKind::Review(tests)));
    assert_eq!(run(&state).cards[&card_id(1)].reviews, 0);
    assert_eq!(run(&state).seq, 3);
}

#[test]
fn state_folds_integration() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let merged = IntegrationEvent::Merged {
        card: card_id(1),
        sha: "abc".into(),
    };
    state.apply(&log.event(RunEventKind::Integration(merged)));
    assert_eq!(
        run(&state).cards[&card_id(1)].state,
        Some(CardState::Integrated)
    );
    assert_eq!(run(&state).cards_done(), (1, 1));
}

#[test]
fn state_folds_checkpoint() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let open = |n, needs| {
        RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(question(
            n,
            "Which cache?",
            needs,
        ))))
    };
    state.apply(&log.event(open(1, true)));
    state.apply(&log.event(open(2, false)));
    state.apply(
        &log.event(RunEventKind::Checkpoint(CheckpointEvent::SpotCheckQueued {
            id: CheckpointId::numbered(2),
        })),
    );
    assert_eq!(run(&state).checkpoints.len(), 2);
    assert!(!run(&state).checkpoints[0].replayed);
    assert_eq!(run(&state).spot_checks, vec![CheckpointId::numbered(2)]);
    // Only a checkpoint that waits on you is listed.
    assert_eq!(state.needs_you().len(), 1);

    state.apply(
        &log.event(RunEventKind::Checkpoint(CheckpointEvent::Withdrawn {
            id: CheckpointId::numbered(1),
            reason: "answered elsewhere".into(),
        })),
    );
    assert_eq!(run(&state).checkpoints.len(), 1);
    assert!(state.needs_you().is_empty());
}

#[test]
fn state_folds_handoff() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.apply(&log.event(RunEventKind::Handoff(HandoffEvent::Waiting {
        card: card_id(1),
        vendor: VendorId("claude-code".into()),
        until: Some(T0 + Duration::hours(2)),
    })));
    assert_eq!(
        run(&state).cards[&card_id(1)]
            .waiting_on_vendor
            .as_ref()
            .map(|wait| wait.vendor.as_str()),
        Some("claude-code")
    );
    state.apply(&log.event(RunEventKind::Handoff(HandoffEvent::HandedOff {
        card: card_id(1),
        to: worker("codex", "gpt-5.6"),
        mode: argonauts_core::settings::handoff::HandoffMode::Auto,
        by: Party::Rules,
        context: None,
    })));
    let card = &run(&state).cards[&card_id(1)];
    assert_eq!(card.standing_in_for, Some(worker("claude-code", "opus")));
    assert_eq!(card.owner, Some(worker("codex", "gpt-5.6")));
    assert_eq!(card.waiting_on_vendor, None);

    // A session that starts on the card afterwards stands in too.
    state.apply(&log.session(2, Some(1), session_started(worker("codex", "gpt-5.6"), 2)));
    assert_eq!(
        run(&state).sessions[&key(2)].standing_in_for,
        Some(worker("claude-code", "opus"))
    );
}

#[test]
fn state_folds_vendor() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let five = reading("claude-code", "five_hour", 0.71, QuotaStatus::Allowed);
    let week = reading("claude-code", "seven_day", 0.40, QuotaStatus::Allowed);
    state.apply(&log.event(RunEventKind::Vendor(VendorEvent::Quota(five.clone()))));
    state.apply(&log.event(RunEventKind::Vendor(VendorEvent::Quota(week))));
    let newer = QuotaReading {
        used: Some(0.75),
        observed_at: T0 + Duration::minutes(1),
        ..five
    };
    state.apply(&log.event(RunEventKind::Vendor(VendorEvent::Quota(newer.clone()))));
    assert_eq!(state.quotas["claude-code"].len(), 2);
    assert_eq!(binding_reading(&state.quotas["claude-code"]), Some(&newer));

    let health = VendorHealth {
        vendor: VendorId("codex".into()),
        state: HealthState::Down,
        since: T0,
        reason: None,
        next_test_at: None,
        disabled: None,
    };
    state.apply(&log.event(RunEventKind::Vendor(VendorEvent::Health(health))));
    state.apply(&log.event(RunEventKind::Vendor(VendorEvent::Disabled {
        vendor: VendorId("antigravity-cli".into()),
        reason: "untested version".into(),
    })));
    assert_eq!(state.health["codex"].state, HealthState::Down);
    assert_eq!(state.disabled["antigravity-cli"], "untested version");
}

#[test]
fn state_folds_budget() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let budget = RunBudget {
        spent: Spend {
            cost_usd: 0.21,
            ..Spend::default()
        },
        limit: Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 5.0,
        }),
        lines: Vec::new(),
        reserve_left: None,
    };
    state.apply(&log.event(RunEventKind::Budget(BudgetEvent::Run(budget.clone()))));
    assert_eq!(run(&state).budget, Some(budget));
    assert!((run(&state).cost_usd() - 0.21).abs() < 1e-9);
}

#[test]
fn state_folds_blackboard() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let message = QueuedMessage {
        id: MessageId::numbered(7),
        from: Party::User,
        to: MentionTarget::All,
        text: "keep the API stable".into(),
        queued_at: T0,
        delivery: Delivery::NextTurn,
    };
    state.apply(&log.session(1, None, SessionEvent::Queued(message)));
    state.apply(
        &log.event(RunEventKind::Blackboard(BlackboardEvent::Delivered {
            message: MessageId::numbered(7),
            to: key(1),
        })),
    );
    assert!(run(&state).queued.is_empty());
    assert!(session(&state).queued.is_empty());
}

#[test]
fn state_folds_workflow() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let id = WorkflowId::numbered(1);
    let workflow = Workflow {
        name: "build".into(),
        description: "Build and review".into(),
        params: Default::default(),
        phases: Vec::new(),
        based_on: None,
    };
    state.apply(
        &log.event(RunEventKind::Workflow(WorkflowEvent::Started(Box::new(
            WorkflowStarted {
                id: id.clone(),
                from: None,
                version: "1".into(),
                parent: None,
                workflow,
            },
        )))),
    );
    let step = StepRef {
        workflow: id.clone(),
        phase: 0,
        path: vec![0],
    };
    state.apply(
        &log.event(RunEventKind::Workflow(WorkflowEvent::StepStarted(
            StepStarted {
                step: step.clone(),
                kind: StepKind::Review,
                standard: StandardPhase::Check,
                round: 1,
            },
        ))),
    );
    assert_eq!(run(&state).workflows[0].steps[0].end, None);
    state.apply(
        &log.event(RunEventKind::Workflow(WorkflowEvent::StepFinished(
            StepFinished {
                step,
                kind: StepKind::Review,
                standard: StandardPhase::Check,
                round: 1,
                end: StepEnd::Done,
                summary: Some("2 findings".into()),
            },
        ))),
    );
    let folded = &run(&state).workflows[0];
    assert_eq!(folded.name, "build");
    assert_eq!(folded.steps.len(), 1);
    assert_eq!(folded.steps[0].end, Some(StepEnd::Done));
    assert_eq!(folded.steps[0].summary.as_deref(), Some("2 findings"));
}

#[test]
fn state_folds_welcome_status_and_mode() {
    let mut state = AppState::default();
    state.apply(&AppEvent::Welcome {
        version: "0.1.0".into(),
        repo: "~/argonauts".into(),
        preset: "peer-crew".into(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        default_agent: None,
        effort: None,
    });
    assert_eq!(
        state.welcome.as_ref().map(|w| w.preset.as_str()),
        Some("peer-crew")
    );

    let status = argonauts_core::app::StatusLine {
        title: "export race".into(),
        repo: "~/argonauts".into(),
        preset: "peer-crew".into(),
        all_hands: false,
        run: None,
        default_agent: worker("claude-code", "opus"),
        quotas: vec![QuotaChip {
            label: "Claude".into(),
            reading: None,
            health: HealthState::Healthy,
        }],
        spent: None,
        limit: None,
        narrator: NarratorMode::Live,
        approval_mode: ApprovalMode::Shadow,
    };
    state.apply(&AppEvent::Status {
        status: status.clone(),
    });
    assert_eq!(state.status, Some(status.clone()));
    // The header waited for this agent, and later ones don't change what it printed.
    let header_agent = |state: &AppState| state.welcome.as_ref()?.default_agent.clone();
    assert_eq!(header_agent(&state), Some(worker("claude-code", "opus")));
    state.apply(&AppEvent::Status {
        status: argonauts_core::app::StatusLine {
            default_agent: worker("codex", "gpt-5.6"),
            ..status
        },
    });
    assert_eq!(header_agent(&state), Some(worker("claude-code", "opus")));

    state.apply(&AppEvent::Mode {
        mode: AppMode::Pausing { run: run_id() },
    });
    assert!(run(&state).pausing);
    state.apply(&AppEvent::Mode {
        mode: AppMode::Parked {
            run: run_id(),
            reason: ParkReason::Checkpoint,
        },
    });
    assert!(!run(&state).pausing);
    assert_eq!(run(&state).parked, Some(ParkReason::Checkpoint));
    state.apply(&AppEvent::Mode {
        mode: AppMode::Working { run: run_id() },
    });
    assert_eq!(run(&state).parked, None);
}

#[test]
fn state_folds_chat() {
    let mut state = AppState::default();
    let turn = TurnId::numbered(1);
    let chat = |event| AppEvent::Chat { event };
    state.apply(&chat(ChatEvent::TurnStarted {
        turn: turn.clone(),
        text: "why?".into(),
        worker: worker("claude-code", "opus"),
        read_only: true,
        commit: "abc".into(),
        uncommitted: Vec::new(),
        settings_sha256: None,
        continues: false,
    }));
    let agent = |event| ChatEvent::Agent {
        turn: turn.clone(),
        event,
    };
    state.apply(&chat(agent(AgentEvent::Reasoning {
        text: String::new(),
    })));
    assert_eq!(state.chat.doing, Doing::Thinking);
    state.apply(&chat(agent(AgentEvent::TextDelta {
        text: "Because".into(),
        parent: None,
    })));
    state.apply(&chat(agent(AgentEvent::Message {
        text: "Because".into(),
        parent: None,
    })));
    state.apply(&chat(agent(AgentEvent::TextDelta {
        text: "Also".into(),
        parent: None,
    })));
    state.apply(&chat(agent(AgentEvent::Usage {
        input_tokens: 10,
        output_tokens: 3_600,
        cached_tokens: 0,
        cost_usd: None,
        cumulative: true,
    })));
    assert_eq!(state.chat.streamed, "Because\n\nAlso");
    assert_eq!(state.chat.tokens_out, 3_600);
    assert_eq!(state.chat.doing, Doing::Writing);
    // Another turn's events don't count.
    state.apply(&chat(ChatEvent::Agent {
        turn: TurnId::numbered(9),
        event: AgentEvent::Reasoning {
            text: String::new(),
        },
    }));
    assert_eq!(state.chat.doing, Doing::Writing);

    state.apply(&chat(ChatEvent::TurnFinished {
        turn: turn.clone(),
        reply: TurnReply::Answer {
            markdown: "Because".into(),
        },
        spend: Spend::default(),
        elapsed_ms: 1_000,
    }));
    assert_eq!(state.chat.turn, None);
    assert!(state.chat.streamed.is_empty());
    assert_eq!(state.chat.doing, Doing::Nothing);
}

#[test]
fn chat_queue_clears_at_turn_start() {
    let mut state = AppState::default();
    state.chat.queued = vec!["first".into(), "second".into()];
    state.apply(&AppEvent::Chat {
        event: ChatEvent::TurnStarted {
            turn: TurnId::numbered(2),
            text: "first".into(),
            worker: worker("claude-code", "opus"),
            read_only: true,
            commit: "abc".into(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
        },
    });
    assert_eq!(state.chat.queued, vec!["second".to_owned()]);
    assert_eq!(state.chat.turn, Some(TurnId::numbered(2)));
}

#[test]
fn state_folds_choice() {
    let mut state = AppState::default();
    state.apply(&AppEvent::Choice {
        choice: choice(1, "Start work?"),
    });
    assert_eq!(state.choices.len(), 1);
    state.apply(&AppEvent::ChoiceClosed {
        id: ChoiceId::numbered(1),
        outcome: "No run started.".into(),
    });
    assert!(state.choices.is_empty());
    assert!(state.needs_you().is_empty());
}

#[test]
fn state_folds_narrator() {
    let mut state = AppState::default();
    let line = argonauts_core::policy::NarratorLine {
        ts: T0,
        run: Some(run_id()),
        kind: NarratorLineKind::Update,
        text: "Codex took over card-002.".into(),
        model: worker("claude-code", "haiku"),
        spend: Spend::default(),
    };
    state.apply(&AppEvent::Narrator { line: line.clone() });
    let status = NarratorStatus {
        mode: NarratorMode::Live,
        model: worker("claude-code", "haiku"),
        spent: Spend {
            cost_usd: 0.06,
            ..Spend::default()
        },
        cap: "0.50 usd".into(),
        capped: false,
        interval_s: 45,
    };
    state.apply(&AppEvent::NarratorStatus {
        status: status.clone(),
    });
    assert_eq!(state.narrator, Some(line));
    assert_eq!(state.narrator_status, Some(status));
    assert_eq!(state.latest, Some(T0));
}

#[test]
fn state_folds_notice_and_ui_settings() {
    let mut state = AppState::default();
    let notice = || AppEvent::Notice {
        level: NoteLevel::Warn,
        text: "agy is an untested version".into(),
    };
    state.apply(&notice());
    assert!(!state.notice_repeated);
    state.apply(&notice());
    assert!(state.notice_repeated);

    let ui = UiSettings {
        narrator: NarratorMode::Live,
        ..UiSettings::default()
    };
    state.apply(&AppEvent::UiSettings {
        vim: false,
        verbosity: argonauts_core::Verbosity::Full,
        ui: Some(Box::new(ui.clone())),
    });
    assert_eq!(state.ui, Some(ui));
}

#[test]
fn state_folds_detached_and_taken_back() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.attached = Some(run_id());
    let message = QueuedMessage {
        id: MessageId::numbered(3),
        from: Party::User,
        to: MentionTarget::All,
        text: "keep it small".into(),
        queued_at: T0,
        delivery: Delivery::NextTurn,
    };
    state.apply(&log.session(1, None, SessionEvent::Queued(message)));
    state.chat.queued.push("later".into());
    state.apply(&AppEvent::TakenBack {
        texts: vec!["keep it small".into(), "later".into()],
    });
    assert!(run(&state).queued.is_empty());
    assert!(state.chat.queued.is_empty());

    state.apply(&AppEvent::Detached { run: run_id() });
    assert_eq!(state.attached, None);
    assert!(state.runs.contains_key(&run_id()));
}

#[test]
fn replay_starts_at_attached_and_ends_at_replay_done() {
    let mut state = AppState::default();
    state.apply(&AppEvent::Attached {
        summary: summary(),
        started_here: false,
    });
    assert!(state.replaying());
    assert_eq!(state.attached, Some(run_id()));
    assert_eq!(run(&state).title, "Fix the export race");

    let mut log = Log::new();
    state.apply(
        &log.event(RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(
            question(1, "Plan ready", true),
        )))),
    );
    assert!(run(&state).checkpoints[0].replayed);

    state.apply(&AppEvent::ReplayDone {
        run: run_id(),
        seq: 0,
    });
    assert!(!state.replaying());
    state.apply(
        &log.event(RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(
            question(2, "Merge?", true),
        )))),
    );
    assert!(!run(&state).checkpoints[1].replayed);
}

#[test]
fn snapshot_seeds_long_run() {
    let mut state = AppState::default();
    state.apply(&AppEvent::Attached {
        summary: summary(),
        started_here: false,
    });
    let snapshot = RunSnapshot {
        upto: 5_000,
        engine: serde_json::Value::Null,
        summary: StatusSummary {
            run: Some(run_id()),
            goal: Vec::new(),
            plan: None,
            decisions: Vec::new(),
            cards: vec![CardBrief {
                card: card_id(4),
                title: "Retry queue".into(),
                state: CardState::Running,
                owner: Some(worker("codex", "gpt-5.6")),
                note: String::new(),
            }],
            recent: Vec::new(),
            budget: Some(RunBudget {
                spent: Spend {
                    cost_usd: 3.1,
                    ..Spend::default()
                },
                limit: None,
                lines: Vec::new(),
                reserve_left: None,
            }),
            quotas: Vec::new(),
            waiting: Vec::new(),
        },
        open_checkpoints: vec![question(3, "Merge card-004?", true)],
        running: vec![key(8)],
    };
    let mut log = Log::new();
    log.seq = 5_000;
    state.apply(
        &log.event(RunEventKind::Run(RunLifecycle::Snapshot(Box::new(
            snapshot,
        )))),
    );
    let folded = run(&state);
    assert_eq!(folded.cards[&card_id(4)].state, Some(CardState::Running));
    assert!((folded.cost_usd() - 3.1).abs() < 1e-9);
    assert!(folded.checkpoints[0].replayed);
    assert_eq!(folded.running_sessions().count(), 1);
    assert!(folded.summary.is_some());
    assert_eq!(state.needs_you().len(), 1);
}

#[test]
fn needs_you_lists_cards_oldest_first() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.apply(
        &log.event(RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(
            question(1, "Which cache?", true),
        )))),
    );
    state.apply(&AppEvent::Choice {
        choice: choice(1, "Start work?"),
    });
    state.apply(
        &log.event(RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(
            question(2, "Merge?", true),
        )))),
    );
    state.set_aside(&run_id(), &CheckpointId::numbered(1), true);
    let titles: Vec<(String, bool)> = state
        .needs_you()
        .into_iter()
        .map(|needs| match needs {
            NeedsYou::Checkpoint {
                title, set_aside, ..
            } => (title, set_aside),
            NeedsYou::Choice { title, .. } => (title, false),
        })
        .collect();
    assert_eq!(
        titles,
        vec![
            ("Which cache?".to_owned(), true),
            ("Start work?".to_owned(), false),
            ("Merge?".to_owned(), false),
        ]
    );
}

#[test]
fn follow_finds_the_attached_runs_agent() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    assert!(state.follow(1).is_none(), "no run is attached yet");
    state.attached = Some(run_id());
    let (id, folded) = state.follow(1).unwrap();
    assert_eq!(*id, run_id());
    assert_eq!(folded.key, key(1));
    assert!(state.follow(2).is_none());
}

#[test]
fn quota_chip_words() {
    let now = T0 + Duration::minutes(1);
    let facts = ChipFacts {
        now,
        utc_offset: UtcOffset::UTC,
        spend_usd: None,
    };
    let chip = |label: &str, reading: Option<QuotaReading>, health| QuotaChip {
        label: label.into(),
        reading,
        health,
    };
    let words = |chip: &QuotaChip, facts: &ChipFacts| {
        let part = quota_part(chip, facts).expect("a chip");
        (part.text, part.tone)
    };
    let healthy = HealthState::Healthy;
    let claude = reading("claude-code", "five_hour", 0.71, QuotaStatus::Allowed);
    assert_eq!(
        words(&chip("Claude", Some(claude.clone()), healthy), &facts),
        ("Claude 5h 71% used".into(), Tone::Normal)
    );
    let near = QuotaReading {
        used: Some(0.92),
        ..claude.clone()
    };
    let part = quota_part(&chip("Claude", Some(near), healthy), &facts).unwrap();
    assert_eq!(
        (part.text.as_str(), part.tone, part.rank),
        ("Claude 5h 92% used", Tone::Warning, crate::text::KEEP)
    );
    assert_eq!(
        words(&chip("Codex", None, HealthState::Down), &facts),
        ("Codex down".into(), Tone::Warning)
    );
    let out = QuotaReading {
        status: QuotaStatus::Rejected,
        resets_at: Some(datetime!(2026-09-30 15:45 UTC)),
        ..reading("antigravity-cli", "daily", 1.0, QuotaStatus::Rejected)
    };
    assert_eq!(
        words(&chip("agy", Some(out.clone()), healthy), &facts),
        ("agy out until 3:45 PM".into(), Tone::Warning)
    );
    let tomorrow = QuotaReading {
        resets_at: Some(datetime!(2026-10-01 09:00 UTC)),
        ..out.clone()
    };
    assert_eq!(
        words(&chip("agy", Some(tomorrow), healthy), &facts).0,
        "agy out until Thu 9:00 AM"
    );
    let no_reset = QuotaReading {
        resets_at: None,
        ..out
    };
    assert_eq!(
        words(&chip("agy", Some(no_reset), healthy), &facts).0,
        "agy out"
    );
    // A quota not read yet leaves its chip out rather than show `Codex ?`.
    assert_eq!(quota_part(&chip("Codex", None, healthy), &facts), None);
    let unreadable = QuotaReading {
        used: None,
        ..reading("codex", "five_hour", 0.0, QuotaStatus::Unknown)
    };
    assert_eq!(
        quota_part(&chip("Codex", Some(unreadable), healthy), &facts),
        None
    );
    let spent = ChipFacts {
        spend_usd: Some(0.06),
        ..facts
    };
    assert_eq!(
        words(&chip("OpenRouter", None, healthy), &spent).0,
        "OpenRouter $0.06"
    );
    assert_eq!(words(&chip("Local", None, healthy), &spent).0, "Local");
    let stale = ChipFacts {
        now: T0 + Duration::hours(1),
        ..facts
    };
    assert_eq!(
        words(&chip("Claude", Some(claude), healthy), &stale),
        ("Claude 5h 71% used".into(), Tone::Dim)
    );
}

#[test]
fn binding_ignores_a_windows_older_reading() {
    // The window reset since the older reading; the fresh one binds even though it's lower.
    let old = reading("claude-code", "five_hour", 0.95, QuotaStatus::Allowed);
    let fresh = QuotaReading {
        used: Some(0.10),
        observed_at: T0 + Duration::hours(5),
        ..old.clone()
    };
    let week = reading("claude-code", "seven_day", 0.40, QuotaStatus::Allowed);
    let readings = [old, fresh, week.clone()];
    assert_eq!(binding_reading(&readings), Some(&week));
}

fn checkpoint_event(log: &mut Log, event: CheckpointEvent) -> AppEvent {
    log.event(RunEventKind::Checkpoint(event))
}

/// An auto call your rules made on checkpoint `n`, queued for a spot check.
fn auto_call(log: &mut Log, n: u32, title: &str, spot_check: bool) -> Vec<AppEvent> {
    let id = CheckpointId::numbered(n);
    let mut events = vec![
        checkpoint_event(
            log,
            CheckpointEvent::Opened(Box::new(question(n, title, false))),
        ),
        checkpoint_event(
            log,
            CheckpointEvent::Checked(PolicyCheck {
                id: id.clone(),
                violations: Vec::new(),
                verdict: None,
                by: Some(DecideWith::Rules),
                approver: None,
                reasons: vec!["It matches your rules".into()],
                shadow: false,
            }),
        ),
        checkpoint_event(
            log,
            CheckpointEvent::Decided(CheckpointDecided {
                id: id.clone(),
                action: AnswerAction::Approve,
                text: None,
                by: Party::Rules,
                evidence_sha256: String::new(),
                shadow: None,
                cards: Vec::new(),
            }),
        ),
    ];
    if spot_check {
        events.push(checkpoint_event(
            log,
            CheckpointEvent::SpotCheckQueued { id },
        ));
    }
    events
}

#[test]
fn spot_check_keeps_its_call_until_you_mark_it() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    for event in auto_call(&mut log, 4, "Which cache?", true) {
        state.apply(&event);
    }
    let id = CheckpointId::numbered(4);
    assert!(run(&state).checkpoints.is_empty());
    assert_eq!(run(&state).spot_checks, vec![id.clone()]);
    let record = run(&state).decision(&id).expect("the call it asks about");
    assert_eq!(record.opened.title, "Which cache?");
    assert_eq!(
        record.decided.as_ref().map(|decided| &decided.by),
        Some(&Party::Rules)
    );
    assert_eq!(
        record.check.as_ref().map(|check| check.reasons.clone()),
        Some(vec!["It matches your rules".to_owned()])
    );

    state.sent(&UiCommand::SpotCheck {
        run: run_id(),
        id: id.clone(),
        correct: false,
    });
    assert!(run(&state).spot_checks.is_empty());
    assert!(run(&state).decision(&id).is_none());
}

#[test]
fn a_waiting_spot_check_outlasts_newer_decisions() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    for n in 1..=50 {
        for event in auto_call(&mut log, n, "Which cache?", n == 1) {
            state.apply(&event);
        }
    }
    let kept = |n| run(&state).decision(&CheckpointId::numbered(n)).is_some();
    assert!(kept(1), "it waits for you");
    assert!(!kept(2), "an old call nobody asked about is dropped");
    assert!(kept(50));
}

/// The bridge folds an event into the state and then hands it to the cards.
fn cards_see(cards: &mut Cards, model: &mut ViewModel, state: &AppState, event: &AppEvent) {
    let settings = UiSettings::default();
    let mut out = Effects::default();
    let mut cx = Cx {
        model,
        state,
        settings: &settings,
        now: std::time::Instant::now(),
        out: &mut out,
    };
    cards.on_event(event, &mut cx);
}

/// Then, as the bridge does, the state learns what went out.
fn cards_act(
    cards: &mut Cards,
    model: &mut ViewModel,
    state: &mut AppState,
    action: &UiAction,
) -> Vec<UiCommand> {
    let settings = UiSettings::default();
    let mut out = Effects::default();
    {
        let mut cx = Cx {
            model,
            state,
            settings: &settings,
            now: std::time::Instant::now(),
            out: &mut out,
        };
        cards.on_action(action, &mut cx);
    }
    if let UiAction::Command(command) = action {
        out.send(command.clone());
    }
    for command in &out.commands {
        state.sent(command);
    }
    out.commands
}

fn approvals_text(view: &mut ApprovalsView, state: &AppState) -> String {
    let theme = Theme::dark();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: std::time::Instant::now(),
    };
    view.render(area, &mut buf, &ctx);
    crate::stability::buffer_text(&buf)
}

fn approvals_key(view: &mut ApprovalsView, state: &AppState, key: char) -> ViewOutcome {
    let theme = Theme::dark();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: std::time::Instant::now(),
    };
    view.handle_key(KeyEvent::from(KeyCode::Char(key)), &ctx)
}

fn spot_cards(cards: &Cards) -> Vec<u64> {
    cards
        .open
        .iter()
        .filter(|(_, card)| matches!(card.target, CardTarget::SpotCheck { .. }))
        .map(|(id, _)| *id)
        .collect()
}

#[test]
fn a_spot_check_goes_out_once() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let mut cards = Cards::default();
    let mut model = ViewModel::default();
    let mut events = auto_call(&mut log, 5, "Which cache?", true);
    events.extend(auto_call(&mut log, 6, "Which queue?", true));
    for event in &events {
        state.apply(event);
        cards_see(&mut cards, &mut model, &state, event);
    }
    assert_eq!(spot_cards(&cards).len(), 2);

    let mut approvals = ApprovalsView::new(Default::default());
    let shown = approvals_text(&mut approvals, &state);
    assert!(shown.contains("Which cache?"), "{shown}");
    assert!(
        shown.contains("· decided by"),
        "each row shows its call: {shown}"
    );

    // Marked in `/approvals`: its card closes and the state lets it go.
    let ViewOutcome::Send(command) = approvals_key(&mut approvals, &state, 'y') else {
        panic!("y sends the mark");
    };
    let sent = cards_act(
        &mut cards,
        &mut model,
        &mut state,
        &UiAction::Command(command),
    );
    assert_eq!(
        sent,
        vec![UiCommand::SpotCheck {
            run: run_id(),
            id: CheckpointId::numbered(5),
            correct: true,
        }]
    );
    assert_eq!(spot_cards(&cards).len(), 1);
    assert_eq!(run(&state).spot_checks, vec![CheckpointId::numbered(6)]);
    assert!(matches!(
        approvals_key(&mut approvals, &state, 'n'),
        ViewOutcome::Notice(_)
    ));

    // Answered on its card: the open view drops the row, and a new one lists nothing.
    let card = spot_cards(&cards)[0];
    let sent = cards_act(
        &mut cards,
        &mut model,
        &mut state,
        &UiAction::Answer {
            card,
            option: 0,
            note: None,
            checked: Vec::new(),
        },
    );
    assert_eq!(sent.len(), 1);
    assert!(run(&state).spot_checks.is_empty());
    let shown = approvals_text(&mut approvals, &state);
    assert!(shown.contains("marked right"), "{shown}");
    assert!(!shown.contains("Which queue?"), "{shown}");
    let shown = approvals_text(&mut ApprovalsView::new(Default::default()), &state);
    assert!(shown.contains("None waiting"), "{shown}");
}

/// A session that names no model runs the one its CLI's settings give, and keeps that name
/// when it reports its model; one with neither takes the model it reports.
#[test]
fn a_session_is_named_by_the_model_it_runs() {
    let mut log = Log::new();
    let mut state = AppState::default();
    state.apply(&AppEvent::Names {
        names: Box::new(DisplayNames {
            configured: BTreeMap::from([("codex".to_owned(), "gpt-5.6-luna".to_owned())]),
            ..DisplayNames::default()
        }),
    });
    state.apply(&log.event(started("Fix the export race", RunMode::Crew)));
    let bare = |name: &str| WorkerRef::new(name, None);
    state.apply(&log.session(1, None, session_started(bare("codex"), 1)));
    state.apply(&log.session(2, None, session_started(bare("claude-code"), 2)));
    let init = |model: &str| AgentEvent::Init {
        session_id: None,
        model: Some(model.to_owned()),
        tools: Vec::new(),
    };
    state.apply(&log.agent(1, init("gpt-5.6")));
    state.apply(&log.agent(2, init("claude-haiku-4-5")));
    let worker = |n: u32| run(&state).sessions[&key(n)].worker.to_string();
    assert_eq!(worker(1), "codex/gpt-5.6-luna");
    assert_eq!(worker(2), "claude-code/claude-haiku-4-5");
}

/// A session that names no model is named by its CLI's default from its start, as the header
/// is, until it reports the model it runs.
#[test]
fn a_session_is_named_by_its_clis_default_until_it_reports() {
    let mut log = Log::new();
    let mut state = AppState::default();
    state.apply(&AppEvent::Names {
        names: Box::new(DisplayNames {
            defaults: BTreeMap::from([("claude-code".to_owned(), "claude-opus-5-5".to_owned())]),
            ..DisplayNames::default()
        }),
    });
    state.apply(&log.event(started("Fix the export race", RunMode::Crew)));
    let bare = |name: &str| WorkerRef::new(name, None);
    state.apply(&log.session(1, None, session_started(bare("claude-code"), 1)));
    state.apply(&log.session(2, None, session_started(bare("claude-code"), 2)));
    let worker = |state: &AppState, n: u32| run(state).sessions[&key(n)].worker.to_string();
    assert_eq!(worker(&state, 1), "claude-code/claude-opus-5-5");
    let init = |model: &str| AgentEvent::Init {
        session_id: None,
        model: Some(model.to_owned()),
        tools: Vec::new(),
    };
    state.apply(&log.agent(1, init("claude-opus-5-5")));
    state.apply(&log.agent(2, init("claude-haiku-4-5")));
    assert_eq!(worker(&state, 1), "claude-code/claude-opus-5-5");
    // The catalog guessed wrong, and the session's own word wins.
    assert_eq!(worker(&state, 2), "claude-code/claude-haiku-4-5");
}

/// The run keeps when it ended and what its merge card put up, for the lines that come after.
#[test]
fn a_run_keeps_its_end_and_its_merge_totals() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    assert_eq!(run(&state).ended, None);
    state.apply(&log.event(RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    })));
    assert!(run(&state).ended.is_some());
    let totals = run(&state).totals(T0 + time::Duration::hours(1));
    assert_eq!(totals.elapsed, run(&state).elapsed(T0));
}

/// A workflow an agent's own CLI runs, still going, with one agent that started at `T0`.
fn vendor_workflow(duration_ms: u64) -> AgentEvent {
    use argonauts_core::events::session::{
        SubagentStatus, VendorAgent, VendorAgentState, VendorPhase, VendorWorkflow,
    };
    let started = u64::try_from((T0 - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).unwrap();
    AgentEvent::Workflow {
        workflow: Box::new(VendorWorkflow {
            id: "w1".into(),
            call: None,
            name: "probe".into(),
            description: "Two phases".into(),
            phases: vec![VendorPhase {
                index: 1,
                title: "Ask".into(),
                detail: String::new(),
            }],
            agents: vec![VendorAgent {
                index: 1,
                label: "ask:one".into(),
                phase: Some(1),
                model: None,
                state: VendorAgentState::Running,
                tokens: 0,
                tool_calls: 0,
                duration_ms: 0,
                started_at_ms: Some(started),
                attempt: 1,
                prompt: None,
                result: None,
            }],
            status: SubagentStatus::Running,
            tokens: 10,
            tool_calls: 0,
            duration_ms,
            summary: None,
            started_at_ms: None,
        }),
    }
}

#[test]
fn a_vendor_workflow_leaves_the_panel_when_its_session_ends() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.apply(&log.agent(1, vendor_workflow(27)));
    let wall = T0 + Duration::seconds(90);
    let now = std::time::Instant::now();
    let rows = crate::bridge::panel::run_rows(&state, wall, now);
    let probe = rows
        .iter()
        .find(|row| row.name == "probe")
        .expect("the workflow's row");
    // Its time counts from when it was first seen, three seconds in, not from the last
    // report's 27 ms.
    assert_eq!(probe.elapsed_ms, 87_000);
    // A later report of its totals never sets the time back.
    state.apply(&log.agent(1, vendor_workflow(3_300)));
    let rows = crate::bridge::panel::run_rows(&state, wall, now);
    let probe = rows.iter().find(|row| row.name == "probe").unwrap();
    assert_eq!(probe.elapsed_ms, 87_000);

    // Stopped before the workflow said it ended: the row goes, and so does its agent.
    let end = SessionEnd {
        vendor_session: None,
        exit_code: None,
        signal: None,
        duration_ms: 1_000,
        reason: EndReason::Stopped,
        spend: Spend::default(),
        tool_calls: 0,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log: None,
    };
    state.apply(&log.session(1, None, SessionEvent::Ended(Box::new(end))));
    let rows = crate::bridge::panel::run_rows(&state, wall, now);
    assert!(rows.iter().all(|row| row.name != "probe"), "{rows:?}");
    let workflow = &session(&state).workflows[0];
    assert!(workflow.ended());
    assert!(
        workflow
            .agents
            .iter()
            .all(|agent| agent.state != argonauts_core::events::session::VendorAgentState::Running)
    );
}
