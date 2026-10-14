use std::collections::{BTreeMap, HashMap};

use argonauts_core::events::checkpoint::CheckpointEvent;
use argonauts_core::events::run::{ParkReason, RunLifecycle, RunNamed, RunOutcome};
use argonauts_core::events::session::{SessionEvent, VendorWorkflow};
use argonauts_core::events::workflow::{StepEnd, WorkflowDeclared, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{CheckpointId, SessionKey, WorkerRef, WorkflowId};
use argonauts_core::views::{
    NestedWorkflowRow, PhaseCount, PhaseRow, RowStatus, StatusKind, StatusLine, WorkflowRow,
    WorkflowView, is_workflow_agent, vendor_workflow_id,
};
use argonauts_core::workflow::{StepRef, Workflow};
use argonauts_core::{AgentEvent, RunId, Spend};
use time::OffsetDateTime;

use super::sessions::{RunFacts, add_spend};

#[cfg(test)]
mod crew_tests;
mod inner;
mod reported;
mod tries;

use reported::StepWorkflow;

/// Nesting stops at depth 2; a deeper chain can only come from a damaged log.
const MAX_DEPTH: usize = 4;

/// The run's own workflow when `workflow` is `None`, else that nested one with its breadcrumb,
/// or a workflow an agent's own CLI runs. Before the lead picks the run's own workflow, `None`
/// gives the one that runs first, the goal's alignment. `None` when the events hold no such
/// workflow.
pub(crate) fn workflow_view(
    events: &[RunEvent],
    run: &RunId,
    workflow: Option<&WorkflowId>,
) -> Option<WorkflowView> {
    view_from(&WorkflowLog::of(events), events, run, workflow)
}

/// The view of `workflow` as `log` has the run.
fn view_from(
    log: &WorkflowLog,
    events: &[RunEvent],
    run: &RunId,
    workflow: Option<&WorkflowId>,
) -> Option<WorkflowView> {
    if let Some(id) = workflow
        && let Some(mut view) = log.vendor_view(run, id)
    {
        // A workflow its CLI runs can have agents on goals too, as the run's own has.
        view.goals = super::goal_feed::agent_goals(events, &view.phases);
        return Some(view);
    }
    let id = match workflow {
        Some(id) => id.clone(),
        None => log
            .order
            .iter()
            .find(|id| {
                log.workflows
                    .get(*id)
                    .is_some_and(|facts| facts.parent.is_none())
            })
            .or_else(|| log.order.first())?
            .clone(),
    };
    let facts = log.workflows.get(&id)?;
    // The top of the log, the run's own workflow or the goal alignment ahead of it, goes by
    // the run's name with no crumbs; a nested one names its parents.
    let top = log.is_top(facts);
    let breadcrumb = if top { Vec::new() } else { log.breadcrumb(&id) };
    let built = log.build(&id, 0)?;
    // The run's own workflow goes by the run's name, the one its panel row shows, and never
    // by the template it started from.
    let (name, description) = match (&log.named, top) {
        (Some(named), true) => (named.name.clone(), named.description.clone()),
        _ => match (log.reported_name(&id), log.declared.get(&id)) {
            // A workflow one of its steps runs says what it is better than its own phases do.
            (Some(reported), _) => reported,
            (None, Some(declared)) => (declared.name.clone(), declared.description.clone()),
            (None, None) => (
                facts.definition.name.clone(),
                facts.definition.description.clone(),
            ),
        },
    };
    // `orchestrator workflow`, after the run's own name, when it started from one.
    let notes = log
        .named
        .as_ref()
        .filter(|_| top)
        .and_then(|named| named.based_on.as_deref())
        .map(str::trim)
        .filter(|based_on| !based_on.is_empty() && *based_on != name.trim())
        .map(|based_on| vec![format!("{based_on} workflow")])
        .unwrap_or_default();
    // The steps of yours the workflow itself says it holds. Those every workflow must hold
    // are marked by whoever answers with the view.
    let required = log
        .declared
        .get(&id)
        .map(|declared| declared.required.clone())
        .unwrap_or_default();
    let mut status_lines = log.status_lines(&id, top);
    let goal = top.then(|| super::goal::goal_view(events, run)).flatten();
    if let Some(goal) = &goal {
        status_lines.insert(0, goal_line(goal));
    }
    // A goal is a condition around the workflow: the view shows one once the run has taken it.
    let style = match goal {
        Some(_) => Some(argonauts_core::settings::workflows::RunStyle::Goal),
        None => log.named.as_ref().filter(|_| top).map(|named| named.style),
    };
    Some(WorkflowView {
        run: run.clone(),
        status_lines,
        style,
        id,
        name,
        description,
        breadcrumb,
        agents_done: built.agents_done,
        agents_total: built.agents_total,
        elapsed_ms: built.elapsed_ms,
        spent: built.spent,
        goals: super::goal_feed::agent_goals(events, &built.phases),
        phases: built.phases,
        notes,
        required,
    })
}

/// The run's workflow, or the nested one named, as it is now, with the name it's saved under:
/// the run's own name for the run's workflow, made safe for a file name.
pub(crate) fn saved_workflow(
    events: &[RunEvent],
    workflow: Option<&WorkflowId>,
) -> Option<(String, Workflow)> {
    let log = WorkflowLog::of(events);
    let (id, facts) = match workflow {
        Some(id) => (id, log.workflows.get(id)?),
        None => log
            .workflows
            .iter()
            .find(|(_, facts)| facts.parent.is_none())?,
    };
    let name = match (&log.named, facts.parent.is_none()) {
        (Some(named), true) => named.name.clone(),
        _ => log
            .declared
            .get(id)
            .map_or_else(|| facts.definition.name.clone(), |d| d.name.clone()),
    };
    let safe: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_matches('-').to_owned();
    let safe = if safe.is_empty() {
        "workflow".to_owned()
    } else {
        safe
    };
    Some((safe, facts.definition.clone()))
}

/// `Goal · 3 of 4 met · round 2 · not yet: median([]) raises ValueError`, the line the workflow
/// view keeps under its description while a goal goes round the run.
fn goal_line(goal: &argonauts_core::views::GoalView) -> StatusLine {
    use argonauts_core::events::goal::CriterionState;
    let met = goal
        .criteria
        .iter()
        .filter(|row| row.state == Some(CriterionState::Met))
        .count();
    let mut text = format!("Goal · {met} of {} met", goal.criteria.len());
    if let Some(attempt) = goal.attempts.last() {
        text.push_str(&format!(" · round {}", attempt.n));
        if let Some(max) = goal.max_rounds {
            text.push_str(&format!(" of up to {max}"));
        }
    }
    if let Some(unmet) = goal
        .criteria
        .iter()
        .find(|row| row.state.is_some() && row.state != Some(CriterionState::Met))
    {
        text.push_str(&format!(" · not yet: {}", unmet.criterion.text.trim()));
    }
    StatusLine {
        kind: StatusKind::Goal,
        text,
    }
}

/// Every workflow the agents' own CLIs run in this run, newest state of each, with the session
/// that runs it, in the order they started.
fn vendor_workflows(events: &[RunEvent]) -> Vec<(SessionKey, VendorWorkflow)> {
    let mut found: Vec<(SessionKey, VendorWorkflow)> = Vec::new();
    for event in events {
        let Some(session) = &event.session else {
            continue;
        };
        match &event.kind {
            RunEventKind::Agent(AgentEvent::Workflow { workflow }) => {
                let mut workflow = (**workflow).clone();
                let seen = epoch_ms(event.ts);
                match found.iter_mut().find(|(_, known)| known.id == workflow.id) {
                    Some((_, known)) => {
                        workflow.started_at_ms = known.started_at_ms.or(seen);
                        *known = workflow;
                    }
                    None => {
                        workflow.started_at_ms = seen;
                        found.push((session.clone(), workflow));
                    }
                }
            }
            // A workflow its CLI ran ends with it, whether or not the stream said so.
            RunEventKind::Session(SessionEvent::Ended(_)) => {
                for (_, workflow) in found.iter_mut().filter(|(key, _)| key == session) {
                    workflow.end_with_session("its session ended before the workflow did");
                }
            }
            _ => {}
        }
    }
    found
}

fn epoch_ms(at: OffsetDateTime) -> Option<u64> {
    u64::try_from((at - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok()
}

struct WorkflowFacts {
    /// The latest definition, after any approved change.
    definition: Workflow,
    parent: Option<StepRef>,
    started_at: OffsetDateTime,
}

/// A step's latest round: still running, or how it ended and when.
#[derive(Clone, Copy)]
enum StepProgress {
    Running,
    Ended(StepEnd, OffsetDateTime),
}

/// Where a declared phase is, as the workflow reported it.
#[derive(Clone)]
enum PhaseState {
    Started,
    Ended(StepEnd),
    Skipped,
}

struct WorkflowLog {
    order: Vec<WorkflowId>,
    workflows: BTreeMap<WorkflowId, WorkflowFacts>,
    steps: HashMap<StepRef, StepProgress>,
    /// The latest declaration of each workflow: the phases it says it has.
    declared: HashMap<WorkflowId, WorkflowDeclared>,
    phases: HashMap<(WorkflowId, u32), PhaseState>,
    named: Option<RunNamed>,
    /// The latest note, and the workflow it's about when it's about one. A skipped phase's
    /// reason is one, since the phase leaves the list.
    note: Option<(Option<WorkflowId>, String)>,
    /// Open checkpoints that wait for you, by title, oldest first.
    asking: Vec<(CheckpointId, String)>,
    /// Every open checkpoint, by title, oldest first: one auto couldn't settle, or any the run
    /// parked on, waits for you too.
    open: Vec<(CheckpointId, String)>,
    finished: Option<(RunOutcome, Option<String>)>,
    vendors: Vec<(SessionKey, VendorWorkflow)>,
    /// The workflows the runner runs as steps, as each was last reported.
    reported: Vec<StepWorkflow>,
    sessions: RunFacts,
}

/// A workflow's rows and totals, counted through every level below it.
struct Built {
    phases: Vec<PhaseRow>,
    agents_done: u32,
    agents_total: u32,
    elapsed_ms: u64,
    spent: Spend,
    workers: Vec<WorkerRef>,
    status: RowStatus,
}

impl WorkflowLog {
    fn of(events: &[RunEvent]) -> Self {
        let mut log = Self {
            order: Vec::new(),
            workflows: BTreeMap::new(),
            steps: HashMap::new(),
            declared: HashMap::new(),
            phases: HashMap::new(),
            named: None,
            note: None,
            asking: Vec::new(),
            open: Vec::new(),
            finished: None,
            vendors: vendor_workflows(events),
            reported: Vec::new(),
            sessions: RunFacts::of(events),
        };
        for event in events {
            match &event.kind {
                RunEventKind::Workflow(workflow) => log.note_workflow(workflow, event.ts),
                RunEventKind::Run(RunLifecycle::Named(named)) => log.named = Some(named.clone()),
                RunEventKind::Run(RunLifecycle::Finished { outcome, detail }) => {
                    log.finished = Some((*outcome, detail.clone()));
                }
                RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                    log.open.push((opened.id.clone(), opened.title.clone()));
                    if opened.needs_you {
                        log.ask(&opened.id);
                    }
                }
                // Auto couldn't settle it, so it's yours.
                RunEventKind::Checkpoint(CheckpointEvent::Checked(check))
                    if !check.violations.is_empty() =>
                {
                    log.ask(&check.id);
                }
                RunEventKind::Run(RunLifecycle::Parked {
                    reason: ParkReason::Checkpoint,
                    waiting_on,
                }) => {
                    let waiting: Vec<CheckpointId> = if waiting_on.is_empty() {
                        log.open.iter().map(|(id, _)| id.clone()).collect()
                    } else {
                        waiting_on.clone()
                    };
                    for id in &waiting {
                        log.ask(id);
                    }
                }
                RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                    log.asking.retain(|(id, _)| *id != decided.id);
                    log.open.retain(|(id, _)| *id != decided.id);
                }
                RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => {
                    log.asking.retain(|(open, _)| open != id);
                    log.open.retain(|(open, _)| open != id);
                }
                _ => {}
            }
        }
        log
    }

    /// The open checkpoint `id` waits for you, once.
    fn ask(&mut self, id: &CheckpointId) {
        if self.asking.iter().any(|(asking, _)| asking == id) {
            return;
        }
        if let Some(card) = self.open.iter().find(|(open, _)| open == id) {
            self.asking.push(card.clone());
        }
    }

    fn note_workflow(&mut self, workflow: &WorkflowEvent, at: OffsetDateTime) {
        match workflow {
            WorkflowEvent::Started(started) => {
                self.order.push(started.id.clone());
                self.workflows.insert(
                    started.id.clone(),
                    WorkflowFacts {
                        definition: started.workflow.clone(),
                        parent: started.parent.clone(),
                        started_at: at,
                    },
                );
            }
            WorkflowEvent::Changed(changed) => {
                if let Some(facts) = self.workflows.get_mut(&changed.id) {
                    facts.definition = changed.workflow.clone();
                }
            }
            WorkflowEvent::StepStarted(step) => {
                self.steps.insert(step.step.clone(), StepProgress::Running);
            }
            WorkflowEvent::StepFinished(step) => {
                self.steps
                    .insert(step.step.clone(), StepProgress::Ended(step.end, at));
            }
            WorkflowEvent::Declared(declared) => {
                self.declared
                    .insert(declared.id.clone(), (**declared).clone());
            }
            WorkflowEvent::PhaseStarted(mark) => {
                self.phases
                    .insert((mark.id.clone(), mark.index), PhaseState::Started);
            }
            WorkflowEvent::PhaseEnded { phase, end } => {
                self.phases
                    .insert((phase.id.clone(), phase.index), PhaseState::Ended(*end));
            }
            WorkflowEvent::PhaseSkipped { phase, why } => {
                self.phases
                    .insert((phase.id.clone(), phase.index), PhaseState::Skipped);
                let why = why.trim();
                let text = if why.is_empty() {
                    format!("{} skipped", phase.title.trim())
                } else {
                    format!("{} skipped · {why}", phase.title.trim())
                };
                self.note = Some((Some(phase.id.clone()), text));
            }
            WorkflowEvent::PhaseAdded { .. } => {}
            WorkflowEvent::Reported(report) => {
                let seats = report
                    .sessions
                    .iter()
                    .map(|seat| (seat.agent, seat.session.clone()))
                    .collect();
                self.note_report(&report.step, &report.workflow, seats);
            }
            WorkflowEvent::Note(note) => {
                self.note = Some((note.workflow.clone(), note.text.trim().to_owned()));
            }
        }
    }

    /// Takes in what the runner reports of the workflow `step` runs: its name, its phases in
    /// the order it declares them, its agents, and in `seats` the session behind each agent
    /// that has one, by the agent's index. Each report is the whole state, so it replaces the
    /// last one of the same workflow.
    fn note_report(
        &mut self,
        step: &StepRef,
        workflow: &VendorWorkflow,
        seats: Vec<(u32, SessionKey)>,
    ) {
        let report = StepWorkflow {
            step: step.clone(),
            workflow: workflow.clone(),
            seats,
        };
        match self
            .reported
            .iter_mut()
            .find(|known| known.workflow.id == workflow.id)
        {
            Some(known) => *known = report,
            None => self.reported.push(report),
        }
    }

    /// The name and description of the workflow a step of `id` runs, when it gave a name.
    fn reported_name(&self, id: &WorkflowId) -> Option<(String, String)> {
        let report = self
            .reported
            .iter()
            .find(|report| report.step.workflow == *id)?;
        let name = report.workflow.name.trim();
        (!name.is_empty()).then(|| (name.to_owned(), report.workflow.description.clone()))
    }

    /// A workflow an agent's own CLI runs, by the id the views give it.
    fn vendor_view(&self, run: &RunId, id: &WorkflowId) -> Option<WorkflowView> {
        let (host, workflow) = self
            .vendors
            .iter()
            .find(|(_, workflow)| vendor_workflow_id(&workflow.id) == *id)?;
        let host = self.sessions.agent_row(self.sessions.session(host)?);
        let now = self.sessions.last_ts.and_then(epoch_ms);
        let mut view = WorkflowView::of_vendor(run, &host, workflow, now);
        super::asked::answered_by_the_crew(&mut view, &self.sessions);
        Some(view)
    }

    /// What the view says under its header: what waits for you, else how the run ended, else
    /// the latest note about this workflow.
    fn status_lines(&self, id: &WorkflowId, top: bool) -> Vec<StatusLine> {
        let line = |kind: StatusKind, text: String| StatusLine { kind, text };
        if self.finished.is_none()
            && let Some((_, title)) = self.asking.last()
        {
            let more = match self.asking.len() {
                1 => String::new(),
                n => format!(" and {} more", n - 1),
            };
            return vec![line(
                StatusKind::NeedsYou,
                format!(
                    "Needs you · {}{more} · the question is in the chat, esc goes to it",
                    title.trim()
                ),
            )];
        }
        if let Some((outcome, detail)) = &self.finished
            && top
        {
            let how = match outcome {
                RunOutcome::Merged => "Merged",
                RunOutcome::PullRequest => "A pull request is open",
                RunOutcome::BranchLeft => "The branch was kept",
                RunOutcome::Abandoned => "Abandoned",
                RunOutcome::Stopped => "Stopped",
                RunOutcome::Failed => "Failed",
            };
            let text = match detail.as_deref().map(str::trim) {
                Some(detail) if !detail.is_empty() => format!("{how} · {detail}"),
                _ => how.to_owned(),
            };
            return vec![line(StatusKind::Ended, text)];
        }
        match &self.note {
            Some((about, text)) if about.as_ref().is_none_or(|about| about == id) => {
                vec![line(StatusKind::Note, text.clone())]
            }
            _ => Vec::new(),
        }
    }

    /// Whether `facts` is the top of what the log holds: the run's own workflow, or before the
    /// lead picks it, the goal's alignment that runs ahead of it.
    fn is_top(&self, facts: &WorkflowFacts) -> bool {
        facts
            .parent
            .as_ref()
            .is_none_or(|parent| !self.workflows.contains_key(&parent.workflow))
    }

    /// Names from the top of the log down to `id`, the top by the run's own name, never a
    /// template's.
    fn breadcrumb(&self, id: &WorkflowId) -> Vec<String> {
        let mut names = Vec::new();
        let mut at = Some(id.clone());
        while let Some(id) = at {
            let Some(facts) = self.workflows.get(&id) else {
                break;
            };
            let top = self.is_top(facts);
            let name = match (&self.named, top) {
                (Some(named), true) => named.name.clone(),
                _ => self
                    .declared
                    .get(&id)
                    .map_or_else(|| facts.definition.name.clone(), |d| d.name.clone()),
            };
            names.push(name);
            if names.len() > MAX_DEPTH || top {
                break;
            }
            at = facts.parent.as_ref().map(|parent| parent.workflow.clone());
        }
        names.reverse();
        names
    }

    /// Whether the workflow is over: the run finished, or the step that started a nested one
    /// ended.
    fn ended(&self, facts: &WorkflowFacts) -> bool {
        match &facts.parent {
            Some(parent) => matches!(self.steps.get(parent), Some(StepProgress::Ended(..))),
            None => self.sessions.finished_at.is_some(),
        }
    }

    fn build(&self, id: &WorkflowId, depth: usize) -> Option<Built> {
        let facts = self.workflows.get(id)?;
        let mut built = Built {
            phases: Vec::new(),
            agents_done: 0,
            agents_total: 0,
            elapsed_ms: self.elapsed(facts),
            spent: Spend::default(),
            workers: Vec::new(),
            status: RowStatus::Waiting,
        };
        let ended = self.ended(facts);
        let now_ms = self.sessions.last_ts.and_then(epoch_ms);
        // Once a workflow the runner runs has said what its phases are, they are the view's
        // phases, and the run's own phases around them leave it.
        let mut replaced = false;
        for (index, phase) in facts.definition.phases.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let mark = self.phases.get(&(id.clone(), index));
            // A skipped phase leaves the list, and its reason shows as the note.
            if matches!(mark, Some(PhaseState::Skipped)) {
                continue;
            }
            let report = self
                .reported
                .iter()
                .find(|report| report.runs_in(id, index));
            let counted = (built.agents_done, built.agents_total);
            let mut rows: Vec<(OffsetDateTime, WorkflowRow)> = Vec::new();
            // Only the agents the workflow ran are its rows: an estimate or the retro, which
            // no step started, isn't one.
            for session in &self.sessions.sessions {
                let started = &session.started;
                let here = started
                    .workflow_step
                    .as_ref()
                    .is_some_and(|step| &step.workflow == id && step.phase == index);
                if !here || !is_workflow_agent(started.step, started.workflow_step.as_ref()) {
                    continue;
                }
                let row = self.sessions.agent_row(session);
                add_spend(&mut built.spent, &row.spent);
                remember(&mut built.workers, &row.worker);
                // An agent of the workflow this phase's step runs is a row of that workflow's
                // own phase, below.
                if report.is_some_and(|report| report.holds(&row)) {
                    continue;
                }
                built.agents_total += 1;
                if row.status == RowStatus::Done {
                    built.agents_done += 1;
                }
                rows.push((session.started_at, WorkflowRow::Agent(row)));
            }
            if depth < MAX_DEPTH {
                for (child_id, child) in &self.workflows {
                    let placed_here = child
                        .parent
                        .as_ref()
                        .is_some_and(|parent| &parent.workflow == id && parent.phase == index);
                    let Some(nested) = placed_here
                        .then(|| self.build(child_id, depth + 1))
                        .flatten()
                    else {
                        continue;
                    };
                    built.agents_done += nested.agents_done;
                    built.agents_total += nested.agents_total;
                    add_spend(&mut built.spent, &nested.spent);
                    for worker in &nested.workers {
                        remember(&mut built.workers, worker);
                    }
                    let name = self
                        .declared
                        .get(child_id)
                        .map_or_else(|| child.definition.name.clone(), |d| d.name.clone());
                    let row = NestedWorkflowRow {
                        id: child_id.clone(),
                        name,
                        status: nested.status,
                        agents_done: nested.agents_done,
                        agents_total: nested.agents_total,
                        workers: nested.workers,
                        elapsed_ms: nested.elapsed_ms,
                        parent: None,
                        owner: None,
                    };
                    rows.push((child.started_at, WorkflowRow::Workflow(row)));
                }
            }
            rows.sort_by_key(|(started, _)| *started);
            let mut rows: Vec<WorkflowRow> = rows.into_iter().map(|(_, row)| row).collect();
            let reported = report.map(|report| report.rows(&self.sessions, phase.standard));
            if let Some(reported) = &reported {
                rows.extend(reported.loose.iter().cloned().map(WorkflowRow::Agent));
                built.agents_done += reported.agents_done;
                built.agents_total += reported.agents_total;
            }
            let (rows, folded) = tries::fold(rows, &self.sessions);
            let rows = inner::under_agents(rows, &self.vendors, now_ms);
            built.agents_total = built.agents_total.saturating_sub(folded.tries);
            built.agents_done = built.agents_done.saturating_sub(folded.done);
            let steps: Vec<Option<StepProgress>> = (0..phase.steps.len())
                .map(|step| {
                    let step = u32::try_from(step).unwrap_or(u32::MAX);
                    self.steps
                        .get(&StepRef {
                            workflow: id.clone(),
                            phase: index,
                            path: vec![step],
                        })
                        .copied()
                })
                .collect();
            let ran = mark.is_some() || !rows.is_empty() || steps.iter().any(Option::is_some);
            // Once the workflow is over, a phase that never ran leaves the list.
            if ended && !ran {
                continue;
            }
            // The run's merge after them shows as the view's status line, and stays a phase
            // only for an agent that works in it.
            if replaced && rows.is_empty() {
                continue;
            }
            // The phases of the workflow its step runs stand in for this one. What this one
            // holds itself, such as the agent that wrote the steps, goes first in the first of
            // them, and only the workflow's own agents count.
            if let Some(reported) = reported.filter(|reported| !reported.phases.is_empty()) {
                replaced = true;
                built.agents_done = counted.0 + reported.agents_done;
                built.agents_total = counted.1 + reported.agents_total;
                let mut phases = reported.phases;
                if let Some(first) = phases.first_mut() {
                    first.rows.splice(0..0, rows);
                }
                for phase in &mut phases {
                    let (rows, folded) =
                        tries::fold(std::mem::take(&mut phase.rows), &self.sessions);
                    phase.rows = inner::under_agents(rows, &self.vendors, now_ms);
                    if folded.tries > 0 {
                        built.agents_total = built.agents_total.saturating_sub(folded.tries);
                        built.agents_done = built.agents_done.saturating_sub(folded.done);
                        phase.count = phase.count.and_then(|count| {
                            PhaseCount::of(
                                count.done.saturating_sub(folded.done),
                                count.total.saturating_sub(folded.tries),
                            )
                        });
                    }
                }
                built.phases.extend(phases);
                continue;
            }
            let done = rows
                .iter()
                .filter(|row| row_status(row) == RowStatus::Done)
                .count();
            // Done of the agents started so far, and nothing before the first one, so a phase
            // that hasn't started never reads 0/0.
            let count = PhaseCount::of(
                u32::try_from(done).unwrap_or(u32::MAX),
                u32::try_from(rows.len()).unwrap_or(u32::MAX),
            );
            let status = match mark {
                Some(PhaseState::Ended(StepEnd::Failed)) => RowStatus::Failed,
                Some(PhaseState::Ended(_)) if !has_status(&rows, RowStatus::NeedsYou) => {
                    RowStatus::Done
                }
                _ => phase_status(&rows, &steps),
            };
            let title = self
                .declared
                .get(id)
                .and_then(|declared| declared.phases.iter().find(|p| p.index == index));
            built.phases.push(PhaseRow {
                name: title.map_or_else(|| phase.name.clone(), |title| title.title.clone()),
                detail: title.map_or_else(|| phase.detail.clone(), |title| title.detail.clone()),
                standard: phase.standard,
                status,
                attempt: None,
                count,
                rows,
            });
        }
        built.status = overall(&built.phases);
        Some(built)
    }

    /// Until the step that started a nested workflow ended, or the run finished, or the log's
    /// last event.
    fn elapsed(&self, facts: &WorkflowFacts) -> u64 {
        let ended = match &facts.parent {
            Some(parent) => match self.steps.get(parent) {
                Some(StepProgress::Ended(_, at)) => Some(*at),
                _ => None,
            },
            None => self.sessions.finished_at,
        };
        match ended {
            Some(ended) => {
                u64::try_from((ended - facts.started_at).whole_milliseconds()).unwrap_or(0)
            }
            None => self.sessions.since(facts.started_at),
        }
    }
}

