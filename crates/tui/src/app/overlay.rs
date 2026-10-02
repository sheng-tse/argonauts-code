//! The transcript overlay shows every item so far, expanded, on the alternate screen. The same
//! screen shows views the host opens, such as the diff a card asks about.

use std::cell::{Cell, RefCell};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::cell::{Cell as HistoryItem, HistoryCell};
use crate::history::History;
use crate::render::RenderCtx;
use crate::text::{KEEP, Part, fit_parts, sanitize_line, str_width, truncate_line};
use crate::theme::{Role, Theme};

#[derive(Debug, Default)]
pub struct TranscriptOverlay {
    scroll_from_bottom: usize,
    /// As of the last render.
    page: Cell<usize>,
    max_scroll: Cell<usize>,
    /// The committed cells as rendered last time. History only grows, so only new cells render
    /// again, and scrolling a long transcript stays cheap.
    committed: RefCell<Option<Rendered>>,
    view: Option<OverlayView>,
}

#[derive(Debug)]
pub struct OverlayView {
    /// Shown in the footer as `Showing {title}`.
    pub title: String,
    pub cells: Vec<HistoryItem>,
}

/// Committed cells rendered at one width with one theme.
#[derive(Debug, Clone, PartialEq)]
struct Rendered {
    cells: usize,
    width: u16,
    theme: Theme,
    lines: Vec<Line<'static>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayEvent {
    Close,
    Scrolled,
    Ignored,
}

