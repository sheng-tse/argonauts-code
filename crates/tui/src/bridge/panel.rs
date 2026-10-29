//! The panel under the bottom line: one row per request (its run), its tasks under it while it
//! goes, and under a task at work whatever does its work, a workflow's steps or its agents. With
//! `ui.panel.agents`, each run's running agents follow under its rows.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::api::{RunState as ApiRunState, RunSummary};
use argonauts_core::app::{HeaderAndBottomLine, PullRequestStands};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::ApprovalOpened;
use argonauts_core::events::run::{RunMode, WaitReason};
use argonauts_core::events::session::{CliAgent, CliAgentState, CliWorkflowRun, SubagentStatus};
use argonauts_core::events::task::{TaskState, Way};
use argonauts_core::logos::logo_for;
use argonauts_core::names::SessionKey;
use argonauts_core::names::{ApprovalId, RoleName};
use argonauts_core::settings::limit::LimitUnit;
use argonauts_core::views::{cli_workflow_run_id, conversation_run};
use argonauts_core::{Spend, TaskId, TodoStatus};
use time::OffsetDateTime;

use crate::bottom::{
    AgentRef, AgentRow, AgentState, RowKind, RunRowKind, WorkflowRow, WorkflowRowState,
};
use crate::state::{
    AppState, RunState, RunTotals, SessionState, SessionStatus, TaskRow, binding_reading,
};
use crate::views::asks::{Ask, label_stem};
use crate::views::panes;

use super::Cx;

#[path = "panel_seats.rs"]
mod seats;

/// Past this share of its limit, a run's row warns.
const LIMIT_WARN_SHARE: f64 = 0.8;

/// How long a run that ended keeps its row, with how it ended. Claude Code keeps a finished
/// workflow listed this long.
pub(crate) const KEPT_AFTER_END: Duration = Duration::from_secs(30);

pub fn sync(cx: &mut Cx<'_>) {
    let wall = OffsetDateTime::now_utc();
    let panel = &cx.settings.panel;
    let bottom = &mut cx.model.bottom;
    let keep = bottom.workflows.selected_id(&bottom.agents);
    let rows = run_rows(cx.state, wall, cx.now);
    let mut listed = conversation_rows(cx.state, &bottom.agents.rows, cx.now);
    if panel.agents && !rows.is_empty() {
        listed.extend(agent_rows(cx.state, wall, cx.now));
    }
    // Your conversation heads the list whenever there's an agent to tell it from.
    if !listed.is_empty() {
        listed.insert(0, AgentRow::main());
    }
    bottom.agents.rows = listed;
    bottom.workflows.rows = rows;
    bottom.workflows.max_rows = panel.max_rows as usize;
    bottom.workflows.show_agents = panel.agents;
    bottom.agents.cli_internals = panel.subagents;
    bottom.workflows.reselect(&bottom.agents, keep.as_ref());
}

/// One row per run still going, oldest first, then one for each workflow an agent's own CLI
/// runs in it. The runs this screen doesn't hold, from an earlier session, come
/// first. A run that ended keeps its row for a while, saying how it ended, as Claude Code
/// keeps a finished workflow listed, and for as long as something of it waits on you.
pub fn run_rows(state: &AppState, wall: OffsetDateTime, now: Instant) -> Vec<WorkflowRow> {
    let mut background: Vec<&RunSummary> = state
        .background
        .iter()
        .filter(|run| !state.runs.contains_key(&run.run))
        // A run that isn't on this screen has no row either while its workflow is written.
        .filter(|run| run.shape_known || !run.is_open() || run.needs_you())
        .collect();
    background.sort_by(|a, b| a.updated.cmp(&b.updated).then_with(|| a.run.cmp(&b.run)));
    let mut runs: Vec<&RunState> = state
        .runs
        .values()
        .filter(|run| {
            run.finished.is_none() || !run.approvals.is_empty() || kept_for(run, wall).is_some()
        })
        .filter(|run| !state.ended_off_screen.contains(&run.id))
        // A workflow still being written has no row: only the working line says so. What
        // waits on you, and how a run ended, still show.
        .filter(|run| run.shape_known() || run.finished.is_some() || !run.approvals.is_empty())
        .collect();
    runs.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
    let held = runs.into_iter().flat_map(|run| {
        let mut row = run_row(run, state.status.as_ref(), wall, now);
        if let Some(listed) = state.listed_off_screen(&run.id) {
            follow_listing(&mut row, listed);
        }
        // Nothing of it waits on you, so its row goes when its time is up.
        if run.approvals.is_empty() {
            row.leaves_at = kept_for(run, wall).map(|left| now + left);
        }
        let tasks = shown_tasks(run);
        let under = if tasks.is_empty() {
            // A council shows its seats while it talks, until it has tasks.
            let seats = run
                .talk
                .as_ref()
                .filter(|_| run.finished.is_none())
                .map(|talk| seats::rows(run, talk, &state.names, wall, now))
                .unwrap_or_default();
            seats
                .into_iter()
                .chain(inside_rows(run, wall, now))
                .collect()
        } else if run.finished.is_none() {
            task_rows(run, &tasks, wall, now)
        } else {
            // A request that ended is one row.
            Vec::new()
        };
        std::iter::once(row).chain(under)
    });
    // A workflow the conversation's own CLI runs gets a row as a run's does.
    let conversation = conversation_run();
    let conversation = state
        .conversation
        .workflows
        .iter()
        .filter(|workflow| !workflow.ended())
        .map(|workflow| cli_workflow_row(&conversation, workflow, wall, now));
    let mut rows: Vec<WorkflowRow> = background
        .into_iter()
        .map(background_row)
        .chain(held)
        .chain(conversation)
        .collect();
    number_alike(&mut rows);
    rows
}

/// How much longer a run that ended keeps its row. `None` for one still going, one whose end
/// this screen never saw, and one that ended longer ago than a row is kept, as every run of
/// an earlier session did.
fn kept_for(run: &RunState, wall: OffsetDateTime) -> Option<Duration> {
    run.finished?;
    let since = Duration::try_from(wall - run.ended?).unwrap_or_default();
    KEPT_AFTER_END
        .checked_sub(since)
        .filter(|left| !left.is_zero())
}

/// Two runs never share a name in the list: the second of a name reads `add-due-dates-2`,
/// the third `-3`. A workflow inside a run is part of it, not a run, and keeps its own.
fn number_alike(rows: &mut [WorkflowRow]) {
    let mut seen: Vec<(String, usize)> = Vec::new();
    for row in rows.iter_mut().filter(|row| row.depth == 0) {
        let name = row.name.trim().to_owned();
        match seen.iter_mut().find(|(known, _)| *known == name) {
            Some((_, count)) => {
                *count += 1;
                let joint = if name.contains(char::is_whitespace) {
                    " "
                } else {
                    "-"
                };
                row.name = format!("{name}{joint}{count}");
            }
            None => seen.push((name, 1)),
        }
    }
}

/// The key of a panel row for a workflow an agent's own CLI runs: its run, `#`, and the id the
/// views give it.
pub(crate) fn agent_key(run: &RunId, workflow: &CliWorkflowRun) -> String {
    format!("{run}#{}", cli_workflow_run_id(&workflow.id))
}

/// The CLI workflows a run is: those the sessions of its only task run, as when Claude Code's
/// own workflow runs the crew's, oldest first. The first is the run's own and gets no row of its
/// own; a later one is a workflow inside the run.
fn the_tasks_own(run: &RunState) -> Vec<&CliWorkflowRun> {
    let mut tasks = run
        .sessions_in_order()
        .filter_map(|session| session.task.as_ref());
    let Some(task) = tasks.next() else {
        return Vec::new();
    };
    if tasks.any(|other| other != task) {
        return Vec::new();
    }
    run.sessions_in_order()
        .filter(|session| session.task.as_ref() == Some(task))
        .flat_map(|session| session.workflows.iter())
        .collect()
}

/// The CLI workflow whose phases are the run's: the first its task ran.
pub(super) fn the_runs_own(run: &RunState) -> Option<&CliWorkflowRun> {
    the_tasks_own(run).into_iter().next()
}

