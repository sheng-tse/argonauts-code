//! What a run event says: the run's lifecycle and phases, tasks, agents and their tool calls,
//! checks, approvals as numbered lists, and notes. Events without wording of their own are
//! read from their names at full verbosity, so nothing is dropped.

use argonauts_core::events::agent_status::{AgentStatusEvent, SpendEvent};
use argonauts_core::events::approval::{ApprovalEvent, PolicyCheck};
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::shared::BlackboardEvent;
use argonauts_core::events::task::TaskEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::{AgentEvent, NoteLevel, Verbosity};

use crate::questions::with_run_workers;
use crate::state::AppState;
use crate::text::sanitize_line;

use super::agent::{StartedCall, agent_lines};
use super::lines::party;
use super::questions::QuestionList;
use super::words::{event_words, labeled, shows, speaker, word};

pub fn run_lines(event: &RunEvent, state: &AppState) -> Vec<String> {
    let normal = shows(state, Verbosity::Normal);
    let full = shows(state, Verbosity::Full);
    match &event.kind {
        RunEventKind::Run(lifecycle) => lifecycle_lines(lifecycle, normal),
        RunEventKind::Phase(change) if normal => {
            let mut line = format!("run: {} {}", word(&change.phase), word(&change.status));
            if let Some(summary) = change.summary.as_deref() {
                line.push_str(&format!(", {}", one_line(summary)));
            }
            vec![line]
        }
        RunEventKind::Task(task) => task_lines(event, task, normal, full),
        RunEventKind::Session(session) => session_lines(event, session, state, normal, full),
        RunEventKind::Agent(agent) => agent_event_lines(event, agent, state),
        RunEventKind::Check(check) => {
            if check.passed && !normal {
                return Vec::new();
            }
            let (label, outcome) = if check.passed {
                ("check", "passed")
            } else {
                ("check error", "failed")
            };
            let mut line = format!("{label}: {} {outcome}", one_line(&check.name));
            if !check.summary.trim().is_empty() {
                line.push_str(&format!(", {}", one_line(&check.summary)));
            }
            vec![line]
        }
        RunEventKind::Approval(approval) => approval_lines(event, approval, state, full),
        RunEventKind::Note(note) => {
            let label = match note.level {
                NoteLevel::Info | NoteLevel::Aside | NoteLevel::Quiet if !normal => {
                    return Vec::new();
                }
                NoteLevel::Info | NoteLevel::Aside | NoteLevel::Quiet => "note",
                NoteLevel::Warn | NoteLevel::Caution => "warning",
                NoteLevel::Error => "error",
            };
            // The plain line; its detail is for Ctrl+O on screen.
            labeled(label, note.line())
        }
        RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(_))
        | RunEventKind::Spend(SpendEvent::Run(_))
            if !full =>
        {
            Vec::new()
        }
        RunEventKind::Blackboard(BlackboardEvent::AnsweredForYou(answered)) if normal => labeled(
            "note",
            &format!(
                "autopilot answered {}'s question: {}",
                answered.asker, answered.answer
            ),
        ),
        RunEventKind::Blackboard(_) | RunEventKind::RunReview(_) if !full => Vec::new(),
        kind if normal => vec![event_words(kind)],
        _ => Vec::new(),
    }
}

fn lifecycle_lines(lifecycle: &RunLifecycle, normal: bool) -> Vec<String> {
    let line = match lifecycle {
        RunLifecycle::Finished { outcome, detail } => {
            let mut line = format!("run: finished, {}", word(outcome));
            if let Some(detail) = detail.as_deref() {
                line.push_str(&format!(", {}", one_line(detail)));
            }
            line
        }
        RunLifecycle::ClosingAnswer(answer) => format!("answer: {}", one_line(&answer.text)),
        _ if !normal => return Vec::new(),
        RunLifecycle::Started(started) => format!(
            "run: {}, a {} run, started",
            one_line(&started.title),
            word(&started.mode)
        ),
        RunLifecycle::Named(named) => format!(
            "run: named {}, {}",
            one_line(&named.name),
            one_line(&named.description)
        ),
        RunLifecycle::Resumed(_) => "run: resumed".to_owned(),
        RunLifecycle::PauseRequested { now: true, .. } => {
            "run: stopping the agents to pause".to_owned()
        }
        RunLifecycle::PauseRequested { now: false, .. } => {
            "run: pausing at the next safe point".to_owned()
        }
        RunLifecycle::PauseCancelled => "run: pause cancelled".to_owned(),
        RunLifecycle::Paused {
            sessions_stopped, ..
        } => format!("run: paused, {sessions_stopped} agents stopped"),
        RunLifecycle::StopRequested { .. } => "run: stopping".to_owned(),
        RunLifecycle::Waiting { reason, .. } => format!("run: waiting, {}", word(reason)),
        RunLifecycle::ApprovalModeChanged { mode, .. } => {
            format!("run: approvals now {}", word(mode))
        }
        RunLifecycle::Snapshot(_) => return Vec::new(),
    };
    vec![line]
}

