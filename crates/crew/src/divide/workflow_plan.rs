//! The plan approval of a workflow that drafts no plan, such as a review the main agent picked: the
//! workflow itself, with the main agent's reason, is what you approve. There are no tasks to edit
//! or re-plan, so it offers approve, run it as Captain, abandon, and show the plan. Nobody
//! contested it, so its facts are an uncontested plan's, made against the commit the run started
//! from, and auto can settle it like any other plan.

use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, FreeText, PlanPacket, PlanWorkflow,
};
use argonauts_core::events::plan::{LimitPlan, TaskGraph};
use argonauts_core::events::run::RunOutcome;
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::{ModelRef, WorkflowId};
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::settings::limit::LimitUnit;
use argonauts_core::steps::StepName;
use argonauts_core::steps::design::DesignOutput;
use argonauts_core::workflow::{StepKind, WorkflowRef};

use crate::approvals::{self, ApprovalRequest, options};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::workflow::{Flow, StepCall, StepDone, runner, shape, yaml};

/// Whether there's a plan for the approval to divide into tasks: drafted already, or by a
/// step of the workflow. One that isn't recorded is taken to have one, as every plan
/// approval did before workflows could skip planning.
pub fn drafts_plan(state: &RunState, id: &WorkflowId) -> bool {
    let drafted = !state.plan.drafts.is_empty() || !state.plan.orchestrated.is_empty();
    drafted
        || state.workflows.by_id.get(id).is_none_or(|record| {
            record
                .workflow
                .phases
                .iter()
                .any(|phase| shape::has_kind(&phase.steps, StepKind::Draft))
        })
}

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let decided = approvals::open_and_wait(host, request(host, call).await?).await?;
    match decided.action {
        AnswerAction::Approve => Ok(StepDone::done("You approved the workflow")),
        AnswerAction::SwitchToCaptain => Ok(StepDone {
            end: StepEnd::Done,
            summary: Some("You chose to run it as Captain".to_owned()),
            flow: Flow::Switch(WorkflowRef::new(runner::captain_workflow(host.settings()))),
        }),
        AnswerAction::Abandon => Ok(StepDone {
            end: StepEnd::Done,
            summary: Some("You abandoned the run".to_owned()),
            flow: Flow::Finish(RunOutcome::Abandoned),
        }),
        other => Err(EngineError::Refused(format!(
            "the plan approval of a workflow with no plan doesn't offer {other:?}"
        ))),
    }
}

async fn request(host: &Host, call: &StepCall) -> EngineResult<ApprovalRequest> {
    let workflow = host.read(plan_workflow);
    let summary = match &workflow {
        Some(shown) => shown
            .why
            .clone()
            .unwrap_or_else(|| format!("The run follows the {} workflow", shown.workflow.name)),
        None => "The run follows its workflow".to_owned(),
    };
    let documents = match &workflow {
        Some(shown) => vec![EvidenceDocument {
            label: "workflow".to_owned(),
            text: yaml::print(&shown.workflow).map_err(EngineError::Refused)?,
            path: None,
            sha256: None,
        }],
        None => Vec::new(),
    };
    let shown = Shown {
        workflow,
        summary,
        documents,
        options: answer_options(),
        free_text: FreeText::None,
        authors: host.read(designer).into_iter().collect(),
    };
    Ok(request_showing(host, call, shown).await)
}

/// A plan approval whose plan is a workflow alone: what it shows, and what it offers.
pub struct Shown {
    pub workflow: Option<PlanWorkflow>,
    pub summary: String,
    /// What a judge reads: the workflow as it would run.
    pub documents: Vec<EvidenceDocument>,
    pub options: Vec<AnswerOption>,
    pub free_text: FreeText,
    /// Who wrote it, so a judge comes from another provider.
    pub authors: Vec<ModelRef>,
}

