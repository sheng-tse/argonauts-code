use std::collections::{BTreeMap, HashMap};

use argonauts_core::events::workflow::{StepEnd, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{WorkerRef, WorkflowId};
use argonauts_core::views::{NestedWorkflowRow, PhaseRow, RowStatus, WorkflowRow, WorkflowView};
use argonauts_core::workflow::{StepRef, Workflow};
use argonauts_core::{RunId, Spend};
use time::OffsetDateTime;

use super::sessions::{RunFacts, add_spend};

/// Nesting stops at depth 2; a deeper chain can only come from a damaged log.
const MAX_DEPTH: usize = 4;

/// The run's own workflow when `workflow` is `None`, else that nested one with its breadcrumb.
/// Before the lead picks the run's own workflow, `None` gives the one that runs first, the goal's
/// alignment. `None` when the events hold no such workflow.
pub(crate) fn workflow_view(
    events: &[RunEvent],
    run: &RunId,
    workflow: Option<&WorkflowId>,
) -> Option<WorkflowView> {
    let log = WorkflowLog::of(events);
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
    let breadcrumb = if facts.parent.is_some() {
        log.breadcrumb(&id)
    } else {
        Vec::new()
    };
    let built = log.build(&id, 0)?;
    Some(WorkflowView {
        run: run.clone(),
        id,
        name: facts.definition.name.clone(),
        description: facts.definition.description.clone(),
        breadcrumb,
        agents_done: built.agents_done,
        agents_total: built.agents_total,
        elapsed_ms: built.elapsed_ms,
        spent: built.spent,
        phases: built.phases,
    })
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

struct WorkflowLog {
    order: Vec<WorkflowId>,
    workflows: BTreeMap<WorkflowId, WorkflowFacts>,
    steps: HashMap<StepRef, StepProgress>,
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
        let mut order = Vec::new();
        let mut workflows = BTreeMap::new();
        let mut steps = HashMap::new();
        for event in events {
            let RunEventKind::Workflow(workflow) = &event.kind else {
                continue;
            };
            match workflow {
                WorkflowEvent::Started(started) => {
                    order.push(started.id.clone());
                    workflows.insert(
                        started.id.clone(),
                        WorkflowFacts {
                            definition: started.workflow.clone(),
                            parent: started.parent.clone(),
                            started_at: event.ts,
                        },
                    );
                }
                WorkflowEvent::Changed(changed) => {
                    if let Some(facts) = workflows.get_mut(&changed.id) {
                        facts.definition = changed.workflow.clone();
                    }
                }
                WorkflowEvent::StepStarted(step) => {
                    steps.insert(step.step.clone(), StepProgress::Running);
                }
                WorkflowEvent::StepFinished(step) => {
                    steps.insert(step.step.clone(), StepProgress::Ended(step.end, event.ts));
                }
            }
        }
        Self {
            order,
            workflows,
            steps,
            sessions: RunFacts::of(events),
        }
    }

    /// Names from the run's workflow down to `id`.
    fn breadcrumb(&self, id: &WorkflowId) -> Vec<String> {
        let mut names = Vec::new();
        let mut at = Some(id.clone());
        while let Some(id) = at {
            let Some(facts) = self.workflows.get(&id) else {
                break;
            };
            names.push(facts.definition.name.clone());
            if names.len() > MAX_DEPTH {
                break;
            }
            at = facts.parent.as_ref().map(|parent| parent.workflow.clone());
        }
        names.reverse();
        names
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
        for (index, phase) in facts.definition.phases.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let mut rows: Vec<(OffsetDateTime, WorkflowRow)> = Vec::new();
            for session in &self.sessions.sessions {
                let Some(step) = &session.started.workflow_step else {
                    continue;
                };
                if &step.workflow != id || step.phase != index {
                    continue;
                }
                let row = self.sessions.agent_row(session);
                built.agents_total += 1;
                if row.status == RowStatus::Done {
                    built.agents_done += 1;
                }
                add_spend(&mut built.spent, &row.spent);
                remember(&mut built.workers, &row.worker);
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
                    let row = NestedWorkflowRow {
                        id: child_id.clone(),
                        name: child.definition.name.clone(),
                        status: nested.status,
                        agents_done: nested.agents_done,
                        agents_total: nested.agents_total,
                        workers: nested.workers,
                        elapsed_ms: nested.elapsed_ms,
                    };
                    rows.push((child.started_at, WorkflowRow::Workflow(row)));
                }
            }
            rows.sort_by_key(|(started, _)| *started);
            let rows: Vec<WorkflowRow> = rows.into_iter().map(|(_, row)| row).collect();
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
            let not_started = steps.iter().filter(|step| step.is_none()).count();
            let done = rows
                .iter()
                .filter(|row| row_status(row) == RowStatus::Done)
                .count();
            built.phases.push(PhaseRow {
                name: phase.name.clone(),
                standard: phase.standard,
                status: phase_status(&rows, &steps),
                done: u32::try_from(done).unwrap_or(u32::MAX),
                total: u32::try_from(rows.len() + not_started).unwrap_or(u32::MAX),
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
    }
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
    use argonauts_core::events::workflow::{StepFinished, StepStarted, WorkflowStarted};
    use argonauts_core::names::{RoleName, SessionKey};
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
                    "name": "peer-crew",
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
            ("peer-crew", top.clone())
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
        assert_eq!((look.done, look.total), (2, 3));
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
        assert_eq!((build.done, build.total, build.rows.len()), (0, 1, 0));

        let inner = workflow_view(&events, &run_id(), Some(&review)).unwrap();
        assert_eq!(inner.breadcrumb, ["peer-crew", "review"]);
        assert_eq!(inner.phases[0].status, RowStatus::Done);
        assert!(workflow_view(&events, &run_id(), Some(&WorkflowId::numbered(9))).is_none());
        assert!(workflow_view(&[], &run_id(), None).is_none());
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

        events.push(started(
            3,
            &own,
            None,
            definition(serde_json::json!({
                "name": "peer-crew",
                "phases": [{"name": "Goal", "standard": "understand",
                            "steps": [{"kind": "workflow", "workflow": "goal"}]}]
            })),
        ));
        let view = workflow_view(&events, &run_id(), None).unwrap();
        assert_eq!((view.name.as_str(), &view.id), ("peer-crew", &own));
        assert!(view.breadcrumb.is_empty());
    }
}
