//! Asking a CLI for its context by category while something on screen shows the session, and
//! counting what Argonauts sent it, for the CLIs that can't say.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::session::SessionEvent;

/// The fastest a watched session is asked again. Each fill would otherwise ask, and a busy
/// session fills several times a second.
pub const ASK_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct ContextWatch {
    /// Set while something shows the session.
    detail: Option<ContextDetail>,
    asked_at: Option<Instant>,
}

impl ContextWatch {
    pub fn set(&mut self, detail: Option<ContextDetail>) {
        self.detail = detail;
    }

    /// A fill came. The detail to ask with, when the session is watched and the last ask was
    /// long enough ago.
    pub fn after_fill(&mut self, now: Instant) -> Option<ContextDetail> {
        let detail = self.detail?;
        if self
            .asked_at
            .is_some_and(|at| now.saturating_duration_since(at) < ASK_EVERY)
        {
            return None;
        }
        self.asked_at = Some(now);
        Some(detail)
    }

    /// Asked outright, as /context opening does. It counts toward the pace.
    pub fn asked(&mut self, now: Instant) {
        self.asked_at = Some(now);
    }
}

/// What Argonauts sent the session, in estimated tokens: every input part it recorded, the
/// brief, the card, a handoff pack, and each message delivered since. Shared by the outbox's
/// clones.
#[derive(Debug, Clone, Default)]
pub struct SentTokens(Arc<AtomicU64>);

impl SentTokens {
    pub fn count(&self, event: &SessionEvent) {
        if let SessionEvent::Input(input) = event {
            let tokens: u64 = input
                .parts
                .iter()
                .map(|part| u64::from(part.approx_tokens))
                .sum();
            self.0.fetch_add(tokens, Ordering::Relaxed);
        }
    }

    /// `None` until something was sent.
    pub fn total(&self) -> Option<u64> {
        Some(self.0.load(Ordering::Relaxed)).filter(|tokens| *tokens > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watched_session_is_asked_after_a_fill_at_most_every_ten_seconds() {
        let start = Instant::now();
        let mut watch = ContextWatch::default();
        assert_eq!(watch.after_fill(start), None, "nothing shows it");
        watch.set(Some(ContextDetail::Summary));
        assert_eq!(watch.after_fill(start), Some(ContextDetail::Summary));
        assert_eq!(watch.after_fill(start + Duration::from_secs(9)), None);
        assert_eq!(
            watch.after_fill(start + ASK_EVERY),
            Some(ContextDetail::Summary)
        );
        watch.set(None);
        assert_eq!(watch.after_fill(start + ASK_EVERY * 3), None);
    }

    #[test]
    fn an_outright_ask_counts_toward_the_pace() {
        let start = Instant::now();
        let mut watch = ContextWatch::default();
        watch.set(Some(ContextDetail::Full));
        watch.asked(start);
        assert_eq!(watch.after_fill(start + Duration::from_secs(5)), None);
        assert_eq!(
            watch.after_fill(start + Duration::from_secs(10)),
            Some(ContextDetail::Full)
        );
    }
}
