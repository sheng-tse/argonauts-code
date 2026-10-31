use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::base::text::marks;
use crate::base::text::str_width;
use crate::base::theme::{Role, Theme};

pub(crate) const TEXT_COL: usize = 2;
pub(crate) const BODY_COL: usize = 5;
/// Extra indent for a nested call's own body.
pub(crate) const NEST: usize = 3;

/// Spaces that stay outside a row's fill, so a diff row's color starts at its gutter.
pub(crate) fn indent(cols: usize) -> Span<'static> {
    Span::styled(" ".repeat(cols), Style::new().bg(Color::Reset))
}

/// `  ⎿  `, [`BODY_COL`] columns wide.
pub(crate) fn result_prefix(theme: &Theme) -> Vec<Span<'static>> {
    let open = Style::new().bg(Color::Reset);
    let hook = theme.glyphs.result;
    let after = BODY_COL.saturating_sub(2 + str_width(hook)).max(1);
    vec![
        Span::styled("  ", open),
        marks::decorative(hook, theme.style(Role::Dim).bg(Color::Reset)),
        Span::styled(" ".repeat(after), open),
    ]
}

pub(crate) fn more_lines(theme: &Theme, hidden: usize) -> String {
    let unit = if hidden == 1 { "line" } else { "lines" };
    format!("{} +{hidden} {unit}", theme.glyphs.ellipsis)
}

/// Kept free right of diff fills and stamps, as in Claude Code's transcript (seven at 110 columns).
pub(crate) fn right_margin(width: usize) -> usize {
    7.min(width / 10)
}

/// The overlay's `01:15 PM claude-haiku-4-5` above each agent message, ending where diff fills do.
pub(crate) fn stamp_row(
    stamp: &str,
    ctx: &crate::base::draw::render::RenderCtx<'_>,
) -> Line<'static> {
    let width = usize::from(ctx.width);
    let stamp = crate::base::text::sanitize_line(stamp).trim().to_owned();
    let end = width - right_margin(width);
    let pad = end.saturating_sub(str_width(&stamp));
    let line = Line::from(vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(stamp, ctx.theme.style(Role::Dim)),
    ]);
    crate::base::text::truncate_line(&line, width)
}

pub(crate) const EXPAND_HINT: &str = " (ctrl+o to expand)";

/// A narrow row drops the expand hint rather than cut it.
pub(crate) fn more_row(
    opening: Vec<Span<'static>>,
    theme: &Theme,
    hidden: usize,
    expandable: bool,
    width: usize,
) -> Line<'static> {
    let mut text = more_lines(theme, hidden);
    let used = opening.iter().map(Span::width).sum::<usize>();
    if expandable && used + str_width(&text) + str_width(EXPAND_HINT) <= width {
        text.push_str(EXPAND_HINT);
    }
    let mut row = opening;
    row.push(Span::styled(text, theme.style(Role::Dim)));
    Line::from(row)
}

pub(crate) fn starts_with_indent(row: &Line<'_>, cols: usize) -> bool {
    let mut seen = 0;
    for span in &row.spans {
        if seen >= cols {
            return true;
        }
        if !span.content.bytes().all(|b| b == b' ') {
            return false;
        }
        seen += span.content.len();
    }
    seen >= cols
}

/// Hangs a body row drawn at [`BODY_COL`] on the `⎿` hook. A block has one hook, on its first
/// body row, as each Claude Code result does.
pub(crate) fn hook(row: &mut Line<'static>, theme: &Theme) {
    if starts_with_indent(row, BODY_COL) {
        replace_indent(row, BODY_COL, result_prefix(theme));
    }
}

/// Hangs a row drawn with a fixed indent under the `⎿` by swapping its first `cols` columns
/// for `prefix`.
pub(crate) fn replace_indent(row: &mut Line<'static>, cols: usize, prefix: Vec<Span<'static>>) {
    let mut removed = 0;
    while removed < cols {
        let Some(first) = row.spans.first_mut() else {
            break;
        };
        if !first.content.bytes().all(|b| b == b' ') {
            break;
        }
        let width = first.content.len();
        if removed + width <= cols {
            removed += width;
            row.spans.remove(0);
        } else {
            first.content = " ".repeat(removed + width - cols).into();
            removed = cols;
        }
    }
    // A row cut to a tiny width may have lost its indent; it keeps its own look then.
    if removed == cols {
        let mut spans = prefix;
        spans.append(&mut row.spans);
        row.spans = spans;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::text::{plain_text, spans_width};
    use crate::base::theme::Glyphs;

    #[test]
    fn the_hook_takes_the_body_column() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let prefix = result_prefix(&theme);
        assert_eq!(spans_width(&prefix), BODY_COL);
        assert_eq!(plain_text(&[Line::from(prefix)]), "  ⎿");
        assert_eq!(more_lines(&theme, 1), "… +1 line");
        assert_eq!(more_lines(&theme, 4), "… +4 lines");
    }

    #[test]
    fn replacing_an_indent_keeps_the_rest_of_the_row() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let mut row = Line::from(vec![indent(8), Span::raw("42 + x")]);
        replace_indent(&mut row, BODY_COL, result_prefix(&theme));
        assert_eq!(plain_text(&[row]), "  ⎿     42 + x");
    }
}
