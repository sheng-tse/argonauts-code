//! A run whose work is one session running its CLI's own workflow, as when Claude Code's own
//! workflow runs the crew's. That workflow is the run's, drawn in the one view every workflow
//! has: its phases are the view's to the end, its agents the run's steps, and the reviewers it
//! asked are in the rows they answered. The session that plans it is none of its steps. A later
//! workflow the session starts for the same task is a row in the last phase that ran.

use argonauts_core::RunId;
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::views::{
    PhaseCount, PhaseRow, RowStatus, WorkflowRow, WorkflowView, agents_counted,
};

use super::WorkflowLog;

/// Puts the first CLI workflow of `view`'s task in place of the phase that holds it, and returns
/// that workflow's name. A later one of the same session goes in the last phase that ran, which
/// is at work again while it runs. A view with none, or with workflows of several sessions,
/// stays as it is.
pub(super) fn as_the_runs(
    view: &mut WorkflowView,
    log: &WorkflowLog,
    run: &RunId,
) -> Option<String> {
    let found = cli_workflows(&view.phases);
    let (at, id, host, worker) = found.first()?.clone();
    if found.iter().any(|(_, _, other, _)| *other != host) {
        return None;
    }
    let later: Vec<WorkflowId> = found[1..].iter().map(|(_, id, ..)| id.clone()).collect();
    let inner = log.agent_view(run, &id)?;
    let name = inner.name.clone();
    let mut phases = inner.phases;
    if phases.is_empty() {
        return None;
    }
    // The reviewers they asked already answer in their own rows. An ask's own rows, its tries
    // among them, go by the label it was asked under, which carries its round from the second.
    let mut asked = answered(&phases, log);
    for later in &later {
        if let Some(view) = log.agent_view(run, later) {
            asked.extend(answered(&view.phases, log));
        }
    }
    // Only a session working alone: beside it are the asks it made, which no task holds.
    let alone = view.phases[at].rows.iter().all(|row| match row {
        WorkflowRow::Agent(agent) => {
            agent.session == host
                || log
                    .sessions
                    .session(&agent.session)
                    .is_some_and(|facts| facts.task.is_none())
        }
        _ => true,
    });
    if !alone {
        return None;
    }
    let held = std::mem::take(&mut view.phases[at].rows);
    let mut planner = None;
    let mut rest = Vec::new();
    let mut inside = Vec::new();
    for row in held {
        match row {
            WorkflowRow::Agent(agent) if agent.session == host => planner = Some(agent),
            WorkflowRow::Workflow(nested) if nested.id == id => {}
            // It carries on the run's own workflow, so it hangs under no agent.
            WorkflowRow::Workflow(mut nested) if later.contains(&nested.id) => {
                nested.parent = None;
                inside.push(WorkflowRow::Workflow(nested));
            }
            WorkflowRow::Agent(agent) if asked.contains(&agent.label) => {}
            other => rest.push(other),
        }
    }
    if let Some(last) = phases.last_mut() {
        last.rows.extend(rest);
    }
    // A later workflow carries on the phase that ran last, which works again until it ends.
    if !inside.is_empty() {
        let ran = phases
            .iter()
            .rposition(|phase| phase.status != RowStatus::Waiting)
            .unwrap_or(phases.len() - 1);
        let running: Vec<&str> = inside
            .iter()
            .filter_map(|row| match row {
                WorkflowRow::Workflow(nested) if nested.status == RowStatus::Running => {
                    Some(nested.name.as_str())
                }
                _ => None,
            })
            .collect();
        // The merge waits for it.
        let waits = running
            .last()
            .map(|name| format!("Merge waits for {name}, then merges the change"));
        phases[ran].rows.extend(inside);
        if let Some(waits) = waits {
            phases[ran].status = RowStatus::Running;
            phases[ran].note = Some(waits);
        }
    }
    for phase in &mut phases {
        let (done, total) = counted(&phase.rows);
        phase.count = PhaseCount::of(done, total);
    }
    let others: (u32, u32) = view
        .phases
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != at)
        .map(|(_, phase)| counted(&phase.rows))
        .chain(phases.iter().map(|phase| counted(&phase.rows)))
        .fold((0, 0), |(done, total), (d, t)| (done + d, total + t));
    // The session that plans and runs the workflow is none of its steps. Its agents stand for
    // it once they show; until then its row does, uncounted.
    let no_agents = phases.iter().all(|phase| {
        !phase
            .rows
            .iter()
            .any(|row| matches!(row, WorkflowRow::Agent(_)))
    });
    if let Some(planner) = planner.filter(|_| no_agents) {
        phases[0].rows.insert(0, WorkflowRow::Agent(planner));
    }
    view.phases.splice(at..=at, phases);
    (view.agents_done, view.agents_total) = others;
    view.worker = worker;
    Some(name)
}

/// The labels the asks behind the rows a role answered through Argonauts Code went under.
fn answered(phases: &[PhaseRow], log: &WorkflowLog) -> Vec<String> {
    phases
        .iter()
        .flat_map(|phase| &phase.rows)
        .filter_map(|row| match row {
            WorkflowRow::Agent(agent) if agent.answered => log
                .sessions
                .session(&agent.session)
                .map(|session| session.label()),
            _ => None,
        })
        .collect()
}

/// Each CLI workflow of the view, oldest first: the phase holding it, the workflow, the session
/// running it, and the CLI whose workflow it is.
fn cli_workflows(phases: &[PhaseRow]) -> Vec<(usize, WorkflowId, SessionKey, Option<String>)> {
    phases
        .iter()
        .enumerate()
        .flat_map(|(at, phase)| {
            phase.rows.iter().filter_map(move |row| match row {
                WorkflowRow::Workflow(nested) => {
                    let host = nested.parent.clone()?;
                    Some((at, nested.id.clone(), host, nested.worker.clone()))
                }
                _ => None,
            })
        })
        .collect()
}

/// Agents done and in all, by the one rule: a Claude Code agent is done once it finished.
fn counted(rows: &[WorkflowRow]) -> (u32, u32) {
    agents_counted(rows, |_| false)
}
