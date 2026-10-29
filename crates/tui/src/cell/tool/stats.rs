use std::time::Instant;

use ratatui::text::Span;

use crate::format;
use crate::render::RenderCtx;
use crate::theme::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct ToolStats {
    pub started_at: Option<Instant>,
    pub ended_at: Option<Instant>,
    /// Tokens received so far.
    pub tokens_down: u64,
}

impl ToolStats {
    pub fn started(at: Instant) -> Self {
        Self {
            started_at: Some(at),
            ..Self::default()
        }
    }

    pub(super) fn span(&self, ctx: &RenderCtx<'_>) -> Option<Span<'static>> {
        let mut text = String::new();
        if let Some(started) = self.started_at {
            let now = self.ended_at.unwrap_or(ctx.clock.now);
            let elapsed = now.saturating_duration_since(started);
            text.push_str(" · ");
            text.push_str(&format::duration_compact(elapsed));
        }
        if self.tokens_down > 0 {
            text.push_str(" · ");
            text.push_str(ctx.theme.glyphs.tokens);
            text.push_str(crate::theme::glyphs::gap_after(ctx.theme.glyphs.tokens));
            text.push_str(&format::tokens(self.tokens_down));
        }
        (!text.is_empty()).then(|| Span::styled(text, ctx.theme.style(Role::Dim)))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cell::{HistoryCell, ToolCall, ToolItem, ToolKind, ToolState};
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    #[test]
    fn a_live_header_ends_with_dim_time_and_tokens() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let start = Instant::now();
        let ctx = RenderCtx::new(
            80,
            &theme,
            Clock::new(start, start + Duration::from_secs(252)),
        );
        let mut call = ToolCall::described(
            ToolKind::Agent,
            "claude-code/opus · worker · task-002 (Add the JSON writer)",
        )
        .with_stats(ToolStats {
            started_at: Some(start),
            ended_at: None,
            tokens_down: 18_100,
        })
        .with_item(ToolItem::text("Read 1 file"));
        let lines = call.lines(&ctx);
        assert_eq!(
            plain_text(&lines[..1]),
            "● claude-code/opus · worker · task-002 (Add the JSON writer) · 4m12s · ↓  18.1k"
        );
        let stats = lines[0].spans.last().expect("the stats");
        assert_eq!(stats.style, theme.style(Role::Dim));

        call.stats.ended_at = Some(start + Duration::from_secs(61));
        assert!(plain_text(&call.lines(&ctx)[..1]).ends_with("· 1m1s · ↓  18.1k"));
        call.stats = ToolStats::started(start);
        assert!(plain_text(&call.lines(&ctx)[..1]).ends_with("(Add the JSON writer) · 4m12s"));
        call.stats = ToolStats::default();
        assert!(plain_text(&call.lines(&ctx)[..1]).ends_with("(Add the JSON writer)"));
    }

    #[test]
    fn stats_stay_on_the_header_row_and_the_task_is_cut_for_them() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let start = Instant::now();
        let at = Clock::new(start, start + Duration::from_secs(103));
        let call = ToolCall::described(
            ToolKind::Agent,
            "agy · reviewer · task-002 (Review the JSON writer)",
        )
        .with_stats(ToolStats {
            started_at: Some(start),
            ended_at: None,
            tokens_down: 6_100,
        })
        .with_state(ToolState::Done);
        let row = |width| plain_text(&call.lines(&RenderCtx::new(width, &theme, at)));
        assert_eq!(
            row(70),
            "● agy · reviewer · task-002 (Review the JSON writer) · 1m43s · ↓  6.1k"
        );
        assert_eq!(
            row(69),
            "● agy · reviewer · task-002 (Review the JSON writ…) · 1m43s · ↓  6.1k"
        );
        assert_eq!(row(48), "● agy · reviewer · task-002 · 1m43s · ↓  6.1k");
        assert_eq!(row(33), "● agy · reviewer · task-002");
    }

    #[test]
    fn a_summary_leaves_the_stats_to_its_result_line() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let start = Instant::now();
        let ctx = RenderCtx::new(80, &theme, Clock::still(start));
        let call = ToolCall::described(ToolKind::Agent, "agent").with_stats(ToolStats {
            started_at: Some(start),
            ended_at: Some(start),
            tokens_down: 900,
        });
        let summary = call.summarized_with("900 tokens · 0s", None);
        assert_eq!(summary.stats, ToolStats::default());
        assert_eq!(
            plain_text(&summary.lines(&ctx)),
            "● agent\n  ⎿  Running (0 tool uses · 900 tokens · 0s)"
        );
    }
}
