//! The goal view's data: the goal around a run's workflow and whether it
//! holds yet, folded from its goal events. Every check audits every line again, so a line's
//! state is the latest check's. A task's own goal isn't here: `argonauts_crew::goal::report`
//! reports every goal in the run, this one included, for the goal panels.

use argonauts_core::events::goal::{Assumption, GoalEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::views::{CriterionRow, GoalView};
use argonauts_core::{RunId, Spend};

use super::sessions::{RunFacts, add_spend};

/// `None` until the run sets a goal: a run without one has no goal view.
pub(crate) fn goal_view(events: &[RunEvent], run: &RunId) -> Option<GoalView> {
    let mut view: Option<GoalView> = None;
    // The approved goal file's assumptions, which a goal run starts from.
    let mut assumptions: Vec<Assumption> = Vec::new();
    for event in events {
        let RunEventKind::Goal(goal) = &event.kind else {
            continue;
        };
        match goal {
            GoalEvent::Approved { file, .. } => assumptions.clone_from(&file.assumptions),
            GoalEvent::Set(set) if set.task.is_none() => {
                view = Some(GoalView {
                    run: run.clone(),
                    objective: set.objective.clone(),
                    status: set.status,
                    criteria: set
                        .criteria
                        .iter()
                        .map(|criterion| CriterionRow {
                            criterion: criterion.clone(),
                            state: None,
                            evidence: String::new(),
                            missing: String::new(),
                        })
                        .collect(),
                    tries: set.tries.clone(),
                    max_rounds: set.max_rounds,
                    last_reason: None,
                    assumptions: assumptions.clone(),
                    spent: Spend::default(),
                });
            }
            _ => {
                if let Some(view) = view.as_mut() {
                    apply(view, goal);
                }
            }
        }
    }
    let mut view = view?;
    let facts = RunFacts::of(events);
    for session in &facts.sessions {
        add_spend(&mut view.spent, &session.spent());
    }
    Some(view)
}

fn apply(view: &mut GoalView, event: &GoalEvent) {
    match event {
        GoalEvent::TryStarted {
            n,
            title,
            phase,
            task: None,
        } => match view.tries.iter_mut().find(|a_try| a_try.n == *n) {
            Some(a_try) => {
                a_try.title.clone_from(title);
                a_try.phase = *phase;
            }
            None => view.tries.push(argonauts_core::events::goal::Try {
                n: *n,
                title: title.clone(),
                phase: *phase,
                verdict: None,
            }),
        },
        GoalEvent::Checked(checked) if checked.task.is_none() => {
            if let Some(a_try) = view
                .tries
                .iter_mut()
                .find(|a_try| a_try.n == checked.try_number)
            {
                a_try.verdict = Some(checked.overall);
            }
            for verdict in &checked.verdicts {
                if let Some(row) = view
                    .criteria
                    .iter_mut()
                    .find(|row| row.criterion.id == verdict.id)
                {
                    row.state = Some(verdict.verdict);
                    row.evidence.clone_from(&verdict.evidence);
                    row.missing.clone_from(&verdict.missing);
                }
            }
            view.last_reason = Some(checked.reason.clone());
        }
        GoalEvent::StatusChanged {
            status, task: None, ..
        } => view.status = *status,
        GoalEvent::AssumptionChanged { id, to, by } => {
            let at = usize::try_from(*id).unwrap_or(usize::MAX).saturating_sub(1);
            if let Some(assumption) = view.assumptions.get_mut(at) {
                assumption.text.clone_from(to);
                assumption.by = Some(*by);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{event, run_id};
    use argonauts_core::events::goal::{
        Criterion, CriterionState, CriterionVerdict, GoalChecked, GoalRun, GoalStatus, GoalVerdict,
    };
    use argonauts_core::goal::{CheckWith, GoalFrom, GoalScope, GoalSpec, KeptBy};

    fn goal(seq: u64, event_kind: GoalEvent) -> RunEvent {
        event(seq, RunEventKind::Goal(event_kind))
    }

    #[test]
    fn the_goal_view_follows_its_tries_and_checks() {
        assert!(goal_view(&[], &run_id()).is_none());
        let lines = vec![
            Criterion::new("G1", "slugify(\"Hello, World!\") returns \"hello-world\""),
            Criterion::new("G2", "median([]) raises ValueError"),
        ];
        let spec = GoalSpec::from_lines(
            "Fix slugify and median in textkit, with tests",
            lines,
            CheckWith::Auto,
            GoalFrom::Command,
        );
        let set = GoalRun::new(&spec, GoalScope::Workflow, KeptBy::Argonauts);
        // A task's own goal, set and checked beside the run's, stays out of the run's view.
        let mut of_a_task = set.clone();
        of_a_task.task = Some(argonauts_core::TaskId::new("task-001").unwrap());
        let events = vec![
            goal(0, GoalEvent::Set(Box::new(set))),
            goal(
                1,
                GoalEvent::TryStarted {
                    n: 1,
                    title: "Both fixes".to_owned(),
                    phase: 1,
                    task: None,
                },
            ),
            goal(1, GoalEvent::Set(Box::new(of_a_task))),
            goal(
                2,
                GoalEvent::Checked(Box::new(GoalChecked {
                    try_number: 1,
                    verdicts: vec![
                        CriterionVerdict {
                            id: "G1".to_owned(),
                            verdict: CriterionState::Met,
                            evidence: "test_slugify_hello passes".to_owned(),
                            missing: String::new(),
                        },
                        CriterionVerdict {
                            id: "G2".to_owned(),
                            verdict: CriterionState::NotYet,
                            evidence: String::new(),
                            missing: "returns None".to_owned(),
                        },
                    ],
                    overall: GoalVerdict::NotYet,
                    reason: "median([]) returns None".to_owned(),
                    task: None,
                    checks: Vec::new(),
                })),
            ),
            goal(
                3,
                GoalEvent::TryStarted {
                    n: 2,
                    title: "Empty list".to_owned(),
                    phase: 2,
                    task: None,
                },
            ),
        ];
        let view = goal_view(&events, &run_id()).unwrap();
        assert_eq!(view.status, GoalStatus::Pursuing);
        assert_eq!(view.tries.len(), 2);
        assert_eq!(view.tries[0].verdict, Some(GoalVerdict::NotYet));
        assert_eq!(view.criteria[0].state, Some(CriterionState::Met));
        assert_eq!(view.criteria[1].missing, "returns None");
        assert_eq!(view.last_reason.as_deref(), Some("median([]) returns None"));
    }
}
