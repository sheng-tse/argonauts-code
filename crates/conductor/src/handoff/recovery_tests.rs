//! Tests for the limits service in `recovery.rs`: the recovery table, the ladder, handoff
//! modes, checkpoints, and parking.

use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::events::card::{AttemptCause, CardEvent, CardState};
use argonauts_core::events::run::{ParkReason, Party};
use argonauts_core::events::session::{
    EndReason, SessionEnd, SessionEvent, SessionStarted, Watchdog,
};
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::handoff::CheckpointTag;
use argonauts_core::limits::{
    Candidate, HealthState, LimitsService, QuotaBoard, QuotaStatus, RecoveryAction, StopContext,
    VendorHealth,
};
use argonauts_core::names::{RoleName, SessionKey, VendorId, WorkerRef};
use argonauts_core::settings::Span;
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit, OnHit};
use argonauts_core::settings::handoff::{
    FallbackEntry, FallbackRules, HandoffMode, HandoffSettings, HandoffStrategy, OnReturn,
    VendorReplacement,
};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::worker::{Access, WorkerKind};
use argonauts_core::{AgentEvent, BudgetDimension, CardId, ErrorCategory, Spend};
use tempfile::TempDir;
use time::OffsetDateTime;

use super::{Limits, OriginalBack};
use crate::handoff::testing::{
    CARD, Fixture, api_model, card, entry, model, quota, run, vendor, worker,
};
use crate::vendors_file::VendorsFile;

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
        card: card(CARD),
        state: CardState::Running,
        role: RoleName::Owner,
        worker: worker(stopped),
        end: SessionEnd {
            vendor_session: Some("v-1".to_owned()),
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
        attempts: vec!["attempt 1: claude-code/opus".to_owned()],
        reassigned_once: false,
        events: events.to_path_buf(),
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

/// An event of the card in the test.
fn event(seq: u64, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts: OffsetDateTime::UNIX_EPOCH,
        run_id: run(),
        card_id: Some(CardId::new(CARD).expect("a valid card id")),
        session: None,
        source: "conductor".to_owned(),
        kind,
    }
}

fn card_event(seq: u64, card: CardEvent) -> RunEvent {
    event(seq, RunEventKind::Card(card))
}

