//! The Learning pickers: `/records`, `/learned`, `/skills`, and `/memory`. One view draws all
//! four from the app's [`LearningView`]: the area's tabs at the right of the name row with a
//! Settings tab after them, the rows as columns, the selected row's detail under them, and its
//! actions in the footer. Tab and ⇧Tab switch tabs; a letter acts on the row. In the Settings
//! tab, Enter opens the setting's picker under the input, as `/config <key>` does.

use argonauts_core::app::{SettingRow, UiCommand, ViewData};
use argonauts_core::input::SlashCommand;
use argonauts_core::views::{LearningAction, LearningRow, LearningView};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::text::{sanitize_line, str_width};
use crate::theme::{Role, Theme};

use super::list::{self, Column, ListState, fit_columns};
use super::panes::{self, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Cells between columns.
const GAP: usize = 2;
/// The settings tab's title.
const SETTINGS: &str = "Settings";
/// Detail rows kept under the list when the screen is short.
const DETAIL_MIN: usize = 3;

#[derive(Debug)]
pub struct LearningPicker {
    pub view: LearningView,
    /// The open tab; the one past the area's tabs is Settings.
    pub tab: usize,
    pub list: ListState,
    page: usize,
}

impl LearningPicker {
    /// Opens on the tab the view names, at the row it focuses, if any.
    pub fn new(view: LearningView) -> Self {
        let tab = view.tab.min(view.tabs.len());
        let mut picker = Self {
            view,
            tab,
            list: ListState::default(),
            page: 1,
        };
        if let Some(focus) = picker.view.focus.clone()
            && let Some(index) = picker.rows().iter().position(|row| row.id == focus)
        {
            picker.list.selected = index;
        }
        picker
    }

    fn on_settings(&self) -> bool {
        self.tab >= self.view.tabs.len()
    }

    fn tab_count(&self) -> usize {
        self.view.tabs.len() + 1
    }

    fn rows(&self) -> &[LearningRow] {
        self.view
            .tabs
            .get(self.tab)
            .map(|tab| tab.rows.as_slice())
            .unwrap_or_default()
    }

    fn len(&self) -> usize {
        if self.on_settings() {
            self.view.settings.len()
        } else {
            self.rows().len()
        }
    }

    fn selected_row(&self) -> Option<&LearningRow> {
        if self.on_settings() {
            return None;
        }
        self.rows().get(self.list.selected)
    }

    fn selected_setting(&self) -> Option<&SettingRow> {
        if !self.on_settings() {
            return None;
        }
        self.view.settings.get(self.list.selected)
    }

    fn switch(&mut self, by: isize) {
        let count = isize::try_from(self.tab_count()).unwrap_or(1);
        let tab = isize::try_from(self.tab).unwrap_or(0);
        self.tab = usize::try_from((tab + by).rem_euclid(count)).unwrap_or(0);
        self.list = ListState::default();
    }

    /// The headings and cells of the open tab.
    fn table(&self) -> (Vec<String>, Vec<Vec<String>>) {
        if self.on_settings() {
            let headings = ["Setting", "Value", "From"].map(str::to_owned).to_vec();
            let cells = self
                .view
                .settings
                .iter()
                .map(|row| {
                    vec![
                        clean(&row.key),
                        clean(if row.value.is_empty() {
                            "unset"
                        } else {
                            &row.value
                        }),
                        clean(&row.layer),
                    ]
                })
                .collect();
            return (headings, cells);
        }
        let Some(tab) = self.view.tabs.get(self.tab) else {
            return (Vec::new(), Vec::new());
        };
        let cells = tab
            .rows
            .iter()
            .map(|row| row.cells.iter().map(|cell| clean(cell)).collect())
            .collect();
        (tab.columns.clone(), cells)
    }

    /// The heading row and one line per row, the selected one marked.
    pub fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let lead = 2;
        let (headings, cells) = self.table();
        let mut columns: Vec<Column> = headings
            .iter()
            .enumerate()
            .map(|(c, heading)| {
                let column_cells = cells
                    .iter()
                    .map(|row| row.get(c).cloned().unwrap_or_default())
                    .collect();
                let mut column = Column::new("", column_cells);
                column.width = column.width.max(str_width(heading));
                column
            })
            .collect();
        // Narrow screens drop columns from the right, the name never. A description in the
        // last column, the widest, flexes instead of going, and so does a setting's value,
        // which a list like `evolution.evolvable` makes too wide to fit whole.
        let count = columns.len();
        let flexing = if self.on_settings() {
            Some(1)
        } else {
            columns
                .iter()
                .enumerate()
                .skip(1)
                .max_by_key(|(_, column)| column.width)
                .map(|(c, _)| c)
                .filter(|&c| c + 1 == count)
        };
        for (c, column) in columns.iter_mut().enumerate().skip(1) {
            column.keep = u8::try_from(count - c).unwrap_or(u8::MAX);
            if flexing == Some(c) {
                column.flex = true;
                column.keep = 0;
            }
        }
        let columns = fit_columns(columns, width.saturating_sub(lead), GAP);
        let mut heading = vec![Span::raw(" ".repeat(lead))];
        for (c, column) in columns.iter().enumerate() {
            if c > 0 {
                heading.push(Span::raw(" ".repeat(GAP)));
            }
            let text = headings.get(c).map(String::as_str).unwrap_or_default();
            heading.push(Span::styled(column.cell(text), dim));
        }
        let mut lines = vec![Line::from(heading)];
        let waiting = self.waiting_marks();
        for (i, row) in cells.iter().enumerate() {
            let selected = i == self.list.selected;
            let mut spans = vec![if selected {
                Span::styled(format!("{} ", marks.selected), theme.style(Role::Card))
            } else if waiting.get(i).copied().unwrap_or(false) {
                // Waiting on you, in the color that state has in every view.
                Span::styled(
                    format!("{} ", marks.needs_you),
                    theme.style(Role::StateNeedsYou),
                )
            } else {
                Span::raw("  ")
            }];
            for (c, column) in columns.iter().enumerate() {
                if c > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                let style = match c {
                    0 if selected => theme.style(Role::Card),
                    0 => theme.style(Role::Text),
                    _ => dim,
                };
                let text = row.get(c).map(String::as_str).unwrap_or_default();
                spans.push(Span::styled(column.cell(text), style));
            }
            lines.push(Line::from(spans));
        }
        lines
    }

    fn waiting_marks(&self) -> Vec<bool> {
        if self.on_settings() {
            return Vec::new();
        }
        self.rows().iter().map(|row| row.waiting).collect()
    }

    /// The selected row's detail, or the setting's description.
    fn detail(&self) -> Vec<String> {
        if let Some(setting) = self.selected_setting() {
            let mut lines = vec![clean(&setting.description)];
            if setting.safety {
                lines.push("A safety key: changing it asks you to confirm.".to_owned());
            }
            return lines;
        }
        self.selected_row()
            .map(|row| row.detail.iter().map(|line| clean(line)).collect())
            .unwrap_or_default()
    }

    /// The name row's right side: `[ Agents ]  Predictions  Settings`.
    fn tabs_text(&self) -> String {
        let titles = self
            .view
            .tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .chain([SETTINGS]);
        titles
            .enumerate()
            .map(|(i, title)| {
                let title = clean(title);
                if i == self.tab {
                    format!("[ {title} ]")
                } else {
                    title
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    }

    fn empty_text(&self) -> String {
        if self.on_settings() {
            return "No settings in this area.".to_owned();
        }
        self.view
            .tabs
            .get(self.tab)
            .map(|tab| clean(&tab.empty))
            .unwrap_or_default()
    }

    fn actions(&self) -> &[LearningAction] {
        self.selected_row()
            .map(|row| row.actions.as_slice())
            .unwrap_or_default()
    }

    fn act(&self, key: char) -> ViewOutcome {
        match self.actions().iter().find(|action| action.key == key) {
            Some(action) => ViewOutcome::Send(UiCommand::Learning {
                area: self.view.area,
                command: action.command.clone(),
            }),
            None => ViewOutcome::Ignored,
        }
    }

    fn enter(&self) -> ViewOutcome {
        if let Some(setting) = self.selected_setting() {
            return ViewOutcome::SendToChat(UiCommand::Slash {
                command: SlashCommand::Config {
                    key: Some(setting.key.clone()),
                    value: None,
                    reset: false,
                },
            });
        }
        match self.actions().first() {
            Some(action) => self.act(action.key),
            None => ViewOutcome::Consumed,
        }
    }

    fn hints(&self) -> Vec<(String, u8)> {
        let mut hints = vec![("↑↓ select".to_owned(), 0), ("tab next tab".to_owned(), 2)];
        if self.on_settings() {
            hints.push(("⏎ change".to_owned(), 1));
        }
        for action in self.actions() {
            hints.push((format!("{} {}", action.key, clean(&action.label)), 1));
        }
        hints.push((panes::ESC_BACK.to_owned(), 0));
        hints
    }
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

impl View for LearningPicker {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let title = Line::from(vec![Span::styled(
            self.view.area.title(),
            theme.style(Role::Label),
        )]);
        let header = Header {
            title,
            description: self.view.area.description().to_owned(),
            summary: clean(&self.view.summary),
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        // The tabs sit at the right of the name row, as the workflow view's do.
        let tabs = self.tabs_text();
        let title_width = u16::try_from(str_width(self.view.area.title())).unwrap_or(0);
        let tabs_width = u16::try_from(str_width(&tabs))
            .unwrap_or(u16::MAX)
            .min(panes.header.width.saturating_sub(title_width + 2));
        if tabs_width > 0 && panes.header.height > 0 {
            let line = Line::from(Span::styled(
                super::layout::right(&tabs, usize::from(tabs_width)),
                theme.style(Role::Dim),
            ));
            panes::put(
                buf,
                panes.header.right() - tabs_width,
                panes.header.y,
                tabs_width,
                &line,
            );
        }
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let height = usize::from(panes.right.height);
        let detail = self.detail();
        let detail_rows = if detail.is_empty() {
            0
        } else {
            (detail.len() + 1)
                .min(height / 2)
                .max(DETAIL_MIN.min(height / 2))
        };
        let list_rows = height.saturating_sub(detail_rows);
        // The heading stays; the rows scroll under it.
        self.page = list_rows.saturating_sub(1).max(1);
        self.list.keep_visible(self.len(), self.page);
        let width = usize::from(panes.right.width);
        let mut shown = Vec::new();
        if self.len() == 0 {
            shown.push(Line::from(Span::styled(
                format!("  {}", self.empty_text()),
                theme.style(Role::Dim),
            )));
        } else {
            let lines = self.lines(width, theme);
            shown.push(lines[0].clone());
            shown.extend(
                lines
                    .iter()
                    .skip(1 + self.list.offset)
                    .take(self.page)
                    .cloned(),
            );
        }
        while shown.len() < list_rows {
            shown.push(Line::default());
        }
        if detail_rows > 0 {
            shown.truncate(list_rows);
            shown.push(Line::default());
            for text in detail.iter().take(detail_rows.saturating_sub(1)) {
                shown.push(Line::from(Span::styled(
                    format!("  {text}"),
                    theme.style(Role::Text),
                )));
            }
        }
        panes::put_lines(buf, panes.right, &shown);

        let marks = Marks::for_theme(theme);
        let mark = super::pager::position_mark(self.list.offset, self.page, self.len(), marks.down);
        panes::draw_footer(buf, panes.footer, Footer::Hints(self.hints()), &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') if plain => return ViewOutcome::Close,
            KeyCode::Tab => {
                self.switch(1);
                return ViewOutcome::Consumed;
            }
            KeyCode::BackTab => {
                self.switch(-1);
                return ViewOutcome::Consumed;
            }
            KeyCode::Enter => return self.enter(),
            KeyCode::Char(c) if plain && !self.on_settings() => {
                return match self.act(c) {
                    ViewOutcome::Ignored => ViewOutcome::Consumed,
                    outcome => outcome,
                };
            }
            _ => {}
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.list.apply(nav, self.len(), self.page);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        // The area's rows again, after an action or a setting changed: same tab, same row.
        let ViewData::Learning { view } = data else {
            return false;
        };
        if view.area != self.view.area {
            return false;
        }
        let picked = self.selected_row().map(|row| row.id.clone());
        let tab = self.tab;
        self.view = (**view).clone();
        self.tab = tab.min(self.view.tabs.len());
        let index = picked
            .and_then(|id| self.rows().iter().position(|row| row.id == id))
            .unwrap_or(self.list.selected);
        self.list.move_to(index, self.len(), self.page);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::views::{LearningArea, LearningCommand, LearningTab};

    use super::*;
    use crate::state::AppState;

    pub(crate) fn records() -> LearningView {
        LearningView {
            area: LearningArea::Records,
            tabs: vec![LearningTab {
                title: "Agents".to_owned(),
                columns: ["Agent", "Kind", "First pass", "Cards"]
                    .map(str::to_owned)
                    .to_vec(),
                rows: vec![LearningRow {
                    id: "codex/build".to_owned(),
                    cells: ["codex", "build", "82%", "11"].map(str::to_owned).to_vec(),
                    detail: vec!["Better than claude-code at build: not clear yet".to_owned()],
                    actions: vec![LearningAction {
                        key: 'p',
                        label: "pin".to_owned(),
                        command: LearningCommand::Pin {
                            worker: "codex".to_owned(),
                            task_type: "build".to_owned(),
                        },
                    }],
                    waiting: false,
                }],
                empty: "No outcomes yet.".to_owned(),
            }],
            tab: 0,
            focus: None,
            summary: "1 agent".to_owned(),
            settings: Vec::new(),
            refresh: false,
        }
    }

    #[test]
    fn a_letter_acts_on_the_row_and_tab_reaches_settings() {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut picker = LearningPicker::new(records());
        let outcome = picker.handle_key(KeyEvent::from(KeyCode::Char('p')), &ctx);
        assert!(matches!(
            outcome,
            ViewOutcome::Send(UiCommand::Learning {
                area: LearningArea::Records,
                command: LearningCommand::Pin { .. }
            })
        ));
        picker.handle_key(KeyEvent::from(KeyCode::Tab), &ctx);
        assert!(picker.on_settings());
        assert!(picker.tabs_text().ends_with("[ Settings ]"));
        picker.handle_key(KeyEvent::from(KeyCode::Tab), &ctx);
        assert_eq!(picker.tab, 0);
        let outcome = picker.handle_key(KeyEvent::from(KeyCode::Esc), &ctx);
        assert!(matches!(outcome, ViewOutcome::Close));
    }
}
