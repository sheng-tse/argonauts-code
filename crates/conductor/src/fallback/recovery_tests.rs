//! Tests for the limits service in `recovery.rs`: the recovery table, the ladder, fallback
//! modes, checkpoints, and waiting.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::events::agent_status::FallbackEvent;
use argonauts_core::events::agent_status::FallbackPlan;
use argonauts_core::events::run::{Party, WaitReason};
use argonauts_core::events::session::{
    EndReason, SessionEnd, SessionEvent, SessionStarted, Watchdog,
};
use argonauts_core::events::task::{TaskEvent, TaskState, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::fallback::CheckpointTag;
use argonauts_core::limits::{
    AgentHealth, Candidate, CandidateQuery, FallbackPlanner, HealthState, LimitsService,
    ModelState, PlanUsageBoard, PlanUsageStatus, RecoveryAction, StopContext, Trouble,
};
use argonauts_core::names::{AgentId, ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::{Access, RunnerKind};
use argonauts_core::settings::Span;
use argonauts_core::settings::agents::AgentSettings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::fallback::{
    AgentFallbacks, FallbackEntry, FallbackMode, FallbackRules, FallbackStrategy, FallbackTable,
    OnReturn, OrderStrategy, Pick,
};
use argonauts_core::settings::limit::{LimitAmount, LimitUnit, OnHit};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::{AgentEvent, ErrorCategory, LimitDimension, Spend, TaskId};
use argonauts_testkit::{FakeDirectory, FakeRunner};
use tempfile::TempDir;
use time::OffsetDateTime;

use super::{Limits, OriginalBack};
use crate::agents_file::AgentsFile;
use crate::fallback::testing::{
    Fixture, TASK, agent_id, api_model, entry, model, plan_usage, run, task, worker,
};

/// A run's journal on disk, as `after_stop` reads it.
struct Journal {
    _dir: TempDir,
    path: PathBuf,
}

fn journal(events: &[RunEvent]) -> Journal {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("events.jsonl");
    let lines: Vec<String> = events
        .iter()
        .map(|event| serde_json::to_string(event).expect("an event serializes"))
        .collect();
    std::fs::write(&path, lines.join("\n")).expect("the journal is written");
    Journal { _dir: dir, path }
}

fn context(stopped: &str, reason: EndReason, events: &Path) -> StopContext {
    StopContext {
        run: run(),
        task: task(TASK),
        state: TaskState::Running,
        role: RoleName::Worker,
        worker: worker(stopped),
        end: SessionEnd {
            cli_session: Some("v-1".to_owned()),
            exit_code: Some(1),
            signal: None,
            duration_ms: 60_000,
            reason,
            spend: Spend::default(),
            tool_calls: 3,
            result_text: None,
            structured: None,
            last_message: None,
            raw_log: None,
        },
        tries: vec!["try 1: claude-code/opus".to_owned()],
        reassigned_once: false,
        events: events.to_path_buf(),
        logged_in: None,
    }
}

async fn decide(
    limits: &Limits,
    stopped: &str,
    reason: EndReason,
    events: &[RunEvent],
) -> RecoveryAction {
    let journal = journal(events);
    limits
        .after_stop(&context(stopped, reason, &journal.path))
        .await
}

/// [`decide`] for a stop whose CLI's own login check said `logged_in`.
async fn decide_logged(
    limits: &Limits,
    stopped: &str,
    reason: EndReason,
    logged_in: Option<bool>,
) -> RecoveryAction {
    let journal = journal(&[]);
    let mut stop = context(stopped, reason, &journal.path);
    stop.logged_in = logged_in;
    limits.after_stop(&stop).await
}

/// An event of the task in the test.
fn event(seq: u64, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts: OffsetDateTime::UNIX_EPOCH,
        run_id: run(),
        task_id: Some(TaskId::new(TASK).expect("a valid task id")),
        session: None,
        source: "argonauts-code".to_owned(),
        kind,
    }
}

fn task_event(seq: u64, task: TaskEvent) -> RunEvent {
    event(seq, RunEventKind::Task(task))
}

fn tagged(n: u32) -> RunEvent {
    task_event(
        u64::from(n),
        TaskEvent::CheckpointTagged {
            n,
            commit: format!("c{n}"),
            from: worker("claude-code/opus"),
            to: vec![worker("codex/gpt-5")],
            reason: "usage limit".to_owned(),
        },
    )
}

fn usage() -> EndReason {
    EndReason::UsageLimit { resets_at: None }
}

fn fell_back_to(action: &RecoveryAction) -> (String, Option<u32>) {
    match action {
        RecoveryAction::Fallback { to, checkpoint, .. } => {
            (to.to_string(), checkpoint.as_ref().map(|tag| tag.n))
        }
        other => panic!("expected a fallback, got {other:?}"),
    }
}

fn asked(action: &RecoveryAction) -> &FallbackPlan {
    match action {
        RecoveryAction::Ask(plan) => plan,
        other => panic!("expected a question, got {other:?}"),
    }
}

fn names(candidates: &[Candidate]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| candidate.worker.to_string())
        .collect()
}

#[tokio::test]
async fn fallback_modes_ask_auto_wait() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Ask;
    let resets = OffsetDateTime::now_utc() + time::Duration::hours(2);
    let limit = EndReason::UsageLimit {
        resets_at: Some(resets),
    };

    let action = decide(&fixture.limits(), "claude-code/opus", limit.clone(), &[]).await;
    let plan = asked(&action);
    assert_eq!(plan.mode, FallbackMode::Ask);
    assert_eq!(plan.stopped, worker("claude-code/opus"));
    assert_eq!(plan.resets_at, Some(resets));
    assert!(
        plan.reason.starts_with("usage limit, resets at"),
        "{}",
        plan.reason
    );
    // Claude Code is out as a whole, so its other model isn't offered.
    assert_eq!(
        names(&plan.candidates),
        ["codex/gpt-5", "antigravity-cli/gemini-3"]
    );

    fixture.settings_mut().fallback.mode = FallbackMode::Auto;
    let limits = fixture.limits();
    assert_eq!(
        fell_back_to(&decide(&limits, "claude-code/opus", limit.clone(), &[]).await),
        ("codex/gpt-5".to_owned(), Some(1))
    );
    // A rate limit past the task's time limit counts as a usage limit.
    assert_eq!(
        fell_back_to(&decide(&limits, "claude-code/opus", EndReason::RateLimited, &[]).await).0,
        "codex/gpt-5"
    );

    fixture.settings_mut().fallback.mode = FallbackMode::Wait;
    assert_eq!(
        decide(&fixture.limits(), "claude-code/opus", limit, &[]).await,
        RecoveryAction::Wait {
            agent_id: agent_id("claude-code"),
            until: Some(resets),
            kept_off: None,
        }
    );
}

/// Every isolated session on Claude Code would load the same plugins, so a blind role that saw
/// them goes to another agent, but only to one that keeps your plugins out. Codex and agy load
/// their own MCP servers and extensions whatever they're asked, so with only them you decide.
#[tokio::test]
async fn a_blind_role_that_loaded_your_plugins_moves_only_to_an_agent_that_isolates() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Auto;
    let leaked = EndReason::IsolationBroken {
        plugins: vec!["memory-keeper".to_owned(), "lint-helper".to_owned()],
        skills: Vec::new(),
    };
    let why =
        "it loaded your plugins memory-keeper and lint-helper, which a blind role mustn't see";
    let none_isolate = Arc::new(FakeDirectory::new());
    let limits = fixture.limits().with_workers(none_isolate);
    let plan = asked(&decide(&limits, "claude-code/opus", leaked.clone(), &[]).await).clone();
    assert_eq!(
        plan.reason,
        format!("{why} · no other agent keeps your plugins out of a blind role")
    );
    assert!(plan.candidates.is_empty());

    let workers = Arc::new(FakeDirectory::new());
    workers.isolating("antigravity-cli");
    let limits = fixture.limits().with_workers(workers.clone());
    let action = decide(&limits, "claude-code/opus", leaked.clone(), &[]).await;
    assert_eq!(fell_back_to(&action).0, "antigravity-cli/gemini-3");

    fixture.settings_mut().fallback.mode = FallbackMode::Ask;
    let limits = fixture.limits().with_workers(workers);
    let action = decide(&limits, "claude-code/opus", leaked, &[]).await;
    let plan = asked(&action);
    assert_eq!(plan.reason, why);
    assert_eq!(names(&plan.candidates), ["antigravity-cli/gemini-3"]);
}

