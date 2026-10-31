//! What a running workflow reports, in the shape Claude Code reports its own: a name, phases in
//! the order declared, and each agent with its label, phase, model, state and tokens. The row
//! at the bottom and the workflow view draw from this alone, whatever the steps are.

use argonauts_core::runs::events::session::{
    CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentStatus,
};

use crate::script::Meta;

/// As much of a prompt or an answer as a row needs.
const SHOWN: usize = 400;

/// The most phases a workflow shows.
const PHASES: usize = 200;

#[derive(Debug, Clone)]
pub(crate) struct Progress {
    workflow: CliWorkflowRun,
}

impl Progress {
    pub fn new(id: &str, meta: &Meta, now_ms: u64) -> Self {
        let phases = meta
            .phases
            .iter()
            .enumerate()
            .map(|(index, phase)| CliPhase {
                index: number(index),
                title: phase.name.clone(),
                detail: phase.detail.clone(),
            })
            .collect();
        Self {
            workflow: CliWorkflowRun {
                id: id.to_owned(),
                call: None,
                name: meta.name.clone(),
                description: meta.description.clone(),
                phases,
                agents: Vec::new(),
                status: SubagentStatus::Running,
                tokens: 0,
                tool_calls: 0,
                duration_ms: 0,
                summary: None,
                started_at_ms: Some(now_ms),
            },
        }
    }

    pub fn workflow(&self) -> &CliWorkflowRun {
        &self.workflow
    }

    /// The phase's number, added at the end when the script didn't declare it. Past
    /// [`PHASES`] a new name joins the last phase, so a script can't grow the list forever.
    pub fn phase(&mut self, name: &str) -> u32 {
        if let Some(phase) = self
            .workflow
            .phases
            .iter()
            .find(|phase| phase.title == name)
        {
            return phase.index;
        }
        if self.workflow.phases.len() >= PHASES {
            return number(PHASES - 1);
        }
        let index = number(self.workflow.phases.len());
        self.workflow.phases.push(CliPhase {
            index,
            title: name.to_owned(),
            detail: String::new(),
        });
        index
    }

    /// A new agent, waiting for its turn. Returns its place in `agents`.
    pub fn queue(
        &mut self,
        label: &str,
        phase: Option<&str>,
        try_number: u32,
        prompt: &str,
    ) -> usize {
        let phase = phase.map(|name| self.phase(name));
        self.workflow.agents.push(CliAgent {
            index: number(self.workflow.agents.len()),
            label: label.to_owned(),
            phase,
            model: None,
            state: CliAgentState::Queued,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            started_at_ms: None,
            try_number,
            prompt: Some(shortened(prompt)),
            result: None,
            ..CliAgent::default()
        });
        self.workflow.agents.len() - 1
    }

    pub fn started(&mut self, agent: usize, model: Option<String>, now_ms: u64) {
        let agent = &mut self.workflow.agents[agent];
        agent.state = CliAgentState::Running;
        agent.model = model;
        agent.started_at_ms = Some(now_ms);
    }

    /// The agent's answer, or why it gave none, which its row keeps so the view can say it.
    pub fn ended(&mut self, agent: usize, end: &AgentTotals, answer: Result<&str, &str>) {
        let row = &mut self.workflow.agents[agent];
        let (result, error) = match answer {
            Ok(text) => (Some(text), None),
            Err(why) => (None, Some(why.trim()).filter(|why| !why.is_empty())),
        };
        row.state = if result.is_some() {
            CliAgentState::Done
        } else {
            CliAgentState::Failed
        };
        row.error = error.map(str::to_owned);
        if end.model.is_some() {
            row.model.clone_from(&end.model);
        }
        row.tokens = end.tokens;
        row.tool_calls = end.tool_calls;
        row.duration_ms = end.duration_ms;
        row.result = result.map(shortened);
        self.workflow.tokens += end.tokens;
        self.workflow.tool_calls += end.tool_calls;
    }

    pub fn say(&mut self, summary: &str) {
        self.workflow.summary = Some(shortened(summary));
    }

    pub fn clock(&mut self, now_ms: u64) {
        let started = self.workflow.started_at_ms.unwrap_or(now_ms);
        self.workflow.duration_ms = now_ms.saturating_sub(started);
    }

    /// The workflow is over. Agents that never got to end count as failed.
    pub fn finish(&mut self, done: bool, summary: Option<&str>) {
        self.workflow.status = if done {
            SubagentStatus::Done
        } else {
            SubagentStatus::Failed
        };
        if let Some(summary) = summary {
            self.say(summary);
        }
        for agent in &mut self.workflow.agents {
            if matches!(agent.state, CliAgentState::Queued | CliAgentState::Running) {
                agent.state = CliAgentState::Failed;
            }
        }
    }
}

/// What an agent used, for its row and the workflow's totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentTotals {
    pub model: Option<String>,
    pub tokens: u64,
    pub tool_calls: u32,
    pub duration_ms: u64,
}

/// Rows count from 1, as Claude Code numbers them.
fn number(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

fn shortened(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= SHOWN {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(SHOWN - 1).collect();
    short.push('…');
    short
}
