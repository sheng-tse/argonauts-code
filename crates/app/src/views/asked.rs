//! A step of Claude Code's own workflow that the crew answered through Argonauts Code: the agent
//! that reviews calls the tool, and Argonauts Code runs the reviewer's model on the change. In the
//! mirrored workflow that step's row is the model that answered, with its tokens and time and,
//! once it answered, what it decided, and it opens that model's session.

use argonauts_core::names::RoleName;
use argonauts_core::views::{RowNote, Tone, WorkflowRow, WorkflowView};

use super::sessions::{RunFacts, SessionFacts};

/// Each agent row of a CLI's own workflow whose label a role asked through Argonauts Code carries,
/// drawn as that role's session. The latest such session wins, as a retried step's would.
pub(super) fn answered_by_the_crew(view: &mut WorkflowView, facts: &RunFacts) {
    for phase in &mut view.phases {
        for row in &mut phase.rows {
            let WorkflowRow::Agent(agent) = row else {
                continue;
            };
            let asked = facts
                .sessions
                .iter()
                .rev()
                .find(|session| asked_for(session, &agent.label));
            if let Some(asked) = asked {
                let mut answered = facts.agent_row(asked);
                answered.label = agent.label.clone();
                answered.answered = true;
                // A role that ended without an answer says so, where its verdict would be.
                let silent = asked
                    .ended
                    .as_ref()
                    .is_some_and(|end| end.structured.is_none());
                if silent && answered.note.is_none() {
                    answered.note = Some(RowNote::new("no answer", Tone::Plain));
                }
                *agent = answered;
            }
        }
    }
}

/// A review or a sign-off asked through the tool: a reviewer's or judge's session tied to no
/// task of its own, under the label the asking agent gave.
fn asked_for(session: &SessionFacts, label: &str) -> bool {
    matches!(session.started.role, RoleName::Reviewer | RoleName::Judge)
        && session.task.is_none()
        && session.started.label.as_deref() == Some(label)
}

#[cfg(test)]
mod tests {
    use argonauts_core::TaskId;
    use argonauts_core::event::AgentEvent;
    use argonauts_core::events::session::EndReason;
    use argonauts_core::events::session::{
        CliAgent, CliAgentState, CliPhase, CliWorkflowRun, SubagentStatus,
    };
    use argonauts_core::events::{RunEvent, RunEventKind, SessionEvent};
    use argonauts_core::names::SessionKey;
    use serde_json::json;

    use super::*;
    use crate::client::test_support::{event, run_id, session_ended, session_started};
    use crate::views::workflow_view;

    fn agent(index: u32, label: &str) -> CliAgent {
        CliAgent {
            index,
            label: label.to_owned(),
            phase: Some(1),
            model: Some("claude-opus-5-5".to_owned()),
            state: CliAgentState::Running,
            tokens: 900,
            tool_calls: 1,
            duration_ms: 1_000,
            started_at_ms: None,
            try_number: 1,
            prompt: None,
            result: None,
            ..CliAgent::default()
        }
    }

    fn labelled(mut started: RunEvent, label: &str) -> RunEvent {
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut started.kind {
            started.label = Some(label.to_owned());
        }
        started
    }

    /// Claude Code's workflow at its Review phase, run by session 1 for its task, and the
    /// events of a run where Astra was asked about `review:store` as session 2.
    fn asked_about_store() -> (SessionKey, SessionKey, Vec<RunEvent>) {
        let host = SessionKey::numbered(1);
        let astra = SessionKey::numbered(2);
        let task = TaskId::new("task-add-due-dates").unwrap();
        let workflow = CliWorkflowRun {
            id: "w1".to_owned(),
            call: None,
            name: "add-due-dates".to_owned(),
            description: String::new(),
            phases: vec![CliPhase {
                index: 1,
                title: "Review".to_owned(),
                detail: String::new(),
            }],
            agents: vec![agent(1, "review:store"), agent(2, "review:cli")],
            status: SubagentStatus::Running,
            tokens: 1_800,
            tool_calls: 2,
            duration_ms: 2_000,
            summary: None,
            started_at_ms: None,
        };
        let mut reported = event(
            1,
            RunEventKind::Agent(AgentEvent::Workflow {
                workflow: Box::new(workflow),
            }),
        );
        reported.session = Some(host.clone());
        let events = vec![
            session_started(0, &host, Some(&task), RoleName::Worker, "claude-code"),
            reported,
            labelled(
                session_started(2, &astra, None, RoleName::Reviewer, "astra"),
                "review:store",
            ),
        ];
        (host, astra, events)
    }

    /// Each agent row's label, worker, session, and what it decided once it answered.
    fn rows(events: &[RunEvent]) -> Vec<(String, String, SessionKey, Option<String>)> {
        let id = argonauts_core::views::cli_workflow_run_id("w1");
        let view = workflow_view(events, &run_id(), Some(&id)).unwrap();
        view.phases[0]
            .rows
            .iter()
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some((
                    agent.label.clone(),
                    agent.worker.agent.clone(),
                    agent.session.clone(),
                    agent.note.as_ref().map(|note| note.text.clone()),
                )),
                _ => None,
            })
            .collect()
    }

    /// Claude Code's workflow reviews `store` by asking Astra through Argonauts Code, so
    /// that row is Astra's, with nothing decided yet; `cli`, which nobody asked about yet, stays
    /// Claude Code's.
    #[test]
    fn a_step_the_crew_answered_shows_who_answered() {
        let (host, astra, events) = asked_about_store();
        assert_eq!(
            rows(&events),
            [
                ("review:store".to_owned(), "astra".to_owned(), astra, None),
                (
                    "review:cli".to_owned(),
                    "claude-code".to_owned(),
                    host,
                    None
                ),
            ]
        );
    }

    /// Once Astra answers, its row says what it decided.
    #[test]
    fn an_answered_step_says_what_the_role_decided() {
        for (answer, said) in [
            (json!({"accept": true, "summary": "Fine"}), "approved"),
            (json!({"accept": false}), "sent it back"),
            (
                json!({"accept": false, "findings": ["--due with no date isn't rejected"]}),
                "sent it back · 1 finding",
            ),
        ] {
            let (_, astra, mut events) = asked_about_store();
            let mut ended = session_ended(3, &astra, EndReason::Completed, 58_000);
            if let RunEventKind::Session(SessionEvent::Ended(end)) = &mut ended.kind {
                end.structured = Some(answer);
            }
            events.push(ended);
            assert_eq!(rows(&events)[0].3.as_deref(), Some(said));
        }
    }
}
