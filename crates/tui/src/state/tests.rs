use argonauts_core::api::{RunSummary, StatusSummary, TaskBrief};
use argonauts_core::app::{AppChoice, ChoiceKind, ConversationEvent, PlanUsageChip, TurnReply};
use argonauts_core::events::agent_status::{AgentStatusEvent, FallbackEvent, RunSpend, SpendEvent};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, PolicyCheck, QuestionPacket,
};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DivideEvent, PlanEvent, TaskGraph};
use argonauts_core::events::run::{
    Party, PhaseChange, RunLifecycle, RunMode, RunOutcome, RunPhase, RunSnapshot, RunStarted,
    WaitReason,
};
use argonauts_core::events::session::{
    Delivery, EndReason, FileDiff, QueuedMessage, SessionEnd, SessionEvent, SessionStarted,
    SubagentKind, SubagentStatus, SubagentUpdate,
};
use argonauts_core::events::shared::{BlackboardEvent, RunReviewEvent};
use argonauts_core::events::task::{
    CheckKind, CheckOutcome, TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState, TryCause,
};
use argonauts_core::events::workflow::{
    StepEnd, StepFinished, StepStarted, WorkflowEvent, WorkflowStarted,
};
use argonauts_core::events::{MergeEvent, ReviewEvent, RunEvent, RunEventKind};
use argonauts_core::fallback::QualityDebt;
use argonauts_core::input::MentionTarget;
use argonauts_core::limits::{
    AgentHealth, HealthState, PlanUsageReading, PlanUsageSource, PlanUsageStatus,
};
use argonauts_core::names::{AgentId, ChoiceId, MessageId, ModelRef, RoleName, TurnId, WorkflowId};
use argonauts_core::policy::{NarratorLineKind, NarratorStatus};
use argonauts_core::session_runner::{Access, RunnerKind};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::steps::judgment::{ReviewOutput, ReviewVerdict};
use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};
use argonauts_core::{AgentEvent, DiffStat, Note, NoteLevel, PhaseStatus, Spend, TaskId};
use crossterm::event::{KeyCode, KeyEvent};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use super::*;
use crate::app::UiAction;
use crate::bridge::{Cx, Effects};
use crate::model::ViewModel;
use crate::questions::{QuestionTarget, Questions};
use crate::theme::Theme;
use crate::views::settings::approvals::ApprovalsView;
use crate::views::{View, ViewCtx, ViewOutcome};

const T0: OffsetDateTime = datetime!(2026-09-30 10:00 UTC);

fn run_id() -> RunId {
    RunId::new("run-20260930-100000-a1b2").unwrap()
}

fn task_id(n: u32) -> TaskId {
    TaskId::new(format!("task-{n:03}")).unwrap()
}

fn key(n: u32) -> SessionKey {
    SessionKey::numbered(n)
}

fn worker(name: &str, model: &str) -> ModelRef {
    ModelRef::new(name, Some(model.to_owned()))
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
        task: Option<TaskId>,
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
                task_id: task,
                session,
                source: "argonauts-code".into(),
                kind,
            }),
        }
    }

    fn agent(&mut self, session: u32, event: AgentEvent) -> AppEvent {
        self.at(None, Some(key(session)), RunEventKind::Agent(event))
    }

    fn session(&mut self, session: u32, task: Option<u32>, event: SessionEvent) -> AppEvent {
        self.at(
            task.map(task_id),
            Some(key(session)),
            RunEventKind::Session(event),
        )
    }

    fn task(&mut self, task: u32, event: TaskEvent) -> AppEvent {
        self.at(Some(task_id(task)), None, RunEventKind::Task(event))
    }
}

fn started(title: &str, mode: RunMode) -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: title.into(),
        title: title.into(),
        mode,
        forced: false,
        workflow: "council".into(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: "/repo".into(),
        base_branch: Some("main".into()),
        base_commit: "abc123".into(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: Vec::new(),
        cli_versions: Default::default(),
        version: "0.1.0".into(),
        outside_agent: Some("slack".into()),
        instructions: Vec::new(),
        required_steps: Vec::new(),
    })))
}

fn session_started(who: ModelRef, follow: u8) -> SessionEvent {
    SessionEvent::Started(Box::new(SessionStarted {
        agent_id: AgentId(who.agent.clone()),
        worker: who,
        provider: "anthropic".into(),
        kind: RunnerKind::Cli,
        role: RoleName::Worker,
        step: None,
        access: Access::Build,
        lifetime: SessionLifetime::Task,
        resumed_from: None,
        follow_key: Some(follow),
        workflow_step: None,
        label: None,
        asked: None,
    }))
}