fn remember(workers: &mut Vec<WorkerRef>, worker: &WorkerRef) {
    if !workers.contains(worker) {
        workers.push(worker.clone());
    }
}

fn row_status(row: &WorkflowRow) -> RowStatus {
    match row {
        WorkflowRow::Agent(agent) => agent.status,
        WorkflowRow::Workflow(nested) => nested.status,
        WorkflowRow::Command(command) => command.status,
    }
}

fn has_status(rows: &[WorkflowRow], status: RowStatus) -> bool {
    rows.iter().any(|row| row_status(row) == status)
}

fn phase_status(rows: &[WorkflowRow], steps: &[Option<StepProgress>]) -> RowStatus {
    let statuses: Vec<RowStatus> = rows.iter().map(row_status).collect();
    if statuses.contains(&RowStatus::NeedsYou) {
        return RowStatus::NeedsYou;
    }
    let step_running = steps
        .iter()
        .any(|step| matches!(step, Some(StepProgress::Running)));
    if statuses.contains(&RowStatus::Running) || step_running {
        return RowStatus::Running;
    }
    let started = steps.iter().filter(|step| step.is_some()).count();
    if started == 0 && rows.is_empty() {
        return RowStatus::Waiting;
    }
    if started < steps.len() {
        return RowStatus::Running;
    }
    let step_failed = steps
        .iter()
        .any(|step| matches!(step, Some(StepProgress::Ended(StepEnd::Failed, _))));
    let all_rows_failed = !statuses.is_empty() && statuses.iter().all(|s| *s == RowStatus::Failed);
    if step_failed || (steps.is_empty() && all_rows_failed) {
        RowStatus::Failed
    } else {
        RowStatus::Done
    }
}

