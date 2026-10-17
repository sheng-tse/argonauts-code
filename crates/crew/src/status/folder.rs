//! The status summary as an incremental fold: each recorded event updates it, so it's never
//! recomputed from the whole log. `status::summary` is a fresh folder over a slice of events.

use std::collections::VecDeque;

use serde::Serialize;

use argonauts_core::api::{StatusSummary, TaskBrief};
use argonauts_core::events::agent_status::{AgentStatusEvent, FallbackEvent, RunSpend, SpendEvent};
use argonauts_core::events::approval::{AnswerAction, ApprovalEvent, ApprovalPacket};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::plan::{DivideEvent, JudgingResult, PlanEvent};
use argonauts_core::events::run::{Party, PhaseChange, RunLifecycle};
use argonauts_core::events::shared::{
    BlackboardEvent, BlackboardKind, BlackboardMessage, Recipient,
};
use argonauts_core::events::task::{
    CheckOutcome, MergeEvent, ReviewEvent, TaskEvent, TaskSpec, TaskState,
};
use argonauts_core::events::workflow::{StepEnd, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::PlanUsageReading;
use argonauts_core::names::{ApprovalId, DecisionId, MessageId};
use argonauts_core::settings::protocol::VisibilitySettings;
use argonauts_core::{PhaseStatus, RunId, TaskId};

use crate::state::workflow::Workflows;
use crate::status::Viewer;
use crate::status::visibility::{self, Audience, Moment};
use crate::status::working;
use crate::understand::materiality::one_line;

/// Lines kept in `recent`, newest last.
pub const RECENT_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub audience: Audience,
    pub text: String,
    /// The same line naming plan authors, for those who may see them.
    pub with_authors: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub brief: TaskBrief,
    pub audience: Audience,
    pub depends_on: Vec<TaskId>,
}

/// Everything any viewer's summary is filtered from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tagged {
    pub run: Option<RunId>,
    pub goal: Vec<String>,
    pub plan: Option<String>,
    pub decisions: Vec<(DecisionId, String)>,
    pub tasks: Vec<Task>,
    pub recent: VecDeque<Line>,
    /// Where the run's workflow is, shown after the recent lines.
    pub now: Option<Line>,
    pub spend: Option<RunSpend>,
    pub plan_windows: Vec<PlanUsageReading>,
    pub waiting: Vec<(ApprovalId, String)>,
    pub moment: Moment,
}

#[derive(Debug, Clone, Default)]
pub struct Folder {
    tagged: Tagged,
    rules: VisibilitySettings,
    /// Every task's hidden paths. No agent sees a line that names one.
    hidden: Vec<String>,
    /// The newest draft round sealed so far.
    drafted_round: u8,
    /// Composed workflows may run no step that records a legacy phase, so the working line
    /// comes from the workflow events alone.
    workflows: Workflows,
    /// The open plan approval's summary, which becomes the plan when you approve it.
    proposed_plan: Option<(ApprovalId, String)>,
    /// Who the last pushed event changed the summary for.
    changed: Option<Audience>,
    /// Agents' questions meant for you, by id, until one is answered.
    asked_you: Vec<(MessageId, String)>,
    /// What autopilot answered in your place, kept with the goal for you to review.
    answered_for_you: Vec<String>,
}

