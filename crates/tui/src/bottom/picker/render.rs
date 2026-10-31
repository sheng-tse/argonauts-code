use std::time::{Duration, Instant};

use argonauts_core::app::model_picker::{EffortLine, EffortSlider, UltracodeToggle};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Picker, PickerRow, RowPart, VISIBLE_ROWS};
use crate::base::draw::layout::cut;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::kit::logo::{LOGO_BEFORE, ModelLabel, NoPicture, logo_before};
use crate::base::kit::{Kit, bars, merge};
use crate::base::text::marks::decorative;
use crate::base::text::{
    At, KEEP, Part, fit_lines, fit_parts, sanitize_line, str_width, wrap_spans, wrap_text,
};
use crate::base::theme::{Role, Theme};
use crate::frame::view::states;

/// Labels past this are cut, so the descriptions keep their room.
const MAX_LABEL: usize = 36;
/// Between the label, description, and letter columns.
const GAP: usize = 3;
/// A narrower description column says too little to be worth drawing.
const MIN_DESCRIPTION: usize = 8;
/// The effort slider's left edge.
const SLIDER_INDENT: usize = 4;
/// Between the slider's levels, where there's room.
const SLIDER_GAP: usize = 6;
/// Between the slider's right end and Ultracode beside it.
const TOGGLE_GAP: usize = 4;
const TOGGLE_HINT: &str = "Tab to toggle";
/// Follows the level of a model that takes one only, as agy's `gemini-3.8-flash-high` does.
const FIXED: &str = "fixed by the model";

/// The marks beyond the theme's glyph set, with ASCII forms.
struct Marks {
    current: &'static str,
    below: &'static str,
    above: &'static str,
    /// Starts the effort line under `/model`'s models.
    effort: &'static str,
    /// Starts the line for a model that takes no effort setting.
    no_effort: &'static str,
    /// Points at the chosen level on the effort slider.
    chosen: &'static str,
}

impl Marks {
    fn for_theme(theme: &Theme) -> Self {
        if theme.glyphs.prompt.is_ascii() {
            Self {
                current: "*",
                below: "v",
                above: "^",
                effort: "*",
                no_effort: "o",
                chosen: "^",
            }
        } else {
            Self {
                current: "✓",
                below: "↓",
                above: "↑",
                effort: "◉",
                no_effort: "○",
                chosen: "▲",
            }
        }
    }
}

impl Picker {
    pub fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let question = theme.style(Role::Question);
        let dim = theme.style(Role::Dim);
        let indent = [Span::raw("  ")];

        let mut lines = vec![Line::from(decorative(
            theme.glyphs.rule.repeat(width),
            question,
        ))];
        let title = sanitize_line(&self.title).trim().to_owned();
        let title_style = question.add_modifier(Modifier::BOLD);
        let mut logo = self
            .title_logo
            .as_ref()
            .map(|logo| logo_before(logo, theme))
            .unwrap_or_default();
        if logo.is_empty() {
            lines.extend(wrap_text(&title, title_style, width, &indent, &indent));
        } else {
            logo.push(Span::styled(title, title_style));
            lines.extend(wrap_spans(&logo, width, &indent, &indent));
        }
        let about = sanitize_line(&self.about);
        if !about.trim().is_empty() {
            lines.extend(wrap_text(about.trim(), dim, width, &indent, &indent));
        }
        // The figures wrap under the label's start, so what holds the memory always shows.
        if let Some((label, figures)) = &self.banner {
            let bold = Modifier::BOLD;
            let said = [
                Span::styled(
                    sanitize_line(label).into_owned(),
                    question.add_modifier(bold),
                ),
                Span::styled(
                    format!(" {}", sanitize_line(figures)),
                    theme.style(Role::Text).add_modifier(bold),
                ),
            ];
            lines.extend(wrap_spans(&said, width, &indent, &indent));
        }
        if let Some(gauge) = &self.gauge {
            lines.push(gauge.line(ctx));
        }
        lines.push(Line::default());