fn tagged(n: u32) -> RunEvent {
    card_event(
        u64::from(n),
        CardEvent::CheckpointTagged {
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

fn handed_to(action: &RecoveryAction) -> (String, Option<u32>) {
    match action {
        RecoveryAction::HandOff { to, checkpoint, .. } => {
            (to.to_string(), checkpoint.as_ref().map(|tag| tag.n))
        }
        other => panic!("expected a handoff, got {other:?}"),
    }
}

fn asked(action: &RecoveryAction) -> &HandoffPlan {
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
async fn handoff_modes_ask_auto_wait() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().handoff.mode = HandoffMode::Ask;
    let resets = OffsetDateTime::now_utc() + time::Duration::hours(2);
    let limit = EndReason::UsageLimit {
        resets_at: Some(resets),
    };

    let action = decide(&fixture.limits(), "claude-code/opus", limit.clone(), &[]).await;
    let plan = asked(&action);
    assert_eq!(plan.mode, HandoffMode::Ask);
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

    fixture.settings_mut().handoff.mode = HandoffMode::Auto;
    let limits = fixture.limits();
    assert_eq!(
        handed_to(&decide(&limits, "claude-code/opus", limit.clone(), &[]).await),
        ("codex/gpt-5".to_owned(), Some(1))
    );
    // A rate limit past the card's time budget counts as a usage limit.
    assert_eq!(
        handed_to(&decide(&limits, "claude-code/opus", EndReason::RateLimited, &[]).await).0,
        "codex/gpt-5"
    );

    fixture.settings_mut().handoff.mode = HandoffMode::Wait;
    assert_eq!(
        decide(&fixture.limits(), "claude-code/opus", limit, &[]).await,
        RecoveryAction::Wait {
            vendor: vendor("claude-code"),
            until: Some(resets),
        }
    );
}

#[tokio::test]
async fn ask_timeout_falls_back_to_auto() {
    let mut fixture = Fixture::with_models();
    fixture
        .settings_mut()
        .roles
        .entry(RoleName::Owner)
        .or_default()
        .handoff = HandoffSettings {
        mode: HandoffMode::Ask,
        ask_timeout: Span::new("10m"),
        ..HandoffSettings::default()
    };
    let limits = fixture.limits();
    let claude = vendor("claude-code");
    assert_eq!(
        limits.ask_timeout(RoleName::Owner, &claude),
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
        handed_to(&limits.after_ask_timeout(&stop).await),
        ("codex/gpt-5".to_owned(), Some(1))
    );

    // With nothing valid left, the checkpoint opens again.
    fixture
        .health
        .set(&vendor("codex"), HealthState::Down, None);
    fixture
        .quota
        .observe(quota("antigravity-cli", QuotaStatus::Rejected, Some(1.0)));
    let plan = limits.after_ask_timeout(&stop).await;
    assert!(asked(&plan).candidates.is_empty());
    assert!(asked(&plan).reason.ends_with("no valid replacement"));
}

#[tokio::test]
async fn retry_ladder() {
    let fixture = Fixture::with_models();
    let limits = fixture.limits();
    // The ladder acts in any handoff mode, and skips the same family.
    for reason in [
        EndReason::Stuck {
            watchdog: Watchdog::Inactivity,
        },
        EndReason::Crashed {
            message: "exit 139".to_owned(),
        },
        EndReason::StartFailed {
            message: "no such binary".to_owned(),
        },
        EndReason::VendorError {
            category: ErrorCategory::Cli,
            message: "unexpected".to_owned(),
        },
    ] {
        assert_eq!(
            handed_to(&decide(&limits, "claude-code/opus", reason, &[]).await),
            ("codex/gpt-5".to_owned(), Some(1))
        );
    }

    let journal = journal(&[]);
    let mut stop = context(
        "claude-code/opus",
        EndReason::Stuck {
            watchdog: Watchdog::Repeat,
        },
        &journal.path,
    );
    stop.reassigned_once = true;
    match limits.after_stop(&stop).await {
        RecoveryAction::Escalate { reason } => {
            assert!(reason.contains("already retried"), "{reason}");
        }
        other => panic!("expected an escalation, got {other:?}"),
    }

    stop.reassigned_once = false;
    fixture
        .health
        .set(&vendor("codex"), HealthState::Down, None);
    fixture
        .health
        .set(&vendor("antigravity-cli"), HealthState::Down, None);
    match limits.after_stop(&stop).await {
        RecoveryAction::Escalate { reason } => {
            assert!(reason.contains("no agent of another family"), "{reason}");
        }
        other => panic!("expected an escalation, got {other:?}"),
    }
}

#[tokio::test]
async fn logged_out_and_billing_ask_with_reason() {
    let mut fixture = Fixture::with_models();
    // Even in auto mode, a login or a top-up is yours to do.
    fixture.settings_mut().handoff.mode = HandoffMode::Auto;
    let limits = fixture.limits();
    let auth = EndReason::Auth {
        message: "token expired".to_owned(),
    };
    let action = decide(&limits, "claude-code/opus", auth, &[]).await;
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
    let action = decide(
        &limits,
        "claude-code/opus",
        EndReason::VendorError {
            category: ErrorCategory::Auth,
            message: String::new(),
        },
        &[],
    )
    .await;
    assert!(asked(&action).reason.contains("log in to claude-code"));

    // After a billing stop, never a silent switch to another paid API, even an allowed one.
    fixture
        .registry
        .add(api_model("openrouter", "kimi", "kimi", 1.0));
    let settings = fixture.settings_mut();
    settings.handoff.allow_api_billing = true;
    settings.handoff.fallback = vec![entry("openrouter/kimi"), entry("codex/gpt-5")];
    let limits = fixture.limits();
    let journal = journal(&[]);
    let stop = context("claude-code/opus", billing, &journal.path);
    assert!(matches!(
        limits.after_stop(&stop).await,
        RecoveryAction::Ask(_)
    ));
    assert_eq!(
        handed_to(&limits.after_ask_timeout(&stop).await).0,
        "codex/gpt-5"
    );
}

#[tokio::test]
async fn outage_follows_handoff_mode() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().handoff.mode = HandoffMode::Ask;
    fixture.health.set(
        &vendor("claude-code"),
        HealthState::Down,
        Some("overloaded"),
    );
    let outage = || EndReason::VendorError {
        category: ErrorCategory::Overloaded,
        message: "529 overloaded".to_owned(),
    };

    let action = decide(&fixture.limits(), "claude-code/opus", outage(), &[]).await;
    assert!(asked(&action).reason.starts_with("overloaded"));

    // The role's own mode wins over the top-level one.
    let role = fixture
        .settings_mut()
        .roles
        .entry(RoleName::Owner)
        .or_default();
    role.handoff.mode = HandoffMode::Auto;
    assert_eq!(
        handed_to(&decide(&fixture.limits(), "claude-code/opus", outage(), &[]).await).0,
        "codex/gpt-5"
    );

    let role = fixture
        .settings_mut()
        .roles
        .entry(RoleName::Owner)
        .or_default();
    role.handoff.mode = HandoffMode::Wait;
    let (waits_for, _) =
        waited(&decide(&fixture.limits(), "claude-code/opus", outage(), &[]).await);
    assert_eq!(waits_for, vendor("claude-code"));
}

fn waited(action: &RecoveryAction) -> (VendorId, OffsetDateTime) {
    match action {
        RecoveryAction::Wait {
            vendor,
            until: Some(until),
        } => (vendor.clone(), *until),
        other => panic!("expected a wait with a wake time, got {other:?}"),
    }
}

/// A park the board can't end, since the vendor never went Down or reported a reset, still
/// wakes once the breaker's cool-down has passed.
#[tokio::test]
async fn a_wait_with_no_known_return_wakes_after_the_cooldown() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().handoff.mode = HandoffMode::Wait;
    let limits = fixture.limits();
    let cooldown = time::Duration::minutes(5);
    for reason in [
        EndReason::VendorError {
            category: ErrorCategory::Network,
            message: "stream disconnected".to_owned(),
        },
        EndReason::VendorError {
            category: ErrorCategory::Timeout,
            message: "504 gateway timeout".to_owned(),
        },
        EndReason::RateLimited,
        usage(),
    ] {
        let parked_at = OffsetDateTime::now_utc();
        let action = decide(&limits, "claude-code/opus", reason.clone(), &[]).await;
        let (claude, until) = waited(&action);
        assert!(until >= parked_at + cooldown, "{reason:?}");
        assert!(until <= OffsetDateTime::now_utc() + cooldown, "{reason:?}");

        let mut file = VendorsFile::default();
        file.vendors.entry(claude.0.clone()).or_default().health = Some(VendorHealth {
            vendor: claude.clone(),
            state: HealthState::Degraded,
            since: parked_at,
            reason: None,
            next_test_at: None,
            disabled: None,
        });
        assert!(!file.vendor_back(&claude, parked_at, until));
        assert!(!file.wait_over(&claude, Some(until), parked_at, parked_at));
        assert!(file.wait_over(&claude, Some(until), parked_at, until));
    }
}

#[tokio::test]
async fn all_out_parks_until_earliest_reset() {
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
        let mut reading = quota(name, QuotaStatus::Rejected, Some(1.0));
        reading.resets_at = resets_at;
        reading
    };
    fixture.quota.observe(rejected(
        "claude-code",
        Some(now + time::Duration::hours(2)),
    ));
    fixture
        .quota
        .observe(rejected("codex", Some(now + time::Duration::hours(1))));
    assert_eq!(limits.all_out(&workers), None, "agy still works");

    // Down with no known test time: it counts as out, but not for the earliest reset.
    fixture
        .health
        .set(&vendor("antigravity-cli"), HealthState::Down, None);
    assert_eq!(
        limits.all_out(&workers),
        Some(ParkReason::AllVendorsOut {
            until: Some(now + time::Duration::hours(1)),
        })
    );

    // A reading whose reset has passed no longer counts.
    fixture
        .quota
        .observe(rejected("codex", Some(now - time::Duration::minutes(1))));
    assert_eq!(limits.all_out(&workers), None);
}

