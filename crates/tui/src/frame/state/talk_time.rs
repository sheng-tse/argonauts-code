//! How long the conversation has gone on, how much of that its agent spent working, and how
//! many lines its edits changed, for the Usage tab's line about this conversation.

use std::time::Duration;

use argonauts_core::AgentEvent;
use argonauts_core::app::ConversationEvent;
use time::OffsetDateTime;

use super::session::ToolRecord;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TalkTime {
    /// When its first turn started.
    pub started: Option<OffsetDateTime>,
    /// Every finished turn's time, added up.
    pub worked: Duration,
    /// Lines added by the edits that went through.
    pub added: usize,
    /// Lines removed by the edits that went through.
    pub removed: usize,
}

impl TalkTime {
    pub(super) fn apply(&mut self, event: &ConversationEvent, now: OffsetDateTime) {
        match event {
            ConversationEvent::TurnStarted { .. } => {
                self.started.get_or_insert(now);
            }
            ConversationEvent::TurnFinished { elapsed_ms, .. } => {
                self.worked += Duration::from_millis(*elapsed_ms);
            }
            _ => {}
        }
    }

    /// Adds what an edit changed once it went through, as `lines` reads it off the call's input.
    /// Read before the finish reaches `tools`, so a call already marked finished is never
    /// counted twice.
    pub fn count_edit(
        &mut self,
        event: &ConversationEvent,
        tools: &[ToolRecord],
        lines: impl Fn(&str, &serde_json::Value) -> (usize, usize),
    ) {
        let ConversationEvent::Agent {
            event: AgentEvent::ToolFinished { id, ok: true, .. },
            ..
        } = event
        else {
            return;
        };
        let Some(call) = tools.iter().rev().find(|call| call.id == *id) else {
            return;
        };
        if call.ok.is_none() {
            let (added, removed) = lines(&call.name, &call.input);
            self.added += added;
            self.removed += removed;
        }
    }
}
