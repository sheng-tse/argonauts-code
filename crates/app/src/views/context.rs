use std::collections::BTreeMap;

use argonauts_conductor::fallback::why::{category_words, stop_line};
use argonauts_core::events::run::Party;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::shared::{BlackboardEvent, BlackboardKind};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::views::{ContextRow, EndedHow, RowStatus, SessionLeft};
use argonauts_core::{ErrorCategory, RunId};
use time::OffsetDateTime;

use super::sessions::{RunFacts, SessionFacts};

/// Each session's context use, cap (`sessions.rotate_at`), tool calls, spend, and rotations,
/// in the order they started. One that ended keeps what it held as it ended, and says how.
///
/// A tie-breaker goes by the label of the agent whose question it answered, so the tab counts
/// it under that agent and not as one of its own.
pub(crate) fn context_rows(events: &[RunEvent], run: &RunId, cap: u64) -> Vec<ContextRow> {
    let facts = RunFacts::of(events);
    let answered_for = answered_for(events);
    let asker = |session: &SessionFacts| {
        answered_for
            .get(&session.key)
            .filter(|_| session.started.role == RoleName::TieBreaker)
            .and_then(|asker| facts.session(asker))
    };
    facts
        .sessions
        .iter()
        .map(|session| {
            let (used, window) = session.context.unwrap_or((0, None));
            ContextRow {
                run: Some(run.clone()),
                session: session.key.clone(),
                label: asker(session).unwrap_or(session).label(),
                worker: session.worker.clone(),
                used,
                window,
                cap: Some(cap),
                tool_calls: session.tool_calls(),
                spent: session.spent(),
                rotations: facts.rotations(session),
                estimated: false,
                counts: false,
                categories: session.context_rows.clone(),
                recorded: None,
                ended: left(&facts, session),
                answered_for: asker(session).map(|asker| asker.key.clone()),
            }
        })
        .collect()
}

/// Each session that answered another agent's question on the board, with the session that
/// asked it.
fn answered_for(events: &[RunEvent]) -> BTreeMap<SessionKey, SessionKey> {
    let agent = |party: &Party| match party {
        Party::Agent { session, .. } => Some(session.clone()),
        _ => None,
    };
    let mut askers = BTreeMap::new();
    let mut answered = BTreeMap::new();
    for event in events {
        let RunEventKind::Blackboard(BlackboardEvent::Posted(message)) = &event.kind else {
            continue;
        };
        match (message.kind, &message.in_reply_to) {
            (BlackboardKind::Question, _) => {
                if let Some(asker) = agent(&message.from) {
                    askers.insert(message.id.clone(), asker);
                }
            }
            (BlackboardKind::Answer, Some(question)) => {
                let asker = askers.get(question);
                if let (Some(by), Some(asker)) = (agent(&message.from), asker)
                    && by != *asker
                {
                    answered.insert(by, asker.clone());
                }
            }
            _ => {}
        }
    }
    answered
}

/// When the run finished, where its log says it has.
pub(crate) fn finished_at(events: &[RunEvent]) -> Option<OffsetDateTime> {
    RunFacts::of(events).finished_at
}

/// How `session` ended, once it has.
fn left(facts: &RunFacts, session: &SessionFacts) -> Option<SessionLeft> {
    let end = session.ended.as_ref()?;
    let how = match facts.status(session) {
        RowStatus::Failed => EndedHow::Failed {
            why: failed_why(session, &end.reason),
        },
        RowStatus::Paused | RowStatus::Skipped => EndedHow::Stopped,
        _ => EndedHow::Done,
    };
    Some(SessionLeft {
        at: session.ended_at.unwrap_or(session.started_at),
        how,
        fallback_for: session.fallback_for.clone(),
    })
}

