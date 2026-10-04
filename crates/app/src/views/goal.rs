//! The goal view's data: what a goal-style run is trying to achieve and
//! whether it's there yet, folded from its goal events. Every check audits every criterion
//! again, so a criterion's state is the latest check's.

use argonauts_core::events::goal::{Assumption, GoalEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::views::{CriterionRow, GoalView};
use argonauts_core::{RunId, Spend};

use super::sessions::{RunFacts, add_spend};

/// `None` until the run sets a goal: a workflow run has no goal view.
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
            GoalEvent::Set(set) => {
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
                    attempts: set.attempts.clone(),
                    max_attempts: set.max_attempts,
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
        GoalEvent::AttemptStarted { n, title, phase } => {
            match view.attempts.iter_mut().find(|attempt| attempt.n == *n) {
                Some(attempt) => {
                    attempt.title.clone_from(title);
                    attempt.phase = *phase;
                }
                None => view.attempts.push(argonauts_core::events::goal::Attempt {
                    n: *n,
                    title: title.clone(),
                    phase: *phase,
                    verdict: None,
                }),
            }
        }
        GoalEvent::Checked(checked) => {
            if let Some(attempt) = view
                .attempts
                .iter_mut()
                .find(|attempt| attempt.n == checked.attempt)
            {
                attempt.verdict = Some(checked.overall);
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
        GoalEvent::StatusChanged { status, .. } => view.status = *status,
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

    fn goal(seq: u64, event_kind: GoalEvent) -> RunEvent {
        event(seq, RunEventKind::Goal(event_kind))
    }

    #[test]
    fn the_goal_view_follows_its_attempts_and_checks() {
        assert!(goal_view(&[], &run_id()).is_none());
        let set = GoalRun {
            objective: "Fix slugify and median in textkit, with tests".to_owned(),
            criteria: vec![
                Criterion::new("G1", "slugify(\"Hello, World!\") returns \"hello-world\""),
                Criterion::new("G2", "median([]) raises ValueError"),
            ],
            attempts: Vec::new(),
            status: GoalStatus::Pursuing,
            max_attempts: 5,
        };
        let events = vec![
            goal(0, GoalEvent::Set(Box::new(set))),
            goal(
                1,
                GoalEvent::AttemptStarted {
                    n: 1,
                    title: "Both fixes".to_owned(),
                    phase: 1,
                },
            ),
            goal(
                2,
                GoalEvent::Checked(Box::new(GoalChecked {
                    attempt: 1,
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
                })),
            ),
            goal(
                3,
                GoalEvent::AttemptStarted {
                    n: 2,
                    title: "Empty list".to_owned(),
                    phase: 2,
                },
            ),
        ];
        let view = goal_view(&events, &run_id()).unwrap();
        assert_eq!(view.status, GoalStatus::Pursuing);
        assert_eq!(view.attempts.len(), 2);
        assert_eq!(view.attempts[0].verdict, Some(GoalVerdict::NotYet));
        assert_eq!(view.criteria[0].state, Some(CriterionState::Met));
        assert_eq!(view.criteria[1].missing, "returns None");
        assert_eq!(view.last_reason.as_deref(), Some("median([]) returns None"));
    }
}
