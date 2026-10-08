use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::layout::{BODY_COL, EXPAND_HINT, indent, result_prefix};
use super::tool::body::text_rows;
use super::{HistoryCell, Tone};
use crate::render::RenderCtx;
use crate::text::marks;
use crate::text::{fit_lines, sanitize, sanitize_line, spans_width, str_width, wrap_spans};
use crate::theme::Role;

/// A short message from the app rather than an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub tone: Tone,
    /// Replaces the bullet.
    pub glyph: Option<String>,
    /// Uses the theme's card glyph, `◆`, for a card's record.
    pub card: bool,
    pub text: String,
    /// Who decided and when, like `merged by you, 10:58`.
    pub decided: Option<String>,
    /// Dim lines under `⎿`.
    pub detail: Vec<String>,
    /// Dim lines under `⎿` that only Ctrl+O shows, such as the raw error behind a plain line.
    pub more: Vec<String>,
}

impl Notice {
    pub fn new(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            tone,
            glyph: None,
            card: false,
            text: text.into(),
            decided: None,
            detail: Vec::new(),
            more: Vec::new(),
        }
    }

    /// The `◆` takes the success color, since the card no longer needs you.
    pub fn answered(title: impl Into<String>, decided: impl Into<String>) -> Self {
        Self {
            card: true,
            decided: Some(decided.into()),
            ..Self::new(Tone::Success, title)
        }
    }

    pub fn with_glyph(mut self, glyph: impl Into<String>) -> Self {
        self.glyph = Some(glyph.into());
        self
    }

    pub fn with_detail(mut self, line: impl Into<String>) -> Self {
        self.detail.push(line.into());
        self
    }

    pub fn with_more(mut self, line: impl Into<String>) -> Self {
        self.more.push(line.into());
        self
    }
}

impl HistoryCell for Notice {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        // The ASCII glyph set is for fonts that draw symbols at the wrong width, so a symbol of
        // the caller's own gives way to the bullet there.
        let ascii = theme.glyphs.bullet.is_ascii();
        let glyph = if self.card {
            theme.glyphs.card.to_owned()
        } else {
            self.glyph
                .as_deref()
                .map(|glyph| sanitize_line(glyph).into_owned())
                .filter(|glyph| !glyph.is_empty() && (glyph.is_ascii() || !ascii))
                .unwrap_or_else(|| theme.glyphs.bullet.to_owned())
        };
        let hang = str_width(&glyph) + 1;
        let first = [
            marks::decorative(glyph, theme.style(self.tone.role())),
            Span::raw(" "),
        ];
        let rest = [Span::raw(" ".repeat(hang))];
        let text_style = match self.tone {
            Tone::Dim | Tone::Warning | Tone::Error => theme.style(self.tone.role()),
            _ => theme.style(Role::Text),
        };
        let mut spans = Vec::new();
        // Only color sets a warning or an error apart from an answer, so without it they say so.
        if !theme.has_color() {
            match self.tone {
                Tone::Warning => spans.push(Span::styled("warning: ", text_style)),
                Tone::Error => spans.push(Span::styled(
                    "error: ",
                    text_style.add_modifier(Modifier::BOLD),
                )),
                _ => {}
            }
        }
        spans.push(Span::styled(sanitize(&self.text).into_owned(), text_style));
        if let Some(decided) = &self.decided {
            let decided = sanitize_line(decided);
            if !decided.trim().is_empty() {
                spans.push(Span::styled(
                    format!(" · {}", decided.trim()),
                    theme.style(Role::Dim),
                ));
            }
        }
        let hidden = !ctx.expanded && self.more.iter().any(|line| !line.trim().is_empty());
        if hidden && spans_width(&first) + spans_width(&spans) + str_width(EXPAND_HINT) <= width {
            spans.push(Span::styled(EXPAND_HINT, theme.style(Role::Dim)));
        }
        let mut lines = wrap_spans(&spans, width, &first, &rest);
        let shown = if ctx.expanded {
            self.detail.iter().chain(&self.more).collect::<Vec<_>>()
        } else {
            self.detail.iter().collect()
        };
        for (i, detail) in shown.into_iter().enumerate() {
            let first = if i == 0 {
                result_prefix(theme)
            } else {
                vec![indent(BODY_COL)]
            };
            // A detail set in under the one before, as /doctor's notes under an agent are, hangs
            // under its own words when it wraps.
            let set_in = detail.len() - detail.trim_start_matches(' ').len();
            // The same formatter as a tool's results, so `/resume` takes the code color here too.
            lines.extend(text_rows(detail, Tone::Dim, ctx, first, BODY_COL + set_in));
        }
        fit_lines(&mut lines, width);
        lines
    }
}

