//! Adapted from the approach in Codex's `markdown_stream.rs` and `streaming/controller.rs`
//! (Apache-2.0): only newline-terminated source is committed, finished rows go to scrollback as
//! soon as they are stable, and the unfinished tail stays in the live region.
//!
//! Printing is tracked by source position, not by row count: every row remembers the source
//! offset it came from, and rows from source before `printed_to` have been handed out. So a row
//! never prints twice, and after a resize the rows still to come simply wrap at the new width.
//!
//! Three rules keep printed rows final. The newest complete line waits for the next one, since a
//! following line can still change it (an underline turning it into a heading, a delimiter row
//! turning it into a table, a backslash turning into a line break); inside a fenced code block a
//! finished line is final at once. A table waits until another block follows it, since a new row
//! can widen every column. And rendering restarts from a restart point at the newest top-level
//! block, or inside a fenced code block at its newest line, so a frame renders only what is
//! still being written.

use ratatui::text::Line;

use super::highlight::LineHighlighter;
use super::writer::{Trace, render_traced};
use super::{Continuation, code_rows};
use crate::base::draw::render::RenderCtx;
use crate::base::text::{fit_lines, sanitize};
use crate::cells::cell::AssistantMessage;

/// One assistant message arriving in pieces. [`take_stable`](Self::take_stable) returns rows
/// that can't change any more, [`tail`](Self::tail) the rows that still can, and
/// [`finish`](Self::finish) everything left when the message ends.
#[derive(Debug, Default)]
pub struct MarkdownStream {
    source: String,
    emitted: usize,
    /// Complete lines of `source`, sanitized. Each line is cleaned once it is whole, so an escape
    /// sequence split across two deltas is still removed.
    clean: String,
    /// Bytes of `source` already cleaned into `clean`.
    raw_done: usize,
    restart: RestartPoint,
    /// Rows from `clean` before this offset have been handed out.
    printed_to: usize,
    /// How much of `clean` the last [`take_stable`](Self::take_stable) looked at.
    looked_at: usize,
}

/// A place in the clean source where rendering can start without changing any row.
#[derive(Debug, Clone, Default)]
struct RestartPoint {
    at: usize,
    /// Whether anything before `at` rendered rows, so the first row after it is not the
    /// message's first row.
    has_rows: bool,
    /// Whether a heading ends right before `at`, so the block there gets a blank row first.
    after_heading: bool,
    /// Set when `at` is inside a fenced code block, with the parser state for the next line.
    fence: Option<FenceCursor>,
}

#[derive(Debug, Clone)]
struct FenceCursor {
    marker: char,
    len: usize,
    highlighter: LineHighlighter,
}

/// The rows of the source from the restart point on, each with the offset it came from.
struct Region {
    rows: Vec<Line<'static>>,
    offsets: Vec<usize>,
    /// Where the Markdown part starts, after a fenced block the restart point was inside of.
    markdown_at: usize,
    /// Where the fence the restart point was inside of closes, when it does.
    fence_close: Option<usize>,
    /// The parser state after the last code line, when the fence is still open at the end.
    fence_state: Option<LineHighlighter>,
    trace: Option<Trace>,
}

