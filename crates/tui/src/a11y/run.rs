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
            labeled(label, &note.text)
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
        _ if !normal => return Vec::new(),
        RunLifecycle::Started(started) => format!(
            "run: {}, a {} run, started",
            one_line(&started.title),
            word(&started.mode)
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
            if !check.violations.is_empty() || auto_left_it(check) {
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
