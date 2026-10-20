//! The panel under the bottom line: one row per run (its workflow), and with `ui.panel.agents`
//! each run's running agents under it.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::api::{RunState as ApiRunState, RunSummary};
use argonauts_core::app::HeaderAndBottomLine;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::{RunMode, RunOutcome, WaitReason};
use argonauts_core::events::session::{CliAgentState, CliWorkflowRun, SubagentStatus};
use argonauts_core::hidden_git::MergedInto;
use argonauts_core::settings::limit::LimitUnit;
use argonauts_core::views::{cli_workflow_run_id, conversation_run};
use argonauts_core::{Spend, TodoStatus};
use time::OffsetDateTime;

use crate::bottom::{AgentRow, AgentState, RowKind, WorkflowRow, WorkflowRowState};
use crate::state::{AppState, RunState, RunTotals, SessionState, SessionStatus, binding_reading};
use crate::views::panes;

use super::Cx;

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
        let mut row = run_row(run, state.status.as_ref(), &state.names, wall, now);
        if let Some(listed) = state.listed_off_screen(&run.id) {
            follow_listing(&mut row, listed);
        }
        // A run that ended keeps no goal going, whatever its record last said of one.
        if run.finished.is_none() {
            row.goals_running = state.goals.running(&run.id);
        }
        // Nothing of it waits on you, so its row goes when its time is up.
        if run.approvals.is_empty() {
            row.leaves_at = kept_for(run, wall).map(|left| now + left);
        }
        let inside = inside_rows(run, wall, now);
        std::iter::once(row).chain(inside)
    });
    // A workflow the conversation's own CLI runs gets a row as a run's does.
    let conversation = conversation_run();
    let conversation = state
        .conversation
        .workflows
        .iter()
        .filter(|workflow| !workflow.ended())
        .map(|workflow| {
            let mut row = cli_workflow_row(&conversation, workflow, wall, now);
            row.goals_running = state.goals.running(&conversation);
            row
        });
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
/// to the next, then the agents of its own that work after them, such as the merge's.
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
    let after = run.sessions_in_order().filter(|session| {
        argonauts_core::views::is_workflow_agent(session.step, session.workflow_step.as_ref())
            && session
                .workflow_step
                .as_ref()
                .is_some_and(|step| !hosts.contains(&step.phase))
    });
    let (after_done, after_all) = after.fold((0, 0), |(done, all), session| {
        (
            done + usize::from(session.status == SessionStatus::Done),
            all + 1,
        )
    });
    let (asked_done, asked_all) = asked_of_its_own(run, &own, &hosts);
    totals.agents_done = u32::try_from(done + after_done + asked_done).unwrap_or(u32::MAX);
    totals.agents_total =
        u32::try_from(agents().count() + after_all + asked_all).unwrap_or(u32::MAX);
    totals
}

/// The reviewers and judges the task's workflows asked of Argonauts Code, done and in all, that
/// fill no row of those workflows: each answer joins the row that asked, by the label its ask
/// kept, so only an ask under a label none of their agents has, as between two of its
/// workflows, is a row of its own in the view, and an agent once across its rounds.
fn asked_of_its_own(run: &RunState, own: &[&CliWorkflowRun], hosts: &[u32]) -> (usize, usize) {
    let named = |label: &str| {
        own.iter()
            .flat_map(|workflow| &workflow.agents)
            .any(|agent| agent.label.trim() == label)
    };
    let mut asks: std::collections::BTreeMap<&str, bool> = Default::default();
    for session in run.sessions_in_order() {
        let in_a_host = session
            .workflow_step
            .as_ref()
            .is_some_and(|step| hosts.contains(&step.phase));
        let asked = in_a_host
            && session.workflows.is_empty()
            && argonauts_core::views::is_workflow_agent(
                session.step,
                session.workflow_step.as_ref(),
            );
        let label = session.label.as_deref().map(str::trim).unwrap_or_default();
        if !asked || label.is_empty() {
            continue;
        }
        let stem = argonauts_core::views::round_stem(label).unwrap_or(label);
        if named(label) || named(stem) {
            continue;
        }
        asks.insert(stem, session.status == SessionStatus::Done);
    }
    let done = asks.values().filter(|done| **done).count();
    (done, asks.len())
}

