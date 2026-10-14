//! A step tried more than once is one row. Every try at a step runs under the step's label, so
//! the newest try stands for the step, and the ones before it, each failed try, the agent it
//! is the fallback for and each earlier round of a review, fold beneath it. A review asked again
//! after its round sent the work back reads `review:per-key · round 2`. Agents that run side by
//! side in a step stay rows of their own. The row keeps the place of the step's first try, so
//! the steps stay in the order they started.

use std::collections::BTreeMap;

use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::views::{AgentRow, RowStatus, StepTry, WorkflowRow};

use crate::views::sessions::RunFacts;

/// What folding took out of a phase's rows, so its counts can be counted in steps.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Folded {
    pub tries: u32,
    pub done: u32,
}

/// A row as folding keeps it.
struct Kept {
    row: WorkflowRow,
    /// Which start of its session the row shows: a session that goes again after a failure
    /// starts again under the same key, so its rows are its starts in order.
    nth: usize,
    /// The label of the step it stands for, without a round.
    step: String,
}

/// Folds every earlier try at a step under the step's newest row.
pub(super) fn fold(rows: Vec<WorkflowRow>, sessions: &RunFacts) -> (Vec<WorkflowRow>, Folded) {
    let mut folded = Folded::default();
    let mut kept: Vec<Kept> = Vec::with_capacity(rows.len());
    let mut starts: BTreeMap<SessionKey, usize> = BTreeMap::new();
    for row in rows {
        let WorkflowRow::Agent(mut agent) = row else {
            kept.push(Kept {
                row,
                nth: 0,
                step: String::new(),
            });
            continue;
        };
        let start = starts.entry(agent.session.clone()).or_default();
        let nth = *start;
        *start += 1;
        let label = agent.label.trim().to_owned();
        let judges = judges(sessions, &agent.session);
        let earlier = (!label.is_empty())
            .then(|| {
                kept.iter().rposition(|kept| {
                    let WorkflowRow::Agent(known) = &kept.row else {
                        return false;
                    };
                    let same_step = kept.step == label || known.label.trim() == label;
                    (same_step && tried_before(known, &agent))
                        || (judges
                            && asked_again(known, &agent)
                            && (same_step || stem(&label) == Some(kept.step.as_str())))
                })
            })
            .flatten();
        let Some(at) = earlier else {
            kept.push(Kept {
                row: WorkflowRow::Agent(agent),
                nth,
                step: label,
            });
            continue;
        };
        if let WorkflowRow::Agent(before) = &kept[at].row {
            folded.tries += 1;
            if before.status == RowStatus::Done {
                folded.done += 1;
            }
            let mut tried = try_of(before, kept[at].nth, sessions);
            if !tried_before(before, &agent) {
                tried.round = Some(rounds(&before.tries) + 1);
            }
            let mut tries = before.tries.clone();
            tries.push(tried);
            tries.append(&mut agent.tries);
            agent.tries = tries;
        }
        kept[at].row = WorkflowRow::Agent(agent);
        kept[at].nth = nth;
    }
    let rows = kept
        .into_iter()
        .map(|kept| match kept.row {
            WorkflowRow::Agent(mut agent) if rounds(&agent.tries) > 0 => {
                agent.label = format!("{} · round {}", kept.step, rounds(&agent.tries) + 1);
                WorkflowRow::Agent(agent)
            }
            row => row,
        })
        .collect();
    (rows, folded)
}

/// Whether `before` was a try `after` came after: one that failed, or the agent `after` took
/// over from. Two agents a step runs side by side share its label too, and stay two rows.
fn tried_before(before: &AgentRow, after: &AgentRow) -> bool {
    // A session held or stopped, then gone on with, is an earlier try of the step too.
    let ended_early = matches!(
        before.status,
        RowStatus::Failed | RowStatus::Paused | RowStatus::Skipped
    );
    ended_early || after.fallback_for.as_ref() == Some(&before.worker)
}

/// Whether `after` is the same reviewer asked again once `before`, its round, finished. Two
/// reviewers of one round are two workers, and stay two rows.
fn asked_again(before: &AgentRow, after: &AgentRow) -> bool {
    before.status == RowStatus::Done && before.worker == after.worker
}

/// Whether the session reviews or signs off, the agents a step asks again round after round.
fn judges(sessions: &RunFacts, session: &SessionKey) -> bool {
    sessions
        .session(session)
        .is_some_and(|facts| matches!(facts.started.role, RoleName::Reviewer | RoleName::Judge))
}

/// The step's label inside a later round's, such as `review:per-key` for `review:per-key-2`,
/// which the run made unique, or for `review:per-key · round 2`, named for its round.
fn stem(label: &str) -> Option<&str> {
    fn numbered<'a>((stem, n): (&'a str, &str)) -> Option<&'a str> {
        n.parse::<u32>().is_ok().then_some(stem)
    }
    label
        .rsplit_once(" · round ")
        .and_then(numbered)
        .or_else(|| label.rsplit_once('-').and_then(numbered))
}