        let shown = self.shown();
        let warning = self.warning.clone().or_else(|| {
            shown
                .get(self.selected)
                .and_then(|&index| self.rows[index].warning.clone())
        });
        if let Some(warning) = warning {
            let warning = sanitize_line(&warning);
            let style = theme.style(Role::Warn);
            lines.extend(wrap_text(warning.trim(), style, width, &indent, &indent));
            lines.push(Line::default());
        }
        if !self.filter.is_empty() || self.filtering {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("Filter: ", dim),
                Span::raw(sanitize_line(&self.filter).into_owned()),
            ]));
        }

        if let Some(slider) = &self.flow.slider {
            lines.extend(slider_lines(slider, ctx));
        } else if let Some(text) = &self.input {
            lines.push(self.input_row(text, ctx));
        } else if shown.is_empty() {
            let text = if self.rows.is_empty() {
                sanitize_line(&self.placeholder).trim().to_owned()
            } else {
                "Nothing matches".to_owned()
            };
            lines.push(Line::from(vec![Span::raw("    "), Span::styled(text, dim)]));
        } else {
            self.push_rows(&mut lines, &shown, ctx);
        }
        if let Some(note) = self.note.as_deref().filter(|_| self.input.is_none()) {
            lines.push(Line::default());
            let note = sanitize_line(note);
            let style = theme.style(Role::Warn);
            lines.extend(wrap_text(note.trim(), style, width, &indent, &indent));
        } else if let Some(line) = self.flow.effort.as_ref().filter(|_| self.input.is_none()) {
            lines.push(Line::default());
            lines.push(self.effort_row(line, ctx));
        }

        lines.push(Line::default());
        lines.push(self.hint_line(ctx));
        fit_lines(&mut lines, width);
        lines
    }

    /// The typed line, its end kept in view when it's longer than the row.
    fn input_row(&self, text: &str, ctx: &RenderCtx<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let opening = format!("  {} ", theme.glyphs.prompt);
        let room = usize::from(ctx.width).saturating_sub(str_width(&opening) + 1);
        let text = sanitize_line(text).into_owned();
        let shown = if str_width(&text) <= room {
            text
        } else {
            let mut tail: Vec<char> = Vec::new();
            let mut used = str_width(theme.glyphs.ellipsis);
            for ch in text.chars().rev() {
                let cells = str_width(ch.encode_utf8(&mut [0; 4]));
                if used + cells > room {
                    break;
                }
                used += cells;
                tail.push(ch);
            }
            let tail: String = tail.into_iter().rev().collect();
            format!("{}{tail}", theme.glyphs.ellipsis)
        };
        Line::from(vec![
            Span::styled(opening, theme.style(Role::Question)),
            Span::raw(shown),
            Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        ])
    }

    fn push_rows(&self, lines: &mut Vec<Line<'static>>, shown: &[usize], ctx: &RenderCtx<'_>) {
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);

        let start = self.offset.min(shown.len().saturating_sub(1));
        let end = (start + VISIBLE_ROWS).min(shown.len());
        let below = shown.len() - end;
        // Every width is measured over the whole list, not the rows a filter leaves or the rows
        // on screen, and each row keeps its number, so nothing moves as you type or scroll.
        let every: Vec<usize> = (0..self.rows.len()).collect();
        let numbers: Vec<String> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| match row.number.as_deref() {
                Some(number) => format!("{} ", sanitize_line(number).trim()),
                None => format!("{}. ", index + 1),
            })
            .collect();
        let number_width = numbers.iter().map(|n| str_width(n)).max().unwrap_or(0);
        // Two spaces, the pointer and its space, then the number.
        let prefix = 4 + number_width;
        // Where logos draw as pictures, a label naming a CLI or a model has its logo before it,
        // in a column of its own. Elsewhere the labels stay as they were, with no dot added.
        let logo_of = |row: &PickerRow| {
            (row.logo.as_ref())
                .map(|logo| logo_before(logo, theme))
                .unwrap_or_default()
        };
        let logos = self.rows.iter().any(|row| !logo_of(row).is_empty());
        let logo_room = if logos { usize::from(LOGO_BEFORE) } else { 0 };
        let shape = Shape {
            label_at: prefix + logo_room,
            width,
        };
        let layout = if self.columns.is_empty() {
            Layout::Described(self.described_layout(&every, &marks, &shape, ctx))
        } else {
            Layout::Table(self.table_layout(&every, &marks, &shape, ctx.theme))
        };
        let headings = self.filter.is_empty();

        for (at, &index) in shown[start..end].iter().enumerate() {
            let row = &self.rows[index];
            let position = start + at;
            match &layout {
                // The first row on screen keeps its group's heading over it, so its columns
                // stay named however far the list has scrolled.
                Layout::Table(table) if headings => {
                    let heading = if at == 0 {
                        self.group_start(index)
                    } else {
                        row.heading.is_some().then_some(index)
                    };
                    if let Some(first) = heading {
                        if at > 0 {
                            lines.push(Line::default());
                        }
                        lines.push(self.heading_line(first, table, ctx));
                    }
                }
                Layout::Described(_) if headings => {
                    if let Some(heading) = row.heading.as_deref() {
                        lines.push(Line::from(vec![
                            Span::raw(" ".repeat(prefix)),
                            Span::styled(sanitize_line(heading).trim().to_owned(), dim),
                        ]));
                    }
                }
                _ => {}
            }
            let (pointer, pointer_style) = if position == self.selected {
                (theme.glyphs.prompt, theme.style(Role::Question))
            } else if at + 1 == end - start && below > 0 {
                (marks.below, dim)
            } else if at == 0 && start > 0 {
                (marks.above, dim)
            } else {
                (" ", dim)
            };
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(format!("{pointer} "), pointer_style),
                Span::styled(format!("{:>number_width$}", numbers[index]), dim),
            ];
            let logo = logo_of(row);
            if !logo.is_empty() {
                spans.extend(logo);
            } else if logo_room > 0 {
                spans.push(Span::raw(" ".repeat(logo_room)));
            }
            let selected = position == self.selected;
            lines.extend(match &layout {
                Layout::Described(layout) => {
                    self.described_row(spans, index, selected, layout, ctx)
                }
                Layout::Table(table) => self.table_row(spans, index, selected, table, ctx),
            });
            if let Some(under) = row.under.as_deref() {
                let under = cut(sanitize_line(under).trim(), width.saturating_sub(prefix));
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(prefix)),
                    Span::styled(under, dim),
                ]));
            }
        }
        if below > 0 {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{} +{below} more", theme.glyphs.ellipsis), dim),
            ]));
        }
    }

    /// The row that heads `index`'s group: the nearest at or above it with a heading.
    fn group_start(&self, index: usize) -> Option<usize> {
        (0..=index)
            .rev()
            .find(|&at| self.rows[at].heading.is_some())
    }

    /// The rows of the group `first` heads, up to the next heading.
    fn group(&self, first: usize) -> impl Iterator<Item = &PickerRow> {
        let rows = &self.rows[first..];
        let len = rows
            .iter()
            .skip(1)
            .position(|row| row.heading.is_some())
            .map_or(rows.len(), |at| at + 1);
        rows[..len].iter()
    }

    /// A group's heading where the number column starts, so it reads as the start of a group
    /// and not as the row above's second line, with the names of the columns its rows fill
    /// over those columns.
    fn heading_line(&self, first: usize, table: &Table, ctx: &RenderCtx<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let name = self.rows[first]
            .heading
            .as_deref()
            .map(|heading| sanitize_line(heading).trim().to_owned())
            .unwrap_or_default();
        let mut used = HEADING_AT + str_width(&name);
        let mut spans = vec![
            Span::raw(" ".repeat(HEADING_AT)),
            Span::styled(name, theme.style(Role::Text)),
        ];
        for (column, place) in self.columns.iter().zip(&table.columns) {
            let named = self.group(first).any(|row| {
                row.cells
                    .get(place.index)
                    .is_some_and(|cell| !cell.is_empty())
            });
            if !named || column.name.is_empty() || place.width == 0 {
                continue;
            }
            let name = cut(&column.name, place.width);
            let at = if column.numbers {
                place.at + place.width - str_width(&name)
            } else {
                place.at
            };
            spans.push(Span::raw(" ".repeat(at.saturating_sub(used).max(1))));
            used = at.max(used + 1) + str_width(&name);
            spans.push(Span::styled(name, theme.style(Role::Dim)));
        }
        Line::from(spans)
    }

    /// The row's `part` as drawn: a model's name worded as it reads beside the title's logo.
    fn worded(&self, row: &PickerRow, part: RowPart, theme: &Theme) -> String {
        let words = match part {
            RowPart::Label => &row.label,
            RowPart::Description => &row.description,
        };
        let words = sanitize_line(words).trim().to_owned();
        match self
            .title_logo
            .as_ref()
            .filter(|_| row.model_in == Some(part))
        {
            Some(logo) => ModelLabel::named(logo, &words, theme, NoPicture::Nothing).name,
            None => words,
        }
    }

    /// The label, as the row draws it, with `✓` after the value in effect.
    fn label_text(&self, index: usize, marks: &Marks, theme: &Theme) -> String {
        let row = &self.rows[index];
        let label = self.worded(row, RowPart::Label, theme);
        if row.current {
            format!("{label} {}", marks.current)
        } else {
            label
        }
    }

    /// The label in its colour, cut to `room`, the `✓` kept.
    fn label_spans(
        &self,
        index: usize,
        selected: bool,
        room: usize,
        marks: &Marks,
        theme: &Theme,
    ) -> (Vec<Span<'static>>, usize) {
        let row = &self.rows[index];
        let success = theme.style(Role::Success);
        let style = if row.current {
            success
        } else if selected {
            theme.style(Role::Question)
        } else {
            theme.style(Role::Text)
        };
        let label = self.worded(row, RowPart::Label, theme);
        if row.current {
            let mark = format!(" {}", marks.current);
            let label = cut(&label, room.saturating_sub(str_width(&mark)));
            let used = str_width(&label) + str_width(&mark);
            (
                vec![Span::styled(label, style), Span::styled(mark, success)],
                used,
            )
        } else {
            let label = cut(&label, room);
            let used = str_width(&label);
            (vec![Span::styled(label, style)], used)
        }
    }

    fn described_layout(
        &self,
        shown: &[usize],
        marks: &Marks,
        shape: &Shape,
        ctx: &RenderCtx<'_>,
    ) -> Described {
        let width = shape.width;
        let keyed = self.rows.iter().any(|row| row.key.is_some());
        let key_room = if keyed { GAP + 1 } else { 0 };
        let aside_width = shown
            .iter()
            .filter_map(|&index| self.aside_text(index).map(|aside| str_width(aside.trim())))
            .max();
        let aside_room = aside_width.map_or(0, |aside| aside + GAP);
        let described = shown
            .iter()
            .map(|&index| {
                let mark = self
                    .state_mark(index, ctx.theme)
                    .map_or(0, |(glyph, _)| str_width(glyph) + 1);
                let (description, tail) = self.description_of(index, ctx.theme);
                let tail = tail.map_or(0, |tail| str_width(SEPARATOR) + str_width(&tail));
                mark + str_width(&description) + tail
            })
            .max()
            .unwrap_or(0);
        let longest = shown
            .iter()
            .map(|&index| str_width(&self.label_text(index, marks, ctx.theme)))
            .max()
            .unwrap_or(0);
        let label_cap = if self.wide_labels {
            let others = key_room + aside_room + if described > 0 { described + GAP } else { 0 };
            width.saturating_sub(shape.label_at + others).max(MAX_LABEL)
        } else {
            MAX_LABEL
        };
        let label_width = longest
            .min(label_cap)
            .min(width.saturating_sub(shape.label_at + key_room));
        let description_at = shape.label_at + label_width + GAP;
        let room = width.saturating_sub(description_at + key_room + aside_room);
        let description_width = if described > 0 && room >= MIN_DESCRIPTION {
            described.min(room)
        } else {
            0
        };
        let key_at = if description_width > 0 {
            description_at + description_width + GAP
        } else {
            shape.label_at + label_width + GAP
        };
        Described {
            label_at: shape.label_at,
            label_width,
            description_at,
            description_width,
            aside_at: key_at,
            key_at: key_at + aside_room,
            width,
        }
    }

    fn aside_text(&self, index: usize) -> Option<String> {
        self.rows[index]
            .aside
            .as_deref()
            .map(|aside| sanitize_line(aside).into_owned())
    }

    /// A row's state glyph comes before its description, so it counts toward the column.
    fn state_mark(&self, index: usize, theme: &Theme) -> Option<(&'static str, Role)> {
        self.rows[index]
            .state
            .map(|state| states::mark(state, theme))
    }

    fn description_of(&self, index: usize, theme: &Theme) -> (String, Option<String>) {
        let row = &self.rows[index];
        let description = self.worded(row, RowPart::Description, theme);
        let tail = row
            .tail
            .as_deref()
            .map(|tail| sanitize_line(tail).trim().to_owned())
            .filter(|tail| !tail.is_empty());
        (description, tail)
    }

    /// A row with a description, and its tail under it where the row can't hold both.
    fn described_row(
        &self,
        mut spans: Vec<Span<'static>>,
        index: usize,
        selected: bool,
        layout: &Described,
        ctx: &RenderCtx<'_>,
    ) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let row = &self.rows[index];
        let (label, drawn) = self.label_spans(index, selected, layout.label_width, &marks, theme);
        spans.extend(label);
        let mut used = layout.label_at + drawn;
        let (description, tail) = self.description_of(index, theme);
        let description_width = layout.description_width;
        let description_at = layout.description_at;
        // The tail goes under the description where the row can't hold both.
        let mut under_description = None;
        if description_width > 0 && !description.is_empty() {
            spans.push(Span::raw(" ".repeat(description_at.saturating_sub(used))));
            let mut drawn = 0;
            if let Some((glyph, role)) = self.state_mark(index, theme) {
                let mark = cut(&format!("{glyph} "), description_width);
                drawn += str_width(&mark);
                spans.push(Span::styled(mark, theme.style(role)));
            }
            let room = description_width - drawn;
            let description = match tail {
                Some(tail) => {
                    let whole = format!("{description}{SEPARATOR}{tail}");
                    if str_width(&whole) <= room {
                        whole
                    } else {
                        under_description = Some(cut(&tail, description_width));
                        description
                    }
                }
                None => description,
            };
            let description = crate::base::text::cut(&description, room, At::Word);
            drawn += str_width(&description);
            used = description_at + drawn;
            spans.extend(with_danger(description, row.danger.as_deref(), dim, theme));
        }
        if let Some(aside) = self
            .aside_text(index)
            .filter(|aside| !aside.trim().is_empty())
        {
            let room = layout.width.saturating_sub(layout.aside_at);
            let aside = cut(aside.trim(), room);
            spans.push(Span::raw(
                " ".repeat(layout.aside_at.saturating_sub(used).max(1)),
            ));
            used = layout.aside_at.max(used + 1) + str_width(&aside);
            spans.push(Span::styled(aside, dim));
        }
        if let Some(key) = row.key {
            spans.push(Span::raw(
                " ".repeat(layout.key_at.saturating_sub(used).max(1)),
            ));
            spans.push(Span::styled(key.to_string(), dim));
        }
        let mut lines = vec![Line::from(spans)];
        if let Some(tail) = under_description {
            let mut spans = vec![Span::raw(" ".repeat(description_at))];
            spans.extend(with_danger(tail, row.danger.as_deref(), dim, theme));
            lines.push(Line::from(spans));
        }
        lines
    }

    fn table_layout(&self, shown: &[usize], marks: &Marks, shape: &Shape, theme: &Theme) -> Table {
        let headed = self.rows.iter().any(|row| row.heading.is_some());
        let longest = shown
            .iter()
            .map(|&index| str_width(&self.label_text(index, marks, theme)))
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL);
        // A heading's name starts left of the labels, and the columns start after the longer.
        let heading = self
            .rows
            .iter()
            .filter_map(|row| row.heading.as_deref())
            .map(|heading| HEADING_AT + str_width(sanitize_line(heading).trim()))
            .max()
            .unwrap_or(0);
        let label_width = longest.max(heading.saturating_sub(shape.label_at));
        let mut at = shape.label_at + label_width + COLUMN_GAP;
        let first = at;
        let mut columns = Vec::new();
        for (index, column) in self.columns.iter().enumerate() {
            let cells = shown
                .iter()
                .filter_map(|&row| self.rows[row].cells.get(index))
                .filter(|cell| !cell.is_empty())
                .map(|cell| cell.width(theme))
                .max();
            let width = match cells {
                Some(cells) if headed => cells.max(str_width(&column.name)),
                Some(cells) => cells,
                None => 0,
            };
            columns.push(Place { index, at, width });
            if width > 0 {
                at += width + COLUMN_GAP;
            }
        }
        Table {
            label_at: shape.label_at,
            label_width,
            first,
            columns,
            end: at,
            width: shape.width,
        }
    }

    /// A row with its cells under the columns, or its description where they start.
    fn table_row(
        &self,
        mut spans: Vec<Span<'static>>,
        index: usize,
        selected: bool,
        table: &Table,
        ctx: &RenderCtx<'_>,
    ) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let row = &self.rows[index];
        let (label, drawn) = self.label_spans(index, selected, table.label_width, &marks, theme);
        spans.extend(label);
        let mut used = table.label_at + drawn;
        if row.cells.iter().all(PickerCell::is_empty) {
            let (description, _) = self.description_of(index, theme);
            if !description.is_empty() {
                spans.push(Span::raw(" ".repeat(table.first.saturating_sub(used))));
                let description = cut(&description, table.width.saturating_sub(table.first));
                used = table.first + str_width(&description);
                spans.extend(with_danger(description, row.danger.as_deref(), dim, theme));
            }
        }
        for (column, place) in self.columns.iter().zip(&table.columns) {
            let Some(cell) = row.cells.get(place.index).filter(|cell| !cell.is_empty()) else {
                continue;
            };
            let drawn = cell.width(theme).min(place.width);
            let at = if column.numbers {
                place.at + place.width - drawn
            } else {
                place.at
            };
            spans.push(Span::raw(" ".repeat(at.saturating_sub(used))));
            spans.extend(cell.spans(place.width, ctx));
            used = at.max(used) + drawn;
        }
        if let Some(key) = row.key {
            spans.push(Span::raw(" ".repeat(table.end.saturating_sub(used).max(1))));
            spans.push(Span::styled(key.to_string(), dim));
        }
        vec![Line::from(spans)]
    }

    /// `◉ xhigh effort (default)  ←/→  to adjust`, for the highlighted model, no hint for one
    /// fixed at its only level, and Claude Code's faint line for one that takes none.
    fn effort_row(&self, line: &EffortLine, ctx: &RenderCtx<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let marks = Marks::for_theme(theme);
        if line.levels.is_empty() {
            return self.no_effort_row(&marks, ctx);
        }
        let default = line.default.and_then(|at| line.levels.get(at));
        let (level, note) = match line.level() {
            Some(level) if line.is_fixed() => (format!("{level} effort"), format!(" ({FIXED})")),
            Some(level) if line.is_default() => (format!("{level} effort"), " (default)".into()),
            Some(level) => (
                format!("{level} effort"),
                default.map_or_else(String::new, |default| format!(" (default {default})")),
            ),
            None => ("the CLI's own effort".to_owned(), String::new()),
        };
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{} ", marks.effort), theme.style(Role::Question)),
            Span::styled(sanitize_line(&level).into_owned(), theme.style(Role::Text)),
            Span::styled(sanitize_line(&note).into_owned(), dim),
        ];
        if !line.is_fixed() {
            spans.push(Span::styled(format!("  {}", self.adjust_hint()), dim));
        }
        Line::from(spans)
    }

    /// Claude Code's line for a model that takes no effort setting, in its faint colour:
    /// `○ Effort not supported for Haiku 4.5`.
    fn no_effort_row(&self, marks: &Marks, ctx: &RenderCtx<'_>) -> Line<'static> {
        let model = self
            .pointed()
            .map(|index| sanitize_line(&self.rows[index].label).trim().to_owned())
            .unwrap_or_default();
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{} Effort not supported for {model}", marks.no_effort),
                ctx.theme.style(Role::Faint),
            ),
        ])
    }

    /// Ghostty with Iosevka draws a side arrow two cells wide, so words after one keep two
    /// cells away.
    fn adjust_hint(&self) -> &'static str {
        if self.vim {
            "h/l to adjust"
        } else {
            "←/→  to adjust"
        }
    }

    fn hint_line(&self, ctx: &RenderCtx<'_>) -> Line<'static> {
        let dim = ctx.theme.style(Role::Dim);
        let part = |text: &str| vec![Span::styled(text.to_owned(), dim)];
        let dot = || Span::styled(" · ", dim);
        let enter = if self.enter_hint.is_empty() {
            "Enter to choose"
        } else {
            self.enter_hint.as_str()
        };
        let keyed = self.rows.iter().any(|row| row.key.is_some());
        let esc = |usual: &'static str| {
            if self.esc_hint.is_empty() {
                usual
            } else {
                self.esc_hint.as_str()
            }
        };
        let mut parts = Vec::new();
        let unmoving = self.flow.slider.as_ref().filter(|slider| !slider.moves());
        if let Some(slider) = unmoving {
            // Tab is named beside Ultracode, where the view offers it.
            if slider.ultracode.is_some() {
                parts.push(Part::new(part(enter), KEEP));
                parts.push(Part::joined(dot(), part(esc("Esc to cancel")), 1));
            } else {
                parts.push(Part::new(part(esc("Esc to close")), KEEP));
            }
        } else if self.flow.slider.is_some() {
            parts.push(Part::new(part(self.adjust_hint()), KEEP));
            parts.push(Part::joined(dot(), part(enter), KEEP));
            parts.push(Part::joined(dot(), part(esc("Esc to cancel")), 1));
        } else if self.input.is_some() {
            parts.push(Part::new(part(enter), KEEP));
            parts.push(Part::joined(dot(), part(esc("Esc to cancel")), 1));
        } else if self.rows.is_empty() {
            parts.push(Part::new(part(esc("Esc to close")), KEEP));
        } else if keyed {
            parts.push(Part::new(part(enter), KEEP));
            parts.push(Part::joined(dot(), part(esc("Esc to cancel")), 1));
        } else if self.filtering {
            parts.push(Part::new(part(enter), KEEP));
            parts.push(Part::joined(dot(), part("↑↓  to move"), 0));
            parts.push(Part::joined(dot(), part("Esc to clear the filter"), 1));
        } else {
            let (moving, filtering) = if self.vim {
                ("j/k to move", "/ to filter")
            } else {
                ("↑↓  to move", "type to filter")
            };
            parts.push(Part::new(part(enter), KEEP));
            parts.push(Part::joined(dot(), part(moving), 0));
            if self.rows.len() > 1 {
                parts.push(Part::joined(dot(), part(filtering), 0));
            }
            parts.push(Part::joined(dot(), part(esc("Esc to close")), 1));
        }
        fit_parts(vec![Span::raw("  ")], &parts, usize::from(ctx.width))
    }
}