/// A run's agents done and in all, its time and its tokens: the one count its row here and
/// its closing row in the conversation show, as its workflow view's header does. A run that
/// is its task's CLI workflows counts every agent of each of them, and never switches from one
/// to the next; their phases are the run's, so nothing of its own after them counts.
pub(crate) fn run_totals(run: &RunState, wall: OffsetDateTime) -> RunTotals {
    let mut totals = run.totals(wall);
    let own = the_tasks_own(run);
    if own.is_empty() {
        // The agents of a workflow a worker's own CLI runs are the run's agents too, and stay
        // in its count once that workflow ends.
        let inside = run
            .sessions_in_order()
            .flat_map(|session| &session.workflows)
            .flat_map(|workflow| &workflow.agents);
        let (done, all) = inside.fold((0u32, 0u32), |(done, all), agent| {
            (
                done + u32::from(agent.state == CliAgentState::Done),
                all + 1,
            )
        });
        totals.agents_done += done;
        totals.agents_total += all;
        return totals;
    }
    let agents = || own.iter().flat_map(|workflow| &workflow.agents);
    let done = agents()
        .filter(|agent| agent.state == CliAgentState::Done)
        .count();
    let hosts: Vec<_> = run
        .sessions_in_order()
        .filter(|session| !session.workflows.is_empty())
        .filter_map(|session| session.workflow_step.as_ref().map(|step| step.phase))
        .collect();
    let (asked_done, asked_all) = asked_of_its_own(run, &own, &hosts);
    totals.agents_done = u32::try_from(done + asked_done).unwrap_or(u32::MAX);
    totals.agents_total = u32::try_from(agents().count() + asked_all).unwrap_or(u32::MAX);
    totals
}

/// The reviewers and judges the task's workflows asked of Argonauts Code, done and in all, that
/// fill no row of those workflows: each answer joins the agent that asked, as the workflow
/// view joins it, so only an ask that joins none is a row of its own in the view, and an agent
/// once across its rounds.
fn asked_of_its_own(run: &RunState, own: &[&CliWorkflowRun], hosts: &[u32]) -> (usize, usize) {
    let asked: Vec<_> = run
        .sessions_in_order()
        .filter(|session| {
            session
                .workflow_step
                .as_ref()
                .is_some_and(|step| hosts.contains(&step.phase))
                && session.workflows.is_empty()
                && argonauts_core::views::is_workflow_agent(
                    session.step,
                    session.workflow_step.as_ref(),
                )
                && session
                    .label
                    .as_deref()
                    .is_some_and(|label| !label.trim().is_empty())
        })
        .collect();
    let asks: Vec<Ask<'_>> = asked
        .iter()
        .map(|session| Ask {
            key: &session.key,
            label: session.label.as_deref().unwrap_or_default(),
            started_ms: u64::try_from(
                (session.started - OffsetDateTime::UNIX_EPOCH).whole_milliseconds(),
            )
            .ok(),
            asked_by: None,
        })
        .collect();
    let joined: std::collections::BTreeSet<SessionKey> = own
        .iter()
        .filter_map(|workflow| {
            let host = run.sessions_in_order().find(|session| {
                session
                    .workflows
                    .iter()
                    .any(|known| known.id == workflow.id)
            })?;
            Some(crate::views::asks::joins(&asks, &host.key, workflow))
        })
        .flat_map(|joined| joined.into_values())
        .collect();
    let mut left: std::collections::BTreeMap<&str, bool> = Default::default();
    for session in asked
        .iter()
        .filter(|session| !joined.contains(&session.key))
    {
        let label = session.label.as_deref().unwrap_or_default();
        left.insert(label_stem(label), session.status == SessionStatus::Done);
    }
    let done = left.values().filter(|done| **done).count();
    (done, left.len())
}

