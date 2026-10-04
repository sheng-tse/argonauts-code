//! The check that decides "met": the gates first, in code, then a fresh
//! read-only agent from another company than the lead, one that wrote none of the run's code,
//! judges every criterion against evidence. Code decides from its answers: met only when the
//! gates pass and every criterion is met. Can't tell counts as not yet.

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::goal::{
    Criterion, CriterionState, CriterionVerdict, GoalChecked, GoalEvent, GoalVerdict,
};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::StepName;
use argonauts_core::steps::pursue::{CheckGoal, CheckGoalInput, GoalCheck};

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::integrate::final_check::{FinalCheck, run_base};
use crate::integrate::{diff, merge, merge_packet};
use crate::prompts::goal::CHECK;
use crate::roles;
use crate::state::RunState;
use crate::workflow::{StepCall, change};

use super::pursue;

/// What the check reads of the diff: enough for a few files, and the checker can read the rest.
const DIFF_BYTES: usize = 60_000;
const GATE_BYTES: usize = 4_000;

/// Runs the gates, then the check agent unless a gate failed, and records `Goal::Checked`.
pub async fn judge(
    host: &Host,
    call: &StepCall,
    attempt: u32,
    objective: &str,
    criteria: &[Criterion],
) -> EngineResult<GoalChecked> {
    let gates = merge::checked(host).await?;
    let gate_output = gate_output(&gates);
    let checked = if !gates.passed {
        // The tests decide first, and no model is asked to look past a failing one.
        let reason = failed_gates(&gates);
        GoalChecked {
            attempt,
            verdicts: criteria
                .iter()
                .map(|criterion| CriterionVerdict {
                    id: criterion.id.clone(),
                    verdict: CriterionState::NotYet,
                    evidence: String::new(),
                    missing: reason.clone(),
                })
                .collect(),
            overall: GoalVerdict::NotYet,
            reason,
        }
    } else {
        let checker = checker(host, attempt);
        let (workdir, diff) = diff_since_start(host).await?;
        let answered = host
            .ask::<CheckGoal>(Ask {
                workdir: Some(workdir),
                blind: true,
                ..Ask::new(
                    Some(call.at.clone()),
                    RoleName::Reviewer,
                    checker,
                    CHECK,
                    CheckGoalInput {
                        goal: objective.to_owned(),
                        criteria: criteria.to_vec(),
                        diff,
                        gate_output,
                    },
                )
            })
            .await;
        match answered {
            Ok(answered) => decide(attempt, criteria, &answered.output, true),
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            // A check that couldn't run proves nothing, so nothing is met yet.
            Err(err) => unchecked(attempt, criteria, &format!("the check couldn't run: {err}")),
        }
    };
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Checked(Box::new(checked.clone()))),
    )?;
    Ok(checked)
}

/// Every criterion can't-tell, for `reason`.
fn unchecked(attempt: u32, criteria: &[Criterion], reason: &str) -> GoalChecked {
    GoalChecked {
        attempt,
        verdicts: criteria
            .iter()
            .map(|criterion| CriterionVerdict {
                id: criterion.id.clone(),
                verdict: CriterionState::CantTell,
                evidence: String::new(),
                missing: reason.to_owned(),
            })
            .collect(),
        overall: GoalVerdict::NotYet,
        reason: reason.to_owned(),
    }
}

/// What code decides from the check's answers. A criterion the check left out can't be told,
/// so it isn't met; only every criterion met, with the gates passing, is "met".
pub fn decide(
    attempt: u32,
    criteria: &[Criterion],
    check: &GoalCheck,
    gates_passed: bool,
) -> GoalChecked {
    let verdicts: Vec<CriterionVerdict> = criteria
        .iter()
        .map(|criterion| {
            check
                .verdicts
                .iter()
                .find(|verdict| verdict.id == criterion.id)
                .cloned()
                .unwrap_or_else(|| CriterionVerdict {
                    id: criterion.id.clone(),
                    verdict: CriterionState::CantTell,
                    evidence: String::new(),
                    missing: "the check said nothing about it".to_owned(),
                })
        })
        .collect();
    let met = gates_passed
        && verdicts
            .iter()
            .all(|verdict| verdict.verdict == CriterionState::Met);
    let overall = if met {
        GoalVerdict::Met
    } else if check.overall == GoalVerdict::Impossible {
        GoalVerdict::Impossible
    } else {
        GoalVerdict::NotYet
    };
    let reason = match check.reason.trim() {
        "" => summary(&verdicts),
        reason => reason.to_owned(),
    };
    GoalChecked {
        attempt,
        verdicts,
        overall,
        reason,
    }
}

