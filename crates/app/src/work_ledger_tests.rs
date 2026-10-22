use super::*;
use crate::client::test_support::{
    agent, approval_decided, event, run_id, session_started, task_event, task_spec, usage,
};
use crate::usage_ledger::{TurnNote, UsageLedger};
use argonauts_core::Paths;
use argonauts_core::crew_stats::{Outcomes, StatsRole};
use argonauts_core::events::SessionEvent;
use argonauts_core::events::agent_status::FallbackPlan;
use argonauts_core::limits::{PlanUsageReading, PlanUsageSource};
use argonauts_core::names::{ApprovalId, RoleName, SessionKey};
use argonauts_core::settings::fallback::{FallbackMode, FallbackStrategy};
use argonauts_core::steps::judgment::{ReviewOutput, SettleOutput};
use time::macros::datetime;

fn reviewed(
    seq: u64,
    task: &TaskId,
    reviewer: &str,
    round: u32,
    verdict: ReviewVerdict,
) -> RunEvent {
    RunEvent {
        task_id: Some(task.clone()),
        ..event(
            seq,
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer: ModelRef::new(reviewer, None),
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

fn merged(seq: u64, task: &TaskId) -> RunEvent {
    task_event(
        seq,
        task,
        TaskEvent::State {
            from: Some(TaskState::Merging),
            to: TaskState::Merged,
            reason: String::new(),
        },
    )
}

/// An orchestrator on Claude Code, two tasks built on Claude Code and Codex, the first sent
/// back once by an agy reviewer, a judge's ruling, and Codex taking the second
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
    let (one, two) = (task_spec(1), task_spec(2));
    let mut stood_in = session_started(20, &fix, Some(&two.id), RoleName::Worker, "codex");
    stood_in.ts += Duration::seconds(1);
    vec![
        session_started(0, &plan, None, RoleName::Orchestrator, "claude-code"),
        usage(1, &plan, 1000, 100, 0.10),
        task_event(2, &one.id, TaskEvent::Created(Box::new(one.clone()))),
        task_event(3, &two.id, TaskEvent::Created(Box::new(two.clone()))),
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
        merged(10, &one.id),
        session_started(10, &out, Some(&two.id), RoleName::Worker, "claude-code"),
        usage(10, &out, 100, 10, 0.01),
        event(
            11,
            RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(PlanUsageReading {
                agent_id: AgentId("claude-code".into()),
                status: PlanUsageStatus::Rejected,
                used: Some(1.0),
                resets_at: None,
                window: Some("five_hour".into()),
                source: PlanUsageSource::RateLimitEvent,
                observed_at: datetime!(2026-09-29 12:00 UTC),
                detail: None,
                wrap_up: false,
            })),
        ),
        event(
            11,
            RunEventKind::Fallback(FallbackEvent::Needed(FallbackPlan {
                task: two.id.clone(),
                title: two.title.clone(),
                stopped: ModelRef::new("claude-code", None),
                reason: "usage limit, resets 3:45pm".into(),
                resets_at: None,
                work: Default::default(),
                checks_ran: false,
                candidates: Vec::new(),
                mode: FallbackMode::Auto,
                strategy: FallbackStrategy::First,
                race_count: 1,
                also_moving: Vec::new(),
            })),
        ),
        event(
            12,
            RunEventKind::Fallback(FallbackEvent::FellBack {
                task: two.id.clone(),
                to: ModelRef::new("codex", None),
                mode: FallbackMode::Auto,
                by: argonauts_core::events::run::Party::Rules,
                context: None,
            }),
        ),
        stood_in,
        usage(21, &fix, 2000, 200, 0.20),
        merged(22, &two.id),
        session_started(
            30,
            &judge,
            Some(&two.id),
            RoleName::Judge,
            "antigravity-cli",
        ),
        RunEvent {
            task_id: Some(two.id.clone()),
            ..event(
                31,
                RunEventKind::Review(ReviewEvent::Settled {
                    judge: ModelRef::new("antigravity-cli", None),
                    ruling: SettleOutput {
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
            } => Some((task.to_string(), by.agent.as_str(), *sent_back, *merged)),
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
            fallback_for: Some(from),
            why,
            ..
        } => Some((from.agent.clone(), why.clone())),
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
fn stats_sum_each_cli_model_and_role_with_the_conversation_turns() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let usage = UsageLedger::new(&paths);
    let at = datetime!(2026-09-30 12:00 UTC);
    usage.run_ended(Path::new("/repo"), &run_id(), &a_run(), at);
    // Three turns of one conversation, and a question put beside it.
    let main = ModelRef::new("claude-code", None);
    let turn = Spend {
        input_tokens: 100,
        output_tokens: 10,
        ..Spend::default()
    };
    for conversation in [Some("conversation-001"); 3].into_iter().chain([None]) {
        let turn_took = TurnNote {
            conversation,
            took: 20,
        };
        usage.turn_ended(Path::new("/repo"), &main, &turn, turn_took, at);
    }
    let lines = usage.read_since(None);
    let stats = stats(&WorkLedger::beside(&paths.usage_dir()), None, &lines);

    assert_eq!(stats.workflows, 1);
    assert!(stats.took_secs >= 31);
    let done = |agent: &str, role: StatsRole| -> u32 {
        stats
            .work
            .iter()
            .filter(|row| row.agent == agent && row.role == role)
            .map(|row| row.done)
            .sum()
    };
    assert_eq!(done("claude-code", StatsRole::Orchestrator), 1);
    // Its worker on the second task stopped, so the task is Codex's, and the tokens stay its.
    assert_eq!(done("claude-code", StatsRole::Worker), 1);
    assert_eq!(done("codex", StatsRole::Worker), 1);
    assert_eq!(done("antigravity-cli", StatsRole::Reviewer), 2);
    // The judge's ruling let the work stand. It signed nothing off, so that is no piece of
    // its work, though its session's tokens are its own.
    assert_eq!(done("antigravity-cli", StatsRole::Judge), 0);
    assert_eq!(stats.outcomes.sided_with_worker, 1);
    assert_eq!(stats.outcomes.sided_with_reviewer, 0);
    // A conversation is the main agent's piece of work, however many turns it took.
    assert_eq!(done("claude-code", StatsRole::Main), 2);
    let main = stats
        .work
        .iter()
        .find(|row| row.role == StatsRole::Main)
        .unwrap();
    assert_eq!((main.turns, main.took_secs), (4, 80));
    let reviewer = stats
        .work
        .iter()
        .find(|row| row.role == StatsRole::Reviewer)
        .unwrap();
    assert_eq!(reviewer.sent_back, 1);
    assert_eq!(reviewer.spent.total_tokens(), 550);

    let tokens: u64 = stats.work.iter().map(|row| row.spent.total_tokens()).sum();
    let counted: u64 = lines.iter().map(|line| line.spent().total_tokens()).sum();
    assert_eq!(tokens, counted, "every token the usage ledger counts, once");
    let charted: u64 = stats.days.iter().map(|day| day.tokens).sum();
    assert_eq!(charted, counted);

    assert_eq!(stats.tasks.len(), 2);
    // A task carries why each review that sent it back did.
    let returned: Vec<&[String]> = stats
        .tasks
        .iter()
        .map(|task| task.reasons.as_slice())
        .filter(|reasons| !reasons.is_empty())
        .collect();
    assert_eq!(returned, [["changes asked for".to_owned()]]);
    assert_eq!(stats.fallbacks.len(), 1);
    assert_eq!(stats.fallbacks[0].agent, "codex");
    assert_eq!(
        stats.fallbacks[0].window.as_deref(),
        Some("five_hour"),
        "the window Claude Code's last reading said had run out"
    );
}

#[test]
fn a_period_counts_only_its_own_days() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let ledger = WorkLedger::beside(&paths.usage_dir());
    let at = datetime!(2026-09-30 12:00 UTC);
    ledger.run_ended(Path::new("/repo"), &run_id(), &a_run(), at);
    let later = local_day(at) + Duration::days(3);
    let stats = stats(&ledger, Some(later), &[]);
    assert_eq!(stats.workflows, 0);
    assert!(stats.work.is_empty());
    assert!(stats.days.is_empty());

    let every_day = stats_of(&ledger);
    let day = local_day(at);
    let reviews: u32 = every_day
        .days
        .iter()
        .filter(|part| part.role == Some(StatsRole::Reviewer))
        .map(|part| part.done)
        .sum();
    assert_eq!(reviews, 2, "each day keeps what each role did");
    assert!(every_day.days.iter().all(|part| part.day <= day));
}

fn stats_of(ledger: &WorkLedger) -> CrewStats {
    stats(ledger, None, &[])
}

/// A plan made twice, a task whose worker called two tools, the merger settling a conflict
/// and writing the commit message, the judge signing the merge off, and the work reaching
/// your branch with its checks passing.
fn a_run_to_its_merge() -> Vec<RunEvent> {
    let (plan, build, conflict, message) = (
        SessionKey::numbered(1),
        SessionKey::numbered(2),
        SessionKey::numbered(3),
        SessionKey::numbered(4),
    );
    let one = task_spec(1);
    // The merger's sessions start as a worker's, told apart by their step or their label.
    let merging = |seq: u64, key: &SessionKey, step: Option<StepName>, label: &str| {
        let mut started = session_started(seq, key, None, RoleName::Worker, "claude-code");
        if let RunEventKind::Session(SessionEvent::Started(start)) = &mut started.kind {
            start.step = step;
            start.label = Some(label.to_owned());
        }
        started
    };
    let planned = |seq: u64| {
        event(
            seq,
            RunEventKind::Plan(PlanEvent::Orchestrated {
                orchestrator: ModelRef::new("claude-code", None),
                rejection: None,
            }),
        )
    };
    let tool = |seq: u64, id: &str| {
        let started = AgentEvent::ToolStarted {
            id: id.to_owned(),
            name: "Bash".to_owned(),
            input: serde_json::Value::Null,
            parent: None,
        };
        agent(seq, &build, started)
    };
    let judge = Party::Judge {
        model: ModelRef::new("antigravity-cli", None),
    };
    vec![
        session_started(0, &plan, None, RoleName::Orchestrator, "claude-code"),
        planned(1),
        planned(2),
        task_event(3, &one.id, TaskEvent::Created(Box::new(one.clone()))),
        session_started(4, &build, Some(&one.id), RoleName::Worker, "codex"),
        tool(5, "a"),
        tool(6, "b"),
        usage(7, &build, 4000, 400, 0.40),
        merged(8, &one.id),
        merging(
            9,
            &conflict,
            Some(StepName::ResolveConflict),
            "resolve:main",
        ),
        usage(10, &conflict, 300, 30, 0.03),
        merging(11, &message, None, COMMIT_MESSAGE),
        usage(12, &message, 100, 10, 0.01),
        approval_decided(
            13,
            &ApprovalId::numbered(1),
            AnswerAction::Merge,
            judge,
            None,
        ),
        event(
            14,
            RunEventKind::Merge(MergeEvent::FinalCheck { passed: true }),
        ),
        event(
            15,
            RunEventKind::Merge(MergeEvent::RunMerged {
                target: "main".into(),
                sha: "abc".into(),
                fast_forward: true,
            }),
        ),
    ]
}

#[test]
fn the_merger_the_judges_sign_off_and_how_the_run_ended_are_filed() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorkLedger::beside(&Paths::new(home.path()).usage_dir());
    let at = datetime!(2026-09-30 12:00 UTC);
    ledger.run_ended(Path::new("/repo"), &run_id(), &a_run_to_its_merge(), at);
    let lines = ledger.read_since(None);

    let sessions: Vec<(&str, &str, u32)> = lines
        .iter()
        .filter_map(|line| match &line.what {
            Work::Session {
                role, by, tools, ..
            } => Some((role.as_str(), by.agent.as_str(), *tools)),
            _ => None,
        })
        .collect();
    assert_eq!(
        sessions,
        [
            ("orchestrator", "claude-code", 0),
            ("worker", "codex", 2),
            ("merger", "claude-code", 0),
            ("merger", "claude-code", 0),
        ]
    );
    assert!(lines.iter().any(|line| matches!(
        &line.what,
        Work::Signoff { by, refused: false } if by.agent == "antigravity-cli"
    )));
    let outcome = lines.iter().find_map(|line| match &line.what {
        Work::Outcome {
            merged,
            replans,
            merge,
            merged_by,
            checks_failed,
            ..
        } => Some((
            *merged,
            *replans,
            *merge,
            merged_by.as_ref().map(|by| by.agent.clone()),
            *checks_failed,
        )),
        _ => None,
    });
    let claude = Some("claude-code".to_owned());
    assert_eq!(
        outcome,
        Some((true, 1, Some(MergeEnd::Settled), claude, false))
    );
    // The task is still its builder's, whoever settled the conflict after.
    assert!(lines.iter().any(|line| matches!(
        &line.what,
        Work::Task { by, merged: true, .. } if by.agent == "codex"
    )));

    let stats = stats_of(&ledger);
    let row = |agent: &str, role: StatsRole| {
        stats
            .work
            .iter()
            .find(|row| row.agent == agent && row.role == role)
            .unwrap_or_else(|| panic!("{agent} as {role:?}: {:#?}", stats.work))
    };
    let merger = row("claude-code", StatsRole::Merger);
    assert_eq!(
        (merger.done, merger.sessions),
        (1, 2),
        "one merge, two sessions"
    );
    assert_eq!(merger.spent.total_tokens(), 440);
    let worker = row("codex", StatsRole::Worker);
    assert_eq!((worker.done, worker.sessions, worker.tool_calls), (1, 1, 2));
    let judge = row("antigravity-cli", StatsRole::Judge);
    assert_eq!(
        (judge.done, judge.sent_back),
        (1, 0),
        "a sign-off, not refused"
    );
    assert_eq!(
        stats.outcomes,
        Outcomes {
            runs: 1,
            runs_merged: 1,
            runs_replanned: 1,
            merges_settled: 1,
            ..Outcomes::default()
        }
    );
}

#[test]
fn a_merge_is_clean_with_no_conflict_and_failed_when_one_kept_the_work_out() {
    let none = MergeFacts::default();
    assert_eq!(none.end(), None, "a run that never got to its merge");
    let merged = MergeFacts {
        merged: true,
        ..MergeFacts::default()
    };
    assert_eq!(merged.end(), Some(MergeEnd::Clean));
    let offered = MergeFacts {
        offered: true,
        conflict: true,
        ..MergeFacts::default()
    };
    assert_eq!(offered.end(), Some(MergeEnd::Settled));
    let kept_out = MergeFacts {
        conflict: true,
        ..MergeFacts::default()
    };
    assert_eq!(kept_out.end(), Some(MergeEnd::Failed));
    // The judge turning the work down is a sign-off refused.
    assert!(refuses(&AnswerAction::RequestChanges));
    assert!(!refuses(&AnswerAction::Merge));
}

/// The model's name as the CLI reported it at its start, as `init` carries it.
fn init(seq: u64, session: &SessionKey, model: &str) -> RunEvent {
    RunEvent {
        session: Some(session.clone()),
        ..event(
            seq,
            RunEventKind::Agent(AgentEvent::Init {
                session_id: None,
                model: Some(model.to_owned()),
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            }),
        )
    }
}

#[test]
fn work_is_filed_under_the_model_the_cli_said_it_ran() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorkLedger::beside(&Paths::new(home.path()).usage_dir());
    let mut events = a_run();
    // The first task's worker said at its start that it runs Haiku 5.5.
    events.push(init(40, &SessionKey::numbered(2), "claude-haiku-5-5"));
    let at = datetime!(2026-09-30 12:00 UTC);
    ledger.run_ended(Path::new("/repo"), &run_id(), &events, at);
    let stats = stats_of(&ledger);
    let haiku = Some("claude-haiku-5-5");
    let worker = stats
        .work
        .iter()
        .find(|row| row.role == StatsRole::Worker && row.model.as_deref() == haiku)
        .unwrap();
    assert_eq!(worker.spent.total_tokens(), 4400);
    let task = stats
        .tasks
        .iter()
        .find(|task| task.model.as_deref() == haiku);
    assert_eq!(task.map(|task| task.sent_back), Some(1));
}

#[test]
fn a_review_says_why_it_sent_work_back_and_which_checks_it_was_given() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorkLedger::beside(&Paths::new(home.path()).usage_dir());
    let mut events = a_run();
    let one = task_spec(1);
    let check = |seq: u64, passed: bool| RunEvent {
        task_id: Some(one.id.clone()),
        ..event(
            seq,
            RunEventKind::Check(argonauts_core::events::task::CheckOutcome {
                name: "tests".into(),
                kind: CheckKind::Command,
                passed,
                command: Some(vec!["cargo".into(), "test".into()]),
                exit_code: Some(i32::from(!passed)),
                duration_ms: 10,
                summary: String::new(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            }),
        )
    };
    // The checks failed before the first review and passed before the second.
    events.insert(7, check(7, false));
    events.insert(10, check(8, true));
    let at = datetime!(2026-09-30 12:00 UTC);
    ledger.run_ended(Path::new("/repo"), &run_id(), &events, at);
    let stats = stats_of(&ledger);
    let reviewer = stats
        .work
        .iter()
        .find(|row| row.role == StatsRole::Reviewer)
        .unwrap();
    assert_eq!(reviewer.review.checks, ["cargo test"]);
    assert_eq!((reviewer.review.passed, reviewer.review.failed), (1, 1));
    assert_eq!(
        reviewer.review.reasons,
        [("a check failed".to_owned(), 1)].into()
    );
}

#[test]
fn records_that_name_no_model_are_filed_under_the_clis_default() {
    let mut stats = CrewStats {
        work: vec![WorkRow {
            agent: "claude-code".into(),
            model: None,
            role: StatsRole::Worker,
            kind: None,
            spent: Spend::default(),
            done: 1,
            sent_back: 0,
            took_secs: 0,
            sessions: 0,
            tool_calls: 0,
            turns: 0,
            review: ReviewTally::default(),
        }],
        ..CrewStats::default()
    };
    let defaults = [("claude-code".to_owned(), "claude-haiku-5-5".to_owned())].into();
    name_unnamed_models(&mut stats, &defaults);
    assert_eq!(stats.work[0].model.as_deref(), Some("claude-haiku-5-5"));
}

/// A session of `worker` started under `key`, on `model` when its start names one.
fn started_on(
    seq: u64,
    key: &SessionKey,
    task: &TaskId,
    role: RoleName,
    (worker, model): (&str, Option<&str>),
) -> RunEvent {
    let mut event = session_started(seq, key, Some(task), role, worker);
    if let RunEventKind::Session(SessionEvent::Started(started)) = &mut event.kind {
        started.worker = ModelRef::new(worker, model.map(str::to_owned));
    }
    event
}

/// A review the session `session` made, naming its reviewer by the CLI alone.
fn reviewed_in(seq: u64, session: &SessionKey, task: &TaskId, verdict: ReviewVerdict) -> RunEvent {
    RunEvent {
        session: Some(session.clone()),
        ..reviewed(seq, task, "codex", 1, verdict)
    }
}

/// A Captain run as its log had it: the worker on Claude Code started with the alias
/// `haiku`, which its CLI reported as `claude-haiku-5-5`, then going on with Codex under the
/// same key, whose start reported no model; a Codex reviewer, and a Codex judge whose verdict
/// was recorded as a review. Both reviews name only the CLI.
fn a_captain_run() -> Vec<RunEvent> {
    let (worker, reviewer, judge) = (
        SessionKey::numbered(16),
        SessionKey::numbered(17),
        SessionKey::numbered(18),
    );
    let task = task_spec(1);
    let id = &task.id;
    vec![
        task_event(0, id, TaskEvent::Created(Box::new(task.clone()))),
        started_on(
            1,
            &worker,
            id,
            RoleName::Worker,
            ("claude-code", Some("haiku")),
        ),
        init(2, &worker, "claude-haiku-5-5"),
        usage(3, &worker, 1000, 100, 0.01),
        started_on(4, &worker, id, RoleName::Worker, ("codex", None)),
        usage(5, &worker, 2000, 200, 0.02),
        started_on(6, &reviewer, id, RoleName::Reviewer, ("codex", None)),
        init(7, &reviewer, "gpt-6-luna"),
        usage(8, &reviewer, 300, 30, 0.003),
        reviewed_in(9, &reviewer, id, ReviewVerdict::RequestChanges),
        started_on(10, &judge, id, RoleName::Judge, ("codex", None)),
        init(11, &judge, "gpt-6-luna"),
        usage(12, &judge, 100, 10, 0.001),
        reviewed_in(13, &judge, id, ReviewVerdict::Approve),
        merged(14, id),
    ]
}

/// Each session keeps the model it ran, a review goes to the session that made it, and the
/// judge's verdict is a sign-off, so nothing lands on Codex under Claude Code's model.
#[test]
fn each_session_keeps_its_own_model_and_its_own_reviews() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorkLedger::beside(&Paths::new(home.path()).usage_dir());
    let at = datetime!(2026-10-14 12:00 UTC);
    ledger.run_ended(Path::new("/repo"), &run_id(), &a_captain_run(), at);
    let lines = ledger.read_since(None);
    let model = |agent: &str, model: Option<&str>| ModelRef::new(agent, model.map(str::to_owned));
    let luna = model("codex", Some("gpt-6-luna"));
    let sessions: Vec<&ModelRef> = lines
        .iter()
        .filter_map(|line| match &line.what {
            Work::Session { by, .. } => Some(by),
            _ => None,
        })
        .collect();
    assert_eq!(
        sessions,
        [
            &model("claude-code", Some("claude-haiku-5-5")),
            &model("codex", None),
            &luna,
            &luna,
        ]
    );
    let reviews: Vec<(&ModelRef, bool)> = lines
        .iter()
        .filter_map(|line| match &line.what {
            Work::Review { by, sent_back, .. } => Some((by, *sent_back)),
            _ => None,
        })
        .collect();
    assert_eq!(reviews, [(&luna, true)]);
    let signoffs: Vec<(&ModelRef, bool)> = lines
        .iter()
        .filter_map(|line| match &line.what {
            Work::Signoff { by, refused } => Some((by, *refused)),
            _ => None,
        })
        .collect();
    assert_eq!(signoffs, [(&luna, false)]);

    let stats = stats_of(&ledger);
    let done = |role: StatsRole| -> Vec<(Option<&str>, u32)> {
        (stats.work.iter())
            .filter(|row| row.role == role && row.done > 0)
            .map(|row| (row.model.as_deref(), row.done))
            .collect()
    };
    assert_eq!(done(StatsRole::Reviewer), [(Some("gpt-6-luna"), 1)]);
    assert_eq!(done(StatsRole::Judge), [(Some("gpt-6-luna"), 1)]);
    let task = &stats.tasks[0];
    assert_eq!(
        (task.agent.as_str(), task.model.as_deref()),
        ("codex", None)
    );
    assert!(task.reviewed);
}

/// The usage ledger files a run's models as the work ledger does, so the two agree on one
/// spelling: the alias `haiku` a session started with is the catalog's `claude-haiku-5-5`, as
/// is a turn of the conversation on it.
#[test]
fn both_ledgers_file_one_model_under_one_spelling() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    let usage_ledger = UsageLedger::new(&paths);
    let mut events = a_captain_run();
    // This time the CLI didn't say which model it ran.
    events.retain(|event| !matches!(event.kind, RunEventKind::Agent(AgentEvent::Init { .. })));
    let at = datetime!(2026-10-14 12:00 UTC);
    usage_ledger.run_ended(Path::new("/repo"), &run_id(), &events, at);
    let haiku = ModelRef::new("claude-code", Some("haiku".to_owned()));
    let spent = Spend {
        input_tokens: 10,
        ..Spend::default()
    };
    usage_ledger.turn_ended(Path::new("/repo"), &haiku, &spent, TurnNote::default(), at);

    let spelled: BTreeSet<(String, Option<String>)> = usage_ledger
        .read_since(None)
        .into_iter()
        .map(|line| (line.agent, line.model))
        .collect();
    let claude = (
        "claude-code".to_owned(),
        Some("claude-haiku-5-5".to_owned()),
    );
    assert!(spelled.contains(&claude), "{spelled:?}");
    assert!(
        spelled
            .iter()
            .all(|(_, model)| model.as_deref() != Some("haiku"))
    );
    let work = WorkLedger::beside(&paths.usage_dir());
    let filed = work
        .read_since(None)
        .into_iter()
        .find_map(|line| match line.what {
            Work::Session { by, .. } if by.agent == "claude-code" => Some(by),
            _ => None,
        });
    assert_eq!(filed.map(|by| (by.agent, by.model)), Some(claude));
}