/// `[fallback.worker]`, the table the worker's fallbacks go by.
fn workers_table(fixture: &mut Fixture) -> &mut FallbackTable {
    fixture
        .settings_mut()
        .fallback
        .roles
        .entry(CrewRole::Worker)
        .or_default()
}

#[tokio::test]
async fn ask_timeout_falls_back_to_auto() {
    let mut fixture = Fixture::with_models();
    *workers_table(&mut fixture) = FallbackTable {
        mode: FallbackMode::Ask,
        ask_timeout: Span::new("10m"),
        ..FallbackTable::default()
    };
    let limits = fixture.limits();
    let claude = agent_id("claude-code");
    assert_eq!(
        limits.ask_timeout(RoleName::Worker, &claude),
        Ok(Some(Duration::from_secs(600)))
    );
    assert_eq!(limits.ask_timeout(RoleName::Reviewer, &claude), Ok(None));

    let journal = journal(&[]);
    let stop = context("claude-code/opus", usage(), &journal.path);
    assert!(matches!(
        limits.after_stop(&stop).await,
        RecoveryAction::Ask(_)
    ));
    assert_eq!(
        fell_back_to(&limits.after_ask_timeout(&stop).await),
        ("codex/gpt-5".to_owned(), Some(1))
    );

    // With nothing valid left, the approval opens again.
    fixture
        .health
        .set(&agent_id("codex"), HealthState::Down, None);
    fixture.plan_usage.observe(plan_usage(
        "antigravity-cli",
        PlanUsageStatus::Rejected,
        Some(1.0),
    ));
    let plan = limits.after_ask_timeout(&stop).await;
    assert!(asked(&plan).candidates.is_empty());
    assert!(asked(&plan).reason.ends_with("no valid fallback"));
}

/// Work done but the report out of format, as Haiku sent once: the same session is asked for
/// the report alone, and only the first time.
#[tokio::test]
async fn a_missed_report_after_real_work_resumes_once_for_the_report() {
    let fixture = Fixture::with_models();
    let limits = fixture.limits();
    let missed = || EndReason::CliError {
        category: ErrorCategory::AnswerMissed,
        message: "error_max_structured_output_retries".to_owned(),
    };
    let ended = |seq: u64| {
        let journal = journal(&[]);
        let mut end = context("claude-code/opus", missed(), &journal.path).end;
        end.cli_session = Some(format!("v-{seq}"));
        event(
            seq,
            RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
        )
    };
    let first = decide(&limits, "claude-code/opus", missed(), &[ended(1)]).await;
    let RecoveryAction::ResumeSame { message } = first else {
        panic!("{first:?}");
    };
    assert!(message.contains("send only the report"), "{message}");

    // The second miss goes the usual way, to another provider.
    let again = decide(&limits, "claude-code/opus", missed(), &[ended(1), ended(2)]).await;
    assert_eq!(fell_back_to(&again), ("codex/gpt-5".to_owned(), Some(1)));

    // A session that missed it before any work has no work to report.
    let journal = journal(&[ended(1)]);
    let mut stop = context("claude-code/opus", missed(), &journal.path);
    stop.end.tool_calls = 0;
    assert_eq!(
        fell_back_to(&limits.after_stop(&stop).await),
        ("codex/gpt-5".to_owned(), Some(1))
    );
}

#[tokio::test]
async fn retry_ladder() {
    let fixture = Fixture::with_models();
    let limits = fixture.limits();
    // The ladder acts in any fallback mode, and skips the same provider.
    for reason in [
        EndReason::Stuck {
            watchdog: Watchdog::Inactivity,
            calls: None,
        },
        EndReason::Crashed {
            message: "exit 139".to_owned(),
        },
        EndReason::StartFailed {
            message: "no such binary".to_owned(),
        },
        EndReason::CliError {
            category: ErrorCategory::Cli,
            message: "unexpected".to_owned(),
        },
    ] {
        assert_eq!(
            fell_back_to(&decide(&limits, "claude-code/opus", reason, &[]).await),
            ("codex/gpt-5".to_owned(), Some(1))
        );
    }

    let journal = journal(&[]);
    let mut stop = context(
        "claude-code/opus",
        EndReason::Stuck {
            watchdog: Watchdog::Repeat,
            calls: None,
        },
        &journal.path,
    );
    stop.reassigned_once = true;
    match limits.after_stop(&stop).await {
        RecoveryAction::AskYou { reason } => {
            assert!(reason.contains("already retried"), "{reason}");
        }
        other => panic!("expected the task to need you, got {other:?}"),
    }

    stop.reassigned_once = false;
    fixture
        .health
        .set(&agent_id("codex"), HealthState::Down, None);
    fixture
        .health
        .set(&agent_id("antigravity-cli"), HealthState::Down, None);
    match limits.after_stop(&stop).await {
        RecoveryAction::AskYou { reason } => {
            assert!(reason.contains("no agent of another provider"), "{reason}");
        }
        other => panic!("expected the task to need you, got {other:?}"),
    }
}

/// The run that showed it: a crew of one local Qwen on OpenCode, with OpenCode itself set to
/// that Qwen and the other CLIs off. The bare `opencode` names no model, so it once counted as
/// another provider's and took the stuck task, only to run the same Qwen again.
fn one_qwen() -> Fixture {
    let mut fixture = Fixture::new();
    let settings = fixture.settings_mut();
    for cli in ["claude-code", "codex", "antigravity-cli"] {
        let off = AgentSettings {
            enabled: false,
            ..AgentSettings::default()
        };
        settings.agents.insert(cli.to_owned(), off);
    }
    let opencode = AgentSettings {
        model: Some("ollama/qwen3-coder:30b-64k".to_owned()),
        ..AgentSettings::default()
    };
    settings.agents.insert("opencode".to_owned(), opencode);
    settings.crew = [QWEN, "opencode"].into_iter().map(worker).collect();
    fixture
        .registry
        .add(model("opencode", "ollama/qwen3-coder:30b-64k", "alibaba"));
    fixture
}

const QWEN: &str = "opencode/ollama/qwen3-coder:30b-64k";

fn repeated() -> EndReason {
    EndReason::Stuck {
        watchdog: Watchdog::Repeat,
        calls: None,
    }
}

#[tokio::test]
async fn a_crew_whose_only_worker_is_one_qwen_asks_instead_of_running_it_again() {
    let fixture = one_qwen();
    let shortlist = fixture.planner().shortlist(&CandidateQuery {
        all_valid: true,
        ..fixture.query(QWEN)
    });
    assert!(names(&shortlist.candidates).is_empty(), "{shortlist:?}");
    match decide(&fixture.limits(), QWEN, repeated(), &[]).await {
        RecoveryAction::AskYou { reason } => {
            assert!(reason.contains("no agent of another provider"), "{reason}");
        }
        other => panic!("expected the task to need you, got {other:?}"),
    }
}

/// The same crew with OpenCode left unset: its runner names the Qwen its own list marks
/// Default, so the bare `opencode` is that Qwen too.
#[tokio::test]
async fn a_bare_cli_counts_as_the_model_its_runner_names() {
    let mut fixture = one_qwen();
    fixture.settings_mut().agents.remove("opencode");
    let workers = Arc::new(FakeDirectory::new());
    workers.add(FakeRunner::cli(QWEN, "alibaba"));
    workers.add(FakeRunner::cli("opencode", "alibaba").running(QWEN));
    let shortlist = fixture
        .planner()
        .with_workers(workers.clone())
        .shortlist(&CandidateQuery {
            all_valid: true,
            ..fixture.query(QWEN)
        });
    assert!(names(&shortlist.candidates).is_empty(), "{shortlist:?}");
    let limits = fixture.limits_with_workers(workers);
    match decide(&limits, QWEN, repeated(), &[]).await {
        RecoveryAction::AskYou { reason } => {
            assert!(reason.contains("no agent of another provider"), "{reason}");
        }
        other => panic!("expected the task to need you, got {other:?}"),
    }
}

