//! A workflow the conversation's own CLI runs in a conversation turn, mirrored as a run's are: a
//! panel row, and the workflow view with whatever phases and agents it reports. The views name the
//! conversation by [`conversation_run`], so a row and a refresh find it.

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::events::session::CliWorkflowRun;
use argonauts_core::names::WorkflowId;
use argonauts_core::views::{
    AgentRow, AgentView, ContextRow, InnerAgent, RowStatus, WorkflowView, cli_workflow_run_id,
    conversation_run,
};
use time::OffsetDateTime;

use super::ConversationState;
use crate::controller::Ctx;

/// The newest report of each workflow, its time counted from when it was first seen here.
pub(super) fn seen(workflows: &mut Vec<CliWorkflowRun>, report: &CliWorkflowRun) {
    let mut report = report.clone();
    let now = now_ms();
    match workflows.iter_mut().find(|known| known.id == report.id) {
        Some(known) => {
            report.started_at_ms = known.started_at_ms.or(report.started_at_ms).or(now);
            *known = report;
        }
        None => {
            report.started_at_ms = report.started_at_ms.or(now);
            workflows.push(report);
        }
    }
}

/// The view of `id`, or of the newest workflow when `None`.
pub(crate) fn show(ctx: &Ctx, conversation: &ConversationState, id: Option<&WorkflowId>) {
    let workflow = match id {
        Some(id) => known(&conversation.workflows, id),
        None => conversation.workflows.last(),
    };
    let (Some(workflow), Some(context)) = (workflow, conversation.context.as_ref()) else {
        // Only a key asks for this view, so the answer goes on the notice line.
        ctx.hint("This conversation runs no workflow now. Ctrl+T shows one once it starts.");
        return;
    };
    let host = host_row(context, workflow);
    let mut view = WorkflowView::of_agent(&conversation_run(), &host, workflow, now_ms());
    crate::views::required::mark(
        &mut view,
        &ctx.env,
        super::talk::conversation_steps(conversation),
    );
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Workflow {
            view: Box::new(view),
        }),
    });
}

/// One agent of a workflow the conversation's CLI runs, as the CLI last reported it.
pub(crate) fn show_agent(ctx: &Ctx, conversation: &ConversationState, agent: &InnerAgent) {
    let view = conversation
        .context
        .as_ref()
        .and_then(|context| agent_view(&conversation.workflows, context, agent, now_ms()));
    match view {
        Some(view) => ctx.send(AppEvent::View {
            data: Box::new(ViewData::Agent {
                view: Box::new(view),
            }),
        }),
        None => ctx.hint("This conversation's workflow no longer reports that agent."),
    }
}

/// `agent` of one of `workflows`, which the session of `context` runs. `None` when no
/// workflow here goes by its id, or that workflow has no such agent.
fn agent_view(
    workflows: &[CliWorkflowRun],
    context: &ContextRow,
    agent: &InnerAgent,
    now_ms: Option<u64>,
) -> Option<AgentView> {
    let workflow = known(workflows, &agent.workflow)?;
    let host = host_row(context, workflow);
    AgentView::of_inner(&conversation_run(), &host, workflow, agent.index, now_ms)
}

fn known<'a>(workflows: &'a [CliWorkflowRun], id: &WorkflowId) -> Option<&'a CliWorkflowRun> {
    workflows
        .iter()
        .find(|workflow| cli_workflow_run_id(&workflow.id) == *id)
}

/// The row of the conversation's session, which runs `workflow`.
fn host_row(context: &ContextRow, workflow: &CliWorkflowRun) -> AgentRow {
    AgentRow {
        session: context.session.clone(),
        label: context.label.clone(),
        worker: context.worker.clone(),
        status: if workflow.ended() {
            RowStatus::Done
        } else {
            RowStatus::Running
        },
        tokens: context.used,
        spent: context.spent,
        elapsed_ms: 0,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note: None,
        inner: None,
        left_out: false,
        not_needed: false,
        fallback_why: None,
    }
}

fn now_ms() -> Option<u64> {
    u64::try_from((OffsetDateTime::now_utc() - OffsetDateTime::UNIX_EPOCH).whole_milliseconds())
        .ok()
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::events::session::{CliAgent, CliAgentState, SubagentStatus};
    use argonauts_core::names::SessionKey;

    use super::*;

    fn agent(index: u32, label: &str, prompt: &str) -> CliAgent {
        CliAgent {
            index,
            label: label.to_owned(),
            phase: None,
            model: Some("claude-haiku-4-5".to_owned()),
            state: CliAgentState::Running,
            tokens: 900,
            tool_calls: 3,
            duration_ms: 1_000,
            started_at_ms: None,
            try_number: 1,
            prompt: Some(prompt.to_owned()),
            result: None,
            ..CliAgent::default()
        }
    }

    fn workflow(id: &str, agents: Vec<CliAgent>) -> CliWorkflowRun {
        CliWorkflowRun {
            id: id.to_owned(),
            call: None,
            name: "add-due-dates".to_owned(),
            description: String::new(),
            phases: Vec::new(),
            agents,
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        }
    }

    fn context() -> ContextRow {
        ContextRow {
            run: None,
            session: SessionKey::numbered(1),
            label: "main".to_owned(),
            worker: "claude-code/claude-opus-5-5".parse().unwrap(),
            used: 40_000,
            window: None,
            cap: None,
            tool_calls: 0,
            spent: Spend::default(),
            rotations: 0,
            estimated: false,
            counts: true,
            categories: Vec::new(),
            recorded: None,
            ended: None,
            answered_for: None,
        }
    }

    /// Each agent of a workflow the conversation's CLI runs is asked for by its workflow and
    /// its number there, and opens with what the CLI says of it.
    #[test]
    fn an_agent_of_the_conversations_workflow_opens_as_itself() {
        let workflows = [
            workflow("wf_first", vec![agent(1, "probe", "Find the parser.")]),
            workflow(
                "wf_second",
                vec![
                    agent(1, "build:flag", "Add --due."),
                    agent(2, "build:tests", "Cover --due with tests."),
                ],
            ),
        ];
        let context = context();
        let asked = InnerAgent {
            workflow: cli_workflow_run_id("wf_second"),
            index: 2,
        };
        let opened = agent_view(&workflows, &context, &asked, None).expect("its view");
        assert_eq!(opened.run, conversation_run());
        assert_eq!(opened.row.label, "build:tests");
        assert_eq!(opened.row.session, context.session);
        assert_eq!(opened.row.inner.as_ref(), Some(&asked));
        assert_eq!(opened.prompt, "Cover --due with tests.");
        assert_eq!(opened.tool_calls, 3);

        // The first workflow's only agent has the same number as one of the second's.
        let first = InnerAgent {
            workflow: cli_workflow_run_id("wf_first"),
            index: 1,
        };
        let opened = agent_view(&workflows, &context, &first, None).expect("its view");
        assert_eq!(opened.row.label, "probe");

        let gone = InnerAgent {
            index: 2,
            ..first.clone()
        };
        assert!(agent_view(&workflows, &context, &gone, None).is_none());
    }
}
