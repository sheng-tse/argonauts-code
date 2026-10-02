//! Goal alignment before the lead's design, since the design works from the approved goal. The
//! preset's goal phase runs as a workflow of its own, nested under the first step of the run's
//! workflow. That workflow is recorded once the lead has picked it, so the run's own workflow
//! is the lead's, framed by code with the goal phase that already ran and a plan checkpoint.

use argonauts_core::events::checkpoint::{CheckpointKind, PlanWorkflow};
use argonauts_core::events::run::RunMode;
use argonauts_core::names::WorkflowId;
use argonauts_core::steps::StepName;
use argonauts_core::steps::design::DesignOutput;
use argonauts_core::workflow::{
    Phase, StandardPhase, Step, StepKind, StepRef, Workflow, WorkflowRef,
};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::state::RunState;
use crate::state::ids::IdKind;
use crate::workflow::{Flow, runner};

/// The alignment's name in the views, where it shows as a nested workflow of the goal phase.
pub const ALIGNMENT: &str = "goal";

/// Whether a new run aligns its goal before the lead designs: a crew run whose lead designs,
/// under a preset whose crew workflow opens with a goal phase. Otherwise the lead, if it
/// designs, works from your request alone.
pub fn applies(host: &Host) -> bool {
    let settings = host.settings();
    settings.workflows.lead_designs
        && host.read(runner::run_mode) != Some(RunMode::Solo)
        && goal_phase(host).is_some()
}

/// The preset's goal phase, when its crew workflow opens with one that only aligns the goal.
pub fn goal_phase(host: &Host) -> Option<Phase> {
    let workflow = runner::lookup(host, runner::crew_workflow(host).as_str()).ok()?;
    opening_goal_phase(&workflow).cloned()
}

fn opening_goal_phase(workflow: &Workflow) -> Option<&Phase> {
    let first = workflow.phases.first()?;
    let aligns =
        first.steps.iter().all(goal_only) && has_checkpoint(&first.steps, CheckpointKind::Goal);
    aligns.then_some(first)
}

/// Runs the alignment, or picks it up from the log, and returns the id the run's workflow will
/// take with how the alignment ended. `None` when the run doesn't align first.
pub async fn align(host: &Host) -> EngineResult<Option<(WorkflowId, Flow)>> {
    let (alignment, run) = match host.read(pending) {
        Some(found) => found,
        None => {
            if !applies(host) {
                return Ok(None);
            }
            let Some(phase) = goal_phase(host) else {
                return Ok(None);
            };
            // Taken first, so the run's own workflow keeps the first number.
            let run = WorkflowId::numbered(host.allocate(IdKind::Workflow)?);
            let workflow = Workflow {
                name: ALIGNMENT.to_owned(),
                description: "Restatements, their differences, and the goal checkpoint, before \
                              the lead picks the workflow"
                    .to_owned(),
                params: Default::default(),
                phases: vec![phase],
            };
            let parent = StepRef {
                workflow: run.clone(),
                phase: 0,
                path: vec![0],
            };
            let from = Some(runner::crew_workflow(host));
            (runner::start(host, from, workflow, Some(parent))?, run)
        }
    };
    let flow = runner::run(host, &alignment).await?;
    Ok(Some((run, flow)))
}

/// An alignment whose run workflow isn't recorded yet, with the id that workflow takes.
fn pending(state: &RunState) -> Option<(WorkflowId, WorkflowId)> {
    let workflows = &state.workflows;
    workflows.by_id.values().find_map(|record| {
        let parent = record.parent.as_ref()?;
        (!workflows.by_id.contains_key(&parent.workflow))
            .then(|| (record.id.clone(), parent.workflow.clone()))
    })
}

/// Whether the workflow's goal phase was aligned before the workflow was recorded, so its end
/// has nothing new for the lead, who designed it right after.
pub fn aligned_before(state: &RunState, id: &WorkflowId) -> bool {
    let workflows = &state.workflows;
    let Some(record) = workflows.by_id.get(id) else {
        return false;
    };
    workflows.by_id.values().any(|child| {
        child.since < record.since
            && child
                .parent
                .as_ref()
                .is_some_and(|parent| parent.workflow == *id && parent.phase == 0)
    })
}

/// The lead's workflow as the run follows it after aligning: the goal phase that ran first,
/// then the lead's phases less any goal alignment of their own, with a plan checkpoint added
/// when they have none, after the planning when there is some.
pub fn frame(goal: &Phase, mut workflow: Workflow) -> Workflow {
    for phase in &mut workflow.phases {
        phase.steps.retain(|step| !goal_only(step));
    }
    workflow.phases.retain(|phase| !phase.steps.is_empty());
    let has_plan = workflow
        .phases
        .iter()
        .any(|phase| has_checkpoint(&phase.steps, CheckpointKind::Plan));
    if !has_plan {
        let after = workflow
            .phases
            .iter()
            .rposition(|phase| has_kind(&phase.steps, StepKind::Draft))
            .map_or(0, |index| index + 1);
        workflow.phases.insert(after, plan_phase());
    }
    workflow.phases.insert(0, goal.clone());
    workflow
}

/// [`frame`] with the preset's goal phase, when the run aligns first.
pub fn frame_for_run(host: &Host, workflow: Workflow) -> Workflow {
    match goal_phase(host) {
        Some(goal) if applies(host) => frame(&goal, workflow),
        _ => workflow,
    }
}

fn plan_phase() -> Phase {
    Phase {
        name: "Plan".to_owned(),
        standard: StandardPhase::Plan,
        steps: vec![checkpoint(CheckpointKind::Plan)],
    }
}

