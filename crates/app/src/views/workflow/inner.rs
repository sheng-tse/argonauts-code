//! A workflow an agent's own CLI runs, such as one Claude Code's Workflow tool starts, is part
//! of that agent's step. It sits under the agent's row, never beside the run as a second one,
//! and opening it shows its own phases.

use argonauts_core::events::session::{CliAgentState, CliWorkflowRun, SubagentStatus};
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::views::{NestedWorkflowRow, RowStatus, WorkflowRow, cli_workflow_run_id};

/// The row of each workflow `parent`'s CLI ran, oldest first.
pub(super) fn rows(
    parent: &SessionKey,
    worker: &ModelRef,
    agent_ids: &[(SessionKey, CliWorkflowRun)],
    now_ms: Option<u64>,
) -> Vec<WorkflowRow> {
    agent_ids
        .iter()
        .filter(|(host, _)| host == parent)
        .map(|(_, workflow)| {
            let done = workflow
                .agents
                .iter()
                .filter(|agent| agent.state == CliAgentState::Done)
                .count();
            let name = Some(workflow.name.trim())
                .filter(|name| !name.is_empty())
                .unwrap_or("workflow");
            WorkflowRow::Workflow(NestedWorkflowRow {
                id: cli_workflow_run_id(&workflow.id),
                name: name.to_owned(),
                status: status(workflow),
                agents_done: u32::try_from(done).unwrap_or(u32::MAX),
                agents_total: u32::try_from(workflow.agents.len()).unwrap_or(u32::MAX),
                workers: vec![worker.clone()],
                elapsed_ms: workflow.elapsed_ms(now_ms),
                parent: Some(parent.clone()),
                worker: Some(worker.agent.clone()),
            })
        })
        .collect()
}

fn status(workflow: &CliWorkflowRun) -> RowStatus {
    match workflow.status {
        SubagentStatus::Done => RowStatus::Done,
        SubagentStatus::Failed => RowStatus::Failed,
        SubagentStatus::Started | SubagentStatus::Running | SubagentStatus::Unknown => {
            if workflow.ended() {
                RowStatus::Done
            } else {
                RowStatus::Running
            }
        }
    }
}

/// `rows` with each workflow an agent's CLI ran just after that agent's row, an earlier try's
/// included, so it reads as part of the agent's step.
pub(super) fn under_agents(
    rows: Vec<WorkflowRow>,
    agent_ids: &[(SessionKey, CliWorkflowRun)],
    now_ms: Option<u64>,
) -> Vec<WorkflowRow> {
    if agent_ids.is_empty() {
        return rows;
    }
    let mut placed = Vec::with_capacity(rows.len());
    for row in rows {
        let WorkflowRow::Agent(agent) = &row else {
            placed.push(row);
            continue;
        };
        let mut inner = Vec::new();
        for tried in &agent.tries {
            inner.extend(self::rows(&tried.session, &tried.worker, agent_ids, now_ms));
        }
        inner.extend(self::rows(&agent.session, &agent.worker, agent_ids, now_ms));
        placed.push(row);
        placed.extend(inner);
    }
    placed
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::views::AgentRow;

    use super::*;

    fn agent(session: &str) -> WorkflowRow {
        WorkflowRow::Agent(AgentRow {
            session: SessionKey::parse(session).unwrap(),
            label: format!("build:{session}"),
            worker: "claude-code/claude-haiku-4-5".parse().unwrap(),
            status: RowStatus::Running,
            tokens: 0,
            spent: Spend::default(),
            elapsed_ms: 0,
            subagents: 0,
            context: None,
            fallback_for: None,
            answered: false,
            tries: Vec::new(),
            note: None,
            inner: None,
        })
    }

    fn claude_codes_own(id: &str, name: &str) -> CliWorkflowRun {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": name,
            "description": "",
            "phases": [],
            "agents": [],
            "status": "running",
            "tokens": 0,
            "tool_calls": 0,
            "duration_ms": 0,
        }))
        .unwrap()
    }

    #[test]
    fn a_workflow_an_agents_cli_runs_sits_under_that_agent() {
        let rows = vec![agent("s-001"), agent("s-002")];
        let agent_ids = vec![(
            SessionKey::parse("s-001").unwrap(),
            claude_codes_own("wf_1", "add-due-dates"),
        )];
        let rows = under_agents(rows, &agent_ids, None);
        let shape: Vec<String> = rows
            .iter()
            .map(|row| match row {
                WorkflowRow::Agent(agent) => agent.session.to_string(),
                WorkflowRow::Workflow(nested) => format!(
                    "{} under {} · {}",
                    nested.name,
                    nested.parent.as_ref().unwrap(),
                    nested.worker.as_deref().unwrap()
                ),
                WorkflowRow::Command(_) => String::new(),
            })
            .collect();
        assert_eq!(
            shape,
            ["s-001", "add-due-dates under s-001 · claude-code", "s-002"]
        );
    }
}