/// `3 of 4 met; not yet: G4` for a check that gave no reason.
pub fn summary(verdicts: &[CriterionVerdict]) -> String {
    let met = met_count(verdicts);
    let unmet: Vec<&str> = unmet(verdicts).collect();
    if unmet.is_empty() {
        format!("{met} of {} met", verdicts.len())
    } else {
        format!(
            "{met} of {} met; not yet: {}",
            verdicts.len(),
            unmet.join(", ")
        )
    }
}

pub fn met_count(verdicts: &[CriterionVerdict]) -> usize {
    verdicts
        .iter()
        .filter(|verdict| verdict.verdict == CriterionState::Met)
        .count()
}

/// The ids of the criteria not met, can't tell included.
pub fn unmet(verdicts: &[CriterionVerdict]) -> impl Iterator<Item = &str> {
    verdicts
        .iter()
        .filter(|verdict| verdict.verdict != CriterionState::Met)
        .map(|verdict| verdict.id.as_str())
}

/// Who checks attempt `attempt`, always in a fresh session: `goal.checker` when it names one of
/// the crew, else a crew member from another company than the lead that wrote none of the
/// code being checked, else one from any company that wrote none of it, the lead included.
/// Only when everyone wrote some does the lead's own CLI check. Workers outside the
/// crew and the reviewer role are never picked: they may not be set up to run here.
pub fn checker(host: &Host, attempt: u32) -> WorkerRef {
    let settings = host.settings();
    let mut pool = settings.crew.clone();
    for worker in roles::candidates(settings, RoleName::Reviewer) {
        if !pool.contains(&worker) {
            pool.push(worker);
        }
    }
    let available = host.services().workers.available();
    pool.retain(|worker| available.iter().any(|ready| ready.worker == worker.worker));
    if settings.goal.checker != "other_company"
        && let Some(named) = pool
            .iter()
            .find(|worker| worker.to_string() == settings.goal.checker)
    {
        return named.clone();
    }
    let lead = change::lead(host);
    let lead_family = roles::family(host, &lead);
    let authors = host.read(|state| authors(state, attempt));
    let other = |worker: &&WorkerRef| roles::family(host, worker) != lead_family;
    let fresh = |worker: &&WorkerRef| !authors.contains(worker);
    pool.iter()
        .find(|worker| other(worker) && fresh(worker))
        .or_else(|| pool.iter().find(fresh))
        .cloned()
        .unwrap_or(lead)
}

/// Who wrote the code a check reads: a goal attempt's card and its copies, or in a workflow
/// every card. Each owner counts, the first and any that took over, and every session that
/// built it.
fn authors(state: &RunState, attempt: u32) -> Vec<WorkerRef> {
    let attempt_card = pursue::attempt_card_id(attempt);
    let copies = format!("{attempt_card}-");
    let of_attempt = |card: &CardId| card == &attempt_card || card.as_str().starts_with(&copies);
    let checks_an_attempt = state.cards.by_id.keys().any(&of_attempt);
    let checked = |card: &CardId| !checks_an_attempt || of_attempt(card);
    let mut authors: Vec<WorkerRef> = Vec::new();
    let owners = state
        .cards
        .by_id
        .values()
        .filter(|record| checked(&record.spec.id))
        .flat_map(|record| [record.owner.clone(), record.spec.owner.clone()])
        .flatten();
    let builders = state
        .sessions
        .in_order()
        .into_iter()
        .filter(|session| {
            session.started.step == Some(StepName::Build)
                && session.card.as_ref().is_some_and(&checked)
        })
        .map(|session| session.started.worker.clone());
    for worker in owners.chain(builders) {
        if !authors.contains(&worker) {
            authors.push(worker);
        }
    }
    authors
}

