//! The panel under the mode line: one row per run (its workflow), and with `ui.panel.agents`
//! each run's running agents under it.

use std::time::{Duration, Instant};

use argonauts_core::app::StatusLine;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::{ParkReason, RunOutcome};
use argonauts_core::events::session::{VendorAgentState, VendorWorkflow};
use argonauts_core::settings::budget::BudgetUnit;
use argonauts_core::views::vendor_workflow_id;
use argonauts_core::{Spend, TodoStatus};
use time::OffsetDateTime;

use crate::bottom::{AgentRow, AgentState, WorkflowRow, WorkflowRowState};
use crate::state::{AppState, RunState, SessionState, SessionStatus};
use crate::views::panes;

use super::Cx;

/// Past this share of its budget, a run's row warns.
const BUDGET_WARN_SHARE: f64 = 0.8;

pub fn sync(cx: &mut Cx<'_>) {
    let wall = OffsetDateTime::now_utc();
    let panel = &cx.settings.panel;
    let bottom = &mut cx.model.bottom;
    let keep = bottom.workflows.selected_id(&bottom.agents);
    let rows = run_rows(cx.state, wall, cx.now);
    bottom.agents.rows = if panel.agents && !rows.is_empty() {
        agent_rows(cx.state, wall, cx.now)
    } else {
        Vec::new()
    };
    bottom.workflows.rows = rows;
    bottom.workflows.max_rows = panel.max_rows as usize;
    bottom.workflows.show_agents = panel.agents;
    bottom.agents.vendor_internals = panel.vendor_internals;
    bottom.workflows.reselect(&bottom.agents, keep.as_ref());
}

/// One row per run still going, oldest first, then one for each workflow an agent's own CLI
/// runs in it. A finished run leaves the panel once nothing waits on you;
/// its closing answer is at the end of the conversation.
pub fn run_rows(state: &AppState, wall: OffsetDateTime, now: Instant) -> Vec<WorkflowRow> {
    let mut runs: Vec<&RunState> = state
        .runs
        .values()
        .filter(|run| run.finished.is_none() || !run.checkpoints.is_empty())
        .collect();
    runs.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
    runs.into_iter()
        .flat_map(|run| {
            std::iter::once(run_row(run, state.status.as_ref(), wall, now))
                .chain(vendor_rows(run, wall, now))
        })
        .collect()
}

/// The key of a panel row for a workflow an agent's own CLI runs: its run, `#`, and the id the
/// views give it.
pub(crate) fn vendor_key(run: &RunState, workflow: &VendorWorkflow) -> String {
    format!("{}#{}", run.id, vendor_workflow_id(&workflow.id))
}

/// A row for each workflow an agent's own CLI is running, as Claude Code's panel lists its own.
/// Its time counts from its start, so a report of its totals never sets it back.
fn vendor_rows(run: &RunState, wall: OffsetDateTime, now: Instant) -> Vec<WorkflowRow> {
    let wall_ms = u64::try_from((wall - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok();
    run.sessions_in_order()
        .flat_map(|session| &session.workflows)
        .filter(|workflow| !workflow.ended())
        .map(|workflow| {
            let name = Some(workflow.name.trim())
                .filter(|name| !name.is_empty())
                .unwrap_or("workflow");
            let mut row =
                WorkflowRow::new(vendor_key(run, workflow), name, WorkflowRowState::Running);
            row.agents_total = u32::try_from(workflow.agents.len()).unwrap_or(u32::MAX);
            row.agents_done = u32::try_from(
                workflow
                    .agents
                    .iter()
                    .filter(|agent| agent.state == VendorAgentState::Done)
                    .count(),
            )
            .unwrap_or(u32::MAX);
            row.elapsed_ms = workflow.elapsed_ms(wall_ms);
            row.measured_at = Some(now);
            row.tokens = workflow.tokens;
            row.phase = workflow
                .agents
                .iter()
                .rev()
                .find(|agent| agent.state == VendorAgentState::Running)
                .and_then(|agent| agent.phase)
                .and_then(|index| workflow.phases.iter().find(|p| p.index == index))
                .map(|phase| phase.title.clone());
            row
        })
        .collect()
}

/// The run's own name, `fix-slugify-median`, or before it has one, its workflow and title,
/// `peer-crew · add --json to export`.
fn run_name(run: &RunState) -> String {
    if let Some(named) = run
        .named
        .as_ref()
        .map(|named| named.name.trim())
        .filter(|name| !name.is_empty())
    {
        return named.to_owned();
    }
    let title = run.title.trim();
    let workflow = workflow_name(run);
    match (workflow, title.is_empty()) {
        (Some(workflow), false) => format!("{workflow} · {title}"),
        (Some(workflow), true) => workflow.to_owned(),
        (None, false) => title.to_owned(),
        (None, true) => run.id.to_string(),
    }
}

/// The run's own workflow. Before the lead picks it, a goal alignment runs ahead of it, so the
/// row names the workflow the alignment came from, then the preset, rather than `goal`.
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
        .or_else(|| run.preset.as_deref().and_then(named))
        .or_else(|| run.workflows.first().and_then(|first| named(&first.name)))
}

fn run_row(
    run: &RunState,
    status: Option<&StatusLine>,
    wall: OffsetDateTime,
    now: Instant,
) -> WorkflowRow {
    let totals = run.totals(wall);
    let mut row = WorkflowRow::new(run.id.as_str(), run_name(run), row_state(run));
    row.agents_total = totals.agents_total;
    row.agents_done = totals.agents_done;
    let session_tokens: u64 = run
        .sessions
        .values()
        .map(|session| session.tokens_out)
        .sum();
    row.tokens = session_tokens.max(totals.spent.output_tokens);
    row.spend = panes::spend(&totals.spent);
    row.front_end = run
        .front_end
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| format!("via {name}"));
    row.elapsed_ms = totals.elapsed.map_or(0, millis);
    row.phase = run.workflow_phase().map(str::to_owned);
    if run.finished.is_none() {
        row.measured_at = Some(now);
    }
    row.notice = parked_notice(run).or_else(|| {
        let share = budget_share(status?, run, Duration::from_millis(row.elapsed_ms))?;
        (share >= BUDGET_WARN_SHARE && run.finished.is_none()).then(|| {
            format!(
                "{}% of budget · /workflows to stop",
                (share * 100.0).floor() as u64
            )
        })
    });
    row
}

