//! A run's story in scrollback: goal and plan steps, checks, fallbacks, pauses, resumes,
//! merges, and the run's end, each a tool-call-shaped line with `⎿` details. Agents and tasks
//! draw their own items.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::events::agent_status::{AgentStatusEvent, FallbackEvent, FallbackPlan};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DivideEvent, JudgingResult, MappedTopic, PlanEvent};
use argonauts_core::events::run::{ClosingAnswer, Party, RunLifecycle, RunMode, RunPhase};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::shared::BlackboardEvent;
use argonauts_core::events::task::{CheckOutcome, MergeEvent, TaskEvent, TaskState, TryCause};
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::{HealthState, ModelHealth, ModelState, TEST_TASK_SUCCEEDED};
use argonauts_core::log::{COPIES_REMOVED, RUN_REVIEW_FAILED};
use argonauts_core::names::{AgentId, ModelRef};
use argonauts_core::{PhaseStatus, RunId, TaskId};
use time::OffsetDateTime;

use crate::format::WallClock;

use super::items::Items;
use super::tries::Tries;
use super::{Cx, checks, fallback, lifecycle, steps, workflows};

#[derive(Debug, Default)]
pub(super) struct Milestones {
    runs: BTreeMap<RunId, RunStory>,
    /// Each agent's last health, to tell when one comes back.
    health: BTreeMap<AgentId, HealthState>,
    /// Each model's last word, so a line says only what changed.
    model_health: BTreeMap<ModelRef, ModelHealth>,
    /// The models a line said went out on their own, so each one's return gets a line too.
    models_said_out: BTreeSet<ModelRef>,
    /// The run whose past, drawn again, waits off scrollback until its wait says how it prints.
    withheld_for: Option<RunId>,
}