/// Between a description and its tail.
const SEPARATOR: &str = " \u{b7} ";

/// A dim description with `danger`, where it holds those words, in the danger colour.
fn with_danger(
    description: String,
    danger: Option<&str>,
    dim: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let split = danger.filter(|words| !words.is_empty()).and_then(|words| {
        let (before, after) = description.split_once(words)?;
        Some((before.to_owned(), words.to_owned(), after.to_owned()))
    });
    match split {
        Some((before, words, after)) => vec![
            Span::styled(before, dim),
            Span::styled(words, theme.style(Role::Danger)),
            Span::styled(after, dim),
        ],
        None => vec![Span::styled(description, dim)],
    }
}

/// `/effort`'s slider: "Faster" and "Smarter" over the ends of a line, a marker on it over the
/// chosen level, the levels under it, and what the chosen one means.
fn slider_lines(slider: &EffortSlider, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    if !slider.moves() {
        return fixed_level_lines(slider, ctx);
    }
    let theme = ctx.theme;
    let marks = Marks::for_theme(theme);
    let dim = theme.style(Role::Dim);
    let question = theme.style(Role::Question);
    let width = usize::from(ctx.width);
    let labels: Vec<String> = slider
        .stops
        .iter()
        .map(|stop| sanitize_line(&stop.level).trim().to_owned())
        .collect();
    let Some(last) = labels.len().checked_sub(1) else {
        return Vec::new();
    };
    let chosen = slider.chosen.min(last);
    let widths: Vec<usize> = labels.iter().map(|label| str_width(label)).collect();
    let total: usize = widths.iter().sum();
    let gaps = labels.len().saturating_sub(1);
    let (faster, smarter) = (EffortSlider::FASTER, EffortSlider::SMARTER);
    let ends = str_width(faster) + 1 + str_width(smarter);
    let room = width.saturating_sub(SLIDER_INDENT + 2);
    let scale = (total + gaps * SLIDER_GAP)
        .max(ends)
        .min(room)
        .max(total + gaps);
    // The levels spread evenly from one end of the line to the other.
    let spare = scale - total;
    let mut starts = Vec::with_capacity(labels.len());
    let share = |gap: usize| (spare * gap).checked_div(gaps);
    let mut at = if gaps == 0 { spare / 2 } else { 0 };
    for (index, cells) in widths.iter().enumerate() {
        starts.push(at);
        if let (Some(next), Some(this)) = (share(index + 1), share(index)) {
            at += cells + next - this;
        }
    }
    let indent = || Span::raw(" ".repeat(SLIDER_INDENT));
    let marker_at = starts[chosen] + widths[chosen].saturating_sub(1) / 2;

    let mut lines = vec![
        Line::from(vec![
            indent(),
            Span::styled(faster, dim),
            Span::raw(" ".repeat(scale.saturating_sub(ends) + 1)),
            Span::styled(smarter, dim),
        ]),
        Line::from(vec![
            indent(),
            Span::styled(theme.glyphs.rule.repeat(marker_at), dim),
            Span::styled(
                marks.chosen,
                if slider.cli_default { dim } else { question },
            ),
            Span::styled(
                theme
                    .glyphs
                    .rule
                    .repeat(scale.saturating_sub(marker_at + 1)),
                dim,
            ),
        ]),
    ];
    let mut levels = vec![indent()];
    let mut used = 0;
    for (index, label) in labels.iter().enumerate() {
        levels.push(Span::raw(" ".repeat(starts[index].saturating_sub(used))));
        let style = if index == chosen && !slider.cli_default {
            question.add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        levels.push(Span::styled(label.clone(), style));
        used = starts[index] + widths[index];
    }
    lines.push(Line::from(levels));
    if let Some(toggle) = slider.ultracode {
        add_ultracode(&mut lines, toggle, scale, ctx);
    }
    lines.push(Line::default());
    let description = sanitize_line(&slider.stops[chosen].description)
        .trim()
        .to_owned();
    let description = match (description.is_empty(), slider.default == Some(chosen)) {
        // As `/model` says it: nothing is set, so the marker only shows where ← and → start.
        _ if slider.cli_default => "The CLI's own effort, until you pick a level".to_owned(),
        (true, true) => "The model's default".to_owned(),
        (true, false) => String::new(),
        (false, true) => format!("{}. The model's default", description.trim_end_matches('.')),
        (false, false) => description,
    };
    if !description.is_empty() {
        let margin = [indent()];
        lines.extend(wrap_text(
            &description,
            theme.style(Role::Text),
            width,
            &margin,
            &margin,
        ));
    }
    lines
}

/// Ultracode beside the slider, where Claude Code puts Ultracode: its name and state level with
/// the line and `Tab to toggle` under them, else one row under the levels where there's no room
/// beside it. While it's on, a line under the levels says what it does.
fn add_ultracode(
    lines: &mut Vec<Line<'static>>,
    toggle: UltracodeToggle,
    scale: usize,
    ctx: &RenderCtx<'_>,
) {
    let column = str_width(UltracodeToggle::NAME) + 2 + str_width("off");
    let end = SLIDER_INDENT + scale;
    // The rule row and the levels row are the last two, each `end` cells wide.
    let rows = lines.len();
    if end + TOGGLE_GAP + column.max(str_width(TOGGLE_HINT)) <= usize::from(ctx.width) {
        let pad =
            |line: &Line<'static>| " ".repeat((end + TOGGLE_GAP).saturating_sub(line.width()));
        let gap = pad(&lines[rows - 2]);
        lines[rows - 2].spans.push(Span::raw(gap));
        lines[rows - 2].spans.extend(ultracode_named(toggle, ctx));
        let gap = pad(&lines[rows - 1]);
        lines[rows - 1].spans.push(Span::raw(gap));
        lines[rows - 1]
            .spans
            .push(Span::styled(TOGGLE_HINT, ctx.theme.style(Role::Dim)));
    } else {
        lines.push(ultracode_row(toggle, ctx));
    }
    lines.extend(ultracode_means(toggle, ctx));
}

/// `Ultracode  on`, brighter while it's on.
fn ultracode_named(toggle: UltracodeToggle, ctx: &RenderCtx<'_>) -> Vec<Span<'static>> {
    let theme = ctx.theme;
    let (name, state) = if toggle.on {
        let label = theme.style(Role::Label);
        (label, label.add_modifier(Modifier::BOLD))
    } else {
        (theme.style(Role::Text), theme.style(Role::Dim))
    };
    let word = if toggle.on { "on" } else { "off" };
    vec![
        Span::styled(UltracodeToggle::NAME, name),
        Span::raw("  "),
        Span::styled(word, state),
    ]
}

/// Ultracode on a row of its own, with `Tab to toggle` after it.
fn ultracode_row(toggle: UltracodeToggle, ctx: &RenderCtx<'_>) -> Line<'static> {
    let mut row = vec![Span::raw(" ".repeat(SLIDER_INDENT))];
    row.extend(ultracode_named(toggle, ctx));
    row.push(Span::styled(
        format!(" · {TOGGLE_HINT}"),
        ctx.theme.style(Role::Dim),
    ));
    Line::from(row)
}

/// What Ultracode does, only while it's on.
fn ultracode_means(toggle: UltracodeToggle, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    if !toggle.on {
        return Vec::new();
    }
    let margin = [Span::raw(" ".repeat(SLIDER_INDENT))];
    let means = format!("{}: {}", UltracodeToggle::NAME, UltracodeToggle::MEANS);
    wrap_text(
        &means,
        ctx.theme.style(Role::Dim),
        usize::from(ctx.width),
        &margin,
        &margin,
    )
}

/// A model that takes one level has no slider: the level, as `/model` shows it, and how to get
/// another. One that takes none says so. Either way, the main agent's view still offers
/// Ultracode.
fn fixed_level_lines(slider: &EffortSlider, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let text = theme.style(Role::Text);
    let width = usize::from(ctx.width);
    let indent = || Span::raw(" ".repeat(SLIDER_INDENT));
    let margin = [indent()];
    let (mut lines, then) = match slider.level() {
        Some(level) => {
            let marks = Marks::for_theme(theme);
            let level = sanitize_line(level).trim().to_owned();
            let line = Line::from(vec![
                indent(),
                Span::styled(format!("{} ", marks.effort), theme.style(Role::Question)),
                Span::styled(format!("{level} effort"), text),
                Span::styled(format!(" ({FIXED})"), theme.style(Role::Dim)),
            ]);
            (vec![line], "To change it, pick another model with /model.")
        }
        None => {
            let model = sanitize_line(&slider.model).trim().to_owned();
            let says = format!("{model} takes no effort setting.");
            (
                wrap_text(&says, text, width, &margin, &margin),
                "To set an effort, pick another model with /model.",
            )
        }
    };
    lines.push(Line::default());
    if let Some(toggle) = slider.ultracode {
        lines.push(ultracode_row(toggle, ctx));
        lines.extend(ultracode_means(toggle, ctx));
        lines.push(Line::default());
    }
    lines.extend(wrap_text(then, text, width, &margin, &margin));
    lines
}

/// Where a heading's name starts: where the rows' numbers do, left of their labels.
const HEADING_AT: usize = 4;
/// Between a table's columns, and between its labels and its first column.
const COLUMN_GAP: usize = 2;
/// A plan window's meter in a cell, as the bottom line draws one.
const METER_CELLS: usize = 6;
/// The memory gauge's length where the line has room: long enough that a gigabyte shows on a
/// machine of 64 GB.
const GAUGE_CELLS: usize = 52;
/// A gauge shorter than this says too little to be worth drawing.
const MIN_GAUGE: usize = 12;
/// How long the gauge takes to reach a newly pointed row.
const GAUGE_SPRING: Duration = Duration::from_millis(200);
const GAUGE_FRAME: Duration = Duration::from_millis(33);
/// How stiff the spring is: critically damped, so it settles without overshooting, nearly
/// there by the end of [`GAUGE_SPRING`].
const GAUGE_STIFFNESS: f64 = 30.0;

/// A column of a picker's cells.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PickerColumn {
    /// What a group's heading calls it, such as "context". A heading names only the columns
    /// its own rows fill.
    pub name: String,
    /// Numbers, lined up on their right edge.
    pub numbers: bool,
}

impl PickerColumn {
    pub fn words(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            numbers: false,
        }
    }

    pub fn numbers(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            numbers: true,
        }
    }
}

