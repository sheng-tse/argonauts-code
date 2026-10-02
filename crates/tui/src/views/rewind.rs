//! `/rewind`: a card's handoff checkpoints, or every card's, oldest first. Enter goes back to
//! the picked one, after a confirm card in the chat. The work after it stays on a side branch.

use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::views::CheckpointRow;
use argonauts_core::{CardId, RunId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::format::WallClock;
use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

use super::list::{self, Column, ListState, fit_columns};
use super::panes::{self, Footer, Header, Marks, Side};
use super::resume::ago;
use super::{View, ViewCtx, ViewOutcome};

/// Cells between columns.
const GAP: usize = 2;
/// A commit is shown by its first characters, as git does.
const SHORT_COMMIT: usize = 7;

#[derive(Debug)]
pub struct RewindPicker {
    pub run: RunId,
    /// The card `/rewind` named, or `None` for every card's.
    pub card: Option<CardId>,
    pub rows: Vec<CheckpointRow>,
    pub list: ListState,
    /// Shows times like `2h ago`; tests pin it.
    pub clock: WallClock,
    page: usize,
}

impl RewindPicker {
    /// Opens on the newest checkpoint, the one you most likely want back.
    pub fn new(run: RunId, card: Option<CardId>, rows: Vec<CheckpointRow>) -> Self {
        let list = ListState {
            selected: rows.len().saturating_sub(1),
            offset: 0,
        };
        Self {
            run,
            card,
            rows,
            list,
            clock: WallClock::local(),
            page: 1,
        }
    }

    fn selected(&self) -> Option<&CheckpointRow> {
        self.rows.get(self.list.selected)
    }

    fn columns(&self, width: usize) -> Vec<Column> {
        let now = self.clock.now();
        let cells = |f: &dyn Fn(&CheckpointRow) -> String| self.rows.iter().map(f).collect();
        let columns = vec![
            Column::new(
                "Checkpoint",
                cells(&|row| match &self.card {
                    Some(_) => format!("handoff-{}", row.n),
                    None => format!("{} handoff-{}", sanitize_line(row.card.as_str()), row.n),
                }),
            ),
            Column::new("When", cells(&|row| ago(row.at, now)))
                .numeric()
                .keep(3),
            Column::new("Handoff", cells(&handed)).keep(4),
            Column::new("Why", cells(&|row| clean(&row.reason))).flex(),
            Column::new(
                "Commit",
                cells(&|row| clean(&row.commit).chars().take(SHORT_COMMIT).collect()),
            )
            .keep(1),
            Column::new(
                "",
                cells(&|row| {
                    if row.reverted_here {
                        "reverted here".to_owned()
                    } else {
                        String::new()
                    }
                }),
            )
            .keep(5),
        ];
        fit_columns(columns, width, GAP)
    }

    fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let lead = 2;
        let columns = self.columns(width.saturating_sub(lead));
        let mut heading = vec![Span::raw(" ".repeat(lead))];
        for (i, column) in columns.iter().enumerate() {
            if i > 0 {
                heading.push(Span::raw(" ".repeat(GAP)));
            }
            heading.push(Span::styled(column.cell(column.heading), dim));
        }
        let mut lines = vec![Line::from(heading)];
        for i in 0..self.rows.len() {
            let selected = i == self.list.selected;
            let mut spans = vec![if selected {
                Span::styled(format!("{} ", marks.selected), theme.style(Role::Label))
            } else {
                Span::raw("  ")
            }];
            for (c, column) in columns.iter().enumerate() {
                if c > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                let style = match column.heading {
                    "Checkpoint" if selected => theme.style(Role::Label),
                    "Checkpoint" | "Why" => theme.style(Role::Text),
                    _ => dim,
                };
                spans.push(Span::styled(column.cell(&column.cells[i]), style));
            }
            lines.push(Line::from(spans));
        }
        lines
    }

    fn pick(&self) -> ViewOutcome {
        match self.selected() {
            Some(row) => ViewOutcome::SendToChat(UiCommand::Rewind {
                card: row.card.clone(),
                checkpoint: row.n,
            }),
            None => ViewOutcome::Consumed,
        }
    }
}

