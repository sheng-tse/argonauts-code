//! Keeps the journal small: each session's `TextDelta` and `Reasoning` events are buffered and
//! recorded merged, at most `run_host.journal_deltas_per_second` a second, and a session's
//! buffer is flushed before any other event of that session.

use std::collections::BTreeMap;

use time::{Duration, OffsetDateTime};

use argonauts_core::event::AgentEvent;
use argonauts_core::names::SessionKey;

/// With `per_second` 0, deltas wait for the session's next other event or its end.
#[derive(Debug, Default)]
pub struct DeltaBuffer {
    per_second: u32,
    pending: BTreeMap<SessionKey, Vec<AgentEvent>>,
    last_written: BTreeMap<SessionKey, OffsetDateTime>,
}

impl DeltaBuffer {
    pub fn new(per_second: u32) -> Self {
        Self {
            per_second,
            pending: BTreeMap::new(),
            last_written: BTreeMap::new(),
        }
    }

    /// What to record now for this session, in order. A delta is held unless its session is
    /// due; any other event releases the session's merged deltas first, then itself.
    pub fn push(
        &mut self,
        session: &SessionKey,
        event: AgentEvent,
        now: OffsetDateTime,
    ) -> Vec<AgentEvent> {
        if !is_delta(&event) {
            let mut out = self.flush(session);
            out.push(event);
            return out;
        }
        merge_into(self.pending.entry(session.clone()).or_default(), event);
        if self.is_due(session, now) {
            self.last_written.insert(session.clone(), now);
            return self.flush(session);
        }
        Vec::new()
    }

    /// Everything held for this session, merged, such as before its end is recorded.
    pub fn flush(&mut self, session: &SessionKey) -> Vec<AgentEvent> {
        self.pending.remove(session).unwrap_or_default()
    }

    /// Sessions whose held deltas are due by `now`, merged.
    pub fn due(&mut self, now: OffsetDateTime) -> Vec<(SessionKey, AgentEvent)> {
        let due: Vec<SessionKey> = self
            .pending
            .keys()
            .filter(|session| self.is_due(session, now))
            .cloned()
            .collect();
        let mut out = Vec::new();
        for session in due {
            self.last_written.insert(session.clone(), now);
            for event in self.flush(&session) {
                out.push((session.clone(), event));
            }
        }
        out
    }

    /// Drops what it remembers of an ended session, after its last flush.
    pub fn forget(&mut self, session: &SessionKey) {
        self.pending.remove(session);
        self.last_written.remove(session);
    }

    fn is_due(&self, session: &SessionKey, now: OffsetDateTime) -> bool {
        if self.per_second == 0 {
            return false;
        }
        let gap = Duration::seconds(1) / self.per_second;
        self.last_written
            .get(session)
            .is_none_or(|last| now - *last >= gap)
    }
}

fn is_delta(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TextDelta { .. } | AgentEvent::Reasoning { .. }
    )
}

/// Appends to the last held delta when it's the same kind (and for text, the same parent), so
/// text and reasoning never mix and their order is kept.
fn merge_into(held: &mut Vec<AgentEvent>, event: AgentEvent) {
    match (held.last_mut(), event) {
        (
            Some(AgentEvent::TextDelta { text, parent }),
            AgentEvent::TextDelta {
                text: more,
                parent: more_parent,
            },
        ) if *parent == more_parent => text.push_str(&more),
        (Some(AgentEvent::Reasoning { text }), AgentEvent::Reasoning { text: more }) => {
            text.push_str(&more);
        }
        (_, event) => held.push(event),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> AgentEvent {
        AgentEvent::TextDelta {
            text: text.to_owned(),
            parent: None,
        }
    }

    fn reasoning(text: &str) -> AgentEvent {
        AgentEvent::Reasoning {
            text: text.to_owned(),
        }
    }

    fn tool() -> AgentEvent {
        AgentEvent::ToolFinished {
            id: "t1".to_owned(),
            ok: true,
            output: None,
            parent: None,
        }
    }

    #[test]
    fn deltas_are_merged_and_written_at_most_per_second() {
        let key = SessionKey::numbered(1);
        let start = OffsetDateTime::UNIX_EPOCH;
        let mut buffer = DeltaBuffer::new(10);
        assert_eq!(buffer.push(&key, text("a"), start), vec![text("a")]);
        assert!(buffer.push(&key, text("b"), start).is_empty());
        assert!(
            buffer
                .push(&key, text("c"), start + Duration::milliseconds(50))
                .is_empty()
        );
        assert_eq!(
            buffer.push(&key, text("d"), start + Duration::milliseconds(100)),
            vec![text("bcd")]
        );
    }

    #[test]
    fn another_event_releases_the_held_deltas_first_in_order() {
        let key = SessionKey::numbered(1);
        let now = OffsetDateTime::UNIX_EPOCH;
        let mut buffer = DeltaBuffer::new(1);
        buffer.push(&key, text("x"), now);
        buffer.push(&key, text("a"), now);
        buffer.push(&key, reasoning("r"), now);
        buffer.push(&key, text("b"), now);
        assert_eq!(
            buffer.push(&key, tool(), now),
            vec![text("a"), reasoning("r"), text("b"), tool()]
        );
    }

    #[test]
    fn due_releases_only_sessions_past_their_gap() {
        let one = SessionKey::numbered(1);
        let two = SessionKey::numbered(2);
        let now = OffsetDateTime::UNIX_EPOCH;
        let mut buffer = DeltaBuffer::new(2);
        buffer.push(&one, text("1"), now);
        buffer.push(&one, text("held"), now);
        buffer.push(&two, text("2"), now + Duration::milliseconds(400));
        buffer.push(&two, text("later"), now + Duration::milliseconds(400));
        let due = buffer.due(now + Duration::milliseconds(600));
        assert_eq!(due, vec![(one.clone(), text("held"))]);
        assert!(buffer.flush(&one).is_empty());
        assert_eq!(buffer.flush(&two), vec![text("later")]);
    }

    #[test]
    fn zero_per_second_holds_deltas_until_a_flush() {
        let key = SessionKey::numbered(1);
        let now = OffsetDateTime::UNIX_EPOCH;
        let mut buffer = DeltaBuffer::new(0);
        assert!(buffer.push(&key, text("a"), now).is_empty());
        assert!(buffer.due(now + Duration::hours(1)).is_empty());
        assert_eq!(buffer.flush(&key), vec![text("a")]);
    }
}
