//! What a run event says: the run's lifecycle and phases, cards, agents and their tool calls,
//! gates, checkpoints as numbered lists, and notes. Events without wording of their own are
//! read from their names at full verbosity, so nothing is dropped.

use argonauts_core::events::card::CardEvent;
use argonauts_core::events::checkpoint::{CheckpointEvent, PolicyCheck};
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::vendor::{BudgetEvent, VendorEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::{AgentEvent, NoteLevel, Verbosity};

use crate::cards::with_run_owners;
use crate::state::AppState;
use crate::text::sanitize_line;

use super::agent::{StartedCall, agent_lines};
use super::cards::CardList;
use super::lines::party;
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
        RunEventKind::Card(card) => card_lines(event, card, normal, full),
        RunEventKind::Session(session) => session_lines(event, session, state, normal, full),
        RunEventKind::Agent(agent) => agent_event_lines(event, agent, state),
        RunEventKind::Gate(gate) => {
            if gate.passed && !normal {
                return Vec::new();
            }
            let (label, outcome) = if gate.passed {
                ("gate", "passed")
            } else {
                ("gate error", "failed")
            };
            let mut line = format!("{label}: {} {outcome}", one_line(&gate.name));
            if !gate.summary.trim().is_empty() {
                line.push_str(&format!(", {}", one_line(&gate.summary)));
            }
            vec![line]
        }
        RunEventKind::Checkpoint(checkpoint) => checkpoint_lines(event, checkpoint, state, full),
        RunEventKind::Note(note) => {
            let label = match note.level {
                NoteLevel::Info if !normal => return Vec::new(),
                NoteLevel::Info => "note",
                NoteLevel::Warn => "warning",
                NoteLevel::Error => "error",
            };
            // The plain line; its detail is for Ctrl+O on screen.
            labeled(label, note.line())
        }
        RunEventKind::Vendor(VendorEvent::Quota(_)) | RunEventKind::Budget(BudgetEvent::Run(_))
            if !full =>
        {
            Vec::new()
        }
        RunEventKind::Blackboard(_) | RunEventKind::Retro(_) if !full => Vec::new(),
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
        RunLifecycle::Parked { reason, .. } => format!("run: parked, {}", word(reason)),
        RunLifecycle::ApprovalModeChanged { mode, .. } => {
            format!("run: approvals now {}", word(mode))
        }
        RunLifecycle::Snapshot(_) => return Vec::new(),
    };
    vec![line]
}

