//! What the working line says while the turn's CLI retries its request to the API, in Claude
//! Code's words: `API error · Retrying in 8s · attempt 4/10`.

use std::time::{Duration, Instant};

use crate::base::format;
use crate::frame::state::ConversationState;

/// The CLI whose retries are counted out of a known number.
const CLAUDE_CODE: &str = "claude-code";

/// How many times Claude Code retries a request before its turn fails: its own default, which
/// Argonauts leaves as it is.
const CLAUDE_CODE_RETRIES: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryNote {
    /// Which retry this is, from 1.
    pub try_number: u32,
    /// How many retries the CLI makes, where that's known.
    pub of: Option<u32>,
    /// When the retry goes, where the CLI said how long it waits.
    pub at: Option<Instant>,
}

impl RetryNote {
    /// The note for the conversation's latest retry. The same retry keeps the moment it goes,
    /// and a new one counts down from `now`.
    pub fn follow(
        last: Option<Self>,
        conversation: &ConversationState,
        now: Instant,
    ) -> Option<Self> {
        let retry = conversation.retry?;
        if let Some(last) = last.filter(|last| last.try_number == retry.try_number) {
            return Some(last);
        }
        let claude_code = conversation
            .worker
            .as_ref()
            .is_some_and(|worker| worker.agent == CLAUDE_CODE);
        Some(Self {
            try_number: retry.try_number,
            // Set higher in its environment, Claude Code goes past its usual count, and the
            // count alone stays true.
            of: Some(CLAUDE_CODE_RETRIES).filter(|&of| claude_code && retry.try_number <= of),
            at: retry
                .delay_ms
                .map(|delay| now + Duration::from_millis(delay)),
        })
    }

    /// The wait counts down in whole seconds, rounded up, to none.
    pub fn words(&self, now: Instant) -> String {
        let wait = self
            .at
            .map(|at| format!(" in {}", format::duration(left(at, now))))
            .unwrap_or_default();
        let of = self.of.map(|of| format!("/{of}")).unwrap_or_default();
        format!(
            "API error · Retrying{wait} · attempt {}{of}",
            self.try_number
        )
    }

    /// When the countdown shows its next second, while there's one to show.
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
        let at = self.at?;
        let left = left(at, now);
        (!left.is_zero()).then(|| at - (left - Duration::from_secs(1)))
    }
}

/// What's left of the wait until `at`, rounded up to a whole second.
fn left(at: Instant, now: Instant) -> Duration {
    let millis = at.saturating_duration_since(now).as_millis();
    Duration::from_secs(u64::try_from(millis.div_ceil(1000)).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use argonauts_core::base::names::ModelRef;

    use super::*;
    use crate::frame::state::TurnRetry;

    fn conversation(agent: &str, try_number: u32, delay_ms: Option<u64>) -> ConversationState {
        let mut conversation = ConversationState::default();
        conversation.worker = Some(ModelRef::new(agent, None));
        conversation.retry = Some(TurnRetry {
            try_number,
            delay_ms,
        });
        conversation
    }

    #[test]
    fn claude_codes_retry_reads_as_its_own_line() {
        let now = Instant::now();
        let note = RetryNote::follow(None, &conversation("claude-code", 4, Some(8_000)), now)
            .expect("a retry");
        assert_eq!(note.words(now), "API error · Retrying in 8s · attempt 4/10");
        // Rounded up, the wait reads 8s until it's down to 7.
        assert_eq!(
            note.words(now + Duration::from_millis(900)),
            "API error · Retrying in 8s · attempt 4/10"
        );
        assert_eq!(
            note.words(now + Duration::from_millis(1_000)),
            "API error · Retrying in 7s · attempt 4/10"
        );
        assert_eq!(
            note.words(now + Duration::from_secs(30)),
            "API error · Retrying in 0s · attempt 4/10"
        );
    }

    #[test]
    fn the_same_retry_keeps_its_countdown_and_the_next_starts_its_own() {
        let t0 = Instant::now();
        let first = RetryNote::follow(None, &conversation("claude-code", 4, Some(8_000)), t0);
        let later = t0 + Duration::from_secs(3);
        let same = RetryNote::follow(first, &conversation("claude-code", 4, Some(8_000)), later);
        assert_eq!(same, first);
        let next = RetryNote::follow(same, &conversation("claude-code", 5, Some(16_000)), later)
            .expect("the next retry");
        assert_eq!(
            next.words(later),
            "API error · Retrying in 16s · attempt 5/10"
        );
        let over = RetryNote::follow(Some(next), &ConversationState::default(), later);
        assert_eq!(over, None);
    }

    #[test]
    fn a_count_nobody_knows_the_end_of_reads_as_the_count_alone() {
        let now = Instant::now();
        let words = |agent, try_number, delay| {
            RetryNote::follow(None, &conversation(agent, try_number, delay), now)
                .map(|note| note.words(now))
        };
        assert_eq!(
            words("codex", 2, Some(4_000)).as_deref(),
            Some("API error · Retrying in 4s · attempt 2")
        );
        assert_eq!(
            words("claude-code", 12, Some(32_000)).as_deref(),
            Some("API error · Retrying in 32s · attempt 12")
        );
        assert_eq!(
            words("claude-code", 1, None).as_deref(),
            Some("API error · Retrying · attempt 1/10")
        );
    }

    /// The picture's working line: Captain's own turn, 41 seconds in with 2.2k tokens down,
    /// while Claude Code waits 8 seconds before its fourth retry, drawn at the picture's width
    /// and compared cell for cell, colours included.
    #[test]
    fn working_line_while_claude_code_retries_reads_as_its_picture() {
        use crate::base::draw::render::{Clock, RenderCtx};
        use crate::base::theme::{Glyphs, Theme};
        use crate::bottom::WorkingLine;
        use crate::testing::pictures::picture_check::assert_rows_match;

        const WIDTH: u16 = 118;
        const ROW: usize = 45;

        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let now = Instant::now();
        let mut line = WorkingLine::new("Writing a workflow", now - Duration::from_secs(41));
        line.tokens_down = 2_200;
        // Nothing but the retries for a while, which turns no line red.
        line.heard_at = Some(now - Duration::from_secs(25));
        line.retry = RetryNote::follow(None, &conversation("claude-code", 4, Some(8_000)), now);
        let ctx = RenderCtx::new(
            WIDTH,
            &theme,
            Clock::new(now - Duration::from_secs(41), now),
        );

        // The picture's rows end where their text does, as the screen's do.
        let txt: Vec<&str> = include_str!("../../../tests/golden/network_after.txt")
            .lines()
            .map(str::trim_end)
            .collect();
        assert_rows_match(
            "network_after",
            &[line.line(&ctx)],
            (
                &txt.join("\n"),
                include_str!("../../../tests/golden/network_after.html"),
            ),
            ROW..ROW + 1,
            &theme,
        );
    }

    #[test]
    fn the_countdown_changes_on_its_own_seconds() {
        let t0 = Instant::now();
        let note = RetryNote::follow(None, &conversation("claude-code", 4, Some(8_500)), t0)
            .expect("a retry");
        // 8.5s left reads 9s, and 8s once half a second has gone.
        assert_eq!(note.next_change(t0), Some(t0 + Duration::from_millis(500)));
        let at = t0 + Duration::from_millis(8_500);
        assert_eq!(note.next_change(at), None);
        assert_eq!(note.next_change(at - Duration::from_millis(200)), Some(at));
    }
}
