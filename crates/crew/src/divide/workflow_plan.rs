//! The plan checkpoint of a workflow that drafts no plan, such as a review the lead picked: the
//! workflow itself, with the lead's reason, is what you approve. There are no cards to edit or
//! re-plan, so it offers approve, run it solo, abandon, and show the plan.

use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointKind, CheckpointPacket, FreeText, PlanPacket,
};
use argonauts_core::events::plan::TaskGraph;
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::WorkflowId;
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::workflow::{StepKind, WorkflowRef};

use crate::checkpoints::{self, CheckpointRequest, options};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::workflow::{Flow, StepCall, StepDone, align, runner, yaml};

/// Whether there's a plan for the checkpoint to divide into cards: drafted already, or by a
/// step of the workflow. One that isn't recorded is taken to have one, as every plan
/// checkpoint did before workflows could skip planning.
pub fn drafts_plan(state: &RunState, id: &WorkflowId) -> bool {
    let drafted = !state.plan.drafts.is_empty() || !state.plan.coordinated.is_empty();
    drafted
        || state.workflows.by_id.get(id).is_none_or(|record| {
            record
                .workflow
                .phases
                .iter()
                .any(|phase| align::has_kind(&phase.steps, StepKind::Draft))
        })
}

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let decided = checkpoints::open_and_wait(host, request(host, call)?).await?;
    match decided.action {
        AnswerAction::Approve => Ok(StepDone::done("You approved the workflow")),
        AnswerAction::SwitchToSolo => Ok(StepDone {
            end: StepEnd::Done,
            summary: Some("You chose to run it solo".to_owned()),
            flow: Flow::Switch(WorkflowRef::new(runner::solo_workflow(host.settings()))),
        }),
        AnswerAction::Abandon => Ok(StepDone {
            end: StepEnd::Done,
            summary: Some("You abandoned the run".to_owned()),
            flow: Flow::Finish(RunOutcome::Abandoned),
        }),
        other => Err(EngineError::Refused(format!(
            "the plan checkpoint of a workflow with no plan doesn't offer {other:?}"
        ))),
    }
}

fn request(host: &Host, call: &StepCall) -> EngineResult<CheckpointRequest> {
    let shown = host.read(align::plan_workflow);
    let summary = match &shown {
        Some(shown) => shown
            .why
            .clone()
            .unwrap_or_else(|| format!("The run follows the {} workflow", shown.workflow.name)),
        None => "The run follows its workflow".to_owned(),
    };
    let documents = match &shown {
        Some(shown) => vec![EvidenceDocument {
            label: "workflow".to_owned(),
            text: yaml::print(&shown.workflow).map_err(EngineError::Refused)?,
            path: None,
            sha256: None,
        }],
        None => Vec::new(),
    };
    let budget = super::budget_lines::plan(&host.settings().budget)
        .unwrap_or_else(|_| super::empty_budget());
    let packet = PlanPacket {
        summary,
        judging: None,
        near_tie: false,
        decisions: host.read(|state| state.plan.decisions.clone()),
        graph: TaskGraph {
            cards: Vec::new(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        budget,
        new_dependencies: Vec::new(),
        flags: Vec::new(),
        workflow: shown,
    };
    Ok(CheckpointRequest {
        at: Some(call.at.clone()),
        kind: CheckpointKind::Plan,
        title: "Approve this plan?".to_owned(),
        card: None,
        packet: CheckpointPacket::Plan(Box::new(packet)),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents,
        options: Some(answer_options()),
        free_text: FreeText::None,
        authors: Vec::new(),
    })
}

/// The plan checkpoint's options less the ones that work on a drafted plan.
pub fn answer_options() -> Vec<AnswerOption> {
    options::standard(CheckpointKind::Plan)
        .into_iter()
        .filter(|option| {
            !matches!(
                option.action,
                AnswerAction::ApproveWithEdits | AnswerAction::SendBack
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workflow_plan_offers_no_edits_or_send_back() {
        let actions: Vec<AnswerAction> = answer_options()
            .into_iter()
            .map(|option| option.action)
            .collect();
        assert_eq!(
            actions,
            [
                AnswerAction::Approve,
                AnswerAction::SwitchToSolo,
                AnswerAction::Abandon,
                AnswerAction::ShowDiff
            ]
        );
    }
}