/// How many earlier rounds the step's tries hold.
fn rounds(tries: &[StepTry]) -> u32 {
    let count = tries.iter().filter(|tried| tried.round.is_some()).count();
    u32::try_from(count).unwrap_or(u32::MAX)
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
        round: None,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::names::{ModelRef, SessionKey};

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
            fallback_for: None,
            answered: false,
            tries: Vec::new(),
            note: None,
            inner: None,
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
        let tried: Vec<(String, ModelRef)> = review
            .tries
            .iter()
            .map(|tried| (tried.session.to_string(), tried.worker.clone()))
            .collect();
        let codex: ModelRef = codex.parse().unwrap();
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
            agent("s-001", "read", "claude-code", RowStatus::Done),
            agent("s-002", "read", "codex", RowStatus::Running),
        ];
        let (rows, folded) = fold(rows, &RunFacts::of(&[]));
        assert_eq!(rows.len(), 2);
        assert_eq!(folded, Folded::default());
    }

    #[test]
    fn a_fallback_folds_the_agent_it_falls_back_for() {
        let mut after = agent("s-002", "review:x", "claude-code", RowStatus::Running);
        if let WorkflowRow::Agent(after) = &mut after {
            after.fallback_for = Some("codex".parse().unwrap());
        }
        let rows = vec![
            agent("s-001", "review:x", "codex", RowStatus::Waiting),
            after,
        ];
        let (rows, folded) = fold(rows, &RunFacts::of(&[]));
        assert_eq!(labels(&rows), [("review:x", 1)]);
        assert_eq!(folded.tries, 1);
    }

    /// The facts of sessions that each start once in `role`.
    fn facts_of(sessions: &[(&str, RoleName)]) -> RunFacts {
        let events: Vec<_> = sessions
            .iter()
            .zip(1..)
            .map(|((session, role), seq)| {
                let key = SessionKey::parse(session).unwrap();
                crate::client::test_support::session_started(seq, &key, None, *role, "claude-code")
            })
            .collect();
        RunFacts::of(&events)
    }

    fn rounds_of(row: &WorkflowRow) -> Vec<Option<u32>> {
        match row {
            WorkflowRow::Agent(agent) => agent.tries.iter().map(|tried| tried.round).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_review_asked_again_folds_under_its_step_as_its_round() {
        let claude = "claude-code/haiku";
        let codex = "codex/gpt-5.6-luna";
        let sessions = facts_of(&[
            ("s-001", RoleName::Worker),
            ("s-002", RoleName::Reviewer),
            ("s-003", RoleName::Reviewer),
            ("s-004", RoleName::Reviewer),
            ("s-005", RoleName::Reviewer),
            ("s-006", RoleName::Judge),
        ]);
        let rows = vec![
            agent("s-001", "build:per-key", codex, RowStatus::Done),
            agent("s-002", "review:per-key", claude, RowStatus::Done),
            agent("s-003", "review:per-key", codex, RowStatus::Done),
            agent("s-004", "review:per-key", claude, RowStatus::Done),
            agent("s-005", "review:per-key", codex, RowStatus::Running),
            agent("s-006", "sign-off:per-key", claude, RowStatus::Done),
        ];
        let (rows, folded) = fold(rows, &sessions);
        assert_eq!(
            labels(&rows),
            [
                ("build:per-key", 0),
                ("review:per-key · round 2", 1),
                ("review:per-key · round 2", 1),
                ("sign-off:per-key", 0),
            ],
            "each reviewer's second round folds under its first, and two reviewers stay two",
        );
        assert_eq!(rounds_of(&rows[1]), [Some(1)]);
        assert_eq!(folded, Folded { tries: 2, done: 2 });
        let WorkflowRow::Agent(review) = &rows[1] else {
            panic!("the review step is an agent row");
        };
        assert_eq!(review.session, SessionKey::parse("s-004").unwrap());
    }

    #[test]
    fn a_round_made_unique_and_a_failed_try_inside_it_count_as_they_happened() {
        let claude = "claude-code/haiku";
        let codex = "codex/gpt-5.6-luna";
        let sessions = facts_of(&[
            ("s-001", RoleName::Judge),
            ("s-002", RoleName::Judge),
            ("s-003", RoleName::Judge),
            ("s-004", RoleName::Judge),
        ]);
        let mut fallback = agent("s-004", "sign-off:per-key-2", claude, RowStatus::Done);
        if let WorkflowRow::Agent(fallback) = &mut fallback {
            fallback.fallback_for = Some(codex.parse().unwrap());
        }
        let rows = vec![
            agent("s-001", "sign-off:per-key", codex, RowStatus::Done),
            agent("s-002", "sign-off:per-key-2", codex, RowStatus::Failed),
            agent("s-003", "sign-off:per-key-2", codex, RowStatus::Failed),
            fallback,
        ];
        let (rows, _) = fold(rows, &sessions);
        assert_eq!(labels(&rows), [("sign-off:per-key · round 2", 3)]);
        assert_eq!(rounds_of(&rows[0]), [Some(1), None, None]);
    }

    #[test]
    fn only_reviews_and_sign_offs_are_asked_again_in_rounds() {
        let sessions = facts_of(&[("s-001", RoleName::Worker), ("s-002", RoleName::Worker)]);
        let rows = vec![
            agent("s-001", "build:per-key", "codex", RowStatus::Done),
            agent("s-002", "build:per-key", "codex", RowStatus::Done),
        ];
        let (rows, folded) = fold(rows, &sessions);
        assert_eq!(rows.len(), 2);
        assert_eq!(folded, Folded::default());
    }

    #[test]
    fn a_round_named_for_its_number_folds_under_its_step_too() {
        let claude = "claude-code/haiku";
        let sessions = facts_of(&[
            ("s-001", RoleName::Reviewer),
            ("s-002", RoleName::Reviewer),
            ("s-003", RoleName::Reviewer),
        ]);
        let rows = vec![
            agent("s-001", "review:add-due-dates", claude, RowStatus::Done),
            agent(
                "s-002",
                "review:add-due-dates · round 2",
                claude,
                RowStatus::Done,
            ),
            agent(
                "s-003",
                "review:add-due-dates · round 3",
                claude,
                RowStatus::Running,
            ),
        ];
        let (rows, _) = fold(rows, &sessions);
        assert_eq!(labels(&rows), [("review:add-due-dates · round 3", 2)]);
        assert_eq!(rounds_of(&rows[0]), [Some(1), Some(2)]);
        assert_eq!(stem("review:fix-it"), None);
    }
}