fn checkpoint(kind: CheckpointKind) -> Step {
    Step {
        kind: StepKind::Checkpoint,
        name: None,
        role: None,
        roles: Vec::new(),
        workers: Default::default(),
        count: None,
        blind: false,
        different_family_from: None,
        when: None,
        task: None,
        input: None,
        steps: Vec::new(),
        until: None,
        max: None,
        checkpoint: Some(kind),
        workflow: None,
        params: Default::default(),
    }
}

/// A step that only aligns the goal: restating, comparing, the goal checkpoint, or a loop of
/// those.
fn goal_only(step: &Step) -> bool {
    match step.kind {
        StepKind::Restate | StepKind::Differ => true,
        StepKind::Checkpoint => step.checkpoint == Some(CheckpointKind::Goal),
        StepKind::LoopUntil => !step.steps.is_empty() && step.steps.iter().all(goal_only),
        _ => false,
    }
}

fn has_checkpoint(steps: &[Step], kind: CheckpointKind) -> bool {
    steps.iter().any(|step| {
        (step.kind == StepKind::Checkpoint && step.checkpoint == Some(kind))
            || has_checkpoint(&step.steps, kind)
    })
}

/// Whether any step, at any depth, is of `kind`.
pub fn has_kind(steps: &[Step], kind: StepKind) -> bool {
    steps
        .iter()
        .any(|step| step.kind == kind || has_kind(&step.steps, kind))
}

/// The run's workflow for the plan checkpoint, with the lead's reason when the lead picked it.
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
/// `None`: its one line for the plan checkpoint.
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
            (output.template.as_deref() == from.map(WorkflowRef::as_str) && !why.is_empty())
                .then(|| why.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{builtin, yaml};

    fn builtin(name: &str) -> Workflow {
        builtin::load(name).unwrap_or_else(|err| panic!("{err}"))
    }

    const REVIEW_PANEL: &str = r#"name: review-panel
description: Two reviewers from other families; an adjudicator only if they disagree
params: { target: diff, reviewers: 2 }
phases:
  - name: Review
    standard: check
    steps:
      - kind: fan_out
        role: reviewer
        count: "{{reviewers}}"
        blind: true
        different_family_from: author
  - name: Adjudicate
    standard: decide
    steps:
      - kind: judge
        role: adjudicator
        when: reviewers_disagree
"#;

    fn names(workflow: &Workflow) -> Vec<&str> {
        workflow
            .phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect()
    }

    #[test]
    fn a_review_gets_the_goal_phase_and_a_plan_checkpoint() {
        let peer = builtin("peer-crew");
        let goal = opening_goal_phase(&peer).expect("peer-crew opens with its goal phase");
        let review = yaml::parse(REVIEW_PANEL).unwrap();
        let framed = frame(goal, review.clone());
        assert_eq!(names(&framed), ["Goal", "Plan", "Review", "Adjudicate"]);
        assert_eq!(&framed.phases[0], goal);
        assert!(has_checkpoint(
            &framed.phases[1].steps,
            CheckpointKind::Plan
        ));
        assert_eq!(framed.phases[2..], review.phases[..]);
        assert_eq!(
            (framed.name.as_str(), &framed.params),
            (review.name.as_str(), &review.params)
        );
    }

    #[test]
    fn a_crew_workflow_frames_to_itself() {
        let peer = builtin("peer-crew");
        let goal = opening_goal_phase(&peer).unwrap();
        assert_eq!(frame(goal, peer.clone()), peer);
        let coordinated = builtin("coordinated-crew");
        assert_eq!(frame(goal, coordinated.clone()), coordinated);
        // Framing twice changes nothing, so a phase end's design compares like for like.
        let review = frame(goal, yaml::parse(REVIEW_PANEL).unwrap());
        assert_eq!(frame(goal, review.clone()), review);
    }

    /// Planning, then building, with no plan checkpoint of its own.
    const DRAFT_THEN_BUILD: &str = r#"name: draft-then-build
description: One draft, then the cards
phases:
  - name: Draft
    standard: plan
    steps:
      - kind: draft
        role: drafter
        input: goal
  - name: Build
    standard: do
    steps:
      - kind: build
        role: owner
        input: plan
  - name: Merge
    standard: decide
    steps:
      - kind: merge
"#;

    #[test]
    fn the_plan_checkpoint_follows_the_planning() {
        let peer = builtin("peer-crew");
        let goal = opening_goal_phase(&peer).unwrap();
        let framed = frame(goal, yaml::parse(DRAFT_THEN_BUILD).unwrap());
        assert_eq!(names(&framed), ["Goal", "Draft", "Plan", "Build", "Merge"]);
        assert!(has_checkpoint(
            &framed.phases[2].steps,
            CheckpointKind::Plan
        ));
    }

    #[test]
    fn only_a_phase_that_just_aligns_the_goal_runs_first() {
        assert!(opening_goal_phase(&builtin("peer-crew")).is_some());
        assert!(opening_goal_phase(&builtin("coordinated-crew")).is_some());
        assert!(opening_goal_phase(&yaml::parse(REVIEW_PANEL).unwrap()).is_none());
        assert!(opening_goal_phase(&yaml::parse(DRAFT_THEN_BUILD).unwrap()).is_none());
        let mut busy = builtin("peer-crew");
        let build = busy.phases[2].steps[0].clone();
        busy.phases[0].steps.push(build);
        assert!(opening_goal_phase(&busy).is_none());
    }
}