/// One row's entry under a column.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PickerCell {
    pub text: String,
    /// The colour of words that say what the rest of the row doesn't, such as whether a model
    /// fits. Dim otherwise.
    pub role: Option<Role>,
    /// A meter after the text, at this share, such as how much of a plan window is used.
    pub meter: Option<f64>,
    /// The text names a model on a row showing this logo, so it drops the model's maker's word
    /// where the logo draws as a picture.
    pub beside: Option<argonauts_core::look::logos::Logo>,
}

impl PickerCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn in_role(text: impl Into<String>, role: Role) -> Self {
        Self {
            text: text.into(),
            role: Some(role),
            ..Self::default()
        }
    }

    /// `5h ▄▄▄▄▄▖`: a window's name and its meter.
    pub fn meter(text: impl Into<String>, share: f64) -> Self {
        Self {
            text: text.into(),
            meter: Some(share),
            ..Self::default()
        }
    }

    /// A model's whole name on a row showing `logo`.
    pub fn model_beside(text: impl Into<String>, logo: argonauts_core::look::logos::Logo) -> Self {
        Self {
            text: text.into(),
            beside: Some(logo),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.meter.is_none()
    }

    /// The text as drawn, a model's name worded as it reads beside its row's logo.
    fn shown(&self, theme: &Theme) -> String {
        let text = sanitize_line(&self.text).trim().to_owned();
        match &self.beside {
            Some(logo) => ModelLabel::named(logo, &text, theme, NoPicture::Nothing).name,
            None => text,
        }
    }

    fn width(&self, theme: &Theme) -> usize {
        let text = str_width(&self.shown(theme));
        match self.meter {
            Some(_) if text > 0 => text + 1 + METER_CELLS,
            Some(_) => METER_CELLS,
            None => text,
        }
    }

    fn spans(&self, room: usize, ctx: &RenderCtx<'_>) -> Vec<Span<'static>> {
        let theme = ctx.theme;
        let style = theme.style(self.role.unwrap_or(Role::Dim));
        let text = self.shown(theme);
        let Some(share) = self.meter else {
            return vec![Span::styled(cut(&text, room), style)];
        };
        if self.width(theme) > room {
            return vec![Span::styled(cut(&text, room), style)];
        }
        let mut spans = Vec::new();
        if !text.is_empty() {
            spans.push(Span::styled(format!("{text} "), style));
        }
        spans.extend(bars::window_meter(
            &Kit::new(theme),
            share,
            METER_CELLS,
            Role::Question,
        ));
        spans
    }
}

/// What the row under the pointer takes of a whole, drawn under the banner: solid for the part
/// that fits now, hatched in the warning colour for the part that doesn't, on a track as long
/// as the whole.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gauge {
    /// Dim, before the bar: "this pick".
    pub label: String,
    /// Shares of the whole, 0 to 1: where the solid part ends, and where the hatched part does.
    pub fits: f64,
    pub needs: f64,
    /// After the bar: "21 of 52 GB".
    pub words: String,
    /// After the words, in the warning colour: "3 GB more than is free now".
    pub short: Option<String>,
    /// Where the bar's ends were when the pointer moved, and when, so the bar springs to its
    /// new ends rather than jumping.
    pub moved: Option<(f64, f64, Instant)>,
}

