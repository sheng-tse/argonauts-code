use super::*;
use crate::client::test_support::{card_event, card_spec, event, run_id, session_started, usage};
use crate::usage_ledger::UsageLedger;
use argonauts_core::Paths;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::limits::{QuotaReading, QuotaSource};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::settings::handoff::{HandoffMode, HandoffStrategy};
use argonauts_core::steps::judgment::{AdjudicateOutput, ReviewOutput};
use time::macros::datetime;

fn reviewed(
    seq: u64,
    card: &CardId,
    reviewer: &str,
    round: u32,
    verdict: ReviewVerdict,
) -> RunEvent {
    RunEvent {
        card_id: Some(card.clone()),
        ..event(
            seq,
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer: WorkerRef::new(reviewer, None),
                blind: true,
                round,
                review: ReviewOutput {
                    verdict,
                    summary: String::new(),
                    comments: Vec::new(),
                    tamper_suspected: false,
                },
            }),
        )
    }
}

fn integrated(seq: u64, card: &CardId) -> RunEvent {
    card_event(
        seq,
        card,
        CardEvent::State {
            from: Some(CardState::Integrating),
            to: CardState::Integrated,
            reason: String::new(),
        },
    )
}

/// An orchestrator on Claude Code, two tasks built on Claude Code and Codex, the first sent
/// back once by an Antigravity CLI reviewer, a judge's ruling, and Codex taking the second
/// over from Claude Code when its window ran out.
fn a_run() -> Vec<RunEvent> {
    let (plan, build, fix, review, judge, out) = (
        SessionKey::numbered(1),
        SessionKey::numbered(2),
        SessionKey::numbered(3),
        SessionKey::numbered(4),
        SessionKey::numbered(5),
        SessionKey::numbered(6),
    );
    let (one, two) = (card_spec(1), card_spec(2));
    let mut stood_in = session_started(20, &fix, Some(&two.id), RoleName::Worker, "codex");
    stood_in.ts += Duration::seconds(1);
    vec![
        session_started(0, &plan, None, RoleName::Orchestrator, "claude-code"),
        usage(1, &plan, 1000, 100, 0.10),
        card_event(2, &one.id, CardEvent::Created(Box::new(one.clone()))),
        card_event(3, &two.id, CardEvent::Created(Box::new(two.clone()))),
        session_started(4, &build, Some(&one.id), RoleName::Worker, "claude-code"),
        usage(5, &build, 4000, 400, 0.40),
        session_started(
            6,
            &review,
            Some(&one.id),
            RoleName::Reviewer,
            "antigravity-cli",
        ),
        usage(7, &review, 500, 50, 0.05),
        reviewed(
            8,
            &one.id,
            "antigravity-cli",
            1,
            ReviewVerdict::RequestChanges,
        ),
        reviewed(9, &one.id, "antigravity-cli", 2, ReviewVerdict::Approve),
        integrated(10, &one.id),
        session_started(10, &out, Some(&two.id), RoleName::Worker, "claude-code"),
        usage(10, &out, 100, 10, 0.01),
        event(
            11,
            RunEventKind::Vendor(VendorEvent::Quota(QuotaReading {
                vendor: VendorId("claude-code".into()),
                status: QuotaStatus::Rejected,
                used: Some(1.0),
                resets_at: None,
                window: Some("five_hour".into()),
                source: QuotaSource::RateLimitEvent,
                observed_at: datetime!(2026-09-29 12:00 UTC),
                detail: None,
                wrap_up: false,
            })),
        ),
        event(
            11,
            RunEventKind::Handoff(HandoffEvent::Needed(HandoffPlan {
                card: two.id.clone(),
                title: two.title.clone(),
                stopped: WorkerRef::new("claude-code", None),
                reason: "usage limit, resets 3:45pm".into(),
                resets_at: None,
                work: Default::default(),
                gates_ran: false,
                candidates: Vec::new(),
                mode: HandoffMode::Auto,
                strategy: HandoffStrategy::First,
                race_count: 1,
                also_moving: Vec::new(),
            })),
        ),
        event(
            12,
            RunEventKind::Handoff(HandoffEvent::HandedOff {
                card: two.id.clone(),
                to: WorkerRef::new("codex", None),
                mode: HandoffMode::Auto,
                by: argonauts_core::events::run::Party::Rules,
                context: None,
            }),
        ),
        stood_in,
        usage(21, &fix, 2000, 200, 0.20),
        integrated(22, &two.id),
        session_started(
            30,
            &judge,
            Some(&two.id),
            RoleName::Adjudicator,
            "antigravity-cli",
        ),
        RunEvent {
            card_id: Some(two.id.clone()),
            ..event(
                31,
                RunEventKind::Review(ReviewEvent::Adjudicated {
                    adjudicator: WorkerRef::new("antigravity-cli", None),
                    ruling: AdjudicateOutput {
                        verdict: ReviewVerdict::Approve,
                        reasons: Vec::new(),
                        required_changes: Vec::new(),
                    },
                }),
            )
        },
    ]
}

