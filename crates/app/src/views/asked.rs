//! A step of Claude Code's own workflow that the crew answered through Argonauts Code: the agent
//! that reviews calls the tool, and Argonauts Code runs the reviewer's model on the change. In the
//! mirrored workflow that step's row is the model that answered, with its tokens and time and,
//! once it answered, what it decided, and it opens that model's session.

use std::collections::BTreeMap;

use argonauts_core::events::session::{CliAgent, CliWorkflowRun};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::views::{PhaseRow, RowNote, RowStatus, Tone, WorkflowRow, WorkflowView};
use argonauts_tui::views::asks::{self, Ask, label_stem};

use super::sessions::{RunFacts, SessionFacts};

/// Each agent row of `host`'s CLI workflow that a role answered through Argonauts Code, drawn
/// as that role's session.
pub(super) fn answered_by_the_crew(
    view: &mut WorkflowView,
    facts: &RunFacts,
    host: &SessionKey,
    workflow: &CliWorkflowRun,
) {
    answered_in(&mut view.phases, facts, host, workflow);
}

/// As [`answered_by_the_crew`], for the phases of a workflow inside another.
pub(super) fn answered_in(
    phases: &mut [PhaseRow],
    facts: &RunFacts,
    host: &SessionKey,
    workflow: &CliWorkflowRun,
) {
    let joined = joins(facts, host, workflow);
    for phase in phases {
        for row in &mut phase.rows {
            let WorkflowRow::Agent(agent) = row else {
                continue;
            };
            let Some(index) = agent.inner.as_ref().map(|inner| inner.index) else {
                continue;
            };
            let Some(asked) = joined
                .get(&index)
                .and_then(|session| facts.session(session))
            else {
                continue;
            };
            let mut answered = facts.agent_row(asked);
            answered.label = label_stem(&agent.label).to_owned();
            answered.answered = true;
            // The agent that asked passes the answer back, and is at work until it has.
            let asking = matches!(agent.status, RowStatus::Running | RowStatus::Queued);
            let passing_on = asking && asked.ended.is_some();
            if let Some(note) = facts.verdicts.handed_back(asked, passing_on) {
                answered.note = Some(note);
            }
            if asking {
                answered.status = RowStatus::Running;
            }
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

/// The agent of a CLI workflow that asked for `session`, when one did, with the workflow and
/// the session running it.
pub(super) fn asker_of<'a>(
    facts: &RunFacts,
    workflows: &'a [(SessionKey, CliWorkflowRun)],
    session: &SessionKey,
) -> Option<(&'a SessionKey, &'a CliAgent)> {
    workflows.iter().find_map(|(host, workflow)| {
        let (index, _) = joins(facts, host, workflow)
            .into_iter()
            .find(|(_, asked)| asked == session)?;
        let agent = workflow.agents.iter().find(|agent| agent.index == index)?;
        Some((host, agent))
    })
}

/// Which ask each agent of `workflow` made, by the agent's number, as the bottom list joins
/// them too.
fn joins(
    facts: &RunFacts,
    host: &SessionKey,
    workflow: &CliWorkflowRun,
) -> BTreeMap<u32, SessionKey> {
    let labels: Vec<(&SessionFacts, String)> = facts
        .sessions
        .iter()
        .filter(|session| is_ask(session))
        .map(|session| (session, session.label()))
        .collect();
    let asks: Vec<Ask<'_>> = labels
        .iter()
        .map(|(session, label)| Ask {
            key: &session.key,
            label,
            started_ms: super::workflow::epoch_ms(session.started_at),
            asked_by: session.started.asked_by.as_ref(),
        })
        .collect();
    asks::joins(&asks, host, workflow)
}

/// A review or a sign-off asked through the tool: a reviewer's or judge's session tied to no
/// task of its own.
fn is_ask(session: &SessionFacts) -> bool {
    matches!(session.started.role, RoleName::Reviewer | RoleName::Judge) && session.task.is_none()
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

    /// Once Astra answers, its row says what it decided. Claude Code's agent that asked is
    /// still at work passing the answer on, so the row has the verdict alone for now.
    #[test]
    fn an_answered_step_says_what_the_role_decided() {
        for (answer, said) in [
            (json!({"accept": true, "summary": "Fine"}), "approved"),
            (json!({"accept": false}), "sent it back"),
            (
                json!({"accept": false, "findings": ["--due with no date isn't rejected"]}),
                "sent it back",
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

    /// Claude Code's workflow asks about `store` a second time under the same label, and the
    /// crew labels that ask by its round: each row of the label is the round it asked for.
    #[test]
    fn a_label_asked_again_fills_the_row_of_its_round() {
        let (_, astra, mut events) = asked_about_store();
        for event in &mut events {
            if let RunEventKind::Agent(AgentEvent::Workflow { workflow }) = &mut event.kind {
                workflow.agents[1] = agent(2, "review:store");
            }
        }
        let again = SessionKey::numbered(3);
        events.push(labelled(
            session_started(4, &again, None, RoleName::Reviewer, "astra"),
            "review:store · round 2",
        ));
        let sessions: Vec<SessionKey> = rows(&events)
            .into_iter()
            .map(|(_, _, session, _)| session)
            .collect();
        assert_eq!(sessions, [astra, again]);
    }

    /// Haiku's script numbers its agent `review:store#1` and asks under `review:store`: the
    /// ask still fills that agent's row, under the label without its round.
    #[test]
    fn an_ask_fills_its_agents_row_when_the_script_numbered_the_label() {
        let (_, astra, mut events) = asked_about_store();
        for event in &mut events {
            if let RunEventKind::Agent(AgentEvent::Workflow { workflow }) = &mut event.kind {
                workflow.agents[0] = agent(1, "review:store#1");
            }
        }
        let rows = rows(&events);
        assert_eq!(rows[0].0, "review:store");
        assert_eq!(rows[0].2, astra);
        assert_eq!(rows[1].1, "claude-code");
    }

    /// The crew stamped the ask with the agent at work, whose label is nothing like it: the
    /// ask fills that agent's row, and no other.
    #[test]
    fn a_stamped_ask_fills_the_row_of_the_agent_it_names() {
        let (host, astra, mut events) = asked_about_store();
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut events[2].kind {
            started.label = Some("sign-off:due-dates".to_owned());
            started.asked_by = Some(argonauts_core::events::session::AskedBy {
                session: host.clone(),
                workflow: None,
                agent: Some("review:cli".to_owned()),
            });
        }
        let rows = rows(&events);
        assert_eq!(rows[0].2, host);
        assert_eq!((rows[1].0.as_str(), &rows[1].2), ("review:cli", &astra));
    }
}