/// One line hung under the item before it, with no blank row between, such as
/// `  ⎿  Interrupted · What should Argonauts do instead?` under a stopped answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultLine {
    pub tone: Tone,
    pub text: String,
    /// Drawn after `text` in the accent color, as in Claude Code's `Kept model as Opus 5.5`.
    pub value: Option<String>,
    /// Dim words after the value, such as where a setting was saved.
    pub after: Option<String>,
}

impl ResultLine {
    pub fn new(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            tone,
            text: text.into(),
            value: None,
            after: None,
        }
    }

    /// `text` in the normal color with `value` after it in the accent color.
    pub fn valued(text: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            value: Some(value.into()),
            ..Self::new(Tone::Normal, text)
        }
    }

    pub fn with_after(mut self, after: impl Into<String>) -> Self {
        self.after = Some(after.into());
        self
    }
}

impl HistoryCell for ResultLine {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let width = usize::from(ctx.width);
        let mut lines = match &self.value {
            None => text_rows(
                &self.text,
                self.tone,
                ctx,
                result_prefix(ctx.theme),
                BODY_COL,
            ),
            Some(value) => {
                let theme = ctx.theme;
                let mut spans = vec![
                    Span::styled(
                        format!("{} ", sanitize_line(&self.text).trim_end()),
                        theme.style(self.tone.role()),
                    ),
                    Span::styled(
                        sanitize_line(value).trim().to_owned(),
                        theme.style(Role::Accent),
                    ),
                ];
                if let Some(after) = &self.after {
                    spans.push(Span::styled(
                        format!(" {}", sanitize_line(after).trim()),
                        theme.style(Role::Dim),
                    ));
                }
                wrap_spans(&spans, width, &result_prefix(theme), &[indent(BODY_COL)])
            }
        };
        fit_lines(&mut lines, width);
        lines
    }

    fn continues_previous(&self) -> bool {
        true
    }
}

/// A dim line hung under the result line before it, at its words' column, such as
/// `     Carried over: the last 6 exchanges in full` under `  ⎿  Set model to …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HungLine {
    pub text: String,
}

impl HungLine {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl HistoryCell for HungLine {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let mut lines = text_rows(&self.text, Tone::Dim, ctx, vec![indent(BODY_COL)], BODY_COL);
        fit_lines(&mut lines, usize::from(ctx.width));
        lines
    }

    fn continues_previous(&self) -> bool {
        true
    }
}

/// The lines left in scrollback on exit, like `Reattach with: argonauts`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitNote {
    pub lines: Vec<String>,
}