impl MarkdownStream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, delta: &str) {
        self.source.push_str(delta);
        if let Some(end) = self.source[self.raw_done..].rfind('\n') {
            let end = self.raw_done + end + 1;
            for line in self.source[self.raw_done..end].split_inclusive('\n') {
                self.clean.push_str(&sanitize(line));
            }
            self.raw_done = end;
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Rows already handed out for printing.
    pub fn emitted(&self) -> usize {
        self.emitted
    }

    /// Whether complete lines arrived since rows were last taken, so
    /// [`take_stable`](Self::take_stable) may have rows to give.
    pub fn has_new_lines(&self) -> bool {
        self.clean.len() > self.looked_at
    }

    /// Finished rows not handed out yet, marked as handed out. The message's first row carries
    /// the `●` bullet and later rows the two column indent, exactly as the whole message would
    /// render.
    pub fn take_stable(&mut self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        if !self.has_new_lines() {
            return Vec::new();
        }
        self.looked_at = self.clean.len();
        let at = self.restart.at;
        let region = self.render_region(&self.clean[at..], ctx);
        let text = &self.clean[at..];
        // Inside an open fence every finished line is final: code never changes meaning.
        let mut hold = if region.fence_state.is_some() {
            text.len()
        } else {
            last_line_start(text)
        };
        if let Some(table) = region.trace.as_ref().and_then(|trace| trace.table_hold) {
            hold = hold.min(region.markdown_at + table);
        }
        let printed = self.printed_to.saturating_sub(at);
        if hold <= printed {
            return Vec::new();
        }
        let mut rows = region.between(printed, hold);
        self.emitted += rows.len();
        self.printed_to = at + hold;
        let stable_rows = region.offsets.iter().any(|&offset| offset < hold);
        self.advance(&region, hold, stable_rows);
        fit_lines(&mut rows, usize::from(ctx.width));
        rows
    }

    /// Rows that may still change, such as the line being written, for the live region. Under
    /// reduced motion the line being written stays hidden until it ends, so text arrives a
    /// whole line at a time instead of word by word.
    pub fn tail(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let mut text = self.clean[self.restart.at..].to_owned();
        if !ctx.theme.reduced_motion {
            text.push_str(&sanitize(&self.source[self.raw_done..]));
        }
        let region = self.render_region(&text, ctx);
        let printed = self.printed_to.saturating_sub(self.restart.at);
        let mut rows = region.between(printed, usize::MAX);
        fit_lines(&mut rows, usize::from(ctx.width));
        rows
    }

    pub fn finish(&mut self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let partial = sanitize(&self.source[self.raw_done..]).into_owned();
        self.clean.push_str(&partial);
        self.raw_done = self.source.len();
        self.looked_at = self.clean.len();
        let at = self.restart.at;
        let region = self.render_region(&self.clean[at..], ctx);
        let printed = self.printed_to.saturating_sub(at);
        let mut rows = region.between(printed, usize::MAX);
        self.emitted += rows.len();
        self.printed_to = self.clean.len();
        fit_lines(&mut rows, usize::from(ctx.width));
        rows
    }

    /// Moves the restart point up to `hold`, the end of what just printed, when rendering can start
    /// over from there: inside the fenced block being written, or at a top-level block.
    fn advance(&mut self, region: &Region, hold: usize, stable_rows: bool) {
        let has_rows = self.restart.has_rows || stable_rows;
        let text = &self.clean[self.restart.at..];
        if let Some(cursor) = &self.restart.fence
            && region.fence_close.is_none_or(|close| hold <= close)
        {
            let mut cursor = cursor.clone();
            match &region.fence_state {
                Some(state) if hold == text.len() => cursor.highlighter = state.clone(),
                _ => {
                    for line in text[..hold].lines() {
                        cursor.highlighter.skip(line);
                    }
                }
            }
            self.restart = RestartPoint {
                at: self.restart.at + hold,
                has_rows,
                after_heading: false,
                fence: Some(cursor),
            };
            return;
        }
        let Some(trace) = &region.trace else {
            return;
        };
        let local = hold.checked_sub(region.markdown_at);
        if let Some(local) = local.filter(|local| trace.starts.contains(local)) {
            self.restart = RestartPoint {
                at: self.restart.at + hold,
                has_rows,
                after_heading: trace.after_heading.contains(&local),
                fence: None,
            };
            return;
        }
        let fence = trace
            .fences
            .iter()
            .find(|fence| local.is_some_and(|local| fence.content <= local && local < fence.end));
        if let (Some(fence), Some(local)) = (fence, local) {
            let mut highlighter = LineHighlighter::for_fence(&fence.info);
            let body = &text[region.markdown_at..];
            for line in body[fence.content..local].lines() {
                highlighter.skip(line);
            }
            self.restart = RestartPoint {
                at: self.restart.at + hold,
                has_rows,
                after_heading: false,
                fence: Some(FenceCursor {
                    marker: fence.marker,
                    len: fence.len,
                    highlighter,
                }),
            };
        }
    }

    /// Renders `text`, the source from the restart point on, remembering each row's offset.
    fn render_region(&self, text: &str, ctx: &RenderCtx<'_>) -> Region {
        let indent = AssistantMessage::indent(ctx.theme);
        let width = usize::from(ctx.width);
        let mut region = Region {
            rows: Vec::new(),
            offsets: Vec::new(),
            markdown_at: 0,
            fence_close: None,
            fence_state: None,
            trace: None,
        };
        let mut has_rows = self.restart.has_rows;
        if let Some(cursor) = &self.restart.fence {
            let mut highlighter = cursor.highlighter.clone();
            let mut pos = 0;
            for line in text.split_inclusive('\n') {
                let body = line.strip_suffix('\n').unwrap_or(line);
                // pulldown-cmark drops a last, unfinished line of one to three spaces, which could
                // still become an indented closing fence. Match it.
                if body.len() == line.len() && body.len() < 4 && body.bytes().all(|b| b == b' ') {
                    break;
                }
                if closes_fence(body, cursor.marker, cursor.len) {
                    region.fence_close = Some(pos);
                    pos += line.len();
                    break;
                }
                let spans = highlighter.line(body, ctx.theme);
                let first = if has_rows {
                    &indent.rest
                } else {
                    &indent.first
                };
                for row in code_rows(spans, width, first, &indent.rest) {
                    region.rows.push(row);
                    region.offsets.push(pos);
                }
                has_rows = true;
                pos += line.len();
            }
            if region.fence_close.is_none() {
                region.fence_state = Some(highlighter);
                return region;
            }
            region.markdown_at = pos;
        }
        let at_restart = region.markdown_at == 0;
        let cont = Continuation {
            rows_before: has_rows,
            blank_before: at_restart && self.blank_before_restart(),
            after_heading: at_restart && self.restart.after_heading,
        };
        let at = region.markdown_at;
        let (rows, trace) = render_traced(&text[at..], ctx, &indent, cont);
        region.rows.extend(rows);
        region
            .offsets
            .extend(trace.rows.iter().map(|offset| offset + at));
        region.trace = Some(trace);
        region
    }

    fn blank_before_restart(&self) -> bool {
        let before = &self.clean[..self.restart.at];
        let Some(body) = before.strip_suffix('\n') else {
            return false;
        };
        let line = body.rfind('\n').map_or(body, |i| &body[i + 1..]);
        line.chars().all(|c| c.is_whitespace() || c == '>')
    }
}

impl Region {
    /// The rows whose source offsets fall in `from..to`.
    fn between(&self, from: usize, to: usize) -> Vec<Line<'static>> {
        self.rows
            .iter()
            .zip(&self.offsets)
            .filter(|(_, offset)| (from..to).contains(*offset))
            .map(|(row, _)| row.clone())
            .collect()
    }
}

/// Where the last complete line of `text` starts. `text` ends with a newline.
fn last_line_start(text: &str) -> usize {
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.rfind('\n').map_or(0, |i| i + 1)
}

/// Whether `line` closes a fence opened with `len` of `marker`: up to three spaces, at least as
/// many markers, and nothing after them but spaces.
fn closes_fence(line: &str, marker: char, len: usize) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let run = rest.chars().take_while(|c| *c == marker).count();
    run >= len && rest[run * marker.len_utf8()..].trim().is_empty()
}

#[cfg(test)]
mod tests;