#[tokio::test]
async fn a_bare_cli_set_to_another_model_still_takes_the_task() {
    let mut fixture = one_qwen();
    fixture.settings_mut().agents.insert(
        "codex".to_owned(),
        AgentSettings {
            model: Some("gpt-5".to_owned()),
            ..AgentSettings::default()
        },
    );
    fixture.settings_mut().crew = [QWEN, "opencode", "codex"]
        .into_iter()
        .map(worker)
        .collect();
    fixture.registry.add(model("codex", "gpt-5", "openai"));
    assert_eq!(
        fell_back_to(&decide(&fixture.limits(), QWEN, repeated(), &[]).await).0,
        "codex"
    );
}

#[tokio::test]
async fn a_stuck_task_follows_the_fallback_mode() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Ask;
    let plan_of = |action: RecoveryAction| match action {
        RecoveryAction::Ask(plan) => plan,
        other => panic!("expected a fallback question, got {other:?}"),
    };
    let plan = plan_of(decide(&fixture.limits(), "claude-code/opus", repeated(), &[]).await);
    assert_eq!(plan.stopped, worker("claude-code/opus"));
    // Another provider's models, as the ladder would pick from.
    assert_eq!(
        names(&plan.candidates),
        ["codex/gpt-5", "antigravity-cli/gemini-3"]
    );
    // Waiting can't help an agent that's still there, so it asks too.
    fixture.settings_mut().fallback.mode = FallbackMode::Wait;
    plan_of(decide(&fixture.limits(), "claude-code/opus", repeated(), &[]).await);
    // Auto, the default, goes on as it did.
    fixture.settings_mut().fallback.mode = FallbackMode::Auto;
    assert_eq!(
        fell_back_to(&decide(&fixture.limits(), "claude-code/opus", repeated(), &[]).await),
        ("codex/gpt-5".to_owned(), Some(1))
    );
}

/// A 401 whose CLI says it's logged in, or says nothing, was passing: the task goes on to the
/// next candidate as for an outage, and nothing tells you to log in.
#[tokio::test]
async fn a_passing_sign_in_error_hands_the_task_on_without_a_login() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Auto;
    let limits = fixture.limits();
    let unauthorized = || EndReason::Auth {
        message: "workspace routing discovery unauthorized (401)".to_owned(),
    };
    for logged_in in [Some(true), None] {
        let action = decide_logged(&limits, "claude-code/opus", unauthorized(), logged_in).await;
        assert_eq!(fell_back_to(&action).0, "codex/gpt-5", "{logged_in:?}");
        let RecoveryAction::Fallback { note, .. } = &action else {
            unreachable!()
        };
        assert!(
            note.why_stopped.starts_with("a sign-in error"),
            "{}",
            note.why_stopped
        );
        assert!(
            !note.why_stopped.contains("logged out"),
            "{}",
            note.why_stopped
        );
    }

    // Asked how to go on, the question offers no login when the CLI says it's logged in.
    fixture.settings_mut().fallback.mode = FallbackMode::Ask;
    let limits = fixture.limits();
    let action = decide_logged(&limits, "claude-code/opus", unauthorized(), Some(true)).await;
    let plan = asked(&action);
    assert!(!plan.reason.contains("log in"), "{}", plan.reason);
}

#[tokio::test]
async fn logged_out_and_billing_ask_with_reason() {
    let mut fixture = Fixture::with_models();
    // Even in auto mode, a login or a top-up is yours to do.
    fixture.settings_mut().fallback.mode = FallbackMode::Auto;
    let limits = fixture.limits();
    let auth = EndReason::Auth {
        message: "token expired".to_owned(),
    };
    let action = decide_logged(&limits, "claude-code/opus", auth, Some(false)).await;
    let plan = asked(&action);
    assert!(
        plan.reason.contains("log in to claude-code"),
        "{}",
        plan.reason
    );
    assert!(plan.reason.contains("token expired"));
    assert_eq!(
        names(&plan.candidates),
        ["codex/gpt-5", "antigravity-cli/gemini-3"]
    );

    let billing = EndReason::Billing {
        message: "credits ran out".to_owned(),
    };
    let action = decide(&limits, "claude-code/opus", billing.clone(), &[]).await;
    assert!(
        asked(&action).reason.contains("top up"),
        "{}",
        asked(&action).reason
    );
    let action = decide_logged(
        &limits,
        "claude-code/opus",
        EndReason::CliError {
            category: ErrorCategory::Auth,
            message: String::new(),
        },
        Some(false),
    )
    .await;
    assert!(asked(&action).reason.contains("log in to claude-code"));

    // After a billing stop, never a silent switch to another paid API, even an allowed one.
    fixture
        .registry
        .add(api_model("openrouter", "kimi", "moonshot", 1.0));
    let settings = fixture.settings_mut();
    settings.fallback.allow_api_billing = true;
    settings.fallback.order = vec![entry("openrouter/kimi"), entry("codex/gpt-5")];
    let limits = fixture.limits();
    let journal = journal(&[]);
    let stop = context("claude-code/opus", billing, &journal.path);
    assert!(matches!(
        limits.after_stop(&stop).await,
        RecoveryAction::Ask(_)
    ));
    assert_eq!(
        fell_back_to(&limits.after_ask_timeout(&stop).await).0,
        "codex/gpt-5"
    );
}

#[tokio::test]
async fn outage_follows_fallback_mode() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Ask;
    fixture.health.set(
        &agent_id("claude-code"),
        HealthState::Down,
        Some("overloaded"),
    );
    let outage = || EndReason::CliError {
        category: ErrorCategory::Overloaded,
        message: "529 overloaded".to_owned(),
    };

    let action = decide(&fixture.limits(), "claude-code/opus", outage(), &[]).await;
    assert!(asked(&action).reason.starts_with("overloaded"));

    // The role's own mode wins over the top-level one.
    workers_table(&mut fixture).mode = FallbackMode::Auto;
    assert_eq!(
        fell_back_to(&decide(&fixture.limits(), "claude-code/opus", outage(), &[]).await).0,
        "codex/gpt-5"
    );

    workers_table(&mut fixture).mode = FallbackMode::Wait;
    let (waits_for, _) =
        waited(&decide(&fixture.limits(), "claude-code/opus", outage(), &[]).await);
    assert_eq!(waits_for, agent_id("claude-code"));
}

fn waited(action: &RecoveryAction) -> (AgentId, OffsetDateTime) {
    match action {
        RecoveryAction::Wait {
            agent_id,
            until: Some(until),
            ..
        } => (agent_id.clone(), *until),
        other => panic!("expected a wait with a wake time, got {other:?}"),
    }
}

/// A wait the board can't end, since the agent never went Down or reported a reset, still
/// wakes once the breaker's cool-down has passed.
#[tokio::test]
async fn a_wait_with_no_known_return_wakes_after_the_cooldown() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Wait;
    let limits = fixture.limits();
    let cooldown = time::Duration::minutes(5);
    for reason in [
        EndReason::CliError {
            category: ErrorCategory::Network,
            message: "stream disconnected".to_owned(),
        },
        EndReason::CliError {
            category: ErrorCategory::Timeout,
            message: "504 gateway timeout".to_owned(),
        },
        EndReason::RateLimited,
        usage(),
    ] {
        let waiting_at = OffsetDateTime::now_utc();
        let action = decide(&limits, "claude-code/opus", reason.clone(), &[]).await;
        let (claude, until) = waited(&action);
        assert!(until >= waiting_at + cooldown, "{reason:?}");
        assert!(until <= OffsetDateTime::now_utc() + cooldown, "{reason:?}");

        let mut file = AgentsFile::default();
        file.agent_ids.entry(claude.0.clone()).or_default().health = Some(AgentHealth {
            agent_id: claude.clone(),
            state: HealthState::Degraded,
            since: waiting_at,
            reason: None,
            next_test_at: None,
            disabled: None,
        });
        assert!(!file.agent_back(&claude, waiting_at, until));
        assert!(!file.wait_over(&claude, Some(until), waiting_at, waiting_at));
        assert!(file.wait_over(&claude, Some(until), waiting_at, until));
    }
}