impl Gauge {
    /// This gauge, springing from where `before` stands at `now`.
    pub fn moving_from(mut self, before: &Gauge, now: Instant) -> Self {
        let (fits, needs) = before.ends_at(now, false);
        if (fits, needs) != (self.fits, self.needs) {
            self.moved = Some((fits, needs, now));
        }
        self
    }

    /// Where the ends stand at `now`: on their way from where they were, or where they are.
    fn ends_at(&self, now: Instant, still: bool) -> (f64, f64) {
        let Some((fits, needs, at)) = self.moved.filter(|_| !still) else {
            return (self.fits, self.needs);
        };
        let elapsed = now.saturating_duration_since(at);
        if elapsed >= GAUGE_SPRING {
            return (self.fits, self.needs);
        }
        let t = elapsed.as_secs_f64() * GAUGE_STIFFNESS;
        let eased = 1.0 - (1.0 + t) * (-t).exp();
        (
            fits + (self.fits - fits) * eased,
            needs + (self.needs - needs) * eased,
        )
    }

    fn springing(&self, now: Instant) -> bool {
        self.moved
            .is_some_and(|(_, _, at)| now.saturating_duration_since(at) < GAUGE_SPRING)
    }

    fn line(&self, ctx: &RenderCtx<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let label = sanitize_line(&self.label).trim().to_owned();
        let words = sanitize_line(&self.words).trim().to_owned();
        let short = self
            .short
            .as_deref()
            .map(|short| sanitize_line(short).trim().to_owned());
        let said = str_width(&words)
            + short
                .as_ref()
                .map_or(0, |s| str_width(SEPARATOR) + str_width(s));
        let room = width.saturating_sub(2 + str_width(&label) + 2 + 2 + said);
        let cells = room.min(GAUGE_CELLS);
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(label, theme.style(Role::Dim)),
            Span::raw("  "),
        ];
        if cells >= MIN_GAUGE {
            let (fits, needs) = self.ends_at(ctx.clock.now, theme.reduced_motion);
            spans.extend(gauge_bar(&Kit::new(theme), fits, needs, cells));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(words, theme.style(Role::Text)));
        if let Some(short) = short {
            spans.push(Span::styled(SEPARATOR, theme.style(Role::Text)));
            spans.push(Span::styled(short, theme.style(Role::Warn)));
        }
        Line::from(spans)
    }
}

