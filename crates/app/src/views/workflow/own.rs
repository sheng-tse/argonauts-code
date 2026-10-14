//! A run whose work is one session running its CLI's own workflow, as when Claude Code's own
//! workflow runs the crew's. That workflow is the run's: its phases are the view's, its agents
//! the run's steps, and the reviewers it asked are in the rows they answered. The session that
//! plans it is the box itself, not a step. Nothing of it nests under the session as a second
//! run.

use argonauts_core::RunId;
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::views::{PhaseCount, PhaseRow, RowStatus, WorkflowRow, WorkflowView};

use super::WorkflowLog;

/// Puts the one CLI workflow of `view` in place of the phase that holds it. A view with none,
/// or with several, stays as it is.
pub(super) fn as_the_runs(view: &mut WorkflowView, log: &WorkflowLog, run: &RunId) {
    let Some((at, id, host, owner)) = the_one_cli_workflow(&view.phases) else {
        return;
    };
    let Some(inner) = log.vendor_view(run, &id) else {
        return;
    };
    let mut phases = inner.phases;
    if phases.is_empty() {
        return;
    }
    // The reviewers it asked already answer in its own rows.
    let asked: Vec<String> = phases
        .iter()
        .flat_map(|phase| &phase.rows)
        .filter_map(|row| match row {
            WorkflowRow::Agent(agent) if agent.answered.is_some() => Some(agent.label.clone()),
            _ => None,
        })
        .collect();
    // Only a session working alone: beside it are the asks it made, which no task holds.
    let alone = view.phases[at].rows.iter().all(|row| match row {
        WorkflowRow::Agent(agent) => {
            agent.session == host
                || log
                    .sessions
                    .session(&agent.session)
                    .is_some_and(|facts| facts.card.is_none())
        }
        _ => true,
    });
    if !alone {
        return;
    }
    let held = std::mem::take(&mut view.phases[at].rows);
    let mut planner = None;
    let mut rest = Vec::new();
    for row in held {
        match row {
            WorkflowRow::Agent(agent) if agent.session == host => planner = Some(agent),
            WorkflowRow::Workflow(nested) if nested.id == id => {}
            WorkflowRow::Agent(agent) if asked.contains(&agent.label) => {}
            other => rest.push(other),
        }
    }
    if let Some(last) = phases.last_mut() {
        last.rows.extend(rest);
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
    // The header names the workflow the run started from, then whose own workflow runs it.
    view.notes = log
        .named
        .as_ref()
        .and_then(|named| named.based_on.as_deref())
        .map(str::trim)
        .filter(|based_on| !based_on.is_empty())
        .map(|based_on| vec![capitalised(based_on)])
        .unwrap_or_default();
    view.owner = owner;
}

/// `Orchestrator` for `orchestrator`: a workflow's name on screen starts with a capital, and
/// stays as typed everywhere else.
fn capitalised(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The phase holding the view's only CLI workflow, that workflow, the session running it, and
/// the CLI whose workflow it is.
fn the_one_cli_workflow(
    phases: &[PhaseRow],
) -> Option<(usize, WorkflowId, SessionKey, Option<String>)> {
    let mut found = phases.iter().enumerate().flat_map(|(at, phase)| {
        phase.rows.iter().filter_map(move |row| match row {
            WorkflowRow::Workflow(nested) => {
                let host = nested.parent.clone()?;
                Some((at, nested.id.clone(), host, nested.owner.clone()))
            }
            _ => None,
        })
    });
    let one = found.next()?;
    found.next().is_none().then_some(one)
}

fn counted(rows: &[WorkflowRow]) -> (u32, u32) {
    let agents = rows.iter().filter_map(|row| match row {
        WorkflowRow::Agent(agent) => Some(agent.status),
        _ => None,
    });
    agents.fold((0, 0), |(done, total), status| {
        (done + u32::from(status == RowStatus::Done), total + 1)
    })
}