impl Folder {
    /// A folder applying `visibility.*` from the run's settings.
    pub fn new(rules: VisibilitySettings) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }

    pub fn push(&mut self, event: &RunEvent) {
        self.changed = self.fold(event);
        if self.tagged.run.is_none() {
            self.tagged.run = Some(event.run_id.clone());
            self.changed = Some(Audience::Everyone);
        }
    }

    /// Whether the last pushed event changed what this viewer sees, so its `status.json` needs
    /// writing.
    pub fn changed_for(&self, viewer: &Viewer) -> bool {
        if let Viewer::Session { role, .. } = viewer
            && !visibility::enabled(*role, &self.rules)
        {
            return false;
        }
        self.changed.as_ref().is_some_and(|audience| {
            visibility::sees(viewer, audience, &self.tagged.moment, &self.rules)
        })
    }

    pub fn summary(&self, viewer: &Viewer) -> StatusSummary {
        visibility::filter(&self.tagged, viewer, &self.rules)
    }

    pub fn tagged(&self) -> &Tagged {
        &self.tagged
    }

    fn fold(&mut self, event: &RunEvent) -> Option<Audience> {
        let task = event.task_id.as_ref();
        match &event.kind {
            RunEventKind::Run(e) => self.run(e),
            RunEventKind::Phase(e) => self.phase(e),
            RunEventKind::Goal(e) => self.goal(e),
            RunEventKind::Plan(e) => self.plan(e),
            RunEventKind::Divide(DivideEvent::Worker { task, worker, .. }) => {
                let known = self.task_mut(task)?;
                known.brief.worker = Some(worker.clone());
                Some(known.audience.clone())
            }
            RunEventKind::Divide(_) => None,
            RunEventKind::Task(e) => self.task(task?, e),
            RunEventKind::Check(outcome) => self.check(task, outcome),
            RunEventKind::Review(e) => self.review(task?, e),
            RunEventKind::Merge(e) => self.merge(e),
            RunEventKind::Approval(e) => self.approval(e),
            RunEventKind::Fallback(e) => self.fallback(e),
            RunEventKind::AgentStatus(e) => self.agent_id(e),
            RunEventKind::Spend(e) => self.limit(e),
            RunEventKind::Workflow(e) => self.workflow(e, event),
            RunEventKind::Blackboard(BlackboardEvent::Posted(message))
                if self.answered_for_you(message) =>
            {
                Some(Audience::Everyone)
            }
            RunEventKind::Blackboard(BlackboardEvent::Posted(message)) => {
                // Only settled records count as decisions; the traffic itself isn't state.
                let record = message.kind == BlackboardKind::Decision
                    && matches!(message.from, Party::ArgonautsCode | Party::User);
                if !record {
                    return None;
                }
                let id = DecisionId::parse(message.id.as_str())?;
                self.tagged.decisions.push((id, message.text.clone()));
                Some(Audience::Everyone)
            }
            RunEventKind::Blackboard(
                BlackboardEvent::Delivered { .. } | BlackboardEvent::AnsweredForYou(_),
            )
            | RunEventKind::Session(_)
            | RunEventKind::Agent(_)
            | RunEventKind::RunReview(_)
            | RunEventKind::Note(_) => None,
        }
    }

    fn run(&mut self, event: &RunLifecycle) -> Option<Audience> {
        match event {
            RunLifecycle::Finished { outcome, .. } => {
                self.line(format!("Run finished: {}", words(outcome)))
            }
            _ => None,
        }
    }

    fn phase(&mut self, change: &PhaseChange) -> Option<Audience> {
        let summary = change.summary.as_ref()?;
        if !matches!(change.status, PhaseStatus::Finished | PhaseStatus::Failed) {
            return None;
        }
        self.line(format!("{}: {summary}", words(&change.phase)))
    }

    fn workflow(&mut self, event: &WorkflowEvent, envelope: &RunEvent) -> Option<Audience> {
        self.workflows.apply(event, envelope);
        // Steps that finish well show up through their tasks, checks, and approvals. The
        // step's own summary stays out, since a draft or review summary can name authors.
        let pushed = match event {
            WorkflowEvent::StepFinished(finished) if finished.end == StepEnd::Failed => {
                let at = working::place(
                    &self.workflows,
                    &finished.step,
                    finished.kind,
                    finished.round,
                );
                self.line(format!("{at} failed"))
            }
            WorkflowEvent::Changed(changed) => {
                self.line(format!("The workflow changed: {}", changed.reason))
            }
            _ => None,
        };
        let now = working::line(&self.workflows).map(|text| {
            self.guarded(Line {
                audience: Audience::Everyone,
                text: format!("Now: {text}"),
                with_authors: None,
            })
        });
        if now == self.tagged.now {
            return pushed;
        }
        self.tagged.now = now;
        Some(Audience::Everyone)
    }

    fn goal(&mut self, event: &GoalEvent) -> Option<Audience> {
        match event {
            GoalEvent::Approved { file, .. } => {
                // The main agent's answers for you stay in view for you to review.
                let by_main_agent = file
                    .assumptions
                    .iter()
                    .filter(|assumption| assumption.by_main_agent())
                    .map(|assumption| format!("Answered by the main agent · {}", assumption.text));
                self.tagged.goal = std::iter::once(file.summary.clone())
                    .chain(
                        file.criteria
                            .iter()
                            .map(|criterion| format!("{}: {}", criterion.id, criterion.text)),
                    )
                    .chain(by_main_agent)
                    .chain(self.answered_for_you.iter().cloned())
                    .collect();
                self.line(format!("Goal approved, version {}", file.version))
            }
            GoalEvent::ChangeResolved { id, resolution, .. } => {
                self.line(format!("Goal change {id}: {}", words(resolution)))
            }
            _ => None,
        }
    }

    /// Notes an agent's question meant for you, and, once autopilot answers one in your place,
    /// keeps the answer with the goal, beside the main agent's answers to the goal's questions.
    /// True when the summary changed.
    fn answered_for_you(&mut self, message: &BlackboardMessage) -> bool {
        match message.kind {
            BlackboardKind::Question if message.to == Recipient::User => {
                self.asked_you
                    .push((message.id.clone(), one_line(&message.text)));
                false
            }
            BlackboardKind::Answer if message.from != Party::User => {
                let Some(at) = self
                    .asked_you
                    .iter()
                    .position(|(id, _)| message.in_reply_to.as_ref() == Some(id))
                else {
                    return false;
                };
                let (_, question) = self.asked_you.remove(at);
                let line = format!(
                    "Answered for you on autopilot · {}: {}",
                    question.trim_end_matches(['?', '.', ':']),
                    one_line(&message.text)
                );
                self.answered_for_you.push(line.clone());
                self.tagged.goal.push(line);
                true
            }
            _ => false,
        }
    }

    fn plan(&mut self, event: &PlanEvent) -> Option<Audience> {
        match event {
            PlanEvent::Sealed {
                label,
                worker,
                round,
                summary,
                ..
            } => {
                self.drafted_round = self.drafted_round.max(*round);
                let audience = Audience::Draft {
                    label: *label,
                    round: *round,
                };
                self.push_line(Line {
                    audience,
                    text: format!("Plan {label} sealed: {summary}"),
                    with_authors: Some(format!("Plan {label} ({worker}) sealed: {summary}")),
                })
            }
            PlanEvent::Mapped { round, topics, .. } => {
                self.all_sealed(*round);
                let disputed = topics.iter().filter(|topic| topic.disputed).count();
                self.line(format!("Drafts mapped: {disputed} disputed topics"))
            }
            PlanEvent::FastPath { .. } => {
                self.all_sealed(self.drafted_round);
                self.line("No disputed topics, so the drafts were merged".to_owned())
            }
            PlanEvent::Critiqued { .. }
            | PlanEvent::ExperimentProposed { .. }
            | PlanEvent::Scored { .. } => {
                self.all_sealed(self.drafted_round);
                Some(Audience::Everyone)
            }
            PlanEvent::ExperimentFinished { task, report } => self.line(format!(
                "{task} experiment on {}: {}",
                report.topic, report.finding
            )),
            PlanEvent::Judged(result) => {
                self.all_sealed(self.drafted_round);
                self.judged(result)
            }
            PlanEvent::Decided(decision) => {
                self.tagged.decisions.push((
                    decision.id.clone(),
                    format!("{}: {}", decision.title, decision.choice),
                ));
                Some(Audience::Everyone)
            }
            PlanEvent::Orchestrated { rejection, .. } => match rejection {
                None => self.line("The orchestrator's plan was accepted".to_owned()),
                Some(rule) => self.line(format!("The orchestrator's plan was rejected: {rule}")),
            },
        }
    }

    fn judged(&mut self, result: &JudgingResult) -> Option<Audience> {
        if result.near_tie {
            return self.line("Judging ended in a near tie, which you break".to_owned());
        }
        let winner = result.winner?;
        let author = result
            .plans
            .iter()
            .find(|tally| tally.label == winner)
            .map(|tally| tally.author.to_string());
        let margin = result.margin;
        self.push_line(Line {
            audience: Audience::Everyone,
            text: format!("Plan {winner} won by {margin:.2}"),
            with_authors: author
                .map(|author| format!("Plan {winner} ({author}) won by {margin:.2}")),
        })
    }

    fn task(&mut self, id: &TaskId, event: &TaskEvent) -> Option<Audience> {
        match event {
            TaskEvent::Created(spec) => Some(self.created(spec)),
            TaskEvent::State { to, reason, .. } => {
                let known = self.task_mut(id)?;
                known.brief.state = *to;
                // A waiting task's note lists what it waits for; the refresh below writes it.
                known.brief.note = match to {
                    TaskState::Blocked
                    | TaskState::NeedsDecision
                    | TaskState::Failed
                    | TaskState::NeedsYou
                    | TaskState::Deferred => reason.clone(),
                    _ => String::new(),
                };
                let audience = known.audience.clone();
                self.refresh_waiting_notes();
                match to {
                    TaskState::Merged
                    | TaskState::Failed
                    | TaskState::NeedsYou
                    | TaskState::NeedsDecision
                    | TaskState::Blocked => {
                        let reason = if reason.is_empty() {
                            String::new()
                        } else {
                            format!(": {reason}")
                        };
                        self.task_line(audience, format!("{id} {}{reason}", words(to)))
                    }
                    _ => Some(audience),
                }
            }
            TaskEvent::Try {
                try_number, worker, ..
            } if *try_number > 1 => {
                let audience = self.task_mut(id)?.audience.clone();
                self.task_line(audience, format!("{id} try {try_number} on {worker}"))
            }
            TaskEvent::Reported(report) => {
                let audience = self.task_mut(id)?.audience.clone();
                // The summary stays out: reviewers never see the worker's notes.
                self.task_line(audience, format!("{id} reported {}", words(&report.status)))
            }
            TaskEvent::Reassigned { to, .. } => {
                let known = self.task_mut(id)?;
                known.brief.worker = Some(to.clone());
                let audience = known.audience.clone();
                self.task_line(audience, format!("{id} reassigned to {to}"))
            }
            TaskEvent::TakenOver { .. } => {
                let audience = self.task_mut(id)?.audience.clone();
                self.task_line(audience, format!("{id} taken over by you"))
            }
            TaskEvent::TakeOverEnded { .. } => {
                let audience = self.task_mut(id)?.audience.clone();
                self.task_line(audience, format!("{id} given back after your take over"))
            }
            TaskEvent::Rewound { to, .. } => {
                let audience = self.task_mut(id)?.audience.clone();
                self.task_line(audience, format!("{id} rewound to fallback-{to}"))
            }
            _ => None,
        }
    }

    fn created(&mut self, spec: &TaskSpec) -> Audience {
        let hidden = !spec.hidden_paths.is_empty()
            && spec
                .owned_paths
                .iter()
                .all(|owned| spec.hidden_paths.iter().any(|path| covers(path, owned)));
        self.hidden.extend(spec.hidden_paths.iter().cloned());
        let audience = if hidden {
            Audience::UserOnly
        } else {
            Audience::Everyone
        };
        let task = Task {
            brief: TaskBrief {
                task: spec.id.clone(),
                title: spec.title.clone(),
                state: TaskState::Waiting,
                worker: spec.worker.clone(),
                note: String::new(),
            },
            audience: audience.clone(),
            depends_on: spec.depends_on.clone(),
        };
        match self
            .tagged
            .tasks
            .iter_mut()
            .find(|known| known.brief.task == spec.id)
        {
            Some(known) => *known = task,
            None => self.tagged.tasks.push(task),
        }
        self.refresh_waiting_notes();
        audience
    }

    /// "needs task-002 and task-003" on each waiting task, for dependencies not merged yet.
    fn refresh_waiting_notes(&mut self) {
        let merged: Vec<TaskId> = self
            .tagged
            .tasks
            .iter()
            .filter(|task| task.brief.state == TaskState::Merged)
            .map(|task| task.brief.task.clone())
            .collect();
        for task in &mut self.tagged.tasks {
            if task.brief.state != TaskState::Waiting {
                continue;
            }
            let missing: Vec<String> = task
                .depends_on
                .iter()
                .filter(|dependency| !merged.contains(dependency))
                .map(ToString::to_string)
                .collect();
            task.brief.note = if missing.is_empty() {
                String::new()
            } else {
                format!("needs {}", and_list(&missing))
            };
        }
    }

    fn check(&mut self, task: Option<&TaskId>, outcome: &CheckOutcome) -> Option<Audience> {
        let verdict = if outcome.passed { "passed" } else { "failed" };
        let scope = task.map(|task| format!("{task} ")).unwrap_or_default();
        let text = format!(
            "{scope}check {} {verdict}: {}",
            outcome.name, outcome.summary
        );
        let audience = task
            .and_then(|task| self.task_mut(task))
            .map(|known| known.audience.clone())
            .unwrap_or(Audience::Everyone);
        self.task_line(audience, text)
    }

    fn review(&mut self, task: &TaskId, event: &ReviewEvent) -> Option<Audience> {
        match event {
            ReviewEvent::Reviewed {
                reviewer,
                blind,
                review,
                ..
            } => {
                let audience = if *blind {
                    Audience::BlindReview(task.clone())
                } else {
                    Audience::Everyone
                };
                self.push_line(Line {
                    audience,
                    text: format!("{task} reviewed by {reviewer}: {}", words(&review.verdict)),
                    with_authors: None,
                })
            }
            ReviewEvent::Settled { ruling, .. } => {
                self.tagged.moment.settled.insert(task.clone());
                self.line(format!(
                    "{task} settled by the judge: {}",
                    words(&ruling.verdict)
                ))
            }
            ReviewEvent::TestsWritten {
                reviewer,
                for_task,
                files,
            } => self.line(format!(
                "{reviewer} wrote {} acceptance test files for {for_task}",
                files.len()
            )),
        }
    }

    fn merge(&mut self, event: &MergeEvent) -> Option<Audience> {
        let text = match event {
            MergeEvent::BranchCreated { branch, .. } => {
                format!("Run branch {branch} created")
            }
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
            MergeEvent::MainMergedIn { .. } => "main merged into the run branch".to_owned(),
            MergeEvent::FinalCheck { passed: true } => "The final check passed".to_owned(),
            MergeEvent::FinalCheck { passed: false } => "The final check failed".to_owned(),
            // The commit your branch moved to, so it's never taken for the one a task merged
            // into the run branch as.
            MergeEvent::RunMerged {
                target,
                sha,
                fast_forward: true,
            } => {
                let short: String = sha.chars().take(8).collect();
                format!("Merged into {target} as {short}")
            }
            MergeEvent::RunMerged { target, .. } => format!("Merged into {target}"),
            MergeEvent::WaitingToMerge { target, in_the_way } => format!(
                "Waiting to merge into {target}: {} in your checkout in the way",
                and_list(in_the_way)
            ),
            MergeEvent::PullRequest { url: Some(url), .. } => {
                format!("Pull request opened: {url}")
            }
            MergeEvent::PullRequest { branch, .. } => {
                format!("A pull request for {branch} is ready to open")
            }
            MergeEvent::Abandoned { .. } => {
                "The run was abandoned, and its work is removed".to_owned()
            }
            MergeEvent::DependenciesAdded { packages, .. } => {
                format!("Added {}", and_list(packages))
            }
        };
        self.line(text)
    }

    fn approval(&mut self, event: &ApprovalEvent) -> Option<Audience> {
        match event {
            ApprovalEvent::Opened(opened) => {
                if let ApprovalPacket::Plan(packet) = &opened.packet {
                    self.all_sealed(self.drafted_round);
                    self.proposed_plan = Some((opened.id.clone(), packet.summary.clone()));
                }
                self.tagged
                    .waiting
                    .push((opened.id.clone(), opened.title.clone()));
                Some(Audience::Everyone)
            }
            ApprovalEvent::Decided(decided) => {
                let title = self.close(&decided.id);
                let approved = matches!(
                    decided.action,
                    AnswerAction::Approve | AnswerAction::ApproveWithEdits
                );
                if let Some((id, summary)) = self.proposed_plan.take() {
                    if id != decided.id {
                        self.proposed_plan = Some((id, summary));
                    } else if approved {
                        self.tagged.plan = Some(summary);
                    }
                }
                let title = title.unwrap_or_else(|| format!("Approval {}", decided.id));
                self.line(format!("{title}: {}", words(&decided.action)))
            }
            ApprovalEvent::Withdrawn { id, .. } => {
                self.close(id);
                Some(Audience::Everyone)
            }
            ApprovalEvent::Checked(_) | ApprovalEvent::SpotCheckQueued { .. } => None,
        }
    }

    /// Takes an approval off the waiting list and returns its title.
    fn close(&mut self, id: &ApprovalId) -> Option<String> {
        let at = self
            .tagged
            .waiting
            .iter()
            .position(|(open, _)| open == id)?;
        Some(self.tagged.waiting.remove(at).1)
    }

    fn fallback(&mut self, event: &FallbackEvent) -> Option<Audience> {
        match event {
            FallbackEvent::Needed(plan) => {
                self.line(format!("{} stopped: {}", plan.task, plan.reason))
            }
            FallbackEvent::FellBack { task, to, .. } => {
                if let Some(known) = self.task_mut(task) {
                    known.brief.worker = Some(to.clone());
                }
                self.line(format!("{task} fell back to {to}"))
            }
            FallbackEvent::Waiting { task, agent_id, .. } => {
                self.line(format!("{task} waits for {agent_id}"))
            }
            FallbackEvent::JobWaiting(wait) => self.line(format!(
                "{}: its {} waits for {}",
                wait.title,
                wait.job.role.as_str(),
                wait.stopped.agent_id()
            )),
            FallbackEvent::Note { .. } | FallbackEvent::JobFreed { .. } => None,
        }
    }

    fn agent_id(&mut self, event: &AgentStatusEvent) -> Option<Audience> {
        match event {
            AgentStatusEvent::PlanUsage(reading) => {
                match self
                    .tagged
                    .plan_windows
                    .iter_mut()
                    .find(|known| known.agent_id == reading.agent_id)
                {
                    Some(known) => *known = reading.clone(),
                    None => self.tagged.plan_windows.push(reading.clone()),
                }
                Some(Audience::Everyone)
            }
            AgentStatusEvent::Disabled { agent_id, reason } => {
                self.line(format!("{agent_id} disabled for new tasks: {reason}"))
            }
            AgentStatusEvent::Health(_) | AgentStatusEvent::Model(_) => None,
        }
    }

    fn limit(&mut self, event: &SpendEvent) -> Option<Audience> {
        match event {
            SpendEvent::Run(limit) => {
                self.tagged.spend = Some(limit.clone());
                Some(Audience::Everyone)
            }
            SpendEvent::Hit {
                task, dimension, ..
            } => self.line(format!("{task} hit its {} limit", words(dimension))),
            SpendEvent::Extended { task, .. } => self.line(format!("{task}'s limit went up")),
            SpendEvent::Deferred { tasks } => {
                let tasks: Vec<String> = tasks.iter().map(ToString::to_string).collect();
                self.line(format!(
                    "Deferred until the limit has room: {}",
                    and_list(&tasks)
                ))
            }
        }
    }

    fn all_sealed(&mut self, round: u8) {
        let moment = &mut self.tagged.moment;
        moment.sealed_through = moment.sealed_through.max(round);
    }

    fn task_mut(&mut self, id: &TaskId) -> Option<&mut Task> {
        self.tagged
            .tasks
            .iter_mut()
            .find(|task| &task.brief.task == id)
    }

    fn line(&mut self, text: String) -> Option<Audience> {
        self.task_line(Audience::Everyone, text)
    }

    fn task_line(&mut self, audience: Audience, text: String) -> Option<Audience> {
        self.push_line(Line {
            audience,
            text,
            with_authors: None,
        })
    }

    fn push_line(&mut self, line: Line) -> Option<Audience> {
        let line = self.guarded(line);
        let audience = line.audience.clone();
        self.tagged.recent.push_back(line);
        while self.tagged.recent.len() > RECENT_LINES {
            self.tagged.recent.pop_front();
        }
        Some(audience)
    }

    /// A line that names a hidden path is for you only.
    fn guarded(&self, mut line: Line) -> Line {
        let named = line.with_authors.as_deref().unwrap_or_default();
        if self.names_hidden(&line.text) || self.names_hidden(named) {
            line.audience = Audience::UserOnly;
        }
        line
    }

    fn names_hidden(&self, text: &str) -> bool {
        self.hidden.iter().any(|pattern| {
            let fixed = literal_prefix(pattern);
            !fixed.is_empty() && text.contains(fixed)
        })
    }
}

