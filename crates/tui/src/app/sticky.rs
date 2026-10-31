//! Scrolled up past one of your prompts, the conversation keeps the prompt being answered in
//! sight: one dim row at the top, `❯ ` and its first paragraph cut with `…`, over a faint rule.
//! Clicking it or Ctrl+↑ jumps to where that prompt starts, and it goes back at the bottom.
//! Claude Code shows it in its full-screen view, which is the screen Argonauts Code always draws.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::base::draw::render::RenderCtx;
use crate::base::text::{marks, str_width};
use crate::base::theme::Role;

/// The prompt's row and the rule under it.
pub(super) const ROWS: usize = 2;

/// How much of a prompt is read for its row, as Claude Code reads it.
const LONGEST: usize = 500;

/// The prompt's first paragraph on one line, or `None` when it has no words.
pub(super) fn words(prompt: &str) -> Option<String> {
    let prompt = prompt.trim_start();
    let first = first_paragraph(prompt);
    let first: String = first.chars().take(LONGEST).collect();
    let words = first.split_whitespace().collect::<Vec<_>>().join(" ");
    (!words.is_empty()).then_some(words)
}

fn first_paragraph(text: &str) -> &str {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() && at > 0 {
            return &text[..at];
        }
        at += line.len();
    }
    text
}

/// `❯ Make the rate limiter per API key: …`, dim on your message's band across the row, cut a
/// cell short of the edge, as Claude Code 2.1.295 draws its own.
pub(super) fn prompt_row(words: &str, ctx: &RenderCtx<'_>) -> Line<'static> {
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let glyph = theme.glyphs.prompt;
    let margin = str_width(glyph) + 1;
    let room = width.saturating_sub(margin + 1);
    let dim = theme.style(Role::Dim);
    Line::from(vec![
        marks::decorative(glyph, dim),
        Span::raw(" "),
        Span::styled(cut(words, room, theme.glyphs.ellipsis), dim),
    ])
    .style(theme.style(Role::UserBand))
}

/// `text` in `room` cells, cut where it no longer fits and marked with `ellipsis`, with no
/// space left before the mark.
fn cut(text: &str, room: usize, ellipsis: &str) -> String {
    if str_width(text) <= room {
        return text.to_owned();
    }
    let limit = room.saturating_sub(str_width(ellipsis));
    let mut kept = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = str_width(grapheme);
        if used + w > limit {
            break;
        }
        used += w;
        kept.push_str(grapheme);
    }
    format!("{}{ellipsis}", kept.trim_end())
}

/// The faint rule under the prompt's row, across the screen.
pub(super) fn rule(ctx: &RenderCtx<'_>) -> Line<'static> {
    let rule = ctx.theme.glyphs.rule.repeat(usize::from(ctx.width));
    Line::from(marks::decorative(rule, ctx.theme.style(Role::Faint)))
}

/// The scrolled view's last row while a prompt stays at the top: how to get back to it.
pub(super) fn hint(ctx: &RenderCtx<'_>) -> Line<'static> {
    let theme = ctx.theme;
    let dim = theme.style(Role::Dim);
    let up = if theme.glyphs.tokens == "↓" {
        "↑"
    } else {
        "up"
    };
    let line = Line::from(vec![
        Span::styled("  click the line at the top, or press ", dim),
        Span::styled(
            format!("ctrl+{up}"),
            theme.style(Role::Text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(", to jump to where that prompt starts", dim),
    ]);
    crate::base::text::truncate_line(&line, usize::from(ctx.width))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::app::Scrollback;
    use crate::base::draw::render::Clock;
    use crate::base::theme::{Glyphs, Theme};
    use crate::cells::cell::UserMessage;
    use crate::cells::cell::history::History;

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    /// Two prompts, each with twenty rows of reply under it.
    fn conversation(ctx: &RenderCtx<'_>) -> Scrollback {
        let mut scrollback = Scrollback::new();
        let mut history = History::default();
        for (prompt, reply) in [
            ("first prompt", "first reply"),
            ("second prompt", "second reply"),
        ] {
            history.push(UserMessage::new(prompt));
            let mut command = UserMessage::new("/model");
            command.command = true;
            history.push(command);
            let rows = history.drain_printable(ctx);
            scrollback.mark_prompts(history.take_prompt_rows());
            scrollback.push(rows, None);
            let rows = (1..=20)
                .map(|n| Line::from(format!("{reply} {n}")))
                .collect();
            scrollback.push(rows, None);
        }
        scrollback
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect()
    }

    #[test]
    fn the_prompt_being_answered_stays_at_the_top_until_the_bottom() {
        let theme = theme();
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let mut scrollback = conversation(&ctx);
        let at_bottom = text(&scrollback.window(&[], 10, &ctx));
        assert!(!at_bottom[0].starts_with('❯'), "{at_bottom:#?}");
        assert!(!scrollback.holds_prompt(0));

        scrollback.scroll(5);
        let up = text(&scrollback.window(&[], 10, &ctx));
        assert_eq!(up[0], "❯ second prompt", "{up:#?}");
        assert!(up[1].starts_with('─'), "{up:#?}");
        assert!(up[9].contains("or press ctrl+↑"), "{up:#?}");
        assert!(scrollback.holds_prompt(0));
        assert!(!scrollback.holds_prompt(1));

        // Further up, the first prompt's reply shows, and so does that prompt.
        scrollback.scroll(20);
        let further = text(&scrollback.window(&[], 10, &ctx));
        assert_eq!(further[0], "❯ first prompt", "{further:#?}");

        // A jump starts the view where that prompt starts, and nothing covers it then.
        assert!(scrollback.jump_to_prompt());
        let jumped = text(&scrollback.window(&[], 10, &ctx));
        assert_eq!(jumped[0], "❯ first prompt", "{jumped:#?}");
        assert_eq!(jumped[1], "", "{jumped:#?}");
        assert!(!scrollback.holds_prompt(0));
        assert!(
            !scrollback.jump_to_prompt(),
            "nothing kept at the top to jump to"
        );

        scrollback.follow();
        let back = text(&scrollback.window(&[], 10, &ctx));
        assert!(!back[0].starts_with('❯'), "{back:#?}");
    }

    #[test]
    fn a_command_s_echo_is_never_kept_at_the_top() {
        let theme = theme();
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let mut history = History::default();
        let mut command = UserMessage::new("/usage");
        command.command = true;
        history.push(command);
        history.push(UserMessage::new("fix it"));
        let rows = history.drain_printable(&ctx);
        let prompts = history.take_prompt_rows();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].1, "fix it");
        assert_eq!(rows[prompts[0].0.start].to_string().trim_end(), "❯ fix it");
    }

    #[test]
    fn a_prompt_is_read_by_its_first_paragraph_on_one_line() {
        assert_eq!(
            words("  Fix the  rounding\nin money.py\n\nThen run the tests.").as_deref(),
            Some("Fix the rounding in money.py")
        );
        assert_eq!(words(" \n "), None);
        let long = "word ".repeat(200);
        assert!(words(&long).is_some_and(|words| words.chars().count() <= LONGEST));
    }

    #[test]
    fn a_cut_leaves_no_space_before_its_mark() {
        assert_eq!(cut("share one. Cap the burst", 16, "…"), "share one. Cap…");
        assert_eq!(cut("short", 16, "…"), "short");
    }
}
