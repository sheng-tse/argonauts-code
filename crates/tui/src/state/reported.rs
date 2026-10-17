//! The workflows the runner runs as steps of a run's own workflow, as each was last reported:
//! its own name, its phases in the order it declares them, and its agents, in the shape a CLI
//! reports its own workflow in. The run's row at the bottom reads the phase going on and the
//! agents still to come from here, since the run's own phases say only that the steps run.

use argonauts_core::events::session::{CliAgentState, CliWorkflowRun};
use argonauts_core::names::WorkflowId;
use argonauts_core::workflow::{StepRef, Workflow};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepWorkflows {
    /// Each with the step that runs it, in the order they were first reported.
    known: Vec<(StepRef, CliWorkflowRun)>,
    /// The workflows with a step that runs another's steps, which that one reports as it goes.
    hand_over: Vec<WorkflowId>,
}

/// Whether one of `workflow`'s steps runs the steps of another workflow, so its own phases
/// say only that they run.
pub fn hands_over(workflow: &Workflow) -> bool {
    argonauts_core::views::writes_its_phases(workflow)
}

impl StepWorkflows {
    /// Notes that a step of `workflow` runs another workflow, whose phases a report will name.
    pub fn expect_from(&mut self, workflow: &WorkflowId) {
        if !self.expected_from(workflow) {
            self.hand_over.push(workflow.clone());
        }
    }

    pub fn expected_from(&self, workflow: &WorkflowId) -> bool {
        self.hand_over.contains(workflow)
    }

    /// Whether a step of `workflow` runs another whose report hasn't named its phases or agents
    /// yet, so nothing of `workflow` draws until it does.
    pub fn awaits_shape(&self, workflow: &WorkflowId) -> bool {
        self.expected_from(workflow)
            && !self.known.iter().any(|(step, reported)| {
                step.workflow == *workflow
                    && (!reported.phases.is_empty() || !reported.agents.is_empty())
            })
    }

    /// Every workflow reported so far, as it last reported itself.
    pub fn all(&self) -> impl Iterator<Item = &CliWorkflowRun> {
        self.known.iter().map(|(_, workflow)| workflow)
    }

    /// Takes in a report, which is the whole state of its workflow and replaces the last one.
    pub fn report(&mut self, step: &StepRef, workflow: &CliWorkflowRun) {
        let report = (step.clone(), workflow.clone());
        match self
            .known
            .iter_mut()
            .find(|(_, known)| known.id == workflow.id)
        {
            Some(known) => *known = report,
            None => self.known.push(report),
        }
    }

    /// The workflow a step in phase `phase` of `workflow` runs.
    pub fn in_phase(&self, workflow: &WorkflowId, phase: u32) -> Option<&CliWorkflowRun> {
        self.known
            .iter()
            .find(|(step, _)| step.workflow == *workflow && step.phase == phase)
            .map(|(_, reported)| reported)
    }

    /// Whether one of their agents goes by `label`. Its sessions, an earlier try among them,
    /// count as that one agent.
    pub fn names(&self, label: &str) -> bool {
        let label = label.trim();
        !label.is_empty()
            && self
                .known
                .iter()
                .flat_map(|(_, workflow)| &workflow.agents)
                .any(|agent| agent.label.trim() == label)
    }

    /// Their agents done, and all of them, the ones still queued among them.
    pub fn agents(&self) -> (usize, usize) {
        let agents = || self.known.iter().flat_map(|(_, workflow)| &workflow.agents);
        let done = agents()
            .filter(|agent| agent.state == CliAgentState::Done)
            .count();
        (done, agents().count())
    }
}

/// The phase a reported workflow is in: its newest agent at work says, else the last one
/// that has started. `None` before any has, or when its agents are in no phase it declares.
pub fn current_phase(workflow: &CliWorkflowRun) -> Option<&str> {
    let started = |state: CliAgentState| state != CliAgentState::Queued;
    let agents = || workflow.agents.iter().rev();
    let agent = agents()
        .find(|agent| matches!(agent.state, CliAgentState::Running | CliAgentState::Other))
        .or_else(|| agents().find(|agent| started(agent.state)))?;
    let index = agent.phase?;
    workflow
        .phases
        .iter()
        .find(|phase| phase.index == index)
        .map(|phase| phase.title.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow(kinds: serde_json::Value) -> Workflow {
        serde_json::from_value(serde_json::json!({
            "name": "any",
            "description": "",
            "phases": [{ "name": "Work", "standard": "do", "steps": kinds }],
        }))
        .unwrap()
    }

    /// Only a step that runs another workflow's steps, at any depth, hands the phases over.
    #[test]
    fn a_workflow_hands_over_where_a_step_runs_another_workflows_steps() {
        assert!(hands_over(&workflow(
            serde_json::json!([{ "kind": "script" }])
        )));
        assert!(hands_over(&workflow(serde_json::json!([{
            "kind": "loop_until",
            "until": "it holds",
            "max": 2,
            "steps": [{ "kind": "script" }],
        }]))));
        assert!(!hands_over(&workflow(
            serde_json::json!([{ "kind": "build" }])
        )));

        let mut known = StepWorkflows::default();
        let id = WorkflowId::numbered(1);
        known.expect_from(&id);
        known.expect_from(&id);
        assert!(known.expected_from(&id));
        assert!(!known.expected_from(&WorkflowId::numbered(2)));
    }
}