#[tokio::test]
async fn auto_parks_when_the_whole_crew_is_out() {
    let mut fixture = Fixture::with_models();
    let settings = fixture.settings_mut();
    settings.handoff.mode = HandoffMode::Auto;
    settings.crew = vec![worker("claude-code/opus")];
    settings.default_agent = worker("claude-code/opus");
    settings.handoff.fallback.clear();
    let resets = OffsetDateTime::now_utc() + time::Duration::minutes(5);
    let mut reading = quota("claude-code", QuotaStatus::Rejected, Some(1.0));
    reading.resets_at = Some(resets);
    fixture.quota.observe(reading);
    let limit = EndReason::UsageLimit {
        resets_at: Some(resets),
    };

    let action = decide(&fixture.limits(), "claude-code/opus", limit.clone(), &[]).await;
    assert_eq!(
        action,
        RecoveryAction::Park(ParkReason::AllVendorsOut {
            until: Some(resets)
        })
    );

    // A crew member that still works takes the card instead.
    fixture.settings_mut().crew.push(worker("codex/gpt-5"));
    let action = decide(&fixture.limits(), "claude-code/opus", limit, &[]).await;
    assert_eq!(handed_to(&action).0, "codex/gpt-5");
}

#[tokio::test]
async fn model_retired_hands_to_successor() {
    let fixture = Fixture::with_models();
    let retired = |model_id: &str, family: &str, successor: Option<&str>| {
        let mut entry = model("claude-code", model_id, family);
        entry.retired = true;
        entry.successor = successor.map(str::to_owned);
        entry
    };
    fixture
        .registry
        .add(retired("opus-4", "claude", Some("opus-4.5")));
    fixture
        .registry
        .add(retired("opus-4.5", "claude", Some("opus-5")));
    fixture
        .registry
        .add(model("claude-code", "opus-5", "claude"));
    fixture.registry.add(retired("haiku-3", "claude", None));
    fixture
        .registry
        .add(retired("odd-1", "claude", Some("other-2")));
    fixture
        .registry
        .add(model("claude-code", "other-2", "other"));
    let limits = fixture.limits();
    let gone = |model_id: &str| EndReason::ModelUnavailable {
        model: model_id.to_owned(),
    };

    // Retired successors are skipped, to the first one that works, on the same worker.
    assert_eq!(
        handed_to(&decide(&limits, "claude-code/opus-4", gone("opus-4"), &[]).await),
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
        handed_to(&decide(&limits, "claude-code", said.clone(), &reported).await).0,
        "claude-code/opus-5"
    );

    for (stopped, model_id) in [
        ("claude-code/haiku-3", "haiku-3"),
        ("claude-code/odd-1", "odd-1"),
    ] {
        let action = decide(&limits, stopped, gone(model_id), &[]).await;
        assert!(asked(&action).reason.contains("no successor"), "{stopped}");
    }

    // Retired successors aren't followed onto another worker, which could bill differently.
    fixture
        .registry
        .add(retired("far-1", "claude", Some("antigravity-cli/claude-5")));
    fixture
        .registry
        .add(model("antigravity-cli", "claude-5", "claude"));
    let action = decide(&limits, "claude-code/far-1", gone(""), &[]).await;
    assert!(asked(&action).reason.contains("no successor"));

    // Without the registry, you decide.
    let bare = Limits::new(
        std::sync::Arc::new(fixture.settings.clone()),
        fixture.quota.clone(),
        fixture.health.clone(),
        std::sync::Arc::new(fixture.planner()),
    );
    assert!(matches!(
        decide(&bare, "claude-code/opus-4", gone("opus-4"), &[]).await,
        RecoveryAction::Ask(_)
    ));
}