/// A `⎿` row for each workflow at work inside the run, a level further in for each one that
/// holds it: one a worker's own CLI runs, which names that worker, a later one the run's task
/// starts, and a child either of them starts, which its CLI reports as a phase named for it.
/// The run's own CLI workflow is the run's row, so only its children have rows. Each leaves
/// once it ends, and the run's count keeps its agents.
fn inside_rows(run: &RunState, wall: OffsetDateTime, now: Instant) -> Vec<WorkflowRow> {
    let wall_ms = u64::try_from((wall - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok();
    let own = the_runs_own(run).map(|workflow| workflow.id.as_str());
    let tasks_own: Vec<&str> = the_tasks_own(run)
        .iter()
        .map(|workflow| workflow.id.as_str())
        .collect();
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
                row.depth = 1;
                // A worker's own workflow says whose it is; a later one of the task's is the
                // run's own carrying on.
                if !tasks_own.contains(&workflow.id.as_str()) {
                    row.run_by = session
                        .label
                        .clone()
                        .or_else(|| Some(session.agent_id.clone()));
                }
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
            row.depth = depth;
            row.agents_total = u32::try_from(agents.len()).unwrap_or(u32::MAX);
            row.agents_done = u32::try_from(
                agents
                    .iter()
                    .filter(|agent| agent.state == CliAgentState::Done)
                    .count(),
            )
            .unwrap_or(u32::MAX);
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
    row.agents_total = u32::try_from(workflow.agents.len()).unwrap_or(u32::MAX);
    row.agents_done = u32::try_from(
        workflow
            .agents
            .iter()
            .filter(|agent| agent.state == CliAgentState::Done)
            .count(),
    )
    .unwrap_or(u32::MAX);
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
        WorkflowRowState::Paused
    } else if run.state == ApiRunState::Waiting {
        // The listing words why it waits, as its journal ends.
        let why = run.stopped_at.as_str();
        // Only a pause you asked for reads paused.
        if why.starts_with("stopped") {
            WorkflowRowState::Stopped
        } else if why == "paused" {
            WorkflowRowState::Paused
        } else {
            WorkflowRowState::Waiting
        }
    } else {
        WorkflowRowState::Running
    }
}

/// A held run's row once it goes on off screen: what this screen last saw of its agents, with
/// its state and tokens as the listing reads them from its journal. What the row said of the
/// run while this screen held it would be stale, so it goes.
fn follow_listing(row: &mut WorkflowRow, listed: &RunSummary) {
    row.state = listed_state(listed);
    row.said = None;
    row.fallback = None;
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
    names: &DisplayNames,
    wall: OffsetDateTime,
    now: Instant,
) -> WorkflowRow {
    let totals = run_totals(run, wall);
    let mut row = WorkflowRow::new(run.id.as_str(), run_name(run), row_state(run));
    row.agents_total = totals.agents_total;
    row.agents_done = totals.agents_done;
    row.tokens = totals.tokens;
    row.said = row_says(run);
    row.carry_on = carry_on(run);
    row.fallback = filling_in(run, names);
    row.outside_agent = run
        .outside_agent
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| format!("via {name}"));
    row.elapsed_ms = totals.elapsed.map_or(0, millis);
    if run.finished.is_none() {
        row.measured_at = Some(now);
    }
    row.notice = waiting_notice(run).or_else(|| {
        let share = limit_share(status?, run, Duration::from_millis(row.elapsed_ms))?;
        (share >= LIMIT_WARN_SHARE && run.finished.is_none()).then(|| {
            format!(
                "{}% of limit · /workflows to stop",
                (share * 100.0).floor() as u64
            )
        })
    });
    row
}