/// The landing worktree and the run's whole diff in it, cut to what the check reads.
async fn diff_since_start(host: &Host) -> EngineResult<(std::path::PathBuf, String)> {
    let (_, worktree) = merge_packet::landing_branch(host).await?;
    let text = match run_base(host) {
        Some(base) => host
            .git()
            .review_diff(&worktree, &base, "HEAD")
            .await
            .unwrap_or_default(),
        None => String::new(),
    };
    Ok((worktree, diff::cut(text, DIFF_BYTES)))
}

fn gate_output(gates: &FinalCheck) -> String {
    let mut text = String::new();
    if let Some(problem) = &gates.problem {
        text.push_str(problem);
        text.push('\n');
    }
    for gate in &gates.gates {
        let mark = if gate.passed { "passed" } else { "failed" };
        text.push_str(&format!("{} {mark}: {}\n", gate.name, gate.summary.trim()));
        if !gate.passed && !gate.output_tail.trim().is_empty() {
            text.push_str(gate.output_tail.trim_end());
            text.push('\n');
        }
    }
    if gates.gates.is_empty() && gates.problem.is_none() {
        text.push_str("No gate commands are set.\n");
    }
    diff::cut(text, GATE_BYTES)
}

fn failed_gates(gates: &FinalCheck) -> String {
    if let Some(problem) = &gates.problem {
        return problem.clone();
    }
    let failed: Vec<String> = gates
        .gates
        .iter()
        .filter(|gate| !gate.passed)
        .map(|gate| match gate.summary.trim() {
            "" => format!("{} failed", gate.name),
            summary => format!("{} failed: {summary}", gate.name),
        })
        .collect();
    match failed.as_slice() {
        [] => "the gates failed".to_owned(),
        _ => failed.join("; "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn criteria() -> Vec<Criterion> {
        vec![
            Criterion::new("G1", "slugify collapses dashes"),
            Criterion::new("G2", "median([]) raises ValueError"),
        ]
    }

    fn verdict(id: &str, verdict: CriterionState) -> CriterionVerdict {
        CriterionVerdict {
            id: id.to_owned(),
            verdict,
            evidence: String::new(),
            missing: String::new(),
        }
    }

    fn check(verdicts: Vec<CriterionVerdict>, overall: GoalVerdict) -> GoalCheck {
        GoalCheck {
            verdicts,
            overall,
            reason: String::new(),
        }
    }

    #[test]
    fn only_every_criterion_met_with_the_gates_passing_is_met() {
        let all = vec![
            verdict("G1", CriterionState::Met),
            verdict("G2", CriterionState::Met),
        ];
        let met = decide(
            1,
            &criteria(),
            &check(all.clone(), GoalVerdict::NotYet),
            true,
        );
        assert_eq!(met.overall, GoalVerdict::Met);
        let gates_failed = decide(1, &criteria(), &check(all, GoalVerdict::Met), false);
        assert_eq!(gates_failed.overall, GoalVerdict::NotYet);
    }

    #[test]
    fn cant_tell_and_silence_are_not_yet() {
        let unsure = vec![
            verdict("G1", CriterionState::Met),
            verdict("G2", CriterionState::CantTell),
        ];
        let checked = decide(2, &criteria(), &check(unsure, GoalVerdict::Met), true);
        assert_eq!(checked.overall, GoalVerdict::NotYet);
        assert_eq!(checked.reason, "1 of 2 met; not yet: G2");

        let silent = decide(
            2,
            &criteria(),
            &check(vec![verdict("G1", CriterionState::Met)], GoalVerdict::Met),
            true,
        );
        assert_eq!(silent.verdicts[1].verdict, CriterionState::CantTell);
        assert_eq!(silent.overall, GoalVerdict::NotYet);
    }

    #[test]
    fn the_check_may_say_it_cant_be_met() {
        let checked = decide(
            1,
            &criteria(),
            &check(
                vec![verdict("G1", CriterionState::Met)],
                GoalVerdict::Impossible,
            ),
            true,
        );
        assert_eq!(checked.overall, GoalVerdict::Impossible);
    }
}
