//! A step of Claude Code's own workflow that the crew answered through Argonauts: the agent
//! that reviews calls the tool, and Argonauts runs the reviewer's model on the change. In the mirrored workflow that step's row shows the model that answered, says it was
//! asked through Argonauts and then what it decided, with its tokens and time, and opens its
//! session.

use argonauts_core::names::RoleName;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::views::{WorkflowRow, WorkflowView};
use argonauts_crew::claude_codes::review_of;

use super::sessions::{RunFacts, SessionFacts};

/// Each agent row of a CLI's own workflow whose label a role asked through Argonauts carries,
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
                answered.answered = Some(words(asked).to_owned());
                *agent = answered;
            }
        }
    }
}

/// `asked through Argonauts` while the role reads the change, then what it decided.
fn words(session: &SessionFacts) -> &'static str {
    let Some(end) = &session.ended else {
        return "asked through Argonauts";
    };
    match end.structured.as_ref().map(review_of) {
        Some(review) if review.verdict == ReviewVerdict::Approve => "accepted",
        Some(_) => "asked for changes",
        None => "no answer",
    }
}

/// A review or a sign-off asked through the tool: a reviewer's or judge's session tied to no
/// task of its own, under the label the asking agent gave.
fn asked_for(session: &SessionFacts, label: &str) -> bool {
    matches!(session.started.role, RoleName::Reviewer | RoleName::Judge)
        && session.card.is_none()
        && session.started.label.as_deref() == Some(label)
}

#[cfg(test)]
mod tests {
    use argonauts_core::CardId;
    use argonauts_core::event::AgentEvent;
    use argonauts_core::events::session::EndReason;
    use argonauts_core::events::session::{
        SubagentStatus, VendorAgent, VendorAgentState, VendorPhase, VendorWorkflow,
    };
    use argonauts_core::events::{RunEvent, RunEventKind, SessionEvent};
    use argonauts_core::names::SessionKey;
    use serde_json::json;

    use super::*;
    use crate::client::test_support::{event, run_id, session_ended, session_started};
    use crate::views::workflow_view;

    fn agent(index: u32, label: &str) -> VendorAgent {
        VendorAgent {
            index,
            label: label.to_owned(),
            phase: Some(1),
            model: Some("claude-opus-5-5".to_owned()),
            state: VendorAgentState::Running,
            tokens: 900,
            tool_calls: 1,
            duration_ms: 1_000,
            started_at_ms: None,
            attempt: 1,
            prompt: None,
            result: None,
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
        let task = CardId::new("task-add-due-dates").unwrap();
        let workflow = VendorWorkflow {
            id: "w1".to_owned(),
            call: None,
            name: "add-due-dates".to_owned(),
            description: String::new(),
            phases: vec![VendorPhase {
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

    /// Each agent row's label, worker, session, and the words after its model.
    fn rows(events: &[RunEvent]) -> Vec<(String, String, SessionKey, Option<String>)> {
        let id = argonauts_core::views::vendor_workflow_id("w1");
        let view = workflow_view(events, &run_id(), Some(&id)).unwrap();
        view.phases[0]
            .rows
            .iter()
            .filter_map(|row| match row {
                WorkflowRow::Agent(agent) => Some((
                    agent.label.clone(),
                    agent.worker.worker.clone(),
                    agent.session.clone(),
                    agent.answered.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    /// Claude Code's workflow reviews `store` by asking Astra through Argonauts, so
    /// that row is Astra's and says how it was asked; `cli`, which nobody asked about yet, stays
    /// Claude Code's.
    #[test]
    fn a_step_the_crew_answered_shows_who_answered() {
        let (host, astra, events) = asked_about_store();
        assert_eq!(
            rows(&events),
            [
                (
                    "review:store".to_owned(),
                    "astra".to_owned(),
                    astra,
                    Some("asked through Argonauts".to_owned())
                ),
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
            (json!({"accept": true, "summary": "Fine"}), "accepted"),
            (json!({"accept": false}), "asked for changes"),
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