fn task_lines(event: &RunEvent, task: &TaskEvent, normal: bool, full: bool) -> Vec<String> {
    let id = event
        .task_id
        .as_ref()
        .map_or_else(|| "task".to_owned(), ToString::to_string);
    let line = match task {
        TaskEvent::Created(spec) if normal => {
            format!("{}: created, {}", spec.id, one_line(&spec.title))
        }
        TaskEvent::State { to, reason, .. } if normal => {
            let mut line = format!("{id}: {}", word(to));
            if !reason.trim().is_empty() {
                line.push_str(&format!(", {}", one_line(reason)));
            }
            line
        }
        TaskEvent::Reassigned { to, .. } if normal => format!("{id}: reassigned to {to}"),
        TaskEvent::TakenOver { .. } if normal => format!("{id}: taken over by you"),
        TaskEvent::TakeOverEnded { .. } if normal => format!("{id}: back from your take-over"),
        TaskEvent::Rewound { .. } if normal => format!("{id}: rewound"),
        other if full => event_words(&RunEventKind::Task(other.clone())),
        _ => return Vec::new(),
    };
    vec![line]
}

fn session_lines(
    event: &RunEvent,
    session: &SessionEvent,
    state: &AppState,
    normal: bool,
    full: bool,
) -> Vec<String> {
    let who = speaker(state, &event.run_id, event.session.as_ref(), &event.source);
    match session {
        SessionEvent::Started(_) if normal => vec![format!("{who}: started")],
        SessionEvent::Ended(end) if normal => vec![format!("{who}: {}", word(&end.reason))],
        other if full => vec![format!(
            "{who}: {}",
            event_words(&RunEventKind::Session(other.clone()))
        )],
        _ => Vec::new(),
    }
}

fn agent_event_lines(event: &RunEvent, agent: &AgentEvent, state: &AppState) -> Vec<String> {
    let who = speaker(state, &event.run_id, event.session.as_ref(), &event.source);
    let session = event
        .session
        .as_ref()
        .and_then(|key| state.session(&event.run_id, key));
    let agent_id = session.map_or(event.source.as_str(), |session| session.agent_id.as_str());
    let call = match agent {
        AgentEvent::ToolFinished { id, .. } => session
            .and_then(|session| session.tools.iter().rev().find(|tool| tool.id == *id))
            .map(|tool| StartedCall {
                name: &tool.name,
                input: &tool.input,
            }),
        _ => None,
    };
    let said = session.and_then(|session| session.messages.last().map(String::as_str));
    agent_lines(&who, agent_id, agent, state, call, said)
}

