//! `/status`: where the run stands, built by code from its log, so it costs nothing. `s` asks
//! the narrator for a summary, which goes into the chat.

use argonauts_core::api::StatusSummary;
use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::display_names::DisplayNames;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::bridge::transcript::{NOTHING_RUNNING, status_sections};
use crate::text::{sanitize_line, wrap_text};
use crate::theme::{Role, Theme};

use super::list::{self, Nav};
use super::pager::position_mark;
use super::panes::{self, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

#[derive(Debug)]
pub struct StatusView {
    pub summary: StatusSummary,
    pub offset: usize,
    page: usize,
    rows: usize,
}

impl StatusView {
    pub fn new(summary: StatusSummary) -> Self {
        Self {
            summary,
            offset: 0,
            page: 1,
            rows: 0,
        }
    }

    fn lines(&self, width: usize, names: &DisplayNames, theme: &Theme) -> Vec<Line<'static>> {
        let sections = status_sections(&self.summary, names);
        if sections.is_empty() {
            return vec![Line::from(Span::styled(
                NOTHING_RUNNING,
                theme.style(Role::Dim),
            ))];
        }
        let mut lines = Vec::new();
        for section in sections {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            for line in section {
                let text = sanitize_line(&line).into_owned();
                lines.extend(wrap_text(&text, theme.style(Role::Text), width, &[], &[]));
            }
        }
        lines
    }

    fn scroll(&mut self, nav: Nav) {
        self.offset = list::scroll(self.offset, nav, self.rows, self.page);
    }
}

impl View for StatusView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let run = self
            .summary
            .run
            .as_ref()
            .map_or_else(|| "no run".to_owned(), ToString::to_string);
        let header = Header {
            title: Line::from(Span::styled("Status", theme.style(Role::Label))),
            description: "Built from the run's log, so it costs nothing".to_owned(),
            summary: run,
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let lines = self.lines(usize::from(panes.right.width), &ctx.state.names, theme);
        let height = usize::from(panes.right.height);
        self.page = height.max(1);
        self.rows = lines.len();
        self.offset = self.offset.min(lines.len().saturating_sub(height));
        panes::put_lines(buf, panes.right, &lines[self.offset..]);

        let marks = Marks::for_theme(theme);
        let mark = position_mark(self.offset, height, lines.len(), marks.down);
        let hints = Footer::hints(&[
            ("s narrator summary", 2),
            ("↑↓ scroll", 1),
            (panes::ESC_BACK, 0),
        ]);
        panes::draw_footer(buf, panes.footer, hints, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') if plain => return ViewOutcome::Close,
            KeyCode::Char('s') if plain => return ViewOutcome::SendToChat(UiCommand::Summary),
            _ => {}
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.scroll(nav);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        let ViewData::Status { summary } = data else {
            return false;
        };
        self.summary = summary.clone();
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::RunId;
    use crossterm::event::KeyEvent;

    use super::*;
    use crate::state::AppState;
    use crate::theme::Theme;

    fn summary() -> StatusSummary {
        StatusSummary {
            run: Some(RunId::new("run-06").unwrap()),
            goal: vec!["Export reports as CSV".to_owned()],
            plan: Some("Two cards".to_owned()),
            ..StatusSummary::default()
        }
    }

    #[test]
    fn s_asks_the_narrator_and_goes_back_to_the_chat() {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut view = StatusView::new(summary());
        let outcome = view.handle_key(KeyEvent::from(KeyCode::Char('s')), &ctx);
        assert!(matches!(
            outcome,
            ViewOutcome::SendToChat(UiCommand::Summary)
        ));
        let outcome = view.handle_key(KeyEvent::from(KeyCode::Esc), &ctx);
        assert!(matches!(outcome, ViewOutcome::Close));
    }

    #[test]
    fn it_draws_the_same_groups_the_chat_prints() {
        let theme = Theme::dark();
        let view = StatusView::new(summary());
        let text: Vec<String> = view
            .lines(60, &Default::default(), &theme)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert_eq!(text, ["Goal: Export reports as CSV", "", "Plan: Two cards"]);
        let empty = StatusView::new(StatusSummary::default());
        let lines = empty.lines(60, &Default::default(), &theme);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn a_fresh_status_replaces_the_shown_one() {
        let mut view = StatusView::new(StatusSummary::default());
        assert!(view.accept(&ViewData::Status { summary: summary() }));
        assert_eq!(view.summary.plan.as_deref(), Some("Two cards"));
    }
}
