//! `/rewind`: a task's fallback checkpoints, or every task's, oldest first. Enter goes back to
//! the picked one, after a confirm question in the conversation. The work after it stays on a side
//! branch.

use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::views::CheckpointRow;
use argonauts_core::{RunId, TaskId};
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
    /// The task `/rewind` named, or `None` for every task's.
    pub task: Option<TaskId>,
    pub rows: Vec<CheckpointRow>,
    pub list: ListState,
    /// Shows times like `2h ago`; tests pin it.
    pub clock: WallClock,
    page: usize,
}

impl RewindPicker {
    /// Opens on the newest checkpoint, the one you most likely want back.
    pub fn new(run: RunId, task: Option<TaskId>, rows: Vec<CheckpointRow>) -> Self {
        let list = ListState {
            selected: rows.len().saturating_sub(1),
            offset: 0,
        };
        Self {
            run,
            task,
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
                cells(&|row| match &self.task {
                    Some(_) => format!("fallback-{}", row.n),
                    None => format!("{} fallback-{}", sanitize_line(row.task.as_str()), row.n),
                }),
            ),
            Column::new("When", cells(&|row| ago(row.at, now)))
                .numeric()
                .keep(3),
            Column::new("Fallback", cells(&from_to)).keep(4),
            Column::new("Why", cells(&|row| clean(&row.reason))).flex(),
            Column::new(
                "Commit",
                cells(&|row| clean(&row.commit).chars().take(SHORT_COMMIT).collect()),
            )
            .keep(1),
            Column::new(
                "",
                cells(&|row| {
                    if row.rewound_here {
                        "rewound here".to_owned()
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
        let margin = 2;
        let columns = self.columns(width.saturating_sub(margin));
        let mut heading = vec![Span::raw(" ".repeat(margin))];
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
                Span::styled(format!("{} ", marks.selected), theme.style(Role::Question))
            } else {
                Span::raw("  ")
            }];
            for (c, column) in columns.iter().enumerate() {
                if c > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                let style = match column.heading {
                    "Checkpoint" if selected => theme.style(Role::Question),
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
            Some(row) => ViewOutcome::SendToConversation(UiCommand::Rewind {
                task: row.task.clone(),
                checkpoint: row.n,
            }),
            None => ViewOutcome::Consumed,
        }
    }
}

fn from_to(row: &CheckpointRow) -> String {
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
        if let Some(task) = &self.task {
            title.push(Span::styled(
                format!(" {}", sanitize_line(task.as_str())),
                theme.style(Role::Dim),
            ));
        }
        let header = Header {
            title: Line::from(title),
            description: "Go back to a fallback checkpoint. The work after it stays on a side \
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
        // A fresh list for the same run and task replaces this one, keeping the selection.
        let ViewData::Checkpoints {
            run,
            task,
            checkpoints,
        } = data
        else {
            return false;
        };
        if *run != self.run || *task != self.task {
            return false;
        }
        let picked = self.selected().map(|row| (row.task.clone(), row.n));
        self.rows = checkpoints.clone();
        let index = picked
            .and_then(|(task, n)| {
                self.rows
                    .iter()
                    .position(|row| row.task == task && row.n == n)
            })
            .unwrap_or(self.rows.len().saturating_sub(1));
        self.list.move_to(index, self.rows.len(), self.page);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::names::ModelRef;
    use time::macros::datetime;

    use super::*;
    use crate::state::AppState;
    use crate::theme::Theme;

    fn row(task: &str, n: u32, to: &str) -> CheckpointRow {
        CheckpointRow {
            task: TaskId::new(task).unwrap(),
            n,
            at: datetime!(2026-09-30 10:15 UTC),
            commit: "0123456789abcdef".to_owned(),
            from: ModelRef::new("claude-code", Some("opus".to_owned())),
            to: vec![ModelRef::new(to, None)],
            reason: "usage limit, resets at 3:45 PM".to_owned(),
            rewound_here: n == 1,
        }
    }

    fn picker(task: Option<&str>) -> RewindPicker {
        let mut picker = RewindPicker::new(
            RunId::new("run-02").unwrap(),
            task.map(|task| TaskId::new(task).unwrap()),
            vec![row("task-001", 1, "codex"), row("task-001", 2, "opencode")],
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
        let mut view = picker(Some("task-001"));
        let rewind = |outcome: ViewOutcome| match outcome {
            ViewOutcome::SendToConversation(UiCommand::Rewind { task, checkpoint }) => {
                (task.to_string(), checkpoint)
            }
            other => panic!("{other:?}"),
        };
        let outcome = view.handle_key(KeyEvent::from(KeyCode::Enter), &ctx);
        assert_eq!(rewind(outcome), ("task-001".to_owned(), 2));
        view.handle_key(KeyEvent::from(KeyCode::Up), &ctx);
        let outcome = view.handle_key(KeyEvent::from(KeyCode::Enter), &ctx);
        assert_eq!(rewind(outcome), ("task-001".to_owned(), 1));
        let outcome = view.handle_key(KeyEvent::from(KeyCode::Esc), &ctx);
        assert!(matches!(outcome, ViewOutcome::Close));
    }

    #[test]
    fn rows_name_the_task_only_when_rewind_named_none() {
        let one = text(&picker(Some("task-001")), 120);
        assert!(one[1].contains("fallback-1"), "{one:?}");
        assert!(!one[1].contains("task-001"), "{one:?}");
        assert!(one[1].contains("2h ago"), "{one:?}");
        assert!(one[1].contains("claude-code/opus → codex"), "{one:?}");
        assert!(one[1].contains("0123456"), "{one:?}");
        assert!(one[1].contains("rewound here"), "{one:?}");
        assert!(!one[2].contains("rewound here"), "{one:?}");
        let all = text(&picker(None), 120);
        assert!(all[1].contains("task-001 fallback-1"), "{all:?}");
    }

    #[test]
    fn a_fresh_list_for_another_task_is_not_this_ones() {
        let mut view = picker(Some("task-001"));
        let other = ViewData::Checkpoints {
            run: RunId::new("run-02").unwrap(),
            task: Some(TaskId::new("task-002").unwrap()),
            checkpoints: vec![row("task-002", 1, "codex")],
        };
        assert!(!view.accept(&other));
        let same = ViewData::Checkpoints {
            run: RunId::new("run-02").unwrap(),
            task: Some(TaskId::new("task-001").unwrap()),
            checkpoints: vec![row("task-001", 1, "codex")],
        };
        assert!(view.accept(&same));
        assert_eq!(view.rows.len(), 1);
    }
}
