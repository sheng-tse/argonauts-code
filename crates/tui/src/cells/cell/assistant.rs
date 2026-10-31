use ratatui::text::{Line, Span};

use super::HistoryCell;
use crate::base::draw::render::RenderCtx;
use crate::base::text::marks;
use crate::base::text::str_width;
use crate::base::theme::{Role, Theme};
use crate::cells::markdown::{Indent, render_markdown};

/// Streaming answers go through [`MarkdownStream`](crate::cells::markdown::stream::MarkdownStream)
/// and end up as one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantMessage {
    pub markdown: String,
    /// Shown in the transcript overlay, like `01:07 PM claude-haiku-4-5`. In a crew it names the
    /// agent too.
    pub stamp: Option<String>,
}

impl AssistantMessage {
    pub fn new(markdown: impl Into<String>) -> Self {
        Self {
            markdown: markdown.into(),
            stamp: None,
        }
    }

    pub fn with_stamp(mut self, stamp: impl Into<String>) -> Self {
        self.stamp = Some(stamp.into());
        self
    }

    /// Shared with streamed answers so they render identically.
    pub fn indent(theme: &Theme) -> Indent {
        let bullet = theme.glyphs.bullet;
        Indent::new(
            vec![
                marks::decorative(bullet, theme.style(Role::Bright)),
                Span::raw(" "),
            ],
            vec![Span::raw(" ".repeat(str_width(bullet) + 1))],
        )
    }
}

impl HistoryCell for AssistantMessage {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if ctx.expanded
            && let Some(stamp) = &self.stamp
        {
            lines.push(super::layout::stamp_row(stamp, ctx));
        }
        lines.extend(render_markdown(
            &self.markdown,
            ctx,
            &Self::indent(ctx.theme),
        ));
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::base::draw::render::Clock;
    use crate::base::text::plain_text;
    use crate::base::theme::Glyphs;

    #[test]
    fn text_hangs_at_column_two_under_the_bullet() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let message = AssistantMessage::new(
            "Hi! I'm the main agent (Claude Code, haiku). Ask me about this repo.",
        );
        assert_eq!(
            plain_text(&message.lines(&ctx)),
            "● Hi! I'm the main agent (Claude Code,\n  haiku). Ask me about this repo."
        );
        assert!(AssistantMessage::new("").lines(&ctx).is_empty());
    }
}
