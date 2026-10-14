//! The panel under the mode line: one row per run (its workflow), and with `ui.panel.agents`
//! each run's running agents under it.

use std::time::{Duration, Instant};

use argonauts_core::RunId;
use argonauts_core::api::{RunState as ApiRunState, RunSummary};
use argonauts_core::app::StatusLine;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::{ParkReason, RunMode};
use argonauts_core::events::session::{SubagentStatus, VendorAgentState, VendorWorkflow};
use argonauts_core::settings::budget::BudgetUnit;
use argonauts_core::views::{conversation_run, vendor_workflow_id};
use argonauts_core::{Spend, TodoStatus};
use time::OffsetDateTime;

use crate::bottom::{AgentRow, AgentState, RowKind, WorkflowRow, WorkflowRowState};
use crate::state::{AppState, RunState, SessionState, SessionStatus, binding_reading};
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
    bottom.agents.vendor_internals = panel.vendor_internals;
    bottom.workflows.reselect(&bottom.agents, keep.as_ref());
}

/// One row per run still going, oldest first, then one for each workflow an agent's own CLI
/// runs in it. The runs this screen doesn't hold, from an earlier session, come
/// first. A finished run leaves the panel once nothing waits on you; its closing answer
/// is at the end of the conversation.
pub fn run_rows(state: &AppState, wall: OffsetDateTime, now: Instant) -> Vec<WorkflowRow> {
    let mut background: Vec<&RunSummary> = state
        .background
        .iter()
        .filter(|run| !state.runs.contains_key(&run.run))
        .collect();
    background.sort_by(|a, b| a.updated.cmp(&b.updated).then_with(|| a.run.cmp(&b.run)));
    let mut runs: Vec<&RunState> = state
        .runs
        .values()
        .filter(|run| run.finished.is_none() || !run.checkpoints.is_empty())
        .filter(|run| !state.ended_off_screen.contains(&run.id))
        .collect();
    runs.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
    let held = runs.into_iter().flat_map(|run| {
        let mut row = run_row(run, state.status.as_ref(), wall, now);
        if let Some(listed) = state.listed_off_screen(&run.id) {
            follow_listing(&mut row, listed);
        }
        // A run that ended keeps no goal going, whatever its record last said of one.
        if run.finished.is_none() {
            row.goals_running = state.goals.running(&run.id);
        }
        let own = the_runs_own(run);
        if let Some(workflow) = own {
            count_its_steps(&mut row, workflow);
        }
        let own_key = own.map(|workflow| vendor_key(&run.id, workflow));
        let nested = vendor_rows(run, wall, now)
            .into_iter()
            .filter(move |nested| own_key.as_ref() != Some(&nested.key));
        std::iter::once(row).chain(nested)
    });
    // A workflow the conversation's own CLI runs gets a row as a run's does.
    let conversation = conversation_run();
    let chat = state
        .chat
        .workflows
        .iter()
        .filter(|workflow| !workflow.ended())
        .map(|workflow| {
            let mut row = vendor_row(&conversation, workflow, wall, now);
            row.goals_running = state.goals.running(&conversation);
            row
        });
    let mut rows: Vec<WorkflowRow> = background
        .into_iter()
        .map(background_row)
        .chain(held)
        .chain(chat)
        .collect();
    number_alike(&mut rows);
    rows
}