fn row_state(run: &RunState) -> WorkflowRowState {
    match (&run.finished, &run.parked) {
        (Some(RunOutcome::Failed), _) => WorkflowRowState::Failed,
        (Some(RunOutcome::Stopped | RunOutcome::Abandoned), _) => WorkflowRowState::Stopped,
        (Some(_), _) => WorkflowRowState::Done,
        (None, _) if !run.checkpoints.is_empty() => WorkflowRowState::NeedsYou,
        (None, Some(ParkReason::Checkpoint)) => WorkflowRowState::NeedsYou,
        (None, Some(_)) => WorkflowRowState::Paused,
        (None, None) if run.pausing => WorkflowRowState::Paused,
        (None, None) => WorkflowRowState::Running,
    }
}

fn parked_notice(run: &RunState) -> Option<String> {
    if run.finished.is_some() {
        return None;
    }
    match &run.parked {
        Some(ParkReason::AllVendorsOut { .. }) => Some("every vendor is out".to_owned()),
        Some(ParkReason::WaitingForVendor { vendor, .. }) => Some(format!("waiting for {vendor}")),
        _ if run.pausing && run.parked.is_none() => Some("pausing at a safe point".to_owned()),
        _ => None,
    }
}

/// The share of the budget the status line's run has used, in its limit's unit, counting the
/// spend the mode line shows. Quota limits are shown by the quota chips instead.
fn budget_share(status: &StatusLine, run: &RunState, elapsed: Duration) -> Option<f64> {
    if status.run.as_ref() != Some(&run.id) {
        return None;
    }
    let limit = status.limit.as_ref().filter(|limit| limit.amount > 0.0)?;
    let spent = run.spend();
    let used = match limit.unit {
        BudgetUnit::Usd => spent.cost_usd,
        BudgetUnit::Tokens => spent.total_tokens() as f64,
        BudgetUnit::Minutes => elapsed.as_secs_f64() / 60.0,
        BudgetUnit::Quota => return None,
    };
    Some(used / limit.amount)
}

