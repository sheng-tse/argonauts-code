use argonauts_core::RunId;
use argonauts_core::events::RunEvent;
use argonauts_core::views::ContextRow;

use super::sessions::RunFacts;

/// Each live session's context use, cap (`sessions.rotate_at`), tool calls, spend, and
/// rotations.
pub(crate) fn context_rows(events: &[RunEvent], run: &RunId, cap: u64) -> Vec<ContextRow> {
    let facts = RunFacts::of(events);
    facts
        .sessions
        .iter()
        .filter(|session| session.ended.is_none())
        .map(|session| {
            let (used, window) = session.context.unwrap_or((0, None));
            ContextRow {
                run: Some(run.clone()),
                session: session.key.clone(),
                label: session.label(),
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
            }
        })
        .collect()
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
    fn context_rows_list_live_sessions_with_cap_calls_spend_and_rotations() {
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
        assert_eq!(rows.len(), 2, "the rotated-away session is gone");
        let worker = &rows[0];
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
        let review = &rows[1];
        assert_eq!(review.worker.agent, "codex");
        assert_eq!((review.used, review.rotations), (0, 0));
        assert!(review.categories.is_empty());
    }
}