fn check(seq: u64, task: &TaskId, passed: bool) -> RunEvent {
    use argonauts_core::events::task::CheckOutcome;
    task_event_kind(
        seq,
        task,
        RunEventKind::Check(CheckOutcome {
            name: "tests".to_owned(),
            kind: CheckKind::Command,
            passed,
            command: Some(vec!["cargo".to_owned(), "test".to_owned()]),
            exit_code: Some(i32::from(!passed)),
            duration_ms: 10,
            summary: String::new(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        }),
    )
}

fn task_event_kind(seq: u64, task: &TaskId, kind: RunEventKind) -> RunEvent {
    RunEvent {
        task_id: Some(task.clone()),
        ..event(seq, kind)
    }
}

/// [`a_run`], where the first review found two things, one of them blocking; the first task's
/// checks failed before it and passed after; the second task's builder stopped at its usage
/// limit; you approved the merge once it had been sent back; and the run was stopped.
fn a_run_with_outcomes() -> Vec<RunEvent> {
    let mut events = a_run();
    let one = task_spec(1).id;
    for event in &mut events {
        if let RunEventKind::Review(ReviewEvent::Reviewed {
            round: 1, review, ..
        }) = &mut event.kind
        {
            let comment = |severity| argonauts_core::steps::judgment::ReviewComment {
                path: "src/money.rs".to_owned(),
                line: Some(4),
                severity,
                text: "rounding".to_owned(),
            };
            review.comments = vec![comment(Severity::High), comment(Severity::Low)];
        }
    }
    let at = |events: &[RunEvent], seq| events.iter().position(|event| event.seq == seq).unwrap();
    events.insert(at(&events, 8), check(7, &one, false));
    events.insert(at(&events, 10), check(9, &one, true));
    let ended = crate::client::test_support::session_ended(
        11,
        &SessionKey::numbered(6),
        EndReason::UsageLimit { resets_at: None },
        1_000,
    );
    events.insert(at(&events, 11), ended);
    let merge = ApprovalId::numbered(9);
    let opened = |seq| {
        crate::client::test_support::opened(
            seq,
            &merge,
            ApprovalKind::Merge,
            None,
            argonauts_core::events::approval::ApprovalPacket::WorkflowPromotion(
                serde_json::Value::Null,
            ),
        )
    };
    events.push(opened(40));
    events.push(approval_decided(
        41,
        &merge,
        AnswerAction::SendBack,
        Party::User,
        None,
    ));
    events.push(opened(42));
    events.push(approval_decided(
        43,
        &merge,
        AnswerAction::Approve,
        Party::User,
        None,
    ));
    events.push(event(
        44,
        RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Stopped,
            detail: None,
        }),
    ));
    events
}

