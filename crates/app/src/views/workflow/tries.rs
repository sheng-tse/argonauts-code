//! A step tried more than once is one row. Every try at a step runs under the step's label, so
//! the newest try stands for the step, and the ones before it, each failed try and the agent it
//! took over from, fold beneath it. Agents that run side by side in a step stay rows of their
//! own. The row keeps the place of the step's first try, so the
//! steps stay in the order they started.

use std::collections::BTreeMap;

use argonauts_core::names::SessionKey;
use argonauts_core::views::{AgentRow, RowStatus, StepTry, WorkflowRow};

use crate::views::sessions::RunFacts;

/// What folding took out of a phase's rows, so its counts can be counted in steps.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Folded {
    pub tries: u32,
    pub done: u32,
}

/// Folds every earlier try at a step under the step's newest row.
pub(super) fn fold(rows: Vec<WorkflowRow>, sessions: &RunFacts) -> (Vec<WorkflowRow>, Folded) {
    let mut folded = Folded::default();
    // Each row with which start of its session it shows: a session that goes again after a
    // failure starts again under the same key, so its rows are its starts in order.
    let mut kept: Vec<(WorkflowRow, usize)> = Vec::with_capacity(rows.len());
    let mut starts: BTreeMap<SessionKey, usize> = BTreeMap::new();
    for row in rows {
        let WorkflowRow::Agent(mut agent) = row else {
            kept.push((row, 0));
            continue;
        };
        let start = starts.entry(agent.session.clone()).or_default();
        let nth = *start;
        *start += 1;
        let step = agent.label.trim().to_owned();
        let earlier = (!step.is_empty())
            .then(|| {
                kept.iter().rposition(|(kept, _)| {
                    matches!(kept, WorkflowRow::Agent(known)
                        if known.label.trim() == step && tried_before(known, &agent))
                })
            })
            .flatten();
        let Some(at) = earlier else {
            kept.push((WorkflowRow::Agent(agent), nth));
            continue;
        };
        if let (WorkflowRow::Agent(before), before_nth) = &kept[at] {
            folded.tries += 1;
            if before.status == RowStatus::Done {
                folded.done += 1;
            }
            let mut tries = before.tries.clone();
            tries.push(try_of(before, *before_nth, sessions));
            tries.append(&mut agent.tries);
            agent.tries = tries;
        }
        kept[at] = (WorkflowRow::Agent(agent), nth);
    }
    (kept.into_iter().map(|(row, _)| row).collect(), folded)
}

/// Whether `before` was a try `after` came after: one that failed, or the agent `after` took
/// over from. Two agents a step runs side by side share its label too, and stay two rows.
fn tried_before(before: &AgentRow, after: &AgentRow) -> bool {
    before.status == RowStatus::Failed || after.standing_in_for.as_ref() == Some(&before.worker)
}

/// The try `row` shows, the `nth` start of its session, with why that start ended.
fn try_of(row: &AgentRow, nth: usize, sessions: &RunFacts) -> StepTry {
    let reason = sessions
        .sessions
        .iter()
        .filter(|facts| facts.key == row.session)
        .nth(nth)
        .or_else(|| sessions.session(&row.session))
        .and_then(|facts| facts.ended.as_ref())
        .map(|end| end.reason.clone());
    StepTry {
        session: row.session.clone(),
        worker: row.worker.clone(),
        status: row.status,
        elapsed_ms: row.elapsed_ms,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::names::{SessionKey, WorkerRef};

    use super::*;

    fn agent(session: &str, label: &str, worker: &str, status: RowStatus) -> WorkflowRow {
        WorkflowRow::Agent(AgentRow {
            session: SessionKey::parse(session).unwrap(),
            label: label.into(),
            worker: worker.parse().unwrap(),
            status,
            tokens: 1_000,
            spent: Spend::default(),
            elapsed_ms: 2_000,
            subagents: 0,
            context: None,
            standing_in_for: None,
            answered: None,
            tries: Vec::new(),
        })
    }

    fn labels(rows: &[WorkflowRow]) -> Vec<(&str, usize)> {
        rows.iter()
            .map(|row| match row {
                WorkflowRow::Agent(agent) => (agent.label.as_str(), agent.tries.len()),
                _ => ("", 0),
            })
            .collect()
    }

    #[test]
    fn each_step_is_one_row_with_its_tries_beneath() {
        let codex = "codex/gpt-5.6-luna";
        let claude = "claude-code/haiku";
        let rows = vec![
            agent("s-001", "build:add-due-dates", claude, RowStatus::Done),
            agent("s-002", "review:due-dates", codex, RowStatus::Failed),
            agent("s-003", "review:due-dates", codex, RowStatus::Failed),
            agent("s-004", "review:due-dates", claude, RowStatus::Done),
            agent("s-005", "judge:due-dates", codex, RowStatus::Failed),
            agent("s-006", "judge:due-dates", codex, RowStatus::Failed),
            agent("s-007", "judge:due-dates", claude, RowStatus::Running),
        ];
        let (rows, folded) = fold(rows, &RunFacts::of(&[]));
        assert_eq!(
            labels(&rows),
            [
                ("build:add-due-dates", 0),
                ("review:due-dates", 2),
                ("judge:due-dates", 2),
            ]
        );
        assert_eq!(folded, Folded { tries: 4, done: 0 });
        let WorkflowRow::Agent(review) = &rows[1] else {
            panic!("the review step is an agent row");
        };
        assert_eq!(review.session, SessionKey::parse("s-004").unwrap());
        let tried: Vec<(String, WorkerRef)> = review
            .tries
            .iter()
            .map(|tried| (tried.session.to_string(), tried.worker.clone()))
            .collect();
        let codex: WorkerRef = codex.parse().unwrap();
        assert_eq!(
            tried,
            [("s-002".into(), codex.clone()), ("s-003".into(), codex)]
        );
    }

    #[test]
    fn rows_without_a_label_are_never_folded() {
        let rows = vec![
            agent("s-001", "", "codex", RowStatus::Failed),
            agent("s-002", "", "codex", RowStatus::Done),
        ];
        let (rows, folded) = fold(rows, &RunFacts::of(&[]));
        assert_eq!(rows.len(), 2);
        assert_eq!(folded, Folded::default());
    }

    #[test]
    fn agents_side_by_side_and_a_finished_round_stay_their_own_rows() {
        let rows = vec![
            agent("s-001", "restate", "claude-code", RowStatus::Done),
            agent("s-002", "restate", "codex", RowStatus::Running),
        ];
        let (rows, folded) = fold(rows, &RunFacts::of(&[]));
        assert_eq!(rows.len(), 2);
        assert_eq!(folded, Folded::default());
    }

    #[test]
    fn a_takeover_folds_the_agent_it_stands_in_for() {
        let mut after = agent("s-002", "review:x", "claude-code", RowStatus::Running);
        if let WorkflowRow::Agent(after) = &mut after {
            after.standing_in_for = Some("codex".parse().unwrap());
        }
        let rows = vec![
            agent("s-001", "review:x", "codex", RowStatus::Waiting),
            after,
        ];
        let (rows, folded) = fold(rows, &RunFacts::of(&[]));
        assert_eq!(labels(&rows), [("review:x", 1)]);
        assert_eq!(folded.tries, 1);
    }
}