/// Two runs never share a name in the list: the second of a name reads `add-due-dates-2`,
/// the third `-3`. A workflow under an agent is part of that agent's step, not a run, and keeps
/// its own.
fn number_alike(rows: &mut [WorkflowRow]) {
    let mut seen: Vec<(String, usize)> = Vec::new();
    for row in rows.iter_mut().filter(|row| row.parent.is_none()) {
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
pub(crate) fn vendor_key(run: &RunId, workflow: &VendorWorkflow) -> String {
    format!("{run}#{}", vendor_workflow_id(&workflow.id))
}

/// The CLI workflow a run is: the one that the sessions of its only task are running, as when
/// Claude Code's own workflow runs the crew's. It gets no row of its own, and the run's row
/// counts its steps.
fn the_runs_own(run: &RunState) -> Option<&VendorWorkflow> {
    let mut cards = run
        .sessions_in_order()
        .filter_map(|session| session.card.as_ref());
    let task = cards.next()?;
    if cards.any(|card| card != task) {
        return None;
    }
    run.sessions_in_order()
        .filter(|session| session.card.as_ref() == Some(task))
        .flat_map(|session| session.workflows.iter())
        .filter(|workflow| !workflow.ended())
        .last()
}

/// The run's row counting the steps of the workflow it is, with the phase going on.
fn count_its_steps(row: &mut WorkflowRow, workflow: &VendorWorkflow) {
    let done = workflow
        .agents
        .iter()
        .filter(|agent| agent.state == VendorAgentState::Done)
        .count();
    row.agents_done = u32::try_from(done).unwrap_or(u32::MAX);
    row.agents_total = u32::try_from(workflow.agents.len()).unwrap_or(u32::MAX);
    if let Some(phase) = running_phase(workflow) {
        row.phase = Some(phase);
    }
}

fn running_phase(workflow: &VendorWorkflow) -> Option<String> {
    workflow
        .agents
        .iter()
        .rev()
        .find(|agent| agent.state == VendorAgentState::Running)
        .and_then(|agent| agent.phase)
        .and_then(|index| workflow.phases.iter().find(|p| p.index == index))
        .map(|phase| phase.title.clone())
}

/// A row for each workflow an agent's own CLI is running, as Claude Code's panel lists its own.
/// Its time counts from its start, so a report of its totals never sets it back.
fn vendor_rows(run: &RunState, wall: OffsetDateTime, now: Instant) -> Vec<WorkflowRow> {
    run.sessions_in_order()
        .flat_map(|session| session.workflows.iter().map(move |w| (session, w)))
        .filter(|(_, workflow)| !workflow.ended())
        .map(|(session, workflow)| {
            let mut row = vendor_row(&run.id, workflow, wall, now);
            row.parent = Some(session.key.to_string());
            row
        })
        .collect()
}

fn vendor_row(
    run: &RunId,
    workflow: &VendorWorkflow,
    wall: OffsetDateTime,
    now: Instant,
) -> WorkflowRow {
    let wall_ms = u64::try_from((wall - OffsetDateTime::UNIX_EPOCH).whole_milliseconds()).ok();
    let name = Some(workflow.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or("workflow");
    let mut row = WorkflowRow::new(vendor_key(run, workflow), name, WorkflowRowState::Running);
    row.vendor = true;
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
}

/// The run's own name, `fix-slugify-median`, or before it has one, its workflow and title,
/// `Council · add --json to export`.
fn run_name(run: &RunState) -> String {
    if let Some(named) = run
        .named
        .as_ref()
        .map(|named| named.name.trim())
        .filter(|name| !name.is_empty())
    {
        return named.to_owned();
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
/// what it spent. Enter on it opens the run, and its card with it.
fn background_row(run: &RunSummary) -> WorkflowRow {
    let state = listed_state(run);
    let workflow = match run.mode {
        RunMode::Solo => Some("solo"),
        RunMode::Crew => Some(run.workflow.trim()).filter(|preset| !preset.is_empty()),
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
    row.spend = panes::spend(&run.spent);
    row
}

fn listed_state(run: &RunSummary) -> WorkflowRowState {
    if run.needs_you() {
        WorkflowRowState::NeedsYou
    } else if matches!(run.state, ApiRunState::Pausing | ApiRunState::Parked) {
        WorkflowRowState::Paused
    } else {
        WorkflowRowState::Running
    }
}

/// A held run's row once it goes on off screen: what this screen last saw of its agents, with
/// its state, tokens, and spend as the listing reads them from its journal. A phase seen before
/// it stopped to wait would be stale, so it goes.
fn follow_listing(row: &mut WorkflowRow, listed: &RunSummary) {
    row.state = listed_state(listed);
    if row.state != WorkflowRowState::Running {
        row.phase = None;
    }
    row.tokens = row.tokens.max(listed.spent.output_tokens);
    if let Some(spend) = panes::spend(&listed.spent) {
        row.spend = Some(spend);
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
        .or_else(|| run.workflow.as_deref().and_then(named))
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
                "{}% of limit · /workflows to stop",
                (share * 100.0).floor() as u64
            )
        })
    });
    row
}

fn row_state(run: &RunState) -> WorkflowRowState {
    match (&run.finished, &run.parked) {
        (Some(outcome), _) => WorkflowRowState::ended(*outcome),
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
        Some(ParkReason::AllVendorsOut { .. }) => Some("every agent is out".to_owned()),
        Some(ParkReason::NoStandIn { detail, .. }) => Some(detail.clone()),
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

/// The subagents your conversation's own CLI started, as Claude Code lists them under `main`:
/// each by its name and what it's doing now, ending with its time and the tokens its CLI
/// counted for it. `shown` is the list as it was, which is where a row's start time lives,
/// since the CLI's stream doesn't date its subagents.
fn conversation_rows(state: &AppState, shown: &[AgentRow], now: Instant) -> Vec<AgentRow> {
    let conversation = conversation_run().to_string();
    state
        .chat
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
                        .quotas
                        .get(&session.worker.worker)
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
    let card = session.card.as_ref().and_then(|id| run.cards.get(id));
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
        .or_else(|| session.card.as_ref().map(|card| run.card_label(card)))
        .unwrap_or_else(|| session.task());
    let mut row = AgentRow::new(
        worker.worker.clone(),
        crate::views::crew::doing(session),
        state,
    )
    .with_label(label.clone(), label);
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
    use argonauts_core::events::run::RunOutcome;
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
            workflow: "council".into(),
            ultracode: false,
            run: Some(RunId::new(run).unwrap()),
            main_agent: "claude-code/opus".parse().unwrap(),
            quotas: Vec::new(),
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
        use argonauts_core::events::session::{VendorAgent, VendorPhase};
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
        host.card = Some(argonauts_core::CardId::new("task-add-due-dates").unwrap());
        let agent = |index: u32, phase: u32, state| VendorAgent {
            index,
            label: format!("step:{index}"),
            phase: Some(phase),
            model: None,
            state,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            started_at_ms: None,
            attempt: 1,
            prompt: None,
            result: None,
        };
        host.workflows.push(VendorWorkflow {
            id: "wf_1".into(),
            call: None,
            name: "add-due-dates".into(),
            description: String::new(),
            phases: ["Build", "Review", "Final"]
                .iter()
                .zip(1..)
                .map(|(title, index)| VendorPhase {
                    index,
                    title: (*title).to_owned(),
                    detail: String::new(),
                })
                .collect(),
            agents: vec![
                agent(1, 1, VendorAgentState::Done),
                agent(2, 2, VendorAgentState::Done),
                agent(3, 3, VendorAgentState::Running),
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
        assert_eq!(rows[0].phase.as_deref(), Some("Final"));
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
        state.chat.subagents = vec![
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
        assert_eq!(first[0].agent_ref().run.as_deref(), Some("chat"));
        assert_eq!(first[0].agent_ref().session, "toolu_a");
        assert_eq!(
            first[0].placeholder().as_deref(),
            Some("Message @Count slowly…")
        );
        // The first has a count from its CLI, and the second has none yet.
        assert_eq!((first[0].tokens_down, first[1].tokens_down), (11_823, 0));

        // Ten seconds on, the first has ended: its clock started at t0 and stops now.
        state.chat.subagents[0].status = SubagentStatus::Done;
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
            Some("85% of limit · /workflows to stop")
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
            Some("90% of limit · /workflows to stop")
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
        // Nobody could take a seat over: the row says which, not that every vendor is out.
        run.parked = Some(ParkReason::NoStandIn {
            detail: "nobody could take over as the main agent".into(),
            until: None,
        });
        assert_eq!(
            row(&run).notice.as_deref(),
            Some("nobody could take over as the main agent")
        );
        assert_eq!(row(&run).state, WorkflowRowState::Paused);

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
        run.workflow = Some("council".into());
        // The goal's alignment runs before the run's own workflow, and the row keeps the
        // workflow's name through it.
        let alignment = |name: &str, parent: bool| crate::state::WorkflowState {
            id: argonauts_core::names::WorkflowId::numbered(if parent { 2 } else { 1 }),
            name: name.into(),
            from: Some("council".into()),
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
            "Council · add --json to export"
        );
        run.workflows.push(alignment("council", false));
        assert_eq!(
            run_row(&run, None, wall, now).name,
            "Council · add --json to export"
        );

        // The sessions' own reports count when the budget's count lags.
        let mut session = SessionState::new(
            argonauts_core::names::SessionKey::numbered(1),
            "claude-code".parse().unwrap(),
            "claude-code",
            argonauts_core::names::RoleName::Worker,
            wall,
        );
        session.cost_usd = Some(0.21);
        run.sessions.insert(session.key.clone(), session);
        assert_eq!(
            run_row(&run, None, wall, now).spend.as_deref(),
            Some("$0.21")
        );
    }

    fn parked_summary(id: &str, waiting: bool) -> RunSummary {
        RunSummary {
            run: RunId::new(id).unwrap(),
            title: "hi".into(),
            mode: RunMode::Solo,
            workflow: "council".into(),
            state: ApiRunState::Parked,
            stopped_at: String::new(),
            updated: OffsetDateTime::UNIX_EPOCH,
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: if waiting {
                vec![argonauts_core::names::CheckpointId::numbered(1)]
            } else {
                Vec::new()
            },
            live: false,
            remind: false,
            name: None,
        }
    }

    /// The mode line counts a card that waits in a run not on screen, until the run opens and
    /// its card counts with the rest.
    #[test]
    fn a_card_waiting_in_a_run_not_on_screen_counts_as_needing_you() {
        let wall = OffsetDateTime::now_utc();
        let mut state = AppState::default();
        state.background = vec![
            parked_summary("run-001", true),
            parked_summary("run-002", false),
        ];
        assert_eq!(state.background_waiting(), 1);
        state
            .runs
            .insert(RunId::new("run-001").unwrap(), run_state("run-001", wall));
        assert_eq!(state.background_waiting(), 0);
    }

    #[test]
    fn a_run_not_on_screen_keeps_its_row_until_it_is_opened() {
        let wall = OffsetDateTime::now_utc();
        let now = Instant::now();
        let mut state = AppState::default();
        state.background = vec![
            parked_summary("run-001", true),
            parked_summary("run-002", false),
        ];
        state
            .runs
            .insert(RunId::new("run-003").unwrap(), run_state("run-003", wall));

        let rows = run_rows(&state, wall, now);
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["run-001", "run-002", "run-003"]);
        assert_eq!(rows[0].state, WorkflowRowState::NeedsYou);
        assert_eq!(rows[0].name, "Solo · hi");
        assert!(!rows[0].timed, "its time isn't known here");
        assert_eq!(rows[1].state, WorkflowRowState::Paused);
        assert!(rows[2].timed);

        // A run that has its own name goes by it, as its row will once it's open.
        state.background[1].name = Some("say-hi".to_owned());
        assert_eq!(run_rows(&state, wall, now)[1].name, "say-hi");

        // Opened, the run is held here, and its own row takes the place of the summary's.
        state
            .runs
            .insert(RunId::new("run-001").unwrap(), run_state("run-001", wall));
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
        state.runs.insert(first.clone(), run_state("run-001", wall));
        state
            .runs
            .insert(second.clone(), run_state("run-002", wall));
        state.attached = Some(second.clone());
        let summary = |waiting: bool| RunSummary {
            spent: Spend {
                output_tokens: 900,
                ..Spend::default()
            },
            ..parked_summary("run-001", waiting)
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
        let mut building = run_row(&state.runs[&first], None, wall, now);
        building.phase = Some("Build".into());
        follow_listing(&mut building, &summary(true));
        assert_eq!(building.phase, None, "the phase it left in is stale");

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
        assert_eq!(row(&state).unwrap().state, WorkflowRowState::Paused);
    }

    #[test]
    fn two_runs_never_share_a_name_and_an_agents_own_workflow_keeps_its_own() {
        let row = |name: &str, parent: Option<&str>| {
            let mut row = WorkflowRow::new(name, name, WorkflowRowState::Running);
            row.parent = parent.map(str::to_owned);
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
