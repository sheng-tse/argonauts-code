//! What stays, most important first: the header, the to-do list as one line, the newest line of
//! streaming text, a `… +N lines` mark, then the newest body rows. A cut diff keeps its first rows.
//!
//! Live blocks redraw every frame, so each body item renders once here, and the same rows serve
//! both the check for whether everything fits and the squeezed form.

use ratatui::text::Line;

use super::body::{item_rows, output_role, todo_line};
use super::{ToolCall, ToolItem};
use crate::base::draw::render::RenderCtx;
use crate::cells::cell::layout::{BODY_COL, hook, indent, more_row};

pub(super) fn within(call: &ToolCall, ctx: &RenderCtx<'_>, limit: usize) -> Vec<Line<'static>> {
    let ctx = &ctx.with_live(true);
    let mut header = call.header(ctx);
    let output = output_role(call);
    let items: Vec<Vec<Line<'static>>> = call
        .body
        .iter()
        .map(|item| item_rows(item, ctx, BODY_COL, output))
        .collect();
    let total = header.len() + items.iter().map(Vec::len).sum::<usize>();
    if total <= limit {
        let body = items.into_iter().flatten().collect();
        return joined(header, body, ctx);
    }
    if header.len() >= limit {
        header.truncate(limit);
        return header;
    }
    let mut room = limit - header.len();

    let todo = call
        .body
        .iter()
        .zip(&items)
        .rev()
        .find_map(|(item, rows)| match item {
            ToolItem::Todos(todos) if !todos.is_empty() => Some(todo_line(todos, ctx, BODY_COL)),
            ToolItem::TodoLine(todos) if !todos.is_empty() => rows.first().cloned(),
            _ => None,
        });
    let todo = todo.filter(|_| take(&mut room));

    // Everything but the to-do lists, which the one line above stands for.
    let mut segments: Vec<Segment> = call
        .body
        .iter()
        .zip(items)
        .filter(|(item, _)| !matches!(item, ToolItem::Todos(_) | ToolItem::TodoLine(_)))
        .map(|(item, rows)| Segment {
            rows,
            preview: shows_diff(item),
            text: matches!(item, ToolItem::Markdown(_) | ToolItem::Streaming(_)),
        })
        .collect();

    let mut hidden = 0;
    let mut text_line = None;
    if segments.last().is_some_and(|segment| segment.text)
        && let Some(mut streaming) = segments.pop().map(|segment| segment.rows)
    {
        hidden += streaming.len().saturating_sub(1);
        if take(&mut room) {
            text_line = streaming.pop();
        } else {
            hidden += usize::from(!streaming.is_empty());
        }
    }

    let total: usize = segments.iter().map(|segment| segment.rows.len()).sum();
    let kept: Vec<Line<'static>> = if total <= room && hidden == 0 {
        segments
            .into_iter()
            .flat_map(|segment| segment.rows)
            .collect()
    } else {
        let mark = take(&mut room);
        let mut picked: Vec<Vec<Line<'static>>> = Vec::new();
        for segment in segments.into_iter().rev() {
            if room == 0 {
                break;
            }
            let len = segment.rows.len();
            if len <= room {
                room -= len;
                picked.push(segment.rows);
            } else if segment.preview {
                picked.push(segment.rows.into_iter().take(room).collect());
                room = 0;
            } else {
                picked.push(segment.rows.into_iter().skip(len - room).collect());
                room = 0;
            }
        }
        let mut kept: Vec<Line<'static>> = picked.into_iter().rev().flatten().collect();
        hidden += total - kept.len();
        if mark && hidden > 0 {
            // The transcript overlay shows every row, so the mark says how to get there.
            let width = usize::from(ctx.width);
            let marker = more_row(vec![indent(BODY_COL)], ctx.theme, hidden, true, width);
            kept.insert(0, marker);
        }
        kept
    };

    let mut body = kept;
    body.extend(text_line);
    body.extend(todo);
    joined(header, body, ctx)
}

/// Whatever row the cut leaves first takes the block's one `⎿` hook.
fn joined(
    mut header: Vec<Line<'static>>,
    mut body: Vec<Line<'static>>,
    ctx: &RenderCtx<'_>,
) -> Vec<Line<'static>> {
    if let Some(row) = body.first_mut() {
        hook(row, ctx.theme);
    }
    header.append(&mut body);
    header
}

struct Segment {
    rows: Vec<Line<'static>>,
    /// A diff, which keeps its first rows when cut rather than its last.
    preview: bool,
    /// The agent's own words, whose newest line stays in view.
    text: bool,
}

fn take(room: &mut usize) -> bool {
    if *room == 0 {
        return false;
    }
    *room -= 1;
    true
}

fn shows_diff(item: &ToolItem) -> bool {
    match item {
        ToolItem::Diff(_) => true,
        ToolItem::Call(call) => call
            .body
            .iter()
            .any(|item| matches!(item, ToolItem::Diff(_))),
        _ => false,
    }
}
