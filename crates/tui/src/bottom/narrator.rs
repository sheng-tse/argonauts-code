//! The narrator's latest line in the live region, `※ narrator: …` in dim italics with a
//! hanging indent, never a box. Only with `ui.narrator = "live"`.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::render::RenderCtx;
use crate::text::{fit_lines, sanitize_line, str_width, wrap_spans};
use crate::theme::Role;

/// A long narration keeps the live region from growing past this many rows.
pub const NARRATOR_MAX_ROWS: usize = 3;

pub fn narrator_lines(text: Option<&str>, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    let Some(text) = text
        .map(sanitize_line)
        .filter(|text| !text.trim().is_empty())
    else {
        return Vec::new();
    };
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let dim = theme.style(Role::Dim);
    let italic = dim.add_modifier(Modifier::ITALIC);
    let glyph = theme.glyphs.recap;
    let spans = [
        Span::styled("narrator: ", dim),
        Span::styled(text.trim().to_owned(), italic),
    ];
    let first = [Span::styled(glyph, dim), Span::raw(" ")];
    let rest = [Span::raw(" ".repeat(str_width(glyph) + 1))];
    let mut lines = wrap_spans(&spans, width, &first, &rest);
    if lines.len() > NARRATOR_MAX_ROWS {
        lines.truncate(NARRATOR_MAX_ROWS);
        if let Some(last) = lines.last_mut() {
            // A full row gets cut back to make room, and still ends with an ellipsis.
            last.spans.push(Span::styled(theme.glyphs.ellipsis, italic));
        }
    }
    fit_lines(&mut lines, width);
    lines
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
    fn nothing_without_a_line() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        assert!(narrator_lines(None, &ctx).is_empty());
        assert!(narrator_lines(Some("  "), &ctx).is_empty());
    }

    #[test]
    fn wraps_under_its_label_in_dim_italics() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let lines = narrator_lines(
            Some("task-002 fell back to Codex while Claude waits for its reset."),
            &ctx,
        );
        assert_eq!(
            plain_text(&lines),
            "※ narrator: task-002 fell back to Codex\n  while Claude waits for its reset."
        );
        let text = lines[0]
            .spans
            .iter()
            .find(|span| span.content.contains("Codex"))
            .unwrap();
        assert!(text.style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(text.style.fg, theme.style(Role::Dim).fg);
    }

    #[test]
    fn a_long_line_stops_at_three_rows() {
        let theme = theme();
        let ctx = RenderCtx::new(30, &theme, Clock::still(Instant::now()));
        let long = "word ".repeat(40);
        let lines = narrator_lines(Some(&long), &ctx);
        assert_eq!(lines.len(), NARRATOR_MAX_ROWS);
        assert!(plain_text(&lines).ends_with('…'));
        assert!(lines.iter().all(|line| line.width() <= 30));
    }
}
