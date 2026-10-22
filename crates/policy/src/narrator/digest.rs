//! Digest lines, built by code from run events: phase and task changes, plans and who drafted
//! them, tool call names with short results, check results, message excerpts, agent outages,
//! and spend. Never transcripts, file contents, prompts, diffs, long tool output, or
//! credentials.

use argonauts_core::PhaseStatus;
use argonauts_core::events::agent_status::{AgentStatusEvent, FallbackEvent};
use argonauts_core::events::approval::ApprovalEvent;
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DesignDecision, JudgingResult, PlanEvent};
use argonauts_core::events::run::{Party, RunLifecycle, RunPhase};
use argonauts_core::events::shared::{BlackboardEvent, BlackboardKind};
use argonauts_core::events::{
    MergeEvent, ReviewEvent, RunEvent, RunEventKind, SpendEvent, TaskEvent,
};
use argonauts_core::limits::{HealthState, ModelState, PlanUsageStatus};
use argonauts_core::{AgentEvent, ErrorCategory};
use serde::Serialize;

use crate::session::clean_text;

/// Message excerpts are cut to this many characters.
pub const EXCERPT_MAX: usize = 200;

/// Short results, reasons, and names are cut to this many characters.
const SHORT_MAX: usize = 80;

/// No digest line is longer than this, whatever it holds.
const LINE_MAX: usize = 320;

/// One short line when something a person would care about happened, or `None`.
pub fn line(event: &RunEvent) -> Option<String> {
    let text = match &event.kind {
        RunEventKind::Run(lifecycle) => run_line(lifecycle)?,
        RunEventKind::Phase(change) => {
            let mut text = format!("{} phase {}", name(&change.phase), name(&change.status));
            if let Some(summary) = &change.summary {
                text.push_str(&format!(": {}", short(summary, SHORT_MAX)));
            }
            text
        }
        RunEventKind::Goal(goal) => goal_line(goal)?,
        RunEventKind::Plan(plan) => plan_line(plan)?,
        RunEventKind::Task(task) => task_line(event, task)?,
        RunEventKind::Agent(agent) => agent_line(event, agent)?,
        RunEventKind::Check(check) => format!(
            "{}: check {} {}: {}",
            who(event),
            short(&check.name, SHORT_MAX),
            if check.passed { "passed" } else { "failed" },
            short(&check.summary, SHORT_MAX)
        ),
        RunEventKind::Review(review) => review_line(event, review)?,
        RunEventKind::Merge(merge) => merge_line(merge)?,
        RunEventKind::Approval(approval) => approval_line(approval)?,
        RunEventKind::Fallback(fallback) => fallback_line(fallback)?,
        RunEventKind::AgentStatus(agent_id) => agent_status_line(agent_id)?,
        RunEventKind::Spend(limit) => limit_line(limit)?,
        RunEventKind::Blackboard(BlackboardEvent::Posted(message)) => format!(
            "{} from {}: \"{}\"",
            name(&message.kind),
            party(&message.from),
            short(&message.text, EXCERPT_MAX)
        ),
        // Session records hold prompts and full results, and the rest is bookkeeping.
        _ => return None,
    };
    Some(short(&text, LINE_MAX))
}

/// A running total that replaces the last one rather than adding a line.
pub fn is_spend(event: &RunEvent) -> bool {
    matches!(event.kind, RunEventKind::Spend(SpendEvent::Run(_)))
}

/// The narrator's goal: what you asked for when the run started, until a crew run approves
/// a goal, whose summary replaces it. A Captain run never has one.
pub fn goal(event: &RunEvent) -> Option<String> {
    match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => {
            Some(short(&started.request, EXCERPT_MAX * 2))
        }
        RunEventKind::Goal(GoalEvent::Approved { file, .. }) => {
            Some(short(&file.summary, EXCERPT_MAX * 2))
        }
        _ => None,
    }
}