/// What a run's later lines need from its earlier events.
#[derive(Debug, Default)]
pub(super) struct RunStory {
    pub started: Option<OffsetDateTime>,
    /// A crew run names the task on its check lines.
    pub crew: bool,
    /// The last pause asked to stop now rather than at a safe point.
    pub pause_now: bool,
    /// Who asked for the pause under way, so its line is theirs.
    pub pause_by: Option<Party>,
    pub readings: u32,
    pub drafts: u32,
    pub mapped: Vec<MappedTopic>,
    pub judged: Option<JudgingResult>,
    pub fast_path: Option<ModelRef>,
    pub orchestrated: Option<ModelRef>,
    /// Check results per task, printed together when the task leaves checking. `None` is the final
    /// check's.
    pub checks: BTreeMap<Option<TaskId>, Vec<CheckOutcome>>,
    pub run_merged: Option<String>,
    /// The run works in a folder without git, where Argonauts Code's own history keeps its
    /// work, so it has no branch of yours to speak of.
    pub folder_without_git: bool,
    /// The copies removed when the run ended, as its note names them. Said on the run's last
    /// row.
    pub copies_removed: Option<String>,
    pub pull_request: Option<PullRequest>,
    pub run_review: bool,
    pub run_review_failed: bool,
    /// Each task's latest checkpoint tag and why it fell back.
    pub tags: BTreeMap<TaskId, u32>,
    pub fallbacks: BTreeMap<TaskId, FallbackPlan>,
    /// The run's own workflow, by the name its start item printed.
    pub workflow: Option<String>,
    /// Its start item, while it waits for the workflow a step runs to report its phases.
    pub workflow_start: Option<workflows::Start>,
    /// The CLI workflows of the run's task whose Workflow item printed, the run's own first.
    pub announced: Vec<String>,
    /// The run's name as its start item printed it, which a later workflow names.
    pub run_name: Option<String>,
    /// The reviewers the request named, said beside the workflow's phases.
    pub reviewers_asked: Option<String>,
    /// The checks the main agent picked for the run, said under the workflow's row.
    pub checks_picked: Option<String>,
    /// Printed at the run's end, after the merge line.
    pub closing: Option<ClosingAnswer>,
    /// Its waits for nobody and its tries after each.
    pub tries: Tries,
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
        // What another run's past held back prints before this run's event.
        if self
            .withheld_for
            .as_ref()
            .is_some_and(|run| *run != event.run_id)
            || ends_a_try(&event.kind)
        {
            self.print_withheld(cx);
        }
        let story = self.runs.entry(event.run_id.clone()).or_default();
        let task = event.task_id.as_ref();
        match &event.kind {
            RunEventKind::Run(lifecycle) => match lifecycle {
                RunLifecycle::Started(started) => {
                    story.started = Some(event.ts);
                    story.crew = started.mode == RunMode::Crew;
                    story.folder_without_git =
                        started.base_branch.as_deref() == Some(argonauts_core::hidden_git::BRANCH);
                    story.reviewers_asked = story
                        .crew
                        .then(|| workflows::reviewers_asked(started, &cx.state.names))
                        .flatten();
                    story.checks_picked = workflows::checks_picked(started);
                    steps::run_started(started, story.reviewers_asked.is_some(), cx);
                }
                RunLifecycle::Resumed(resumed) => {
                    let (held, woke_unsaid) = story.tries.woke(event.ts, cx);
                    if held {
                        self.withheld_for = Some(event.run_id.clone());
                    }
                    let tried_at = woke_unsaid.then_some(event.ts);
                    lifecycle::resumed(&event.run_id, resumed, tried_at, clock, cx);
                }
                RunLifecycle::PauseRequested { now, by } => {
                    story.pause_now = *now;
                    story.pause_by = Some(by.clone());
                }
                RunLifecycle::Paused {
                    sessions_stopped,
                    committed,
                } => lifecycle::paused(
                    &event.run_id,
                    story.pause_now,
                    story.pause_by.take().as_ref(),
                    *sessions_stopped,
                    committed,
                    cx,
                ),
                RunLifecycle::Waiting { reason, .. } => {
                    if story.tries.wait(reason, clock, cx) {
                        self.withheld_for = None;
                    } else {
                        if self.withheld_for.take().is_some() {
                            cx.model.history.print_withheld();
                        }
                        lifecycle::waiting(reason, clock, cx);
                    }
                }
                RunLifecycle::ClosingAnswer(answer) => story.closing = Some((**answer).clone()),
                RunLifecycle::Finished { outcome, detail } => {
                    let elapsed = story.started.map(|at| {
                        let seconds = (event.ts - at).whole_seconds().max(0);
                        std::time::Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
                    });
                    workflows::done(&event.run_id, story, *outcome, elapsed, cx);
                    lifecycle::run_end(event, story, *outcome, detail.as_deref(), clock, cx);
                    self.runs.remove(&event.run_id);
                }
                _ => {}
            },
            RunEventKind::Phase(change) => match (change.phase, change.status) {
                (RunPhase::RunReview, PhaseStatus::Finished) => story.run_review = true,
                // The finish line mustn't call a failed run review saved.
                (RunPhase::RunReview, PhaseStatus::Failed) => {
                    story.run_review = false;
                    story.run_review_failed = true;
                    steps::run_review_failed(change.summary.as_deref(), cx);
                }
                (phase, status) => steps::phase(phase, status, change.summary.as_deref(), cx),
            },
            RunEventKind::Goal(goal) => match goal {
                GoalEvent::Read { .. } => story.readings += 1,
                GoalEvent::Approved { file, by } => steps::goal(story, file, by, cx),
                GoalEvent::ChangeResolved { resolution, .. } => steps::goal_change(*resolution, cx),
                _ => {}
            },
            RunEventKind::Plan(plan) => match plan {
                PlanEvent::Sealed { round: 1, .. } => story.drafts += 1,
                PlanEvent::Mapped { topics, .. } => story.mapped = topics.clone(),
                PlanEvent::FastPath { merged_by } => story.fast_path = Some(merged_by.clone()),
                PlanEvent::Judged(result) => story.judged = Some(result.clone()),
                PlanEvent::Orchestrated {
                    orchestrator,
                    rejection: None,
                } => story.orchestrated = Some(orchestrator.clone()),
                _ => {}
            },
            RunEventKind::Divide(DivideEvent::Graph(graph)) => {
                steps::plan(story, graph.tasks.len(), cx)
            }
            RunEventKind::Check(outcome) => {
                story
                    .checks
                    .entry(task.cloned())
                    .or_default()
                    .push(outcome.clone());
            }
            RunEventKind::Task(task_event) => {
                let Some(task) = task else { return };
                match task_event {
                    TaskEvent::State {
                        from: Some(TaskState::Checking),
                        to,
                        ..
                    } if *to != TaskState::Checking => {
                        if let Some(checks) = story.checks.remove(&Some(task.clone())) {
                            let label = task_label(cx, &event.run_id, task);
                            checks::report(Some(&label), &checks, story.crew, cx);
                        }
                    }
                    TaskEvent::CheckpointTagged { n, .. } => {
                        story.tags.insert(task.clone(), *n);
                    }
                    TaskEvent::Reassigned { from, to, by, .. } => {
                        let label = task_label(cx, &event.run_id, task);
                        fallback::reassigned(&label, from.as_ref(), to, by, cx)
                    }
                    TaskEvent::Rewound { to, kept_on } => {
                        let label = task_label(cx, &event.run_id, task);
                        fallback::rewound(&label, *to, kept_on, cx)
                    }
                    TaskEvent::DebtCleared { by } => {
                        let label = task_label(cx, &event.run_id, task);
                        fallback::debt_cleared(&label, by, cx)
                    }
                    TaskEvent::Try {
                        worker,
                        cause: TryCause::Rotation,
                        ..
                    } => {
                        let label = task_label(cx, &event.run_id, task);
                        fallback::rotated(&label, worker, cx)
                    }
                    TaskEvent::Try {
                        worker,
                        cause: TryCause::Restarted,
                        ..
                    } => {
                        let label = task_label(cx, &event.run_id, task);
                        fallback::restarted(&label, worker, cx)
                    }
                    _ => {}
                }
            }
            RunEventKind::Merge(merge) => match merge {
                MergeEvent::RunMerged { target, .. } => story.run_merged = Some(target.clone()),
                MergeEvent::PullRequest { url, command, .. } => {
                    story.pull_request = Some(PullRequest {
                        url: url.clone(),
                        command: command.clone(),
                    });
                }
                MergeEvent::FinalCheck { .. } => {
                    let checks = story.checks.remove(&None).unwrap_or_default();
                    checks::report(None, &checks, false, cx);
                }
                MergeEvent::Conflict {
                    task,
                    files,
                    assigned_to,
                } => {
                    let label = task_label(cx, &event.run_id, task);
                    fallback::conflict(&label, files.len(), assigned_to, cx)
                }
                MergeEvent::MainMoved { commits, .. } => lifecycle::main_moved(*commits, cx),
                _ => {}
            },
            RunEventKind::Fallback(fallback_event) => match fallback_event {
                FallbackEvent::Needed(plan) => {
                    story.fallbacks.insert(plan.task.clone(), plan.clone());
                }
                FallbackEvent::FellBack {
                    task,
                    to,
                    mode,
                    context,
                    ..
                } => fallback::fell_back(
                    fallback::Recorded {
                        task: &task_label(cx, &event.run_id, task),
                        to,
                        mode: *mode,
                        context: *context,
                        tag: story.tags.get(task).copied(),
                    },
                    story.fallbacks.get(task),
                    clock,
                    cx,
                ),
                FallbackEvent::Waiting {
                    task,
                    agent_id,
                    until,
                    kept_off,
                } => {
                    let wait = fallback::Wait {
                        task: &task_label(cx, &event.run_id, task),
                        agent_id,
                        until: *until,
                        kept_off: kept_off.as_ref(),
                    };
                    fallback::waiting(&wait, clock, cx)
                }
                FallbackEvent::Note { .. }
                | FallbackEvent::JobWaiting(_)
                | FallbackEvent::JobFreed { .. } => {}
            },
            RunEventKind::AgentStatus(agent_id) => match agent_id {
                AgentStatusEvent::Health(health) => {
                    let before = self.health.insert(health.agent_id.clone(), health.state);
                    // A CLI comes back from Down by way of its test task.
                    let was_out = matches!(
                        before,
                        Some(HealthState::Down | HealthState::Degraded | HealthState::Testing)
                    );
                    if was_out && health.state == HealthState::Healthy {
                        let tested = health.reason.as_deref() == Some(TEST_TASK_SUCCEEDED);
                        fallback::agent_back(&event.run_id, &health.agent_id, tested, cx);
                    }
                    if health.state == HealthState::Down && before != Some(HealthState::Down) {
                        let out: Vec<&ModelHealth> = self
                            .model_health
                            .values()
                            .filter(|model| model.model.agent_id() == health.agent_id)
                            .collect();
                        fallback::agent_down(&event.run_id, health, &out, cx);
                    }
                }
                AgentStatusEvent::Disabled { agent_id, reason } => items.notice(
                    argonauts_core::NoteLevel::Warn,
                    &format!("{agent_id} is off for new tasks: {reason}"),
                    cx,
                ),
                AgentStatusEvent::Model(health) => {
                    let cli = health.model.agent_id();
                    let cli_out = matches!(
                        self.health.get(&cli),
                        Some(HealthState::Down | HealthState::Testing)
                    );
                    let another_out = self.model_health.values().any(|other| {
                        other.model != health.model
                            && other.model.agent_id() == cli
                            && other.state == ModelState::Out
                    });
                    let before = self
                        .model_health
                        .insert(health.model.clone(), health.clone());
                    let was = before.map_or(ModelState::Healthy, |before| before.state);
                    let went_out = health.state == ModelState::Out && was != ModelState::Out;
                    let came_back = health.state == ModelState::Healthy
                        && matches!(was, ModelState::Out | ModelState::Testing);
                    // A second model out takes the CLI down, and that line names each model's
                    // trouble. A model's return is said only where its going was.
                    if went_out && !cli_out && !another_out {
                        self.models_said_out.insert(health.model.clone());
                        fallback::model_out(health, &event.run_id, clock, cx);
                    } else if came_back && self.models_said_out.remove(&health.model) {
                        fallback::model_back(&health.model, &event.run_id, cx);
                    }
                }
                AgentStatusEvent::PlanUsage(_) => {}
            },
            RunEventKind::Workflow(WorkflowEvent::Declared(declared)) => {
                workflows::declared(&event.run_id, declared, story, cx);
            }
            RunEventKind::Workflow(WorkflowEvent::PhaseAdded { phase, why, .. }) => {
                workflows::phase_added(&phase.name, why, story, cx);
            }
            RunEventKind::Workflow(WorkflowEvent::Reported(report)) => {
                workflows::reported(report, story, cx);
            }
            RunEventKind::Agent(argonauts_core::AgentEvent::Workflow { workflow }) => {
                workflows::tasks_own_reported(event, workflow, story, cx);
            }
            RunEventKind::RunReview(_) => story.run_review = true,
            // The engine's note repeats the line the failed phase printed, without the reason.
            RunEventKind::Note(note)
                if story.run_review_failed && note.text == RUN_REVIEW_FAILED => {}
            RunEventKind::Note(note) if note.text.starts_with(COPIES_REMOVED) => {
                story.copies_removed = Some(note.line().to_owned());
            }
            RunEventKind::Note(note) => {
                if story.tries.note(note, cx) {
                    self.withheld_for = Some(event.run_id.clone());
                }
                items.note(note, cx);
            }
            RunEventKind::Session(SessionEvent::Ended(end)) => {
                if let Some(key) = &event.session {
                    story.tries.session_ended(&event.run_id, key, end, cx);
                }
            }
            RunEventKind::Blackboard(BlackboardEvent::AnsweredForYou(answered)) => {
                let by = answered.by.as_ref().map(|by| cx.state.names.short(by));
                super::answered::answered_for_you(answered, by.as_deref(), cx);
            }
            _ => {}
        }
    }
}

impl Milestones {
    /// A past drawn again that waited off scrollback prints, in order, as something else
    /// comes on screen.
    pub(super) fn print_withheld(&mut self, cx: &mut Cx<'_>) {
        if self.withheld_for.take().is_some() {
            cx.model.history.print_withheld();
        }
    }
}

/// An event no try that failed the way the one before did has, so what a try held back prints
/// before it.
fn ends_a_try(kind: &RunEventKind) -> bool {
    match kind {
        RunEventKind::Run(lifecycle) => matches!(
            lifecycle,
            RunLifecycle::Finished { .. }
                | RunLifecycle::Paused { .. }
                | RunLifecycle::PauseRequested { .. }
                | RunLifecycle::ClosingAnswer(_)
        ),
        RunEventKind::Merge(_) | RunEventKind::RunReview(_) => true,
        _ => false,
    }
}

/// What a task goes by in the conversation: its label, never its id (`RunState::task_label`).
fn task_label(cx: &Cx<'_>, run: &RunId, task: &TaskId) -> String {
    cx.state
        .runs
        .get(run)
        .map_or_else(|| task.to_string(), |state| state.task_label(task))
}