#[tokio::test]
async fn all_out_waits_until_earliest_reset() {
    let fixture = Fixture::with_models();
    let limits = fixture.limits();
    let now = OffsetDateTime::now_utc();
    let workers = [
        worker("claude-code/opus"),
        worker("claude-code/sonnet"),
        worker("codex/gpt-5"),
        worker("antigravity-cli/gemini-3"),
    ];
    assert_eq!(limits.all_out(&workers), None);
    assert_eq!(limits.all_out(&[]), None);

    let rejected = |name: &str, resets_at| {
        let mut reading = plan_usage(name, PlanUsageStatus::Rejected, Some(1.0));
        reading.resets_at = resets_at;
        reading
    };
    fixture.plan_usage.observe(rejected(
        "claude-code",
        Some(now + time::Duration::hours(2)),
    ));
    fixture
        .plan_usage
        .observe(rejected("codex", Some(now + time::Duration::hours(1))));
    assert_eq!(limits.all_out(&workers), None, "agy still works");

    // Down with no known test time: it counts as out, but not for the earliest reset.
    fixture
        .health
        .set(&agent_id("antigravity-cli"), HealthState::Down, None);
    assert_eq!(
        limits.all_out(&workers),
        Some(WaitReason::AllAgentsOut {
            until: Some(now + time::Duration::hours(1)),
        })
    );

    // A reading whose reset has passed no longer counts.
    fixture
        .plan_usage
        .observe(rejected("codex", Some(now - time::Duration::minutes(1))));
    assert_eq!(limits.all_out(&workers), None);
}

#[tokio::test]
async fn agents_at_your_plan_limit_count_as_out_until_their_windows_reset() {
    let mut fixture = Fixture::with_models();
    let now = OffsetDateTime::now_utc();
    let workers = [worker("claude-code/opus"), worker("codex/gpt-5")];
    let used = |name: &str, used: f64, hours: i64| {
        let mut reading = plan_usage(name, PlanUsageStatus::Allowed, Some(used));
        reading.resets_at = Some(now + time::Duration::hours(hours));
        reading
    };
    fixture.plan_usage.observe(used("claude-code", 0.9, 2));
    fixture.plan_usage.observe(used("codex", 0.85, 1));
    assert_eq!(
        fixture.limits().all_out(&workers),
        None,
        "with no limit of yours, a plan is used until it runs out"
    );

    fixture.settings_mut().limits.spend.plan_usage = Some(0.8);
    assert_eq!(
        fixture.limits().all_out(&workers),
        Some(WaitReason::AllAgentsOut {
            until: Some(now + time::Duration::hours(1)),
        })
    );
    fixture.settings_mut().limits.spend.plan_usage = Some(0.88);
    assert_eq!(
        fixture.limits().all_out(&workers),
        None,
        "codex is under it"
    );
}

#[tokio::test]
async fn auto_waits_when_the_whole_crew_is_out() {
    let mut fixture = Fixture::with_models();
    let settings = fixture.settings_mut();
    settings.fallback.mode = FallbackMode::Auto;
    settings.crew = vec![worker("claude-code/opus")].into();
    settings.crew.main = worker("claude-code/opus");
    settings.fallback.order.clear();
    let resets = OffsetDateTime::now_utc() + time::Duration::minutes(5);
    let mut reading = plan_usage("claude-code", PlanUsageStatus::Rejected, Some(1.0));
    reading.resets_at = Some(resets);
    fixture.plan_usage.observe(reading);
    let limit = EndReason::UsageLimit {
        resets_at: Some(resets),
    };

    let action = decide(&fixture.limits(), "claude-code/opus", limit.clone(), &[]).await;
    assert_eq!(
        action,
        RecoveryAction::RunWaits(WaitReason::AllAgentsOut {
            until: Some(resets)
        })
    );

    // A crew member that still works takes the task instead.
    fixture.settings_mut().crew.add(worker("codex/gpt-5"));
    let action = decide(&fixture.limits(), "claude-code/opus", limit, &[]).await;
    assert_eq!(fell_back_to(&action).0, "codex/gpt-5");
}

#[tokio::test]
async fn model_retired_hands_to_successor() {
    let fixture = Fixture::with_models();
    let retired = |model_id: &str, provider: &str, successor: Option<&str>| {
        let mut entry = model("claude-code", model_id, provider);
        entry.retired = true;
        entry.successor = successor.map(str::to_owned);
        entry
    };
    fixture
        .registry
        .add(retired("opus-4", "anthropic", Some("opus-4.5")));
    fixture
        .registry
        .add(retired("opus-4.5", "anthropic", Some("opus-5")));
    fixture
        .registry
        .add(model("claude-code", "opus-5", "anthropic"));
    fixture.registry.add(retired("haiku-3", "anthropic", None));
    fixture
        .registry
        .add(retired("odd-1", "anthropic", Some("other-2")));
    fixture
        .registry
        .add(model("claude-code", "other-2", "other"));
    let limits = fixture.limits();
    let gone = |model_id: &str| EndReason::ModelUnavailable {
        model: model_id.to_owned(),
    };

    // Retired successors are skipped, to the first one that works, on the same agent.
    assert_eq!(
        fell_back_to(&decide(&limits, "claude-code/opus-4", gone("opus-4"), &[]).await),
        ("claude-code/opus-5".to_owned(), Some(1))
    );
    // The reason's text is never read as a model.
    let said = gone("There's an issue with the selected model (opus-4).");
    assert!(matches!(
        decide(&limits, "claude-code", said.clone(), &[]).await,
        RecoveryAction::Ask(_)
    ));
    // A default model is the one its CLI reported at the start of the session.
    let reported = [started(1, "claude-code"), init(2, "opus-4")];
    assert_eq!(
        fell_back_to(&decide(&limits, "claude-code", said.clone(), &reported).await).0,
        "claude-code/opus-5"
    );

    for (stopped, model_id) in [
        ("claude-code/haiku-3", "haiku-3"),
        ("claude-code/odd-1", "odd-1"),
    ] {
        let action = decide(&limits, stopped, gone(model_id), &[]).await;
        assert!(asked(&action).reason.contains("no successor"), "{stopped}");
    }

    // Retired successors aren't followed onto another agent, which could bill differently.
    fixture.registry.add(retired(
        "far-1",
        "anthropic",
        Some("antigravity-cli/claude-5"),
    ));
    fixture
        .registry
        .add(model("antigravity-cli", "claude-5", "anthropic"));
    let action = decide(&limits, "claude-code/far-1", gone(""), &[]).await;
    assert!(asked(&action).reason.contains("no successor"));

    // Without the registry, you decide.
    let bare = Limits::new(
        std::sync::Arc::new(fixture.settings.clone()),
        fixture.plan_usage.clone(),
        fixture.health.clone(),
        std::sync::Arc::new(fixture.planner()),
    );
    assert!(matches!(
        decide(&bare, "claude-code/opus-4", gone("opus-4"), &[]).await,
        RecoveryAction::Ask(_)
    ));
}

/// An agent on its default model, set by `agents.<id>.model`, whose successor the registry names
/// as a full `agent/model` reference.
#[tokio::test]
async fn a_default_model_hands_to_a_full_reference_successor() {
    let mut fixture = Fixture::with_models();
    fixture
        .settings_mut()
        .agents
        .entry("claude-code".to_owned())
        .or_default()
        .model = Some("claude-old-1".to_owned());
    let mut old = model("claude-code", "claude-old-1", "anthropic");
    old.successor = Some("claude-code/haiku".to_owned());
    fixture.registry.add(old);
    fixture
        .registry
        .add(model("claude-code", "haiku", "anthropic"));
    let limits = fixture.limits();

    // The CLI runner names the model it asked for, and a driven session leaves it empty.
    for named in ["claude-old-1", ""] {
        let reason = EndReason::ModelUnavailable {
            model: named.to_owned(),
        };
        let action = decide(&limits, "claude-code", reason, &[]).await;
        match &action {
            RecoveryAction::Fallback { to, note, .. } => {
                assert_eq!(*to, ModelRef::new("claude-code", Some("haiku".to_owned())));
                assert_eq!(
                    note.why_stopped,
                    "model claude-old-1 is no longer available"
                );
            }
            other => panic!("expected a fallback, got {other:?}"),
        }
    }
}