/// What a run's row says at its end, when there is something to say: how it ended, or the
/// question that waits for you.
fn row_says(run: &RunState) -> Option<String> {
    if let Some(outcome) = run.finished {
        return Some(match (outcome, run.merged_onto.as_deref()) {
            // In a folder without git the work was written in: no branch of yours took it.
            (RunOutcome::Merged, Some(target)) => MergedInto::of(target).ended(),
            (RunOutcome::Merged, None) => "merged".to_owned(),
            (RunOutcome::PullRequest, _) => "a pull request is open".to_owned(),
            (RunOutcome::BranchLeft, _) => "its branch was kept".to_owned(),
            (RunOutcome::Stopped, _) => "stopped · /resume carries it on".to_owned(),
            (RunOutcome::Abandoned, _) => "abandoned".to_owned(),
            (RunOutcome::Failed, _) => "failed".to_owned(),
        });
    }
    match &run.waiting {
        Some(WaitReason::Paused) => return Some("paused by you".to_owned()),
        Some(WaitReason::NoFallback { .. }) => return Some("stopped".to_owned()),
        Some(WaitReason::AllAgentsOut { until }) => return Some(waiting_for("an agent", *until)),
        Some(WaitReason::WaitingForAgent { agent_id, until }) => {
            let cli = DisplayNames::default().cli(&agent_id.0);
            return Some(waiting_for(&cli, *until));
        }
        _ => {}
    }
    let asks = run
        .approvals
        .iter()
        .rev()
        .find(|open| open.opened.needs_you);
    // The question's headline: what follows its first ` · ` is on the question itself.
    let title = asks?.opened.title.trim();
    let headline = title.split_once(" · ").map_or(title, |(first, _)| first);
    Some(headline.to_owned()).filter(|headline| !headline.is_empty())
}

/// `waiting for Claude Code · tries again at 1:14 AM`, for an outage you chose to wait out.
fn waiting_for(who: &str, until: Option<OffsetDateTime>) -> String {
    match until {
        Some(until) => format!(
            "waiting for {who} · tries again at {}",
            crate::format::WallClock::default().clock_rounded_up(until)
        ),
        None => format!("waiting for {who}"),
    }
}

/// How a run that stopped or that you paused goes on: only `/resume` carries it on.
fn carry_on(run: &RunState) -> Option<String> {
    // A run you stopped says it in one breath, as its row always has.
    let stopped = run.finished.is_none()
        && matches!(
            run.waiting,
            Some(WaitReason::Paused | WaitReason::NoFallback { .. })
        );
    stopped.then(|| "/resume carries it on".to_owned())
}

/// `Sonnet 5.5 for Opus 5.5`: the model at work in another's place, while the run goes on.
fn filling_in(run: &RunState, names: &DisplayNames) -> Option<String> {
    if run.finished.is_some() {
        return None;
    }
    run.sessions
        .values()
        .filter(|session| session.is_running())
        .find_map(|session| {
            let first = session.fallback_for.as_ref()?;
            Some(format!(
                "{} for {}",
                names.short(&session.worker),
                names.short(first)
            ))
        })
}

fn row_state(run: &RunState) -> WorkflowRowState {
    match (&run.finished, &run.waiting) {
        (Some(outcome), _) => WorkflowRowState::ended(*outcome),
        (None, _) if !run.approvals.is_empty() => WorkflowRowState::NeedsYou,
        (None, Some(WaitReason::Approval)) => WorkflowRowState::NeedsYou,
        // Only a pause you asked for says paused.
        (None, Some(WaitReason::Paused)) => WorkflowRowState::Paused,
        (None, Some(WaitReason::NoFallback { .. })) => WorkflowRowState::Stopped,
        (None, Some(WaitReason::AllAgentsOut { .. } | WaitReason::WaitingForAgent { .. })) => {
            WorkflowRowState::Waiting
        }
        (None, None) if run.pausing => WorkflowRowState::Paused,
        (None, None) => WorkflowRowState::Running,
    }
}