fn approval_lines(
    event: &RunEvent,
    approval: &ApprovalEvent,
    state: &AppState,
    full: bool,
) -> Vec<String> {
    match approval {
        ApprovalEvent::Opened(opened) if opened.needs_you => {
            let run = state.runs.get(&event.run_id);
            let offset = state.clock_offset();
            let opened = with_run_workers(opened, run, &state.names);
            QuestionList::approval(&event.run_id, &opened, run, offset, &state.names).lines()
        }
        ApprovalEvent::Opened(opened) => vec![format!(
            "approval: {}, the approval rules are deciding",
            one_line(&opened.title)
        )],
        ApprovalEvent::Decided(decided) => {
            let mut line = format!(
                "approval: {} settled, {} by {}",
                decided.id,
                word(&decided.action),
                party(&decided.by, &state.names)
            );
            if let Some(text) = decided.text.as_deref() {
                line.push_str(&format!(", {}", one_line(text)));
            }
            vec![line]
        }
        ApprovalEvent::Withdrawn { id, reason } => {
            vec![format!("approval: {id} withdrawn, {}", one_line(reason))]
        }
        ApprovalEvent::SpotCheckQueued { id } => {
            vec![format!(
                "approval: {id} decided on its own, queued for a spot check"
            )]
        }
        ApprovalEvent::Checked(check)
            if full || !check.violations.is_empty() || auto_left_it(check) =>
        {
            let mut line = format!("policy: {} checked", check.id);
            // Autopilot keeps what the policy found on a question it already settled in your place.
            let open = state
                .runs
                .get(&event.run_id)
                .is_none_or(|run| run.approvals.iter().any(|open| open.opened.id == check.id));
            if open && (!check.violations.is_empty() || auto_left_it(check)) {
                line.push_str(", it waits for you");
            }
            if let Some(reason) = check.reasons.first() {
                line.push_str(&format!(", {}", one_line(reason)));
            }
            vec![line]
        }
        ApprovalEvent::Checked(_) => Vec::new(),
    }
}

/// Auto couldn't settle a question opened in auto, and the check says why.
fn auto_left_it(check: &PolicyCheck) -> bool {
    !check.auto_preview && check.verdict.is_none() && !check.reasons.is_empty()
}

fn one_line(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::AppEvent;
    use argonauts_core::events::approval::{
        ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText, PolicyCheck,
        QuestionPacket,
    };
    use argonauts_core::events::{RunEvent, RunEventKind};
    use argonauts_core::names::{ApprovalId, ModelRef};
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
    use argonauts_core::settings::asking::WhenDeclined;
    use argonauts_core::{RunId, TaskId};
    use time::OffsetDateTime;

    use super::run_lines;
    use crate::state::AppState;

    fn event(seq: u64, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-20261004-043029-d8bb").unwrap(),
            task_id: None,
            session: None,
            source: "engine".into(),
            kind,
        }
    }

    /// What the policy found on a question autopilot already settled is kept for review, and the
    /// line for it never says the question waits for you.
    #[test]
    fn a_check_kept_after_autopilot_settled_the_question_doesnt_say_it_waits() {
        let id = ApprovalId::numbered(1);
        let task = TaskId::new("task-001").unwrap();
        let opened = ApprovalOpened {
            id: id.clone(),
            kind: ApprovalKind::Question,
            title: "Question from task-001".into(),
            task: Some(task.clone()),
            packet: ApprovalPacket::Question(QuestionPacket {
                from: ModelRef::new("codex", None),
                task,
                text: "Which level?".into(),
                options: Vec::new(),
            }),
            evidence_sha256: String::new(),
            mode: ApprovalMode::Autopilot,
            decide_with: DecideWith::Model,
            when_declined: WhenDeclined::Decide,
            options: Vec::new(),
            free_text: FreeText::Answer,
            needs_you: false,
        };
        let check = event(
            2,
            RunEventKind::Approval(ApprovalEvent::Checked(PolicyCheck {
                id: id.clone(),
                violations: Vec::new(),
                verdict: None,
                by: None,
                judge: None,
                reasons: vec!["the judge needs you".into()],
                auto_preview: false,
            })),
        );
        let mut state = AppState::default();
        let opened = event(
            1,
            RunEventKind::Approval(ApprovalEvent::Opened(Box::new(opened))),
        );
        state.apply(&AppEvent::Run {
            event: Box::new(opened),
        });
        let waiting = run_lines(&check, &state).join("\n");
        assert!(waiting.contains("it waits for you"), "{waiting}");

        state.apply(&AppEvent::Run {
            event: Box::new(event(
                2,
                RunEventKind::Approval(ApprovalEvent::Withdrawn {
                    id,
                    reason: "settled".into(),
                }),
            )),
        });
        let settled = run_lines(&check, &state).join("\n");
        assert!(!settled.contains("it waits for you"), "{settled}");
        assert!(settled.contains("the judge needs you"), "{settled}");
    }
}
