//! A workflow's setup sheet, in place of the input: the rule, `◆ Set up Council` and what the
//! workflow does, a row for each step or seat with who fills it and how it stands, How tasks
//! are done, the notes, and the keys. The app builds every row; the sheet moves the pointer,
//! sends each change, and takes the sheet back with the change made. Enter saves everything at
//! once and switches; Esc leaves it with nothing saved.
//!
//! The columns sit where the pictures put them: on a workflow's steps the role word at 16 and
//! who fills it at 32, on Council's seats who sits at 25 and the seat's role at 47. A row's
//! state ends three cells short of the right edge. The `‹ ›` of the value the arrows change
//! sit in the two cells before and after it, so every row's names start in one column.

use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::setup_sheet::{
    AsideRow, FillRow, Part, SetupSheet, SheetAction, SheetColumn, SheetLayout, SheetRow, Tone,
    WayRow,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::{View, ViewCtx, ViewOutcome, list};
use crate::kit::logo::{LOGO_WIDTH, ModelLabel, NoPicture};
use crate::text::{sanitize_line, truncate_spans};
use crate::theme::{Role, Theme};

/// Where the pointer's `❯` sits, and where a row's label starts.
const POINTER: u16 = 2;
const LABEL: u16 = 4;
/// Cells from the last cell of a row's state to the right edge.
const RIGHT: u16 = 2;
/// A way's name, value and hint, from the column its `●` sits in.
const WAY_NAME: u16 = 3;
const WAY_VALUE: u16 = 16;
const WAY_HINT: u16 = 24;
/// The width an under line's label takes, so what follows it lines up.
const UNDER_LABEL: usize = 10;
const OPEN: &str = "‹ ";
const SHUT: &str = " ›";

/// A way allowed, and one kept out of every plan, or a seat to add: shape first, colour second.
fn mark(theme: &Theme, on: bool) -> &'static str {
    match (theme.glyphs.is_ascii(), on) {
        (false, true) => "●",
        (false, false) => "○",
        (true, true) => "*",
        (true, false) => "o",
    }
}

/// Where a layout's columns start.
#[derive(Debug, Clone, Copy)]
struct Columns {
    /// The role word on steps, the seat's role on seats.
    role: u16,
    who: u16,
    what: u16,
}

impl Columns {
    fn of(layout: SheetLayout) -> Self {
        match layout {
            SheetLayout::Steps => Self {
                role: 16,
                who: 32,
                what: 74,
            },
            SheetLayout::Seats => Self {
                role: 47,
                who: 25,
                what: 62,
            },
        }
    }
}

#[derive(Debug)]
pub struct SetupSheetView {
    sheet: SetupSheet,
    /// The row the pointer is on, by its id.
    at: Option<String>,
    column: SheetColumn,
}

impl SetupSheetView {
    pub fn new(sheet: SetupSheet) -> Self {
        let mut view = Self {
            sheet,
            at: None,
            column: SheetColumn::Who,
        };
        view.take_pointer();
        view
    }

    /// The pointer where the sheet puts it, else where it was, else on the first row it can be.
    fn take_pointer(&mut self) {
        if let Some(pointer) = self.sheet.pointer.clone()
            && self.ids().contains(&pointer.row)
        {
            self.at = Some(pointer.row);
            self.column = pointer.column;
            return;
        }
        let ids = self.ids();
        if self.at.as_ref().is_none_or(|at| !ids.contains(at)) {
            self.at = ids.first().cloned();
        }
        let columns = self.columns_here();
        if !columns.contains(&self.column) {
            self.column = columns.first().copied().unwrap_or_default();
        }
    }

    /// Every row the pointer can be on, in order.
    fn ids(&self) -> Vec<String> {
        self.sheet.rows.iter().filter_map(row_id).collect()
    }

    fn columns_here(&self) -> Vec<SheetColumn> {
        let Some(at) = &self.at else {
            return Vec::new();
        };
        let row = self
            .sheet
            .rows
            .iter()
            .find(|row| row_id(row).as_ref() == Some(at));
        match row {
            Some(SheetRow::Fill(fill)) if !fill.columns.is_empty() => fill.columns.clone(),
            Some(SheetRow::Way(way)) if way.value.is_some() => vec![SheetColumn::Who],
            _ => Vec::new(),
        }
    }