impl ExitNote {
    pub fn new(lines: Vec<String>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for ExitNote {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let width = usize::from(ctx.width);
        let dim = ctx.theme.style(Role::Dim);
        let mut lines = Vec::new();
        for line in &self.lines {
            let spans = [Span::styled(sanitize_line(line).into_owned(), dim)];
            lines.extend(wrap_spans(&spans, width, &[], &[]));
        }
        fit_lines(&mut lines, width);
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::render::{Clock, RenderCtx};
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    #[test]
    fn a_result_draws_its_value_in_the_accent_color() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let lines = ResultLine::valued("Kept model as", "Opus 5.5").lines(&ctx);
        assert_eq!(plain_text(&lines), "  ⎿  Kept model as Opus 5.5");
        let spans = &lines[0].spans;
        let value = spans
            .iter()
            .find(|span| span.content == "Opus 5.5")
            .unwrap();
        assert_eq!(value.style, theme.style(Role::Accent));
        let words = spans
            .iter()
            .find(|span| span.content.contains("Kept"))
            .unwrap();
        assert_eq!(words.style, theme.style(Role::Text));

        let narrow = RenderCtx::new(20, &theme, Clock::still(Instant::now()));
        for line in ResultLine::valued("Kept model as", "Opus 5.5").lines(&narrow) {
            assert!(line.width() <= 20, "{line:?}");
        }

        let set = ResultLine::valued("Set model to", "Opus 5.5").with_after("· just you");
        let lines = set.lines(&ctx);
        assert_eq!(plain_text(&lines), "  ⎿  Set model to Opus 5.5 · just you");
        let after = lines[0]
            .spans
            .iter()
            .find(|span| span.content.contains("just you"))
            .unwrap();
        assert_eq!(after.style, theme.style(Role::Dim));
        for line in set.lines(&narrow) {
            assert!(line.width() <= 20, "{line:?}");
        }
    }

    #[test]
    fn a_notice_hangs_its_detail_under_the_hook() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let notice = Notice::new(Tone::Warning, "Merged task-002 into main")
            .with_glyph("◆")
            .with_detail("Interrupted · What should Argonauts do instead?")
            .with_detail("second");
        let lines = notice.lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "◆ Merged task-002 into main\n  ⎿  Interrupted · What should Argonauts\n     do instead?\n     second"
        );
        assert_eq!(
            lines[0].spans[0].style,
            marks::decoration(theme.style(Role::Warning))
        );
        // A detail set in under the one before hangs under its own words.
        let doctor = Notice::new(Tone::Normal, "Doctor")
            .with_detail("claude-code 2.1.291 · newer than tested")
            .with_detail("  loaded built-ins it doesn't know yet")
            .lines(&ctx);
        assert_eq!(
            plain_text(&doctor),
            "● Doctor\n  ⎿  claude-code 2.1.291 · newer than\n     tested\n       loaded built-ins it doesn't know\n       yet"
        );
        let plain = Notice::new(Tone::Error, "failed\x1b[31m hard").lines(&ctx);
        assert_eq!(plain_text(&plain), "● failed hard");
        assert_eq!(
            plain[0].spans[0].style,
            marks::decoration(theme.style(Role::Error))
        );
    }

    #[test]
    fn an_answered_card_says_who_decided_and_when() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let lines =
            Notice::answered("Merge task-002 into main?", "merged by you, 10:58").lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "◆ Merge task-002 into main? · merged by you, 10:58"
        );
        assert_eq!(
            lines[0].spans[0].style,
            marks::decoration(theme.style(Role::Success))
        );
        assert_eq!(lines[0].spans[3].style, theme.style(Role::Dim));
    }

    #[test]
    fn without_color_warnings_and_errors_say_so() {
        let mut theme = Theme::dark().with_level(crate::theme::ColorLevel::None);
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let warning = Notice::new(Tone::Warning, "Codex is logged out").lines(&ctx);
        assert_eq!(plain_text(&warning), "● warning: Codex is logged out");
        let error = Notice::new(Tone::Error, "agy stopped").lines(&ctx);
        assert_eq!(plain_text(&error), "● error: agy stopped");
        assert!(
            error[0].spans[2]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn notices_color_their_text_by_tone() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let info = Notice::new(Tone::Dim, "Saved.").lines(&ctx);
        assert_eq!(info[0].spans[2].style, theme.style(Role::Dim));
        let warning = Notice::new(Tone::Warning, "Codex is logged out").lines(&ctx);
        assert_eq!(warning[0].spans[2].style, theme.style(Role::Warning));
        let done = Notice::new(Tone::Success, "Set ui.theme = dark").lines(&ctx);
        assert_eq!(done[0].spans[2].style, theme.style(Role::Text));
    }

    #[test]
    fn a_symbol_gives_way_to_the_bullet_in_ascii() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::ASCII;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let paused = Notice::new(Tone::Warning, "Paused").with_glyph("■");
        let bullet = theme.glyphs.bullet;
        assert_eq!(plain_text(&paused.lines(&ctx)), format!("{bullet} Paused"));
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        assert_eq!(plain_text(&paused.lines(&ctx)), "■ Paused");
    }

    #[test]
    fn a_result_line_hangs_under_the_item_before_it() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let line = ResultLine::new(Tone::Dim, "Interrupted · What should Argonauts do instead?");
        assert!(line.continues_previous());
        assert_eq!(
            plain_text(&line.lines(&ctx)),
            "  ⎿  Interrupted · What should Argonauts\n     do instead?"
        );
    }

    #[test]
    fn exit_lines_are_dim_at_column_zero() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let note = ExitNote::new(vec![
            "run-01 keeps going · 3 agents working · $7.40 spent".into(),
            "Reattach with: argonauts\x1b[2J".into(),
        ]);
        let lines = note.lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "run-01 keeps going · 3 agents working · $7.40 spent\nReattach with: argonauts"
        );
        assert_eq!(lines[1].spans[0].style, theme.style(Role::Dim));
    }
}
