//! All settings, `/config all` or `/config <key>`: every key, searchable and grouped by section,
//! with its value, the layer it came from in words, its description, and a mark on safety keys.
//! Enter edits a key: its values or a typed line, saved where your changes go, with Reset where
//! the layer allows.

use argonauts_core::app::{ConfigEntry, SettingRow, ViewData};
use argonauts_core::settings::layer;
use argonauts_core::settings::{FILE_LAYERS, SettingScope};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::flow;
use super::panel::{draw_editor, edit_key, edit_paste};
use super::table::{Widths, heading_line, setting_line};
use crate::base::draw::panes::{self, Footer, Header, Side};
use crate::base::text::words::{layer_words, reset_scope, row_value_words};
use crate::base::theme::Role;
use crate::bottom::Picker;
use crate::bottom::picker::one_line;
use crate::frame::view::list::{self, FilterKey};
use crate::frame::view::{View, ViewCtx, ViewOutcome};

#[derive(Debug)]
pub struct AllSettingsView {
    pub rows: Vec<SettingRow>,
    pub filter: String,
    /// Typing the filter after `/`, with vim keys.
    pub filtering: bool,
    /// The pointer, as a position among the rows the filter keeps.
    pub selected: usize,
    /// The first line on screen.
    offset: usize,
    /// Editing the row under the pointer.
    editor: Option<Picker>,
}

/// One line of the list: a section's heading or a row, by its index in `rows`.
enum Entry {
    Heading(String),
    Row(usize),
}

impl AllSettingsView {
    pub fn new(rows: Vec<SettingRow>) -> Self {
        Self {
            rows,
            filter: String::new(),
            filtering: false,
            selected: 0,
            offset: 0,
            editor: None,
        }
    }

    /// Every key, searched for `key` with the pointer on it, as Config opens a value you type or
    /// one a higher layer holds. Enter edits it there.
    pub fn at_key(rows: Vec<SettingRow>, key: &str) -> Self {
        let mut view = Self::new(rows);
        view.filter = key.to_owned();
        view.selected = view
            .shown()
            .iter()
            .position(|&index| view.rows[index].key == key)
            .unwrap_or(0);
        view
    }

    /// From the plain list of resolved keys, which has no descriptions or allowed values. A
    /// change saves where the key is set now, or everywhere for a built-in value.
    pub fn from_entries(entries: &[ConfigEntry]) -> Self {
        let picked = entries
            .iter()
            .find(|entry| entry.key == "workflow")
            .map(|entry| entry.layer.clone())
            .filter(|layer| FILE_LAYERS.contains(&layer.as_str()));
        let rows = entries
            .iter()
            .map(|entry| SettingRow {
                key: entry.key.clone(),
                value: entry.value.clone(),
                layer: entry.layer.clone(),
                workflow_from: picked.clone().filter(|_| entry.layer == layer::WORKFLOW),
                default_scope: reset_scope(&entry.layer).unwrap_or(SettingScope::Global),
                safety: entry.safety,
                allowed: Vec::new(),
                description: String::new(),
                picked: None,
            })
            .collect();
        Self::new(rows)
    }

    /// Indices into `rows` the search keeps, grouped by section in the order sections first
    /// appear.
    fn shown(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        let matches = |row: &SettingRow| {
            needle.is_empty()
                || [
                    row.key.as_str(),
                    &row_value_words(&row.key, &row.value),
                    &layer_words(&row.layer),
                    row.description.as_str(),
                ]
                .iter()
                .any(|text| text.to_lowercase().contains(&needle))
        };
        let mut sections: Vec<&str> = Vec::new();
        for row in &self.rows {
            let section = section_of(&row.key);
            if !sections.contains(&section) {
                sections.push(section);
            }
        }
        sections
            .iter()
            .flat_map(|section| {
                self.rows
                    .iter()
                    .enumerate()
                    .filter(move |(_, row)| section_of(&row.key) == *section && matches(row))
                    .map(|(index, _)| index)
            })
            .collect()
    }

    fn entries(&self, shown: &[usize]) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut last = None;
        for &index in shown {
            let section = section_of(&self.rows[index].key);
            if last != Some(section) {
                if last.is_some() {
                    entries.push(Entry::Heading(String::new()));
                }
                entries.push(Entry::Heading(section.to_owned()));
                last = Some(section);
            }
            entries.push(Entry::Row(index));
        }
        entries
    }

    fn move_by(&mut self, delta: isize) -> ViewOutcome {
        let count = self.shown().len();
        if count > 0 {
            let last = count as isize - 1;
            self.selected = (self.selected as isize)
                .saturating_add(delta)
                .clamp(0, last) as usize;
        }
        ViewOutcome::Consumed
    }

    fn search(&mut self, change: impl FnOnce(&mut String)) -> ViewOutcome {
        change(&mut self.filter);
        self.selected = 0;
        self.offset = 0;
        ViewOutcome::Consumed
    }
}

/// The first and last row on screen, counted among the rows alone, since the headings and the
/// blank lines between sections are no settings.
fn rows_on_screen(entries: &[Entry], offset: usize, height: usize) -> Option<(usize, usize)> {
    let before = entries[..offset.min(entries.len())]
        .iter()
        .filter(|entry| matches!(entry, Entry::Row(_)))
        .count();
    let on_screen = entries
        .iter()
        .skip(offset)
        .take(height)
        .filter(|entry| matches!(entry, Entry::Row(_)))
        .count();
    (on_screen > 0).then(|| (before + 1, before + on_screen))
}