fn started(seq: u64, stopped: &str) -> RunEvent {
    RunEvent {
        session: SessionKey::parse("s-001"),
        ..event(
            seq,
            RunEventKind::Session(SessionEvent::Started(Box::new(SessionStarted {
                worker: worker(stopped),
                agent_id: worker(stopped).agent_id(),
                provider: "anthropic".to_owned(),
                kind: RunnerKind::Cli,
                role: RoleName::Worker,
                step: None,
                access: Access::Build,
                lifetime: SessionLifetime::Task,
                resumed_from: None,
                follow_key: None,
                workflow_step: None,
                label: None,
                asked: None,
            }))),
        )
    }
}

fn init(seq: u64, model_id: &str) -> RunEvent {
    RunEvent {
        session: SessionKey::parse("s-001"),
        ..event(
            seq,
            RunEventKind::Agent(AgentEvent::Init {
                session_id: Some("v-1".to_owned()),
                model: Some(model_id.to_owned()),
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            }),
        )
    }
}

#[tokio::test]
async fn context_full_rotates() {
    let fixture = Fixture::with_models();
    let limits = fixture.limits();
    for reason in [
        EndReason::ContextFull,
        EndReason::CliError {
            category: ErrorCategory::ContextFull,
            message: "prompt is too long".to_owned(),
        },
        // A resume the CLI has no conversation for starts fresh at once, as no crash.
        EndReason::CliError {
            category: ErrorCategory::SessionLost,
            message: "No conversation found with session ID: v-1".to_owned(),
        },
    ] {
        match decide(&limits, "claude-code/opus", reason, &[]).await {
            RecoveryAction::Rotate { note } => {
                assert_eq!(note.task, task(TASK));
                assert_eq!(note.work.branch, "argonauts/run-01/task-003");
            }
            other => panic!("expected a rotation, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_reached_limit_goes_by_when_reached() {
    let mut fixture = Fixture::with_models();
    let hit = || EndReason::LimitHit {
        dimension: LimitDimension::Spend,
    };
    // The task falls back unless you say to stop.
    assert_eq!(
        decide(&fixture.limits(), "claude-code/opus", hit(), &[]).await,
        RecoveryAction::LimitHit(OnHit::Fallback)
    );
    for on_hit in [OnHit::Stop, OnHit::Fallback] {
        fixture.settings_mut().limits.spend.when_reached = on_hit;
        assert_eq!(
            decide(&fixture.limits(), "claude-code/opus", hit(), &[]).await,
            RecoveryAction::LimitHit(on_hit)
        );
    }
}

#[tokio::test]
async fn max_fallbacks_asks_you() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Auto;
    let three = [tagged(1), tagged(2), tagged(3)];
    match decide(&fixture.limits(), "claude-code/opus", usage(), &three).await {
        RecoveryAction::AskYou { reason } => assert_eq!(
            reason, "\"Middleware\" stopped after 3 fallbacks, the most /fallback allows",
            "the task by its title and the limit by where it's set, never an id or a key"
        ),
        other => panic!("expected the task to need you, got {other:?}"),
    }

    fixture.settings_mut().fallback.max_fallbacks = Some(4);
    assert_eq!(
        fell_back_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &three).await),
        ("codex/gpt-5".to_owned(), Some(4))
    );

    // With checkpoints off, fallback tries count instead.
    let settings = fixture.settings_mut();
    settings.fallback.max_fallbacks = Some(2);
    settings.fallback.checkpoint = Some(false);
    let tries: Vec<RunEvent> = (1..=2)
        .map(|try_number| {
            task_event(
                u64::from(try_number),
                TaskEvent::Try {
                    try_number,
                    worker: worker("codex/gpt-5"),
                    cause: TryCause::Fallback,
                    told: Vec::new(),
                },
            )
        })
        .collect();
    assert!(matches!(
        decide(&fixture.limits(), "claude-code/opus", usage(), &tries).await,
        RecoveryAction::AskYou { .. }
    ));

    // Waiting isn't another fallback.
    fixture.settings_mut().fallback.mode = FallbackMode::Wait;
    assert!(matches!(
        decide(&fixture.limits(), "claude-code/opus", usage(), &tries).await,
        RecoveryAction::Wait { .. }
    ));
}

#[tokio::test]
async fn a_checkpointed_race_is_one_fallback() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Auto;
    let racers = [
        "codex/gpt-5",
        "antigravity-cli/gemini-3",
        "claude-code/sonnet",
    ];
    let mut race = vec![tagged(1)];
    race.extend((2u32..).zip(racers).map(|(try_number, racer)| {
        task_event(
            u64::from(try_number),
            TaskEvent::Try {
                try_number,
                worker: worker(racer),
                cause: TryCause::Fallback,
                told: Vec::new(),
            },
        )
    }));
    // Three racers are still one round, so the default of three fallbacks is far off.
    assert_eq!(
        fell_back_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &race).await).1,
        Some(2)
    );
}

/// An event of `task`.
fn on_task(task: &str, seq: u64, kind: TaskEvent) -> RunEvent {
    RunEvent {
        task_id: Some(TaskId::new(task).expect("a valid task id")),
        ..task_event(seq, kind)
    }
}

/// `task` falling back from Claude Code to `to`, as the engine logs it.
fn logged_fallback(task: &str, seq: u64, to: &str) -> RunEvent {
    on_task(
        task,
        seq,
        TaskEvent::Try {
            try_number: 2,
            worker: worker(to),
            cause: TryCause::Fallback,
            told: Vec::new(),
        },
    )
}

#[tokio::test]
async fn spread_rotates_an_agents_tasks_across_candidates() {
    let mut fixture = Fixture::with_models();
    let settings = fixture.settings_mut();
    settings.fallback.mode = FallbackMode::Auto;
    settings.fallback.strategy = Some(OrderStrategy::Spread);
    let tasks = ["task-003", "task-004", "task-005"];
    // Each task was running on Claude Code when it went out.
    let running: Vec<RunEvent> = (0u64..)
        .zip(tasks)
        .flat_map(|(n, id)| {
            [
                on_task(
                    id,
                    2 * n + 1,
                    TaskEvent::Try {
                        try_number: 1,
                        worker: worker("claude-code/opus"),
                        cause: TryCause::First,
                        told: Vec::new(),
                    },
                ),
                on_task(
                    id,
                    2 * n + 2,
                    TaskEvent::State {
                        from: None,
                        to: TaskState::Running,
                        reason: String::new(),
                    },
                ),
            ]
        })
        .collect();
    let stop = |id: &str, events: &Path| StopContext {
        task: task(id),
        ..context("claude-code/opus", usage(), events)
    };

    // Every stop reads the same log and boards, before the engine logs any fallback.
    let log = journal(&running);
    let limits = fixture.limits();
    let mut to = Vec::new();
    for id in tasks {
        to.push(fell_back_to(&limits.after_stop(&stop(id, &log.path)).await).0);
    }
    assert_eq!(
        to,
        ["codex/gpt-5", "antigravity-cli/gemini-3", "codex/gpt-5"]
    );

    // A fresh service reads the same from the log once the fallbacks are logged.
    let mut logged = running.clone();
    logged.push(logged_fallback("task-003", 7, "codex/gpt-5"));
    logged.push(logged_fallback("task-004", 8, "antigravity-cli/gemini-3"));
    let log = journal(&logged);
    assert_eq!(
        fell_back_to(
            &fixture
                .limits()
                .after_stop(&stop("task-005", &log.path))
                .await
        )
        .0,
        "codex/gpt-5"
    );

    // Asked instead, the plan puts the least loaded first and names the tasks still to move.
    fixture.settings_mut().fallback.mode = FallbackMode::Ask;
    let mut one_moved = running;
    one_moved.push(logged_fallback("task-003", 7, "codex/gpt-5"));
    let log = journal(&one_moved);
    let action = fixture
        .limits()
        .after_stop(&stop("task-004", &log.path))
        .await;
    let plan = asked(&action);
    assert_eq!(plan.strategy, FallbackStrategy::Spread);
    assert_eq!(
        names(&plan.candidates),
        ["antigravity-cli/gemini-3", "codex/gpt-5"]
    );
    assert_eq!(
        plan.also_moving,
        [TaskId::new("task-005").expect("a valid task id")]
    );

    // The strategy `first` sends every task to the same candidate and lists no others.
    fixture.settings_mut().fallback.strategy = None;
    let plan = fixture
        .limits()
        .after_stop(&stop("task-004", &log.path))
        .await;
    assert!(asked(&plan).also_moving.is_empty());
    assert_eq!(
        names(&asked(&plan).candidates),
        ["codex/gpt-5", "antigravity-cli/gemini-3"]
    );
}