    fn step(&mut self, by: isize) {
        let ids = self.ids();
        let Some(here) = self
            .at
            .as_ref()
            .and_then(|at| ids.iter().position(|id| id == at))
        else {
            return;
        };
        let next = here
            .saturating_add_signed(by)
            .min(ids.len().saturating_sub(1));
        if next != here {
            self.at = Some(ids[next].clone());
            let columns = self.columns_here();
            if !columns.contains(&self.column) {
                self.column = columns.first().copied().unwrap_or_default();
            }
        }
    }

    fn send(action: SheetAction) -> ViewOutcome {
        ViewOutcome::Send(UiCommand::SetupSheet { action })
    }

    /// Every line the sheet draws at `width`, as `(x, text, style)` runs per row.
    fn lines(&self, width: u16, theme: &Theme) -> Vec<Vec<Run>> {
        let sheet = &self.sheet;
        let cols = Columns::of(sheet.layout);
        let end = width.saturating_sub(RIGHT + 1);
        let text = theme.style(Role::Text);
        let dim = theme.style(Role::Dim);
        let lav = theme.style(Role::Question);
        let mut lines: Vec<Vec<Run>> = Vec::new();
        lines.push(vec![Run::at(
            0,
            theme.glyphs.rule.repeat(usize::from(width)),
            theme.style(Role::Rule),
        )]);
        lines.push(vec![
            Run::at(0, theme.glyphs.question.to_owned(), lav),
            Run::at(
                2,
                sanitize_line(&sheet.title),
                text.add_modifier(Modifier::BOLD),
            ),
        ]);
        for line in &sheet.about {
            lines.extend(wrapped(line, 2, width, text));
        }
        if !sheet.status.is_empty() {
            let mut runs = Vec::new();
            let mut x = 2;
            for words in &sheet.status {
                let run = Run::at(x, sanitize_line(&words.text), tone(theme, words.tone));
                x = run.end();
                runs.push(run);
            }
            lines.push(runs);
        }
        lines.push(Vec::new());
        for row in &sheet.rows {
            let pointed = row_id(row).is_some_and(|id| self.at.as_ref() == Some(&id));
            let mut runs = Vec::new();
            if pointed {
                runs.push(Run::at(POINTER, theme.glyphs.prompt.to_owned(), lav));
            }
            match row {
                SheetRow::Fill(fill) => {
                    self.fill(&mut runs, fill, pointed, cols, end, theme);
                    lines.push(runs);
                    if pointed {
                        for under in &fill.under {
                            let mut runs = Vec::new();
                            let mut x = cols.who;
                            if !under.label.is_empty() {
                                let label = format!("{:<UNDER_LABEL$}", under.label);
                                runs.push(Run::at(x, label, dim));
                                x += UNDER_LABEL as u16;
                            }
                            parts(&mut runs, x, &under.parts, dim, theme);
                            lines.push(runs);
                        }
                    }
                }
                SheetRow::Aside(aside) => {
                    aside_row(&mut runs, aside, cols, theme);
                    lines.push(runs);
                }
                SheetRow::Heading { label, text: words } => {
                    runs.push(Run::at(LABEL, sanitize_line(label), text));
                    runs.push(Run::at(cols.who, sanitize_line(words), text));
                    lines.push(runs);
                }
                SheetRow::Way(way) => {
                    way_row(&mut runs, way, pointed, cols, theme);
                    lines.push(runs);
                }
                SheetRow::Blank => lines.push(runs),
            }
        }
        lines.push(Vec::new());
        for note in &sheet.notes {
            lines.extend(wrapped(note, 2, width, dim));
        }
        if !sheet.notes.is_empty() {
            lines.push(Vec::new());
        }
        lines.extend(wrapped(&sheet.keys, 2, width, dim));
        lines
    }

