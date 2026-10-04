//! Every item renders at its column, and the block hangs only its first row on the `  ⎿  `
//! hook. The rest, a nested call's results among them, go without one, as in Claude Code.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Todo, TodoState, ToolCall, ToolItem, ToolState, ToolTitle};
use crate::cell::Tone;
use crate::cell::layout::{BODY_COL, NEST, hook, indent, more_row};
use crate::markdown::{Indent, render_markdown};
use crate::render::RenderCtx;
use crate::text::{sanitize, sanitize_line, str_width, truncate_spans, wrap_spans};
use crate::theme::{Role, Theme};

const OUTPUT_LINES: usize = 5;
const COMMAND_LINES: usize = 10;

pub(super) fn rows(call: &ToolCall, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    let narrowed;
    let ctx = match call.wrap_at {
        Some(columns) if columns < ctx.width => {
            narrowed = RenderCtx {
                width: columns,
                ..*ctx
            };
            &narrowed
        }
        _ => ctx,
    };
    let output = output_role(call);
    let mut rows: Vec<Line<'static>> = call
        .body
        .iter()
        .flat_map(|item| item_rows(item, ctx, BODY_COL, output))
        .collect();
    if let Some(row) = rows.first_mut() {
        hook(row, ctx.theme);
    }
    rows
}

/// A failed call's output reads as its error, as Claude Code draws stderr under
/// `Error: Exit code 1`.
pub(super) fn output_role(call: &ToolCall) -> Role {
    if call.state == ToolState::Failed {
        Role::Error
    } else {
        Role::Dim
    }
}

/// `output` colors a command's output lines.
pub(in crate::cell) fn item_rows(
    item: &ToolItem,
    ctx: &RenderCtx<'_>,
    col: usize,
    output: Role,
) -> Vec<Line<'static>> {
    match item {
        ToolItem::Text { text, tone } => text_rows(text, *tone, ctx, vec![indent(col)], col),
        ToolItem::Hanging { lead, text } => hanging_rows(lead, text, ctx, col),
        ToolItem::Command {
            command,
            output: lines,
            hidden,
        } => command_rows(command, lines, *hidden, output, ctx, col),
        ToolItem::Diff(view) => {
            let mut head = vec![indent(col)];
            head.extend(view.summary(ctx.theme));
            let mut rows = vec![Line::from(head)];
            rows.extend(view.render(ctx, col as u16));
            rows
        }
        ToolItem::Markdown(markdown) => render_markdown(
            markdown,
            ctx,
            &Indent::new(vec![indent(col)], vec![indent(col)]),
        ),
        ToolItem::Call(call) => nested_rows(call, ctx, col),
        // In the transcript overlay each call shows whole.
        ToolItem::Group(group) if ctx.expanded => group
            .calls
            .iter()
            .flat_map(|call| nested_rows(call, ctx, col))
            .collect(),
        ToolItem::Group(group) => group.render(ctx, col as u16),
        ToolItem::Todos(todos) => todo_rows(todos, ctx, col),
        ToolItem::TodoLine(todos) => vec![checklist_row(todos, ctx, col)],
        ToolItem::Streaming(text) => streaming_rows(text, ctx, col),
        ToolItem::Running { since, background } => running_rows(*since, *background, ctx, col),
    }
}

fn running_rows(
    since: std::time::Instant,
    background: bool,
    ctx: &RenderCtx<'_>,
    col: usize,
) -> Vec<Line<'static>> {
    let dim = ctx.theme.style(Role::Dim);
    let elapsed = ctx.clock.now.saturating_duration_since(since);
    let text = format!(
        "Running{} ({})",
        ctx.theme.glyphs.ellipsis,
        crate::format::duration(elapsed)
    );
    let width = usize::from(ctx.width);
    let lead = [indent(col)];
    let mut rows = wrap_spans(&[Span::styled(text, dim)], width, &lead, &lead);
    if background {
        let hint = [Span::styled("(ctrl+b to run in background)", dim)];
        rows.extend(wrap_spans(&hint, width, &lead, &lead));
    }
    rows
}

/// A call inside a block, at `col`, with its results [`NEST`] columns further in.
pub(in crate::cell) fn nested_rows(
    call: &ToolCall,
    ctx: &RenderCtx<'_>,
    col: usize,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let role = match call.state {
        ToolState::Running => Role::Dim,
        ToolState::Stopped => Role::Warning,
        ToolState::Failed => Role::Error,
        ToolState::Done => Role::Text,
    };
    let mut head = vec![indent(col)];
    let mut content = Vec::new();
    let hang = match &call.title {
        ToolTitle::Call { name, args } => {
            let name = sanitize_line(name).into_owned();
            let hang = col + str_width(&name);
            head.push(Span::styled(
                name,
                theme.style(role).add_modifier(Modifier::BOLD),
            ));
            let args = sanitize_line(args);
            if !args.is_empty() {
                content.push(Span::styled(format!("({args})"), theme.style(Role::Text)));
            }
            hang
        }
        title @ (ToolTitle::Text(_) | ToolTitle::Agent { .. }) => {
            content.push(Span::styled(
                sanitize_line(&title.plain()).into_owned(),
                theme.style(role),
            ));
            col
        }
    };
    content.extend(call.failed_word(ctx));
    content.extend(call.stats.span(ctx));
    let mut rows = wrap_spans(&content, width, &head, &[indent(hang)]);
    let output = output_role(call);
    for item in &call.body {
        rows.extend(item_rows(item, ctx, col + NEST, output));
    }
    rows
}