/// What the day's report counts of a run: each review's findings, the first check of each task,
/// a fallback's cause as a word, who approved the merge, and how the run ended.
#[test]
fn a_run_end_keeps_how_the_work_went() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorkLedger::beside(&Paths::new(home.path()).usage_dir());
    let at = datetime!(2026-09-30 12:00 UTC);
    ledger.run_ended(Path::new("/repo"), &run_id(), &a_run_with_outcomes(), at);
    let lines = ledger.read_since(None);
    let findings: Vec<(u32, u32)> = lines
        .iter()
        .filter_map(|line| match &line.what {
            Work::Review {
                judge: false,
                findings,
                blocking,
                ..
            } => Some((*findings, *blocking)),
            _ => None,
        })
        .collect();
    assert_eq!(findings, [(2, 1), (0, 0)]);
    let checks: Vec<(String, Option<bool>, Option<bool>)> = lines
        .iter()
        .filter_map(|line| match &line.what {
            Work::Task {
                task,
                checks_first,
                checks_last,
                ..
            } => Some((task.to_string(), *checks_first, *checks_last)),
            _ => None,
        })
        .collect();
    assert_eq!(
        checks,
        [
            ("task-001".to_owned(), Some(false), Some(true)),
            ("task-002".to_owned(), None, None),
        ]
    );
    let cause = lines.iter().find_map(|line| match &line.what {
        Work::Session {
            fallback_for: Some(_),
            cause,
            ..
        } => *cause,
        _ => None,
    });
    assert_eq!(cause, Some(FallbackCause::UsageLimit));
    let outcome = lines.iter().find_map(|line| match &line.what {
        Work::Outcome {
            ended,
            merge_approved_by,
            merge_sent_back,
            ..
        } => Some((*ended, *merge_approved_by, *merge_sent_back)),
        _ => None,
    });
    assert_eq!(
        outcome,
        Some((Some(RunOutcome::Stopped), Some(ApprovedBy::You), 1))
    );
}

/// An end reason reads as the fixed word the report sends.
#[test]
fn a_fallbacks_cause_is_one_of_a_few_words() {
    use argonauts_core::event::ErrorCategory;
    let cli = |category| EndReason::CliError {
        category,
        message: "secret detail".to_owned(),
    };
    let causes = [
        (
            EndReason::Auth {
                message: String::new(),
            },
            FallbackCause::SignedOut,
        ),
        (EndReason::RateLimited, FallbackCause::UsageLimit),
        (cli(ErrorCategory::Overloaded), FallbackCause::Down),
        (cli(ErrorCategory::Network), FallbackCause::Down),
        (
            EndReason::ModelUnavailable {
                model: "m".to_owned(),
            },
            FallbackCause::ModelUnavailable,
        ),
        (
            EndReason::Crashed {
                message: String::new(),
            },
            FallbackCause::Other,
        ),
    ];
    for (reason, cause) in causes {
        assert_eq!(FallbackCause::of(&reason), cause, "{reason:?}");
    }
}