    fn fill(
        &self,
        runs: &mut Vec<Run>,
        fill: &FillRow,
        pointed: bool,
        cols: Columns,
        end: u16,
        theme: &Theme,
    ) {
        let text = theme.style(Role::Text);
        let lav = theme.style(Role::Question);
        let chosen = |column: SheetColumn| {
            pointed && fill.columns.contains(&column) && self.column == column
        };
        runs.push(Run::at(LABEL, sanitize_line(&fill.label), text));
        let role_style = if chosen(SheetColumn::Role) {
            lav
        } else {
            tone(theme, fill.role.tone)
        };
        let role = Run::at(cols.role, sanitize_line(&fill.role.text), role_style);
        if chosen(SheetColumn::Role) {
            runs.push(Run::at(cols.role - 2, OPEN.to_owned(), lav));
            runs.push(Run::at(role.end(), SHUT.to_owned(), lav));
        }
        runs.push(role);
        let who_style = if chosen(SheetColumn::Who) && fill.lit {
            lav
        } else {
            tone(theme, fill.who_tone)
        };
        let after = parts(runs, cols.who, &fill.who, who_style, theme);
        if chosen(SheetColumn::Who) {
            runs.push(Run::at(cols.who - 2, OPEN.to_owned(), lav));
            runs.push(Run::at(after, SHUT.to_owned(), lav));
        }
        let state = fill.state.as_ref().map(|state| {
            let state_text = sanitize_line(&state.text);
            let x = (end + 1).saturating_sub(width_of(&state_text));
            Run::at(x, state_text, tone(theme, state.tone))
        });
        // On a narrower screen what a step does is cut short of its state.
        let limit = state
            .as_ref()
            .map_or(end + 1, |state| state.x.saturating_sub(2));
        let mut x = cols.what;
        for words in &fill.what {
            if x >= limit {
                break;
            }
            let style = tone(theme, words.tone);
            let cut = truncate_spans(
                &[Span::raw(sanitize_line(&words.text).into_owned())],
                usize::from(limit - x),
            );
            let run = Run::at(
                x,
                cut.iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                style,
            );
            x = run.end();
            runs.push(run);
        }
        runs.extend(state);
    }
}

/// Names and words from `x` on, each logo before its name. Returns where they end.
fn parts(runs: &mut Vec<Run>, mut x: u16, parts: &[Part], style: Style, theme: &Theme) -> u16 {
    for part in parts {
        match part {
            Part::Named { logo, name, number } => {
                let label = ModelLabel::whole(logo, &sanitize_line(name), theme, NoPicture::Dot);
                for span in numbered(label.logo, *number, theme) {
                    let run = Run::at(x, span.content.into_owned(), span.style);
                    x = run.end();
                    runs.push(run);
                }
                let run = Run::at(x, label.name, style);
                x = run.end();
                runs.push(run);
            }
            Part::Text { text } => {
                let run = Run::at(x, sanitize_line(text), style);
                x = run.end();
                runs.push(run);
            }
        }
    }
    x
}

/// A label's logo cells and the blank after them. A model that sits more than once shows its
/// seat's number in the dot's place, in the same colour, so `· 1` and `· 2` read apart at a
/// glance.
fn numbered(
    mut spans: Vec<ratatui::text::Span<'static>>,
    number: Option<u8>,
    theme: &Theme,
) -> Vec<ratatui::text::Span<'static>> {
    if let (Some(number), Some(first)) = (number, spans.first_mut()) {
        let circled = char::from_u32(0x2460 + u32::from(number.clamp(1, 20)) - 1);
        let digit = match circled {
            Some(circled) if !theme.glyphs.is_ascii() => circled.to_string(),
            _ => number.to_string(),
        };
        first.content = digit.into();
    }
    debug_assert_eq!(
        spans.iter().map(|span| span.width()).sum::<usize>(),
        usize::from(LOGO_WIDTH) + 1
    );
    spans
}

fn aside_row(runs: &mut Vec<Run>, aside: &AsideRow, cols: Columns, theme: &Theme) {
    let dim = theme.style(Role::Dim);
    let mut name = sanitize_line(&aside.name).into_owned();
    match &aside.logo {
        Some(logo) => {
            let label = ModelLabel::whole(logo, &name, theme, NoPicture::Dot);
            let mut x = cols.who;
            for span in label.logo {
                let run = Run::at(x, span.content.into_owned(), span.style);
                x = run.end();
                runs.push(run);
            }
            name = label.name;
        }
        None => runs.push(Run::at(cols.who, mark(theme, false).to_owned(), dim)),
    }
    runs.push(Run::at(cols.who + LOGO_WIDTH + 1, name, dim));
    let at = match Columns::of(SheetLayout::Seats).role == cols.role {
        true => cols.role,
        false => cols.what,
    };
    parts(runs, at, &aside.parts, dim, theme);
}