#[tokio::test]
async fn several_fallbacks_work_at_once_when_together_says_so() {
    let mut fixture = Fixture::with_models();
    // One at a time unless a table says otherwise.
    assert_eq!(
        fell_back_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await).0,
        "codex/gpt-5"
    );

    let role = workers_table(&mut fixture);
    role.mode = FallbackMode::Auto;
    role.together = Some(2);
    role.pick = Some(Pick::Judge);
    role.replace.insert(
        agent_id("claude-code"),
        AgentFallbacks {
            with: vec![entry("codex/gpt-5"), entry("antigravity-cli/gemini-3")],
            ..AgentFallbacks::default()
        },
    );
    let limits = fixture.limits();
    match decide(&limits, "claude-code/opus", usage(), &[]).await {
        RecoveryAction::Race {
            to,
            pick,
            checkpoint,
            ..
        } => {
            assert_eq!(
                to,
                [worker("codex/gpt-5"), worker("antigravity-cli/gemini-3")]
            );
            assert_eq!(pick, Pick::Judge);
            assert_eq!(checkpoint.map(|tag| tag.n), Some(1));
        }
        other => panic!("expected two at once, got {other:?}"),
    }

    // Several at once needs two that can take it; with one left it's a plain fallback.
    fixture
        .health
        .set(&agent_id("antigravity-cli"), HealthState::Down, None);
    assert_eq!(
        fell_back_to(&decide(&limits, "claude-code/opus", usage(), &[]).await).0,
        "codex/gpt-5"
    );
}

#[tokio::test]
async fn checkpoint_tag_on_fallback_and_race() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Auto;
    let expected = |n| CheckpointTag {
        run: run(),
        task: TaskId::new(TASK).expect("a valid task id"),
        n,
    };
    match decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await {
        RecoveryAction::Fallback { checkpoint, .. } => {
            let tag = checkpoint.expect("a checkpoint tag");
            assert_eq!(tag, expected(1));
            assert_eq!(tag.tag_name(), "argonauts/run-01/task-003/fallback-1");
        }
        other => panic!("expected a fallback, got {other:?}"),
    }
    // The next fallback of the same task is fallback-2.
    assert_eq!(
        fell_back_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &[tagged(1)]).await).1,
        Some(2)
    );

    fixture.settings_mut().fallback.together = Some(2);
    match decide(&fixture.limits(), "claude-code/opus", usage(), &[tagged(1)]).await {
        RecoveryAction::Race { checkpoint, .. } => assert_eq!(checkpoint, Some(expected(2))),
        other => panic!("expected a race, got {other:?}"),
    }

    fixture.settings_mut().fallback.checkpoint = Some(false);
    match decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await {
        RecoveryAction::Race { checkpoint, .. } => assert_eq!(checkpoint, None),
        other => panic!("expected a race, got {other:?}"),
    }
    fixture.settings_mut().fallback.together = None;
    assert_eq!(
        fell_back_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await).1,
        None
    );
}

#[tokio::test]
async fn auto_fallback_carries_the_entry_rules() {
    let mut fixture = Fixture::with_models();
    let settings = fixture.settings_mut();
    settings.fallback.mode = FallbackMode::Auto;
    settings.fallback.order = vec![FallbackEntry::WithRules(FallbackRules {
        model: worker("codex/gpt-5"),
        max_spend_usd: None,
        instructions: Some("Only finish the failing test.".to_owned()),
        read_only: true,
        allow_api_billing: false,
    })];
    match decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await {
        RecoveryAction::Fallback { to, note, .. } => {
            assert_eq!(to, worker("codex/gpt-5"));
            assert!(
                note.notes
                    .contains(&"Instructions: Only finish the failing test.".to_owned())
            );
            assert!(note.notes.iter().any(|note| note.starts_with("Read-only")));
        }
        other => panic!("expected a fallback, got {other:?}"),
    }
}

#[tokio::test]
async fn journal_feeds_the_plan_and_a_missing_one_is_noted() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.mode = FallbackMode::Ask;
    let limits = fixture.limits();
    let check = argonauts_core::events::task::CheckOutcome {
        name: "tests".to_owned(),
        kind: argonauts_core::events::task::CheckKind::Command,
        passed: false,
        command: None,
        exit_code: Some(1),
        duration_ms: 5,
        summary: "1 failed".to_owned(),
        output_tail: String::new(),
        violations: Vec::new(),
        sandboxed: true,
        skipped: false,
    };
    let checked = event(9, RunEventKind::Check(check));
    let action = decide(&limits, "claude-code/opus", usage(), &[tagged(1), checked]).await;
    assert!(asked(&action).checks_ran);

    let missing = context(
        "claude-code/opus",
        usage(),
        Path::new("/nonexistent/events.jsonl"),
    );
    match fixture.limits().after_stop(&missing).await {
        RecoveryAction::Ask(plan) => assert!(!plan.checks_ran),
        other => panic!("expected a question, got {other:?}"),
    }
    let rotate = context(
        "claude-code/opus",
        EndReason::ContextFull,
        Path::new("/nonexistent/events.jsonl"),
    );
    match limits.after_stop(&rotate).await {
        RecoveryAction::Rotate { note } => {
            assert!(
                note.notes
                    .iter()
                    .any(|note| note.contains("couldn't be read"))
            );
        }
        other => panic!("expected a rotation, got {other:?}"),
    }
}

#[tokio::test]
async fn paused_resumes_and_stopped_asks_you() {
    let limits = Fixture::with_models().limits();
    assert!(matches!(
        decide(&limits, "claude-code/opus", EndReason::Paused, &[]).await,
        RecoveryAction::ResumeSame { .. }
    ));
    assert!(matches!(
        decide(&limits, "claude-code/opus", EndReason::Stopped, &[]).await,
        RecoveryAction::AskYou { .. }
    ));
    assert_eq!(
        decide(&limits, "claude-code/opus", EndReason::Completed, &[]).await,
        RecoveryAction::Proceed
    );
}