/// `ui` for `ui.theme`; keys with no dot, such as `workflow`, go under `general`.
fn section_of(key: &str) -> &str {
    match key.split_once('.') {
        Some((section, _)) => section,
        None => "general",
    }
}

impl View for AllSettingsView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let shown = self.shown();
        let summary = if self.filter.is_empty() {
            panes::count(self.rows.len(), "setting")
        } else {
            format!("{} of {} match", shown.len(), self.rows.len())
        };
        let header = Header {
            title: Line::from(Span::styled("All settings", theme.style(Role::Label))),
            description: "Every setting and the layer it comes from. ! marks a safety key"
                .to_owned(),
            summary,
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        let search = if self.filter.is_empty() && !self.filtering {
            String::new()
        } else {
            format!("Search: {}", crate::base::text::sanitize_line(&self.filter))
        };
        panes::draw_box(buf, &panes, ["", &search], Side::Right, theme);

        if let Some(editor) = &self.editor {
            draw_editor(editor, panes.right, buf, ctx);
            let hints = Footer::hints(&[("⏎ choose", 0), ("esc back to the list", 0)]);
            panes::draw_footer(buf, panes.footer, hints, "", theme);
            return;
        }

        let width = usize::from(panes.right.width);
        let height = usize::from(panes.right.height);
        let widths = Widths::fit(
            shown
                .iter()
                .map(|&index| (self.rows[index].key.as_str(), &self.rows[index])),
            width,
        );
        let entries = self.entries(&shown);
        let pointed = shown.get(self.selected).copied();
        let at = entries
            .iter()
            .position(|entry| matches!(entry, Entry::Row(index) if Some(*index) == pointed))
            .unwrap_or(0);
        if at < self.offset {
            // Keep the section's heading in view with its first row.
            self.offset = at.saturating_sub(1);
        } else if height > 0 && at >= self.offset + height {
            self.offset = at + 1 - height;
        }
        let lines: Vec<Line<'static>> = entries
            .iter()
            .skip(self.offset)
            .take(height)
            .map(|entry| match entry {
                Entry::Heading(name) => heading_line(name, "", width, theme),
                Entry::Row(index) => {
                    let row = &self.rows[*index];
                    setting_line(&row.key, row, &widths, Some(*index) == pointed, theme)
                }
            })
            .collect();
        if shown.is_empty() {
            let text = if self.rows.is_empty() {
                "No settings yet"
            } else {
                "Nothing matches"
            };
            let line = Line::from(Span::styled(text, theme.style(Role::Dim)));
            panes::put_lines(buf, panes.right, &[line]);
        } else {
            panes::put_lines(buf, panes.right, &lines);
        }

        let mark = if entries.len() > height && height > 0 {
            rows_on_screen(&entries, self.offset, height)
                .map(|(first, last)| format!("{first}-{last} of {}", shown.len()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let search = if list::vim(ctx.state) && !self.filtering {
            "/ to search"
        } else {
            "type to search"
        };
        let hints = Footer::hints(&[
            ("↑↓  select", 0),
            ("⏎ edit", 2),
            (search, 1),
            (panes::ESC_BACK, 0),
        ]);
        panes::draw_footer(buf, panes.footer, hints, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if let Some(outcome) = edit_key(&mut self.editor, key) {
            return outcome;
        }
        let key = if list::vim(ctx.state) {
            match list::filter_key(key, self.filtering) {
                FilterKey::Start => {
                    self.filtering = true;
                    return ViewOutcome::Consumed;
                }
                FilterKey::Leave => {
                    self.filtering = false;
                    return self.search(String::clear);
                }
                FilterKey::Nothing => return ViewOutcome::Consumed,
                FilterKey::Key(key) => key,
            }
        } else {
            key
        };
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc | KeyCode::Left if !self.filter.is_empty() => self.search(String::clear),
            KeyCode::Esc | KeyCode::Left => ViewOutcome::Close,
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::Home => self.move_by(isize::MIN / 2),
            KeyCode::End => self.move_by(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Right => {
                if let Some(&index) = self.shown().get(self.selected) {
                    let mut editor = flow::edit_picker(self.rows[index].clone());
                    editor.vim = list::vim(ctx.state);
                    self.editor = Some(editor);
                }
                ViewOutcome::Consumed
            }
            KeyCode::Backspace => self.search(|filter| {
                filter.pop();
            }),
            KeyCode::Char(ch) if plain => self.search(|filter| filter.push(ch)),
            _ => ViewOutcome::Ignored,
        }
    }

    /// A paste goes to the editor while one is open, and searches otherwise, as typing does.
    /// With vim keys it searches only after `/`.
    fn handle_paste(&mut self, text: &str, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if let Some(outcome) = edit_paste(&mut self.editor, text) {
            return outcome;
        }
        if list::vim(ctx.state) && !self.filtering {
            return ViewOutcome::Ignored;
        }
        let pasted = one_line(text);
        self.search(|filter| filter.push_str(&pasted))
    }

    /// Fresh rows for every key, such as after a change.
    fn accept(&mut self, data: &ViewData) -> bool {
        let rows = match data {
            ViewData::Settings {
                rows,
                command: Some(command),
                ..
            } if command == "config" => rows.clone(),
            ViewData::Settings {
                rows,
                command: None,
                section: None,
                ..
            } => rows.clone(),
            ViewData::Config { entries } => Self::from_entries(entries).rows,
            _ => return false,
        };
        self.rows = rows;
        self.selected = self.selected.min(self.shown().len().saturating_sub(1));
        true
    }
}