fn spec(n: u32, title: &str) -> TaskSpec {
    TaskSpec {
        id: task_id(n),
        title: title.into(),
        task: title.into(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: Some(worker("claude-code", "opus")),
        reviewers: Vec::new(),
        limit: TaskLimit {
            limit: None,
            max_minutes: None,
            max_tool_calls: None,
            inactivity_min: None,
        },
        predictions: None,
        item: None,
    }
}

fn question(id: u32, title: &str, needs_you: bool) -> ApprovalOpened {
    ApprovalOpened {
        id: ApprovalId::numbered(id),
        kind: ApprovalKind::Question,
        title: title.into(),
        task: Some(task_id(1)),
        packet: ApprovalPacket::Question(QuestionPacket {
            from: worker("codex", "gpt-5.6"),
            task: task_id(1),
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

fn reading(agent_id: &str, window: &str, used: f64, status: PlanUsageStatus) -> PlanUsageReading {
    PlanUsageReading {
        agent_id: AgentId(agent_id.into()),
        status,
        used: Some(used),
        resets_at: None,
        window: Some(window.into()),
        source: PlanUsageSource::RateLimitEvent,
        observed_at: T0,
        detail: None,
        wrap_up: false,
    }
}

fn summary() -> RunSummary {
    RunSummary {
        run: run_id(),
        title: "Fix the export race".into(),
        mode: RunMode::Crew,
        workflow: "council".into(),
        state: argonauts_core::api::RunState::Running,
        stopped_at: String::new(),
        updated: T0,
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: true,
        remind: false,
        name: None,
        shape_known: true,
    }
}

fn fold(events: &[AppEvent]) -> AppState {
    let mut state = AppState::default();
    for event in events {
        state.apply(event);
    }
    state
}

/// A crew run with one Claude Code session on task-001.
fn crew(log: &mut Log) -> Vec<AppEvent> {
    vec![
        log.event(started("Fix the export race", RunMode::Crew)),
        log.task(1, TaskEvent::Created(Box::new(spec(1, "Middleware")))),
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
    assert_eq!(folded.outside_agent.as_deref(), Some("slack"));
    assert_eq!(folded.workflow.as_deref(), Some("council"));
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
    let waiting = RunLifecycle::Waiting {
        reason: WaitReason::Paused,
        waiting_on: Vec::new(),
    };
    state.apply(&log.event(RunEventKind::Run(waiting)));
    assert!(!run(&state).pausing);
    assert_eq!(run(&state).waiting, Some(WaitReason::Paused));
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
    let tasks = run(&state).tasks.clone();
    state.apply(&log.at(None, Some(key(1)), kind));
    assert_eq!(run(&state).seq, 3);
    assert_eq!(run(&state).updated, Some(T0 + Duration::seconds(3)));
    assert_eq!(session(&state).last_event, T0 + Duration::seconds(3));
    assert_eq!(run(&state).tasks, tasks);
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
fn state_folds_run_review() {
    folds_as_progress(RunEventKind::RunReview(RunReviewEvent::Committed {
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
        tasks: vec![spec(2, "Tests"), spec(3, "Docs")],
        critical_path: Vec::new(),
    };
    state.apply(&log.event(RunEventKind::Divide(DivideEvent::Graph(graph))));
    assert_eq!(run(&state).tasks.len(), 3);
    assert_eq!(run(&state).tasks[&task_id(3)].title, "Docs");
    let picked = DivideEvent::Worker {
        task: task_id(2),
        worker: worker("codex", "gpt-5.6"),
        algorithm: argonauts_core::settings::protocol::RoutingAlgorithm::RoundRobin,
        explored: false,
        reason: String::new(),
    };
    state.apply(&log.event(RunEventKind::Divide(picked)));
    assert_eq!(
        run(&state).tasks[&task_id(2)].worker,
        Some(worker("codex", "gpt-5.6"))
    );
}

#[test]
fn state_folds_task() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.apply(&log.task(
        1,
        TaskEvent::State {
            from: None,
            to: TaskState::Running,
            reason: String::new(),
        },
    ));
    state.apply(&log.task(
        1,
        TaskEvent::Try {
            try_number: 2,
            worker: worker("claude-code", "opus"),
            cause: TryCause::Rotation,
            told: Vec::new(),
        },
    ));
    let task = &run(&state).tasks[&task_id(1)];
    assert_eq!(task.state, Some(TaskState::Running));
    assert_eq!((task.try_number, task.rotations), (2, 1));
    assert_eq!(session(&state).rotations, 1);

    // A fallback stays the first worker's fallback until work goes back to it.
    state.apply(&log.task(
        1,
        TaskEvent::Reassigned {
            from: Some(worker("claude-code", "opus")),
            to: worker("codex", "gpt-5.6"),
            by: Party::Rules,
            rule_override: None,
        },
    ));
    state.apply(&log.task(
        1,
        TaskEvent::CheckpointTagged {
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
    state.apply(&log.task(1, TaskEvent::QualityDebt(debt.clone())));
    let stat = DiffStat {
        files_changed: 2,
        insertions: 10,
        deletions: 3,
    };
    state.apply(&log.task(
        1,
        TaskEvent::Diff {
            stat,
            path: "d.patch".into(),
        },
    ));
    let task = &run(&state).tasks[&task_id(1)];
    assert_eq!(task.fallback_for, Some(worker("claude-code", "opus")));
    assert_eq!(task.worker, Some(worker("codex", "gpt-5.6")));
    assert_eq!(task.checkpoints, 1);
    assert_eq!(task.debt, vec![debt]);
    assert_eq!(task.diff, Some(stat));

    state.apply(&log.task(
        1,
        TaskEvent::DebtCleared {
            by: worker("claude-code", "opus"),
        },
    ));
    state.apply(&log.task(
        1,
        TaskEvent::Reassigned {
            from: Some(worker("codex", "gpt-5.6")),
            to: worker("claude-code", "opus"),
            by: Party::Rules,
            rule_override: None,
        },
    ));
    let task = &run(&state).tasks[&task_id(1)];
    assert!(task.debt.is_empty());
    assert_eq!(task.fallback_for, None);
}

/// A session its run named no model for reads by the model its command launched, not by what
/// the settings say today; its CLI's own report still says it more exactly. One its run named
/// keeps that name.
#[test]
fn a_session_is_named_by_the_model_it_launched() {
    use argonauts_core::events::session::{LaunchRecord, PromptVia, SessionInput};
    let input = |model: &str| {
        SessionEvent::Input(Box::new(SessionInput {
            launch: LaunchRecord {
                program: "agy".into(),
                args: vec!["--model".into(), model.into(), "<prompt>".into()],
                cwd: "/w".into(),
                env_set: Vec::new(),
                env_removed: Vec::new(),
                prompt_via: PromptVia::Argument,
                sandbox: None,
            },
            parts: Vec::new(),
            queued: Vec::new(),
        }))
    };
    let mut log = Log::new();
    let unnamed = ModelRef::new("antigravity-cli", None);
    let mut state = fold(&[
        log.event(started("Fix the export race", RunMode::Crew)),
        log.session(1, None, session_started(unnamed, 1)),
        log.session(1, None, input("gemini-3.6-flash-low")),
    ]);
    assert_eq!(
        session(&state).worker,
        worker("antigravity-cli", "gemini-3.6-flash-low")
    );
    state.apply(&log.agent(
        1,
        AgentEvent::Init {
            session_id: None,
            model: Some("gemini-3.6-flash-low-2".into()),
            tools: Vec::new(),
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        },
    ));
    assert_eq!(
        session(&state).worker,
        worker("antigravity-cli", "gemini-3.6-flash-low-2")
    );

    let mut log = Log::new();
    let named = worker("codex", "gpt-6-luna");
    let state = fold(&[
        log.event(started("Fix the export race", RunMode::Crew)),
        log.session(1, None, session_started(named.clone(), 1)),
        log.session(1, None, input("gpt-6")),
    ]);
    assert_eq!(session(&state).worker, named);
}

#[test]
fn state_folds_session() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let folded = session(&state);
    assert_eq!(folded.follow, Some(1));
    assert_eq!(folded.agent_id, "claude-code");
    assert_eq!(folded.task, Some(task_id(1)));
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
        cli_session: None,
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
    assert_eq!(
        session(&state).retry.as_ref().map(|r| r.try_number),
        Some(2)
    );

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
            sent: None,
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
        // A session starts fresh near the end of the window it reports.
        Some(argonauts_core::views::ContextUse {
            used: 150_000,
            window: Some(1_000_000),
            cap: Some(900_000),
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
                tokens: None,
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
fn state_folds_check() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let check = CheckOutcome {
        name: "tests".into(),
        kind: CheckKind::Command,
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
    state.apply(&log.at(Some(task_id(1)), None, RunEventKind::Check(check)));
    assert_eq!(
        run(&state).tasks[&task_id(1)].last_check,
        Some(CheckBrief {
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
        for_task: task_id(1),
        files: Vec::new(),
    };
    state.apply(&log.at(Some(task_id(1)), None, RunEventKind::Review(tests)));
    assert_eq!(run(&state).tasks[&task_id(1)].reviews, 0);
    assert_eq!(run(&state).seq, 3);
}

#[test]
fn a_reviewer_that_sent_the_work_back_waits_for_its_next_round() {
    let mut log = Log::new();
    let mut state = fold(&[log.event(started("Fix the export race", RunMode::Crew))]);
    let step = StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 1,
        path: vec![0],
    };
    let start = |log: &mut Log, n: u32, role: RoleName, label: &str| {
        let SessionEvent::Started(mut started) = session_started(worker("codex", "gpt-6"), 1)
        else {
            unreachable!()
        };
        started.role = role;
        started.label = Some(label.into());
        started.workflow_step = Some(step.clone());
        log.session(n, Some(1), SessionEvent::Started(started))
    };
    let ended = |log: &mut Log, n: u32| {
        log.session(
            n,
            Some(1),
            SessionEvent::Ended(Box::new(SessionEnd {
                cli_session: None,
                exit_code: Some(0),
                signal: None,
                duration_ms: 1_000,
                reason: EndReason::Completed,
                spend: Spend::default(),
                tool_calls: 0,
                result_text: None,
                structured: None,
                last_message: None,
                raw_log: None,
            })),
        )
    };
    let reviewed = |log: &mut Log, n: u32, verdict: ReviewVerdict| {
        let review = ReviewEvent::Reviewed {
            reviewer: worker("codex", "gpt-6"),
            blind: false,
            round: n,
            review: ReviewOutput {
                verdict,
                summary: String::new(),
                comments: Vec::new(),
                tamper_suspected: false,
            },
        };
        log.at(Some(task_id(1)), Some(key(n)), RunEventKind::Review(review))
    };
    let counted = |state: &AppState| {
        let totals = run(state).totals(T0);
        (totals.agents_done, totals.agents_total)
    };

    let first = [
        start(&mut log, 1, RoleName::Reviewer, "review:per-key"),
        ended(&mut log, 1),
        reviewed(&mut log, 1, ReviewVerdict::RequestChanges),
    ];
    for event in &first {
        state.apply(event);
    }
    assert_eq!(counted(&state), (0, 1), "its next round is still to come");

    let again = [
        start(&mut log, 2, RoleName::Reviewer, "review:per-key-2"),
        ended(&mut log, 2),
        reviewed(&mut log, 2, ReviewVerdict::Approve),
    ];
    for event in &again {
        state.apply(event);
    }
    assert_eq!(counted(&state), (1, 1), "its rounds are one agent");
}

#[test]
fn state_folds_merge() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let merged = MergeEvent::Merged {
        task: task_id(1),
        sha: "abc".into(),
    };
    state.apply(&log.event(RunEventKind::Merge(merged)));
    assert_eq!(
        run(&state).tasks[&task_id(1)].state,
        Some(TaskState::Merged)
    );
    assert_eq!(run(&state).tasks_done(), (1, 1));
}

#[test]
fn state_folds_approval() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let open = |n, needs| {
        RunEventKind::Approval(ApprovalEvent::Opened(Box::new(question(
            n,
            "Which cache?",
            needs,
        ))))
    };
    state.apply(&log.event(open(1, true)));
    state.apply(&log.event(open(2, false)));
    state.apply(
        &log.event(RunEventKind::Approval(ApprovalEvent::SpotCheckQueued {
            id: ApprovalId::numbered(2),
        })),
    );
    assert_eq!(run(&state).approvals.len(), 2);
    assert!(!run(&state).approvals[0].replayed);
    assert_eq!(run(&state).spot_checks, vec![ApprovalId::numbered(2)]);
    // Only an approval that waits on you is listed.
    assert_eq!(state.needs_you().len(), 1);

    state.apply(&log.event(RunEventKind::Approval(ApprovalEvent::Withdrawn {
        id: ApprovalId::numbered(1),
        reason: "answered elsewhere".into(),
    })));
    assert_eq!(run(&state).approvals.len(), 1);
    assert!(state.needs_you().is_empty());
}

#[test]
fn state_folds_fallback() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.apply(&log.event(RunEventKind::Fallback(FallbackEvent::Waiting {
        task: task_id(1),
        agent_id: AgentId("claude-code".into()),
        until: Some(T0 + Duration::hours(2)),
        kept_off: None,
    })));
    assert_eq!(
        run(&state).tasks[&task_id(1)]
            .waiting_on_agent
            .as_ref()
            .map(|wait| wait.agent_id.as_str()),
        Some("claude-code")
    );
    state.apply(&log.event(RunEventKind::Fallback(FallbackEvent::FellBack {
        task: task_id(1),
        to: worker("codex", "gpt-5.6"),
        mode: argonauts_core::settings::fallback::FallbackMode::Auto,
        by: Party::Rules,
        context: None,
    })));
    let task = &run(&state).tasks[&task_id(1)];
    assert_eq!(task.fallback_for, Some(worker("claude-code", "opus")));
    assert_eq!(task.worker, Some(worker("codex", "gpt-5.6")));
    assert_eq!(task.waiting_on_agent, None);

    // A session that starts on the task afterwards is a fallback too.
    state.apply(&log.session(2, Some(1), session_started(worker("codex", "gpt-5.6"), 2)));
    assert_eq!(
        run(&state).sessions[&key(2)].fallback_for,
        Some(worker("claude-code", "opus"))
    );
}

#[test]
fn state_folds_agent_status() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let five = reading("claude-code", "five_hour", 0.71, PlanUsageStatus::Allowed);
    let week = reading("claude-code", "seven_day", 0.40, PlanUsageStatus::Allowed);
    state.apply(
        &log.event(RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(
            five.clone(),
        ))),
    );
    state.apply(&log.event(RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(week))));
    let newer = PlanUsageReading {
        used: Some(0.75),
        observed_at: T0 + Duration::minutes(1),
        ..five
    };
    state.apply(
        &log.event(RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(
            newer.clone(),
        ))),
    );
    assert_eq!(state.plan_windows["claude-code"].len(), 2);
    assert_eq!(
        binding_reading(&state.plan_windows["claude-code"]),
        Some(&newer)
    );

    let health = AgentHealth {
        agent_id: AgentId("codex".into()),
        state: HealthState::Down,
        since: T0,
        reason: None,
        next_test_at: None,
        disabled: None,
    };
    state.apply(&log.event(RunEventKind::AgentStatus(AgentStatusEvent::Health(health))));
    state.apply(
        &log.event(RunEventKind::AgentStatus(AgentStatusEvent::Disabled {
            agent_id: AgentId("antigravity-cli".into()),
            reason: "untested version".into(),
        })),
    );
    assert_eq!(state.health["codex"].state, HealthState::Down);
    assert_eq!(state.disabled["antigravity-cli"], "untested version");
}

#[test]
fn state_folds_limit() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let limit = RunSpend {
        spent: Spend {
            cost_usd: 0.21,
            ..Spend::default()
        },
        limit: Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount: 5.0,
        }),
        lines: Vec::new(),
        reserve_left: None,
    };
    state.apply(&log.event(RunEventKind::Spend(SpendEvent::Run(limit.clone()))));
    assert_eq!(run(&state).spend, Some(limit));
    assert!((run(&state).cost_usd() - 0.21).abs() < 1e-9);
}

/// A refused brief resumes the main agent's CLI session as s-002. Claude Code's totals for it
/// include what s-001 spent, so a log from before Argonauts Code took that off reports $0.152
/// for s-002, though it spent $0.086. Its row and the run show what each session spent.
#[test]
fn a_resumed_session_counts_only_what_it_spent() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let usage = |tokens_in: u64, tokens_out: u64, cost: f64| AgentEvent::Usage {
        input_tokens: tokens_in,
        output_tokens: tokens_out,
        cached_tokens: 0,
        cost_usd: Some(cost),
        cumulative: true,
    };
    let ended = |tokens_in: u64, tokens_out: u64, cost: f64| {
        SessionEvent::Ended(Box::new(SessionEnd {
            cli_session: Some("cli-7".into()),
            exit_code: Some(0),
            signal: None,
            duration_ms: 1_000,
            reason: EndReason::Completed,
            spend: Spend {
                input_tokens: tokens_in,
                output_tokens: tokens_out,
                cost_usd: cost,
                ..Spend::default()
            },
            tool_calls: 1,
            result_text: None,
            structured: None,
            last_message: None,
            raw_log: None,
        }))
    };
    state.apply(&log.agent(1, usage(12_611, 8_142, 0.066)));
    state.apply(&log.session(1, None, ended(12_611, 8_142, 0.066)));
    let SessionEvent::Started(mut resumed) = session_started(worker("claude-code", "haiku"), 1)
    else {
        unreachable!()
    };
    resumed.resumed_from = Some("cli-7".into());
    state.apply(&log.session(2, Some(1), SessionEvent::Started(resumed)));
    state.apply(&log.agent(2, usage(15_000, 9_000, 0.152)));
    state.apply(&log.session(2, None, ended(2_389, 858, 0.086)));
    let limit = RunSpend {
        spent: Spend {
            cost_usd: 0.152,
            ..Spend::default()
        },
        limit: None,
        lines: Vec::new(),
        reserve_left: None,
    };
    state.apply(&log.event(RunEventKind::Spend(SpendEvent::Run(limit))));
    let fixed = &run(&state).sessions[&key(2)];
    assert_eq!(fixed.cost_usd, Some(0.086));
    assert_eq!((fixed.tokens_in, fixed.tokens_out), (2_389, 858));
    assert!((run(&state).spend().cost_usd - 0.152).abs() < 1e-9);
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
        workflow: "council".into(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        main_agent: None,
        effort: None,
    });
    assert_eq!(
        state.welcome.as_ref().map(|w| w.workflow.as_str()),
        Some("council")
    );

    let status = argonauts_core::app::HeaderAndBottomLine {
        title: "export race".into(),
        repo: "~/argonauts".into(),
        workflow: "council".into(),
        ultracode: false,
        ultra: None,
        run: None,
        main_agent: worker("claude-code", "opus"),
        plan_chips: vec![PlanUsageChip {
            label: "Claude".into(),
            reading: None,
            health: HealthState::Healthy,
            windows: Vec::new(),
        }],
        line: Default::default(),
        spent: None,
        limit: None,
        narrator: NarratorMode::Live,
        approval_mode: ApprovalMode::AutoPreview,
        awake: None,
    };
    state.apply(&AppEvent::Status {
        status: status.clone(),
    });
    assert_eq!(state.status, Some(status.clone()));
    // The header waited for this agent, and later ones don't change what it printed.
    let header_agent = |state: &AppState| state.welcome.as_ref()?.main_agent.clone();
    assert_eq!(header_agent(&state), Some(worker("claude-code", "opus")));
    state.apply(&AppEvent::Status {
        status: argonauts_core::app::HeaderAndBottomLine {
            main_agent: worker("codex", "gpt-5.6"),
            ..status
        },
    });
    assert_eq!(header_agent(&state), Some(worker("claude-code", "opus")));

    state.apply(&AppEvent::Mode {
        mode: AppMode::Pausing { run: run_id() },
    });
    assert!(run(&state).pausing);
    state.apply(&AppEvent::Mode {
        mode: AppMode::Waiting {
            run: run_id(),
            reason: WaitReason::Approval,
        },
    });
    assert!(!run(&state).pausing);
    assert_eq!(run(&state).waiting, Some(WaitReason::Approval));
    state.apply(&AppEvent::Mode {
        mode: AppMode::Working { run: run_id() },
    });
    assert_eq!(run(&state).waiting, None);
}

/// The conversation's context as its CLI counted it stays known between turns, since there's
/// no session to ask then, and goes when another model answers. What its turns add goes with
/// it.
#[test]
fn the_conversations_context_count_is_kept_until_another_model_answers() {
    use argonauts_core::events::context::{ContextCategory, ContextKind, ContextUsage};

    let mut state = AppState::default();
    let conversation = |event| AppEvent::Conversation { event };
    let started = |n: u32, model: &str| ConversationEvent::TurnStarted {
        turn: TurnId::numbered(n),
        text: "why?".into(),
        worker: worker("claude-code", model),
        read_only: true,
        commit: "abc".into(),
        uncommitted: Vec::new(),
        settings_sha256: None,
        continues: false,
    };
    let agent = |n: u32, event| {
        conversation(ConversationEvent::Agent {
            turn: TurnId::numbered(n),
            event,
        })
    };
    let fill = |used| AgentEvent::ContextFill {
        used,
        size: Some(1_000_000),
        sent: None,
    };
    let finished = |n: u32| {
        conversation(ConversationEvent::TurnCancelled {
            turn: TurnId::numbered(n),
        })
    };
    let usage = ContextUsage {
        categories: vec![ContextCategory {
            name: "Messages".into(),
            tokens: 186_000,
            color: "purple_FOR_SUBAGENTS_ONLY".into(),
            kind: ContextKind::Used,
        }],
        total_tokens: 186_000,
        window: 1_000_000,
        raw_window: 1_000_000,
        percentage: 19,
        auto_compact: true,
        auto_compact_threshold: Some(950_000),
        ..ContextUsage::default()
    };
    state.apply(&conversation(started(1, "opus")));
    state.apply(&agent(1, fill(186_000)));
    state.apply(&agent(
        1,
        AgentEvent::ContextBreakdown {
            usage: Box::new(usage.clone()),
        },
    ));
    let kept = state.conversation.context.clone().expect("the count");
    assert_eq!(kept.usage, usage);
    state.apply(&finished(1));

    state.apply(&conversation(started(2, "opus")));
    assert_eq!(state.conversation.context.as_ref(), Some(&kept));
    state.apply(&agent(2, fill(195_400)));
    state.apply(&finished(2));
    assert_eq!(state.conversation.growth.per_turn(), Some(9_400));

    state.apply(&conversation(started(3, "sonnet")));
    assert_eq!(state.conversation.context, None);
    assert_eq!(state.conversation.growth.per_turn(), None);

    // A resumed conversation's record brings the count back, as old as it is.
    let at = time::macros::datetime!(2026-10-14 01:52 UTC);
    let mut resumed = AppState::default();
    resumed.apply(&conversation(started(1, "opus")));
    resumed.apply(&conversation(ConversationEvent::ContextLeft {
        turn: TurnId::numbered(1),
        at,
        used: usage.total_tokens,
        window: Some(usage.window),
        usage: Some(Box::new(usage.clone())),
    }));
    resumed.apply(&finished(1));
    let counted = resumed.conversation.context.expect("the record's count");
    assert_eq!((counted.usage, counted.counted), (usage, at));
}

#[test]
fn state_folds_conversation() {
    let mut state = AppState::default();
    let turn = TurnId::numbered(1);
    let conversation = |event| AppEvent::Conversation { event };
    state.apply(&conversation(ConversationEvent::TurnStarted {
        turn: turn.clone(),
        text: "why?".into(),
        worker: worker("claude-code", "opus"),
        read_only: true,
        commit: "abc".into(),
        uncommitted: Vec::new(),
        settings_sha256: None,
        continues: false,
    }));
    let agent = |event| ConversationEvent::Agent {
        turn: turn.clone(),
        event,
    };
    state.apply(&conversation(agent(AgentEvent::Reasoning {
        text: String::new(),
    })));
    assert_eq!(state.conversation.doing, Doing::Thinking);
    state.apply(&conversation(agent(AgentEvent::TextDelta {
        text: "Because".into(),
        parent: None,
    })));
    state.apply(&conversation(agent(AgentEvent::Message {
        text: "Because".into(),
        parent: None,
    })));
    state.apply(&conversation(agent(AgentEvent::TextDelta {
        text: "Also".into(),
        parent: None,
    })));
    state.apply(&conversation(agent(AgentEvent::Usage {
        input_tokens: 10,
        output_tokens: 3_600,
        cached_tokens: 0,
        cost_usd: None,
        cumulative: true,
    })));
    assert_eq!(state.conversation.streamed, "Because\n\nAlso");
    assert_eq!(state.conversation.tokens_out, 3_600);
    assert_eq!(state.conversation.doing, Doing::Writing);
    // Another turn's events don't count.
    state.apply(&conversation(ConversationEvent::Agent {
        turn: TurnId::numbered(9),
        event: AgentEvent::Reasoning {
            text: String::new(),
        },
    }));
    assert_eq!(state.conversation.doing, Doing::Writing);

    state.apply(&conversation(ConversationEvent::TurnFinished {
        turn: turn.clone(),
        reply: TurnReply::Answer {
            markdown: "Because".into(),
        },
        spend: Spend::default(),
        elapsed_ms: 1_000,
    }));
    assert_eq!(state.conversation.turn, None);
    assert!(state.conversation.streamed.is_empty());
    assert_eq!(state.conversation.doing, Doing::Nothing);
}

/// What Claude Code drew above the input stays across turns, since each turn's session draws
/// it again, until a session says nothing is there or `/clear` starts a new conversation.
#[test]
fn the_notice_goes_when_nothing_is_drawn_or_the_conversation_is_cleared() {
    use argonauts_core::screen::{Drawn, DrawnLook, DrawnRow, DrawnSpan, ScreenChange};

    let mut state = AppState::default();
    let turn = |n| TurnId::numbered(n);
    let start = |n| AppEvent::Conversation {
        event: ConversationEvent::TurnStarted {
            turn: turn(n),
            text: "check release.md".into(),
            worker: worker("claude-code", "opus"),
            read_only: true,
            commit: "abc".into(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
        },
    };
    let drawn = |n, rows: Vec<DrawnRow>| AppEvent::Conversation {
        event: ConversationEvent::Agent {
            turn: turn(n),
            event: AgentEvent::ScreenChanged {
                change: ScreenChange::Drawn {
                    drawn: Drawn {
                        rows,
                        ..Drawn::default()
                    },
                },
            },
        },
    };
    let notice = vec![DrawnRow {
        spans: vec![DrawnSpan {
            text: "Heads up · Dana is away until Monday.".into(),
            look: DrawnLook::Plain,
        }],
    }];
    state.apply(&start(1));
    state.apply(&drawn(1, notice.clone()));
    state.apply(&start(2));
    assert!(
        state.conversation.notice.is_some(),
        "kept for the next turn"
    );
    state.apply(&drawn(2, Vec::new()));
    assert!(state.conversation.notice.is_none());

    state.apply(&drawn(2, notice));
    assert!(state.conversation.notice.is_some());
    state.apply(&AppEvent::ConversationCleared { way_back: None });
    assert!(state.conversation.notice.is_none());
}

#[test]
fn conversation_queue_clears_at_turn_start() {
    let mut state = AppState::default();
    state.conversation.queued = vec!["first".into(), "second".into()];
    state.apply(&AppEvent::Conversation {
        event: ConversationEvent::TurnStarted {
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
    assert_eq!(state.conversation.queued, vec!["second".to_owned()]);
    assert_eq!(state.conversation.turn, Some(TurnId::numbered(2)));
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
        text: "task-002 fell back to Codex.".into(),
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
    state.conversation.queued.push("later".into());
    state.apply(&AppEvent::TakenBack {
        texts: vec!["keep it small".into(), "later".into()],
    });
    assert!(run(&state).queued.is_empty());
    assert!(state.conversation.queued.is_empty());

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
        &log.event(RunEventKind::Approval(ApprovalEvent::Opened(Box::new(
            question(1, "Plan ready", true),
        )))),
    );
    assert!(run(&state).approvals[0].replayed);

    state.apply(&AppEvent::ReplayDone {
        run: run_id(),
        seq: 0,
    });
    assert!(!state.replaying());
    state.apply(
        &log.event(RunEventKind::Approval(ApprovalEvent::Opened(Box::new(
            question(2, "Merge?", true),
        )))),
    );
    assert!(!run(&state).approvals[1].replayed);
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
            tasks: vec![TaskBrief {
                task: task_id(4),
                title: "Retry queue".into(),
                state: TaskState::Running,
                worker: Some(worker("codex", "gpt-5.6")),
                note: String::new(),
            }],
            recent: Vec::new(),
            spend: Some(RunSpend {
                spent: Spend {
                    cost_usd: 3.1,
                    ..Spend::default()
                },
                limit: None,
                lines: Vec::new(),
                reserve_left: None,
            }),
            plan_windows: Vec::new(),
            waiting: Vec::new(),
        },
        open_approvals: vec![question(3, "Merge task-004?", true)],
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
    assert_eq!(folded.tasks[&task_id(4)].state, Some(TaskState::Running));
    assert!((folded.cost_usd() - 3.1).abs() < 1e-9);
    assert!(folded.approvals[0].replayed);
    assert_eq!(folded.running_sessions().count(), 1);
    assert!(folded.summary.is_some());
    assert_eq!(state.needs_you().len(), 1);
}

#[test]
fn needs_you_lists_questions_oldest_first() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.apply(
        &log.event(RunEventKind::Approval(ApprovalEvent::Opened(Box::new(
            question(1, "Which cache?", true),
        )))),
    );
    state.apply(&AppEvent::Choice {
        choice: choice(1, "Start work?"),
    });
    state.apply(
        &log.event(RunEventKind::Approval(ApprovalEvent::Opened(Box::new(
            question(2, "Merge?", true),
        )))),
    );
    state.set_aside(&run_id(), &ApprovalId::numbered(1), true);
    let titles: Vec<(String, bool)> = state
        .needs_you()
        .into_iter()
        .map(|needs| match needs {
            NeedsYou::Approval {
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
fn binding_ignores_a_windows_older_reading() {
    // The window reset since the older reading; the fresh one binds even though it's lower.
    let old = reading("claude-code", "five_hour", 0.95, PlanUsageStatus::Allowed);
    let fresh = PlanUsageReading {
        used: Some(0.10),
        observed_at: T0 + Duration::hours(5),
        ..old.clone()
    };
    let week = reading("claude-code", "seven_day", 0.40, PlanUsageStatus::Allowed);
    let readings = [old, fresh, week.clone()];
    assert_eq!(binding_reading(&readings), Some(&week));
}

fn approval_event(log: &mut Log, event: ApprovalEvent) -> AppEvent {
    log.event(RunEventKind::Approval(event))
}

/// An auto call your rules made on approval `n`, queued for a spot check.
fn auto_call(log: &mut Log, n: u32, title: &str, spot_check: bool) -> Vec<AppEvent> {
    let id = ApprovalId::numbered(n);
    let mut events = vec![
        approval_event(
            log,
            ApprovalEvent::Opened(Box::new(question(n, title, false))),
        ),
        approval_event(
            log,
            ApprovalEvent::Checked(PolicyCheck {
                id: id.clone(),
                violations: Vec::new(),
                verdict: None,
                by: Some(DecideWith::Rules),
                judge: None,
                reasons: vec!["It matches your rules".into()],
                auto_preview: false,
            }),
        ),
        approval_event(
            log,
            ApprovalEvent::Decided(ApprovalDecided {
                id: id.clone(),
                action: AnswerAction::Approve,
                text: None,
                by: Party::Rules,
                evidence_sha256: String::new(),
                auto_preview: None,
                tasks: Vec::new(),
            }),
        ),
    ];
    if spot_check {
        events.push(approval_event(log, ApprovalEvent::SpotCheckQueued { id }));
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
    let id = ApprovalId::numbered(4);
    assert!(run(&state).approvals.is_empty());
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
    let kept = |n| run(&state).decision(&ApprovalId::numbered(n)).is_some();
    assert!(kept(1), "it waits for you");
    assert!(!kept(2), "an old call nobody asked about is dropped");
    assert!(kept(50));
}

/// The bridge folds an event into the state and then hands it to the questions.
fn questions_see(
    questions: &mut Questions,
    model: &mut ViewModel,
    state: &AppState,
    event: &AppEvent,
) {
    let settings = UiSettings::default();
    let mut out = Effects::default();
    let mut cx = Cx {
        model,
        state,
        settings: &settings,
        now: std::time::Instant::now(),
        out: &mut out,
    };
    questions.on_event(event, &mut cx);
}

/// Then, as the bridge does, the state learns what went out.
fn questions_act(
    questions: &mut Questions,
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
        questions.on_action(action, &mut cx);
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

fn spot_questions(questions: &Questions) -> Vec<u64> {
    questions
        .open
        .iter()
        .filter(|(_, question)| matches!(question.target, QuestionTarget::SpotCheck { .. }))
        .map(|(id, _)| *id)
        .collect()
}

#[test]
fn a_spot_check_goes_out_once() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    let mut questions = Questions::default();
    let mut model = ViewModel::default();
    let mut events = auto_call(&mut log, 5, "Which cache?", true);
    events.extend(auto_call(&mut log, 6, "Which queue?", true));
    for event in &events {
        state.apply(event);
        questions_see(&mut questions, &mut model, &state, event);
    }
    assert_eq!(spot_questions(&questions).len(), 2);

    let mut approvals = ApprovalsView::new(Default::default());
    let shown = approvals_text(&mut approvals, &state);
    assert!(shown.contains("Which cache?"), "{shown}");
    assert!(
        shown.contains("· decided by"),
        "each row shows its call: {shown}"
    );

    // Marked in `/approvals`: its question closes and the state lets it go.
    let ViewOutcome::Send(command) = approvals_key(&mut approvals, &state, 'y') else {
        panic!("y sends the mark");
    };
    let sent = questions_act(
        &mut questions,
        &mut model,
        &mut state,
        &UiAction::Command(command),
    );
    assert_eq!(
        sent,
        vec![UiCommand::SpotCheck {
            run: run_id(),
            id: ApprovalId::numbered(5),
            correct: true,
        }]
    );
    assert_eq!(spot_questions(&questions).len(), 1);
    assert_eq!(run(&state).spot_checks, vec![ApprovalId::numbered(6)]);
    assert!(matches!(
        approvals_key(&mut approvals, &state, 'n'),
        ViewOutcome::Notice(_)
    ));

    // Answered on its question: the open view drops the row, and a new one lists nothing.
    let question = spot_questions(&questions)[0];
    let sent = questions_act(
        &mut questions,
        &mut model,
        &mut state,
        &UiAction::Answer {
            question,
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
    let bare = |name: &str| ModelRef::new(name, None);
    state.apply(&log.session(1, None, session_started(bare("codex"), 1)));
    state.apply(&log.session(2, None, session_started(bare("claude-code"), 2)));
    let init = |model: &str| AgentEvent::Init {
        session_id: None,
        model: Some(model.to_owned()),
        tools: Vec::new(),
        plugins: Vec::new(),
        capabilities: Vec::new(),
        version: None,
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
    let bare = |name: &str| ModelRef::new(name, None);
    state.apply(&log.session(1, None, session_started(bare("claude-code"), 1)));
    state.apply(&log.session(2, None, session_started(bare("claude-code"), 2)));
    let worker = |state: &AppState, n: u32| run(state).sessions[&key(n)].worker.to_string();
    assert_eq!(worker(&state, 1), "claude-code/claude-opus-5-5");
    let init = |model: &str| AgentEvent::Init {
        session_id: None,
        model: Some(model.to_owned()),
        tools: Vec::new(),
        plugins: Vec::new(),
        capabilities: Vec::new(),
        version: None,
    };
    state.apply(&log.agent(1, init("claude-opus-5-5")));
    state.apply(&log.agent(2, init("claude-haiku-4-5")));
    assert_eq!(worker(&state, 1), "claude-code/claude-opus-5-5");
    // The catalog guessed wrong, and the session's own word wins.
    assert_eq!(worker(&state, 2), "claude-code/claude-haiku-4-5");
}

/// The run keeps when it ended and what its merge question put up, for the lines that come after.
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
fn cli_workflow_run(duration_ms: u64) -> AgentEvent {
    use argonauts_core::events::session::{
        CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentStatus,
    };
    let started = u64::try_from((T0 - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).unwrap();
    AgentEvent::Workflow {
        workflow: Box::new(CliWorkflowRun {
            id: "w1".into(),
            call: None,
            name: "probe".into(),
            description: "Two phases".into(),
            phases: vec![CliPhase {
                index: 1,
                title: "Ask".into(),
                detail: String::new(),
            }],
            agents: vec![CliAgent {
                index: 1,
                label: "ask:one".into(),
                phase: Some(1),
                model: None,
                state: CliAgentState::Running,
                tokens: 0,
                tool_calls: 0,
                duration_ms: 0,
                started_at_ms: Some(started),
                try_number: 1,
                prompt: None,
                result: None,
                ..CliAgent::default()
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
fn a_cli_workflow_run_leaves_the_panel_when_its_session_ends() {
    let mut log = Log::new();
    let mut state = fold(&crew(&mut log));
    state.apply(&log.agent(1, cli_workflow_run(27)));
    // A second task, so the workflow is one agent's part of the run and not the run itself.
    state.apply(&log.task(2, TaskEvent::Created(Box::new(spec(2, "Exporter")))));
    state.apply(&log.session(2, Some(2), session_started(worker("codex", "gpt-5.6"), 2)));
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
    state.apply(&log.agent(1, cli_workflow_run(3_300)));
    let rows = crate::bridge::panel::run_rows(&state, wall, now);
    let probe = rows.iter().find(|row| row.name == "probe").unwrap();
    assert_eq!(probe.elapsed_ms, 87_000);

    // Stopped before the workflow said it ended: the row goes, and so does its agent.
    let end = SessionEnd {
        cli_session: None,
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
            .all(|agent| agent.state != argonauts_core::events::session::CliAgentState::Running)
    );
}

/// The conversation's own turn keeps the latest state of each subagent its CLI started, for
/// the list under the input. A workflow has a row of its own, and the turn's end clears them.
#[test]
fn a_conversation_turn_keeps_its_subagents_until_it_ends() {
    let conversation = |event| AppEvent::Conversation { event };
    let turn = TurnId::numbered(1);
    let mut state = AppState::default();
    state.apply(&conversation(ConversationEvent::TurnStarted {
        turn: turn.clone(),
        text: "split the checks".into(),
        worker: worker("claude-code", "opus"),
        read_only: true,
        commit: "abc".into(),
        uncommitted: Vec::new(),
        settings_sha256: None,
        continues: false,
    }));
    let update = |id: &str, kind, status, said: Option<&str>| {
        conversation(ConversationEvent::Agent {
            turn: turn.clone(),
            event: AgentEvent::Subagent {
                update: SubagentUpdate {
                    id: id.into(),
                    parent: None,
                    kind,
                    label: Some("Count slowly".into()),
                    prompt: None,
                    status,
                    last_message: said.map(str::to_owned),
                    log_uri: None,
                    steps_visible: true,
                    tokens: None,
                },
            },
        })
    };
    let agent = SubagentKind::Subagent;
    state.apply(&update("toolu_a", agent, SubagentStatus::Started, None));
    state.apply(&update("toolu_b", agent, SubagentStatus::Running, None));
    state.apply(&update(
        "toolu_w",
        SubagentKind::Workflow,
        SubagentStatus::Running,
        None,
    ));
    state.apply(&update(
        "toolu_a",
        agent,
        SubagentStatus::Done,
        Some("DONE-A"),
    ));
    let seen: Vec<(&str, SubagentStatus, Option<&str>)> = state
        .conversation
        .subagents
        .iter()
        .map(|known| {
            (
                known.id.as_str(),
                known.status,
                known.last_message.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("toolu_a", SubagentStatus::Done, Some("DONE-A")),
            ("toolu_b", SubagentStatus::Running, None),
        ]
    );
    state.apply(&conversation(ConversationEvent::TurnCancelled { turn }));
    assert!(state.conversation.subagents.is_empty());
}

/// A turn waits on the background once its main agent has a result while its CLI still runs
/// something, whether the turn ended by itself or Esc stopped it. The main
/// agent starting again, the last task ending, and the turn's end each end the wait.
#[test]
fn a_conversation_turn_waits_on_the_background_while_its_main_agent_rests() {
    let turn = TurnId::numbered(1);
    let said = |event| AppEvent::Conversation {
        event: ConversationEvent::Agent {
            turn: TurnId::numbered(1),
            event,
        },
    };
    let running = |tasks: &[&str]| {
        said(AgentEvent::BackgroundTasks {
            running: tasks.iter().map(|&task| task.to_owned()).collect(),
            ended: tasks.is_empty(),
        })
    };
    let result = || {
        said(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: true,
        })
    };
    let writes = || {
        said(AgentEvent::TextDelta {
            text: "Both are done.".into(),
            parent: None,
        })
    };
    let mut state = AppState::default();
    state.apply(&AppEvent::Conversation {
        event: ConversationEvent::TurnStarted {
            turn: turn.clone(),
            text: "count in the background".into(),
            worker: worker("claude-code", "opus"),
            read_only: true,
            commit: "abc".into(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
        },
    });
    let agent = |status| {
        said(AgentEvent::Subagent {
            update: SubagentUpdate {
                id: "toolu_a".into(),
                parent: None,
                kind: SubagentKind::Subagent,
                label: Some("Count slowly".into()),
                prompt: None,
                status,
                last_message: None,
                log_uri: None,
                steps_visible: true,
                tokens: None,
            },
        })
    };
    state.apply(&writes());
    state.apply(&running(&["task-a"]));
    state.apply(&result());
    assert!(
        !state.conversation.waits_on_background(),
        "a command left running is no agent, and Esc still stops the turn"
    );
    state.apply(&writes());
    state.apply(&agent(SubagentStatus::Running));
    assert!(
        !state.conversation.waits_on_background(),
        "the main agent works"
    );
    state.apply(&result());
    assert!(state.conversation.waits_on_background());
    // A subagent's own text doesn't wake the main agent.
    state.apply(&said(AgentEvent::TextDelta {
        text: "three hundred".into(),
        parent: Some("toolu_a".into()),
    }));
    assert!(state.conversation.waits_on_background());
    state.apply(&writes());
    assert!(
        !state.conversation.waits_on_background(),
        "it started again"
    );
    state.apply(&result());
    state.apply(&running(&[]));
    assert!(
        !state.conversation.waits_on_background(),
        "nothing runs now"
    );
    state.apply(&running(&["task-b"]));
    assert!(state.conversation.waits_on_background());
    state.apply(&agent(SubagentStatus::Done));
    assert!(
        !state.conversation.waits_on_background(),
        "its agent is done, and only a command runs"
    );
    state.apply(&agent(SubagentStatus::Running));
    state.apply(&AppEvent::Conversation {
        event: ConversationEvent::TurnCancelled { turn },
    });
    assert!(!state.conversation.waits_on_background());
    assert_eq!(state.conversation.background, 0);
}

/// A page published with Claude Code's Artifact tool is recorded when its call finishes, by
/// the main agent in the conversation or by an agent inside a run, and stays past the turn.
/// The bottom line's part names the newest and counts the others.
#[test]
fn published_artifacts_are_kept_with_who_published_them() {
    let overview = "https://claude.ai/code/artifact/0b1e7c9a-4f1d-4c57-9a55-2f6f0d3a8e11";
    let report = "https://claude.ai/code/artifact/7d2c4b18-93aa-4e0f-8c61-5b0e9f1a2c33";
    let started = |id: &str, file: &str| AgentEvent::ToolStarted {
        id: id.into(),
        name: "Artifact".into(),
        input: serde_json::json!({"file_path": file, "icon": "document"}),
        parent: None,
    };
    let finished = |id: &str, ok: bool, output: String| AgentEvent::ToolFinished {
        id: id.into(),
        ok,
        output: Some(output),
        parent: None,
    };
    let published = |file: &str, url: &str| format!("Published {file} at {url} (Version 1)");

    let conversation = |event| AppEvent::Conversation { event };
    let turn = |n: u32| ConversationEvent::TurnStarted {
        turn: TurnId::numbered(n),
        text: "write it up".into(),
        worker: worker("claude-code", "opus"),
        read_only: false,
        commit: "abc".into(),
        uncommitted: Vec::new(),
        settings_sha256: None,
        continues: false,
    };
    let said = |n: u32, event| {
        conversation(ConversationEvent::Agent {
            turn: TurnId::numbered(n),
            event,
        })
    };
    let mut state = AppState::default();
    assert_eq!(state.artifacts.line_part(), None);
    state.apply(&conversation(turn(1)));
    state.apply(&said(1, started("t1", "out/weekly-usage-chart.html")));
    assert!(state.artifacts.is_empty(), "nothing until it's published");
    let refused = finished(
        "t1",
        false,
        published("out/weekly-usage-chart.html", overview),
    );
    state.apply(&said(1, refused));
    assert!(
        state.artifacts.is_empty(),
        "a call that failed published none"
    );
    state.apply(&said(1, started("t2", "out/weekly-usage-chart.html")));
    let done = finished(
        "t2",
        true,
        published("out/weekly-usage-chart.html", overview),
    );
    state.apply(&said(1, done));
    assert_eq!(state.artifacts.line_part(), Some(("weekly-usage-chart", 0)));
    // The next turn forgets the last one's calls, and the page stays.
    state.apply(&conversation(turn(2)));
    assert_eq!(state.artifacts.all().len(), 1);
    assert_eq!(state.artifacts.all()[0].by, None);

    // An agent inside a run publishes one, which is its row's too.
    let mut log = Log::new();
    for event in crew(&mut log) {
        state.apply(&event);
    }
    state.apply(&log.agent(1, started("t9", "out/test-report.html")));
    let done = finished("t9", true, published("out/test-report.html", report));
    state.apply(&log.agent(1, done));
    assert_eq!(state.artifacts.line_part(), Some(("test-report", 1)));
    let latest = state.artifacts.latest().expect("the newest");
    assert_eq!(latest.artifact.name, "test-report");
    let (run_id, session) = latest.by.clone().expect("an agent's");
    assert_eq!(session, key(1));
    assert_eq!(
        state
            .artifacts
            .by_agent(&run_id, &session)
            .map(|artifact| artifact.url.as_str()),
        Some(report)
    );

    // Published again, the first is the most recent, and still one page.
    state.apply(&said(2, started("t3", "out/weekly-usage-chart.html")));
    let again = finished(
        "t3",
        true,
        published("out/weekly-usage-chart.html", overview),
    );
    state.apply(&said(2, again));
    let names: Vec<&str> = state
        .artifacts
        .all()
        .iter()
        .map(|published| published.artifact.name.as_str())
        .collect();
    assert_eq!(names, ["test-report", "weekly-usage-chart"]);

    state.apply(&AppEvent::ConversationCleared { way_back: None });
    assert!(state.artifacts.is_empty(), "a new conversation has none");

    // A conversation carried on brings its own pages back, in place of what was held.
    state.apply(&said(3, started("t4", "out/test-report.html")));
    let done = finished("t4", true, published("out/test-report.html", report));
    state.apply(&said(3, done));
    state.apply(&AppEvent::ConversationResumed {
        conversation: argonauts_core::names::ConversationId::numbered(1),
        title: "write it up".into(),
        worker: None,
        turns: Vec::new(),
        artifacts: vec![argonauts_core::artifacts::Artifact {
            url: overview.into(),
            name: "weekly-usage-chart".into(),
            title: None,
            file: Some("out/weekly-usage-chart.html".into()),
            updated: false,
        }],
    });
    assert_eq!(state.artifacts.line_part(), Some(("weekly-usage-chart", 0)));
    assert_eq!(state.artifacts.latest().expect("its page").by, None);
}

#[test]
fn the_conversation_counts_the_lines_its_edits_changed_once() {
    let mut state = AppState::default();
    let turn = TurnId::numbered(1);
    let conversation = |event| AppEvent::Conversation { event };
    let agent = |event| {
        conversation(ConversationEvent::Agent {
            turn: turn.clone(),
            event,
        })
    };
    state.apply(&conversation(ConversationEvent::TurnStarted {
        turn: turn.clone(),
        text: "tidy it".into(),
        worker: worker("claude-code", "opus"),
        read_only: false,
        commit: "abc".into(),
        uncommitted: Vec::new(),
        settings_sha256: None,
        continues: false,
    }));
    let call = |id: &str, name: &str, input: serde_json::Value| AgentEvent::ToolStarted {
        id: id.into(),
        name: name.into(),
        input,
        parent: None,
    };
    let finished = |id: &str, ok: bool| AgentEvent::ToolFinished {
        id: id.into(),
        ok,
        output: None,
        parent: None,
    };
    state.apply(&agent(call(
        "edit",
        "Edit",
        serde_json::json!({"file_path": "a.rs", "old_string": "a\nb\nc\n", "new_string": "a\nB\n"}),
    )));
    state.apply(&agent(finished("edit", true)));
    // The same finish again is still one edit.
    state.apply(&agent(finished("edit", true)));
    state.apply(&agent(call(
        "write",
        "Write",
        serde_json::json!({"file_path": "b.rs", "content": "one\ntwo\nthree\n"}),
    )));
    state.apply(&agent(finished("write", true)));
    // An edit that failed changed nothing.
    state.apply(&agent(call(
        "refused",
        "Edit",
        serde_json::json!({"file_path": "c.rs", "old_string": "x", "new_string": "y"}),
    )));
    state.apply(&agent(finished("refused", false)));
    assert_eq!((state.talk.added, state.talk.removed), (4, 2));
}