/// A worker on its default model, set by `agents.<id>.model`, whose successor the registry names
/// as a full `worker/model` reference.
#[tokio::test]
async fn a_default_model_hands_to_a_full_reference_successor() {
    let mut fixture = Fixture::with_models();
    fixture
        .settings_mut()
        .agents
        .entry("claude-code".to_owned())
        .or_default()
        .model = Some("claude-old-1".to_owned());
    let mut old = model("claude-code", "claude-old-1", "claude");
    old.successor = Some("claude-code/haiku".to_owned());
    fixture.registry.add(old);
    fixture
        .registry
        .add(model("claude-code", "haiku", "claude"));
    let limits = fixture.limits();

    // The harness worker names the model it asked for, and a driven session leaves it empty.
    for named in ["claude-old-1", ""] {
        let reason = EndReason::ModelUnavailable {
            model: named.to_owned(),
        };
        let action = decide(&limits, "claude-code", reason, &[]).await;
        match &action {
            RecoveryAction::HandOff { to, packet, .. } => {
                assert_eq!(*to, WorkerRef::new("claude-code", Some("haiku".to_owned())));
                assert_eq!(
                    packet.why_stopped,
                    "model claude-old-1 is no longer available"
                );
            }
            other => panic!("expected a handoff, got {other:?}"),
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
                vendor: worker(stopped).vendor(),
                family: "claude".to_owned(),
                kind: WorkerKind::Harness,
                role: RoleName::Owner,
                step: None,
                access: Access::Build,
                lifetime: SessionLifetime::Card,
                resumed_from: None,
                follow_key: None,
                workflow_step: None,
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
        EndReason::VendorError {
            category: ErrorCategory::ContextFull,
            message: "prompt is too long".to_owned(),
        },
    ] {
        match decide(&limits, "claude-code/opus", reason, &[]).await {
            RecoveryAction::Rotate { packet } => {
                assert_eq!(packet.card, card(CARD));
                assert_eq!(packet.work.branch, "argonauts/run-01/card-003");
            }
            other => panic!("expected a rotation, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn budget_hit_applies_on_hit() {
    let mut fixture = Fixture::with_models();
    let hit = || EndReason::BudgetHit {
        dimension: BudgetDimension::Spend,
    };
    assert_eq!(
        decide(&fixture.limits(), "claude-code/opus", hit(), &[]).await,
        RecoveryAction::BudgetHit(OnHit::Ask)
    );
    for on_hit in [OnHit::Extend, OnHit::Handoff, OnHit::Escalate] {
        fixture.settings_mut().budget.on_hit = on_hit;
        assert_eq!(
            decide(&fixture.limits(), "claude-code/opus", hit(), &[]).await,
            RecoveryAction::BudgetHit(on_hit)
        );
    }
}

#[tokio::test]
async fn max_handoffs_escalates() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().handoff.mode = HandoffMode::Auto;
    let three = [tagged(1), tagged(2), tagged(3)];
    match decide(&fixture.limits(), "claude-code/opus", usage(), &three).await {
        RecoveryAction::Escalate { reason } => assert!(reason.contains("max_handoffs"), "{reason}"),
        other => panic!("expected an escalation, got {other:?}"),
    }

    fixture.settings_mut().handoff.max_handoffs = Some(4);
    assert_eq!(
        handed_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &three).await),
        ("codex/gpt-5".to_owned(), Some(4))
    );

    // With checkpoints off, handoff attempts count instead.
    let settings = fixture.settings_mut();
    settings.handoff.max_handoffs = Some(2);
    settings.handoff.checkpoint = Some(false);
    let attempts: Vec<RunEvent> = (1..=2)
        .map(|attempt| {
            card_event(
                u64::from(attempt),
                CardEvent::Attempt {
                    attempt,
                    worker: worker("codex/gpt-5"),
                    cause: AttemptCause::Handoff,
                },
            )
        })
        .collect();
    assert!(matches!(
        decide(&fixture.limits(), "claude-code/opus", usage(), &attempts).await,
        RecoveryAction::Escalate { .. }
    ));

    // Waiting isn't another handoff.
    fixture.settings_mut().handoff.mode = HandoffMode::Wait;
    assert!(matches!(
        decide(&fixture.limits(), "claude-code/opus", usage(), &attempts).await,
        RecoveryAction::Wait { .. }
    ));
}

#[tokio::test]
async fn a_checkpointed_race_is_one_handoff() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().handoff.mode = HandoffMode::Auto;
    let racers = [
        "codex/gpt-5",
        "antigravity-cli/gemini-3",
        "claude-code/sonnet",
    ];
    let mut race = vec![tagged(1)];
    race.extend((2u32..).zip(racers).map(|(attempt, racer)| {
        card_event(
            u64::from(attempt),
            CardEvent::Attempt {
                attempt,
                worker: worker(racer),
                cause: AttemptCause::Handoff,
            },
        )
    }));
    // Three racers are still one round, so the default of three handoffs is far off.
    assert_eq!(
        handed_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &race).await).1,
        Some(2)
    );
}

