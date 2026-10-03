use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::layout::{self, cells, drawn};
use super::{Composer, ComposerState, ComposerView, MAX_INPUT_ROWS, ViewState};
use crate::render::RenderCtx;
use crate::text::marks;
use crate::text::{sanitize_line, str_width, truncate_line};
use crate::theme::Role;

/// Columns before the text on each input row: `❯ ` or two spaces.
const PROMPT_WIDTH: usize = 2;

/// Shell mode's marker, in place of `❯`.
pub const SHELL_PROMPT: &str = "!";

/// What the empty input says in shell mode, where Enter runs what you type.
pub const SHELL_PLACEHOLDER: &str = "Run a shell command in the repo folder";

pub(super) fn render(
    composer: &Composer,
    ctx: &RenderCtx<'_>,
    state: ComposerState<'_>,
) -> ComposerView {
    let ComposerState { focused, busy, .. } = state;
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let dim = theme.style(Role::Dim);
    let shell = composer.shell;
    let prompt_style = if shell {
        theme.style(Role::Shell)
    } else if focused && !busy {
        theme.style(Role::Text)
    } else {
        dim
    };
    let prompt = if shell {
        SHELL_PROMPT
    } else {
        theme.glyphs.prompt
    };
    // Shell mode draws both rules in its color, as Claude Code's bash mode does.
    let rule_style = marks::decoration(theme.style(if shell { Role::Shell } else { Role::Rule }));
    let text = &composer.draft.text;
    let input_width = width.saturating_sub(PROMPT_WIDTH).max(1);
    let rows = layout::rows(text, input_width);
    let (cursor_row, cursor_col) =
        layout::cursor_position(text, &rows, composer.draft.cursor, input_width);

    let scroll = scroll_for(composer.view.get().scroll, cursor_row, rows.len());
    composer.view.set(ViewState {
        width: input_width,
        scroll,
    });

    let search = composer.search.as_ref().map(|search| {
        let mut label = format!("search: {}", search.query);
        if search.failed {
            label.push_str(" (no match)");
        }
        label
    });
    let selection = composer.visual_range();
    let mut lines = Vec::with_capacity(rows.len().min(MAX_INPUT_ROWS) + 2);
    lines.push(top_rule(
        search.as_deref().or(composer.label.as_deref()),
        rule_style,
        ctx,
    ));
    for (index, row) in rows.iter().enumerate().skip(scroll).take(MAX_INPUT_ROWS) {
        let prefix = if index == 0 {
            marks::decorative(format!("{prompt} "), prompt_style)
        } else {
            Span::raw(" ".repeat(PROMPT_WIDTH))
        };
        let shown = layout::visible(text, *row, input_width);
        let mut spans = vec![prefix];
        let placeholder = if !text.is_empty() {
            None
        } else if shell {
            Some(SHELL_PLACEHOLDER.to_owned())
        } else {
            composer.placeholder_for(state)
        };
        match placeholder {
            Some(placeholder) => spans.extend(placeholder_spans(&placeholder, dim, focused)),
            None if let Some(selection) = &selection => {
                let cursor = (focused && index == cursor_row).then_some(cursor_col);
                spans.extend(selected(shown, row.start, selection, cursor));
            }
            None if focused && index == cursor_row => spans.extend(with_block(shown, cursor_col)),
            None if !shown.is_empty() => spans.push(Span::raw(drawn(shown).into_owned())),
            None => {}
        }
        lines.push(truncate_line(&Line::from(spans), width));
    }
    lines.push(Line::from(Span::styled(
        theme.glyphs.rule.repeat(width),
        rule_style,
    )));

    let column = PROMPT_WIDTH + cursor_col;
    let row = 1 + cursor_row - scroll;
    ComposerView {
        lines,
        cursor: (
            u16::try_from(column).unwrap_or(u16::MAX),
            u16::try_from(row).unwrap_or(u16::MAX),
        ),
    }
}

fn top_rule(label: Option<&str>, dim: Style, ctx: &RenderCtx<'_>) -> Line<'static> {
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let rule = theme.glyphs.rule;
    let label = label
        .map(|label| sanitize_line(label).trim().to_owned())
        .filter(|label| !label.is_empty());
    match label {
        // At least one rule cell on each side, with a space around the label.
        Some(label) if str_width(&label) + 4 <= width => {
            let left = width - str_width(&label) - 3;
            Line::from(vec![
                Span::styled(rule.repeat(left), dim),
                Span::raw(" "),
                Span::styled(label, theme.style(Role::Label)),
                Span::raw(" "),
                Span::styled(rule.to_owned(), dim),
            ])
        }
        _ => Line::from(Span::styled(rule.repeat(width), dim)),
    }
}

/// With focus the block sits on the placeholder's first letter, the way Claude Code draws it.
fn placeholder_spans(placeholder: &str, dim: Style, focused: bool) -> Vec<Span<'static>> {
    if !focused {
        return vec![Span::styled(placeholder.to_owned(), dim)];
    }
    let first = placeholder.graphemes(true).next().unwrap_or(" ");
    vec![
        Span::styled(first.to_owned(), block()),
        Span::styled(placeholder[first.len()..].to_owned(), dim),
    ]
}

fn block() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

/// Past the row's end, the block is a reversed space.
fn with_block(row: &str, column: usize) -> Vec<Span<'static>> {
    let block = block();
    let mut used = 0;
    for (offset, grapheme) in row.grapheme_indices(true) {
        if used == column {
            let after = offset + grapheme.len();
            let mut spans = Vec::with_capacity(3);
            if offset > 0 {
                spans.push(Span::raw(drawn(&row[..offset]).into_owned()));
            }
            spans.push(Span::styled(drawn(grapheme).into_owned(), block));
            if after < row.len() {
                spans.push(Span::raw(drawn(&row[after..]).into_owned()));
            }
            return spans;
        }
        used += cells(grapheme);
    }
    let mut spans = Vec::with_capacity(2);
    if !row.is_empty() {
        spans.push(Span::raw(drawn(row).into_owned()));
    }
    spans.push(Span::styled(" ", block));
    spans
}

/// VISUAL's selection is reversed, with the cursor's cell underlined in it. `start` is the
/// row's byte offset in the draft.
fn selected(
    row: &str,
    start: usize,
    selection: &Range<usize>,
    cursor: Option<usize>,
) -> Vec<Span<'static>> {
    let chosen = block();
    let at_cursor = chosen.add_modifier(Modifier::UNDERLINED);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (offset, grapheme) in row.grapheme_indices(true) {
        let inside = selection.contains(&(start + offset));
        let style = match (inside, cursor == Some(used)) {
            (true, true) => at_cursor,
            (true, false) => chosen,
            (false, true) => block(),
            (false, false) => Style::new(),
        };
        let grapheme = drawn(grapheme);
        used += str_width(&grapheme);
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push_str(&grapheme),
            _ => spans.push(Span::styled(grapheme.into_owned(), style)),
        }
    }
    if cursor.is_some_and(|column| column >= used) {
        spans.push(Span::styled(" ", block()));
    }
    spans
}

/// Moves the last scroll just enough to keep the cursor in view.
fn scroll_for(previous: usize, cursor_row: usize, rows: usize) -> usize {
    let mut scroll = previous.min(rows.saturating_sub(MAX_INPUT_ROWS));
    if cursor_row < scroll {
        scroll = cursor_row;
    } else if cursor_row >= scroll + MAX_INPUT_ROWS {
        scroll = cursor_row + 1 - MAX_INPUT_ROWS;
    }
    scroll
}