/// Why a session failed, in the few words its row has room for: `no valid report in 2 tries`.
fn failed_why(session: &SessionFacts, reason: &EndReason) -> String {
    let why = match reason {
        _ if session.unreported => "no valid report".to_owned(),
        EndReason::CliError {
            category: ErrorCategory::AnswerMissed,
            ..
        } => "no valid report".to_owned(),
        EndReason::CliError { category, .. } => category_words(*category).to_owned(),
        reason => stop_line(reason),
    };
    match session.try_number {
        Some(tries) if tries > 1 => format!("{why} in {tries} tries"),
        _ => why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{agent, run_id, session_ended, session_started, usage};
    use argonauts_core::AgentEvent;
    use argonauts_core::events::context::{ContextCategory, ContextKind, ContextUsage};
    use argonauts_core::events::session::EndReason;
    use argonauts_core::names::{RoleName, SessionKey};

    #[test]
    fn context_rows_list_every_session_with_cap_calls_spend_and_rotations() {
        let task = argonauts_core::TaskId::new("task-001").unwrap();
        let first = SessionKey::numbered(1);
        let second = SessionKey::numbered(2);
        let reviewer = SessionKey::numbered(3);
        let messages = ContextCategory {
            name: "Messages".to_owned(),
            tokens: 17_500,
            color: "purple_FOR_SUBAGENTS_ONLY".to_owned(),
            kind: ContextKind::Used,
        };
        let events = vec![
            session_started(0, &first, Some(&task), RoleName::Worker, "claude-code"),
            agent(
                1,
                &first,
                AgentEvent::ContextFill {
                    used: 199_000,
                    size: Some(200_000),
                    sent: None,
                },
            ),
            session_ended(2, &first, EndReason::ContextFull, 1_000),
            session_started(3, &second, Some(&task), RoleName::Worker, "claude-code"),
            agent(
                4,
                &second,
                AgentEvent::ContextFill {
                    used: 42_000,
                    size: Some(1_000_000),
                    sent: None,
                },
            ),
            agent(
                5,
                &second,
                AgentEvent::ToolStarted {
                    id: "t1".to_owned(),
                    name: "Bash".to_owned(),
                    input: serde_json::json!({"command": "cargo test"}),
                    parent: None,
                },
            ),
            usage(6, &second, 1_000, 200, 0.25),
            session_started(7, &reviewer, Some(&task), RoleName::Reviewer, "codex"),
            agent(
                8,
                &second,
                AgentEvent::ContextBreakdown {
                    usage: Box::new(ContextUsage {
                        categories: vec![messages.clone()],
                        total_tokens: 44_000,
                        window: 1_000_000,
                        ..ContextUsage::default()
                    }),
                },
            ),
        ];
        let rows = context_rows(&events, &run_id(), 200_000);
        assert_eq!(rows.len(), 3, "{rows:?}");
        // The session it rotated away from stays, as it left its window.
        let rotated = &rows[0];
        assert_eq!((rotated.session.clone(), rotated.used), (first, 199_000));
        assert_eq!(
            rotated.ended.as_ref().map(|left| &left.how),
            Some(&EndedHow::Done)
        );
        let worker = &rows[1];
        assert_eq!(worker.ended, None, "it still runs");
        assert_eq!(worker.session, second);
        assert_eq!(worker.label, "worker · task-001");
        assert_eq!(
            (worker.used, worker.window, worker.cap),
            (42_000, Some(1_000_000), Some(200_000))
        );
        assert_eq!(worker.tool_calls, 1);
        assert_eq!(worker.spent.input_tokens, 1_000);
        assert_eq!(worker.rotations, 1);
        // Its CLI's count says what fills it. How full it is stays the fill's to say.
        assert_eq!(worker.categories, [messages]);
        let review = &rows[2];
        assert_eq!(review.worker.agent, "codex");
        assert_eq!((review.used, review.rotations), (0, 0));
        assert!(review.categories.is_empty());
    }

    /// Every session of a finished run keeps the row it left: its fill, what filled it and how
    /// it ended, and the run says when it finished.
    #[test]
    fn a_finished_runs_sessions_keep_their_rows() {
        use crate::client::test_support::event;
        use argonauts_core::events::RunEventKind;
        use argonauts_core::events::run::{RunLifecycle, RunOutcome};

        let task = argonauts_core::TaskId::new("task-due-dates").unwrap();
        let (planner, worker) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let messages = |tokens| ContextCategory {
            name: "Messages".to_owned(),
            tokens,
            color: "purple_FOR_SUBAGENTS_ONLY".to_owned(),
            kind: ContextKind::Used,
        };
        let filled = |seq, session: &SessionKey, used, tokens| {
            [
                agent(
                    seq,
                    session,
                    AgentEvent::ContextFill {
                        used,
                        size: Some(1_000_000),
                        sent: None,
                    },
                ),
                agent(
                    seq + 1,
                    session,
                    AgentEvent::ContextBreakdown {
                        usage: Box::new(ContextUsage {
                            categories: vec![messages(tokens)],
                            total_tokens: used,
                            window: 1_000_000,
                            ..ContextUsage::default()
                        }),
                    },
                ),
            ]
        };
        let mut events = vec![session_started(
            0,
            &planner,
            None,
            RoleName::Orchestrator,
            "claude-code",
        )];
        events.extend(filled(1, &planner, 12_021, 4_921));
        events.push(session_ended(3, &planner, EndReason::Completed, 19_512));
        events.push(session_started(
            4,
            &worker,
            Some(&task),
            RoleName::Worker,
            "claude-code",
        ));
        events.extend(filled(5, &worker, 24_924, 5_989));
        events.push(session_ended(
            7,
            &worker,
            EndReason::CliError {
                category: ErrorCategory::AnswerMissed,
                message: "Failed to provide valid structured output".to_owned(),
            },
            25_200,
        ));
        events.push(event(
            8,
            RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::Merged,
                detail: None,
            }),
        ));
        let rows = context_rows(&events, &run_id(), 0);
        let kept: Vec<(&SessionKey, u64, u64)> = rows
            .iter()
            .map(|row| (&row.session, row.used, row.categories[0].tokens))
            .collect();
        assert_eq!(kept, [(&planner, 12_021, 4_921), (&worker, 24_924, 5_989)]);
        let how: Vec<Option<&EndedHow>> = rows
            .iter()
            .map(|row| row.ended.as_ref().map(|left| &left.how))
            .collect();
        assert_eq!(
            how,
            [
                Some(&EndedHow::Done),
                Some(&EndedHow::Failed {
                    why: "no valid report".to_owned()
                })
            ]
        );
        assert_eq!(
            finished_at(&events),
            Some(crate::client::test_support::base_ts() + time::Duration::seconds(8))
        );
    }

    #[test]
    fn a_session_that_failed_says_why_in_a_few_words() {
        let task = argonauts_core::TaskId::new("task-001").unwrap();
        let worker = SessionKey::numbered(1);
        let missed = EndReason::CliError {
            category: ErrorCategory::AnswerMissed,
            message: "Failed to provide valid structured output".to_owned(),
        };
        let overloaded = EndReason::CliError {
            category: ErrorCategory::Overloaded,
            message: "529".to_owned(),
        };
        let ended = |reason: EndReason| {
            let events = vec![
                session_started(0, &worker, Some(&task), RoleName::Worker, "claude-code"),
                session_ended(1, &worker, reason, 1_000),
            ];
            context_rows(&events, &run_id(), 0)
                .remove(0)
                .ended
                .map(|left| left.how)
        };
        assert_eq!(
            ended(missed),
            Some(EndedHow::Failed {
                why: "no valid report".to_owned()
            })
        );
        assert_eq!(
            ended(overloaded),
            Some(EndedHow::Failed {
                why: "overloaded".to_owned()
            })
        );
        assert_eq!(ended(EndReason::Stopped), Some(EndedHow::Stopped));
        assert_eq!(ended(EndReason::Completed), Some(EndedHow::Done));
    }

    /// A tie-breaker that answered a worker's question goes by the worker's label; one that
    /// answered nobody's keeps its own, and so does an agent that answers another.
    #[test]
    fn a_tie_breaker_goes_by_the_agent_whose_question_it_answered() {
        use argonauts_core::events::RunEventKind;
        use argonauts_core::events::run::Party;
        use argonauts_core::events::shared::{
            BlackboardEvent, BlackboardKind, BlackboardMessage, Recipient,
        };
        use argonauts_core::names::MessageId;
        use argonauts_core::names::ModelRef;

        use crate::client::test_support::session_event;

        let (worker, judge, idle, other) = (
            SessionKey::numbered(1),
            SessionKey::numbered(2),
            SessionKey::numbered(3),
            SessionKey::numbered(4),
        );
        let labelled = |seq, key: &SessionKey, role, label: &str| {
            let mut event = session_started(seq, key, None, role, "claude-code");
            if let RunEventKind::Session(argonauts_core::events::SessionEvent::Started(started)) =
                &mut event.kind
            {
                started.label = Some(label.to_owned());
            }
            event
        };
        let posted = |seq, id: u32, kind, from: &SessionKey, reply: Option<u32>| {
            session_event(
                seq,
                from,
                None,
                RunEventKind::Blackboard(BlackboardEvent::Posted(BlackboardMessage {
                    id: MessageId::numbered(id),
                    kind,
                    from: Party::Agent {
                        session: from.clone(),
                        worker: ModelRef::new("claude-code", None),
                    },
                    to: Recipient::All,
                    text: "Which header names the key?".to_owned(),
                    in_reply_to: reply.map(MessageId::numbered),
                    refs: Vec::new(),
                })),
            )
        };
        let mut events = vec![labelled(0, &worker, RoleName::Worker, "build:per-api-key")];
        events.extend([
            labelled(1, &judge, RoleName::TieBreaker, "tie-breaker"),
            labelled(2, &idle, RoleName::TieBreaker, "tie-breaker-2"),
            labelled(3, &other, RoleName::Reviewer, "review:per-api-key"),
            posted(4, 1, BlackboardKind::Question, &worker, None),
            posted(5, 2, BlackboardKind::Answer, &judge, Some(1)),
            posted(6, 3, BlackboardKind::Question, &worker, None),
            posted(7, 4, BlackboardKind::Answer, &other, Some(3)),
        ]);
        let rows = context_rows(&events, &run_id(), 0);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "build:per-api-key",
                "build:per-api-key",
                "tie-breaker-2",
                "review:per-api-key"
            ]
        );
        let answered: Vec<Option<&SessionKey>> =
            rows.iter().map(|row| row.answered_for.as_ref()).collect();
        assert_eq!(answered, [None, Some(&worker), None, None]);
    }
}
