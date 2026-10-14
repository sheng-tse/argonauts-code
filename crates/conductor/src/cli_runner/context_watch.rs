//! Asking a CLI for its context by category while something on screen shows the session, and
//! counting what Argonauts Code sent it, for the CLIs that can't say.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::session::SessionEvent;

/// The fastest a watched session is asked again. Each fill would otherwise ask, and a busy
/// session fills several times a second.
pub const ASK_EVERY: Duration = Duration::from_secs(10);

/// The fastest a crew agent's session is asked while nothing shows it: often enough for its
/// row on the Context tab, and seldom enough that the run's journal stays small.
pub const CREW_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct ContextWatch {
    /// Set while something shows the session.
    detail: Option<ContextDetail>,
    /// A crew agent's session is asked for the short count though nothing shows it, so the
    /// Context tab can say what fills its window.
    crew: bool,
    asked_at: Option<Instant>,
}

impl ContextWatch {
    /// The watch of a run's session.
    pub fn of_crew() -> Self {
        Self {
            crew: true,
            ..Self::default()
        }
    }

    pub fn set(&mut self, detail: Option<ContextDetail>) {
        self.detail = detail;
    }

    /// A fill came. The detail to ask with, when the session is watched or a crew agent's,
    /// and the last ask was long enough ago.
    pub fn after_fill(&mut self, now: Instant) -> Option<ContextDetail> {
        let (detail, every) = match self.detail {
            Some(detail) => (detail, ASK_EVERY),
            None if self.crew => (ContextDetail::Summary, CREW_EVERY),
            None => return None,
        };
        if self
            .asked_at
            .is_some_and(|at| now.saturating_duration_since(at) < every)
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

/// What Argonauts Code sent the session, in estimated tokens: every input part it recorded, the
/// brief, the task, a fallback note, and each message delivered since. Shared by the outbox's
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
    fn a_crew_agents_session_is_asked_in_short_though_nothing_shows_it() {
        let start = Instant::now();
        let mut watch = ContextWatch::of_crew();
        assert_eq!(watch.after_fill(start), Some(ContextDetail::Summary));
        assert_eq!(watch.after_fill(start + ASK_EVERY), None);
        assert_eq!(
            watch.after_fill(start + CREW_EVERY),
            Some(ContextDetail::Summary)
        );
        // Shown on a screen, it is asked as the screen asks and at the screen's pace.
        watch.set(Some(ContextDetail::Full));
        assert_eq!(
            watch.after_fill(start + CREW_EVERY + ASK_EVERY),
            Some(ContextDetail::Full)
        );
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