/// Notice details use this too, so every `⎿` line reads the same.
pub(in crate::cell) fn text_rows(
    text: &str,
    tone: Tone,
    ctx: &RenderCtx<'_>,
    first: Vec<Span<'static>>,
    col: usize,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let spans = slash_commands(
        &sanitize(text),
        theme.style(tone.role()),
        theme.style(Role::Code),
    );
    wrap_spans(&spans, usize::from(ctx.width), &first, &[indent(col)])
}

fn hanging_rows(lead: &str, text: &str, ctx: &RenderCtx<'_>, col: usize) -> Vec<Line<'static>> {
    let dim = ctx.theme.style(Role::Dim);
    let lead = sanitize_line(lead).into_owned();
    let under = col + str_width(&lead);
    let first = [indent(col), Span::styled(lead, dim)];
    let spans = [Span::styled(sanitize_line(text).into_owned(), dim)];
    wrap_spans(&spans, usize::from(ctx.width), &first, &[indent(under)])
}

fn command_rows(
    command: &str,
    output: &[String],
    hidden: usize,
    output_role: Role,
    ctx: &RenderCtx<'_>,
    col: usize,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let dim = theme.style(Role::Dim);
    let lead = [indent(col)];
    let mut rows = Vec::new();

    let command = sanitize(command);
    let command_lines: Vec<&str> = command.trim_end_matches('\n').lines().collect();
    let shown = if ctx.expanded {
        command_lines.len()
    } else {
        command_lines.len().min(COMMAND_LINES)
    };
    for (i, line) in command_lines.iter().take(shown).enumerate() {
        let mut line = (*line).to_owned();
        if i + 1 == shown && shown < command_lines.len() {
            line.push_str(theme.glyphs.ellipsis);
        }
        let spans = [Span::styled(line, dim)];
        if i == 0 {
            let head = [indent(col), Span::styled("$ ", dim)];
            rows.extend(wrap_spans(&spans, width, &head, &[indent(col + 2)]));
        } else {
            rows.extend(wrap_spans(&spans, width, &lead, &lead));
        }
    }

    let output: Vec<String> = output
        .iter()
        .flat_map(|chunk| {
            let clean = sanitize(chunk);
            clean.lines().map(str::to_owned).collect::<Vec<_>>()
        })
        .collect();
    let kept = if ctx.expanded {
        output.len()
    } else {
        output.len().min(OUTPUT_LINES)
    };
    let text = theme.style(output_role);
    for line in &output[..kept] {
        let spans = [Span::styled(line.clone(), text)];
        rows.extend(wrap_spans(&spans, width, &lead, &lead));
    }
    let cut = output.len() - kept + hidden;
    if cut > 0 {
        // In the overlay this counts only lines the host never kept, so there's nothing to expand.
        rows.push(more_row(lead.to_vec(), theme, cut, !ctx.expanded, width));
    }
    rows
}

fn todo_rows(todos: &[Todo], ctx: &RenderCtx<'_>, col: usize) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let mut rows = Vec::new();
    for todo in todos {
        let (glyph, glyph_style, text_style) = todo_look(todo.state, theme);
        let head = [
            indent(col),
            Span::styled(glyph, glyph_style),
            Span::raw(" "),
        ];
        let spans = [Span::styled(
            sanitize_line(&todo.text).into_owned(),
            text_style,
        )];
        let hang = [indent(col + str_width(glyph) + 1)];
        rows.extend(wrap_spans(&spans, usize::from(ctx.width), &head, &hang));
    }
    rows
}

/// `☒ wire middleware  ☐ tests`. When the row is too narrow, finished items give way first so
/// the one in progress stays in view.
fn checklist_row(todos: &[Todo], ctx: &RenderCtx<'_>, col: usize) -> Line<'static> {
    let theme = ctx.theme;
    let text = theme.style(Role::Text);
    let dim = theme.style(Role::Dim);
    let unfinished = todos
        .iter()
        .position(|todo| todo.state != TodoState::Done)
        .unwrap_or(todos.len());
    let skipped = unfinished.saturating_sub(1);
    let mut spans = Vec::new();
    if skipped > 0 {
        spans.push(Span::styled(format!("{skipped} done · "), dim));
    }
    for (i, todo) in todos.iter().enumerate().skip(skipped) {
        if i > skipped {
            spans.push(Span::raw("  "));
        }
        let (glyph, glyph_style, text_style) = match todo.state {
            TodoState::Done => (theme.glyphs.checkbox_on, dim, dim),
            TodoState::InProgress => (
                theme.glyphs.checkbox_off,
                text,
                text.add_modifier(Modifier::BOLD),
            ),
            TodoState::Pending => (theme.glyphs.checkbox_off, text, text),
        };
        spans.push(Span::styled(glyph, glyph_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            sanitize_line(&todo.text).trim().to_owned(),
            text_style,
        ));
    }
    let room = usize::from(ctx.width).saturating_sub(col);
    let mut row = vec![indent(col)];
    row.extend(truncate_spans(&spans, room));
    Line::from(row)
}

/// Only the newest row of the text, so a live block keeps its size while an agent writes.
fn streaming_rows(text: &str, ctx: &RenderCtx<'_>, col: usize) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let blank = |row: &Line<'_>| row.spans.iter().all(|span| span.content.trim().is_empty());
    let rows = render_markdown(
        text,
        ctx,
        &Indent::new(vec![indent(col)], vec![indent(col)]),
    );
    let mut row = rows
        .into_iter()
        .rev()
        .find(|row| !blank(row))
        .unwrap_or_else(|| Line::from(indent(col)));
    row.spans
        .push(Span::styled(theme.glyphs.quote, theme.style(Role::Dim)));
    vec![row]
}

pub(super) fn todo_line(todos: &[Todo], ctx: &RenderCtx<'_>, col: usize) -> Line<'static> {
    let theme = ctx.theme;
    let done = todos.iter().filter(|t| t.state == TodoState::Done).count();
    let current = todos
        .iter()
        .find(|t| t.state == TodoState::InProgress)
        .or_else(|| todos.iter().find(|t| t.state == TodoState::Pending))
        .or(todos.last());
    let mut spans = Vec::new();
    if let Some(todo) = current {
        let (glyph, glyph_style, text_style) = todo_look(todo.state, theme);
        spans.push(Span::styled(glyph, glyph_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            sanitize_line(&todo.text).into_owned(),
            text_style,
        ));
    }
    spans.push(Span::styled(
        format!(" · {done}/{} done", todos.len()),
        theme.style(Role::Dim),
    ));
    let room = usize::from(ctx.width).saturating_sub(col);
    let mut row = vec![indent(col)];
    row.extend(truncate_spans(&spans, room));
    Line::from(row)
}

fn todo_look(state: TodoState, theme: &Theme) -> (&'static str, Style, Style) {
    let text = theme.style(Role::Text);
    let dim = theme.style(Role::Dim);
    match state {
        TodoState::Pending => (theme.glyphs.todo_pending, text, text),
        TodoState::InProgress => {
            let bold = text.add_modifier(Modifier::BOLD);
            (theme.glyphs.todo_active, bold, bold)
        }
        TodoState::Done => (
            theme.glyphs.todo_done,
            dim,
            dim.add_modifier(Modifier::CROSSED_OUT),
        ),
    }
}

/// A path like `src/app.rs` or `/tmp/x/y` is not a command.
fn slash_commands(text: &str, style: Style, code: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut push = |piece: &str, style: Style| {
        if piece.is_empty() {
            return;
        }
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push_str(piece),
            _ => spans.push(Span::styled(piece.to_owned(), style)),
        }
    };
    let mut rest = text;
    while !rest.is_empty() {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, after) = rest.split_at(word_end);
        match command_in(word) {
            Some((start, end)) => {
                push(&word[..start], style);
                push(&word[start..end], code);
                push(&word[end..], style);
            }
            None => push(word, style),
        }
        let space_end = after
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(after.len());
        push(&after[..space_end], style);
        rest = &after[space_end..];
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), style));
    }
    spans
}