impl TranscriptOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    /// A view of `cells`, shown from the top.
    pub fn view(title: impl Into<String>, cells: Vec<HistoryItem>) -> Self {
        Self {
            // As far up as it goes, once a render knows how far that is.
            scroll_from_bottom: usize::MAX,
            view: Some(OverlayView {
                title: title.into(),
                cells,
            }),
            ..Self::default()
        }
    }

    pub fn is_view(&self) -> bool {
        self.view.is_some()
    }

    /// Zero follows the end.
    pub fn scroll_from_bottom(&self) -> usize {
        self.scroll_from_bottom.min(self.max_scroll.get())
    }

    /// Ctrl+C closes too, so the overlay never traps you.
    pub fn handle_key(&mut self, key: KeyEvent) -> OverlayEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.page.get().max(1);
        let max = self.max_scroll.get();
        let current = self.scroll_from_bottom.min(max);
        let target = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return OverlayEvent::Close,
            KeyCode::Char('o' | 'c') if ctrl => return OverlayEvent::Close,
            KeyCode::Up | KeyCode::Char('k') => current + 1,
            KeyCode::Down | KeyCode::Char('j') => current.saturating_sub(1),
            KeyCode::PageUp | KeyCode::Char('b') => current + page,
            KeyCode::PageDown | KeyCode::Char(' ') => current.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => max,
            KeyCode::End | KeyCode::Char('G') => 0,
            _ => return OverlayEvent::Ignored,
        };
        let target = target.min(max);
        if target == current {
            self.scroll_from_bottom = current;
            return OverlayEvent::Ignored;
        }
        self.scroll_from_bottom = target;
        OverlayEvent::Scrolled
    }

    pub fn render(
        &self,
        history: &History,
        ctx: &RenderCtx<'_>,
        height: u16,
    ) -> Vec<Line<'static>> {
        let ctx = ctx.with_expanded(true);
        if let Some(view) = &self.view {
            let mut lines: Vec<Line<'static>> = Vec::new();
            for cell in &view.cells {
                if !lines.is_empty() && !cell.continues_previous() {
                    lines.push(Line::default());
                }
                lines.extend(cell.lines(&ctx));
            }
            return self.window(&lines, &[], &ctx, height);
        }
        self.refresh_committed(history.cells(), &ctx);
        let committed = self.committed.borrow();
        let committed = committed.as_ref().map_or(&[][..], |r| r.lines.as_slice());
        let live = live_rows(history, &ctx, !committed.is_empty());
        self.window(committed, &live, &ctx, height)
    }

    /// Renders only cells added since the last call, unless the width or the theme changed.
    fn refresh_committed(&self, cells: &[crate::cell::Cell], ctx: &RenderCtx<'_>) {
        let mut cache = self.committed.borrow_mut();
        let reusable = cache
            .take()
            .filter(|r| r.width == ctx.width && r.theme == *ctx.theme && r.cells <= cells.len());
        let mut rendered = reusable.unwrap_or_else(|| Rendered {
            cells: 0,
            width: ctx.width,
            theme: ctx.theme.clone(),
            lines: Vec::new(),
        });
        for cell in &cells[rendered.cells..] {
            if !rendered.lines.is_empty() && !cell.continues_previous() {
                rendered.lines.push(Line::default());
            }
            rendered.lines.extend(cell.lines(ctx));
        }
        rendered.cells = cells.len();
        *cache = Some(rendered);
    }

    /// The content in view, padded so the footer sits on the last rows.
    fn window(
        &self,
        committed: &[Line<'static>],
        live: &[Line<'static>],
        ctx: &RenderCtx<'_>,
        height: u16,
    ) -> Vec<Line<'static>> {
        let rows = usize::from(height).saturating_sub(FOOTER_ROWS);
        let total = committed.len() + live.len();
        let max_scroll = total.saturating_sub(rows);
        self.page.set(rows);
        self.max_scroll.set(max_scroll);
        let scroll = self.scroll_from_bottom.min(max_scroll);
        let end = total - scroll;
        let start = end.saturating_sub(rows);
        let mut lines: Vec<Line<'static>> = committed
            .iter()
            .chain(live)
            .skip(start)
            .take(end - start)
            .cloned()
            .collect();
        lines.resize(rows, Line::default());
        let footer = [
            Line::from(Span::styled(
                ctx.theme.glyphs.rule.repeat(usize::from(ctx.width)),
                ctx.theme.style(Role::Rule),
            )),
            self.hint(scroll, ctx),
        ];
        let room = usize::from(height).min(FOOTER_ROWS);
        lines.extend(footer.into_iter().skip(FOOTER_ROWS - room));
        lines
    }

    /// Worded as Claude Code's, with how far up the view is at the right.
    fn hint(&self, scroll: usize, ctx: &RenderCtx<'_>) -> Line<'static> {
        let width = usize::from(ctx.width);
        let dim = ctx.theme.style(Role::Dim);
        let part = |text: String| vec![Span::styled(text, dim)];
        let dot = || Span::styled(" · ", dim);
        // Parts give way whole on a narrow screen; the way out stays.
        let parts = match &self.view {
            Some(view) => vec![
                Part::new(
                    part(format!("Showing {}", sanitize_line(&view.title).trim())),
                    KEEP,
                ),
                Part::joined(dot(), part("esc to go back".into()), KEEP),
            ],
            None => vec![
                Part::new(part("Showing detailed transcript".into()), 1),
                Part::joined(dot(), part("ctrl+o to toggle".into()), 0),
                Part::joined(dot(), part("esc to close".into()), KEEP),
            ],
        };
        let left = fit_parts(vec![Span::raw("  ")], &parts, width);
        let unit = if scroll == 1 { "row" } else { "rows" };
        let right = (scroll > 0 && self.view.is_none()).then(|| format!("{scroll} {unit} up"));
        let mut spans = left.spans;
        if let Some(right) = right {
            let used = spans.iter().map(Span::width).sum::<usize>() + str_width(&right) + 1;
            if used + 2 <= width {
                spans.push(Span::raw(" ".repeat(width - used)));
                spans.push(Span::styled(right, dim));
            }
        }
        truncate_line(&Line::from(spans), width)
    }
}

/// A rule, then the hint row.
const FOOTER_ROWS: usize = 2;

fn live_rows(history: &History, ctx: &RenderCtx<'_>, after_committed: bool) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = Vec::new();
    for block in history.live_blocks(ctx) {
        if block.lines.is_empty() {
            continue;
        }
        if block.gap_before && (after_committed || !rows.is_empty()) {
            rows.push(Line::default());
        }
        rows.extend(block.lines);
    }
    rows
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::cell::Cell as HistoryItem;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::Glyphs;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn rows(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(format!("row {i}"))).collect()
    }

    #[derive(Debug)]
    struct Counted {
        text: &'static str,
        renders: Arc<AtomicUsize>,
    }

    impl HistoryCell for Counted {
        fn lines(&self, _ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
            self.renders.fetch_add(1, Ordering::Relaxed);
            vec![Line::from(self.text)]
        }
    }

    #[test]
    fn keys_scroll_within_the_content_and_close() {
        let mut overlay = TranscriptOverlay::new();
        overlay.page.set(5);
        overlay.max_scroll.set(12);
        assert_eq!(
            overlay.handle_key(key(KeyCode::Down)),
            OverlayEvent::Ignored
        );
        assert_eq!(overlay.handle_key(key(KeyCode::Up)), OverlayEvent::Scrolled);
        assert_eq!(overlay.scroll_from_bottom(), 1);
        overlay.handle_key(key(KeyCode::PageUp));
        assert_eq!(overlay.scroll_from_bottom(), 6);
        overlay.handle_key(key(KeyCode::Char('g')));
        assert_eq!(overlay.scroll_from_bottom(), 12);
        assert_eq!(overlay.handle_key(key(KeyCode::Up)), OverlayEvent::Ignored);
        overlay.handle_key(key(KeyCode::PageDown));
        assert_eq!(overlay.scroll_from_bottom(), 7);
        overlay.handle_key(key(KeyCode::Char('G')));
        assert_eq!(overlay.scroll_from_bottom(), 0);
        assert_eq!(overlay.handle_key(key(KeyCode::Esc)), OverlayEvent::Close);
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('q'))),
            OverlayEvent::Close
        );
        assert_eq!(
            overlay.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            OverlayEvent::Close
        );
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('x'))),
            OverlayEvent::Ignored
        );
    }

    #[test]
    fn the_window_ends_at_the_bottom_until_scrolled() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let mut overlay = TranscriptOverlay::new();

        let lines = overlay.window(&rows(3), &[], &ctx, 6);
        assert_eq!(lines.len(), 6);
        let text = plain_text(&lines);
        assert!(text.starts_with("row 0\nrow 1\nrow 2\n\n─"), "{text}");
        assert!(
            text.ends_with("  Showing detailed transcript · ctrl+o to toggle · esc to close"),
            "{text}"
        );
        assert_eq!(lines[4].spans[0].style, theme.style(Role::Rule));

        let lines = overlay.window(&rows(2), &rows(3)[2..], &ctx, 4);
        assert_eq!(plain_text(&lines[..2]), "row 1\nrow 2");
        overlay.handle_key(key(KeyCode::Up));
        let lines = overlay.window(&rows(2), &rows(3)[2..], &ctx, 4);
        assert_eq!(plain_text(&lines[..2]), "row 0\nrow 1");
        assert!(plain_text(&lines[3..]).ends_with("1 row up"));
    }

    #[test]
    fn an_empty_history_still_fills_the_screen() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let lines = TranscriptOverlay::new().render(&History::new(), &ctx, 4);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn committed_cells_render_once_per_width_and_theme() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let renders = Arc::new(AtomicUsize::new(0));
        let counted = |text| {
            HistoryItem::Custom(Box::new(Counted {
                text,
                renders: renders.clone(),
            }))
        };
        let count = || renders.load(Ordering::Relaxed);
        let overlay = TranscriptOverlay::new();

        let mut cells = vec![counted("one")];
        overlay.refresh_committed(&cells, &ctx);
        overlay.refresh_committed(&cells, &ctx);
        assert_eq!(count(), 1);

        cells.push(counted("two"));
        overlay.refresh_committed(&cells, &ctx);
        assert_eq!(count(), 2);
        let lines = overlay.committed.borrow().as_ref().unwrap().lines.clone();
        assert_eq!(plain_text(&lines), "one\n\ntwo");

        overlay.refresh_committed(&cells, &ctx.with_width(30));
        assert_eq!(count(), 4);
        let light = Theme::light();
        overlay.refresh_committed(&cells, &RenderCtx::new(30, &light, ctx.clock));
        assert_eq!(count(), 6);
    }
}