/// The part of a glob before its first wildcard, without a trailing `/`.
fn literal_prefix(pattern: &str) -> &str {
    let end = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    pattern[..end].trim_end_matches('/')
}

/// Whether the hidden pattern covers this owned path or pattern.
fn covers(hidden: &str, owned: &str) -> bool {
    if hidden == owned {
        return true;
    }
    let fixed = literal_prefix(hidden);
    !fixed.is_empty() && literal_prefix(owned).starts_with(fixed)
}

/// A serialized enum's name in plain words, such as "needs decision".
pub(crate) fn words<T: Serialize>(value: &T) -> String {
    let name = match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        Ok(serde_json::Value::Object(fields)) => fields
            .values()
            .find_map(|field| field.as_str().map(str::to_owned))
            .unwrap_or_default(),
        _ => String::new(),
    };
    argonauts_core::text::state_words(&name).replace(['_', '-'], " ")
}

fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::OffsetDateTime;

    use argonauts_core::events::workflow::{StepFinished, StepStarted, WorkflowStarted};
    use argonauts_core::names::{RoleName, WorkflowId};
    use argonauts_core::workflow::{StandardPhase, StepKind, StepRef, Workflow};

    use super::*;

    fn event(seq: u64, kind: WorkflowEvent) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Workflow(kind),
        }
    }

    fn at(workflow: &WorkflowId, phase: u32, index: u32) -> StepRef {
        StepRef {
            workflow: workflow.clone(),
            phase,
            path: vec![index],
        }
    }

    fn started_workflow(
        id: &WorkflowId,
        parent: Option<StepRef>,
        workflow: Workflow,
    ) -> WorkflowEvent {
        WorkflowEvent::Started(Box::new(WorkflowStarted {
            id: id.clone(),
            from: None,
            version: "v1".to_owned(),
            parent,
            workflow,
        }))
    }

    fn started(
        step: StepRef,
        kind: StepKind,
        standard: StandardPhase,
        round: u32,
    ) -> WorkflowEvent {
        WorkflowEvent::StepStarted(StepStarted {
            step,
            kind,
            standard,
            round,
        })
    }

    fn finished(
        step: StepRef,
        kind: StepKind,
        standard: StandardPhase,
        end: StepEnd,
    ) -> WorkflowEvent {
        WorkflowEvent::StepFinished(StepFinished {
            step,
            kind,
            standard,
            round: 1,
            end,
            summary: Some("codex said the plan by claude is better".to_owned()),
        })
    }

    fn now(folder: &Folder, viewer: &Viewer) -> Option<String> {
        folder
            .summary(viewer)
            .recent
            .last()
            .filter(|line| line.starts_with("Now: "))
            .cloned()
    }

    /// Only runner steps, so no legacy phase event says where the run is: the workflow
    /// events alone give the standard phase, the phase, the step number, and the breadcrumb.
    #[test]
    fn status_follows_a_workflow_of_runner_steps() {
        let outer_id = WorkflowId::numbered(1);
        let inner_id = WorkflowId::numbered(2);
        let outer: Workflow = serde_json::from_value(json!({
            "name": "composed",
            "phases": [
                {"name": "Look", "standard": "check", "steps": [
                    {"kind": "checks"},
                    {"kind": "fan_out", "name": "Second opinions", "roles": ["reviewer"]},
                    {"kind": "review"},
                ]},
                {"name": "Settle", "standard": "decide", "steps": [
                    {"kind": "workflow", "workflow": "review-panel"},
                ]},
            ],
        }))
        .unwrap();
        let inner: Workflow = serde_json::from_value(json!({
            "name": "review-panel",
            "phases": [{"name": "Panel", "standard": "check", "steps": [{"kind": "review"}]}],
        }))
        .unwrap();
        let worker = Viewer::Session {
            role: RoleName::Worker,
            task: None,
            label: None,
        };
        let mut folder = Folder::default();
        folder.push(&event(1, started_workflow(&outer_id, None, outer)));
        assert_eq!(now(&folder, &Viewer::User), None);

        let check = at(&outer_id, 0, 0);
        folder.push(&event(
            2,
            started(check.clone(), StepKind::Checks, StandardPhase::Check, 1),
        ));
        assert!(folder.changed_for(&worker));
        assert_eq!(
            now(&folder, &worker).as_deref(),
            Some("Now: check · Look, step 1 of 3: checks")
        );
        folder.push(&event(
            3,
            finished(check, StepKind::Checks, StandardPhase::Check, StepEnd::Done),
        ));
        assert_eq!(now(&folder, &worker), None);

        let fan_out = at(&outer_id, 0, 1);
        folder.push(&event(
            4,
            started(fan_out.clone(), StepKind::FanOut, StandardPhase::Check, 1),
        ));
        assert_eq!(
            now(&folder, &worker).as_deref(),
            Some("Now: check · Look, step 2 of 3: Second opinions")
        );
        folder.push(&event(
            5,
            finished(
                fan_out,
                StepKind::FanOut,
                StandardPhase::Check,
                StepEnd::Failed,
            ),
        ));
        let recent = folder.summary(&worker).recent;
        assert_eq!(recent, ["Look, step 2 of 3: Second opinions failed"]);

        let parent = at(&outer_id, 1, 0);
        folder.push(&event(
            6,
            started(parent.clone(), StepKind::Workflow, StandardPhase::Decide, 1),
        ));
        folder.push(&event(7, started_workflow(&inner_id, Some(parent), inner)));
        assert_eq!(
            now(&folder, &worker).as_deref(),
            Some("Now: decide · Settle, step 1 of 1: workflow")
        );
        folder.push(&event(
            8,
            started(
                at(&inner_id, 0, 0),
                StepKind::Review,
                StandardPhase::Check,
                2,
            ),
        ));
        assert_eq!(
            now(&folder, &Viewer::Narrator).as_deref(),
            Some("Now: check · Settle › review-panel › Panel, step 1 of 1: review, round 2")
        );
        // The step summaries never reach the status: they can name plan authors.
        let text = crate::status::render(&folder.summary(&worker));
        assert!(!text.contains("claude"), "{text}");
    }

    /// A step or phase named after a hidden path keeps the working line from agents.
    #[test]
    fn working_line_naming_hidden_paths_is_for_you() {
        let id = WorkflowId::numbered(1);
        let workflow: Workflow = serde_json::from_value(json!({
            "name": "hidden",
            "phases": [{"name": "Check tests/hidden", "standard": "check", "steps": [{"kind": "checks"}]}],
        }))
        .unwrap();
        let mut folder = Folder::default();
        folder.hidden.push("tests/hidden/**".to_owned());
        folder.push(&event(1, started_workflow(&id, None, workflow)));
        folder.push(&event(
            2,
            started(at(&id, 0, 0), StepKind::Checks, StandardPhase::Check, 1),
        ));
        let worker = Viewer::Session {
            role: RoleName::Worker,
            task: None,
            label: None,
        };
        assert_eq!(now(&folder, &worker), None);
        assert!(now(&folder, &Viewer::User).is_some());
    }

    #[test]
    fn status_lists_what_the_main_agent_answered_for_you() {
        use argonauts_core::events::goal::{Assumption, GoalFile, GoalSource, MAIN_AGENT_ANSWER};
        use argonauts_core::events::run::Party;

        let assumption = |text: &str, basis: String| Assumption {
            text: text.to_owned(),
            basis,
            by: None,
        };
        let file = GoalFile {
            run: RunId::new("run-01").unwrap(),
            version: 1,
            summary: "Add a --verbose flag".to_owned(),
            criteria: Vec::new(),
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: vec![
                assumption(
                    "Which log level: trace",
                    format!("{MAIN_AGENT_ANSWER}: the README"),
                ),
                assumption("the flag's name: --verbose", "a detail".to_owned()),
            ],
            answers: Vec::new(),
            source: GoalSource::Readings,
            path: Default::default(),
            sha256: String::new(),
        };
        let mut folder = Folder::default();
        folder.push(&RunEvent {
            seq: 1,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Goal(GoalEvent::Approved {
                file,
                by: Party::User,
            }),
        });
        assert_eq!(
            folder.summary(&Viewer::User).goal,
            [
                "Add a --verbose flag",
                "Answered by the main agent · Which log level: trace"
            ]
        );
    }

    #[test]
    fn words_reads_plain_and_tagged_enums() {
        assert_eq!(words(&TaskState::NeedsDecision), "needs decision");
        assert_eq!(words(&AnswerAction::RequestChanges), "request changes");
    }

    #[test]
    fn hidden_patterns_cover_paths_below_them() {
        assert!(covers("tests/hidden/**", "tests/hidden/limits.rs"));
        assert!(covers("tests/hidden/**", "tests/hidden/**"));
        assert!(!covers("tests/hidden/**", "tests/public.rs"));
        assert_eq!(literal_prefix("tests/hidden/**"), "tests/hidden");
    }

    /// A task's merge into the run branch and the run's onto your branch each say their own
    /// commit, so a closing answer never gives the task's as the one your branch moved to.
    #[test]
    fn a_run_merged_onto_your_branch_says_the_commit_it_moved_to() {
        let merge = |seq: u64, kind: MergeEvent| RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Merge(kind),
        };
        let mut folder = Folder::default();
        folder.push(&merge(
            1,
            MergeEvent::Merged {
                task: TaskId::new("task-001").unwrap(),
                sha: "e9ae4e0ab7ebe8c577c2e13ab89dc040567712d2".to_owned(),
            },
        ));
        folder.push(&merge(
            2,
            MergeEvent::RunMerged {
                target: "main".to_owned(),
                sha: "5b826b9acc92769b9ea7681ada95e94b052a1adb".to_owned(),
                fast_forward: true,
            },
        ));
        let recent = folder.summary(&Viewer::User).recent;
        assert_eq!(
            recent,
            [
                "task-001 merged into the run branch",
                "Merged into main as 5b826b9a"
            ]
        );

        // Only written into your files, your branch kept its commit.
        let mut folder = Folder::default();
        folder.push(&merge(
            1,
            MergeEvent::RunMerged {
                target: "main".to_owned(),
                sha: "5b826b9acc92769b9ea7681ada95e94b052a1adb".to_owned(),
                fast_forward: false,
            },
        ));
        assert_eq!(folder.summary(&Viewer::User).recent, ["Merged into main"]);
    }

    #[test]
    fn and_list_joins_like_a_sentence() {
        let items = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(and_list(&items), "a, b and c");
        assert_eq!(and_list(&items[..1]), "a");
    }
}