/// A decision record: the plan's design decisions, your answers at approvals, and decisions
/// posted to the blackboard.
pub fn decision(event: &RunEvent) -> Option<String> {
    match &event.kind {
        RunEventKind::Plan(PlanEvent::Decided(decided)) => Some(design_decision(decided)),
        RunEventKind::Approval(ApprovalEvent::Decided(decided)) => Some(format!(
            "{}: {} by {}",
            decided.id,
            name(&decided.action),
            party(&decided.by)
        )),
        RunEventKind::Blackboard(BlackboardEvent::Posted(message))
            if message.kind == BlackboardKind::Decision =>
        {
            Some(format!(
                "{}: {}",
                message.id,
                short(&message.text, EXCERPT_MAX)
            ))
        }
        _ => None,
    }
}

/// The phase that just ended, finished or failed.
pub fn phase_end(event: &RunEvent) -> Option<RunPhase> {
    match &event.kind {
        RunEventKind::Phase(change)
            if matches!(change.status, PhaseStatus::Finished | PhaseStatus::Failed) =>
        {
            Some(change.phase)
        }
        _ => None,
    }
}

fn run_line(lifecycle: &RunLifecycle) -> Option<String> {
    Some(match lifecycle {
        RunLifecycle::Started(_) => "the run started".to_owned(),
        RunLifecycle::Named(named) => format!("the run is {}", named.name),
        RunLifecycle::Resumed(_) => "the run resumed".to_owned(),
        RunLifecycle::PauseRequested { by, .. } => format!("{} asked to pause", party(by)),
        RunLifecycle::Paused { .. } => "the run paused".to_owned(),
        RunLifecycle::StopRequested { by } => format!("{} asked to stop", party(by)),
        RunLifecycle::ApprovalModeChanged { mode, .. } => {
            format!("approval mode is now {}", name(mode))
        }
        RunLifecycle::Waiting { reason, .. } => format!("the run is waiting: {}", name(reason)),
        RunLifecycle::Finished { outcome, .. } => format!("the run finished: {}", name(outcome)),
        RunLifecycle::PauseCancelled
        | RunLifecycle::Snapshot(_)
        | RunLifecycle::ClosingAnswer(_) => return None,
    })
}

fn goal_line(goal: &GoalEvent) -> Option<String> {
    Some(match goal {
        GoalEvent::Read { worker, .. } => format!("{worker} read the request back"),
        GoalEvent::Settled { questions, .. } => {
            format!("the goal settled with {} open questions", questions.len())
        }
        GoalEvent::Drafted(_) => "the goal was drafted".to_owned(),
        GoalEvent::Approved { file, by } => format!(
            "{} approved the goal: \"{}\"",
            party(by),
            short(&file.summary, EXCERPT_MAX)
        ),
        GoalEvent::ChangeRequested { from, .. } => {
            format!("{} asked to change the goal", party(from))
        }
        _ => return None,
    })
}

fn plan_line(plan: &PlanEvent) -> Option<String> {
    Some(match plan {
        PlanEvent::Sealed {
            label,
            worker,
            round: 1,
            ..
        } => format!("plan {label} sealed, drafted by {worker}"),
        PlanEvent::Sealed {
            label,
            worker,
            round,
            ..
        } => format!("plan {label} sealed, revised by {worker} (round {round})"),
        PlanEvent::FastPath { merged_by } => {
            format!("no disputed topics, so {merged_by} merged the drafts")
        }
        PlanEvent::Judged(judged) => judged_line(judged),
        PlanEvent::Decided(decided) => format!("decided {}", design_decision(decided)),
        PlanEvent::Orchestrated {
            orchestrator,
            rejection: None,
        } => format!("{orchestrator}'s plan was accepted"),
        PlanEvent::Orchestrated {
            orchestrator,
            rejection: Some(rule),
        } => format!(
            "{orchestrator}'s plan was rejected: {}",
            short(rule, SHORT_MAX)
        ),
        _ => return None,
    })
}

fn judged_line(judged: &JudgingResult) -> String {
    let margin = judged.margin;
    match judged.winner {
        _ if judged.near_tie => {
            format!("the judges found a near tie (margin {margin:.2}), so the plan is your call")
        }
        Some(winner) => {
            let author = judged
                .plans
                .iter()
                .find(|plan| plan.label == winner)
                .map(|plan| format!(" by {}", plan.author))
                .unwrap_or_default();
            format!("the judges picked plan {winner}{author}, margin {margin:.2}")
        }
        None => "the judges found no clear winner".to_owned(),
    }
}