fn way_row(runs: &mut Vec<Run>, way: &WayRow, pointed: bool, cols: Columns, theme: &Theme) {
    let text = theme.style(Role::Text);
    let lav = theme.style(Role::Question);
    let at = if way.inner {
        cols.who + WAY_VALUE
    } else {
        cols.who
    };
    let style = if way.allowed {
        theme.style(Role::Success)
    } else {
        theme.style(Role::Dim)
    };
    runs.push(Run::at(at, mark(theme, way.allowed).to_owned(), style));
    runs.push(Run::at(at + WAY_NAME, sanitize_line(&way.name), text));
    if let Some(value) = &way.value {
        let value = Run::at(cols.who + WAY_VALUE, sanitize_line(value), text);
        if pointed {
            runs.push(Run::at(value.x - 2, OPEN.to_owned(), lav));
            runs.push(Run::at(value.end(), SHUT.to_owned(), lav));
        }
        runs.push(value);
    }
    if let Some(hint) = &way.hint {
        runs.push(Run::at(
            cols.who + WAY_HINT,
            sanitize_line(hint),
            theme.style(Role::Dim),
        ));
    }
}

/// The sheet as a screen reader reads it: the title and what it does, then each row as its
/// label, role, who and state, then the notes and the keys.
pub fn reader_lines(sheet: &SetupSheet) -> Vec<String> {
    let said = |parts: &[Part]| -> String {
        parts
            .iter()
            .map(|part| match part {
                Part::Named { name, .. } => name.clone(),
                Part::Text { text } => text.clone(),
            })
            .collect::<String>()
    };
    let mut lines = vec![sheet.title.clone()];
    lines.extend(sheet.about.iter().cloned());
    lines.push(
        sheet
            .status
            .iter()
            .map(|words| words.text.as_str())
            .collect(),
    );
    for row in &sheet.rows {
        let line = match row {
            SheetRow::Fill(fill) => {
                let what: String = fill.what.iter().map(|words| words.text.as_str()).collect();
                let state = fill.state.as_ref().map_or("", |state| state.text.as_str());
                format!(
                    "{}: {}, {}, {what}, {state}",
                    fill.label,
                    fill.role.text,
                    said(&fill.who)
                )
            }
            SheetRow::Aside(aside) => format!("{}: {}", aside.name, said(&aside.parts)),
            SheetRow::Heading { label, text } => format!("{label}: {text}"),
            SheetRow::Way(way) => {
                let allowed = if way.allowed { "allowed" } else { "kept out" };
                let value = way.value.as_deref().unwrap_or_default();
                format!("{}: {allowed} {value}", way.name)
            }
            SheetRow::Blank => continue,
        };
        lines.push(sanitize_line(line.trim_end_matches([' ', ','])).into_owned());
    }
    lines.extend(sheet.notes.iter().cloned());
    lines.push(sheet.keys.clone());
    lines.retain(|line| !line.trim().is_empty());
    lines
}

fn row_id(row: &SheetRow) -> Option<String> {
    match row {
        SheetRow::Fill(fill) => Some(fill.id.clone()),
        SheetRow::Aside(aside) => aside.id.clone(),
        SheetRow::Way(way) => Some(way.id.clone()),
        SheetRow::Heading { .. } | SheetRow::Blank => None,
    }
}

fn tone(theme: &Theme, tone: Tone) -> Style {
    theme.style(match tone {
        Tone::Text => Role::Text,
        Tone::Dim => Role::Dim,
        Tone::Ok => Role::Success,
        Tone::Warn => Role::Warn,
    })
}

fn width_of(text: &str) -> u16 {
    u16::try_from(unicode_width::UnicodeWidthStr::width(text)).unwrap_or(u16::MAX)
}