fn overall(phases: &[PhaseRow]) -> RowStatus {
    let statuses: Vec<RowStatus> = phases.iter().map(|phase| phase.status).collect();
    if statuses.contains(&RowStatus::NeedsYou) {
        RowStatus::NeedsYou
    } else if statuses.contains(&RowStatus::Running) {
        RowStatus::Running
    } else if statuses.contains(&RowStatus::Failed) {
        RowStatus::Failed
    } else if !statuses.is_empty() && statuses.iter().all(|s| *s == RowStatus::Done) {
        RowStatus::Done
    } else if statuses.contains(&RowStatus::Done) {
        RowStatus::Running
    } else {
        RowStatus::Waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{event, run_id, session_ended, session_started, usage};
    use argonauts_core::events::session::{EndReason, SessionEvent};
    use argonauts_core::events::workflow::PhaseMark;
    use argonauts_core::events::workflow::{StepFinished, StepStarted, WorkflowStarted};
    use argonauts_core::names::{RoleName, SessionKey};
    use argonauts_core::settings::workflows::RunStyle;
    use argonauts_core::workflow::{StandardPhase, StepKind};

    fn definition(value: serde_json::Value) -> Workflow {
        serde_json::from_value(value).unwrap()
    }

    fn step(workflow: &WorkflowId, phase: u32, index: u32) -> StepRef {
        StepRef {
            workflow: workflow.clone(),
            phase,
            path: vec![index],
        }
    }

    fn started(seq: u64, id: &WorkflowId, parent: Option<StepRef>, workflow: Workflow) -> RunEvent {
        let started = WorkflowStarted {
            id: id.clone(),
            from: None,
            version: "v1".to_owned(),
            parent,
            workflow,
        };
        event(
            seq,
            RunEventKind::Workflow(WorkflowEvent::Started(Box::new(started))),
        )
    }

    fn step_started(seq: u64, at: StepRef, kind: StepKind, standard: StandardPhase) -> RunEvent {
        let step = StepStarted {
            step: at,
            kind,
            standard,
            round: 1,
        };
        event(
            seq,
            RunEventKind::Workflow(WorkflowEvent::StepStarted(step)),
        )
    }

    fn step_finished(seq: u64, at: StepRef, kind: StepKind, standard: StandardPhase) -> RunEvent {
        let step = StepFinished {
            step: at,
            kind,
            standard,
            round: 1,
            end: StepEnd::Done,
            summary: None,
        };
        event(
            seq,
            RunEventKind::Workflow(WorkflowEvent::StepFinished(step)),
        )
    }

    fn placed(
        seq: u64,
        session: &SessionKey,
        role: RoleName,
        worker: &str,
        at: StepRef,
    ) -> RunEvent {
        let mut event = session_started(seq, session, None, role, worker);
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut event.kind {
            started.workflow_step = Some(at);
        }
        event
    }

    #[test]
    fn workflow_view_built_from_events() {
        let top = WorkflowId::numbered(1);
        let review = WorkflowId::numbered(2);
        let look = step(&top, 0, 0);
        let nested_step = step(&top, 0, 1);
        let reviewing = step(&review, 0, 0);
        let (s1, s2, s3) = (
            SessionKey::numbered(1),
            SessionKey::numbered(2),
            SessionKey::numbered(3),
        );
        let understand = StandardPhase::Understand;
        let events = vec![
            started(
                0,
                &top,
                None,
                definition(serde_json::json!({
                    "name": "council",
                    "description": "Agents check each other",
                    "phases": [
                        {"name": "Look", "standard": "understand",
                         "steps": [{"kind": "restate"}, {"kind": "workflow", "workflow": "review"}]},
                        {"name": "Build", "standard": "do", "steps": [{"kind": "build"}]}
                    ]
                })),
            ),
            step_started(1, look.clone(), StepKind::Restate, understand),
            placed(2, &s1, RoleName::Restater, "claude-code", look.clone()),
            placed(3, &s2, RoleName::Restater, "codex", look),
            usage(4, &s1, 800, 200, 0.1),
            session_ended(5, &s1, EndReason::Completed, 1_000),
            step_started(6, nested_step.clone(), StepKind::Workflow, understand),
            started(
                7,
                &review,
                Some(nested_step.clone()),
                definition(serde_json::json!({
                    "name": "review",
                    "phases": [{"name": "Check", "standard": "check", "steps": [{"kind": "review"}]}]
                })),
            ),
            step_started(8, reviewing.clone(), StepKind::Review, StandardPhase::Check),
            placed(
                9,
                &s3,
                RoleName::Reviewer,
                "antigravity-cli",
                reviewing.clone(),
            ),
            session_ended(10, &s3, EndReason::Completed, 2_000),
            step_finished(11, reviewing, StepKind::Review, StandardPhase::Check),
            step_finished(12, nested_step, StepKind::Workflow, understand),
        ];

        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(
            (view.name.as_str(), view.id.clone()),
            ("council", top.clone())
        );
        assert!(view.breadcrumb.is_empty());
        assert_eq!((view.agents_done, view.agents_total), (2, 3));
        assert_eq!(view.elapsed_ms, 12_000);
        assert_eq!(
            view.spent.input_tokens, 0,
            "ended sessions count their final spend"
        );

        let look = &view.phases[0];
        assert_eq!(look.status, RowStatus::Running);
        assert_eq!(look.count, PhaseCount::of(2, 3));
        let WorkflowRow::Workflow(nested) = &look.rows[2] else {
            panic!("the nested workflow is the last row");
        };
        assert_eq!(nested.name, "review");
        assert_eq!(nested.status, RowStatus::Done);
        assert_eq!((nested.agents_done, nested.agents_total), (1, 1));
        assert_eq!(nested.workers[0].worker, "antigravity-cli");
        assert_eq!(nested.elapsed_ms, 5_000);

        let build = &view.phases[1];
        assert_eq!(build.status, RowStatus::Waiting);
        assert_eq!((build.count, build.rows.len()), (None, 0));

        let inner = workflow_view(&events, &run_id(), Some(&review)).unwrap();
        assert_eq!(inner.breadcrumb, ["council", "review"]);
        assert_eq!(inner.phases[0].status, RowStatus::Done);
        assert!(workflow_view(&events, &run_id(), Some(&WorkflowId::numbered(9))).is_none());
        assert!(workflow_view(&[], &run_id(), None).is_none());
    }

    fn solo() -> Workflow {
        definition(serde_json::json!({
            "name": "solo",
            "description": "The main agent alone",
            "phases": [
                {"name": "Understand", "standard": "understand", "steps": [{"kind": "restate"}]},
                {"name": "Build", "standard": "do", "steps": [{"kind": "build"}]},
                {"name": "Review", "standard": "check", "steps": [{"kind": "review"}]},
                {"name": "Merge", "standard": "decide", "steps": [{"kind": "merge"}]}
            ]
        }))
    }

    fn workflow_event(seq: u64, workflow: WorkflowEvent) -> RunEvent {
        event(seq, RunEventKind::Workflow(workflow))
    }

    fn mark(id: &WorkflowId, index: u32, title: &str) -> PhaseMark {
        PhaseMark {
            id: id.clone(),
            index,
            title: title.to_owned(),
        }
    }

    /// Only the agents the workflow ran are rows: the retro, which no step
    /// started, isn't one, and a phase never counts what hasn't started.
    #[test]
    fn only_the_agents_the_workflow_ran_are_rows() {
        let top = WorkflowId::numbered(1);
        let (build, merge) = (step(&top, 1, 0), step(&top, 3, 0));
        let (owner, retro) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let events = vec![
            started(0, &top, None, solo()),
            step_started(1, build.clone(), StepKind::Build, StandardPhase::Do),
            placed(2, &owner, RoleName::Worker, "claude-code", build.clone()),
            session_ended(3, &owner, EndReason::Completed, 1_000),
            step_finished(4, build, StepKind::Build, StandardPhase::Do),
            step_started(5, merge.clone(), StepKind::Merge, StandardPhase::Decide),
            step_finished(6, merge, StepKind::Merge, StandardPhase::Decide),
            session_started(7, &retro, None, RoleName::Retro, "claude-code"),
            session_ended(8, &retro, EndReason::Completed, 1_000),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!((view.agents_done, view.agents_total), (1, 1));
        let shown: Vec<(&str, Option<PhaseCount>)> = view
            .phases
            .iter()
            .map(|phase| (phase.name.as_str(), phase.count))
            .collect();
        assert_eq!(
            shown,
            [
                ("Understand", None),
                ("Build", PhaseCount::of(1, 1)),
                ("Review", None),
                ("Merge", None)
            ]
        );
        assert!(view.phases.iter().all(|phase| phase.rows.len() <= 1));
    }

    /// The run's own name heads its workflow, never the template's. A skipped phase leaves the
    /// list with its reason as the note, and once the run ends, so does a phase that never ran.
    #[test]
    fn the_view_draws_only_what_the_workflow_declares() {
        let top = WorkflowId::numbered(1);
        let build = step(&top, 1, 0);
        let owner = SessionKey::numbered(1);
        let mut events = vec![
            started(0, &top, None, solo()),
            event(
                1,
                RunEventKind::Run(RunLifecycle::Named(RunNamed {
                    name: "fix-money-rounding".to_owned(),
                    description: "Round invoice totals half up, with tests".to_owned(),
                    style: RunStyle::Workflow,
                    based_on: Some("solo".to_owned()),
                    done_when: Vec::new(),
                })),
            ),
            workflow_event(
                2,
                WorkflowEvent::PhaseSkipped {
                    phase: mark(&top, 0, "Understand"),
                    why: "Your prompt is the goal".to_owned(),
                },
            ),
            workflow_event(3, WorkflowEvent::PhaseStarted(mark(&top, 1, "Build"))),
            step_started(4, build.clone(), StepKind::Build, StandardPhase::Do),
            placed(5, &owner, RoleName::Worker, "claude-code", build.clone()),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(view.name, "fix-money-rounding");
        assert_eq!(view.description, "Round invoice totals half up, with tests");
        let names: Vec<&str> = view.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Build", "Review", "Merge"]);
        assert_eq!(
            view.status_lines,
            [StatusLine {
                kind: StatusKind::Note,
                text: "Understand skipped · Your prompt is the goal".to_owned()
            }]
        );

        events.extend([
            session_ended(6, &owner, EndReason::Completed, 1_000),
            step_finished(7, build, StepKind::Build, StandardPhase::Do),
            workflow_event(
                8,
                WorkflowEvent::PhaseEnded {
                    phase: mark(&top, 1, "Build"),
                    end: StepEnd::Done,
                },
            ),
            event(
                9,
                RunEventKind::Run(RunLifecycle::Finished {
                    outcome: RunOutcome::Merged,
                    detail: None,
                }),
            ),
        ]);
        let view = workflow_view(&events, &run_id(), None).unwrap();
        let names: Vec<&str> = view.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Build"]);
        assert_eq!(view.phases[0].status, RowStatus::Done);
        assert_eq!(view.status_lines[0].kind, StatusKind::Ended);
    }

    /// A workflow Claude Code runs itself draws from its own reports, with its own names.
    #[test]
    fn a_vendors_own_workflow_is_drawn_from_its_reports() {
        use argonauts_core::events::session::{
            SubagentStatus, VendorAgent, VendorAgentState, VendorPhase, VendorWorkflow,
        };
        let host = SessionKey::numbered(1);
        let agent = |index: u32, label: &str, phase: u32, state: VendorAgentState| VendorAgent {
            index,
            label: label.to_owned(),
            phase: Some(phase),
            model: Some("claude-haiku-4-5-20251001".to_owned()),
            state,
            tokens: 10_311,
            tool_calls: 0,
            duration_ms: 3_266,
            started_at_ms: None,
            attempt: 1,
            prompt: None,
            result: None,
        };
        let workflow = VendorWorkflow {
            id: "w4d273qmn".to_owned(),
            call: None,
            name: "probe".to_owned(),
            description: "Two-phase workflow".to_owned(),
            phases: ["Ask", "Answer", "Wrap"]
                .iter()
                .zip(1..)
                .map(|(title, index)| VendorPhase {
                    index,
                    title: (*title).to_owned(),
                    detail: String::new(),
                })
                .collect(),
            agents: vec![
                agent(1, "ask:one", 1, VendorAgentState::Done),
                agent(2, "answer:one", 2, VendorAgentState::Running),
            ],
            status: SubagentStatus::Running,
            tokens: 20_626,
            tool_calls: 0,
            duration_ms: 6_661,
            summary: None,
            started_at_ms: None,
        };
        let mut events = vec![
            session_started(0, &host, None, RoleName::Worker, "claude-code"),
            event(
                1,
                RunEventKind::Agent(AgentEvent::Workflow {
                    workflow: Box::new(workflow.clone()),
                }),
            ),
        ];
        events[1].session = Some(host.clone());
        let id = vendor_workflow_id("w4d273qmn");
        let view = workflow_view(&events, &run_id(), Some(&id)).unwrap();
        assert_eq!((view.name.as_str(), view.id.clone()), ("probe", id.clone()));
        let shown: Vec<(&str, Option<PhaseCount>, RowStatus)> = view
            .phases
            .iter()
            .map(|phase| (phase.name.as_str(), phase.count, phase.status))
            .collect();
        assert_eq!(
            shown,
            [
                ("Ask", PhaseCount::of(1, 1), RowStatus::Done),
                ("Answer", PhaseCount::of(0, 1), RowStatus::Running),
                ("Wrap", None, RowStatus::Waiting),
            ]
        );
        let WorkflowRow::Agent(row) = &view.phases[1].rows[0] else {
            panic!("an agent row");
        };
        assert_eq!(row.label, "answer:one");
        assert_eq!(row.session, host);
        assert_eq!((view.agents_done, view.agents_total), (1, 2));
        assert_eq!(vendor_workflows(&events).len(), 1);
    }

    /// Before the lead picks the run's workflow, the goal's alignment runs nested under the
    /// workflow to come, and the view shows it rather than nothing.
    #[test]
    fn the_goal_alignment_shows_before_the_runs_own_workflow() {
        let own = WorkflowId::numbered(1);
        let alignment = WorkflowId::numbered(2);
        let goal_step = step(&own, 0, 0);
        let restating = step(&alignment, 0, 0);
        let goal = definition(serde_json::json!({
            "name": "goal",
            "phases": [{"name": "Goal", "standard": "understand", "steps": [{"kind": "restate"}]}]
        }));
        let mut events = vec![
            started(0, &alignment, Some(goal_step), goal),
            step_started(
                1,
                restating.clone(),
                StepKind::Restate,
                StandardPhase::Understand,
            ),
            placed(
                2,
                &SessionKey::numbered(1),
                RoleName::Restater,
                "claude-code",
                restating,
            ),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!((view.name.as_str(), &view.id), ("goal", &alignment));
        assert_eq!(view.phases[0].name, "Goal");
        assert_eq!(view.agents_total, 1);
        assert!(view.breadcrumb.is_empty());
        // Once the run has its name, the alignment goes by it, never by a template's.
        events.push(event(
            3,
            RunEventKind::Run(RunLifecycle::Named(RunNamed {
                name: "per-key-rate-limits".to_owned(),
                description: "Limit requests per API key".to_owned(),
                style: RunStyle::Workflow,
                based_on: Some("council".to_owned()),
                done_when: Vec::new(),
            })),
        ));
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(view.name, "per-key-rate-limits");
        assert!(view.breadcrumb.is_empty());

        events.push(started(
            4,
            &own,
            None,
            definition(serde_json::json!({
                "name": "council",
                "phases": [{"name": "Goal", "standard": "understand",
                            "steps": [{"kind": "workflow", "workflow": "goal"}]}]
            })),
        ));
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(
            (view.name.as_str(), &view.id),
            ("per-key-rate-limits", &own)
        );
        assert!(view.breadcrumb.is_empty());
    }

    /// A default crew run as the engine records it (`crates/crew/tests/engine_goals.rs`
    /// writes it with `ARGONAUTS_RECORD_FIXTURES=1`).
    const DEFAULT_CREW_RUN: &str = include_str!("../../tests/fixtures/runs/default-crew-run.jsonl");

    #[test]
    fn a_default_crew_runs_build_phase_holds_its_build_and_its_review() {
        let events: Vec<RunEvent> = DEFAULT_CREW_RUN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let run = events[0].run_id.clone();
        let view = workflow_view(&events, &run, None).unwrap();
        let labels = |phase: &PhaseRow| -> Vec<String> {
            phase
                .rows
                .iter()
                .filter_map(|row| match row {
                    WorkflowRow::Agent(agent) => Some(agent.label.clone()),
                    _ => None,
                })
                .collect()
        };
        let build = view
            .phases
            .iter()
            .find(|phase| phase.name == "Build")
            .unwrap();
        assert_eq!(labels(build), ["build:flag", "review:flag"]);
        assert_eq!(build.count, PhaseCount::of(2, 2));
        // The lead's own asks beside the workflow are notes, never rows or counts.
        let all: Vec<String> = view.phases.iter().flat_map(labels).collect();
        assert!(
            all.iter().all(|label| !label.starts_with("lead:")),
            "{all:?}"
        );
        assert_eq!(view.agents_total as usize, all.len(), "{all:?}");
        assert_eq!(view.agents_done, view.agents_total);
        assert!(all.iter().all(|label| !label.contains("task-")), "{all:?}");
    }

    #[test]
    fn a_card_auto_left_to_you_says_so_under_the_header() {
        let mut events: Vec<RunEvent> = DEFAULT_CREW_RUN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        // Cut the run where its first card opened, before auto settled it, and park there.
        let opened = events
            .iter()
            .position(|event| {
                matches!(
                    event.kind,
                    RunEventKind::Checkpoint(CheckpointEvent::Opened(_))
                )
            })
            .unwrap();
        events.truncate(opened + 1);
        let RunEventKind::Checkpoint(CheckpointEvent::Opened(card)) = &events[opened].kind else {
            unreachable!();
        };
        assert!(!card.needs_you, "auto may settle it when it opens");
        let (id, title) = (card.id.clone(), card.title.clone());
        let mut parked = events[opened].clone();
        parked.seq += 1;
        parked.kind = RunEventKind::Run(RunLifecycle::Parked {
            reason: ParkReason::Checkpoint,
            waiting_on: vec![id],
        });
        events.push(parked);
        let run = events[0].run_id.clone();
        let view = workflow_view(&events, &run, None).unwrap();
        let lines: Vec<&str> = view
            .status_lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(
            lines,
            [format!(
                "Needs you · {title} · the question is in the chat, esc goes to it"
            )]
        );
    }
    /// A workflow's phases are whatever it declares, in its order: none of them is a name the
    /// view knows, two share one name, and there are twelve.
    #[test]
    fn any_phases_a_workflow_declares_are_the_views_phases() {
        let names = [
            "Survey", "Outline", "Draft", "Critique", "Draft", "Critique", "Numbers", "Figures",
            "Proof", "Typeset", "Package", "Announce",
        ];
        let phases: Vec<serde_json::Value> = names
            .iter()
            .map(|name| {
                serde_json::json!({"name": name, "standard": "do", "steps": [{"kind": "build"}]})
            })
            .collect();
        let top = WorkflowId::numbered(1);
        let second_draft = step(&top, 4, 0);
        let writer = SessionKey::numbered(1);
        let events = vec![
            started(
                0,
                &top,
                None,
                definition(serde_json::json!({"name": "write-the-paper", "phases": phases})),
            ),
            step_started(1, second_draft.clone(), StepKind::Build, StandardPhase::Do),
            placed(2, &writer, RoleName::Worker, "codex", second_draft),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        let shown: Vec<&str> = view
            .phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect();
        assert_eq!(shown, names);
        // The agent sits in the second Draft, the phase it was placed in, not the first.
        let with_agents: Vec<usize> = view
            .phases
            .iter()
            .enumerate()
            .filter(|(_, phase)| !phase.rows.is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(with_agents, [4]);
        assert_eq!(view.phases[4].status, RowStatus::Running);
        assert_eq!(view.phases[2].status, RowStatus::Waiting);
    }

    #[test]
    fn a_workflow_of_one_phase_and_one_with_no_phase_yet_both_open() {
        let top = WorkflowId::numbered(1);
        let one = vec![started(
            0,
            &top,
            None,
            definition(serde_json::json!({
                "name": "translate",
                "phases": [{"name": "Translate the changelog", "standard": "do",
                            "steps": [{"kind": "build"}]}]
            })),
        )];
        let view = workflow_view(&one, &run_id(), None).unwrap();
        assert_eq!(view.phases.len(), 1);
        assert_eq!(view.phases[0].name, "Translate the changelog");
        assert_eq!(view.phases[0].count, None);

        let none = vec![started(
            0,
            &top,
            None,
            definition(serde_json::json!({"name": "warming-up", "phases": []})),
        )];
        let view = workflow_view(&none, &run_id(), None).unwrap();
        assert!(view.phases.is_empty());
        assert_eq!((view.agents_done, view.agents_total), (0, 0));
    }

    /// `orchestrator workflow` follows the run's own name, and a run named after the workflow
    /// it started from doesn't say so twice.
    #[test]
    fn the_workflow_a_run_started_from_is_noted_after_its_name() {
        let top = WorkflowId::numbered(1);
        let named = |name: &str| {
            event(
                1,
                RunEventKind::Run(RunLifecycle::Named(RunNamed {
                    name: name.to_owned(),
                    description: "Add due dates to tasks".to_owned(),
                    style: RunStyle::Workflow,
                    based_on: Some("orchestrator".to_owned()),
                    done_when: Vec::new(),
                })),
            )
        };
        let events = vec![started(0, &top, None, solo()), named("add-due-dates")];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(view.notes, ["orchestrator workflow"]);

        let events = vec![started(0, &top, None, solo()), named("orchestrator")];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert!(view.notes.is_empty());
    }

    /// The steps a workflow's own declaration says you require are the view's, so one asked
    /// for in a single request is marked as one kept in a file is.
    #[test]
    fn the_steps_a_declaration_requires_are_the_views() {
        let top = WorkflowId::numbered(1);
        let workflow = definition(serde_json::json!({
            "name": "switch-reads",
            "phases": [
                {"name": "Build", "standard": "do", "steps": [{"kind": "build"}]},
                {"name": "Benchmark", "standard": "do", "steps": [{"kind": "build"}]}
            ]
        }));
        let mut declared = WorkflowDeclared::of(&top, &workflow);
        declared.required = vec!["benchmark".to_owned()];
        let events = vec![
            started(0, &top, None, workflow),
            event(
                1,
                RunEventKind::Workflow(WorkflowEvent::Declared(Box::new(declared))),
            ),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(view.required, ["benchmark"]);
        assert!(!view.requires(&view.phases[0]));
        assert!(view.requires(&view.phases[1]));
    }
}