#[test]
fn session_limits_follow_limit_and_watchdogs() {
    let mut fixture = Fixture::new();
    let spec = task(TASK);

    // Nothing is set, so nothing stops a task: only a hung or looping session is caught.
    let unlimited = fixture
        .limits()
        .session_limits(Some(&spec), RoleName::Worker);
    assert_eq!(unlimited.limit, None);
    assert_eq!(unlimited.native_cap_usd, None);
    assert_eq!(unlimited.max_minutes, None);
    assert_eq!(unlimited.max_tool_calls, None);
    assert_eq!(unlimited.inactivity_min, Some(15.0));
    assert_eq!(unlimited.repeat_limit, Some(10));
    assert_eq!(unlimited.no_progress_calls, Some(8));

    let settings = fixture.settings_mut();
    settings.limits.spend.per_task_usd = Some(3.0);
    settings.watchdogs.wall_clock_min = Some(30.0);
    settings.watchdogs.max_tool_calls = Some(150);
    let limits = fixture.limits();
    let worker = limits.session_limits(Some(&spec), RoleName::Worker);
    assert_eq!(
        worker.limit,
        Some(LimitAmount {
            unit: LimitUnit::Usd,
            amount: 3.0
        })
    );
    assert!((worker.native_cap_usd.expect("a dollar backstop") - 3.3).abs() < 1e-9);
    assert!((worker.soft_stop - 0.85).abs() < 1e-9);
    assert_eq!(worker.max_minutes, Some(30.0));
    assert_eq!(worker.max_tool_calls, Some(150));
    assert_eq!(worker.inactivity_min, Some(15.0));
    assert_eq!(worker.repeat_limit, Some(10));

    // The task's own limits come first.
    let mut own = task(TASK);
    own.limit.limit = Some(LimitAmount {
        unit: LimitUnit::Tokens,
        amount: 200_000.0,
    });
    own.limit.max_minutes = Some(5.0);
    let custom = limits.session_limits(Some(&own), RoleName::Worker);
    assert_eq!(
        custom.limit.map(|limit| limit.unit),
        Some(LimitUnit::Tokens)
    );
    assert_eq!(custom.native_cap_usd, None);
    assert_eq!(custom.max_minutes, Some(5.0));

    // A session that outlives a task answers to the run's limit lines.
    fixture
        .settings_mut()
        .roles
        .entry(RoleName::Orchestrator)
        .or_default()
        .lifetime = Some(argonauts_core::settings::sessions::SessionLifetime::Run);
    let orchestrator = fixture
        .limits()
        .session_limits(None, RoleName::Orchestrator);
    assert_eq!(orchestrator.limit, None);
    assert_eq!(orchestrator.max_minutes, None);
    assert_eq!(orchestrator.inactivity_min, Some(15.0));
    assert_eq!(
        orchestrator.no_progress_calls, None,
        "no task, so no later try"
    );

    // A share that can't be read falls back to the built-in one.
    fixture.settings_mut().limits.spend.soft_stop = f64::NAN;
    let odd = fixture
        .limits()
        .session_limits(Some(&own), RoleName::Worker);
    assert!((odd.soft_stop - argonauts_core::DEFAULT_SOFT_RATIO).abs() < 1e-9);
}

/// The task started on `claude-code/opus` and auto fell back to `codex/gpt-5`.
fn fell_back_to_codex() -> Vec<RunEvent> {
    vec![
        task_event(
            1,
            TaskEvent::Try {
                try_number: 1,
                worker: worker("claude-code/opus"),
                cause: TryCause::First,
                told: Vec::new(),
            },
        ),
        event(
            2,
            RunEventKind::Fallback(FallbackEvent::FellBack {
                task: TaskId::new(TASK).expect("a valid task id"),
                to: worker("codex/gpt-5"),
                mode: FallbackMode::Auto,
                by: Party::Rules,
                context: None,
            }),
        ),
    ]
}

#[test]
fn a_returning_original_leaves_the_task_with_a_notice() {
    let fixture = Fixture::with_models();
    let limits = fixture.limits();
    let claude = agent_id("claude-code");
    let events = fell_back_to_codex();
    assert_eq!(
        limits.original_returned(&claude, "Claude Code", &events),
        vec![OriginalBack::Stays {
            task: TaskId::new(TASK).expect("a valid task id"),
            notice: "Claude Code is back · task-003 stays with codex/gpt-5 · \
                     /reassign task-003 claude-code/opus moves it back"
                .to_owned(),
        }]
    );
    // Nothing was taken from Codex.
    assert!(
        limits
            .original_returned(&agent_id("codex"), "Codex", &events)
            .is_empty()
    );

    // Moved back, or finished, it's no longer away.
    for last in [
        TaskEvent::Reassigned {
            from: Some(worker("codex/gpt-5")),
            to: worker("claude-code/opus"),
            by: Party::User,
            rule_override: None,
        },
        TaskEvent::State {
            from: Some(TaskState::Merging),
            to: TaskState::Merged,
            reason: "merged".to_owned(),
        },
    ] {
        let mut events = fell_back_to_codex();
        events.push(task_event(3, last));
        assert!(
            limits
                .original_returned(&claude, "Claude Code", &events)
                .is_empty()
        );
    }
}

#[test]
fn a_returning_original_follows_on_return() {
    let mut fixture = Fixture::with_models();
    let claude = agent_id("claude-code");
    let task_id = || TaskId::new(TASK).expect("a valid task id");

    let mut events = fell_back_to_codex();
    events.push(task_event(
        3,
        TaskEvent::QualityDebt(argonauts_core::fallback::QualityDebt {
            since: 1,
            by: worker("codex/gpt-5"),
            needs: worker("claude-code/opus"),
        }),
    ));
    assert_eq!(
        fixture
            .limits()
            .original_returned(&claude, "Claude Code", &events),
        vec![OriginalBack::Debt {
            task: task_id(),
            original: worker("claude-code/opus"),
            holder: worker("codex/gpt-5"),
            on_return: OnReturn::Ask,
        }]
    );

    // With the debt cleared, `give_back` gives the task back at its next try.
    events.push(task_event(
        4,
        TaskEvent::DebtCleared {
            by: worker("claude-code/opus"),
        },
    ));
    let fallback = &mut fixture.settings_mut().fallback;
    fallback.on_return = Some(OnReturn::GiveBack);
    assert_eq!(
        fixture
            .limits()
            .original_returned(&claude, "Claude Code", &events),
        vec![OriginalBack::GiveBack {
            task: task_id(),
            to: worker("claude-code/opus"),
        }]
    );
}

// ── Every kind of failure: whose it is, and where the task may go ────────────────────────────

fn cli_error(category: ErrorCategory) -> EndReason {
    EndReason::CliError {
        category,
        message: String::new(),
    }
}

fn crashed() -> EndReason {
    EndReason::Crashed {
        message: "exit 139".to_owned(),
    }
}

/// The crew of [`Fixture::with_models`], every fallback kept on the CLI that stopped.
fn staying_on_its_cli() -> Fixture {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().fallback.same_cli = Some(true);
    fixture
}

fn out(fixture: &Fixture, model: &str, trouble: Trouble) {
    let model = worker(model);
    fixture
        .health
        .set_model(&model, ModelState::Out, Some(trouble));
}

/// An overload or a server error is about the model it came from, so another model of the same
/// CLI may take the task, and one that is out itself is passed over.
#[tokio::test]
async fn an_overload_or_server_error_leaves_the_clis_other_models_to_take_the_task() {
    let fixture = Fixture::with_models();
    let limits = fixture.limits();
    for category in [ErrorCategory::Overloaded, ErrorCategory::Server] {
        let action = decide(&limits, "claude-code/opus", cli_error(category), &[]).await;
        assert_eq!(
            fell_back_to(&action).0,
            "claude-code/sonnet",
            "{category:?}"
        );
    }

    out(&fixture, "claude-code/sonnet", Trouble::ServerErrors);
    let overload = cli_error(ErrorCategory::Overloaded);
    let action = decide(&limits, "claude-code/opus", overload, &[]).await;
    assert_eq!(fell_back_to(&action).0, "codex/gpt-5");
}

/// The network and a timeout are no model's doing, so the CLI's other models would meet them
/// too: the task moves to another CLI.
#[tokio::test]
async fn a_network_error_or_timeout_moves_the_task_to_another_cli() {
    let limits = Fixture::with_models().limits();
    for category in [ErrorCategory::Network, ErrorCategory::Timeout] {
        let action = decide(&limits, "claude-code/opus", cli_error(category), &[]).await;
        assert_eq!(fell_back_to(&action).0, "codex/gpt-5", "{category:?}");
    }
}

