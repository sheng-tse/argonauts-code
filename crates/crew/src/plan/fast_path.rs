//! The fast path when no topic is disputed: one agent merges the drafts, a judge checks the
//! merge is complete, and `Plan::FastPath` is recorded.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::plan::{
    Draft, DraftInput, Judge, JudgeInput, LabeledPlan, PlanDraft, RubricCriterion,
};
use argonauts_core::worker::Access;
use argonauts_core::workflow::StandardPhase;

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::lead;
use crate::plan::context::{self, crew_for, families, part, workers_for};
use crate::plan::{decisions, drafts};
use crate::prompts::plan::{MERGE_CHECK, MERGE_DRAFTS, SYNTHESIZE};
use crate::state::plan::{MERGED_LABEL, SealedDraft};
use crate::workflow::{StepCall, StepDone};

/// A completeness score under this sends the merge back once with the judge's reasons.
const COMPLETE_AT: f64 = 7.0;

pub async fn merge(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let (latest, disputed, mapped, fast, judged, merged) = host.read(|state| {
        let plan = &state.plan;
        (
            plan.latest().into_iter().cloned().collect::<Vec<_>>(),
            plan.disputed().len(),
            plan.latest_map().is_some(),
            plan.fast_path.is_some(),
            plan.judged.is_some(),
            plan.merged().cloned(),
        )
    });
    if latest.is_empty() && call.standard != StandardPhase::Plan {
        return synthesize_input(host, call).await;
    }
    if fast {
        return Ok(StepDone::done("The drafts were merged"));
    }
    if judged {
        return Ok(StepDone::skipped("The plans were judged"));
    }
    if latest.is_empty() {
        return Ok(StepDone::skipped("No drafts to merge"));
    }
    if latest.len() > 1 && !mapped {
        return Ok(StepDone::skipped("The drafts aren't mapped yet"));
    }
    if disputed > 0 {
        return Ok(StepDone::skipped(format!("{disputed} topics are disputed")));
    }
    let (merger, chosen) = match (latest.as_slice(), merged) {
        ([only], _) => (only.worker.clone(), only.clone()),
        (_, Some(merged)) => (merged.worker.clone(), merged),
        _ => {
            let merger = merger(host, call);
            let merged = merge_drafts(host, call, &merger, &latest).await?;
            (merger, merged)
        }
    };
    host.record(
        None,
        None,
        RunEventKind::Plan(PlanEvent::FastPath {
            merged_by: merger.clone(),
        }),
    )?;
    decisions::record_for(host, &chosen).await?;
    let summary = if latest.len() == 1 {
        "One draft, taken as the plan".to_owned()
    } else {
        format!("{merger} merged {} drafts", latest.len())
    };
    context::phase_finished(host, &summary)?;
    Ok(StepDone::done(summary))
}

/// The step's worker, else the phase lead, whose process work this is.
fn merger(host: &Host, call: &StepCall) -> WorkerRef {
    match &call.step.workers {
        argonauts_core::workflow::StepWorkers::Named(named) if !named.is_empty() => {
            named[0].clone()
        }
        _ => host
            .read(|state| lead::phase_lead(state, host.settings()))
            .unwrap_or_else(|| host.settings().default_agent.clone()),
    }
}

async fn merge_drafts(
    host: &Host,
    call: &StepCall,
    merger: &WorkerRef,
    latest: &[SealedDraft],
) -> EngineResult<SealedDraft> {
    let plans = drafts::labeled(&latest.iter().collect::<Vec<_>>())?;
    let all = plans_text(&plans);
    let mut gaps: Option<String> = None;
    let mut merged = None;
    for slot in 0..2u32 {
        let answered = ask_merge(host, call, merger, &all, gaps.as_deref(), slot).await?;
        let text = drafts::render(&answered.output);
        merged = Some((answered.output, answered.session));
        match check_complete(host, call, merger, &all, &text, slot).await? {
            Some((score, rationale)) if score < COMPLETE_AT => {
                gaps = Some(format!(
                    "A judge scored the merge {score:.1} of 10 for completeness: {rationale}"
                ));
            }
            _ => break,
        }
    }
    let Some((draft, session)) = merged else {
        return Err(EngineError::Refused("the merge gave no plan".to_owned()));
    };
    let round = host.read(|state| state.plan.current_round());
    let id = drafts::seal(host, MERGED_LABEL, merger, round, &draft, Some(&session))?;
    host.read(|state| state.plan.drafts.iter().find(|d| d.draft == id).cloned())
        .ok_or_else(|| EngineError::Refused(format!("draft {id} was not recorded")))
}