/// A `⎿` row for each workflow at work inside the run, a level further in for each one that
/// holds it: one a worker's own CLI runs, which names that worker, a later one the run's task
/// starts, and a child either of them starts, which its CLI reports as a phase named for it.
/// The run's own CLI workflow is the run's row, so only its children have rows. Each leaves
/// once it ends, and the run's count keeps its agents.
fn inside_rows(run: &RunState, wall: OffsetDateTime, now: Instant) -> Vec<WorkflowRow> {
    let wall_ms = u64::try_from((wall - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok();
    let own = the_runs_own(run).map(|workflow| workflow.id.as_str());
    let mut rows = Vec::new();
    for session in run.sessions_in_order() {
        for workflow in session
            .workflows
            .iter()
            .filter(|workflow| !workflow.ended())
        {
            let depth = if own == Some(workflow.id.as_str()) {
                0
            } else {
                let mut row = cli_workflow_row(&run.id, workflow, wall, now);
                row.kind = RunRowKind::Task;
                row.depth = 1;
                rows.push(row);
                1
            };
            rows.extend(child_rows(&run.id, workflow, depth + 1, wall_ms, now));
        }
    }
    rows
}

/// A row for each child at work that `workflow`'s script started, which its CLI reports as a
/// phase named `▸` and the child's name: its agents done of all, its time and its tokens.
fn child_rows(
    run: &RunId,
    workflow: &CliWorkflowRun,
    depth: usize,
    wall_ms: Option<u64>,
    now: Instant,
) -> Vec<WorkflowRow> {
    workflow
        .phases
        .iter()
        .filter_map(|phase| {
            let name = phase.child()?;
            let agents: Vec<_> = workflow.agents_in(phase.index).collect();
            let at_work = agents.iter().any(|agent| !agent.state.ended());
            if !at_work {
                return None;
            }
            let id = cli_workflow_run_id(&format!("{}-{name}", workflow.id));
            let mut row = WorkflowRow::new(format!("{run}#{id}"), name, WorkflowRowState::Running);
            row.kind = RunRowKind::Task;
            row.depth = depth;
            row.count = Some(done_of(&agents));
            let started = agents.iter().filter_map(|agent| agent.started_at_ms).min();
            row.elapsed_ms = match (started, wall_ms) {
                (Some(started), Some(wall)) => wall.saturating_sub(started),
                _ => 0,
            };
            row.measured_at = Some(now);
            row.tokens = agents.iter().map(|agent| agent.tokens).sum();
            Some(row)
        })
        .collect()
}

fn cli_workflow_row(
    run: &RunId,
    workflow: &CliWorkflowRun,
    wall: OffsetDateTime,
    now: Instant,
) -> WorkflowRow {
    let wall_ms = u64::try_from((wall - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok();
    let name = Some(workflow.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or("workflow");
    let mut row = WorkflowRow::new(agent_key(run, workflow), name, WorkflowRowState::Running);
    let agents: Vec<_> = workflow.agents.iter().collect();
    // A workflow that has reported no agent yet has nothing to count.
    row.count = Some(done_of(&agents)).filter(|(_, all)| *all > 0);
    row.elapsed_ms = workflow.elapsed_ms(wall_ms);
    row.measured_at = Some(now);
    row.tokens = workflow.tokens;
    row
}

/// The run's own name, `fix-slugify-median`, or before it has one, its workflow and title,
/// `Council · add --json to export`.
fn run_name(run: &RunState) -> String {
    // A workflow its script writes names itself in its first report, as the view's header does.
    let templates = [run.workflow.as_deref()];
    if let Some(reported) = run
        .reported
        .all()
        .map(|workflow| workflow.name.trim())
        .find(|name| argonauts_core::text::names_the_run(name, &templates))
    {
        return argonauts_core::text::title_words(reported);
    }
    if let Some(named) = run
        .named
        .as_ref()
        .map(|named| named.name.trim())
        .filter(|name| !name.is_empty())
    {
        return argonauts_core::text::title_words(named);
    }
    row_name(workflow_name(run), &run.title, &run.id)
}

fn row_name(workflow: Option<&str>, title: &str, run: &RunId) -> String {
    use argonauts_core::text::shown_name;
    let title = title.trim();
    match (workflow, title.is_empty()) {
        (Some(workflow), false) => format!("{} · {title}", shown_name(workflow)),
        (Some(workflow), true) => shown_name(workflow),
        (None, false) => title.to_owned(),
        (None, true) => run.to_string(),
    }
}

/// A run this screen doesn't hold, drawn from its summary. Its agents and time aren't known
/// here, so the row names it as it'll be named once open, says whether it needs you, and shows
/// what it spent. Enter on it opens the run, and its question with it.
fn background_row(run: &RunSummary) -> WorkflowRow {
    let state = listed_state(run);
    let workflow = match run.mode {
        RunMode::Captain => Some("captain"),
        RunMode::Crew => Some(run.workflow.trim()).filter(|chosen| !chosen.is_empty()),
    };
    let name = run
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| row_name(workflow, &run.title, &run.run), str::to_owned);
    let mut row = WorkflowRow::new(run.run.as_str(), name, state);
    row.timed = false;
    row.tokens = run.spent.output_tokens;
    row
}

fn listed_state(run: &RunSummary) -> WorkflowRowState {
    if run.needs_you() {
        WorkflowRowState::NeedsYou
    } else if run.state == ApiRunState::Pausing {
        WorkflowRowState::Pausing
    } else if paused_by_you(run) {
        WorkflowRowState::Paused
    } else if run.state == ApiRunState::Waiting {
        if run.stopped_at.starts_with("stopped") {
            WorkflowRowState::Stopped
        } else {
            WorkflowRowState::Waiting
        }
    } else {
        WorkflowRowState::Running
    }
}

/// Whether a listed run waits on a pause you asked for. The listing words why a run waits as
/// its journal ends, and only that pause reads `paused`.
pub(crate) fn paused_by_you(run: &RunSummary) -> bool {
    run.state == ApiRunState::Waiting && run.stopped_at == "paused"
}

/// A held run's row once it goes on off screen: what this screen last saw of its agents, with
/// its state and tokens as the listing reads them from its journal. What the row said of the
/// run while this screen held it would be stale, so it goes.
fn follow_listing(row: &mut WorkflowRow, listed: &RunSummary) {
    row.state = listed_state(listed);
    row.tokens = row.tokens.max(listed.spent.output_tokens);
}

/// The run's own workflow. Before the main agent picks it, Understand runs ahead of it, so the row
/// names the workflow Understand came from, then the workflow, rather than `understanding`.
pub(crate) fn workflow_name(run: &RunState) -> Option<&str> {
    fn named(name: &str) -> Option<&str> {
        Some(name.trim()).filter(|name| !name.is_empty())
    }
    if let Some(own) = run
        .workflows
        .iter()
        .find(|workflow| workflow.parent.is_none())
    {
        return named(&own.name);
    }
    run.workflows
        .iter()
        .find_map(|workflow| workflow.from.as_deref().and_then(named))
        .or_else(|| run.workflow.as_deref().and_then(named))
        .or_else(|| run.workflows.first().and_then(|first| named(&first.name)))
}

fn run_row(
    run: &RunState,
    status: Option<&HeaderAndBottomLine>,
    wall: OffsetDateTime,
    now: Instant,
) -> WorkflowRow {
    let totals = run_totals(run, wall);
    let mut row = WorkflowRow::new(run.id.as_str(), run_name(run), row_state(run));
    row.answers = question_of(run, |_| true);
    let tasks = shown_tasks(run);
    // A council talks before it has tasks, and its row counts the seats that back every point.
    row.count = if let Some(talk) = run.talk.as_ref().filter(|_| tasks.is_empty()) {
        Some(talk.backing())
    } else if tasks.is_empty() {
        (totals.agents_total > 0).then_some((totals.agents_done, totals.agents_total))
    } else {
        let merged = tasks
            .iter()
            .filter(|task| task.state == Some(TaskState::Merged))
            .count();
        Some((count(merged), count(tasks.len())))
    };
    row.tokens = totals.tokens;
    row.elapsed_ms = totals.elapsed.map_or(0, millis);
    if run.finished.is_none() {
        row.measured_at = Some(now);
    }
    row.near_limit = run.finished.is_none()
        && status
            .and_then(|status| limit_share(status, run, Duration::from_millis(row.elapsed_ms)))
            .is_some_and(|share| share >= LIMIT_WARN_SHARE);
    row
}

/// The run's mark: how it ended, or what it does now. The pull request it opened says where it
/// stands once the run that opened it ended.
fn row_state(run: &RunState) -> WorkflowRowState {
    if let Some(followed) = run.followed.as_ref().filter(|_| run.finished.is_some()) {
        match followed.stands {
            PullRequestStands::Waits => return WorkflowRowState::PullRequest,
            PullRequestStands::Works => return WorkflowRowState::Running,
            PullRequestStands::NeedsYou => return WorkflowRowState::NeedsYou,
            PullRequestStands::Ended => {}
        }
    }
    match (&run.finished, &run.waiting) {
        (Some(outcome), _) => WorkflowRowState::ended(*outcome),
        // Only a question that waits on you marks it; one the rules still decide doesn't.
        (None, _) if waits_on_you(run, None) => WorkflowRowState::NeedsYou,
        (None, Some(WaitReason::Paused)) => WorkflowRowState::Paused,
        (None, Some(WaitReason::NoFallback { .. })) => WorkflowRowState::Stopped,
        (None, Some(WaitReason::AllAgentsOut { .. } | WaitReason::WaitingForAgent { .. })) => {
            WorkflowRowState::Waiting
        }
        (None, _) if run.pausing => WorkflowRowState::Pausing,
        (None, _) if on_a_fallback(run) => WorkflowRowState::Fallback,
        (None, _) => WorkflowRowState::Running,
    }
}

/// A question of the run, or of its task `task`, waits on you: one the rules still decide, or
/// one already answered, doesn't.
/// The oldest question about a row that waits on you, which Enter on the row brings up.
pub(super) fn question_of(
    run: &RunState,
    about: impl Fn(&ApprovalOpened) -> bool,
) -> Option<ApprovalId> {
    run.approvals
        .iter()
        .find(|open| open.opened.needs_you && about(&open.opened))
        .map(|open| open.opened.id.clone())
}

fn waits_on_you(run: &RunState, task: Option<&TaskId>) -> bool {
    run.approvals.iter().any(|open| {
        open.opened.needs_you && task.is_none_or(|task| open.opened.task.as_ref() == Some(task))
    })
}

/// A model works in another's place while the run goes on.
fn on_a_fallback(run: &RunState) -> bool {
    run.sessions
        .values()
        .any(|session| session.is_running() && session.fallback_for.is_some())
}

/// Agents of a CLI's own workflow done, of all of them.
fn done_of(agents: &[&CliAgent]) -> (u32, u32) {
    let done = agents
        .iter()
        .filter(|agent| agent.state == CliAgentState::Done)
        .count();
    (count(done), count(agents.len()))
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The tasks a request's row counts and lists: every one but those cancelled, in their order. A
/// run that is its one task's CLI workflow counts that workflow's agents instead, as Claude
/// Code's own row of it does.
fn shown_tasks(run: &RunState) -> Vec<&TaskRow> {
    let tasks: Vec<&TaskRow> = run
        .tasks
        .values()
        .filter(|task| task.state != Some(TaskState::Cancelled))
        .collect();
    if tasks.len() <= 1 && !the_tasks_own(run).is_empty() {
        return Vec::new();
    }
    tasks
}

/// A row for each task, and under a task at work whatever does its work: its workflow's steps,
/// its agents, or its CLI's own workflow's agents. A task in its own session has nothing under
/// it. Each counts done of total for what sits under it, and a task in its own session counts
/// its session's to-do list.
fn task_rows(
    run: &RunState,
    tasks: &[&TaskRow],
    wall: OffsetDateTime,
    now: Instant,
) -> Vec<WorkflowRow> {
    let mut rows = Vec::new();
    for task in tasks {
        let sessions: Vec<&SessionState> = run
            .sessions_in_order()
            .filter(|session| session.task.as_ref() == Some(&task.id))
            .collect();
        let worker = sessions
            .iter()
            .rev()
            .find(|session| session.role == RoleName::Worker)
            .or_else(|| sessions.last())
            .copied();
        let way = task.way.as_ref().map(|way| &way.how);
        // What does the work: a row for each step or agent, each followed by any workflow its
        // own CLI runs, a level further in.
        let work: Vec<(WorkflowRow, Vec<WorkflowRow>)> = match way {
            None | Some(Way::OwnSession) => Vec::new(),
            Some(Way::Workflow { .. }) => sessions
                .iter()
                .map(|session| {
                    let row = work_row(run, session, step_words(session), wall, now);
                    (row, cli_workflow_rows(run, session, 3, wall, now))
                })
                .collect(),
            Some(Way::Agents { .. }) => sessions
                .iter()
                .map(|session| {
                    let row = work_row(run, session, agent_words(run, session), wall, now);
                    (row, cli_workflow_rows(run, session, 3, wall, now))
                })
                .collect(),
            Some(Way::CliWorkflow) => worker
                .into_iter()
                .flat_map(|session| cli_agent_rows(run, session, wall, now))
                .map(|row| (row, Vec::new()))
                .collect(),
        };
        let done = work
            .iter()
            .filter(|(row, _)| row.state == WorkflowRowState::Done)
            .count();
        let mut row = WorkflowRow::new(
            format!("{}#{}", run.id, task.id),
            task_name(task),
            task_state(run, task, &sessions),
        );
        row.kind = RunRowKind::Task;
        row.depth = 1;
        row.answers = question_of(run, |opened| opened.task.as_ref() == Some(&task.id));
        row.way = Some(way_words(way, worker));
        row.logo = task
            .worker
            .as_ref()
            .or(worker.map(|session| &session.worker))
            .map(|worker| logo_for(&worker.agent, worker.model.as_deref()));
        row.count = match way {
            None | Some(Way::OwnSession) => task.todos.or_else(|| worker.and_then(todo_count)),
            Some(Way::Agents { count: agents }) => {
                Some((count(done), u32::from(*agents).max(count(work.len()))))
            }
            // A workflow's steps count before they start, as many as it declares, else its build
            // and a review session for each reviewer the plan gave the task. A step done again
            // after a send back counts once.
            Some(Way::Workflow { .. }) => {
                let distinct = |done_only: bool| {
                    let mut names: Vec<&str> = work
                        .iter()
                        .filter(|(row, _)| !done_only || row.state == WorkflowRowState::Done)
                        .map(|(row, _)| row.name.as_str())
                        .collect();
                    names.sort_unstable();
                    names.dedup();
                    count(names.len())
                };
                let total = task
                    .way
                    .as_ref()
                    .and_then(|way| way.steps)
                    .unwrap_or_else(|| task.reviewers.saturating_add(1))
                    .max(distinct(false));
                Some((distinct(true), total))
            }
            Some(Way::CliWorkflow) => {
                Some((count(done), count(work.len()))).filter(|(_, all)| *all > 0)
            }
        };
        if row.count.is_none() {
            row.doing = worker.map(crate::views::crew::doing);
        }
        // A task in its own session is that session, and Enter opens it. One done by a workflow
        // or by agents opens the run's view on the workflow its sessions work in.
        if matches!(way, None | Some(Way::OwnSession)) {
            row.session = worker.map(|session| agent_ref(run, session));
        } else {
            row.opens = sessions
                .iter()
                .find_map(|session| session.workflow_step.as_ref())
                .filter(|step| run.workflows.iter().any(|known| known.id == step.workflow))
                .map(|step| format!("{}#{}", run.id, step.workflow));
        }
        row.tokens = sessions.iter().map(|session| session.tokens_out).sum();
        time_of(&mut row, &sessions, wall, now);
        let at_work = matches!(
            row.state,
            WorkflowRowState::Running | WorkflowRowState::Fallback
        );
        rows.push(row);
        if work.is_empty() {
            // A workflow the session's own CLI runs is at work whatever the task's mark says.
            rows.extend(
                sessions
                    .iter()
                    .flat_map(|session| cli_workflow_rows(run, session, 2, wall, now)),
            );
        } else if at_work {
            rows.extend(
                work.into_iter()
                    .flat_map(|(row, inside)| std::iter::once(row).chain(inside)),
            );
        }
    }
    rows
}

/// A row for each workflow `session`'s own CLI runs while it is at work, `depth` in, and one more
/// a level further in for each child its script starts.
fn cli_workflow_rows(
    run: &RunState,
    session: &SessionState,
    depth: usize,
    wall: OffsetDateTime,
    now: Instant,
) -> Vec<WorkflowRow> {
    let wall_ms = u64::try_from((wall - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok();
    session
        .workflows
        .iter()
        .filter(|workflow| !workflow.ended())
        .flat_map(|workflow| {
            let mut row = cli_workflow_row(&run.id, workflow, wall, now);
            row.kind = RunRowKind::Task;
            row.depth = depth;
            row.logo = Some(logo_for(
                &session.worker.agent,
                session.worker.model.as_deref(),
            ));
            std::iter::once(row).chain(child_rows(&run.id, workflow, depth + 1, wall_ms, now))
        })
        .collect()
}

/// A task's mark: done once merged, ◆ only while a question of it waits on you, ◌ while it waits
/// out an outage, dim ◌ before it starts, and at work otherwise.
fn task_state(run: &RunState, task: &TaskRow, sessions: &[&SessionState]) -> WorkflowRowState {
    if waits_on_you(run, Some(&task.id)) || task.state == Some(TaskState::NeedsYou) {
        return WorkflowRowState::NeedsYou;
    }
    if task.waiting_on_agent.is_some() {
        return WorkflowRowState::Waiting;
    }
    match task.state {
        Some(TaskState::Merged) => WorkflowRowState::Done,
        Some(TaskState::Failed) => WorkflowRowState::Failed,
        Some(TaskState::Cancelled) => WorkflowRowState::Stopped,
        // You took it over in your own terminal.
        Some(TaskState::TakenOver) => WorkflowRowState::Paused,
        // A record that hasn't said it started still has its sessions at work.
        None | Some(TaskState::Waiting | TaskState::Ready | TaskState::Deferred)
            if !sessions.iter().any(|session| session.is_running()) =>
        {
            WorkflowRowState::Queued
        }
        _ if run.pausing => WorkflowRowState::Pausing,
        _ => {
            let filling_in = task.fallback_for.is_some()
                || sessions
                    .iter()
                    .any(|session| session.is_running() && session.fallback_for.is_some());
            if filling_in {
                WorkflowRowState::Fallback
            } else {
                WorkflowRowState::Running
            }
        }
    }
}

/// How a task is done, in a word or two: `Own session`, its workflow's name, its CLI's own
/// workflow's name, or `3 agents`. A task from before ways were kept was done in its own session.
fn way_words(way: Option<&Way>, worker: Option<&SessionState>) -> String {
    match way {
        None | Some(Way::OwnSession) => "Own session".to_owned(),
        Some(Way::Workflow { name }) => argonauts_core::text::shown_name(name.as_str()),
        Some(Way::CliWorkflow) => worker
            .and_then(|session| session.workflows.first())
            .map(|workflow| workflow.name.trim())
            .filter(|name| !name.is_empty())
            .map_or_else(
                || "CLI workflow".to_owned(),
                argonauts_core::text::shown_name,
            ),
        Some(Way::Agents { count }) => crate::council::plan::agents_word(*count),
    }
}

fn task_name(task: &TaskRow) -> String {
    Some(task.title.trim())
        .filter(|title| !title.is_empty())
        .map_or_else(|| task.id.to_string(), str::to_owned)
}

/// A session's to-do list, done of total, when it keeps one.
fn todo_count(session: &SessionState) -> Option<(u32, u32)> {
    let done = session
        .todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Completed)
        .count();
    (!session.todos.is_empty()).then(|| (count(done), count(session.todos.len())))
}

/// A step of a task's workflow by its step's name, `Build` or `Review`, else its label.
fn step_words(session: &SessionState) -> String {
    let step = session
        .step
        .and_then(|step| serde_json::to_value(step).ok())
        .and_then(|value| value.as_str().map(|name| name.replace('_', " ")));
    step.map(|name| argonauts_core::text::shown_name(&name))
        .or_else(|| session.label.clone())
        .unwrap_or_else(|| session.task())
}

/// One of a task's agents by its label without the step it does, `Templates` for
/// `build:templates`.
fn agent_words(run: &RunState, session: &SessionState) -> String {
    let label = session
        .label
        .clone()
        .or_else(|| session.task.as_ref().map(|task| run.task_label(task)));
    match label {
        Some(label) => {
            let item = label
                .split_once(':')
                .map_or(label.as_str(), |(_, item)| item);
            argonauts_core::text::shown_name(&argonauts_core::text::title_words(item))
        }
        None => session.task(),
    }
}

/// A session under a task: a step of its workflow or one of its agents, with the agents' marks,
/// what it is doing in place of a bar, and Enter opening it.
fn work_row(
    run: &RunState,
    session: &SessionState,
    name: String,
    wall: OffsetDateTime,
    now: Instant,
) -> WorkflowRow {
    let state = match session.status {
        SessionStatus::Done => WorkflowRowState::Done,
        SessionStatus::Failed => WorkflowRowState::Failed,
        SessionStatus::Stopped => WorkflowRowState::Stopped,
        _ => WorkflowRowState::Running,
    };
    let mut row = WorkflowRow::new(format!("{}#{}", run.id, session.key), name, state);
    row.kind = RunRowKind::Work;
    row.depth = 2;
    row.logo = Some(logo_for(
        &session.worker.agent,
        session.worker.model.as_deref(),
    ));
    row.doing = Some(crate::views::crew::doing(session));
    row.session = Some(agent_ref(run, session));
    row.tokens = session.tokens_out;
    time_of(&mut row, &[session], wall, now);
    row
}

/// The agents of the CLI workflow a task's session runs, as rows under the task.
fn cli_agent_rows(
    run: &RunState,
    session: &SessionState,
    wall: OffsetDateTime,
    now: Instant,
) -> Vec<WorkflowRow> {
    let wall_ms = u64::try_from((wall - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok();
    let Some(workflow) = session.workflows.first() else {
        return Vec::new();
    };
    workflow
        .agents
        .iter()
        .map(|agent| {
            let state = match agent.state {
                CliAgentState::Done => WorkflowRowState::Done,
                state if state.ended() => WorkflowRowState::Failed,
                _ => WorkflowRowState::Running,
            };
            let key = format!(
                "{}#{}-{}",
                run.id,
                cli_workflow_run_id(&workflow.id),
                agent.index
            );
            let mut row = WorkflowRow::new(key, agent.label.trim(), state);
            row.kind = RunRowKind::Work;
            row.depth = 2;
            row.logo = Some(logo_for(&session.worker.agent, agent.model.as_deref()));
            row.doing = agent
                .result
                .as_deref()
                .or(agent.prompt.as_deref())
                .map(|text| first_line(text).to_owned());
            row.tokens = agent.tokens;
            row.elapsed_ms = agent.duration_ms;
            if let (WorkflowRowState::Running, Some(started), Some(wall)) =
                (state, agent.started_at_ms, wall_ms)
            {
                row.elapsed_ms = wall.saturating_sub(started);
                row.measured_at = Some(now);
            }
            row
        })
        .collect()
}

fn agent_ref(run: &RunState, session: &SessionState) -> AgentRef {
    AgentRef {
        run: Some(run.id.to_string()),
        session: session.key.to_string(),
    }
}

/// A row's time: from the first of `sessions` to start to the last to end, or to now while one
/// runs, when it keeps counting. Before any started it has none.
fn time_of(row: &mut WorkflowRow, sessions: &[&SessionState], wall: OffsetDateTime, now: Instant) {
    let Some(started) = sessions.iter().map(|session| session.started).min() else {
        row.timed = false;
        return;
    };
    let running = sessions.iter().any(|session| session.is_running());
    let end = if running {
        wall
    } else {
        sessions
            .iter()
            .filter_map(|session| session.ended)
            .max()
            .unwrap_or(wall)
    };
    row.elapsed_ms = Duration::try_from(end - started).map_or(0, millis);
    if running {
        row.measured_at = Some(now);
    }
}

/// The share of the limit the bottom line's run has used, in its limit's unit, counting the
/// spend the bottom line shows. Plan usage limits are shown by the plan usage chips instead.
fn limit_share(status: &HeaderAndBottomLine, run: &RunState, elapsed: Duration) -> Option<f64> {
    if status.run.as_ref() != Some(&run.id) {
        return None;
    }
    let limit = status.limit.as_ref().filter(|limit| limit.amount > 0.0)?;
    let spent = run.spend();
    let used = match limit.unit {
        LimitUnit::Usd => spent.cost_usd,
        LimitUnit::Tokens => spent.total_tokens() as f64,
        LimitUnit::Minutes => elapsed.as_secs_f64() / 60.0,
        LimitUnit::PlanUsage => return None,
    };
    Some(used / limit.amount)
}

/// The subagents your conversation's own CLI started, as Claude Code lists them under `main`:
/// each by its name and what it's doing now, ending with its time and the tokens its CLI
/// counted for it. `shown` is the list as it was, which is where a row's start time lives,
/// since the CLI's stream doesn't date its subagents.
fn conversation_rows(state: &AppState, shown: &[AgentRow], now: Instant) -> Vec<AgentRow> {
    let conversation = conversation_run().to_string();
    state
        .conversation
        .subagents
        .iter()
        .map(|subagent| {
            let ended = matches!(
                subagent.status,
                SubagentStatus::Done | SubagentStatus::Failed
            );
            let agent_state = match subagent.status {
                SubagentStatus::Done => AgentState::Done,
                SubagentStatus::Failed => AgentState::Failed,
                _ => AgentState::Working,
            };
            let name = subagent.label.as_deref().unwrap_or("subagent").trim();
            let doing = subagent
                .last_message
                .as_deref()
                .or(subagent.prompt.as_deref())
                .unwrap_or_default();
            let mut row =
                AgentRow::new(name, first_line(doing), agent_state).with_label(name, name);
            row.kind = RowKind::Subagent;
            row.key = Some(subagent.id.clone());
            row.run = Some(conversation.clone());
            row.tokens_down = subagent.tokens.unwrap_or_default();
            let before = shown.iter().find(|row| {
                row.kind == RowKind::Subagent && row.key.as_ref() == Some(&subagent.id)
            });
            row.started_at = Some(before.and_then(|row| row.started_at).unwrap_or(now));
            row.ended_at = ended.then(|| before.and_then(|row| row.ended_at).unwrap_or(now));
            row
        })
        .collect()
}

/// The first line of `text` with anything in it.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

/// Every run's running agents, run by run, with their markers.
pub fn agent_rows(state: &AppState, wall: OffsetDateTime, now: Instant) -> Vec<AgentRow> {
    state
        .runs
        .values()
        .flat_map(|run| {
            run.sessions_in_order()
                .filter(|session| session.is_running())
                .map(move |session| {
                    let mut row = agent_row(run, session, &state.names, wall, now);
                    row.markers.wrapping_up = state
                        .plan_windows
                        .get(&session.worker.agent)
                        .and_then(|readings| binding_reading(readings))
                        .is_some_and(|reading| reading.wrap_up);
                    // A page it published inside the workflow shows on its row too.
                    row.markers.artifact = state
                        .artifacts
                        .by_agent(&run.id, &session.key)
                        .map(|artifact| artifact.name.clone());
                    row
                })
        })
        .collect()
}

fn agent_row(
    run: &RunState,
    session: &SessionState,
    names: &DisplayNames,
    wall: OffsetDateTime,
    now: Instant,
) -> AgentRow {
    let task = session.task.as_ref().and_then(|id| run.tasks.get(id));
    let state = match session.status {
        SessionStatus::Starting | SessionStatus::Working => AgentState::Working,
        SessionStatus::Waiting | SessionStatus::Stopped => AgentState::Waiting,
        SessionStatus::Sleeping => AgentState::Timer,
        SessionStatus::Done => AgentState::Done,
        SessionStatus::Failed => AgentState::Failed,
    };
    let worker = &session.worker;
    // The list names an agent by its label and says what it's doing now, as Claude Code's
    // does. Who runs it is in its own view and in the crew list.
    let label = session
        .label
        .clone()
        .or_else(|| session.task.as_ref().map(|task| run.task_label(task)))
        .unwrap_or_else(|| session.task());
    // While it works, the task in hand from its own list says more than its latest tool call,
    // as on the working line.
    let task_in_hand = session
        .todos
        .iter()
        .find(|todo| todo.status == TodoStatus::InProgress && !todo.text.trim().is_empty())
        .filter(|_| state == AgentState::Working);
    let doing = match task_in_hand {
        Some(todo) => todo.text.clone(),
        None => crate::views::crew::doing(session),
    };
    let mut row =
        AgentRow::new(worker.agent.clone(), doing, state).with_label(label.clone(), label);
    // A session that names no model runs the one your settings or its CLI's default give, and
    // its logo is that model's: OpenCode running a model on Ollama shows Ollama's.
    row.model = names.running(worker).model;
    if !session.todos.is_empty() {
        let done = session
            .todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Completed)
            .count();
        row.steps = Some((done as u32, session.todos.len() as u32));
    }
    row.started_at = instant_at(session.started, wall, now);
    row.ended_at = session.ended.and_then(|end| instant_at(end, wall, now));
    row.tokens_down = session.tokens_out;
    row.spend = session.cost_usd.and_then(|cost_usd| {
        panes::spend(&Spend {
            cost_usd,
            ..Spend::default()
        })
    });
    if session.status == SessionStatus::Stopped {
        row.note = Some("stopped".to_owned());
    }
    row.key = Some(session.key.to_string());
    row.run = Some(run.id.to_string());
    row.markers.context = session.context;
    row.markers.fallback_for = session
        .fallback_for
        .as_ref()
        .or_else(|| task.and_then(|task| task.fallback_for.as_ref()))
        .map(|original| names.worker(original));
    row.markers.needs_review = task.is_some_and(|task| !task.debt.is_empty());
    row.markers.subagents = session.subagents.len() as u32;
    row
}

/// The monotonic instant for a wall-clock time, so the panel's timers keep counting.
fn instant_at(when: OffsetDateTime, wall: OffsetDateTime, now: Instant) -> Option<Instant> {
    let ago = Duration::try_from(wall - when).unwrap_or(Duration::ZERO);
    now.checked_sub(ago)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "panel_task_tests.rs"]
mod task_tests;

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::events::run::{Party, RunOutcome};
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::limit::LimitAmount;
    use argonauts_core::settings::ui::NarratorMode;

    use super::*;

    fn run_state(id: &str, wall: OffsetDateTime) -> RunState {
        let mut run = RunState::new(RunId::new(id).unwrap());
        run.title = format!("title of {id}");
        run.started = Some(wall - time::Duration::minutes(10));
        run
    }

    /// [`run_state`] once its workflow has started, so its row shows.
    fn begun(id: &str, wall: OffsetDateTime) -> RunState {
        let mut run = run_state(id, wall);
        run.workflows.push(crate::state::WorkflowState {
            id: argonauts_core::names::WorkflowId::numbered(1),
            name: "council".into(),
            from: Some("council".into()),
            description: String::new(),
            phases: vec!["Build".into()],
            parent: None,
            steps: Vec::new(),
        });
        run
    }

    fn status(run: &str, limit: LimitAmount) -> HeaderAndBottomLine {
        HeaderAndBottomLine {
            title: String::new(),
            repo: "~/code".into(),
            workflow: "council".into(),
            ultracode: false,
            ultra: None,
            run: Some(RunId::new(run).unwrap()),
            main_agent: "claude-code/opus".parse().unwrap(),
            plan_chips: Vec::new(),
            line: Default::default(),
            spent: None,
            limit: Some(limit),
            narrator: NarratorMode::Off,
            approval_mode: ApprovalMode::Supervised,
            awake: None,
        }
    }

    /// A run whose only task's session runs Claude Code's own workflow is that workflow: one
    /// row counting its steps, with nothing nested under the session.
    #[test]
    fn a_run_that_is_its_clis_workflow_is_one_row() {
        use argonauts_core::events::session::{CliAgent, CliPhase};
        use argonauts_core::names::{RoleName, SessionKey};
        let wall = OffsetDateTime::now_utc();
        let mut run = run_state("run-own", wall);
        let mut host = SessionState::new(
            SessionKey::numbered(1),
            "claude-code/haiku".parse().unwrap(),
            "claude-code",
            RoleName::Worker,
            wall,
        );
        host.task = Some(argonauts_core::TaskId::new("task-add-due-dates").unwrap());
        let agent = |index: u32, phase: u32, state| CliAgent {
            index,
            label: format!("step:{index}"),
            phase: Some(phase),
            model: None,
            state,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            started_at_ms: None,
            try_number: 1,
            prompt: None,
            result: None,
            ..CliAgent::default()
        };
        host.workflows.push(CliWorkflowRun {
            id: "wf_1".into(),
            call: None,
            name: "add-due-dates".into(),
            description: String::new(),
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
                agent(1, 1, CliAgentState::Done),
                agent(2, 2, CliAgentState::Done),
                agent(3, 3, CliAgentState::Running),
            ],
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        });
        run.session_order.push(host.key.clone());
        run.sessions.insert(host.key.clone(), host);
        let mut state = AppState::default();
        state.runs.insert(run.id.clone(), run);
        let rows = run_rows(&state, wall, Instant::now());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].count, Some((2, 3)));

        // A later workflow for the same task adds its agents to the run's count, which never
        // switches from the first workflow to the next.
        let run = state.runs.get_mut(&RunId::new("run-own").unwrap()).unwrap();
        let host = run.sessions.get_mut(&SessionKey::numbered(1)).unwrap();
        let mut later = host.workflows[0].clone();
        later.id = "wf_2".into();
        later.name = "final-signoff".into();
        later.agents = vec![
            agent(1, 1, CliAgentState::Done),
            agent(2, 2, CliAgentState::Running),
        ];
        host.workflows.push(later);
        let rows = run_rows(&state, wall, Instant::now());
        assert_eq!(rows[0].count, Some((3, 5)));
        // While it runs it has a row of its own under the run's, the run's own carrying on.
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!((rows[1].name.as_str(), rows[1].depth), ("final-signoff", 1));
    }

    /// A worker's own workflow is a row under its run's while it runs, and a child its script
    /// starts is one more a step further in. The run counts their agents.
    #[test]
    fn a_workflow_inside_a_run_follows_its_row() {
        use argonauts_core::events::session::{CliAgent, CliPhase};
        use argonauts_core::names::{RoleName, SessionKey, WorkflowId};
        use argonauts_core::workflow::StepRef;
        let wall = OffsetDateTime::now_utc();
        let mut run = run_state("run-due", wall);
        let step = StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 0,
            path: vec![0],
        };
        let worker = |n: u32, label: &str, task: &str| {
            let mut session = SessionState::new(
                SessionKey::numbered(n),
                "claude-code/sonnet".parse().unwrap(),
                "claude-code",
                RoleName::Worker,
                wall,
            );
            session.task = Some(argonauts_core::TaskId::new(task).unwrap());
            session.label = Some(label.to_owned());
            session.workflow_step = Some(step.clone());
            session
        };
        let agent = |index: u32, phase: u32, state, started: u64| CliAgent {
            index,
            label: format!("step:{index}"),
            phase: Some(phase),
            state,
            tokens: 1_000,
            started_at_ms: Some(started),
            try_number: 1,
            ..CliAgent::default()
        };
        let wall_ms =
            u64::try_from((wall - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).unwrap();
        let mut flag = worker(1, "build:due-flag", "task-due-flag");
        flag.workflows.push(CliWorkflowRun {
            id: "wf_due".into(),
            call: None,
            name: "add-due-flag".into(),
            description: String::new(),
            phases: ["Write", "Check", "▸ date-cases"]
                .iter()
                .zip(1..)
                .map(|(title, index)| CliPhase {
                    index,
                    title: (*title).to_owned(),
                    detail: String::new(),
                })
                .collect(),
            agents: vec![
                agent(1, 1, CliAgentState::Done, wall_ms - 60_000),
                agent(2, 2, CliAgentState::Done, wall_ms - 40_000),
                agent(3, 3, CliAgentState::Done, wall_ms - 22_000),
                agent(4, 3, CliAgentState::Running, wall_ms - 9_000),
            ],
            status: SubagentStatus::Running,
            tokens: 25_900,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: Some(wall_ms - 120_000),
        });
        let mut tests = worker(2, "build:due-tests", "task-due-tests");
        tests.status = SessionStatus::Done;
        for session in [flag, tests] {
            run.session_order.push(session.key.clone());
            run.sessions.insert(session.key.clone(), session);
        }
        let mut state = AppState::default();
        state.runs.insert(run.id.clone(), run);
        let rows = run_rows(&state, wall, Instant::now());
        let shape: Vec<String> = rows
            .iter()
            .map(|row| {
                let (done, all) = row.count.unwrap_or_default();
                format!("{} · {} in · {done}/{all}", row.name, row.depth)
            })
            .collect();
        assert_eq!(
            shape,
            [
                "title of run-due · 0 in · 4/6",
                "add-due-flag · 1 in · 3/4",
                "date-cases · 2 in · 1/2",
            ]
        );
        assert_eq!(rows[2].elapsed_ms / 1000, 22);
        assert_eq!(rows[2].tokens, 2_000);
    }

    /// The main agent's own subagents list under `main`, as Claude Code lists them: each by
    /// its name and what it's doing, with a clock that starts when the screen first saw it and
    /// stops when it ends.
    #[test]
    fn the_conversations_subagents_list_under_main() {
        use argonauts_core::events::session::{SubagentKind, SubagentUpdate};
        let update = |id: &str, label: &str, status, said: Option<&str>| SubagentUpdate {
            id: id.into(),
            parent: None,
            kind: SubagentKind::Subagent,
            label: Some(label.into()),
            prompt: Some("Count the files".into()),
            status,
            last_message: said.map(str::to_owned),
            log_uri: None,
            steps_visible: true,
            tokens: None,
        };
        let mut state = AppState::default();
        state.conversation.subagents = vec![
            SubagentUpdate {
                tokens: Some(11_823),
                ..update(
                    "toolu_a",
                    "Count slowly",
                    SubagentStatus::Running,
                    Some("Reading src\n"),
                )
            },
            update("toolu_b", "Read the readme", SubagentStatus::Started, None),
        ];
        let t0 = Instant::now();
        let first = conversation_rows(&state, &[], t0);
        let shown: Vec<(&str, &str, RowKind)> = first
            .iter()
            .map(|row| (row.name.as_str(), row.task.as_str(), row.kind))
            .collect();
        assert_eq!(
            shown,
            [
                ("Count slowly", "Reading src", RowKind::Subagent),
                ("Read the readme", "Count the files", RowKind::Subagent),
            ]
        );
        assert_eq!(first[0].agent_ref().run.as_deref(), Some("conversation"));
        assert_eq!(first[0].agent_ref().session, "toolu_a");
        assert_eq!(
            first[0].placeholder().as_deref(),
            Some("Message @Count slowly…")
        );
        // The first has a count from its CLI, and the second has none yet.
        assert_eq!((first[0].tokens_down, first[1].tokens_down), (11_823, 0));

        // Ten seconds on, the first has ended: its clock started at t0 and stops now.
        state.conversation.subagents[0].status = SubagentStatus::Done;
        let later = t0 + Duration::from_secs(10);
        let rows = conversation_rows(&state, &first, later);
        assert_eq!(rows[0].state, AgentState::Done);
        assert_eq!(
            (rows[0].started_at, rows[0].ended_at),
            (Some(t0), Some(later))
        );
        assert_eq!((rows[1].started_at, rows[1].ended_at), (Some(t0), None));
        // And it keeps that end from then on.
        let after = conversation_rows(&state, &rows, later + Duration::from_secs(60));
        assert_eq!(after[0].ended_at, Some(later));
    }

    #[test]
    fn near_its_limit_a_runs_bar_turns_amber() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut state = AppState::default();
        state
            .runs
            .insert(RunId::new("run-a").unwrap(), begun("run-a", wall));
        state
            .runs
            .insert(RunId::new("run-b").unwrap(), begun("run-b", wall));
        let usd = LimitAmount {
            unit: LimitUnit::Usd,
            amount: 5.0,
        };
        let spend = |state: &mut AppState, cost_usd: f64| {
            let run = state.runs.get_mut(&RunId::new("run-a").unwrap()).unwrap();
            run.spent = Spend {
                cost_usd,
                ..Spend::default()
            };
        };
        state.status = Some(status("run-a", usd));
        spend(&mut state, 4.25);
        let rows = run_rows(&state, wall, now);
        assert!(rows[0].near_limit);
        // Only the bottom line's run has a known limit.
        assert!(!rows[1].near_limit);

        spend(&mut state, 3.0);
        assert!(!run_rows(&state, wall, now)[0].near_limit);

        // A minutes limit counts the run's time.
        let minutes = LimitAmount {
            unit: LimitUnit::Minutes,
            amount: 11.0,
        };
        state.status = Some(status("run-a", minutes));
        assert!(run_rows(&state, wall, now)[0].near_limit);
    }

    /// Once a run that opened a pull request ended, its row follows the pull request: at work
    /// while a round works on it, ◐ while it waits on its checks, ◆ when it needs you, and done
    /// once GitHub merged or closed it.
    #[test]
    fn a_row_follows_the_pull_request_its_run_opened() {
        use argonauts_core::app::FollowedPullRequest;
        use argonauts_core::events::run::RunOutcome;

        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut run = run_state("run-a", wall);
        run.finished = Some(RunOutcome::PullRequest);
        let row = |run: &RunState| run_row(run, None, wall, now).state;
        assert_eq!(row(&run), WorkflowRowState::Done);
        for (stands, state) in [
            (PullRequestStands::Works, WorkflowRowState::Running),
            (PullRequestStands::Waits, WorkflowRowState::PullRequest),
            (PullRequestStands::NeedsYou, WorkflowRowState::NeedsYou),
            (PullRequestStands::Ended, WorkflowRowState::Done),
        ] {
            run.followed = Some(FollowedPullRequest {
                said: "#57 · CI running".to_owned(),
                stands,
            });
            assert_eq!(row(&run), state, "{stands:?}");
        }
    }

    /// Each state of a run has its own mark: at work, needs you, paused, pausing at a safe
    /// point, waiting out an outage, on a fallback model, stopped, and how it ended.
    #[test]
    fn a_row_follows_its_run() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut run = run_state("run-a", wall);
        let row = |run: &RunState| run_row(run, None, wall, now);
        assert_eq!(row(&run).state, WorkflowRowState::Running);
        assert_eq!(row(&run).name, "title of run-a");
        assert_eq!(row(&run).elapsed_ms, 600_000);
        assert_eq!(row(&run).measured_at, Some(now));

        // A question your rules still decide doesn't wait on you.
        run.waiting = Some(WaitReason::Approval);
        assert_eq!(row(&run).state, WorkflowRowState::Running);
        run.waiting = Some(WaitReason::Paused);
        run.paused_by = Some(Party::User);
        assert_eq!(row(&run).state, WorkflowRowState::Paused);
        run.paused_by = None;
        run.waiting = Some(WaitReason::WaitingForAgent {
            agent_id: argonauts_core::names::AgentId("codex".into()),
            until: None,
        });
        // An outage you chose to wait out, never paused.
        assert_eq!(row(&run).state, WorkflowRowState::Waiting);
        // Nothing could take a job the run needs: it stopped, and only /resume carries it on.
        run.waiting = Some(WaitReason::NoFallback {
            detail: "nobody to fall back to as the main agent".into(),
            until: None,
            stop: None,
        });
        assert_eq!(row(&run).state, WorkflowRowState::Stopped);
        run.waiting = None;
        run.pausing = true;
        assert_eq!(row(&run).state, WorkflowRowState::Pausing);
        run.pausing = false;

        // A model at work in another's place.
        let mut fallback = SessionState::new(
            argonauts_core::names::SessionKey::numbered(1),
            "claude-code/sonnet-5.5".parse().unwrap(),
            "claude-code",
            argonauts_core::names::RoleName::Worker,
            wall,
        );
        fallback.status = SessionStatus::Working;
        fallback.fallback_for = Some("claude-code/opus-5.5".parse().unwrap());
        run.sessions.insert(fallback.key.clone(), fallback);
        assert_eq!(row(&run).state, WorkflowRowState::Fallback);

        // A finished run's time stops at its end.
        run.finished = Some(RunOutcome::Failed);
        run.ended = Some(wall - time::Duration::minutes(4));
        let later = run_row(&run, None, wall + time::Duration::minutes(3), now);
        assert_eq!(later.state, WorkflowRowState::Failed);
        assert_eq!(later.elapsed_ms, 360_000);
        assert_eq!(later.measured_at, None);
        run.finished = Some(RunOutcome::Merged);
        assert_eq!(row(&run).state, WorkflowRowState::Done);
    }

    #[test]
    fn a_row_names_the_workflow_and_the_title() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut run = run_state("run-a", wall);
        run.title = "add --json to export".into();
        run.workflow = Some("council".into());
        // Understand runs before the run's own workflow, and the row keeps the
        // workflow's name through it.
        let alignment = |name: &str, parent: bool| crate::state::WorkflowState {
            id: argonauts_core::names::WorkflowId::numbered(if parent { 2 } else { 1 }),
            name: name.into(),
            from: Some("council".into()),
            description: String::new(),
            phases: vec!["Understand".into()],
            parent: parent.then(|| argonauts_core::workflow::StepRef {
                workflow: argonauts_core::names::WorkflowId::numbered(1),
                phase: 0,
                path: vec![0],
            }),
            steps: Vec::new(),
        };
        run.workflows.push(alignment("understanding", true));
        assert_eq!(
            run_row(&run, None, wall, now).name,
            "Council · add --json to export"
        );
        run.workflows.push(alignment("council", false));
        assert_eq!(
            run_row(&run, None, wall, now).name,
            "Council · add --json to export"
        );
    }

    fn waiting_summary(id: &str, waiting: bool) -> RunSummary {
        RunSummary {
            run: RunId::new(id).unwrap(),
            title: "hi".into(),
            mode: RunMode::Captain,
            workflow: "council".into(),
            state: ApiRunState::Waiting,
            stopped_at: String::new(),
            updated: OffsetDateTime::UNIX_EPOCH,
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: if waiting {
                vec![argonauts_core::names::ApprovalId::numbered(1)]
            } else {
                Vec::new()
            },
            live: false,
            remind: false,
            name: None,
            shape_known: true,
        }
    }

    /// The bottom line counts a question that waits in a run not on screen, until the run opens and
    /// its question counts with the rest.
    #[test]
    fn a_question_waiting_in_a_run_not_on_screen_counts_as_needing_you() {
        let wall = OffsetDateTime::now_utc();
        let mut state = AppState::default();
        state.background = vec![
            waiting_summary("run-001", true),
            waiting_summary("run-002", false),
        ];
        assert_eq!(state.background_waiting(), 1);
        state
            .runs
            .insert(RunId::new("run-001").unwrap(), run_state("run-001", wall));
        assert_eq!(state.background_waiting(), 0);
    }

    /// A run this screen doesn't stream draws no row while its workflow is written, as the one
    /// it streams doesn't: it shows once its summary says the shape is known. What waits on you
    /// and how a run ended still show.
    #[test]
    fn a_run_not_on_screen_has_no_row_until_its_shape_is_known() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut state = AppState::default();
        let mut written = waiting_summary("run-001", false);
        written.state = ApiRunState::Running;
        written.shape_known = false;
        state.background = vec![written.clone()];
        assert!(run_rows(&state, wall, now).is_empty());

        let asking = argonauts_core::api::RunSummary {
            run: RunId::new("run-002").unwrap(),
            state: ApiRunState::Waiting,
            ..waiting_summary("run-002", true)
        };
        let ended = argonauts_core::api::RunSummary {
            run: RunId::new("run-003").unwrap(),
            state: ApiRunState::Merged,
            ..written.clone()
        };
        state.background = vec![
            written.clone(),
            argonauts_core::api::RunSummary {
                shape_known: false,
                ..asking
            },
            ended,
        ];
        let keys: Vec<String> = run_rows(&state, wall, now)
            .into_iter()
            .map(|row| row.key)
            .collect();
        assert_eq!(keys, ["run-002", "run-003"]);

        written.shape_known = true;
        state.background = vec![written];
        let rows = run_rows(&state, wall, now);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "run-001");
    }

    #[test]
    fn a_run_not_on_screen_keeps_its_row_until_it_is_opened() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut state = AppState::default();
        state.background = vec![
            waiting_summary("run-001", true),
            waiting_summary("run-002", false),
        ];
        state
            .runs
            .insert(RunId::new("run-003").unwrap(), begun("run-003", wall));

        let rows = run_rows(&state, wall, now);
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["run-001", "run-002", "run-003"]);
        assert_eq!(rows[0].state, WorkflowRowState::NeedsYou);
        assert_eq!(rows[0].name, "Captain · hi");
        assert!(!rows[0].timed, "its time isn't known here");
        // A wait the listing gives no words for isn't a pause you asked for.
        assert_eq!(rows[1].state, WorkflowRowState::Waiting);
        assert!(rows[2].timed);

        // A run that has its own name goes by it, as its row will once it's open.
        state.background[1].name = Some("say-hi".to_owned());
        assert_eq!(run_rows(&state, wall, now)[1].name, "say-hi");

        // Opened, the run is held here, and its own row takes the place of the summary's.
        state
            .runs
            .insert(RunId::new("run-001").unwrap(), begun("run-001", wall));
        let rows = run_rows(&state, wall, now);
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["run-002", "run-001", "run-003"]);
        assert!(rows[1].timed);
    }

    /// A second workflow took the screen: the first goes on off screen, and its row says what
    /// the listing reads of it rather than what this screen last saw.
    #[test]
    fn a_run_that_left_the_screen_follows_its_listing_and_goes_once_it_ends() {
        use argonauts_core::app::AppEvent;
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let first = RunId::new("run-001").unwrap();
        let second = RunId::new("run-002").unwrap();
        let mut state = AppState::default();
        state.runs.insert(first.clone(), begun("run-001", wall));
        state.runs.insert(second.clone(), begun("run-002", wall));
        state.attached = Some(second.clone());
        let summary = |waiting: bool| RunSummary {
            spent: Spend {
                output_tokens: 900,
                ..Spend::default()
            },
            ..waiting_summary("run-001", waiting)
        };
        let listing = |waiting: bool| AppEvent::BackgroundRuns {
            runs: vec![summary(waiting)],
        };
        let row = |state: &AppState| {
            run_rows(state, wall, now)
                .into_iter()
                .find(|row| row.key == "run-001")
        };

        // It left building, and the listing came back saying it waits on you.
        assert_eq!(row(&state).unwrap().state, WorkflowRowState::Running);
        state.apply(&listing(true));
        let waiting = row(&state).unwrap();
        assert_eq!(waiting.state, WorkflowRowState::NeedsYou);
        assert_eq!(waiting.tokens, 900);
        assert!(waiting.timed, "it keeps what this screen saw of it");

        // A listing that leaves it out tells it ended there, and its row goes.
        state.apply(&AppEvent::BackgroundRuns { runs: Vec::new() });
        assert!(row(&state).is_none());
        let keys: Vec<String> = run_rows(&state, wall, now)
            .into_iter()
            .map(|row| row.key)
            .collect();
        assert_eq!(keys, ["run-002"], "the run on screen keeps its row");

        // Resumed in another window, it is listed again and has its row back.
        state.apply(&listing(false));
        assert_eq!(row(&state).unwrap().state, WorkflowRowState::Waiting);
    }

    #[test]
    fn two_runs_never_share_a_name_and_an_agents_own_workflow_keeps_its_own() {
        let row = |name: &str, parent: Option<&str>| {
            let mut row = WorkflowRow::new(name, name, WorkflowRowState::Running);
            row.depth = usize::from(parent.is_some());
            row
        };
        let mut rows = vec![
            row("add-due-dates-tasks", None),
            row("add-due-dates-tasks", Some("s-002")),
            row("add-due-dates-tasks", None),
            row("council · add --json", None),
            row("council · add --json", None),
            row("add-due-dates-tasks", None),
        ];
        number_alike(&mut rows);
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "add-due-dates-tasks",
                "add-due-dates-tasks",
                "add-due-dates-tasks-2",
                "council · add --json",
                "council · add --json 2",
                "add-due-dates-tasks-3",
            ]
        );
    }

    /// An agent whose session names no model shows the logo of the model it runs: the one your
    /// settings give its CLI.
    #[test]
    fn an_agent_that_names_no_model_shows_the_logo_of_the_one_it_runs() {
        use argonauts_core::logos::{LogoMark, logo_for};
        use argonauts_core::names::{RoleName, SessionKey};

        let wall = OffsetDateTime::now_utc();
        let mut state = AppState::default();
        state
            .names
            .configured
            .insert("opencode".to_owned(), "ollama/qwen3-coder:30b".to_owned());
        let mut run = run_state("run-oc", wall);
        let mut session = SessionState::new(
            SessionKey::numbered(1),
            "opencode".parse().unwrap(),
            "opencode",
            RoleName::Worker,
            wall,
        );
        session.status = SessionStatus::Working;
        run.session_order.push(session.key.clone());
        run.sessions.insert(session.key.clone(), session);
        state.runs.insert(run.id.clone(), run);

        let rows = agent_rows(&state, wall, Instant::now());
        let [row] = rows.as_slice() else {
            panic!("one row: {rows:?}");
        };
        assert_eq!(row.model.as_deref(), Some("ollama/qwen3-coder:30b"));
        assert_eq!(
            logo_for(&row.name, row.model.as_deref()).mark,
            Some(LogoMark::Ollama)
        );
    }
}
