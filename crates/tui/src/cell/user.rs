use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::HistoryCell;
use crate::render::RenderCtx;
use crate::text::{fit_lines, sanitize, spans_width, str_width, wrap_spans};
use crate::theme::Role;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessage {
    /// Long pastes stay as their labels, like `[Pasted text #1 +39 lines]`.
    pub text: String,
    /// With pastes expanded, for the transcript overlay.
    pub full: Option<String>,
    /// Sent, but the request hasn't started, so the text is grey. A pending message belongs in the
    /// live region ([`History::push_active`](crate::History::push_active)) and prints once started.
    pub pending: bool,
    /// The echo of a slash command you ran, whose band ends one cell past the text, as Claude
    /// Code echoes `/model`. A message's band fills the row.
    pub command: bool,
}

impl UserMessage {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            full: None,
            pending: false,
            command: false,
        }
    }

    pub fn command(text: impl Into<String>) -> Self {
        Self {
            command: true,
            ..Self::new(text)
        }
    }

    pub fn with_full(mut self, full: impl Into<String>) -> Self {
        let full = full.into();
        self.full = (full != self.text).then_some(full);
        self
    }

    pub fn pending(text: impl Into<String>) -> Self {
        Self {
            pending: true,
            ..Self::new(text)
        }
    }

    pub fn start(&mut self) {
        self.pending = false;
    }
}

impl HistoryCell for UserMessage {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let prompt = theme.glyphs.prompt;
        let first = [
            Span::styled(prompt, theme.style(Role::Faint)),
            Span::raw(" "),
        ];
        let rest = [Span::raw(" ".repeat(str_width(prompt) + 1))];
        let text = match &self.full {
            Some(full) if ctx.expanded => full,
            _ => &self.text,
        };
        let text = sanitize(text.trim_end_matches('\n'));
        let role = if self.pending {
            Role::Dim
        } else {
            Role::Bright
        };
        let spans = [Span::styled(text.into_owned(), theme.style(role))];
        let band = theme.style(Role::UserBand);
        let width = usize::from(ctx.width);
        let mut lines = wrap_spans(&spans, width, &first, &rest);
        fit_lines(&mut lines, width);
        lines
            .into_iter()
            .map(|line| {
                if self.command {
                    short_band(line, band, width)
                } else {
                    line.style(band)
                }
            })
            .collect()
    }
}

/// The band under the row's text and one cell past it, rather than across the whole row.
fn short_band(line: Line<'static>, band: Style, width: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| Span::styled(span.content, band.patch(span.style)))
        .collect();
    if spans_width(&spans) < width {
        spans.push(Span::styled(" ", band));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::style::Color;

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
    fn a_long_paste_echoes_as_its_label_and_expands_in_the_overlay() {
        let theme = theme();
        let mut ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let message = UserMessage::new("see [Pasted text #1 +2 lines]").with_full("see a\nb\nc");
        assert_eq!(
            plain_text(&message.lines(&ctx)),
            "❯ see [Pasted text #1 +2 lines]"
        );
        ctx.expanded = true;
        assert_eq!(plain_text(&message.lines(&ctx)), "❯ see a\n  b\n  c");
        assert_eq!(UserMessage::new("hi").with_full("hi").full, None);
    }

    #[test]
    fn every_row_sits_on_the_band_with_a_hanging_indent() {
        let theme = theme();
        let ctx = RenderCtx::new(20, &theme, Clock::still(Instant::now()));
        let lines = UserMessage::new("zsh: command not found: argonauts").lines(&ctx);
        assert_eq!(plain_text(&lines), "❯ zsh: command not\n  found: argonauts");
        for line in &lines {
            assert_eq!(line.style.bg, theme.style(Role::UserBand).bg);
            assert!(line.style.bg.is_some_and(|bg| bg != Color::Reset));
        }
        assert_eq!(lines[0].spans[0].style, theme.style(Role::Faint));
        assert_eq!(lines[0].spans[2].style, theme.style(Role::Bright));
    }

    #[test]
    fn a_command_echo_has_its_band_end_one_cell_past_the_text() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let lines = UserMessage::command("/model").lines(&ctx);
        assert_eq!(plain_text(&lines), "❯ /model");
        let [line] = lines.as_slice() else {
            panic!("one row");
        };
        assert_eq!(line.style, Style::default());
        assert_eq!(spans_width(&line.spans), 9);
        let band = theme.style(Role::UserBand).bg;
        assert!(band.is_some());
        assert!(line.spans.iter().all(|span| span.style.bg == band));
        assert_eq!(line.spans[0].style.fg, theme.style(Role::Faint).fg);
        assert_eq!(line.spans[2].style.fg, theme.style(Role::Bright).fg);
    }

    #[test]
    fn a_pending_message_is_grey_until_it_starts() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut message = UserMessage::pending("hi");
        assert_eq!(
            message.lines(&ctx)[0].spans[2].style,
            theme.style(Role::Dim)
        );
        message.start();
        assert_eq!(
            message.lines(&ctx)[0].spans[2].style,
            theme.style(Role::Bright)
        );
    }

    #[test]
    fn empty_text_still_gives_one_band_row() {
        let theme = theme();
        let ctx = RenderCtx::new(20, &theme, Clock::still(Instant::now()));
        let lines = UserMessage::new("").lines(&ctx);
        assert_eq!(lines.len(), 1);
        assert_eq!(plain_text(&lines), "❯");
        assert_eq!(lines[0].style, theme.style(Role::UserBand));
    }

    #[test]
    fn escapes_never_reach_the_screen() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let lines = UserMessage::new("hi\x1b[2J\x1b]0;pwned\x07 there").lines(&ctx);
        assert_eq!(plain_text(&lines), "❯ hi there");
    }
}