/// An event of `card`.
fn on_card(card: &str, seq: u64, kind: CardEvent) -> RunEvent {
    RunEvent {
        card_id: Some(CardId::new(card).expect("a valid card id")),
        ..card_event(seq, kind)
    }
}

/// `card` handed from Claude Code to `to`, as the engine logs it.
fn logged_handoff(card: &str, seq: u64, to: &str) -> RunEvent {
    on_card(
        card,
        seq,
        CardEvent::Attempt {
            attempt: 2,
            worker: worker(to),
            cause: AttemptCause::Handoff,
        },
    )
}

#[tokio::test]
async fn spread_rotates_a_vendors_cards_across_candidates() {
    let mut fixture = Fixture::with_models();
    let settings = fixture.settings_mut();
    settings.handoff.mode = HandoffMode::Auto;
    settings.handoff.strategy = Some(HandoffStrategy::Spread);
    let cards = ["card-003", "card-004", "card-005"];
    // Each card was running on Claude Code when it went out.
    let running: Vec<RunEvent> = (0u64..)
        .zip(cards)
        .flat_map(|(n, id)| {
            [
                on_card(
                    id,
                    2 * n + 1,
                    CardEvent::Attempt {
                        attempt: 1,
                        worker: worker("claude-code/opus"),
                        cause: AttemptCause::First,
                    },
                ),
                on_card(
                    id,
                    2 * n + 2,
                    CardEvent::State {
                        from: None,
                        to: CardState::Running,
                        reason: String::new(),
                    },
                ),
            ]
        })
        .collect();
    let stop = |id: &str, events: &Path| StopContext {
        card: card(id),
        ..context("claude-code/opus", usage(), events)
    };

    // Every stop reads the same log and boards, before the engine logs any handoff.
    let log = journal(&running);
    let limits = fixture.limits();
    let mut to = Vec::new();
    for id in cards {
        to.push(handed_to(&limits.after_stop(&stop(id, &log.path)).await).0);
    }
    assert_eq!(
        to,
        ["codex/gpt-5", "antigravity-cli/gemini-3", "codex/gpt-5"]
    );

    // A fresh service reads the same from the log once the handoffs are logged.
    let mut logged = running.clone();
    logged.push(logged_handoff("card-003", 7, "codex/gpt-5"));
    logged.push(logged_handoff("card-004", 8, "antigravity-cli/gemini-3"));
    let log = journal(&logged);
    assert_eq!(
        handed_to(
            &fixture
                .limits()
                .after_stop(&stop("card-005", &log.path))
                .await
        )
        .0,
        "codex/gpt-5"
    );

    // Asked instead, the plan puts the least loaded first and names the cards still to move.
    fixture.settings_mut().handoff.mode = HandoffMode::Ask;
    let mut one_moved = running;
    one_moved.push(logged_handoff("card-003", 7, "codex/gpt-5"));
    let log = journal(&one_moved);
    let action = fixture
        .limits()
        .after_stop(&stop("card-004", &log.path))
        .await;
    let plan = asked(&action);
    assert_eq!(plan.strategy, HandoffStrategy::Spread);
    assert_eq!(
        names(&plan.candidates),
        ["antigravity-cli/gemini-3", "codex/gpt-5"]
    );
    assert_eq!(
        plan.also_moving,
        [CardId::new("card-005").expect("a valid card id")]
    );

    // The strategy `first` sends every card to the same candidate and lists no others.
    fixture.settings_mut().handoff.strategy = None;
    let plan = fixture
        .limits()
        .after_stop(&stop("card-004", &log.path))
        .await;
    assert!(asked(&plan).also_moving.is_empty());
    assert_eq!(
        names(&asked(&plan).candidates),
        ["codex/gpt-5", "antigravity-cli/gemini-3"]
    );
}