fn waiting_notice(run: &RunState) -> Option<String> {
    if run.finished.is_some() {
        return None;
    }
    match &run.waiting {
        _ if run.pausing && run.waiting.is_none() => Some("pausing at a safe point".to_owned()),
        _ => None,
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
    row.model = worker.model.clone();
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
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::events::run::RunOutcome;
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
        assert_eq!((rows[0].agents_done, rows[0].agents_total), (2, 3));

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
        assert_eq!((rows[0].agents_done, rows[0].agents_total), (3, 5));
        // While it runs it has a row of its own under the run's, the run's own carrying on.
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!((rows[1].name.as_str(), rows[1].depth), ("final-signoff", 1));
        assert_eq!(rows[1].run_by, None);
    }

    /// A worker's own workflow is a row under its run's while it runs, naming the worker, and
    /// a child its script starts is one more a step further in. The run counts their agents.
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
                format!(
                    "{} · {} in · by {} · {}/{}",
                    row.name,
                    row.depth,
                    row.run_by.as_deref().unwrap_or("-"),
                    row.agents_done,
                    row.agents_total
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                "title of run-due · 0 in · by - · 4/6",
                "add-due-flag · 1 in · by build:due-flag · 3/4",
                "date-cases · 2 in · by - · 1/2",
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
    fn limit_notice_warns_near_the_limit() {
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
        assert_eq!(
            rows[0].notice.as_deref(),
            Some("85% of limit · /workflows to stop")
        );
        // Only the bottom line's run has a known limit.
        assert_eq!(rows[1].notice, None);

        spend(&mut state, 3.0);
        assert_eq!(run_rows(&state, wall, now)[0].notice, None);

        // A minutes limit counts the run's time.
        let minutes = LimitAmount {
            unit: LimitUnit::Minutes,
            amount: 11.0,
        };
        state.status = Some(status("run-a", minutes));
        assert_eq!(
            run_rows(&state, wall, now)[0].notice.as_deref(),
            Some("90% of limit · /workflows to stop")
        );
    }

    #[test]
    fn a_row_follows_its_run() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut run = run_state("run-a", wall);
        let names = DisplayNames::default();
        let row = |run: &RunState| run_row(run, None, &names, wall, now);
        assert_eq!(row(&run).state, WorkflowRowState::Running);
        assert_eq!(row(&run).name, "title of run-a");
        assert_eq!(row(&run).elapsed_ms, 600_000);
        assert_eq!(row(&run).measured_at, Some(now));

        run.waiting = Some(WaitReason::Approval);
        assert_eq!(row(&run).state, WorkflowRowState::NeedsYou);
        run.waiting = Some(WaitReason::Paused);
        assert_eq!(row(&run).state, WorkflowRowState::Paused);
        run.waiting = Some(WaitReason::WaitingForAgent {
            agent_id: argonauts_core::names::AgentId("codex".into()),
            until: None,
        });
        // An outage you chose to wait out says what it waits for, never paused.
        assert_eq!(row(&run).state, WorkflowRowState::Waiting);
        assert_eq!(row(&run).said.as_deref(), Some("waiting for Codex"));
        assert_eq!(row(&run).notice, None);
        // Nothing could take a job the run needs: it stopped, and only /resume carries it on.
        run.waiting = Some(WaitReason::NoFallback {
            detail: "nobody to fall back to as the main agent".into(),
            until: None,
            stop: None,
        });
        assert_eq!(row(&run).state, WorkflowRowState::Stopped);
        assert_eq!(row(&run).said.as_deref(), Some("stopped"));
        assert_eq!(row(&run).carry_on.as_deref(), Some("/resume carries it on"));
        // Only a pause you asked for says paused.
        run.waiting = Some(WaitReason::Paused);
        assert_eq!(row(&run).said.as_deref(), Some("paused by you"));
        assert_eq!(row(&run).carry_on.as_deref(), Some("/resume carries it on"));

        // A finished run's time stops at its end.
        run.waiting = None;
        run.finished = Some(RunOutcome::Failed);
        run.ended = Some(wall - time::Duration::minutes(4));
        let later = run_row(&run, None, &names, wall + time::Duration::minutes(3), now);
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
            run_row(&run, None, &DisplayNames::default(), wall, now).name,
            "Council · add --json to export"
        );
        run.workflows.push(alignment("council", false));
        assert_eq!(
            run_row(&run, None, &DisplayNames::default(), wall, now).name,
            "Council · add --json to export"
        );

        // How it ended is said at the row's end, with the branch it merged onto.
        run.finished = Some(RunOutcome::Merged);
        run.merged_onto = Some("main".into());
        assert_eq!(
            run_row(&run, None, &DisplayNames::default(), wall, now)
                .said
                .as_deref(),
            Some("merged onto main")
        );
        // A folder without git has no branch to name: the hidden history's own never shows.
        run.merged_onto = Some(argonauts_core::hidden_git::BRANCH.into());
        assert_eq!(
            run_row(&run, None, &DisplayNames::default(), wall, now)
                .said
                .as_deref(),
            Some("written into your folder")
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
        let mut building = run_row(&state.runs[&first], None, &state.names, wall, now);
        building.fallback = Some("Sonnet 5.5 for Opus 5.5".into());
        follow_listing(&mut building, &summary(true));
        assert_eq!(
            building.fallback, None,
            "who filled in while on screen is stale"
        );

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
}