#[test]
fn a_run_end_writes_its_sessions_tasks_reviews_and_itself_once() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorkLedger::beside(&Paths::new(home.path()).usage_dir());
    let at = datetime!(2026-09-30 12:00 UTC);
    ledger.run_ended(Path::new("/repo"), &run_id(), &a_run(), at);
    ledger.run_ended(Path::new("/repo"), &run_id(), &a_run(), at);
    let lines = ledger.read_since(None);
    let count = |pick: fn(&Work) -> bool| lines.iter().filter(|line| pick(&line.what)).count();
    assert_eq!(count(|w| matches!(w, Work::Session { .. })), 6);
    assert_eq!(count(|w| matches!(w, Work::Review { judge: false, .. })), 2);
    assert_eq!(count(|w| matches!(w, Work::Review { judge: true, .. })), 1);
    assert_eq!(count(|w| matches!(w, Work::Run { .. })), 1);
    let tasks: Vec<(String, &str, u32, bool)> = lines
        .iter()
        .filter_map(|line| match &line.what {
            Work::Task {
                task,
                by,
                sent_back,
                merged,
                ..
            } => Some((task.to_string(), by.worker.as_str(), *sent_back, *merged)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tasks,
        [
            ("task-001".to_owned(), "claude-code", 1, true),
            ("task-002".to_owned(), "codex", 0, true),
        ]
    );
    let fallback = lines.iter().find_map(|line| match &line.what {
        Work::Session {
            stood_in_for: Some(from),
            why,
            ..
        } => Some((from.worker.clone(), why.clone())),
        _ => None,
    });
    assert_eq!(
        fallback,
        Some((
            "claude-code".to_owned(),
            "usage limit, resets 3:45pm".to_owned()
        ))
    );
    // The usage ledger's own folder still reads as months only.
    assert!(
        UsageLedger::new(&Paths::new(home.path()))
            .read_since(None)
            .is_empty()
    );
}

#[test]
fn stats_sum_each_cli_model_and_role_with_the_chat_turns() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let usage = UsageLedger::new(&paths);
    let at = datetime!(2026-09-30 12:00 UTC);
    usage.run_ended(Path::new("/repo"), &run_id(), &a_run(), at);
    usage.turn_ended(
        Path::new("/repo"),
        &WorkerRef::new("claude-code", None),
        &Spend {
            input_tokens: 300,
            output_tokens: 30,
            ..Spend::default()
        },
        at,
    );
    let today = local_day(at);
    let lines = usage.read_since(None);
    let stats = stats(&WorkLedger::beside(&paths.usage_dir()), None, today, &lines);

    assert_eq!(stats.workflows, 1);
    assert!(stats.took_secs >= 31);
    let done = |agent: &str, role: CrewRole| -> u32 {
        stats
            .work
            .iter()
            .filter(|row| row.agent == agent && row.role == role)
            .map(|row| row.done)
            .sum()
    };
    assert_eq!(done("claude-code", CrewRole::Orchestrator), 1);
    // Its worker on the second task stopped, so the task is Codex's, and the tokens stay its.
    assert_eq!(done("claude-code", CrewRole::Worker), 1);
    assert_eq!(done("codex", CrewRole::Worker), 1);
    assert_eq!(done("antigravity-cli", CrewRole::Reviewer), 2);
    assert_eq!(done("antigravity-cli", CrewRole::Judge), 1);
    assert_eq!(done("claude-code", CrewRole::Main), 1);
    let reviewer = stats
        .work
        .iter()
        .find(|row| row.role == CrewRole::Reviewer)
        .unwrap();
    assert_eq!(reviewer.sent_back, 1);
    assert_eq!(reviewer.spent.total_tokens(), 550);

    let tokens: u64 = stats.work.iter().map(|row| row.spent.total_tokens()).sum();
    let counted: u64 = lines.iter().map(|line| line.spent().total_tokens()).sum();
    assert_eq!(tokens, counted, "every token the usage ledger counts, once");
    let charted: u64 = stats.days.iter().map(|day| day.tokens).sum();
    assert_eq!(charted, counted);

    assert_eq!(stats.tasks.len(), 2);
    assert_eq!(stats.fallbacks.len(), 1);
    assert_eq!(stats.fallbacks[0].agent, "codex");
    assert_eq!(
        stats.fallbacks[0].window.as_deref(),
        Some("five_hour"),
        "the window Claude Code's last reading said had run out"
    );
}

#[test]
fn a_period_counts_its_own_days_and_the_chart_keeps_seven() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let ledger = WorkLedger::beside(&paths.usage_dir());
    let at = datetime!(2026-09-30 12:00 UTC);
    ledger.run_ended(Path::new("/repo"), &run_id(), &a_run(), at);
    let today = local_day(at) + Duration::days(3);
    let stats = stats(&ledger, Some(today), today, &[]);
    assert_eq!(stats.workflows, 0);
    assert!(stats.work.is_empty());
    assert!(stats.days.iter().map(|day| day.tokens).sum::<u64>() > 0);
}