/// The command's byte range inside one word, past any wrapping punctuation.
fn command_in(word: &str) -> Option<(usize, usize)> {
    let start = word.find(|c: char| !matches!(c, '(' | '[' | '"' | '\'' | '`'))?;
    let core = word[start..].trim_end_matches(|c: char| {
        matches!(
            c,
            ')' | ']' | '"' | '\'' | '`' | ',' | '.' | ':' | ';' | '!' | '?'
        )
    });
    let name = core.strip_prefix('/')?;
    let plain = name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    plain.then_some((start, start + core.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(text: &str) -> Vec<(String, bool)> {
        let code = Style::new().bold();
        slash_commands(text, Style::new(), code)
            .into_iter()
            .map(|span| (span.content.into_owned(), span.style == code))
            .collect()
    }

    #[test]
    fn slash_commands_are_found_inside_punctuation() {
        assert_eq!(
            pieces("Running · /workflows to monitor (/config)"),
            [
                ("Running · ".to_owned(), false),
                ("/workflows".to_owned(), true),
                (" to monitor (".to_owned(), false),
                ("/config".to_owned(), true),
                (")".to_owned(), false),
            ]
        );
    }

    #[test]
    fn paths_are_not_commands() {
        assert!(
            pieces("read src/app.rs and /tmp/x/y and /")
                .iter()
                .all(|(_, code)| !code)
        );
    }
}