fn design_decision(decided: &DesignDecision) -> String {
    format!(
        "{}: {}: {}",
        decided.id,
        short(&decided.title, SHORT_MAX),
        short(&decided.choice, SHORT_MAX)
    )
}

fn task_line(event: &RunEvent, task: &TaskEvent) -> Option<String> {
    let who = who(event);
    Some(match task {
        TaskEvent::Created(spec) => {
            format!("{} created: {}", spec.id, short(&spec.title, SHORT_MAX))
        }
        TaskEvent::State { from, to, reason } => {
            let mut text = match from {
                Some(from) => format!("{who}: {} → {}", name(from), name(to)),
                None => format!("{who}: {}", name(to)),
            };
            if !reason.trim().is_empty() {
                text.push_str(&format!(" ({})", short(reason, SHORT_MAX)));
            }
            text
        }
        TaskEvent::Try {
            try_number,
            worker,
            cause,
            ..
        } => format!("{who}: try {try_number} by {worker} ({})", name(cause)),
        TaskEvent::Reported(_) => format!("{who} reported"),
        TaskEvent::Reassigned { to, by, .. } => {
            format!("{who} reassigned to {to} by {}", party(by))
        }
        TaskEvent::CheckpointTagged {
            from, to, reason, ..
        } => {
            let to: Vec<String> = to.iter().map(ToString::to_string).collect();
            format!(
                "{who} fell back from {from} to {}: {}",
                to.join(", "),
                short(reason, SHORT_MAX)
            )
        }
        TaskEvent::TakenOver { .. } => format!("{who} taken over by you"),
        TaskEvent::Rewound { to, .. } => format!("{who} rewound to checkpoint {to}"),
        _ => return None,
    })
}

fn agent_line(event: &RunEvent, agent: &AgentEvent) -> Option<String> {
    let who = who(event);
    Some(match agent {
        // Only what the agent says at its top level; subagents and reasoning are transcript.
        AgentEvent::Message { text, parent: None } => {
            format!("{who} said: \"{}\"", short(text, EXCERPT_MAX))
        }
        AgentEvent::ToolStarted { name, .. } => {
            format!("{who} ran {}", short(name, SHORT_MAX))
        }
        // Success says nothing new, and a tool's output is often file contents.
        AgentEvent::ToolFinished {
            ok: false, output, ..
        } => {
            let first = output
                .as_deref()
                .and_then(|output| output.lines().find(|line| !line.trim().is_empty()));
            match first {
                Some(first) => format!("{who}: a tool call failed: {}", short(first, SHORT_MAX)),
                None => format!("{who}: a tool call failed"),
            }
        }
        AgentEvent::Error {
            category: ErrorCategory::Stalled,
            ..
        } => return None,
        AgentEvent::Error { message, .. } => {
            format!("{who} hit an error: {}", short(message, SHORT_MAX))
        }
        _ => return None,
    })
}

fn review_line(event: &RunEvent, review: &ReviewEvent) -> Option<String> {
    let who = who(event);
    Some(match review {
        ReviewEvent::Reviewed {
            reviewer,
            round,
            review,
            ..
        } => format!(
            "{who}: {reviewer} reviewed (round {round}): {}",
            name(&review.verdict)
        ),
        ReviewEvent::Settled { judge, ruling } => {
            format!("{who}: {judge} ruled {}", name(&ruling.verdict))
        }
        ReviewEvent::TestsWritten {
            reviewer, for_task, ..
        } => format!("{reviewer} wrote acceptance tests for {for_task}"),
    })
}

fn merge_line(merge: &MergeEvent) -> Option<String> {
    Some(match merge {
        MergeEvent::Merged { task, .. } => {
            format!("{task} merged into the run branch")
        }
        MergeEvent::Conflict {
            task,
            files,
            assigned_to,
        } => format!(
            "{task} conflicts in {} files, assigned to {assigned_to}",
            files.len()
        ),
        MergeEvent::MainMoved { commits, .. } => {
            format!("main moved {commits} commits since the run started")
        }
        _ => return None,
    })
}