#[tokio::test]
async fn race_only_when_a_list_sets_it() {
    let mut fixture = Fixture::with_models();
    let role = fixture
        .settings_mut()
        .roles
        .entry(RoleName::Owner)
        .or_default();
    role.handoff.mode = HandoffMode::Auto;
    role.handoff.replace.insert(
        vendor("claude-code"),
        VendorReplacement {
            with: vec![entry("codex/gpt-5"), entry("antigravity-cli/gemini-3")],
            strategy: Some(HandoffStrategy::Race),
            ..VendorReplacement::default()
        },
    );
    let limits = fixture.limits();
    match decide(&limits, "claude-code/opus", usage(), &[]).await {
        RecoveryAction::Race { to, checkpoint, .. } => {
            assert_eq!(
                to,
                [worker("codex/gpt-5"), worker("antigravity-cli/gemini-3")]
            );
            assert_eq!(checkpoint.map(|tag| tag.n), Some(1));
        }
        other => panic!("expected a race, got {other:?}"),
    }

    // Codex's stop has no list that sets a race, so one candidate takes it.
    assert_eq!(
        handed_to(&decide(&limits, "codex/gpt-5", usage(), &[]).await).0,
        "claude-code/opus"
    );

    // A race needs two valid racers; with one left it's a plain handoff.
    fixture
        .health
        .set(&vendor("antigravity-cli"), HealthState::Down, None);
    assert_eq!(
        handed_to(&decide(&limits, "claude-code/opus", usage(), &[]).await).0,
        "codex/gpt-5"
    );
}

