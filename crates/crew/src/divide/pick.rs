//! A near tie, or `judging.method: human`: you pick the plan before it's divided. Your pick is
//! recorded as the judged result's winner, and its decision records are written as a judged
//! winner's are.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointKind, CheckpointPacket, FreeText, PlanPacket,
};
use argonauts_core::events::plan::{JudgingResult, PlanEvent, TaskGraph};
use argonauts_core::names::PlanLabel;
use argonauts_core::policy::EvidenceDocument;
use argonauts_core::{Note, NoteLevel};

use super::plan_facts;
use crate::checkpoints::{self, CheckpointRequest, options};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::plan::{decisions, drafts};
use crate::state::plan::SealedDraft;
use crate::workflow::{StepCall, align};

/// Opens the pick card when the plans were judged without a winner, and records your pick.
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
    let request = request(host, call, &judged, &latest)?;
    let decided = checkpoints::open_and_wait(host, request).await?;
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

fn request(
    host: &Host,
    call: &StepCall,
    judged: &JudgingResult,
    latest: &[SealedDraft],
) -> EngineResult<CheckpointRequest> {
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
            cards: Vec::new(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        budget: super::empty_budget(),
        new_dependencies: Vec::new(),
        flags: review.flags(),
        workflow: host.read(align::plan_workflow),
    };
    Ok(CheckpointRequest {
        at: Some(call.at.clone()),
        kind: CheckpointKind::Plan,
        title: "Which plan should the run follow?".to_owned(),
        card: None,
        packet: CheckpointPacket::Plan(Box::new(packet)),
        facts: review.facts(),
        agent_outputs,
        documents,
        options: Some(options::plan_pick(judged)),
        free_text: FreeText::None,
        authors: latest.iter().map(|draft| draft.worker.clone()).collect(),
    })
}