fn approval_line(approval: &ApprovalEvent) -> Option<String> {
    Some(match approval {
        ApprovalEvent::Opened(opened) => {
            let waits = if opened.needs_you {
                ", waiting for you"
            } else {
                ""
            };
            format!(
                "{} approval {} opened{waits}",
                name(&opened.kind),
                opened.id
            )
        }
        ApprovalEvent::Checked(check) => match (&check.verdict, check.violations.len()) {
            (_, n) if n > 0 => format!("{}: {n} hard limits failed, so it asks you", check.id),
            (Some(verdict), _) => format!("{}: auto would {}", check.id, name(verdict)),
            (None, _) => return None,
        },
        ApprovalEvent::Decided(decided) => format!(
            "{}: {} by {}",
            decided.id,
            name(&decided.action),
            party(&decided.by)
        ),
        ApprovalEvent::Withdrawn { id, reason } => {
            format!("{id} withdrawn: {}", short(reason, SHORT_MAX))
        }
        ApprovalEvent::SpotCheckQueued { id } => format!("{id} joined your spot checks"),
    })
}

fn fallback_line(fallback: &FallbackEvent) -> Option<String> {
    Some(match fallback {
        FallbackEvent::Needed(plan) => format!(
            "{}: {} stopped ({}), so it needs a fallback",
            plan.task,
            plan.stopped,
            short(&plan.reason, SHORT_MAX)
        ),
        FallbackEvent::FellBack { task, to, by, .. } => {
            format!("{task} fell back to {to} by {}", party(by))
        }
        FallbackEvent::Waiting { task, agent_id, .. } => format!("{task} waits for {agent_id}"),
        FallbackEvent::JobWaiting(wait) => format!(
            "{}: its {} waits for {}",
            short(&wait.title, SHORT_MAX),
            wait.job.role.as_str(),
            wait.stopped.agent_id()
        ),
        FallbackEvent::JobFreed { job, to, by } => {
            let role = job.role.as_str();
            match to {
                Some(to) => format!("a waiting {role} job goes to {to} by {}", party(by)),
                None => format!("a waiting {role} job may leave its CLI by {}", party(by)),
            }
        }
        FallbackEvent::Note { .. } => return None,
    })
}

/// Outages and plan usage trouble. Readings that say all is well are routine.
fn agent_status_line(agent_id: &AgentStatusEvent) -> Option<String> {
    Some(match agent_id {
        AgentStatusEvent::PlanUsage(reading) => {
            let used = reading
                .used
                .map(|used| format!(", {:.0}% used", used * 100.0))
                .unwrap_or_default();
            match reading.status {
                PlanUsageStatus::Warning => {
                    format!("{} is low on plan usage{used}", reading.agent_id)
                }
                PlanUsageStatus::Rejected => {
                    format!("{} is out of plan usage{used}", reading.agent_id)
                }
                PlanUsageStatus::Allowed | PlanUsageStatus::Unknown => return None,
            }
        }
        AgentStatusEvent::Health(health) => {
            let reason = health
                .reason
                .as_deref()
                .map(|reason| format!(": {}", short(reason, SHORT_MAX)))
                .unwrap_or_default();
            match health.state {
                HealthState::Down => format!("{} is down{reason}", health.agent_id),
                HealthState::Degraded => format!("{} is degraded{reason}", health.agent_id),
                HealthState::Testing => format!("{} gets one test task", health.agent_id),
                HealthState::Healthy => format!("{} is healthy again", health.agent_id),
            }
        }
        AgentStatusEvent::Disabled { agent_id, reason } => format!(
            "{agent_id} is disabled for new tasks: {}",
            short(reason, SHORT_MAX)
        ),
        AgentStatusEvent::Model(health) => {
            let reason = health
                .reason
                .as_deref()
                .map(|reason| format!(": {}", short(reason, SHORT_MAX)))
                .unwrap_or_default();
            match health.state {
                ModelState::Out => format!("{} is out{reason}", health.model),
                ModelState::Degraded => format!("{} is degraded{reason}", health.model),
                ModelState::Testing => format!("{} gets one test task", health.model),
                ModelState::Healthy => format!("{} is healthy again", health.model),
            }
        }
    })
}

