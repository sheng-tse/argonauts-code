use std::time::Duration;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::HistoryCell;
use crate::format::{self, ClockTime};
use crate::render::RenderCtx;
use crate::text::{fit_lines, sanitize_line, str_width, wrap_spans};
use crate::theme::Role;

/// `✻ Sailed for 11m 3s · done 10:58 AM`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnd {
    /// A past-tense verb and `for`, such as `Sailed for`.
    pub label: String,
    pub elapsed: Duration,
    pub done_at: Option<ClockTime>,
}

impl TurnEnd {
    pub fn new(label: impl Into<String>, elapsed: Duration, done_at: Option<ClockTime>) -> Self {
        Self {
            label: label.into(),
            elapsed,
            done_at,
        }
    }
}

impl HistoryCell for TurnEnd {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let glyph = theme.glyphs.turn_end;
        let label = sanitize_line(&self.label);
        let mut text = if label.is_empty() {
            format::duration(self.elapsed)
        } else {
            format!("{label} {}", format::duration(self.elapsed))
        };
        if let Some(clock) = self.done_at {
            text.push_str(&format!(" · done {clock}"));
        }
        let first = [Span::styled(glyph, dim), Span::raw(" ")];
        let rest = [Span::raw(" ".repeat(str_width(glyph) + 1))];
        let mut lines = wrap_spans(
            &[Span::styled(text, dim)],
            usize::from(ctx.width),
            &first,
            &rest,
        );
        fit_lines(&mut lines, usize::from(ctx.width));
        lines
    }
}

/// Where things stand after a long turn. The narrator's lines share the look with their own
/// label, so they are never mistaken for an agent's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recap {
    pub label: String,
    pub text: String,
    pub hint: Option<String>,
}

impl Recap {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            label: "recap".to_owned(),
            text: text.into(),
            hint: Some("(disable recaps in /config)".to_owned()),
        }
    }

    pub fn narrator(text: impl Into<String>) -> Self {
        Self {
            label: "narrator".to_owned(),
            text: text.into(),
            hint: None,
        }
    }
}

impl HistoryCell for Recap {
    /// The text is shown as written, not rendered as Markdown.
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let italic = dim.add_modifier(Modifier::ITALIC);
        let glyph = theme.glyphs.recap;
        let mut spans = vec![
            Span::styled(format!("{}: ", sanitize_line(&self.label)), dim),
            Span::styled(sanitize_line(&self.text).into_owned(), italic),
        ];
        if let Some(hint) = self.hint.as_deref().map(sanitize_line)
            && !hint.is_empty()
        {
            spans.push(Span::styled(format!(" {hint}"), italic));
        }
        let first = [Span::styled(glyph, dim), Span::raw(" ")];
        let rest = [Span::raw(" ".repeat(str_width(glyph) + 1))];
        let mut lines = wrap_spans(&spans, usize::from(ctx.width), &first, &rest);
        fit_lines(&mut lines, usize::from(ctx.width));
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    #[test]
    fn turn_end_reads_like_claude_code() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let done = TurnEnd::new(
            "Sautéed for",
            Duration::from_secs(2 * 3600 + 53 * 60 + 6),
            ClockTime::new(6, 41),
        );
        let lines = done.lines(&ctx);
        assert_eq!(plain_text(&lines), "✻ Sautéed for 2h 53m 6s · done 6:41 AM");
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|span| span.style == theme.style(Role::Dim) || span.content.trim().is_empty())
        );
        let quick = TurnEnd::new("Rowed for", Duration::from_secs(3), None);
        assert_eq!(plain_text(&quick.lines(&ctx)), "✻ Rowed for 3s");
    }

    #[test]
    fn recap_wraps_under_column_two_in_dim_italics() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let recap = Recap::new("Export now takes --json, all 4 cards merged, and the tests pass.");
        let lines = recap.lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "※ recap: Export now takes --json, all 4\n  cards merged, and the tests pass.\n  (disable recaps in /config)"
        );
        let italic = theme.style(Role::Dim).add_modifier(Modifier::ITALIC);
        assert_eq!(lines[0].spans[2].style, theme.style(Role::Dim));
        let last = lines[1]
            .spans
            .last()
            .map(|s| crate::text::marks::unmarked(s.style));
        assert_eq!(last, Some(italic));
    }

    #[test]
    fn the_narrator_speaks_in_the_same_voice_with_its_own_label() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let line = Recap::narrator("The plan is agreed; three builders start now.");
        assert_eq!(
            plain_text(&line.lines(&ctx)),
            "※ narrator: The plan is agreed; three builders start now."
        );
    }
}
