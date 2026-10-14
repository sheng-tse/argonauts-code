use argonauts_core::input::at_words;
use argonauts_core::keywords::Keyword;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::HistoryCell;
use crate::keywords;
use crate::render::RenderCtx;
use crate::text::marks;
use crate::text::{fit_lines, sanitize, spans_width, str_width, wrap_spans};
use crate::theme::{Role, Theme};

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
    /// It runs one of the CLI's own commands, such as `/review` or a skill, so it never stays
    /// at the top as the prompt being answered.
    pub runs_a_command: bool,
    /// The keywords of the CLI the message went to that it draws in its echo too.
    pub keywords: Vec<Keyword>,
    /// The numbers of the pictures sent with it, a `⎿ [Image #N]` row each beneath, as in
    /// Claude Code.
    pub pictures: Vec<u32>,
}

impl UserMessage {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            full: None,
            pending: false,
            command: false,
            runs_a_command: false,
            keywords: Vec::new(),
            pictures: Vec::new(),
        }
    }

    pub fn command(text: impl Into<String>) -> Self {
        Self {
            command: true,
            ..Self::new(text)
        }
    }

    pub fn running_a_command(mut self, runs: bool) -> Self {
        self.runs_a_command = runs;
        self
    }

    pub fn with_full(mut self, full: impl Into<String>) -> Self {
        let full = full.into();
        self.full = (full != self.text).then_some(full);
        self
    }

    /// Keeps those of `keywords` the CLI draws in its echo.
    pub fn with_keywords(mut self, keywords: &[Keyword]) -> Self {
        self.keywords = keywords
            .iter()
            .filter(|keyword| keyword.echoed)
            .cloned()
            .collect();
        self
    }

    pub fn with_pictures(mut self, pictures: impl IntoIterator<Item = u32>) -> Self {
        self.pictures = pictures.into_iter().collect();
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
            marks::decorative(prompt, theme.style(Role::Faint)),
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
        let spans = with_mentions(&text, theme.style(role), theme);
        let letters = keywords::letters(&text, &self.keywords, theme, None);
        let spans = keywords::overlay(spans, &letters);
        let band = theme.style(Role::UserBand);
        let width = usize::from(ctx.width);
        let mut lines = wrap_spans(&spans, width, &first, &rest);
        fit_lines(&mut lines, width);
        let mut lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| {
                if self.command {
                    short_band(line, band, width)
                } else {
                    line.style(band)
                }
            })
            .collect();
        for number in &self.pictures {
            lines.push(Line::from(vec![
                Span::raw("  "),
                marks::decorative(theme.glyphs.result, theme.style(Role::Dim)),
                Span::raw(" "),
                Span::styled(argonauts_core::pictures::label(*number), theme.style(role)),
            ]));
        }
        lines
    }
}

/// Each `@` word, such as `@src/main.rs` or `@codex`, in the color the `@` list picks with,
/// or bold with no color.
fn with_mentions(text: &str, style: Style, theme: &Theme) -> Vec<Span<'static>> {
    let mention = if theme.has_color() {
        theme.style(Role::Card)
    } else {
        style.add_modifier(Modifier::BOLD)
    };
    let mut spans = Vec::new();
    let mut at = 0;
    for word in at_words(text) {
        if word.range.start > at {
            spans.push(Span::styled(text[at..word.range.start].to_owned(), style));
        }
        spans.push(Span::styled(text[word.range.clone()].to_owned(), mention));
        at = word.range.end;
    }
    if at < text.len() || spans.is_empty() {
        spans.push(Span::styled(text[at..].to_owned(), style));
    }
    spans
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
    fn each_picture_sent_has_a_row_beneath_the_prompt() {
        let theme = theme();
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let message = UserMessage::new("what's off on this screen? [Image #1]").with_pictures([1]);
        assert_eq!(
            plain_text(&message.lines(&ctx)),
            "❯ what's off on this screen? [Image #1]\n  ⎿ [Image #1]"
        );
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
        assert_eq!(
            lines[0].spans[0].style,
            marks::decoration(theme.style(Role::Faint))
        );
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
    fn mentions_show_in_the_color_the_list_picks_with() {
        let theme = theme();
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let lines = UserMessage::new("explain @src/main.rs, ask @codex; me@x.example").lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "❯ explain @src/main.rs, ask @codex; me@x.example"
        );
        let colored: Vec<&str> = lines[0]
            .spans
            .iter()
            .filter(|span| span.style.fg == theme.style(Role::Card).fg)
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(colored, ["@src/main.rs", "@codex"]);
        assert_eq!(
            lines[0].spans.last().unwrap().style.fg,
            theme.style(Role::Bright).fg
        );

        let mut plain = Theme::dark().with_level(crate::theme::ColorLevel::None);
        plain.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(60, &plain, Clock::still(Instant::now()));
        let lines = UserMessage::new("see @notes.txt").lines(&ctx);
        let bold: Vec<&str> = lines[0]
            .spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(bold, ["@notes.txt"]);
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

    #[test]
    fn the_echo_draws_the_clis_echoed_keywords_held_still() {
        use argonauts_core::keywords::KeywordLook;

        let theme = theme();
        let ctx = RenderCtx::new(60, &theme, Clock::new(Instant::now(), Instant::now()));
        let echoed = Keyword::new("ultrathink", KeywordLook::Rainbow, true);
        let input_only = Keyword::new("about", KeywordLook::Rainbow, false);
        let message = UserMessage::new("please ultrathink about this @codex")
            .with_keywords(&[echoed.clone(), input_only]);
        assert_eq!(message.keywords, [echoed]);
        let lines = message.lines(&ctx);
        assert_eq!(plain_text(&lines), "❯ please ultrathink about this @codex");
        let spans = &lines[0].spans;
        let bright = theme.style(Role::Bright);
        assert_eq!(spans[2], Span::styled("please ", bright));
        assert_eq!(
            spans[3],
            Span::styled("u", bright.patch(theme.rainbow(0, false)))
        );
        assert_eq!(
            spans[12],
            Span::styled("k", bright.patch(theme.rainbow(9, false)))
        );
        assert_eq!(spans[13], Span::styled(" about this ", bright));
        assert_eq!(spans[14].content, "@codex");
    }
}