fn limit_line(limit: &SpendEvent) -> Option<String> {
    Some(match limit {
        SpendEvent::Run(run) => format!(
            "spent so far: ${:.2}, {} tokens",
            run.spent.cost_usd,
            run.spent.total_tokens()
        ),
        SpendEvent::Hit {
            task, dimension, ..
        } => format!("{task} hit its {} limit", name(dimension)),
        SpendEvent::Extended { task, by, .. } => {
            format!("{task}'s limit went up, by {}", party(by))
        }
        SpendEvent::Deferred { tasks } => {
            format!("{} tasks wait for the spend limit", tasks.len())
        }
    })
}

/// The task, else the session, else the source.
fn who(event: &RunEvent) -> String {
    match (&event.task_id, &event.session) {
        (Some(task), _) => task.to_string(),
        (None, Some(session)) => session.to_string(),
        (None, None) => short(&event.source, SHORT_MAX),
    }
}

fn party(party: &Party) -> String {
    match party {
        Party::User | Party::Declined => "you".to_owned(),
        Party::Rules => "your rules".to_owned(),
        Party::Judge { model } => format!("the judge {model}"),
        Party::Config => "a setting".to_owned(),
        Party::Autopilot => "autopilot".to_owned(),
        Party::ArgonautsCode => "Argonauts Code".to_owned(),
        Party::Network => "the network".to_owned(),
        Party::Agent { worker, .. } => worker.to_string(),
        // The name is the client's own, so code says it's an outside agent: a client calling
        // itself "you" must not read as you.
        Party::OutsideAgent { client } => {
            format!("the outside agent \"{}\"", short(client, SHORT_MAX))
        }
    }
}

/// The serde name of a plain enum value, in words: `in_review` reads `in review`.
fn name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name.replace('_', " "),
        _ => "something".to_owned(),
    }
}