fn card_lines(event: &RunEvent, card: &CardEvent, normal: bool, full: bool) -> Vec<String> {
    let id = event
        .card_id
        .as_ref()
        .map_or_else(|| "card".to_owned(), ToString::to_string);
    let line = match card {
        CardEvent::Created(spec) if normal => {
            format!("{}: created, {}", spec.id, one_line(&spec.title))
        }
        CardEvent::State { to, reason, .. } if normal => {
            let mut line = format!("{id}: {}", word(to));
            if !reason.trim().is_empty() {
                line.push_str(&format!(", {}", one_line(reason)));
            }
            line
        }
        CardEvent::Reassigned { to, .. } if normal => format!("{id}: reassigned to {to}"),
        CardEvent::TakenOver { .. } if normal => format!("{id}: taken over by you"),
        CardEvent::TakeOverEnded { .. } if normal => format!("{id}: back from your take-over"),
        CardEvent::Reverted { .. } if normal => format!("{id}: reverted"),
        other if full => event_words(&RunEventKind::Card(other.clone())),
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
    let vendor = session.map_or(event.source.as_str(), |session| session.vendor.as_str());
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
    agent_lines(&who, vendor, agent, state, call, said)
}

fn checkpoint_lines(
    event: &RunEvent,
    checkpoint: &CheckpointEvent,
    state: &AppState,
    full: bool,
) -> Vec<String> {
    match checkpoint {
        CheckpointEvent::Opened(opened) if opened.needs_you => {
            let run = state.runs.get(&event.run_id);
            let offset = state.clock_offset();
            let opened = with_run_owners(opened, run, &state.names);
            CardList::checkpoint(&event.run_id, &opened, run, offset, &state.names).lines()
        }
        CheckpointEvent::Opened(opened) => vec![format!(
            "card: {}, the approval rules are deciding",
            one_line(&opened.title)
        )],
        CheckpointEvent::Decided(decided) => {
            let mut line = format!(
                "card: {} settled, {} by {}",
                decided.id,
                word(&decided.action),
                party(&decided.by, &state.names)
            );
            if let Some(text) = decided.text.as_deref() {
                line.push_str(&format!(", {}", one_line(text)));
            }
            vec![line]
        }
        CheckpointEvent::Withdrawn { id, reason } => {
            vec![format!("card: {id} withdrawn, {}", one_line(reason))]
        }
        CheckpointEvent::SpotCheckQueued { id } => {
            vec![format!(
                "card: {id} decided on its own, queued for a spot check"
            )]
        }
        CheckpointEvent::Checked(check)
            if full || !check.violations.is_empty() || auto_left_it(check) =>
        {
            let mut line = format!("policy: {} checked", check.id);
            // Full auto keeps what the policy found on a card it already settled in your place.
            let open = state.runs.get(&event.run_id).is_none_or(|run| {
                run.checkpoints
                    .iter()
                    .any(|open| open.opened.id == check.id)
            });
            if open && (!check.violations.is_empty() || auto_left_it(check)) {
                line.push_str(", it waits for you");
            }
            if let Some(reason) = check.reasons.first() {
                line.push_str(&format!(", {}", one_line(reason)));
            }
            vec![line]
        }
        CheckpointEvent::Checked(_) => Vec::new(),
    }
}

/// Auto couldn't settle a card opened in auto, and the check says why.
fn auto_left_it(check: &PolicyCheck) -> bool {
    !check.shadow && check.verdict.is_none() && !check.reasons.is_empty()
}

fn one_line(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::AppEvent;
    use argonauts_core::events::checkpoint::{
        CheckpointEvent, CheckpointKind, CheckpointOpened, CheckpointPacket, FreeText, PolicyCheck,
        QuestionPacket,
    };
    use argonauts_core::events::{RunEvent, RunEventKind};
    use argonauts_core::names::{CheckpointId, WorkerRef};
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
    use argonauts_core::{CardId, RunId};
    use time::OffsetDateTime;

    use super::run_lines;
    use crate::state::AppState;

    fn event(seq: u64, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-20261004-043029-d8bb").unwrap(),
            card_id: None,
            session: None,
            source: "engine".into(),
            kind,
        }
    }

    /// What the policy found on a card full auto already settled is kept for review, and the
    /// line for it never says the card waits for you.
    #[test]
    fn a_check_kept_after_full_auto_settled_the_card_doesnt_say_it_waits() {
        let id = CheckpointId::numbered(1);
        let card = CardId::new("card-001").unwrap();
        let opened = CheckpointOpened {
            id: id.clone(),
            kind: CheckpointKind::Question,
            title: "Question from card-001".into(),
            card: Some(card.clone()),
            packet: CheckpointPacket::Question(QuestionPacket {
                from: WorkerRef::new("codex", None),
                card,
                text: "Which level?".into(),
                options: Vec::new(),
            }),
            evidence_sha256: String::new(),
            mode: ApprovalMode::FullAuto,
            decide_with: DecideWith::Model,
            options: Vec::new(),
            free_text: FreeText::Answer,
            needs_you: false,
        };
        let check = event(
            2,
            RunEventKind::Checkpoint(CheckpointEvent::Checked(PolicyCheck {
                id: id.clone(),
                violations: Vec::new(),
                verdict: None,
                by: None,
                approver: None,
                reasons: vec!["the approver escalated".into()],
                shadow: false,
            })),
        );
        let mut state = AppState::default();
        let opened = event(
            1,
            RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened))),
        );
        state.apply(&AppEvent::Run {
            event: Box::new(opened),
        });
        let waiting = run_lines(&check, &state).join("\n");
        assert!(waiting.contains("it waits for you"), "{waiting}");

        state.apply(&AppEvent::Run {
            event: Box::new(event(
                2,
                RunEventKind::Checkpoint(CheckpointEvent::Withdrawn {
                    id,
                    reason: "settled".into(),
                }),
            )),
        });
        let settled = run_lines(&check, &state).join("\n");
        assert!(!settled.contains("it waits for you"), "{settled}");
        assert!(settled.contains("the approver escalated"), "{settled}");
    }
}