/// The gauge's cells: solid to `fits`, hatched to `needs`, the track after.
fn gauge_bar(kit: &Kit<'_>, fits: f64, needs: f64, cells: usize) -> Vec<Span<'static>> {
    let end = |share: f64| ((share.clamp(0.0, 1.0) * cells as f64).round() as usize).min(cells);
    let solid = end(fits);
    let hatched = end(needs).max(solid);
    let ascii = kit.theme.glyphs.is_ascii();
    let colour = kit.has_color();
    let cell = |at: usize| {
        if at < solid {
            match (colour, ascii) {
                (true, _) => Span::styled(" ", kit.fill(Role::Question)),
                (false, true) => Span::raw("#"),
                (false, false) => Span::raw("█"),
            }
        } else if at < hatched {
            let glyph = if ascii { "/" } else { "╱" };
            Span::styled(glyph, kit.cell(Role::Warn, Role::UserBand))
        } else {
            match (colour, ascii) {
                (true, _) => Span::styled(" ", kit.fill(Role::UserBand)),
                (false, true) => Span::raw("."),
                (false, false) => Span::raw("░"),
            }
        }
    };
    merge((0..cells).map(cell).collect())
}

/// What each row's parts are placed by, the same down the whole list.
struct Shape {
    /// Where labels start, after the pointer, the number and any logo.
    label_at: usize,
    width: usize,
}

enum Layout {
    Described(Described),
    Table(Table),
}

/// Rows with a description column, then an aside and a letter.
struct Described {
    label_at: usize,
    label_width: usize,
    description_at: usize,
    description_width: usize,
    aside_at: usize,
    key_at: usize,
    width: usize,
}

/// Rows with cells under the picker's columns.
struct Table {
    label_at: usize,
    label_width: usize,
    /// Where the first column starts, and the description of a row with no cells.
    first: usize,
    columns: Vec<Place>,
    /// After the last column, where a letter goes.
    end: usize,
    width: usize,
}

/// A column's place: which of the picker's columns, where it starts, and its width, none for a
/// column no row fills.
struct Place {
    index: usize,
    at: usize,
    width: usize,
}

impl Picker {
    /// When the picker next changes by itself: while the gauge springs to a new row.
    pub fn next_change(&self, clock: &Clock, theme: &Theme) -> Option<Instant> {
        let gauge = self.gauge.as_ref()?;
        (!theme.reduced_motion && gauge.springing(clock.now))
            .then(|| clock.next_frame_at(GAUGE_FRAME))
    }
}