#[tokio::test]
async fn checkpoint_tag_on_handoff_and_race() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().handoff.mode = HandoffMode::Auto;
    let expected = |n| CheckpointTag {
        run: run(),
        card: CardId::new(CARD).expect("a valid card id"),
        n,
    };
    match decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await {
        RecoveryAction::HandOff { checkpoint, .. } => {
            let tag = checkpoint.expect("a checkpoint");
            assert_eq!(tag, expected(1));
            assert_eq!(tag.tag_name(), "argonauts/run-01/card-003/handoff-1");
        }
        other => panic!("expected a handoff, got {other:?}"),
    }
    // The next handoff of the same card is handoff-2.
    assert_eq!(
        handed_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &[tagged(1)]).await).1,
        Some(2)
    );

    fixture.settings_mut().handoff.strategy = Some(HandoffStrategy::Race);
    match decide(&fixture.limits(), "claude-code/opus", usage(), &[tagged(1)]).await {
        RecoveryAction::Race { checkpoint, .. } => assert_eq!(checkpoint, Some(expected(2))),
        other => panic!("expected a race, got {other:?}"),
    }

    fixture.settings_mut().handoff.checkpoint = Some(false);
    match decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await {
        RecoveryAction::Race { checkpoint, .. } => assert_eq!(checkpoint, None),
        other => panic!("expected a race, got {other:?}"),
    }
    fixture.settings_mut().handoff.strategy = None;
    assert_eq!(
        handed_to(&decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await).1,
        None
    );
}

#[tokio::test]
async fn auto_handoff_carries_the_entry_rules() {
    let mut fixture = Fixture::with_models();
    let settings = fixture.settings_mut();
    settings.handoff.mode = HandoffMode::Auto;
    settings.handoff.fallback = vec![FallbackEntry::WithRules(FallbackRules {
        worker: worker("codex/gpt-5"),
        max_budget_usd: None,
        instructions: Some("Only finish the failing test.".to_owned()),
        read_only: true,
        allow_api_billing: false,
    })];
    match decide(&fixture.limits(), "claude-code/opus", usage(), &[]).await {
        RecoveryAction::HandOff { to, packet, .. } => {
            assert_eq!(to, worker("codex/gpt-5"));
            assert!(
                packet
                    .notes
                    .contains(&"Instructions: Only finish the failing test.".to_owned())
            );
            assert!(
                packet
                    .notes
                    .iter()
                    .any(|note| note.starts_with("Read-only"))
            );
        }
        other => panic!("expected a handoff, got {other:?}"),
    }
}

#[tokio::test]
async fn journal_feeds_the_plan_and_a_missing_one_is_noted() {
    let mut fixture = Fixture::with_models();
    fixture.settings_mut().handoff.mode = HandoffMode::Ask;
    let limits = fixture.limits();
    let gate = argonauts_core::events::card::GateOutcome {
        name: "tests".to_owned(),
        kind: argonauts_core::events::card::GateKind::Command,
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
    let gated = event(9, RunEventKind::Gate(gate));
    let action = decide(&limits, "claude-code/opus", usage(), &[tagged(1), gated]).await;
    assert!(asked(&action).gates_ran);

    let missing = context(
        "claude-code/opus",
        usage(),
        Path::new("/nonexistent/events.jsonl"),
    );
    match fixture.limits().after_stop(&missing).await {
        RecoveryAction::Ask(plan) => assert!(!plan.gates_ran),
        other => panic!("expected a question, got {other:?}"),
    }
    let rotate = context(
        "claude-code/opus",
        EndReason::ContextFull,
        Path::new("/nonexistent/events.jsonl"),
    );
    match limits.after_stop(&rotate).await {
        RecoveryAction::Rotate { packet } => {
            assert!(
                packet
                    .notes
                    .iter()
                    .any(|note| note.contains("couldn't be read"))
            );
        }
        other => panic!("expected a rotation, got {other:?}"),
    }
}

#[tokio::test]
async fn paused_resumes_and_stopped_escalates() {
    let limits = Fixture::with_models().limits();
    assert!(matches!(
        decide(&limits, "claude-code/opus", EndReason::Paused, &[]).await,
        RecoveryAction::ResumeSame { .. }
    ));
    assert!(matches!(
        decide(&limits, "claude-code/opus", EndReason::Stopped, &[]).await,
        RecoveryAction::Escalate { .. }
    ));
    assert_eq!(
        decide(&limits, "claude-code/opus", EndReason::Completed, &[]).await,
        RecoveryAction::Proceed
    );
}

#[test]
fn session_limits_follow_budget_and_watchdogs() {
    let mut fixture = Fixture::new();
    let limits = fixture.limits();
    let spec = card(CARD);
    let owner = limits.session_limits(Some(&spec), RoleName::Owner);
    assert_eq!(
        owner.budget,
        Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 3.0
        })
    );
    assert!((owner.native_cap_usd.expect("a dollar backstop") - 3.3).abs() < 1e-9);
    assert!((owner.soft_stop - 0.85).abs() < 1e-9);
    assert_eq!(owner.max_minutes, Some(30.0));
    assert_eq!(owner.max_tool_calls, Some(150));
    assert_eq!(owner.inactivity_min, Some(15.0));
    assert_eq!(owner.repeat_limit, Some(10));

    // The card's own limits come first.
    let mut own = card(CARD);
    own.budget.limit = Some(BudgetAmount {
        unit: BudgetUnit::Tokens,
        amount: 200_000.0,
    });
    own.budget.max_minutes = Some(5.0);
    let custom = limits.session_limits(Some(&own), RoleName::Owner);
    assert_eq!(
        custom.budget.map(|budget| budget.unit),
        Some(BudgetUnit::Tokens)
    );
    assert_eq!(custom.native_cap_usd, None);
    assert_eq!(custom.max_minutes, Some(5.0));

    // A session that outlives a card answers to the run's budget lines.
    fixture
        .settings_mut()
        .roles
        .entry(RoleName::Coordinator)
        .or_default()
        .lifetime = Some(argonauts_core::settings::sessions::SessionLifetime::Run);
    let coordinator = fixture.limits().session_limits(None, RoleName::Coordinator);
    assert_eq!(coordinator.budget, None);
    assert_eq!(coordinator.max_minutes, None);
    assert_eq!(coordinator.inactivity_min, Some(15.0));

    // Budgets off: the meter counts, nothing stops for budget.
    let settings = fixture.settings_mut();
    settings.budget.enabled = false;
    settings.budget.soft_stop = f64::NAN;
    let off = fixture.limits().session_limits(Some(&own), RoleName::Owner);
    assert_eq!(off.budget, None);
    assert_eq!(off.native_cap_usd, None);
    assert!((off.soft_stop - argonauts_core::DEFAULT_SOFT_RATIO).abs() < 1e-9);
}

