//! Where the goal panels get what they draw. The workflow view's rows and goal panel, the
//! opened agent's Goal tab, and the row at the bottom all read [`AgentGoal`]s, and this is the
//! one place that makes them from a run's record.
//!
//! The engine reports every goal in a run from its journal ([`reports`]). A goal an agent
//! works toward becomes that agent's `AgentGoal`, found by its session. The run's own goal goes
//! round the whole workflow and belongs to no one row, so it stays with the workflow's status
//! line and the goal view.

use argonauts_core::app::agent_goal::{
    AgentGoal, CliGoal, GoalCheck, GoalKeeper, GoalLine, GoalState, GoalTry,
};
use argonauts_core::app::views::{AgentRow, PhaseRow, WorkflowRow};
use argonauts_core::base::names::ModelRef;
use argonauts_core::look::display_names::builtin_cli_name;
use argonauts_core::models::model_catalog::display_name_from_id;
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::goal::GoalStatus;
use argonauts_core::workflows::goal::KeptBy;
use argonauts_core::workflows::goal::report::{
    CheckReport, CliGoalReport, GoalReport, LineReport, LineState, TryReport,
};
use argonauts_crew::goal::report::reports;
use time::OffsetDateTime;

/// Who runs every goal's checks, whoever keeps it going.
const CHECKER: &str = "Argonauts Code";

/// Every agent's goal in the run as its events have it now, each under the label its agent's
/// row has in `phases`, so the screen finds it on that row. The engine words a task's label
/// itself, and the two can be different. A goal whose agent has no row of its own here keeps the
/// engine's label.
pub(crate) fn agent_goals(events: &[RunEvent], phases: &[PhaseRow]) -> Vec<AgentGoal> {
    let mut goals = goals_at(events, OffsetDateTime::now_utc());
    for goal in &mut goals {
        let mut rows = phases
            .iter()
            .flat_map(|phase| &phase.rows)
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) if agent.session == goal.session => Some(agent),
                _ => None,
            });
        // Agents their CLI runs itself share its session, and none of them is the goal's.
        if let (Some(row), None) = (rows.next(), rows.next()) {
            goal.label.clone_from(&row.label);
        }
    }
    goals
}

/// The goal the agent on `row` works toward, when it has one, under that row's label.
pub(crate) fn goal_of(events: &[RunEvent], row: &AgentRow) -> Option<Box<AgentGoal>> {
    let mut goal = goals_at(events, OffsetDateTime::now_utc())
        .into_iter()
        .find(|goal| goal.session == row.session)?;
    goal.label.clone_from(&row.label);
    Some(Box::new(goal))
}

/// Every agent's goal as `events` have it, with `now` to say how long ago each was set.
fn goals_at(events: &[RunEvent], now: OffsetDateTime) -> Vec<AgentGoal> {
    reports(events)
        .into_iter()
        .filter_map(|report| agent_goal(report, now))
        .collect()
}

/// One agent's goal from the engine's report of it. `None` for a goal no one agent works
/// toward yet: the run's own, and a task's before its first session.
fn agent_goal(report: GoalReport, now: OffsetDateTime) -> Option<AgentGoal> {
    let session = report.session.clone()?;
    let set_ms_ago = report
        .set_at
        .and_then(|set| u64::try_from((now - set).whole_milliseconds()).ok());
    Some(AgentGoal {
        session,
        role: report.role().map(str::to_owned),
        counting: report.counting().to_owned(),
        kept_by: match (report.kept.by, &report.kept.cli) {
            (KeptBy::Cli, Some(cli)) => GoalKeeper::Cli { name: cli.clone() },
            (KeptBy::Cli, None) => GoalKeeper::Cli {
                name: cli_of(report.worker.as_ref()),
            },
            (KeptBy::Argonauts, _) => GoalKeeper::Argonauts,
        },
        kept_note: report.kept.note,
        checked_by: CHECKER.to_owned(),
        state: match report.status {
            GoalStatus::Pursuing => GoalState::Running,
            GoalStatus::Met => GoalState::Met,
            GoalStatus::Paused => GoalState::Paused,
            GoalStatus::Stuck | GoalStatus::OverLimit => GoalState::Stuck,
            GoalStatus::Cleared => GoalState::Cleared,
        },
        stopped_because: report.ended_because,
        set_ms_ago,
        lines: report.lines.into_iter().map(line).collect(),
        tries: report.tries.into_iter().map(a_try).collect(),
        max_tries: report.max_rounds,
        work_ms: report.work_ms,
        tokens: report.tokens,
        checks: report.checks.into_iter().map(check).collect(),
        checking: report.checking,
        cli_says: report.cli_says.map(cli_says),
        label: report.of,
        task: report.task,
    })
}

/// The CLI by the name the screen gives it, for a goal that names its worker and not its CLI.
fn cli_of(worker: Option<&ModelRef>) -> String {
    match worker {
        Some(worker) => {
            builtin_cli_name(&worker.agent).map_or_else(|| worker.agent.clone(), str::to_owned)
        }
        None => "the CLI".to_owned(),
    }
}

fn line(line: LineReport) -> GoalLine {
    GoalLine {
        text: line.text,
        met: match line.state {
            LineState::Unchecked => None,
            LineState::Met => Some(true),
            LineState::NotMet => Some(false),
        },
        checked_by: line.checked_by,
        met_on_try: line.met_on_try,
        failing: Some(line.still_fails).filter(|fails| !fails.trim().is_empty()),
    }
}

fn a_try(a_try: TryReport) -> GoalTry {
    GoalTry {
        number: a_try.n,
        did: if a_try.changed_approach {
            format!("{} · a different approach", a_try.what)
        } else {
            a_try.what
        },
        changed: a_try.changed,
        failing: a_try.still_failing.map(|failing| failing.count),
        elapsed_ms: a_try.ms,
        tokens: a_try.tokens,
        running: a_try.running,
    }
}

/// `1 failed · 11 passed · 4.1s` for a command, and `1 of 1 line met · Astra` for the judge,
/// which reads by its model's name.
fn check(check: CheckReport) -> GoalCheck {
    let mut result = check.summary;
    match &check.agent {
        Some(judge) => result.push_str(&format!(" · {}", judge_name(judge))),
        None if check.ms > 0 => result.push_str(&format!(" · {}", took(check.ms))),
        None => {}
    }
    GoalCheck {
        name: check.name,
        passed: Some(check.passed),
        result,
        detail: check.output,
    }
}

/// The judge by its model's name, or by its CLI's where it runs the CLI's own default.
fn judge_name(judge: &ModelRef) -> String {
    match judge.model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => display_name_from_id(model),
        _ => cli_of(Some(judge)),
    }
}

/// `4.1s`, and `1m05s` from a minute on.
fn took(ms: u64) -> String {
    let tenths = (ms + 50) / 100;
    if tenths < 600 {
        return format!("{}.{}s", tenths / 10, tenths % 10);
    }
    let seconds = ms / 1000;
    format!("{}m{:02}s", seconds / 60, seconds % 60)
}

fn cli_says(says: CliGoalReport) -> CliGoal {
    CliGoal {
        cli: says.cli,
        state: says.status,
        note: Some(says.note).filter(|note| !note.trim().is_empty()),
        turns: says.turns,
    }
}

#[cfg(test)]
mod tests;