fn handed(row: &CheckpointRow) -> String {
    let to: Vec<String> = row.to.iter().map(ToString::to_string).collect();
    clean(&format!("{} → {}", row.from, to.join(", ")))
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

impl View for RewindPicker {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let mut title = vec![Span::styled("Rewind", theme.style(Role::Label))];
        if let Some(card) = &self.card {
            title.push(Span::styled(
                format!(" {}", sanitize_line(card.as_str())),
                theme.style(Role::Dim),
            ));
        }
        let header = Header {
            title: Line::from(title),
            description: "Go back to a handoff checkpoint. The work after it stays on a side \
                          branch"
                .to_owned(),
            summary: panes::count(self.rows.len(), "checkpoint"),
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        // The heading stays; the rows scroll under it.
        let height = usize::from(panes.right.height).saturating_sub(1);
        self.page = height.max(1);
        self.list.keep_visible(self.rows.len(), self.page);
        let lines = self.lines(usize::from(panes.right.width), theme);
        let mut shown = vec![lines[0].clone()];
        shown.extend(lines.iter().skip(1 + self.list.offset).cloned());
        panes::put_lines(buf, panes.right, &shown);

        let marks = Marks::for_theme(theme);
        let mark =
            super::pager::position_mark(self.list.offset, height, self.rows.len(), marks.down);
        let hints = Footer::hints(&[
            ("↑↓ select", 0),
            ("⏎ go back to it", 1),
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
            KeyCode::Enter => return self.pick(),
            _ => {}
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.list.apply(nav, self.rows.len(), self.page);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        // A fresh list for the same run and card replaces this one, keeping the selection.
        let ViewData::Checkpoints {
            run,
            card,
            checkpoints,
        } = data
        else {
            return false;
        };
        if *run != self.run || *card != self.card {
            return false;
        }
        let picked = self.selected().map(|row| (row.card.clone(), row.n));
        self.rows = checkpoints.clone();
        let index = picked
            .and_then(|(card, n)| {
                self.rows
                    .iter()
                    .position(|row| row.card == card && row.n == n)
            })
            .unwrap_or(self.rows.len().saturating_sub(1));
        self.list.move_to(index, self.rows.len(), self.page);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::names::WorkerRef;
    use time::macros::datetime;

    use super::*;
    use crate::state::AppState;
    use crate::theme::Theme;

    fn row(card: &str, n: u32, to: &str) -> CheckpointRow {
        CheckpointRow {
            card: CardId::new(card).unwrap(),
            n,
            at: datetime!(2026-09-30 10:15 UTC),
            commit: "0123456789abcdef".to_owned(),
            from: WorkerRef::new("claude-code", Some("opus".to_owned())),
            to: vec![WorkerRef::new(to, None)],
            reason: "usage limit, resets at 3:45 PM".to_owned(),
            reverted_here: n == 1,
        }
    }

    fn picker(card: Option<&str>) -> RewindPicker {
        let mut picker = RewindPicker::new(
            RunId::new("run-02").unwrap(),
            card.map(|card| CardId::new(card).unwrap()),
            vec![row("card-001", 1, "codex"), row("card-001", 2, "opencode")],
        );
        picker.clock = WallClock::pinned(datetime!(2026-09-30 12:15 UTC));
        picker
    }

    fn text(picker: &RewindPicker, width: usize) -> Vec<String> {
        picker
            .lines(width, &Theme::dark())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn enter_goes_back_to_the_newest_checkpoint_unless_you_move() {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut view = picker(Some("card-001"));
        let rewind = |outcome: ViewOutcome| match outcome {
            ViewOutcome::SendToChat(UiCommand::Rewind { card, checkpoint }) => {
                (card.to_string(), checkpoint)
            }
            other => panic!("{other:?}"),
        };
        let outcome = view.handle_key(KeyEvent::from(KeyCode::Enter), &ctx);
        assert_eq!(rewind(outcome), ("card-001".to_owned(), 2));
        view.handle_key(KeyEvent::from(KeyCode::Up), &ctx);
        let outcome = view.handle_key(KeyEvent::from(KeyCode::Enter), &ctx);
        assert_eq!(rewind(outcome), ("card-001".to_owned(), 1));
        let outcome = view.handle_key(KeyEvent::from(KeyCode::Esc), &ctx);
        assert!(matches!(outcome, ViewOutcome::Close));
    }

    #[test]
    fn rows_name_the_card_only_when_rewind_named_none() {
        let one = text(&picker(Some("card-001")), 120);
        assert!(one[1].contains("handoff-1"), "{one:?}");
        assert!(!one[1].contains("card-001"), "{one:?}");
        assert!(one[1].contains("2h ago"), "{one:?}");
        assert!(one[1].contains("claude-code/opus → codex"), "{one:?}");
        assert!(one[1].contains("0123456"), "{one:?}");
        assert!(one[1].contains("reverted here"), "{one:?}");
        assert!(!one[2].contains("reverted here"), "{one:?}");
        let all = text(&picker(None), 120);
        assert!(all[1].contains("card-001 handoff-1"), "{all:?}");
    }

    #[test]
    fn a_fresh_list_for_another_card_is_not_this_ones() {
        let mut view = picker(Some("card-001"));
        let other = ViewData::Checkpoints {
            run: RunId::new("run-02").unwrap(),
            card: Some(CardId::new("card-002").unwrap()),
            checkpoints: vec![row("card-002", 1, "codex")],
        };
        assert!(!view.accept(&other));
        let same = ViewData::Checkpoints {
            run: RunId::new("run-02").unwrap(),
            card: Some(CardId::new("card-001").unwrap()),
            checkpoints: vec![row("card-001", 1, "codex")],
        };
        assert!(view.accept(&same));
        assert_eq!(view.rows.len(), 1);
    }
}
