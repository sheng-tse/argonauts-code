//! A run's story in scrollback: triage, goal and plan steps, gates, handoffs, pauses, resumes,
//! merges, and the run's end, each a tool-call-shaped line with `⎿` details. Agents and cards
//! draw their own items.

use std::collections::BTreeMap;

use argonauts_core::events::card::{
    AttemptCause, CardEvent, CardState, GateOutcome, IntegrationEvent,
};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DivideEvent, JudgingResult, MappedTopic, PlanEvent};
use argonauts_core::events::run::{RunLifecycle, RunMode, RunPhase};
use argonauts_core::events::vendor::{HandoffEvent, HandoffPlan, VendorEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::HealthState;
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::{CardId, PhaseStatus, RunId};
use time::OffsetDateTime;

use crate::format::WallClock;

use super::items::Items;
use super::{Cx, gates, handoff, lifecycle, steps};

#[derive(Debug, Default)]
pub(super) struct Milestones {
    runs: BTreeMap<RunId, RunStory>,
    /// Each vendor's last health, to tell when one comes back.
    health: BTreeMap<VendorId, HealthState>,
}

/// What a run's later lines need from its earlier events.
#[derive(Debug, Default)]
pub(super) struct RunStory {
    pub started: Option<OffsetDateTime>,
    /// A crew run names the card on its gate lines.
    pub crew: bool,
    /// The last pause asked to stop now rather than at a safe point.
    pub pause_now: bool,
    pub restatements: u32,
    pub drafts: u32,
    pub mapped: Vec<MappedTopic>,
    pub judged: Option<JudgingResult>,
    pub fast_path: Option<WorkerRef>,
    pub coordinated: Option<WorkerRef>,
    /// Gate results per card, printed together when the card leaves gating. `None` is the final
    /// check's.
    pub gates: BTreeMap<Option<CardId>, Vec<GateOutcome>>,
    pub landed: Option<String>,
    pub pull_request: Option<PullRequest>,
    pub retro: bool,
    /// Each card's latest checkpoint tag and why it was handed off.
    pub tags: BTreeMap<CardId, u32>,
    pub handoffs: BTreeMap<CardId, HandoffPlan>,
}

#[derive(Debug, Clone)]
pub(super) struct PullRequest {
    pub url: Option<String>,
    pub command: Option<Vec<String>>,
}

impl Milestones {
    pub(super) fn on_run_event(
        &mut self,
        event: &RunEvent,
        items: &mut Items,
        clock: &WallClock,
        cx: &mut Cx<'_>,
    ) {
        let story = self.runs.entry(event.run_id.clone()).or_default();
        let card = event.card_id.as_ref();
        match &event.kind {
            RunEventKind::Run(lifecycle) => match lifecycle {
                RunLifecycle::Started(started) => {
                    story.started = Some(event.ts);
                    story.crew = started.mode == RunMode::Crew;
                    steps::triage(started, cx);
                }
                RunLifecycle::Resumed(resumed) => lifecycle::resumed(&event.run_id, resumed, cx),
                RunLifecycle::PauseRequested { now, .. } => story.pause_now = *now,
                RunLifecycle::Paused {
                    sessions_stopped,
                    committed,
                } => lifecycle::paused(story.pause_now, *sessions_stopped, committed, cx),
                RunLifecycle::Parked { reason, .. } => lifecycle::parked(reason, clock, cx),
                RunLifecycle::Finished { outcome, detail } => {
                    lifecycle::run_end(event, story, *outcome, detail.as_deref(), clock, cx);
                    self.runs.remove(&event.run_id);
                }
                _ => {}
            },
            RunEventKind::Phase(change) => match (change.phase, change.status) {
                (RunPhase::Retro, PhaseStatus::Finished) => story.retro = true,
                // The engine's warning says a failed retro once, with why and that the run's
                // result stands, and the finish line mustn't call it saved.
                (RunPhase::Retro, PhaseStatus::Failed) => story.retro = false,
                (phase, status) => steps::phase(phase, status, change.summary.as_deref(), cx),
            },
            RunEventKind::Goal(goal) => match goal {
                GoalEvent::Restated { .. } => story.restatements += 1,
                GoalEvent::Approved { file, by } => steps::goal(story, file, by, cx),
                GoalEvent::ChangeResolved { resolution, .. } => steps::goal_change(*resolution, cx),
                _ => {}
            },
            RunEventKind::Plan(plan) => match plan {
                PlanEvent::Sealed { round: 1, .. } => story.drafts += 1,
                PlanEvent::Mapped { topics, .. } => story.mapped = topics.clone(),
                PlanEvent::FastPath { merged_by } => story.fast_path = Some(merged_by.clone()),
                PlanEvent::Judged(result) => story.judged = Some(result.clone()),
                PlanEvent::Coordinated {
                    coordinator,
                    rejection: None,
                } => story.coordinated = Some(coordinator.clone()),
                _ => {}
            },
            RunEventKind::Divide(DivideEvent::Graph(graph)) => {
                steps::plan(story, graph.cards.len(), cx)
            }
            RunEventKind::Gate(outcome) => {
                story
                    .gates
                    .entry(card.cloned())
                    .or_default()
                    .push(outcome.clone());
            }
            RunEventKind::Card(card_event) => {
                let Some(card) = card else { return };
                match card_event {
                    CardEvent::State {
                        from: Some(CardState::Gating),
                        to,
                        ..
                    } if *to != CardState::Gating => {
                        if let Some(gates) = story.gates.remove(&Some(card.clone())) {
                            gates::report(Some(card), &gates, story.crew, cx);
                        }
                    }
                    CardEvent::CheckpointTagged { n, .. } => {
                        story.tags.insert(card.clone(), *n);
                    }
                    CardEvent::Reassigned { from, to, by, .. } => {
                        handoff::reassigned(card, from.as_ref(), to, by, cx)
                    }
                    CardEvent::Reverted { to, kept_on } => {
                        handoff::reverted(card, *to, kept_on, cx)
                    }
                    CardEvent::DebtCleared { by } => handoff::debt_cleared(card, by, cx),
                    CardEvent::Attempt {
                        worker,
                        cause: AttemptCause::Rotation,
                        ..
                    } => handoff::rotated(card, worker, cx),
                    _ => {}
                }
            }
            RunEventKind::Integration(integration) => match integration {
                IntegrationEvent::Landed { target, .. } => story.landed = Some(target.clone()),
                IntegrationEvent::PullRequest { url, command, .. } => {
                    story.pull_request = Some(PullRequest {
                        url: url.clone(),
                        command: command.clone(),
                    });
                }
                IntegrationEvent::FinalCheck { .. } => {
                    let gates = story.gates.remove(&None).unwrap_or_default();
                    gates::report(None, &gates, false, cx);
                }
                IntegrationEvent::Conflict {
                    card,
                    files,
                    assigned_to,
                } => handoff::conflict(card, files.len(), assigned_to, cx),
                IntegrationEvent::MainMoved { commits, .. } => lifecycle::main_moved(*commits, cx),
                _ => {}
            },
            RunEventKind::Handoff(handoff_event) => match handoff_event {
                HandoffEvent::Needed(plan) => {
                    story.handoffs.insert(plan.card.clone(), plan.clone());
                }
                HandoffEvent::HandedOff {
                    card,
                    to,
                    mode,
                    context,
                    ..
                } => handoff::handed_off(
                    handoff::Handed {
                        card,
                        to,
                        mode: *mode,
                        context: *context,
                        tag: story.tags.get(card).copied(),
                    },
                    story.handoffs.get(card),
                    clock,
                    cx,
                ),
                HandoffEvent::Waiting {
                    card,
                    vendor,
                    until,
                } => handoff::waiting(card, vendor, *until, clock, cx),
                HandoffEvent::Packet { .. } => {}
            },
            RunEventKind::Vendor(vendor) => match vendor {
                VendorEvent::Health(health) => {
                    let before = self.health.insert(health.vendor.clone(), health.state);
                    let was_out = matches!(before, Some(HealthState::Down | HealthState::Degraded));
                    if was_out && health.state == HealthState::Healthy {
                        handoff::vendor_back(&event.run_id, &health.vendor, cx);
                    }
                }
                VendorEvent::Disabled { vendor, reason } => items.notice(
                    argonauts_core::NoteLevel::Warn,
                    &format!("{vendor} is off for new cards: {reason}"),
                    cx,
                ),
                VendorEvent::Quota(_) => {}
            },
            RunEventKind::Retro(_) => story.retro = true,
            RunEventKind::Note(note) => items.notice(note.level, &note.text, cx),
            _ => {}
        }
    }
}
