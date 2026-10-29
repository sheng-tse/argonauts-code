//! A near tie, or `judging.method: human`: you pick the plan before it's divided. Your pick is
//! recorded as the judged result's winner, and its decision records are written as a judged
//! winner's are. When the main agent answers the crew's questions, a near tie goes to the plan the
//! judges scored highest instead, and the plan approval after it checks that plan.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalKind, ApprovalPacket, FreeText, PlanPacket,
};
use argonauts_core::events::plan::{JudgingResult, PlanEvent, TaskGraph};
use argonauts_core::names::PlanLabel;
use argonauts_core::policy::EvidenceDocument;
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::{Note, NoteLevel};

use super::plan_facts;
use crate::approvals::{self, ApprovalRequest, options};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::plan::{decisions, drafts};
use crate::state::plan::SealedDraft;
use crate::workflow::StepCall;

/// Opens the pick question when the plans were judged without a winner, and records your pick.
/// Nothing to do when a plan is already chosen.
pub async fn pick_if_tied(host: &Host, call: &StepCall) -> EngineResult<()> {
    let tied = host.read(|state| {
        let judged = state.plan.judged.clone()?;
        let latest: Vec<SealedDraft> = state.plan.latest().into_iter().cloned().collect();
        (judged.winner.is_none() && latest.len() > 1).then_some((judged, latest))
    });
    let Some((judged, latest)) = tied else {
        return Ok(());
    };
    let by_main_agent = judged.method != JudgingMethod::Human
        && approvals::answerer(host, ApprovalKind::Plan).answers_for_you();
    if by_main_agent && let Some(top) = top_scored(&judged) {
        let chosen = latest
            .iter()
            .find(|draft| draft.label == top)
            .ok_or_else(|| EngineError::Refused(format!("no Plan {top} to take")))?;
        host.record(
            None,
            None,
            RunEventKind::Plan(PlanEvent::Judged(JudgingResult {
                winner: Some(top),
                ..judged
            })),
        )?;
        host.record(
            None,
            None,
            RunEventKind::Note(Note {
                level: NoteLevel::Info,
                text: format!(
                    "The judges couldn't separate the plans, so Plan {top}, which they scored \
                     highest, goes ahead for the plan approval to check"
                ),
            }),
        )?;
        decisions::record_for(host, chosen).await?;
        return Ok(());
    }
    let request = request(host, call, &judged, &latest)?;
    let decided = approvals::open_and_wait(host, request).await?;
    let label = match &decided.action {
        AnswerAction::Choose { value } => value.parse::<PlanLabel>().ok(),
        _ => None,
    };
    let chosen = label
        .and_then(|label| latest.iter().find(|draft| draft.label == label))
        .ok_or_else(|| {
            EngineError::Refused(format!(
                "{:?} doesn't pick one of the plans at {}.",
                decided.action, decided.id
            ))
        })?;
    host.record(
        None,
        None,
        RunEventKind::Plan(PlanEvent::Judged(JudgingResult {
            winner: Some(chosen.label),
            ..judged
        })),
    )?;
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: format!("You picked Plan {} at {}", chosen.label, decided.id),
        }),
    )?;
    decisions::record_for(host, chosen).await?;
    Ok(())
}

/// The plan with the best mean score, the earlier label on a tie. `None` when no judge's score
/// counted.
fn top_scored(judged: &JudgingResult) -> Option<PlanLabel> {
    judged
        .plans
        .iter()
        .filter(|tally| tally.counted > 0 && tally.mean.is_finite())
        .min_by(|a, b| b.mean.total_cmp(&a.mean).then(a.label.cmp(&b.label)))
        .map(|tally| tally.label)
}

fn request(
    host: &Host,
    call: &StepCall,
    judged: &JudgingResult,
    latest: &[SealedDraft],
) -> EngineResult<ApprovalRequest> {
    let review = plan_facts::review(host);
    let plans = drafts::labeled(&latest.iter().collect::<Vec<_>>())?;
    let documents = plans
        .into_iter()
        .map(|plan| EvidenceDocument {
            label: format!("Plan {}", plan.label),
            text: plan.text,
            path: None,
            sha256: None,
        })
        .collect();
    let agent_outputs = judged
        .plans
        .iter()
        .map(|tally| {
            format!(
                "Plan {}: mean {:.1} over {} judges, spread {:.1}, {} scores dropped",
                tally.label, tally.mean, tally.counted, tally.spread, tally.dropped
            )
        })
        .collect();
    let summary = if judged.near_tie {
        format!(
            "The judges couldn't separate the plans (margin {:.1}). Pick the one to divide.",
            judged.margin
        )
    } else {
        "You judge the plans. Pick the one to divide.".to_owned()
    };
    let packet = PlanPacket {
        summary,
        judging: Some(judged.clone()),
        near_tie: judged.near_tie,
        decisions: host.read(|state| state.plan.decisions.clone()),
        graph: TaskGraph {
            tasks: Vec::new(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        limit: super::empty_limit(),
        new_dependencies: Vec::new(),
        flags: review.flags(),
        workflow: host.read(super::workflow_plan::plan_workflow),
        seats: Vec::new(),
        objections: Vec::new(),
        offer: None,
    };
    Ok(ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Plan,
        title: "Which plan should the run follow?".to_owned(),
        task: None,
        packet: ApprovalPacket::Plan(Box::new(packet)),
        facts: review.facts(),
        agent_outputs,
        documents,
        options: Some(options::plan_pick(judged)),
        free_text: FreeText::None,
        authors: latest.iter().map(|draft| draft.worker.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::plan::PlanTally;

    use super::*;

    fn tally(label: PlanLabel, mean: f64, counted: u32) -> PlanTally {
        PlanTally {
            label,
            author: "codex".parse().expect("a model"),
            mean,
            spread: 0.0,
            counted,
            dropped: 0,
        }
    }

    #[test]
    fn the_main_agent_takes_the_best_mean_and_the_earlier_label_on_a_tie() {
        let judged = |plans| JudgingResult {
            method: JudgingMethod::Cross,
            plans,
            winner: None,
            margin: 0.2,
            near_tie: true,
        };
        let close = judged(vec![tally(1, 7.1, 2), tally(2, 7.3, 2), tally(3, 9.0, 0)]);
        assert_eq!(top_scored(&close), Some(2));
        let even = judged(vec![tally(2, 7.0, 2), tally(1, 7.0, 2)]);
        assert_eq!(top_scored(&even), Some(1));
        assert_eq!(top_scored(&judged(vec![tally(1, 7.0, 0)])), None);
    }
}