/// Every run's running agents, run by run, with their markers.
pub fn agent_rows(state: &AppState, wall: OffsetDateTime, now: Instant) -> Vec<AgentRow> {
    state
        .runs
        .values()
        .flat_map(|run| {
            run.sessions_in_order()
                .filter(|session| session.is_running())
                .map(move |session| agent_row(run, session, &state.names, wall, now))
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
    let card = session.card.as_ref().and_then(|id| run.cards.get(id));
    let task = session.task();
    let state = match session.status {
        SessionStatus::Starting | SessionStatus::Working => AgentState::Working,
        SessionStatus::Waiting | SessionStatus::Stopped => AgentState::Waiting,
        SessionStatus::Sleeping => AgentState::Timer,
        SessionStatus::Done => AgentState::Done,
        SessionStatus::Failed => AgentState::Failed,
    };
    let worker = &session.worker;
    let mut row = AgentRow::new(worker.worker.clone(), task, state)
        .with_label(names.worker(worker), names.harness(&worker.worker));
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
    // Under its run's row an agent goes by its label, `build:flag`, as Claude Code's do.
    row.card = session
        .label
        .clone()
        .or_else(|| session.card.as_ref().map(|card| run.card_label(card)));
    row.markers.context = session.context.map(|context| (context.used, context.cap));
    row.markers.stands_in_for = session
        .standing_in_for
        .as_ref()
        .or_else(|| card.and_then(|card| card.standing_in_for.as_ref()))
        .map(|original| names.worker(original));
    row.markers.needs_review = card.is_some_and(|card| !card.debt.is_empty());
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
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::budget::BudgetAmount;
    use argonauts_core::settings::ui::NarratorMode;

    use super::*;

    fn run_state(id: &str, wall: OffsetDateTime) -> RunState {
        let mut run = RunState::new(RunId::new(id).unwrap());
        run.title = format!("title of {id}");
        run.started = Some(wall - time::Duration::minutes(10));
        run
    }

    fn status(run: &str, limit: BudgetAmount) -> StatusLine {
        StatusLine {
            title: String::new(),
            repo: "~/code".into(),
            preset: "peer-crew".into(),
            all_hands: false,
            run: Some(RunId::new(run).unwrap()),
            default_agent: "claude-code/opus".parse().unwrap(),
            quotas: Vec::new(),
            spent: None,
            limit: Some(limit),
            narrator: NarratorMode::Off,
            approval_mode: ApprovalMode::Supervised,
        }
    }

    #[test]
    fn budget_notice_warns_near_the_limit() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut state = AppState::default();
        state
            .runs
            .insert(RunId::new("run-a").unwrap(), run_state("run-a", wall));
        state
            .runs
            .insert(RunId::new("run-b").unwrap(), run_state("run-b", wall));
        let usd = BudgetAmount {
            unit: BudgetUnit::Usd,
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
            Some("85% of budget · /workflows to stop")
        );
        // Only the status line's run has a known budget.
        assert_eq!(rows[1].notice, None);

        spend(&mut state, 3.0);
        assert_eq!(run_rows(&state, wall, now)[0].notice, None);

        // A minutes budget counts the run's time.
        let minutes = BudgetAmount {
            unit: BudgetUnit::Minutes,
            amount: 11.0,
        };
        state.status = Some(status("run-a", minutes));
        assert_eq!(
            run_rows(&state, wall, now)[0].notice.as_deref(),
            Some("90% of budget · /workflows to stop")
        );
    }

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

        run.parked = Some(ParkReason::Checkpoint);
        assert_eq!(row(&run).state, WorkflowRowState::NeedsYou);
        run.parked = Some(ParkReason::Paused);
        assert_eq!(row(&run).state, WorkflowRowState::Paused);
        run.parked = Some(ParkReason::WaitingForVendor {
            vendor: argonauts_core::names::VendorId("codex".into()),
            until: None,
        });
        assert_eq!(row(&run).notice.as_deref(), Some("waiting for codex"));

        // A finished run's time stops at its end.
        run.parked = None;
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
    fn a_row_names_the_workflow_and_the_title_and_shows_the_spend() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut run = run_state("run-a", wall);
        run.title = "add --json to export".into();
        run.preset = Some("peer-crew".into());
        // The goal's alignment runs before the run's own workflow, and the row keeps the
        // workflow's name through it.
        let alignment = |name: &str, parent: bool| crate::state::WorkflowState {
            id: argonauts_core::names::WorkflowId::numbered(if parent { 2 } else { 1 }),
            name: name.into(),
            from: Some("peer-crew".into()),
            description: String::new(),
            phases: vec!["Goal".into()],
            parent: parent.then(|| argonauts_core::workflow::StepRef {
                workflow: argonauts_core::names::WorkflowId::numbered(1),
                phase: 0,
                path: vec![0],
            }),
            steps: Vec::new(),
        };
        run.workflows.push(alignment("goal", true));
        assert_eq!(
            run_row(&run, None, wall, now).name,
            "peer-crew · add --json to export"
        );
        run.workflows.push(alignment("peer-crew", false));
        assert_eq!(
            run_row(&run, None, wall, now).name,
            "peer-crew · add --json to export"
        );

        // The sessions' own reports count when the budget's count lags.
        let mut session = SessionState::new(
            argonauts_core::names::SessionKey::numbered(1),
            "claude-code".parse().unwrap(),
            "claude-code",
            argonauts_core::names::RoleName::Owner,
            wall,
        );
        session.cost_usd = Some(0.21);
        run.sessions.insert(session.key.clone(), session);
        assert_eq!(
            run_row(&run, None, wall, now).spend.as_deref(),
            Some("$0.21")
        );
    }
}
