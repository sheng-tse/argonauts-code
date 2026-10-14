use std::collections::{BTreeMap, HashMap};

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::{ApprovalEvent, ApprovalOpened, ApprovalPacket};
use argonauts_core::events::run::{RunLifecycle, RunMode, RunNamed, RunOutcome, WaitReason};
use argonauts_core::events::session::{CliWorkflowRun, SessionEvent};
use argonauts_core::events::task::MergeEvent;
use argonauts_core::events::workflow::{StepEnd, WorkflowDeclared, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::hidden_git::{MergedInto, PUT_BACK};
use argonauts_core::names::{ApprovalId, ModelRef, SessionKey, WorkflowId};
use argonauts_core::views::{
    AgentRow, AgentView, InnerAgent, NestedWorkflowRow, PhaseCount, PhaseRow, RowStatus,
    StatusKind, StatusLine, Tone, WorkflowRow, WorkflowState, WorkflowView, cli_workflow_run_id,
    is_workflow_agent,
};
use argonauts_core::workflow::{StandardPhase, StepRef, Workflow};
use argonauts_core::{AgentEvent, DiffStat, RunId, Spend};
use time::OffsetDateTime;

use super::sessions::{RunFacts, add_spend};

#[cfg(test)]
mod crew_tests;
mod inner;
mod own;
#[cfg(test)]
mod own_tests;
mod reported;
mod steps;
mod tries;

use reported::StepWorkflow;

/// Nesting stops at depth 2; a deeper chain can only come from a damaged log.
const MAX_DEPTH: usize = 4;

/// The run's own workflow when `workflow` is `None`, else that nested one with its breadcrumb,
/// or a workflow an agent's own CLI runs. Before the main agent picks the run's own workflow,
/// `None` gives the one that runs first, Understand. `None` when the events hold no such workflow.
pub(crate) fn workflow_view(
    events: &[RunEvent],
    run: &RunId,
    workflow: Option<&WorkflowId>,
) -> Option<WorkflowView> {
    view_from(&WorkflowLog::of(events), events, run, workflow)
}

/// One agent of a workflow its CLI runs itself, as the CLI last reported it. `None` when the
/// events hold no such workflow or agent.
pub(crate) fn inner_agent_view(
    events: &[RunEvent],
    run: &RunId,
    agent: &InnerAgent,
) -> Option<AgentView> {
    WorkflowLog::of(events).inner_agent_view(run, agent)
}

/// The view of `workflow` as `log` has the run.
fn view_from(
    log: &WorkflowLog,
    events: &[RunEvent],
    run: &RunId,
    workflow: Option<&WorkflowId>,
) -> Option<WorkflowView> {
    if let Some(id) = workflow
        && let Some(mut view) = log.agent_view(run, id)
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
    // The top of the log, the run's own workflow or Understand ahead of it, goes by
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
    // `orchestrator workflow`, after the run's own name: the workflow it started from, else
    // the one in use, as the run's Workflow item in the conversation says. Neither is said of
    // a run that goes by that name.
    let kind = |known: Option<&str>| {
        known
            .map(str::trim)
            .filter(|kind| !kind.is_empty() && *kind != name.trim())
            .map(str::to_lowercase)
    };
    let started_from = log
        .named
        .as_ref()
        .and_then(|named| named.based_on.as_deref());
    let notes = kind(started_from)
        .or_else(|| kind(log.in_use.as_deref()))
        .filter(|_| top)
        .map(|kind| vec![format!("{kind} workflow")])
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
    let mut view = WorkflowView {
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
        worker: None,
        state: if top {
            log.state()
        } else {
            WorkflowState::Running
        },
    };
    if top {
        own::as_the_runs(&mut view, log, run);
    }
    Some(view)
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
    if let Some(last_try) = goal.tries.last() {
        text.push_str(&format!(" · round {}", last_try.n));
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
    StatusLine::new(StatusKind::Goal, text)
}

/// Every workflow the agents' own CLIs run in this run, newest state of each, with the session
/// that runs it, in the order they started.
fn cli_workflow_runs(events: &[RunEvent]) -> Vec<(SessionKey, CliWorkflowRun)> {
    let mut found: Vec<(SessionKey, CliWorkflowRun)> = Vec::new();
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

/// How long until `at` by the clock on the wall, since a wait goes on while the run says
/// nothing.
fn ms_until(at: OffsetDateTime) -> u64 {
    u64::try_from((at - OffsetDateTime::now_utc()).whole_milliseconds()).unwrap_or(0)
}

/// An open approval, as the view's status line speaks of it. The question itself draws under
/// the view, so the line says only what waits.
#[derive(Debug, Clone)]
struct Asked {
    id: ApprovalId,
    /// `Claude Code is down · 2 tasks paused` for a CLI that is down, else the approval's
    /// title.
    says: String,
    /// When the CLI that is down is tested again.
    retried_at: Option<OffsetDateTime>,
}

impl Asked {
    fn of(opened: &ApprovalOpened) -> Self {
        let (says, retried_at) = match &opened.packet {
            ApprovalPacket::AgentDown(down) => {
                let cli = DisplayNames::default().cli(&down.agent_id.0);
                let paused = match down.paused.len() {
                    0 => String::new(),
                    1 => " · 1 task paused".to_owned(),
                    tasks => format!(" · {tasks} tasks paused"),
                };
                (format!("{cli} is down{paused}"), down.test_at)
            }
            _ => (opened.title.trim().to_owned(), None),
        };
        Self {
            id: opened.id.clone(),
            says,
            retried_at,
        }
    }
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
    /// Open approvals that wait for you, oldest first.
    asking: Vec<Asked>,
    /// Every open approval, oldest first: one auto couldn't settle, or any the run waiting on,
    /// waits for you too.
    open: Vec<Asked>,
    finished: Option<(RunOutcome, Option<String>)>,
    agent_ids: Vec<(SessionKey, CliWorkflowRun)>,
    /// The workflows the runner runs as steps, as each was last reported.
    reported: Vec<StepWorkflow>,
    sessions: RunFacts,
    /// The steps Argonauts Code ran itself at the run's end: its checks and the merge.
    own_steps: Vec<steps::Step>,
    /// Whether the run is paused and waits to be resumed.
    paused: bool,
    /// The branch the run merged onto and the commit, once it did.
    merged: Option<(String, String)>,
    /// The workflow your settings had in use when a crew's run started.
    in_use: Option<String>,
    /// The whole change the latest merge question put up: its files and lines.
    change: Option<DiffStat>,
}

/// A workflow's rows and totals, counted through every level below it.
struct Built {
    phases: Vec<PhaseRow>,
    agents_done: u32,
    agents_total: u32,
    elapsed_ms: u64,
    spent: Spend,
    workers: Vec<ModelRef>,
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
            agent_ids: cli_workflow_runs(events),
            reported: Vec::new(),
            sessions: RunFacts::of(events),
            own_steps: steps::of_the_merge(events),
            paused: false,
            merged: None,
            in_use: None,
            change: None,
        };
        for event in events {
            match &event.kind {
                RunEventKind::Workflow(workflow) => log.note_workflow(workflow, event.ts),
                RunEventKind::Run(RunLifecycle::Named(named)) => log.named = Some(named.clone()),
                // Captain's own steps are no workflow of yours, whatever your settings name.
                RunEventKind::Run(RunLifecycle::Started(started))
                    if started.mode == RunMode::Crew =>
                {
                    log.in_use = Some(started.workflow.clone());
                }
                RunEventKind::Run(RunLifecycle::Finished { outcome, detail }) => {
                    log.finished = Some((*outcome, detail.clone()));
                }
                RunEventKind::Run(RunLifecycle::Resumed(_)) => log.paused = false,
                RunEventKind::Merge(MergeEvent::RunMerged { target, sha, .. }) => {
                    log.merged = Some((target.clone(), sha.clone()));
                }
                RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                    if let ApprovalPacket::Merge(merge) = &opened.packet {
                        log.change = Some(merge.combined);
                    }
                    log.open.push(Asked::of(opened));
                    if opened.needs_you {
                        log.ask(&opened.id);
                    }
                }
                // Auto couldn't settle it, so it's yours.
                RunEventKind::Approval(ApprovalEvent::Checked(check))
                    if !check.violations.is_empty() =>
                {
                    log.ask(&check.id);
                }
                RunEventKind::Run(RunLifecycle::Waiting { reason, waiting_on }) => {
                    log.paused = *reason == WaitReason::Paused;
                    if *reason == WaitReason::Approval {
                        let waiting: Vec<ApprovalId> = if waiting_on.is_empty() {
                            log.open.iter().map(|open| open.id.clone()).collect()
                        } else {
                            waiting_on.clone()
                        };
                        for id in &waiting {
                            log.ask(id);
                        }
                    }
                }
                RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                    log.asking.retain(|asked| asked.id != decided.id);
                    log.open.retain(|open| open.id != decided.id);
                }
                RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                    log.asking.retain(|asked| asked.id != *id);
                    log.open.retain(|open| open.id != *id);
                }
                _ => {}
            }
        }
        log
    }

    /// The open approval `id` waits for you, once.
    fn ask(&mut self, id: &ApprovalId) {
        if self.asking.iter().any(|asked| asked.id == *id) {
            return;
        }
        if let Some(open) = self.open.iter().find(|open| open.id == *id) {
            self.asking.push(open.clone());
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
                    format!("{} skipped", phase.name.trim())
                } else {
                    format!("{} skipped · {why}", phase.name.trim())
                };
                self.note = Some((Some(phase.id.clone()), text));
            }
            WorkflowEvent::PhaseAdded { .. } => {}
            WorkflowEvent::Reported(report) => {
                let agent_sessions = report
                    .sessions
                    .iter()
                    .map(|known| (known.agent, known.session.clone()))
                    .collect();
                self.note_report(&report.step, &report.workflow, agent_sessions);
            }
            WorkflowEvent::Note(note) => {
                self.note = Some((note.workflow.clone(), note.text.trim().to_owned()));
            }
        }
    }

    /// Takes in what the runner reports of the workflow `step` runs: its name, its phases in
    /// the order it declares them, its agents, and in `agent_sessions` the session behind each
    /// agent that has one, by the agent's index. Each report is the whole state, so it replaces the
    /// last one of the same workflow.
    fn note_report(
        &mut self,
        step: &StepRef,
        workflow: &CliWorkflowRun,
        agent_sessions: Vec<(u32, SessionKey)>,
    ) {
        let report = StepWorkflow {
            step: step.clone(),
            workflow: workflow.clone(),
            agent_sessions,
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

    /// A workflow an agent's own CLI runs, by the id the views give it, with the row of the
    /// session running it.
    fn cli_workflow(&self, id: &WorkflowId) -> Option<(AgentRow, &CliWorkflowRun)> {
        let (host, workflow) = self
            .agent_ids
            .iter()
            .find(|(_, workflow)| cli_workflow_run_id(&workflow.id) == *id)?;
        let host = self.sessions.agent_row(self.sessions.session(host)?);
        Some((host, workflow))
    }

    /// The view of a workflow an agent's own CLI runs.
    fn agent_view(&self, run: &RunId, id: &WorkflowId) -> Option<WorkflowView> {
        let (host, workflow) = self.cli_workflow(id)?;
        let now = self.sessions.last_ts.and_then(epoch_ms);
        let mut view = WorkflowView::of_agent(run, &host, workflow, now);
        super::asked::answered_by_the_crew(&mut view, &self.sessions);
        Some(view)
    }

    fn inner_agent_view(&self, run: &RunId, agent: &InnerAgent) -> Option<AgentView> {
        let (host, workflow) = self.cli_workflow(&agent.workflow)?;
        let now = self.sessions.last_ts.and_then(epoch_ms);
        AgentView::of_inner(run, &host, workflow, agent.index, now)
    }

    /// Where the run stands, for the header's totals.
    fn state(&self) -> WorkflowState {
        match &self.finished {
            Some((RunOutcome::Merged | RunOutcome::PullRequest | RunOutcome::BranchLeft, _)) => {
                WorkflowState::Done
            }
            Some((RunOutcome::Stopped | RunOutcome::Abandoned, _)) => WorkflowState::Stopped,
            Some((RunOutcome::Failed, _)) => WorkflowState::Failed,
            None if self.paused => WorkflowState::Paused,
            None => WorkflowState::Running,
        }
    }

    /// What the view says under its header: what waits for you, else how the run ended, else
    /// the latest note about this workflow. The words before the first ` · ` are the headline.
    fn status_lines(&self, id: &WorkflowId, top: bool) -> Vec<StatusLine> {
        if self.finished.is_none()
            && let Some(asked) = self.asking.last()
        {
            let more = match self.asking.len() {
                1 => String::new(),
                n => format!(" · and {} more", n - 1),
            };
            let line = StatusLine::new(StatusKind::NeedsYou, format!("{}{more}", asked.says));
            return vec![match asked.retried_at {
                Some(at) => line.counting("tried again in", ms_until(at)),
                None => line,
            }];
        }
        if let Some((outcome, detail)) = &self.finished
            && top
        {
            let (how, tone) = match outcome {
                RunOutcome::Merged => (self.merged_words(), Tone::Good),
                RunOutcome::PullRequest => ("A pull request is open".to_owned(), Tone::Good),
                RunOutcome::BranchLeft => ("The branch was kept".to_owned(), Tone::Good),
                RunOutcome::Abandoned => ("Abandoned".to_owned(), Tone::Bad),
                RunOutcome::Stopped => ("Stopped".to_owned(), Tone::Bad),
                RunOutcome::Failed => ("Failed".to_owned(), Tone::Bad),
            };
            let text = match detail.as_deref().map(str::trim) {
                Some(detail) if !detail.is_empty() => format!("{how} · {detail}"),
                _ => how,
            };
            // No commit of yours undoes it there, so the line says what does.
            let text = if *outcome == RunOutcome::Merged && self.written_into_your_folder() {
                format!("{text} · {PUT_BACK}")
            } else {
                text
            };
            return vec![StatusLine::new(StatusKind::Ended, text).toned(tone)];
        }
        match &self.note {
            Some((about, text)) if about.as_ref().is_none_or(|about| about == id) => {
                vec![StatusLine::new(StatusKind::Note, text.clone())]
            }
            _ => Vec::new(),
        }
    }

    /// `Merged onto main · 3f9c2a1 · 4 files +52 −6`, from the merge the run's own steps
    /// record and the change its merge question put up, or `Merged`. A folder without git has
    /// no commit of yours to name: `Written into your folder · 4 files +52 −6`.
    fn merged_words(&self) -> String {
        let Some((target, sha)) = &self.merged else {
            return "Merged".to_owned();
        };
        let into = MergedInto::of(target);
        let mut said = vec![into.heading()];
        if matches!(into, MergedInto::Branch(_)) {
            said.push(sha.chars().take(7).collect());
        }
        if let Some(change) = self.change.filter(|change| !change.is_empty()) {
            said.push(changed(&change));
        }
        said.join(" · ")
    }

    /// Whether the run's work was written into a folder without git.
    fn written_into_your_folder(&self) -> bool {
        self.merged
            .as_ref()
            .is_some_and(|(target, _)| MergedInto::of(target) == MergedInto::YourFolder)
    }

    /// Whether `facts` is the top of what the log holds: the run's own workflow, or before the
    /// main agent picks it, Understand, which runs ahead of it.
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
            // The sessions this phase's own step started, which a workflow that step runs
            // doesn't count among its agents.
            let mut own: Vec<SessionKey> = Vec::new();
            // Only the agents the workflow ran are its rows: an estimate or the run review, which
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
                own.push(row.session.clone());
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
                        worker: None,
                    };
                    rows.push((child.started_at, WorkflowRow::Workflow(row)));
                }
            }
            // The run's own steps are rows of the phase that merges, and its checks of the
            // phase that checks where the workflow has one. What one of them has its phase
            // say goes under the phase's rows.
            let mut said: Option<&'static str> = None;
            if facts.parent.is_none() {
                let holds = |checks: bool| {
                    let wanted = if checks {
                        StandardPhase::Check
                    } else {
                        StandardPhase::Decide
                    };
                    let has = |standard| {
                        facts
                            .definition
                            .phases
                            .iter()
                            .rposition(|phase| phase.standard == standard)
                    };
                    has(wanted).or_else(|| has(StandardPhase::Decide))
                };
                for step in &self.own_steps {
                    if holds(step.checks) == usize::try_from(index).ok() {
                        rows.push((step.at, step.row.clone()));
                        said = said.or(step.note);
                    }
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
            let rows = inner::under_agents(rows, &self.agent_ids, now_ms);
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
            // The phases of the workflow its step runs take the place of this one, and that
            // workflow's agents are the view's: the session that wrote its steps is none of
            // them, so it is no row here and no count, in the header or in any phase.
            if let Some(reported) = reported.filter(|reported| !reported.phases.is_empty()) {
                replaced = true;
                built.agents_done = counted.0 + reported.agents_done;
                built.agents_total = counted.1 + reported.agents_total;
                let mut phases = reported.phases;
                let wrote_the_steps = |row: &WorkflowRow| {
                    matches!(row, WorkflowRow::Agent(agent)
                        if agent.inner.is_none() && own.contains(&agent.session))
                };
                let mut rows = rows;
                rows.retain(|row| !wrote_the_steps(row));
                if let Some(first) = phases.first_mut() {
                    // An agent the workflow put in no phase of its own shows in its first, and
                    // counts there, so a phase never lists more agents than its count says.
                    let loose = || rows.iter().filter_map(agent_status);
                    let total = u32::try_from(loose().count()).unwrap_or(u32::MAX);
                    if total > 0 {
                        let done = loose().filter(|status| *status == RowStatus::Done).count();
                        let done = u32::try_from(done).unwrap_or(u32::MAX);
                        let (was_done, was_total) = first
                            .count
                            .map_or((0, 0), |count| (count.done, count.total));
                        first.count = PhaseCount::of(was_done + done, was_total + total);
                    }
                    first.rows.splice(0..0, rows);
                }
                for phase in &mut phases {
                    let (rows, folded) =
                        tries::fold(std::mem::take(&mut phase.rows), &self.sessions);
                    phase.rows = inner::under_agents(rows, &self.agent_ids, now_ms);
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
            // Done of the agents started so far, and nothing before the first one, so a phase
            // that hasn't started never reads 0/0. The run's own steps aren't agents.
            let agents = || {
                rows.iter()
                    .filter(|row| !matches!(row, WorkflowRow::Command(_)))
            };
            let done = agents()
                .filter(|row| row_status(row) == RowStatus::Done)
                .count();
            let count = PhaseCount::of(
                u32::try_from(done).unwrap_or(u32::MAX),
                u32::try_from(agents().count()).unwrap_or(u32::MAX),
            );
            let status = match mark {
                Some(PhaseState::Ended(StepEnd::Failed)) => RowStatus::Failed,
                Some(PhaseState::Ended(_)) if !has_status(&rows, RowStatus::NeedsYou) => {
                    RowStatus::Done
                }
                _ => phase_status(&rows, &steps),
            };
            let named = self
                .declared
                .get(id)
                .and_then(|declared| declared.phases.iter().find(|p| p.index == index));
            built.phases.push(PhaseRow {
                name: named.map_or_else(|| phase.name.clone(), |named| named.name.clone()),
                detail: named.map_or_else(|| phase.detail.clone(), |named| named.detail.clone()),
                standard: phase.standard,
                status,
                try_number: None,
                count,
                note: self.waits_on(&rows).or(said.map(str::to_owned)),
                rows,
            });
        }
        built.status = overall(&built.phases);
        Some(built)
    }

    /// What a phase's rows wait for: the work a reviewer among them sent back, while its
    /// worker is on the next round.
    fn waits_on(&self, rows: &[WorkflowRow]) -> Option<String> {
        rows.iter().find_map(|row| match row {
            WorkflowRow::Agent(agent) => {
                let reviewer = self.sessions.session(&agent.session)?;
                self.sessions.verdicts.waits_on(&self.sessions, reviewer)
            }
            _ => None,
        })
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

/// `4 files +52 −6`: what a merge changed.
fn changed(change: &DiffStat) -> String {
    let files = match change.files_changed {
        1 => "1 file".to_owned(),
        files => format!("{files} files"),
    };
    format!(
        "{files} +{} \u{2212}{}",
        change.insertions, change.deletions
    )
}

fn remember(workers: &mut Vec<ModelRef>, worker: &ModelRef) {
    if !workers.contains(worker) {
        workers.push(worker.clone());
    }
}

/// An agent row's status. `None` for a step Argonauts Code ran or a nested workflow.
fn agent_status(row: &WorkflowRow) -> Option<RowStatus> {
    match row {
        WorkflowRow::Agent(agent) => Some(agent.status),
        _ => None,
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
                         "steps": [{"kind": "read"}, {"kind": "workflow", "workflow": "review"}]},
                        {"name": "Build", "standard": "do", "steps": [{"kind": "build"}]}
                    ]
                })),
            ),
            step_started(1, look.clone(), StepKind::Read, understand),
            placed(2, &s1, RoleName::Reader, "claude-code", look.clone()),
            placed(3, &s2, RoleName::Reader, "codex", look),
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
        assert_eq!(nested.workers[0].agent, "antigravity-cli");
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

    fn captain() -> Workflow {
        definition(serde_json::json!({
            "name": "captain",
            "description": "The main agent alone",
            "phases": [
                {"name": "Understand", "standard": "understand", "steps": [{"kind": "read"}]},
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
            name: title.to_owned(),
        }
    }

    /// Only the agents the workflow ran are rows: the run review, which no step
    /// started, isn't one, and a phase never counts what hasn't started.
    #[test]
    fn only_the_agents_the_workflow_ran_are_rows() {
        let top = WorkflowId::numbered(1);
        let (build, merge) = (step(&top, 1, 0), step(&top, 3, 0));
        let (worker, run_review) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let events = vec![
            started(0, &top, None, captain()),
            step_started(1, build.clone(), StepKind::Build, StandardPhase::Do),
            placed(2, &worker, RoleName::Worker, "claude-code", build.clone()),
            session_ended(3, &worker, EndReason::Completed, 1_000),
            step_finished(4, build, StepKind::Build, StandardPhase::Do),
            step_started(5, merge.clone(), StepKind::Merge, StandardPhase::Decide),
            step_finished(6, merge, StepKind::Merge, StandardPhase::Decide),
            session_started(7, &run_review, None, RoleName::RunReview, "claude-code"),
            session_ended(8, &run_review, EndReason::Completed, 1_000),
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
        let worker = SessionKey::numbered(1);
        let mut events = vec![
            started(0, &top, None, captain()),
            event(
                1,
                RunEventKind::Run(RunLifecycle::Named(RunNamed {
                    name: "fix-money-rounding".to_owned(),
                    description: "Round invoice totals half up, with tests".to_owned(),
                    style: RunStyle::Workflow,
                    based_on: Some("captain".to_owned()),
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
            placed(5, &worker, RoleName::Worker, "claude-code", build.clone()),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(view.name, "fix-money-rounding");
        assert_eq!(view.description, "Round invoice totals half up, with tests");
        let names: Vec<&str> = view.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Build", "Review", "Merge"]);
        assert_eq!(
            view.status_lines,
            [StatusLine::new(
                StatusKind::Note,
                "Understand skipped · Your prompt is the goal"
            )]
        );

        events.extend([
            session_ended(6, &worker, EndReason::Completed, 1_000),
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
    fn a_agents_own_workflow_is_drawn_from_its_reports() {
        use argonauts_core::events::session::{
            CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentStatus,
        };
        let host = SessionKey::numbered(1);
        let agent = |index: u32, label: &str, phase: u32, state: CliAgentState| CliAgent {
            index,
            label: label.to_owned(),
            phase: Some(phase),
            model: Some("claude-haiku-4-5-20251001".to_owned()),
            state,
            tokens: 10_311,
            tool_calls: 0,
            duration_ms: 3_266,
            started_at_ms: None,
            try_number: 1,
            prompt: None,
            result: None,
            ..CliAgent::default()
        };
        let workflow = CliWorkflowRun {
            id: "w4d273qmn".to_owned(),
            call: None,
            name: "probe".to_owned(),
            description: "Two-phase workflow".to_owned(),
            phases: ["Ask", "Answer", "Wrap"]
                .iter()
                .zip(1..)
                .map(|(title, index)| CliPhase {
                    index,
                    title: (*title).to_owned(),
                    detail: String::new(),
                })
                .collect(),
            agents: vec![
                agent(1, "ask:one", 1, CliAgentState::Done),
                agent(2, "answer:one", 2, CliAgentState::Running),
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
        let id = cli_workflow_run_id("w4d273qmn");
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
        assert_eq!(cli_workflow_runs(&events).len(), 1);
    }

    /// A run whose one task is a session running Claude Code's own workflow: its phases are
    /// the run's, the session that plans first, the reviewer it asked in the row it answered,
    /// and no second run nested under the session.
    #[test]
    fn claude_codes_own_workflow_is_the_runs_steps() {
        use argonauts_core::events::session::{
            CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentStatus,
        };
        let top = WorkflowId::numbered(1);
        let script = step(&top, 0, 0);
        let host = SessionKey::numbered(1);
        let reviewer = SessionKey::numbered(2);
        let task = argonauts_core::TaskId::new("task-add-due-dates").unwrap();
        let agent = |index: u32, label: &str, phase: u32, state: CliAgentState| CliAgent {
            index,
            label: label.to_owned(),
            phase: Some(phase),
            model: Some("claude-haiku-4-5".to_owned()),
            state,
            tokens: 1_000,
            tool_calls: 0,
            duration_ms: 1_000,
            started_at_ms: None,
            try_number: 1,
            prompt: None,
            result: None,
            ..CliAgent::default()
        };
        let workflow = CliWorkflowRun {
            id: "wf_1".to_owned(),
            call: None,
            name: "add-due-dates".to_owned(),
            description: "Add due dates".to_owned(),
            phases: ["Build", "Review", "Final"]
                .iter()
                .zip(1..)
                .map(|(title, index)| CliPhase {
                    index,
                    title: (*title).to_owned(),
                    detail: String::new(),
                })
                .collect(),
            agents: vec![
                agent(1, "build:due-dates", 1, CliAgentState::Done),
                agent(2, "review:due-dates", 2, CliAgentState::Done),
                agent(3, "final:due-dates", 3, CliAgentState::Running),
            ],
            status: SubagentStatus::Running,
            tokens: 3_000,
            tool_calls: 0,
            duration_ms: 3_000,
            summary: None,
            started_at_ms: None,
        };
        let mut events = vec![
            started(
                0,
                &top,
                None,
                definition(serde_json::json!({
                    "name": "add-due-dates",
                    "phases": [{"name": "Work", "standard": "do", "steps": [{"kind": "script"}]}]
                })),
            ),
            step_started(1, script.clone(), StepKind::Script, StandardPhase::Do),
            placed(
                2,
                &host,
                RoleName::Worker,
                "claude-code/haiku",
                script.clone(),
            ),
            event(
                3,
                RunEventKind::Agent(AgentEvent::Workflow {
                    workflow: Box::new(workflow),
                }),
            ),
            placed(
                4,
                &reviewer,
                RoleName::Reviewer,
                "codex/gpt-5.6-luna",
                script,
            ),
        ];
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut events[2].kind {
            started.label = Some("build:add-due-dates".to_owned());
        }
        events[2].task_id = Some(task);
        events[3].session = Some(host.clone());
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut events[4].kind {
            started.label = Some("review:due-dates".to_owned());
        }
        let view = workflow_view(&events, &run_id(), None).unwrap();
        let shown: Vec<(&str, Vec<String>)> = view
            .phases
            .iter()
            .map(|phase| {
                let rows = phase
                    .rows
                    .iter()
                    .map(|row| match row {
                        WorkflowRow::Agent(agent) => {
                            format!("{} {}", agent.label, agent.worker.agent)
                        }
                        WorkflowRow::Workflow(nested) => format!("run {}", nested.name),
                        WorkflowRow::Command(_) => "command".to_owned(),
                    })
                    .collect();
                (phase.name.as_str(), rows)
            })
            .collect();
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert_eq!(shown[0].0, "Build");
        // The session that plans it is the box, not one of its steps.
        assert_eq!(
            shown[0].1,
            ["build:due-dates claude-code/haiku"],
            "{shown:?}"
        );
        assert_eq!(shown[1].1, ["review:due-dates codex/gpt-5.6-luna"]);
        assert_eq!(shown[2].1, ["final:due-dates claude-code/haiku"]);
        assert_eq!((view.agents_done, view.agents_total), (1, 3));
        let counts: Vec<Option<(u32, u32)>> = view
            .phases
            .iter()
            .map(|phase| phase.count.map(|count| (count.done, count.total)))
            .collect();
        assert_eq!(counts[0], Some((1, 1)), "{counts:?}");
        // Whose own workflow it is: the session's CLI, which the test names with its model.
        assert_eq!(view.worker.as_deref(), Some("claude-code/haiku"));
    }

    /// Before the main agent picks the run's workflow, Understand runs nested under the
    /// workflow to come, and the view shows it rather than nothing.
    #[test]
    fn understand_shows_before_the_runs_own_workflow() {
        let own = WorkflowId::numbered(1);
        let understand = WorkflowId::numbered(2);
        let loop_step = step(&own, 0, 0);
        let reading = step(&understand, 0, 0);
        let nested = definition(serde_json::json!({
            "name": "understanding",
            "phases": [{"name": "Understand", "standard": "understand", "steps": [{"kind": "read"}]}]
        }));
        let mut events = vec![
            started(0, &understand, Some(loop_step), nested),
            step_started(
                1,
                reading.clone(),
                StepKind::Read,
                StandardPhase::Understand,
            ),
            placed(
                2,
                &SessionKey::numbered(1),
                RoleName::Reader,
                "claude-code",
                reading,
            ),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(
            (view.name.as_str(), &view.id),
            ("understanding", &understand)
        );
        assert_eq!(view.phases[0].name, "Understand");
        assert_eq!(view.agents_total, 1);
        assert!(view.breadcrumb.is_empty());
        // Once the run has its name, Understand goes by it, never by a template's.
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
                "phases": [{"name": "Understand", "standard": "understand",
                            "steps": [{"kind": "workflow", "workflow": "understanding"}]}]
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
        // The main agent's own asks beside the workflow are notes, never rows or counts.
        let all: Vec<String> = view.phases.iter().flat_map(labels).collect();
        assert!(
            all.iter().all(|label| !label.starts_with("main:")),
            "{all:?}"
        );
        assert_eq!(view.agents_total as usize, all.len(), "{all:?}");
        assert_eq!(view.agents_done, view.agents_total);
        assert!(all.iter().all(|label| !label.contains("task-")), "{all:?}");
    }

    /// The run's own checks and its merge are rows of the phases that hold them, with the
    /// command or `git` where an agent's model would be, and they count as no agent. The
    /// agents' rows say what was decided, and the header says how the run ended.
    #[test]
    fn a_default_crew_runs_checks_and_merge_are_steps_of_its_last_phases() {
        use argonauts_core::views::CommandRow;

        let events: Vec<RunEvent> = DEFAULT_CREW_RUN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let run = events[0].run_id.clone();
        let view = workflow_view(&events, &run, None).unwrap();
        let phase = |name: &str| {
            view.phases
                .iter()
                .find(|phase| phase.name == name)
                .unwrap_or_else(|| panic!("no {name} phase"))
        };
        let steps = |phase: &PhaseRow| -> Vec<CommandRow> {
            phase
                .rows
                .iter()
                .filter_map(|row| match row {
                    WorkflowRow::Command(step) => Some(step.clone()),
                    _ => None,
                })
                .collect()
        };
        let said = |phase: &PhaseRow| -> Vec<(String, Option<String>)> {
            phase
                .rows
                .iter()
                .filter_map(|row| match row {
                    WorkflowRow::Agent(agent) => Some((
                        agent.label.clone(),
                        agent.note.as_ref().map(|note| note.text.clone()),
                    )),
                    _ => None,
                })
                .collect()
        };

        // The worker's checks passed and its reviewer approved.
        assert_eq!(
            said(phase("Build")),
            [
                ("build:flag".to_owned(), Some("passed".to_owned())),
                ("review:flag".to_owned(), Some("approved".to_owned())),
            ]
        );
        // The final check is a step of the phase that checks, beside the agent that checks
        // the goal, and only the agent counts.
        let check = phase("Check");
        let checked = steps(check);
        assert_eq!(checked.len(), 1, "{checked:?}");
        assert_eq!(checked[0].label, "final check");
        assert_eq!(checked[0].command, "true");
        assert_eq!(checked[0].result.as_deref(), Some("passed"));
        assert_eq!(checked[0].status, RowStatus::Done);
        assert_eq!(check.count, PhaseCount::of(1, 1));
        // The merge is git's, in the Merge phase, which no agent worked in.
        let merge = phase("Merge");
        let merged = steps(merge);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].label, "merge onto main");
        assert_eq!(merged[0].command, "git");
        assert_eq!(merged[0].result.as_deref(), Some("1 task · no conflicts"));
        assert_eq!(merge.count, None);
        assert_eq!(merge.status, RowStatus::Done);

        assert_eq!(view.state, WorkflowState::Done);
        let ended = view.status_lines.last().unwrap();
        assert_eq!(ended.kind, StatusKind::Ended);
        assert_eq!(ended.tone, Tone::Good);
        assert_eq!(ended.text, "Merged onto main · 5362b60");
    }

    /// The Merge phase's agents and its own steps, each as its label and what it says after
    /// it: an agent's note, or what ran a step and what came of it.
    fn merge_rows(view: &WorkflowView) -> Vec<(String, Option<String>)> {
        let merge = view
            .phases
            .iter()
            .find(|phase| phase.name == "Merge")
            .expect("a Merge phase");
        merge
            .rows
            .iter()
            .map(|row| match row {
                WorkflowRow::Agent(agent) => (
                    agent.label.clone(),
                    agent.note.as_ref().map(|note| note.text.clone()),
                ),
                WorkflowRow::Command(step) => {
                    let result = step.result.as_deref().unwrap_or_default();
                    let said = format!("{} · {result}", step.command);
                    (step.label.clone(), Some(said))
                }
                WorkflowRow::Workflow(nested) => (nested.name.clone(), None),
            })
            .collect()
    }

    /// The merger is asked only for the commit message, with the diff in its prompt and no
    /// tools. Its session is a row of the Merge phase under the label the crew gives it,
    /// ahead of the merge git ran, and says how it was asked while it is the phase's only
    /// agent. A merge that also needed a conflict settled leaves that to the rows.
    #[test]
    fn the_mergers_commit_message_is_a_row_of_the_merge_phase() {
        use argonauts_core::session_runner::Access;
        use argonauts_core::steps::StepName;
        use argonauts_crew::merge::merger::{COMMIT_MESSAGE, SETTLE_CONFLICT, fix_after_move};

        let recorded: Vec<RunEvent> = DEFAULT_CREW_RUN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let run = recorded[0].run_id.clone();
        let merge_step = recorded
            .iter()
            .find_map(|event| match &event.kind {
                RunEventKind::Workflow(WorkflowEvent::StepStarted(step))
                    if step.kind == StepKind::Merge =>
                {
                    Some(step.step.clone())
                }
                _ => None,
            })
            .expect("the run's Merge step");
        let merged = recorded
            .iter()
            .position(|event| {
                matches!(
                    event.kind,
                    RunEventKind::Merge(MergeEvent::RunMerged { .. })
                )
            })
            .expect("the run merged");
        // A session of the merger at the Merge step, just before git merged the run.
        let session_of = |key: &SessionKey, label: &str| {
            let at = recorded[merged - 1].ts;
            let mut started = session_started(0, key, None, RoleName::Worker, "claude-code");
            if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
                session.label = Some(label.to_owned());
                session.access = Access::NoTools;
                session.workflow_step = Some(merge_step.clone());
            }
            let mut ended = session_ended(0, key, EndReason::Completed, 19_000);
            for event in [&mut started, &mut ended] {
                event.run_id = run.clone();
                event.ts = at;
            }
            [started, ended]
        };
        let message = SessionKey::numbered(90);

        let mut clean = recorded.clone();
        clean.splice(merged..merged, session_of(&message, COMMIT_MESSAGE));
        let view = workflow_view(&clean, &run, None).unwrap();
        assert_eq!(
            merge_rows(&view),
            [
                (
                    "commit message".to_owned(),
                    Some("given the diff, no tools".to_owned())
                ),
                (
                    "merge onto main".to_owned(),
                    Some("git · 1 task · no conflicts".to_owned())
                ),
            ]
        );
        let merge = view.phases.iter().find(|phase| phase.name == "Merge");
        assert_eq!(merge.unwrap().count, PhaseCount::of(1, 1));

        let mut settled = recorded.clone();
        let both = session_of(&SessionKey::numbered(89), SETTLE_CONFLICT)
            .into_iter()
            .chain(session_of(&message, COMMIT_MESSAGE));
        settled.splice(merged..merged, both);
        let view = workflow_view(&settled, &run, None).unwrap();
        let rows = merge_rows(&view);
        assert_eq!(rows[0], ("settle conflict".to_owned(), None));
        assert_eq!(rows[1], ("commit message".to_owned(), None));

        // A session the merge needed says what it did once it ended with its report: the one
        // that settled a conflict in its own words, and the one that fixed what broke after
        // your branch came in by the files it changed.
        let reported = |key: &SessionKey, label: &str, step: Option<StepName>, did: &str| {
            let [mut started, mut ended] = session_of(key, label);
            if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
                session.step = step;
                session.access = Access::Build;
            }
            if let RunEventKind::Session(SessionEvent::Ended(end)) = &mut ended.kind {
                end.structured = Some(serde_json::json!({
                    "status": "done", "summary": did, "files_changed": ["src/cli.js"],
                    "tests_run": [], "concerns": []
                }));
            }
            [started, ended]
        };
        let settler = reported(
            &SessionKey::numbered(88),
            SETTLE_CONFLICT,
            Some(StepName::ResolveConflict),
            "kept both changes to the --due parsing",
        );
        let fixer = reported(
            &SessionKey::numbered(89),
            &fix_after_move("main"),
            None,
            "Made the date test read the new format.",
        );
        let mut moved = recorded.clone();
        let sessions = settler
            .into_iter()
            .chain(fixer)
            .chain(session_of(&message, COMMIT_MESSAGE));
        moved.splice(merged..merged, sessions);
        let rows = merge_rows(&workflow_view(&moved, &run, None).unwrap());
        assert_eq!(
            rows[0],
            (
                "settle conflict".to_owned(),
                Some("kept both changes to the --due parsing".to_owned())
            )
        );
        assert_eq!(
            rows[1],
            ("fix after main moved".to_owned(), Some("1 file".to_owned()))
        );
        assert_eq!(rows[2], ("commit message".to_owned(), None));
    }

    /// The line a merged run ends on says what the merge changed, as its merge question put
    /// it up: `Merged onto main · 3f9c2a1 · 4 files +52 −6`, and in a folder without git
    /// `Written into your folder · 4 files +52 −6 · /rewind puts it back`.
    #[test]
    fn a_merged_run_says_what_its_merge_changed() {
        use argonauts_core::hidden_git;

        let mut events: Vec<RunEvent> = DEFAULT_CREW_RUN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for event in &mut events {
            if let RunEventKind::Approval(ApprovalEvent::Opened(opened)) = &mut event.kind
                && let ApprovalPacket::Merge(merge) = &mut opened.packet
            {
                merge.combined = DiffStat {
                    files_changed: 4,
                    insertions: 52,
                    deletions: 6,
                };
            }
        }
        let run = events[0].run_id.clone();
        let ended = |events: &[RunEvent]| {
            let view = workflow_view(events, &run, None).unwrap();
            view.status_lines.last().unwrap().text.clone()
        };
        assert_eq!(
            ended(&events),
            "Merged onto main · 5362b60 · 4 files +52 \u{2212}6"
        );

        for event in &mut events {
            if let RunEventKind::Merge(MergeEvent::RunMerged { target, .. }) = &mut event.kind {
                hidden_git::BRANCH.clone_into(target);
            }
        }
        assert_eq!(
            ended(&events),
            "Written into your folder · 4 files +52 \u{2212}6 · /rewind puts it back"
        );
    }

    /// A crew's run that started from no learned workflow names the workflow in use after its
    /// own name, as its Workflow item in the conversation does. Captain's own steps are no
    /// workflow of yours, so its run names none.
    #[test]
    fn a_crews_run_names_the_workflow_in_use_after_its_name() {
        use crate::client::test_support::run_started;

        let top = WorkflowId::numbered(1);
        let begun = |mode: RunMode| {
            let mut event = run_started(0, "add per-key rate limiting", None);
            if let RunEventKind::Run(RunLifecycle::Started(started)) = &mut event.kind {
                started.mode = mode;
                started.workflow = "council".to_owned();
            }
            event
        };
        let named = event(
            2,
            RunEventKind::Run(RunLifecycle::Named(RunNamed {
                name: "per-key-rate-limiting".to_owned(),
                description: "Per-key rate limiting for the public API".to_owned(),
                style: RunStyle::Workflow,
                based_on: None,
                done_when: Vec::new(),
            })),
        );
        let notes = |mode: RunMode| {
            let events = vec![
                begun(mode),
                started(1, &top, None, captain()),
                named.clone(),
            ];
            workflow_view(&events, &run_id(), None).unwrap().notes
        };
        assert_eq!(notes(RunMode::Crew), ["council workflow"]);
        assert!(notes(RunMode::Captain).is_empty());
    }

    /// In a folder without git a run's merge names the hidden history's branch, which is
    /// never shown. The Merge phase's last step writes into your folder with the history
    /// kept for it where git would stand, and the view's ending says how to undo it.
    #[test]
    fn a_run_in_a_folder_without_git_ends_written_into_your_folder() {
        use argonauts_core::hidden_git;

        let mut events: Vec<RunEvent> = DEFAULT_CREW_RUN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for event in &mut events {
            if let RunEventKind::Merge(MergeEvent::RunMerged { target, .. }) = &mut event.kind {
                hidden_git::BRANCH.clone_into(target);
            }
        }
        let run = events[0].run_id.clone();
        let view = workflow_view(&events, &run, None).unwrap();
        assert_eq!(
            merge_rows(&view),
            [(
                "write into your folder".to_owned(),
                Some("history kept by Argonauts · 1 task".to_owned())
            )]
        );
        // No model wrote a commit message, and the phase says why none is among its steps.
        let merge = view.phases.iter().find(|phase| phase.name == "Merge");
        assert_eq!(
            merge.and_then(|phase| phase.note.as_deref()),
            Some("no model writes a commit message here: only /rewind reads the history")
        );
        let ended = view.status_lines.last().unwrap();
        assert_eq!(ended.kind, StatusKind::Ended);
        assert_eq!(
            ended.text,
            "Written into your folder · /rewind puts it back"
        );
        let shown = serde_json::to_string(&view).unwrap();
        assert!(!shown.contains(hidden_git::BRANCH), "{shown}");
    }

    /// A review that stays on its CLI with the next model: the run says who stopped and who
    /// it fell back to in one line about the workflow, which shows under the header while the
    /// phase's row is the fallback's, for the model the step asked and why it stopped.
    #[test]
    fn a_steps_fallback_is_said_under_the_header() {
        use argonauts_core::ErrorCategory;
        use argonauts_core::events::workflow::RunNote;

        let top = WorkflowId::numbered(1);
        let review = step(&top, 2, 0);
        let (first, second) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let opus: ModelRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let sonnet: ModelRef = "claude-code/claude-sonnet-5-5".parse().unwrap();
        let reviewing = |seq, key: &SessionKey, worker: &ModelRef, asked: Option<&ModelRef>| {
            let mut started = placed(seq, key, RoleName::Reviewer, "claude-code", review.clone());
            if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
                session.worker = worker.clone();
                session.label = Some("review:due-tests".to_owned());
                session.asked = asked.cloned();
            }
            started
        };
        let overloaded = EndReason::CliError {
            category: ErrorCategory::Overloaded,
            message: "Overloaded".to_owned(),
        };
        let said = "Opus 5.5 on Claude Code is overloaded · fell back to Sonnet 5.5 on Claude \
                    Code, as same_cli asks";
        let events = vec![
            started(0, &top, None, captain()),
            step_started(1, review.clone(), StepKind::Review, StandardPhase::Check),
            reviewing(2, &first, &opus, None),
            session_ended(3, &first, overloaded, 2_000),
            reviewing(4, &second, &sonnet, Some(&opus)),
            workflow_event(
                5,
                WorkflowEvent::Note(RunNote {
                    text: said.to_owned(),
                    workflow: Some(top.clone()),
                    left: false,
                }),
            ),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        let [line] = view.status_lines.as_slice() else {
            panic!("{:?}", view.status_lines);
        };
        assert_eq!((line.kind, line.text.as_str()), (StatusKind::Note, said));

        let phase = view
            .phases
            .iter()
            .find(|phase| phase.name == "Review")
            .expect("a Review phase");
        let [WorkflowRow::Agent(fallback)] = phase.rows.as_slice() else {
            panic!("one row, the fallback's: {:?}", phase.rows);
        };
        assert_eq!(fallback.label, "review:due-tests");
        assert_eq!(fallback.status, RowStatus::Running);
        assert_eq!(fallback.worker, sonnet);
        assert_eq!(fallback.fallback_for, Some(opus));
        assert_eq!(
            fallback.note.as_ref().map(|note| note.text.as_str()),
            Some("overloaded")
        );
        assert_eq!(phase.count, PhaseCount::of(0, 1));
    }

    /// A build task that fell back to another worker: the line the run wrote about the task's
    /// workflow shows under the header, and the task's row in Build is the fallback's, for the
    /// worker that stopped and why.
    #[test]
    fn a_tasks_fallback_is_said_under_the_header() {
        use argonauts_core::events::agent_status::{FallbackEvent, FallbackPlan};
        use argonauts_core::events::run::Party;
        use argonauts_core::events::workflow::RunNote;
        use argonauts_core::settings::fallback::{FallbackMode, FallbackStrategy};
        use argonauts_core::{DiffStat, TaskId};

        let top = WorkflowId::numbered(1);
        let build = step(&top, 1, 0);
        let task = TaskId::new("task-001").unwrap();
        let (first, second) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let opus: ModelRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let gpt: ModelRef = "codex/gpt-5.6".parse().unwrap();
        let building = |seq, key: &SessionKey, worker: &ModelRef| {
            let mut started = session_started(
                seq,
                key,
                Some(&task),
                RoleName::Worker,
                worker.agent.as_str(),
            );
            if let RunEventKind::Session(SessionEvent::Started(session)) = &mut started.kind {
                session.worker = worker.clone();
                session.label = Some("build:due-flag".to_owned());
                session.workflow_step = Some(build.clone());
            }
            started
        };
        let about_the_task = |seq, kind| {
            let mut event = event(seq, kind);
            event.task_id = Some(task.clone());
            event
        };
        let plan = FallbackPlan {
            task: task.clone(),
            title: "The --due flag".to_owned(),
            stopped: opus.clone(),
            reason: "usage limit".to_owned(),
            resets_at: None,
            work: DiffStat::default(),
            checks_ran: false,
            candidates: Vec::new(),
            mode: FallbackMode::Auto,
            strategy: FallbackStrategy::First,
            race_count: 1,
            also_moving: Vec::new(),
        };
        let fell_back = FallbackEvent::FellBack {
            task: task.clone(),
            to: gpt.clone(),
            mode: FallbackMode::Auto,
            by: Party::Rules,
            context: None,
        };
        let said = "Opus 5.5 on Claude Code's session ended early (its usage limit) · fell back \
                    to GPT 5.6 on Codex";
        let events = vec![
            started(0, &top, None, captain()),
            step_started(1, build.clone(), StepKind::Build, StandardPhase::Do),
            building(2, &first, &opus),
            session_ended(3, &first, EndReason::UsageLimit { resets_at: None }, 2_000),
            about_the_task(4, RunEventKind::Fallback(FallbackEvent::Needed(plan))),
            about_the_task(5, RunEventKind::Fallback(fell_back)),
            workflow_event(
                6,
                WorkflowEvent::Note(RunNote {
                    text: said.to_owned(),
                    workflow: Some(top.clone()),
                    left: false,
                }),
            ),
            building(7, &second, &gpt),
        ];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        let [line] = view.status_lines.as_slice() else {
            panic!("{:?}", view.status_lines);
        };
        assert_eq!((line.kind, line.text.as_str()), (StatusKind::Note, said));

        let phase = view
            .phases
            .iter()
            .find(|phase| phase.name == "Build")
            .expect("a Build phase");
        let [WorkflowRow::Agent(fallback)] = phase.rows.as_slice() else {
            panic!("one row, the fallback's: {:?}", phase.rows);
        };
        assert_eq!(fallback.label, "build:due-flag");
        assert_eq!(fallback.status, RowStatus::Running);
        assert_eq!(fallback.worker, gpt);
        assert_eq!(fallback.fallback_for, Some(opus));
        assert_eq!(
            fallback.note.as_ref().map(|note| note.text.as_str()),
            Some("usage limit")
        );
    }

    #[test]
    fn a_task_auto_left_to_you_says_so_under_the_header() {
        let mut events: Vec<RunEvent> = DEFAULT_CREW_RUN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        // Cut the run where its first approval opened, before auto settled it, and wait there.
        let opened = events
            .iter()
            .position(|event| {
                matches!(event.kind, RunEventKind::Approval(ApprovalEvent::Opened(_)))
            })
            .unwrap();
        events.truncate(opened + 1);
        let RunEventKind::Approval(ApprovalEvent::Opened(task)) = &events[opened].kind else {
            unreachable!();
        };
        assert!(!task.needs_you, "auto may settle it when it opens");
        let (id, title) = (task.id.clone(), task.title.clone());
        let mut waiting = events[opened].clone();
        waiting.seq += 1;
        waiting.kind = RunEventKind::Run(RunLifecycle::Waiting {
            reason: WaitReason::Approval,
            waiting_on: vec![id],
        });
        events.push(waiting);
        let run = events[0].run_id.clone();
        let view = workflow_view(&events, &run, None).unwrap();
        let lines: Vec<&str> = view
            .status_lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        // The question itself draws under the view, so the line only names it.
        assert_eq!(lines, [title.trim()]);
    }

    /// A CLI that is down says so under the header: which CLI, how many tasks wait on it, and
    /// how long until it is tried again, for the screen to count down.
    #[test]
    fn a_cli_that_is_down_says_what_waits_and_when_it_is_tried_again() {
        use argonauts_core::TaskId;
        use argonauts_core::events::approval::{AgentDownPacket, PausedTask};
        use argonauts_core::names::{AgentId, RoleName};

        let mut events: Vec<RunEvent> = DEFAULT_CREW_RUN
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let opened = events
            .iter()
            .position(|event| {
                matches!(event.kind, RunEventKind::Approval(ApprovalEvent::Opened(_)))
            })
            .unwrap();
        events.truncate(opened + 1);
        let RunEventKind::Approval(ApprovalEvent::Opened(asked)) = &mut events[opened].kind else {
            unreachable!();
        };
        let paused = |task: &str| PausedTask {
            task: TaskId::new(task).unwrap(),
            title: "a task".to_owned(),
            role: RoleName::Worker,
        };
        asked.needs_you = true;
        asked.packet = ApprovalPacket::AgentDown(AgentDownPacket {
            agent_id: AgentId("claude-code".into()),
            evidence: "overloaded".to_owned(),
            test_at: Some(OffsetDateTime::now_utc() + time::Duration::minutes(10)),
            paused: vec![paused("task-001"), paused("task-002")],
            fits_all: None,
            kept_by: None,
        });
        let run = events[0].run_id.clone();
        let view = workflow_view(&events, &run, None).unwrap();
        let [line] = view.status_lines.as_slice() else {
            panic!("{:?}", view.status_lines);
        };
        assert_eq!(line.kind, StatusKind::NeedsYou);
        assert_eq!(line.text, "Claude Code is down · 2 tasks paused");
        let left = line.left.as_ref().expect("a wait to count down");
        assert_eq!(left.words, "tried again in");
        assert!((590_000..=600_000).contains(&left.ms), "{}", left.ms);
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
        let events = vec![started(0, &top, None, captain()), named("add-due-dates")];
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!(view.notes, ["orchestrator workflow"]);

        let events = vec![started(0, &top, None, captain()), named("orchestrator")];
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