fn plans_text(plans: &[LabeledPlan]) -> String {
    plans
        .iter()
        .map(|plan| format!("# Plan {}\n\n{}", plan.label, plan.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

async fn ask_merge(
    host: &Host,
    call: &StepCall,
    merger: &WorkerRef,
    plans: &str,
    gaps: Option<&str>,
    slot: u32,
) -> EngineResult<crate::engine::host::Answered<PlanDraft>> {
    let input = host.read(|state| DraftInput {
        goal: context::goal_brief(state),
        notes: Vec::new(),
        decisions: context::decision_lines(state),
        status: String::new(),
    });
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::PhaseLead,
        merger.clone(),
        MERGE_DRAFTS,
        input,
    );
    ask.access = Access::ReadOnly;
    ask.slot = slot;
    ask.parts.push(part(
        InputPartKind::StepInput,
        "the drafts",
        plans.to_owned(),
    ));
    if let Some(gaps) = gaps {
        ask.parts.push(part(
            InputPartKind::Message,
            "completeness",
            gaps.to_owned(),
        ));
    }
    host.ask::<Draft>(ask).await
}

/// A judge of another family than the merger scores the merge's completeness. `None` when no
/// such judge is available or its answer failed; the merge then stands.
async fn check_complete(
    host: &Host,
    call: &StepCall,
    merger: &WorkerRef,
    plans: &str,
    merged: &str,
    slot: u32,
) -> EngineResult<Option<(f64, String)>> {
    let family = families(host);
    let merger_family = family(merger);
    let judge = crew_for(host.settings(), RoleName::Judge)
        .into_iter()
        .find(|judge| judge != merger && family(judge) != merger_family);
    let Some(judge) = judge else {
        return Ok(None);
    };
    let input = JudgeInput {
        goal: host.read(context::goal_brief),
        rubric: vec![RubricCriterion {
            name: "completeness".to_owned(),
            description: "10 means the merged plan keeps every piece, choice, risk, and test \
                          of the drafts, or says why it drops one."
                .to_owned(),
        }],
        plans: vec![LabeledPlan {
            label: 1,
            text: merged.to_owned(),
        }],
        spikes: Vec::new(),
    };
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::Judge,
        judge,
        MERGE_CHECK,
        input,
    );
    ask.access = Access::NoTools;
    ask.blind = true;
    ask.slot = slot;
    ask.parts.push(part(
        InputPartKind::StepInput,
        "the drafts",
        plans.to_owned(),
    ));
    match host.ask::<Judge>(ask).await {
        Ok(answered) => Ok(answered.output.scores.first().and_then(|score| {
            let value = *score.criteria.first()?;
            value
                .is_finite()
                .then(|| (value.clamp(0.0, 10.0), score.rationale.clone()))
        })),
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        Err(_) => Ok(None),
    }
}

/// Outside planning: one agent combines what the step's input holds.
async fn synthesize_input(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let role = call.step.role.unwrap_or(RoleName::PhaseLead);
    let worker = workers_for(call, host.settings(), role, Some(1))
        .into_iter()
        .next()
        .unwrap_or_else(|| host.settings().default_agent.clone());
    let text = context::step_input(host, call).await?;
    let input = host.read(|state| DraftInput {
        goal: context::goal_brief(state),
        notes: Vec::new(),
        decisions: context::decision_lines(state),
        status: String::new(),
    });
    let mut ask = Ask::new(Some(call.at.clone()), role, worker, SYNTHESIZE, input);
    ask.access = Access::ReadOnly;
    ask.parts
        .push(part(InputPartKind::StepInput, "results", text));
    let answered = host.ask::<Draft>(ask).await?;
    Ok(StepDone::done(answered.output.summary))
}