/// Untrusted text as one clean line: control sequences stripped, secrets redacted, and cut to
/// `max` characters.
fn short(text: &str, max: usize) -> String {
    let clean = clean_text(text);
    let line = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::CheckOutcome;
    use argonauts_core::events::plan::PlanTally;
    use argonauts_core::events::run::{PhaseChange, RunStarted};
    use argonauts_core::events::session::{EndReason, SessionEvent};
    use argonauts_core::events::task::CheckKind;
    use argonauts_core::limits::{AgentHealth, PlanUsageReading, PlanUsageSource};
    use argonauts_core::names::{AgentId, DecisionId, DraftId};
    use argonauts_core::settings::fallback::FallbackMode;
    use argonauts_core::settings::protocol::JudgingMethod;
    use argonauts_core::{RunId, TaskId};
    use serde_json::json;
    use std::path::PathBuf;
    use time::OffsetDateTime;

    fn event(kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: Some(TaskId::numbered(1)),
            session: None,
            source: "claude-code".to_owned(),
            kind,
        }
    }

    fn agent(agent: AgentEvent) -> RunEvent {
        event(RunEventKind::Agent(agent))
    }

    #[test]
    fn a_stall_gets_no_line() {
        let stall = AgentEvent::Error {
            category: ErrorCategory::Stalled,
            message: "Claude Code has sent nothing for 45 seconds, not even its heartbeat".into(),
            retryable: true,
        };
        assert_eq!(agent_line(&agent(stall.clone()), &stall), None);
        let failed = AgentEvent::Error {
            category: ErrorCategory::Server,
            message: "the server failed".into(),
            retryable: true,
        };
        assert!(agent_line(&agent(failed.clone()), &failed).is_some());
    }

    #[test]
    fn narrator_digest_has_no_transcripts() {
        let key = format!("sk-ant-{}", "x".repeat(30));
        let long = format!("{} END-OF-LONG-MESSAGE", "word ".repeat(300));
        let events = vec![
            agent(AgentEvent::Reasoning {
                text: "PRIVATE-REASONING".to_owned(),
            }),
            agent(AgentEvent::TextDelta {
                text: "STREAMED-TEXT".to_owned(),
                parent: None,
            }),
            agent(AgentEvent::Message {
                text: "SUBAGENT-MESSAGE".to_owned(),
                parent: Some("tool-1".to_owned()),
            }),
            agent(AgentEvent::ToolStarted {
                id: "t1".to_owned(),
                name: "Read".to_owned(),
                input: json!({"file_path": "INPUT-PATH.rs"}),
                parent: None,
            }),
            agent(AgentEvent::ToolFinished {
                id: "t1".to_owned(),
                ok: true,
                output: Some("FILE-CONTENTS".to_owned()),
                parent: None,
            }),
            agent(AgentEvent::Result {
                text: Some("FINAL-RESULT".to_owned()),
                structured: None,
                is_error: false,
            }),
            agent(AgentEvent::Message {
                text: long.clone(),
                parent: None,
            }),
            agent(AgentEvent::Message {
                text: format!("my key is {key}\u{1b}[31m red"),
                parent: None,
            }),
            event(RunEventKind::Session(SessionEvent::Ended(Box::new(
                argonauts_testkit::runner::session_end(
                    EndReason::Completed,
                    Some("SESSION-TRANSCRIPT"),
                    None,
                ),
            )))),
            event(RunEventKind::Check(CheckOutcome {
                name: "tests".to_owned(),
                kind: CheckKind::Command,
                passed: false,
                command: Some(vec!["cargo".to_owned(), "test".to_owned()]),
                exit_code: Some(101),
                duration_ms: 10,
                summary: "41 passed, 1 failed".to_owned(),
                output_tail: "CHECK-OUTPUT-TAIL".to_owned(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            })),
        ];
        let lines: Vec<String> = events.iter().filter_map(line).collect();
        let all = lines.join("\n");
        for hidden in [
            "PRIVATE-REASONING",
            "STREAMED-TEXT",
            "SUBAGENT-MESSAGE",
            "INPUT-PATH",
            "FILE-CONTENTS",
            "FINAL-RESULT",
            "END-OF-LONG-MESSAGE",
            "SESSION-TRANSCRIPT",
            "CHECK-OUTPUT-TAIL",
            key.as_str(),
            "\u{1b}",
        ] {
            assert!(!all.contains(hidden), "{hidden:?} leaked into:\n{all}");
        }
        assert!(all.contains("task-001 ran Read"), "{all}");
        assert!(
            all.contains("check tests failed: 41 passed, 1 failed"),
            "{all}"
        );
        assert!(all.contains("[redacted"), "{all}");
        let excerpt = lines.iter().find(|l| l.contains("word word")).unwrap();
        assert!(excerpt.chars().count() <= EXCERPT_MAX + 40, "{excerpt}");
        assert!(lines.iter().all(|l| !l.contains('\n')));
    }

    #[test]
    fn plans_decisions_and_their_authors_are_lines() {
        let sealed = event(RunEventKind::Plan(PlanEvent::Sealed {
            draft: DraftId::numbered(1),
            label: 2,
            worker: "codex/gpt-5.6-luna".parse().unwrap(),
            round: 1,
            summary: "PLAN-TEXT".to_owned(),
            path: PathBuf::from("drafts/dr-001.md"),
            sha256: String::new(),
        }));
        assert_eq!(
            line(&sealed).as_deref(),
            Some("plan 2 sealed, drafted by codex/gpt-5.6-luna")
        );
        let tally = |label, author: &str| PlanTally {
            label,
            author: author.parse().unwrap(),
            mean: 0.0,
            spread: 0.0,
            counted: 2,
            dropped: 0,
        };
        let judged = JudgingResult {
            method: JudgingMethod::Cross,
            plans: vec![
                tally(1, "claude-code/haiku"),
                tally(2, "codex/gpt-5.6-luna"),
            ],
            winner: Some(2),
            margin: 0.4,
            near_tie: false,
        };
        let picked = event(RunEventKind::Plan(PlanEvent::Judged(judged.clone())));
        assert_eq!(
            line(&picked).as_deref(),
            Some("the judges picked plan 2 by codex/gpt-5.6-luna, margin 0.40")
        );
        let tie = event(RunEventKind::Plan(PlanEvent::Judged(JudgingResult {
            near_tie: true,
            margin: 0.02,
            ..judged
        })));
        assert!(line(&tie).unwrap().contains("near tie"));

        let decided = event(RunEventKind::Plan(PlanEvent::Decided(DesignDecision {
            id: DecisionId::numbered(3),
            title: "Cache store".to_owned(),
            choice: format!("Redis {}", "because ".repeat(40)),
            rationale: "RATIONALE".to_owned(),
            alternatives: vec!["memcached".to_owned()],
            path: None,
        })));
        let record = decision(&decided).unwrap();
        assert!(record.starts_with("D-003: Cache store: Redis"), "{record}");
        assert!(record.chars().count() < 200, "{record}");
        assert!(line(&decided).unwrap().starts_with("decided D-003"));
        assert!(!record.contains("RATIONALE") && !line(&sealed).unwrap().contains("PLAN-TEXT"));
    }

    #[test]
    fn agent_trouble_and_fallbacks_are_lines_and_good_news_is_routine() {
        let agent_id = AgentId("codex".to_owned());
        let health = |state| {
            event(RunEventKind::AgentStatus(AgentStatusEvent::Health(
                AgentHealth {
                    agent_id: agent_id.clone(),
                    state,
                    since: OffsetDateTime::UNIX_EPOCH,
                    reason: Some("3 errors in a row".to_owned()),
                    next_test_at: None,
                    disabled: None,
                },
            )))
        };
        assert_eq!(
            line(&health(HealthState::Down)).as_deref(),
            Some("codex is down: 3 errors in a row")
        );
        let plan_usage = |status| {
            event(RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(
                PlanUsageReading {
                    agent_id: agent_id.clone(),
                    status,
                    used: Some(0.92),
                    resets_at: None,
                    window: None,
                    source: PlanUsageSource::RateLimitEvent,
                    observed_at: OffsetDateTime::UNIX_EPOCH,
                    detail: None,
                    wrap_up: false,
                },
            )))
        };
        assert_eq!(
            line(&plan_usage(PlanUsageStatus::Warning)).as_deref(),
            Some("codex is low on plan usage, 92% used")
        );
        assert_eq!(line(&plan_usage(PlanUsageStatus::Allowed)), None);
        let handed = event(RunEventKind::Fallback(FallbackEvent::FellBack {
            task: TaskId::numbered(2),
            to: "claude-code/haiku".parse().unwrap(),
            mode: FallbackMode::Auto,
            by: Party::Rules,
            context: None,
        }));
        assert_eq!(
            line(&handed).as_deref(),
            Some("task-002 fell back to claude-code/haiku by your rules")
        );
    }

    #[test]
    fn a_captain_run_has_its_request_as_the_goal() {
        let started: RunStarted = serde_json::from_value(json!({
            "request": format!("Fix the flaky login test {}", "please ".repeat(100)),
            "title": "Fix login test",
            "mode": "captain",
            "forced": false,
            "workflow": "default",
            "repo_root": "/repo",
            "base_commit": "abc",
            "uncommitted": [],
            "uncommitted_included": false,
            "settings_sha256": "",
            "settings": {},
            "overrides": [],
            "cli_versions": {},
            "version": "0.0.1",
        }))
        .unwrap();
        let started = event(RunEventKind::Run(RunLifecycle::Started(Box::new(started))));
        let goal = goal(&started).unwrap();
        assert!(goal.starts_with("Fix the flaky login test"), "{goal}");
        assert!(goal.chars().count() <= EXCERPT_MAX * 2 + 1, "{goal}");
    }

    #[test]
    fn a_outside_agent_never_reads_as_you() {
        for client in ["you", "your rules", "OpenClaw"] {
            let paused = event(RunEventKind::Run(RunLifecycle::PauseRequested {
                by: Party::OutsideAgent {
                    client: client.to_owned(),
                },
                now: false,
            }));
            assert_eq!(
                line(&paused),
                Some(format!("the outside agent \"{client}\" asked to pause"))
            );
        }
    }

    #[test]
    fn phase_changes_are_lines_and_ends_are_noticed() {
        let ended = event(RunEventKind::Phase(PhaseChange {
            phase: RunPhase::FinalCheck,
            status: PhaseStatus::Finished,
            summary: Some("the full suite passed".to_owned()),
        }));
        assert_eq!(
            line(&ended).as_deref(),
            Some("final check phase finished: the full suite passed")
        );
        assert_eq!(phase_end(&ended), Some(RunPhase::FinalCheck));
        let started = event(RunEventKind::Phase(PhaseChange {
            phase: RunPhase::Build,
            status: PhaseStatus::Started,
            summary: None,
        }));
        assert_eq!(phase_end(&started), None);
    }
}
