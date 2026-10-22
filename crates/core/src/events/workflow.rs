//! Workflows and their steps, for the panel row, the workflow view, and track records per step
//! kind and standard phase.

use serde::{Deserialize, Serialize};

use crate::events::session::CliWorkflowRun;
use crate::names::{ApprovalId, ModelRef, SessionKey, WorkflowId};
use crate::workflow::{StandardPhase, StepKind, StepRef, Workflow, WorkflowRef};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkflowEvent {
    /// The run's own workflow, or a nested one that a `workflow` step started.
    Started(Box<WorkflowStarted>),
    StepStarted(StepStarted),
    StepFinished(StepFinished),
    /// An approved change request replaced the workflow from its next phase on.
    Changed(Box<WorkflowChanged>),
    /// What the workflow says it is: its name, description, and phases. Only declared phases
    /// exist in the views, so a template's phase that never runs is never drawn. Recorded
    /// when a workflow starts, and again after a change.
    Declared(Box<WorkflowDeclared>),
    /// A declared phase's first step started.
    PhaseStarted(PhaseMark),
    /// A declared phase's last step ended.
    PhaseEnded {
        #[serde(flatten)]
        phase: PhaseMark,
        end: StepEnd,
    },
    /// The main agent added a phase that hadn't started, at a phase end or between tries.
    PhaseAdded {
        id: WorkflowId,
        phase: DeclaredPhase,
        why: String,
    },
    /// A declared phase that won't run. It stays in the view, dim, with its reason.
    PhaseSkipped {
        #[serde(flatten)]
        phase: PhaseMark,
        why: String,
    },
    /// One line on why the plan moved, by the main agent or by code, such as "review:median found
    /// median([]) returns None, so a second round".
    Note(RunNote),
    /// A job nothing could take, which its workflow goes on without, as a council goes on
    /// with the proposals it has. Its row is drawn grey and isn't counted.
    LeftOut(AgentLeftOut),
    /// What the runner says a workflow is doing now: its name, its phases in the order it
    /// declares them, and its agents. It is the shape a CLI reports its own workflow in, and
    /// each report holds the whole state, replacing the last one of the same id.
    Reported(Box<WorkflowReport>),
}

/// A running workflow as the runner reports it, and the session behind each of its agents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowReport {
    /// The workflow step that runs it.
    pub step: StepRef,
    pub workflow: CliWorkflowRun,
    /// The session behind each agent that has one yet, in the agents' order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<AgentSession>,
    /// The size the request asked for, which the orchestrator keeps to, as the workflow's row
    /// says it: "a small workflow, 5 agents at most".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_asked: Option<String>,
}

/// The session that ran one agent of a reported workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// The agent's `index` in the report.
    pub agent: u32,
    pub session: SessionKey,
}

impl WorkflowReport {
    /// The session that ran the agent of this index, once it has one.
    pub fn session_of(&self, agent: u32) -> Option<&SessionKey> {
        self.sessions
            .iter()
            .find(|ran| ran.agent == agent)
            .map(|ran| &ran.session)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDeclared {
    pub id: WorkflowId,
    pub name: String,
    pub description: String,
    pub phases: Vec<DeclaredPhase>,
    /// The names of the steps you require, of every workflow or of this request, so the
    /// conversation and the view can mark them as yours where a phase has one of them as its name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
}

/// A phase as the workflow declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclaredPhase {
    /// From 0, its place in the workflow.
    pub index: u32,
    pub name: String,
    /// One line on who does what.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub standard: StandardPhase,
}

/// A declared phase, by place and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseMark {
    pub id: WorkflowId,
    /// From 0.
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLeftOut {
    /// The step whose agent it was.
    pub step: StepRef,
    /// The agent's label, as its row names it: `propose:p3`.
    pub label: String,
    /// The model the job was meant for.
    pub meant_for: ModelRef,
    /// What its CLI said, in a few words for its row: `signed out`.
    pub why: String,
    /// The same, whole, for its agent view: `agy is signed out`. Empty in a run
    /// recorded before it was kept.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub said: String,
    /// What the job was in its phase, for its agent view: `proposer`, `reviewer`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub acts_as: String,
    /// What going on without it means, one line each, for its agent view: `a council can do
    /// without one proposal, so it goes on with two`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub means: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunNote {
    pub text: String,
    /// The workflow it's about, when it's about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowId>,
    /// It says what the run leaves undone, so the run's closing answer lists it too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left: bool,
}

impl WorkflowDeclared {
    /// The declaration of a workflow as it's defined.
    pub fn of(id: &WorkflowId, workflow: &Workflow) -> Self {
        Self {
            id: id.clone(),
            name: workflow.name.clone(),
            description: workflow.description.clone(),
            phases: workflow
                .phases
                .iter()
                .enumerate()
                .map(|(index, phase)| DeclaredPhase {
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    name: phase.name.clone(),
                    detail: phase.detail.clone(),
                    standard: phase.standard,
                })
                .collect(),
            required: Vec::new(),
        }
    }

    /// Whether you require the step `phase` is, whatever case its name is written in.
    pub fn requires(&self, phase: &DeclaredPhase) -> bool {
        self.requires_step(&phase.name)
    }

    /// Whether a step of this name is one you require, in a phase of this workflow or of the
    /// workflow one of its steps runs.
    pub fn requires_step(&self, name: &str) -> bool {
        let name = name.trim();
        self.required
            .iter()
            .any(|required| required.trim().eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStarted {
    pub id: WorkflowId,
    /// The template it came from, or `None` when the main agent composed it.
    pub from: Option<WorkflowRef>,
    /// SHA-256 of the definition's canonical JSON, so every record names exactly what ran.
    pub version: String,
    /// The `workflow` step that started it, for a nested one.
    pub parent: Option<StepRef>,
    /// As validated, with its parameters filled in.
    pub workflow: Workflow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepStarted {
    pub step: StepRef,
    pub kind: StepKind,
    pub standard: StandardPhase,
    /// 1 the first time. Loops and `for_each` count up.
    pub round: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepFinished {
    pub step: StepRef,
    pub kind: StepKind,
    pub standard: StandardPhase,
    pub round: u32,
    pub end: StepEnd,
    /// One line, such as "2 reviewers agreed".
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepEnd {
    Done,
    Failed,
    /// Its `when` condition didn't hold.
    Skipped,
    /// The run was stopped, or the step was replaced by a workflow change.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowChanged {
    pub id: WorkflowId,
    pub version: String,
    pub workflow: Workflow,
    /// The `change_request` approval that approved it. `None` for a change that needs no
    /// approval: the main agent's design moving before anything writes, or a goal's next try.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalId>,
    pub reason: String,
}