/// `text` from column `x`, cut at words to fit `width`.
fn wrapped(text: &str, x: u16, width: u16, style: Style) -> Vec<Vec<Run>> {
    let room = usize::from(width.saturating_sub(x + RIGHT)).max(1);
    let text = sanitize_line(text);
    if unicode_width::UnicodeWidthStr::width(text.as_ref()) <= room {
        return vec![vec![Run::at(x, text, style)]];
    }
    crate::text::wrap_text(&text, style, room, &[], &[])
        .into_iter()
        .map(|line| {
            let words: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            vec![Run::at(x, words.trim_end().to_owned(), style)]
        })
        .collect()
}

/// Text placed at a column.
#[derive(Debug, Clone)]
struct Run {
    x: u16,
    text: String,
    style: Style,
}

impl Run {
    fn at(x: u16, text: impl Into<String>, style: Style) -> Self {
        Self {
            x,
            text: text.into(),
            style,
        }
    }

    fn end(&self) -> u16 {
        self.x.saturating_add(width_of(&self.text))
    }
}

impl View for SetupSheetView {
    fn docked(&self) -> bool {
        true
    }

    fn dock_height(&mut self, screen: ratatui::layout::Size, ctx: &ViewCtx<'_>) -> Option<u16> {
        let rows = self.lines(screen.width, ctx.theme).len();
        Some(u16::try_from(rows).unwrap_or(u16::MAX).min(screen.height))
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        let ViewData::SetupSheet { sheet } = data else {
            return false;
        };
        if sheet.workflow != self.sheet.workflow {
            return false;
        }
        self.sheet = (**sheet).clone();
        self.take_pointer();
        true
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let lines = self.lines(area.width, ctx.theme);
        // A sheet taller than the room keeps its keys' line: rows go from the top.
        let skip = lines.len().saturating_sub(usize::from(area.height));
        let keep = lines.len() - skip;
        let shown = lines[..keep.saturating_sub(1)]
            .iter()
            .chain(lines.last())
            .take(usize::from(area.height));
        for (y, runs) in shown.enumerate() {
            let y = area.y + y as u16;
            for run in runs {
                if run.x >= area.width {
                    continue;
                }
                let room = usize::from(area.width - run.x);
                buf.set_stringn(area.x + run.x, y, &run.text, room, run.style);
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('s') {
            return self.save(true);
        }
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ViewOutcome::Ignored;
        }
        let Some(row) = self.at.clone() else {
            return match key.code {
                KeyCode::Esc => Self::leave(),
                _ => ViewOutcome::Consumed,
            };
        };
        match key.code {
            KeyCode::Esc => Self::leave(),
            KeyCode::Up => {
                self.step(-1);
                ViewOutcome::Consumed
            }
            KeyCode::Down => {
                self.step(1);
                ViewOutcome::Consumed
            }
            KeyCode::Left | KeyCode::Right if self.columns_here().contains(&self.column) => {
                Self::send(SheetAction::Change {
                    row,
                    column: self.column,
                    forward: key.code == KeyCode::Right,
                })
            }
            KeyCode::Tab => {
                let columns = self.columns_here();
                if let Some(here) = columns.iter().position(|column| *column == self.column) {
                    self.column = columns[(here + 1) % columns.len()];
                }
                ViewOutcome::Consumed
            }
            KeyCode::Char(' ') => Self::send(SheetAction::Toggle { row }),
            KeyCode::Enter => self.save(false),
            _ => ViewOutcome::Consumed,
        }
    }
}

impl SetupSheetView {
    fn save(&self, session: bool) -> ViewOutcome {
        if !self.sheet.ready {
            let why = self.sheet.not_yet.clone().unwrap_or_else(|| {
                format!(
                    "{} isn't ready yet",
                    self.sheet.title.trim_start_matches("Set up ")
                )
            });
            return ViewOutcome::Notice(why);
        }
        ViewOutcome::SendToConversation(UiCommand::SetupSheet {
            action: SheetAction::Save { session },
        })
    }

    /// Back to the conversation with nothing saved, and the app stops reading the logins.
    fn leave() -> ViewOutcome {
        ViewOutcome::SendToConversation(UiCommand::SetupSheet {
            action: SheetAction::Close,
        })
    }
}

#[cfg(test)]
#[path = "setup_sheet_tests.rs"]
mod tests;
