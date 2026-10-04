//! A run's story in scrollback: triage, goal and plan steps, gates, handoffs, pauses, resumes,
//! merges, and the run's end, each a tool-call-shaped line with `⎿` details. Agents and cards
//! draw their own items.

use std::collections::BTreeMap;

use argonauts_core::events::card::{
    AttemptCause, CardEvent, CardState, GateOutcome, IntegrationEvent,
};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DivideEvent, JudgingResult, MappedTopic, PlanEvent};
use argonauts_core::events::run::{ClosingAnswer, RunLifecycle, RunMode, RunPhase};
use argonauts_core::events::vendor::{HandoffEvent, HandoffPlan, VendorEvent};
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::HealthState;
use argonauts_core::log::RETRO_FAILED;
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::{CardId, PhaseStatus, RunId};
use time::OffsetDateTime;

use crate::format::WallClock;

use super::items::Items;
use super::{Cx, gates, handoff, lifecycle, steps, workflows};

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
    pub retro_failed: bool,
    /// Each card's latest checkpoint tag and why it was handed off.
    pub tags: BTreeMap<CardId, u32>,
    pub handoffs: BTreeMap<CardId, HandoffPlan>,
    /// The run's own workflow, by the name its start item printed.
    pub workflow: Option<String>,
    /// Printed at the run's end, after the merge line.
    pub closing: Option<ClosingAnswer>,
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
                RunLifecycle::ClosingAnswer(answer) => story.closing = Some((**answer).clone()),
                RunLifecycle::Finished { outcome, detail } => {
                    let elapsed = story.started.map(|at| {
                        let seconds = (event.ts - at).whole_seconds().max(0);
                        std::time::Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
                    });
                    workflows::done(&event.run_id, story, elapsed, cx);
                    lifecycle::run_end(event, story, *outcome, detail.as_deref(), clock, cx);
                    self.runs.remove(&event.run_id);
                }
                _ => {}
            },
            RunEventKind::Phase(change) => match (change.phase, change.status) {
                (RunPhase::Retro, PhaseStatus::Finished) => story.retro = true,
                // The finish line mustn't call a failed retro saved.
                (RunPhase::Retro, PhaseStatus::Failed) => {
                    story.retro = false;
                    story.retro_failed = true;
                    steps::retro_failed(change.summary.as_deref(), cx);
                }
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
                            let label = card_label(cx, &event.run_id, card);
                            gates::report(Some(&label), &gates, story.crew, cx);
                        }
                    }
                    CardEvent::CheckpointTagged { n, .. } => {
                        story.tags.insert(card.clone(), *n);
                    }
                    CardEvent::Reassigned { from, to, by, .. } => {
                        let label = card_label(cx, &event.run_id, card);
                        handoff::reassigned(&label, from.as_ref(), to, by, cx)
                    }
                    CardEvent::Reverted { to, kept_on } => {
                        let label = card_label(cx, &event.run_id, card);
                        handoff::reverted(&label, *to, kept_on, cx)
                    }
                    CardEvent::DebtCleared { by } => {
                        let label = card_label(cx, &event.run_id, card);
                        handoff::debt_cleared(&label, by, cx)
                    }
                    CardEvent::Attempt {
                        worker,
                        cause: AttemptCause::Rotation,
                        ..
                    } => {
                        let label = card_label(cx, &event.run_id, card);
                        handoff::rotated(&label, worker, cx)
                    }
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
                } => {
                    let label = card_label(cx, &event.run_id, card);
                    handoff::conflict(&label, files.len(), assigned_to, cx)
                }
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
                        card: &card_label(cx, &event.run_id, card),
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
                } => {
                    let label = card_label(cx, &event.run_id, card);
                    handoff::waiting(&label, vendor, *until, clock, cx)
                }
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
            RunEventKind::Workflow(WorkflowEvent::Declared(declared)) => {
                workflows::declared(&event.run_id, declared, story, cx);
            }
            RunEventKind::Workflow(WorkflowEvent::PhaseAdded { phase, why, .. }) => {
                workflows::phase_added(&phase.title, why, story, cx);
            }
            RunEventKind::Retro(_) => story.retro = true,
            // The engine's note repeats the line the failed phase printed, without the reason.
            RunEventKind::Note(note) if story.retro_failed && note.text == RETRO_FAILED => {}
            RunEventKind::Note(note) => items.notice(note.level, &note.text, cx),
            _ => {}
        }
    }
}

/// What a card goes by in the chat: its label, never its id (`RunState::card_label`).
fn card_label(cx: &Cx<'_>, run: &RunId, card: &CardId) -> String {
    cx.state
        .runs
        .get(run)
        .map_or_else(|| card.to_string(), |state| state.card_label(card))
}