/// Two models out took the CLI down, so its provider is what fails: a model another CLI
/// serves from that provider is passed over, where a network outage says nothing of it.
#[tokio::test]
async fn a_model_of_the_failing_provider_on_another_cli_is_passed_over() {
    let mut fixture = Fixture::with_models();
    fixture
        .registry
        .add(model("opencode", "claude-x", "anthropic"));
    fixture.settings_mut().fallback.order = vec![entry("opencode/claude-x"), entry("codex/gpt-5")];
    let overload = || cli_error(ErrorCategory::Overloaded);

    // While only Opus is out, Anthropic's other models are fine wherever they run.
    let action = decide(&fixture.limits(), "claude-code/opus", overload(), &[]).await;
    assert_eq!(fell_back_to(&action).0, "opencode/claude-x");

    out(&fixture, "claude-code/opus", Trouble::Overload);
    out(&fixture, "claude-code/sonnet", Trouble::ServerErrors);
    let down = agent_id("claude-code");
    fixture
        .health
        .set(&down, HealthState::Down, Some("two of its models are out"));
    let action = decide(&fixture.limits(), "claude-code/opus", overload(), &[]).await;
    assert_eq!(fell_back_to(&action).0, "codex/gpt-5");

    // A CLI the network took down says nothing of its provider.
    let network = cli_error(ErrorCategory::Network);
    let action = decide(&fixture.limits(), "claude-code/opus", network, &[]).await;
    assert_eq!(fell_back_to(&action).0, "opencode/claude-x");
}

/// `same_cli` keeps an overloaded model's task on its CLI, whatever else is listed first, and
/// once none of the CLI's models is left the task waits for it and says until when.
#[tokio::test]
async fn same_cli_keeps_an_overloaded_models_task_on_its_cli_or_waits() {
    let mut fixture = staying_on_its_cli();
    fixture.settings_mut().fallback.order = vec![entry("codex/gpt-5"), entry("claude-code/sonnet")];
    let overload = || cli_error(ErrorCategory::Overloaded);
    let action = decide(&fixture.limits(), "claude-code/opus", overload(), &[]).await;
    assert_eq!(fell_back_to(&action).0, "claude-code/sonnet");

    // A list of other CLIs alone still leaves the CLI's own models to go to.
    fixture.settings_mut().fallback.order = vec![entry("codex/gpt-5")];
    let action = decide(&fixture.limits(), "claude-code/opus", overload(), &[]).await;
    assert_eq!(fell_back_to(&action).0, "claude-code/sonnet");

    out(&fixture, "claude-code/sonnet", Trouble::ServerErrors);
    let before = OffsetDateTime::now_utc();
    let action = decide(&fixture.limits(), "claude-code/opus", overload(), &[]).await;
    let (waits_for, until) = waited(&action);
    assert_eq!(waits_for, agent_id("claude-code"));
    assert!(until > before, "{until}");
}

/// The role's own table may keep its fallbacks on one CLI while the others move freely.
#[tokio::test]
async fn same_cli_can_be_set_for_one_role() {
    let mut fixture = Fixture::with_models();
    workers_table(&mut fixture).same_cli = Some(true);
    let network = cli_error(ErrorCategory::Network);
    let action = decide(&fixture.limits(), "claude-code/opus", network.clone(), &[]).await;
    assert_eq!(waited(&action).0, agent_id("claude-code"));

    // A reviewer follows the top-level table, which lets it move.
    let journal = journal(&[]);
    let mut stop = context("claude-code/opus", network, &journal.path);
    stop.role = RoleName::Reviewer;
    stop.task.worker = Some(worker("antigravity-cli/gemini-3"));
    let action = fixture.limits().after_stop(&stop).await;
    assert_eq!(fell_back_to(&action).0, "codex/gpt-5");
}

/// With `same_cli`, a failure of the whole CLI leaves nobody to take the task: it waits for the
/// network, for the rate limit, and for the usage limit's reset.
#[tokio::test]
async fn same_cli_waits_for_a_cli_that_stopped_as_a_whole() {
    let limits = staying_on_its_cli().limits();
    for reason in [
        cli_error(ErrorCategory::Network),
        cli_error(ErrorCategory::Timeout),
        cli_error(ErrorCategory::RateLimit),
        EndReason::RateLimited,
    ] {
        let action = decide(&limits, "claude-code/opus", reason.clone(), &[]).await;
        assert_eq!(waited(&action).0, agent_id("claude-code"), "{reason:?}");
    }

    let resets = OffsetDateTime::now_utc() + time::Duration::hours(2);
    let limit = EndReason::UsageLimit {
        resets_at: Some(resets),
    };
    assert_eq!(
        decide(&limits, "claude-code/opus", limit, &[]).await,
        RecoveryAction::Wait {
            agent_id: agent_id("claude-code"),
            until: Some(resets),
            kept_off: Some(agent_id("codex")),
        }
    );
}

/// Signed out and out of credit still ask you. They are the whole CLI's trouble, so with
/// `same_cli` the question offers no other CLI, and none of its own models either.
#[tokio::test]
async fn same_cli_offers_no_other_cli_when_you_are_signed_out_or_out_of_credit() {
    let limits = staying_on_its_cli().limits();
    let signed_out = EndReason::Auth {
        message: "401".to_owned(),
    };
    let action = decide_logged(&limits, "claude-code/opus", signed_out, Some(false)).await;
    let plan = asked(&action);
    assert!(
        plan.reason.contains("log in to claude-code"),
        "{}",
        plan.reason
    );
    assert!(plan.candidates.is_empty(), "{:?}", names(&plan.candidates));

    let billing = EndReason::Billing {
        message: "credit balance too low".to_owned(),
    };
    let action = decide(&limits, "claude-code/opus", billing, &[]).await;
    assert!(asked(&action).candidates.is_empty());

    // Free to move, the same questions offer the other CLIs.
    let free = Fixture::with_models().limits();
    let signed_out = EndReason::Auth {
        message: "401".to_owned(),
    };
    let action = decide_logged(&free, "claude-code/opus", signed_out, Some(false)).await;
    assert_eq!(
        names(&asked(&action).candidates),
        ["codex/gpt-5", "antigravity-cli/gemini-3"]
    );
}

/// A crash or a stuck session gets one fresh session: on another provider's model when the task
/// may move, on another model of the same CLI with `same_cli`. After that you are asked.
#[tokio::test]
async fn a_crash_gets_one_fresh_session_where_the_task_may_go() {
    let free = Fixture::with_models().limits();
    let action = decide(&free, "claude-code/opus", crashed(), &[]).await;
    assert_eq!(fell_back_to(&action).0, "codex/gpt-5");

    let fixture = staying_on_its_cli();
    let staying = fixture.limits();
    let stuck = EndReason::Stuck {
        watchdog: Watchdog::Inactivity,
        calls: None,
    };
    for reason in [crashed(), stuck] {
        let action = decide(&staying, "claude-code/opus", reason.clone(), &[]).await;
        assert_eq!(fell_back_to(&action).0, "claude-code/sonnet", "{reason:?}");
    }

    // Already retried once, it comes to you.
    let journal = journal(&[]);
    let mut stop = context("claude-code/sonnet", crashed(), &journal.path);
    stop.reassigned_once = true;
    let action = staying.after_stop(&stop).await;
    assert!(
        matches!(action, RecoveryAction::AskYou { .. }),
        "{action:?}"
    );

    // With no other model on the CLI, it comes to you at once, and says why.
    out(&fixture, "claude-code/sonnet", Trouble::Overload);
    match decide(&staying, "claude-code/opus", crashed(), &[]).await {
        RecoveryAction::AskYou { reason } => {
            assert!(reason.contains("no other model on claude-code"), "{reason}");
        }
        other => panic!("expected a question for you, got {other:?}"),
    }
}

/// A full context is no trouble of the agent's at all, and a model that's gone already stays
/// on its CLI: neither changes with `same_cli`.
#[tokio::test]
async fn a_full_context_and_a_retired_model_go_on_as_before() {
    for limits in [
        Fixture::with_models().limits(),
        staying_on_its_cli().limits(),
    ] {
        let action = decide(&limits, "claude-code/opus", EndReason::ContextFull, &[]).await;
        assert!(
            matches!(action, RecoveryAction::Rotate { .. }),
            "{action:?}"
        );

        let gone = EndReason::ModelUnavailable {
            model: "opus".to_owned(),
        };
        let action = decide(&limits, "claude-code/opus", gone, &[]).await;
        let plan = asked(&action);
        assert!(plan.reason.contains("no successor"), "{}", plan.reason);
    }
}