/// The card started on `claude-code/opus` and auto handed it to `codex/gpt-5`.
fn handed_to_codex() -> Vec<RunEvent> {
    vec![
        card_event(
            1,
            CardEvent::Attempt {
                attempt: 1,
                worker: worker("claude-code/opus"),
                cause: AttemptCause::First,
            },
        ),
        event(
            2,
            RunEventKind::Handoff(HandoffEvent::HandedOff {
                card: CardId::new(CARD).expect("a valid card id"),
                to: worker("codex/gpt-5"),
                mode: HandoffMode::Auto,
                by: Party::Rules,
                context: None,
            }),
        ),
    ]
}

#[test]
fn a_returning_original_leaves_the_card_with_a_notice() {
    let fixture = Fixture::with_models();
    let limits = fixture.limits();
    let claude = vendor("claude-code");
    let events = handed_to_codex();
    assert_eq!(
        limits.original_returned(&claude, "Claude Code", &events),
        vec![OriginalBack::Stays {
            card: CardId::new(CARD).expect("a valid card id"),
            notice: "Claude Code is back · card-003 stays with codex/gpt-5 · \
                     /reassign card-003 claude-code/opus moves it back"
                .to_owned(),
        }]
    );
    // Nothing was taken from Codex.
    assert!(
        limits
            .original_returned(&vendor("codex"), "Codex", &events)
            .is_empty()
    );

    // Moved back, or finished, it's no longer away.
    for last in [
        CardEvent::Reassigned {
            from: Some(worker("codex/gpt-5")),
            to: worker("claude-code/opus"),
            by: Party::User,
            rule_override: None,
        },
        CardEvent::State {
            from: Some(CardState::Integrating),
            to: CardState::Integrated,
            reason: "merged".to_owned(),
        },
    ] {
        let mut events = handed_to_codex();
        events.push(card_event(3, last));
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
    let claude = vendor("claude-code");
    let card_id = || CardId::new(CARD).expect("a valid card id");

    let mut events = handed_to_codex();
    events.push(card_event(
        3,
        CardEvent::QualityDebt(argonauts_core::handoff::QualityDebt {
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
            card: card_id(),
            original: worker("claude-code/opus"),
            holder: worker("codex/gpt-5"),
            on_return: OnReturn::Ask,
        }]
    );

    // With the debt cleared, `return_to_original` hands the card back at its next attempt.
    events.push(card_event(
        4,
        CardEvent::DebtCleared {
            by: worker("claude-code/opus"),
        },
    ));
    let handoff = &mut fixture.settings_mut().handoff;
    handoff.on_return = None;
    handoff.return_to_original = true;
    assert_eq!(
        fixture
            .limits()
            .original_returned(&claude, "Claude Code", &events),
        vec![OriginalBack::HandBack {
            card: card_id(),
            to: worker("claude-code/opus"),
        }]
    );
}
