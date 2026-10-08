use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::layout::{self, cells, drawn};
use super::{Composer, ComposerState, ComposerView, MAX_INPUT_ROWS, ViewState};
use crate::keywords::{self, Letter};
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

    let selection = composer.visual_range();
    let letters = if shell {
        Vec::new()
    } else {
        keywords::letters(text, &composer.drawn_keywords(), theme, Some(&ctx.clock))
    };
    let mut lines = Vec::with_capacity(rows.len().min(MAX_INPUT_ROWS) + 2);
    lines.push(top_rule(
        composer.label.as_deref(),
        composer.ultracode,
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
            None => {
                let cursor = (focused && index == cursor_row).then_some(cursor_col);
                let row = Painted {
                    text: shown,
                    start: row.start,
                    letters: &letters,
                };
                match &selection {
                    Some(selection) => spans.extend(selected(&row, selection, cursor)),
                    None => spans.extend(unselected(&row, cursor)),
                }
            }
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

/// The word on the rule while ultracode is on, where Claude Code draws it.
pub const ULTRACODE_TAG: &str = "ultracode";

/// The rule above the input. Its label is the main model's name, with the workflow's first
/// when it isn't solo, in the rule's own grey. Purple is kept for the `ultracode` tag, at the
/// far right, two spaces after the name as Claude Code joins the tags in this slot. On a row
/// too narrow for both, the name goes first.
fn top_rule(
    label: Option<&str>,
    ultracode: bool,
    dim: Style,
    ctx: &RenderCtx<'_>,
) -> Line<'static> {
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let rule = theme.glyphs.rule;
    let label = label
        .map(|label| sanitize_line(label).trim().to_owned())
        .filter(|label| !label.is_empty());
    let tag = ultracode.then(|| Span::styled(ULTRACODE_TAG, theme.style(Role::Ultracode)));
    // At least one rule cell on each side, with a space around the words.
    let fits = |words: usize| words + 4 <= width;
    let name = |label: String| Span::styled(label, dim);
    let words = match (label, tag) {
        (Some(label), Some(tag)) if fits(str_width(&label) + 2 + ULTRACODE_TAG.len()) => {
            vec![name(label), Span::raw("  "), tag]
        }
        (_, Some(tag)) if fits(ULTRACODE_TAG.len()) => vec![tag],
        (Some(label), None) if fits(str_width(&label)) => vec![name(label)],
        _ => return Line::from(Span::styled(rule.repeat(width), dim)),
    };
    let left = width - crate::text::spans_width(&words) - 3;
    let mut spans = vec![Span::styled(rule.repeat(left), dim), Span::raw(" ")];
    spans.extend(words);
    spans.extend([Span::raw(" "), Span::styled(rule.to_owned(), dim)]);
    Line::from(spans)
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

/// One input row, with the keyword letters the draft has.
struct Painted<'a> {
    text: &'a str,
    /// The row's byte offset in the draft.
    start: usize,
    letters: &'a [Letter],
}

impl Painted<'_> {
    fn style_at(&self, offset: usize) -> Style {
        keywords::style_at(self.letters, self.start + offset).unwrap_or_default()
    }
}

/// The row in runs of one style, each drawn whole. The cursor's cell is a block of its own, and
/// past the row's end the block is a reversed space.
fn unselected(row: &Painted<'_>, cursor: Option<usize>) -> Vec<Span<'static>> {
    let text = row.text;
    let mut spans = Vec::with_capacity(3);
    let mut push = |range: Range<usize>, style: Style| {
        if !range.is_empty() {
            spans.push(Span::styled(drawn(&text[range]).into_owned(), style));
        }
    };
    let mut run: Option<(usize, Style)> = None;
    let mut cursor = cursor;
    let mut used = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let style = row.style_at(offset);
        if cursor == Some(used) {
            cursor = None;
            if let Some((from, style)) = run.take() {
                push(from..offset, style);
            }
            push(offset..offset + grapheme.len(), style.patch(block()));
        } else {
            match run {
                Some((_, current)) if current == style => {}
                _ => {
                    if let Some((from, style)) = run {
                        push(from..offset, style);
                    }
                    run = Some((offset, style));
                }
            }
        }
        used += cells(grapheme);
    }
    if let Some((from, style)) = run {
        push(from..text.len(), style);
    }
    if cursor.is_some() {
        spans.push(Span::styled(" ", block()));
    }
    spans
}

/// VISUAL's selection is reversed, with the cursor's cell underlined in it.
fn selected(
    row: &Painted<'_>,
    selection: &Range<usize>,
    cursor: Option<usize>,
) -> Vec<Span<'static>> {
    let chosen = block();
    let at_cursor = chosen.add_modifier(Modifier::UNDERLINED);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (offset, grapheme) in row.text.grapheme_indices(true) {
        let inside = selection.contains(&(row.start + offset));
        let style = match (inside, cursor == Some(used)) {
            (true, true) => at_cursor,
            (true, false) => chosen,
            (false, true) => row.style_at(offset).patch(block()),
            (false, false) => row.style_at(offset),
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