/// The plan approval over `shown`, with an uncontested plan's facts.
pub async fn request_showing(host: &Host, call: &StepCall, shown: Shown) -> ApprovalRequest {
    let limit = super::limit_plan(host.settings());
    let review = super::plan_facts::review(host);
    let facts = EvidenceFacts {
        subject: super::plan_approval::subject(host).await,
        estimate_usd: estimate_usd(&limit),
        ..review.facts()
    };
    let packet = PlanPacket {
        summary: shown.summary,
        judging: None,
        near_tie: false,
        decisions: host.read(|state| state.plan.decisions.clone()),
        graph: TaskGraph {
            tasks: Vec::new(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        limit,
        new_dependencies: Vec::new(),
        flags: review.flags(),
        workflow: shown.workflow,
    };
    ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Plan,
        title: "Approve this plan?".to_owned(),
        task: None,
        packet: ApprovalPacket::Plan(Box::new(packet)),
        facts,
        agent_outputs: Vec::new(),
        documents: shown.documents,
        options: Some(shown.options),
        free_text: shown.free_text,
        authors: shown.authors,
    }
}

/// The top of the plan's estimate, when the run's limit is in dollars.
fn estimate_usd(limit: &LimitPlan) -> Option<f64> {
    (limit.total.unit == LimitUnit::Usd && limit.estimate_high > 0.0).then_some(limit.estimate_high)
}

/// Who designed the run's workflow, so the judge comes from another provider.
fn designer(state: &RunState) -> Option<ModelRef> {
    state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .find(|session| session.started.step == Some(StepName::DesignWorkflow))
        .map(|session| session.started.worker.clone())
}

/// The plan approval's options less the ones that work on a drafted plan.
pub fn answer_options() -> Vec<AnswerOption> {
    options::standard(ApprovalKind::Plan)
        .into_iter()
        .filter(|option| {
            !matches!(
                option.action,
                AnswerAction::ApproveWithEdits | AnswerAction::SendBack
            )
        })
        .collect()
}

/// The run's workflow for the plan approval, with the main agent's reason when the main agent
/// picked it.
pub fn plan_workflow(state: &RunState) -> Option<PlanWorkflow> {
    let root = state.workflows.root.as_ref()?;
    let record = state.workflows.by_id.get(root)?;
    Some(PlanWorkflow {
        from: record.from.clone(),
        why: design_why(state, record.from.as_ref()),
        workflow: record.workflow.clone(),
    })
}

/// The newest design for the task that named this template, or composed one when `from` is
/// `None`: its one line for the plan approval.
fn design_why(state: &RunState, from: Option<&WorkflowRef>) -> Option<String> {
    state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .find_map(|session| {
            let started = &session.started;
            if started.step != Some(StepName::DesignWorkflow) || started.workflow_step.is_some() {
                return None;
            }
            let structured = session.end.as_ref()?.structured.clone()?;
            let output: DesignOutput = serde_json::from_value(structured).ok()?;
            let why = output.why.trim();
            let named = match from {
                Some(from) => output.based_on_names(from.as_str()),
                None => output.based_on.is_none(),
            };
            (named && !why.is_empty()).then(|| why.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::session::{EndReason, SessionEvent};
    use argonauts_core::names::RoleName;

    use crate::state::tests::{end, envelope, session_started};

    /// The plan question carries the main agent's why when the template that runs is the one its
    /// brief named, however the main agent spelled it, since the log keeps the brief as written.
    #[test]
    fn the_main_agents_why_follows_the_template_it_spelled() {
        let mut state = RunState::default();
        let mut started =
            session_started(&ModelRef::new("claude-code", None), RoleName::TieBreaker);
        started.step = Some(StepName::DesignWorkflow);
        let kind = RunEventKind::Session(SessionEvent::Started(Box::new(started)));
        state.apply(&envelope(1, None, Some("s-001"), kind));
        let mut ended = end(EndReason::Completed, Some("v-1"));
        ended.structured = Some(serde_json::json!({
            "name": "fix-it", "description": "Fix it", "based_on": "Council",
            "why": "each bug has one clear fix", "done_when": [],
        }));
        let kind = RunEventKind::Session(SessionEvent::Ended(Box::new(ended)));
        state.apply(&envelope(2, None, Some("s-001"), kind));
        assert_eq!(
            design_why(&state, Some(&WorkflowRef::new("council"))).as_deref(),
            Some("each bug has one clear fix")
        );
        assert_eq!(design_why(&state, Some(&WorkflowRef::new("captain"))), None);
        assert_eq!(design_why(&state, None), None);
    }

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
                AnswerAction::SwitchToCaptain,
                AnswerAction::Abandon,
                AnswerAction::ShowDiff
            ]
        );
    }
}
