//! The Config tab drawn as Claude Code draws its own: two blank rows under the tabs, the
//! search box, the rows with the label in a column of its own, what is above or below, the
//! keys, and a rule under them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::rows::Control;
use super::{ConfigTab, Focus, Shown};
use crate::kit::Kit;
use crate::state::AppState;
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::views::ViewCtx;
use crate::views::layout::cut;

/// The most rows that show at once, as in Claude Code.
const MOST_ROWS: usize = 20;
/// Cells the label's column takes after the pointer, as in Claude Code.
const LABEL_COLUMN: usize = 43;
const POINTER: &str = "  ❯ ";
const NO_POINTER: &str = "    ";
/// Between a row's value and what its values mean.
const DESCRIBED_GAP: &str = "   ";
const PLACEHOLDER: &str = "Search settings…";

/// Rows the tab takes under its tabs for `rows` setting rows: two blank, the search box, a
/// blank, the rows, what is below, a blank, the keys and the rule.
pub fn body_rows(rows: usize) -> u16 {
    u16::try_from(rows.min(MOST_ROWS) + 10).unwrap_or(u16::MAX)
}

impl ConfigTab {
    /// How many setting rows the tab would show, for the panel's height. A Behaviors row's
    /// picker takes the rows' place, with its title, its own rows and its hint.
    pub fn wanted(&self, state: &AppState) -> usize {
        // The title, what the choice does over two lines or three, and the keys.
        let picker = self
            .picker()
            .map_or(0, |picker| picker.rows.len().max(1) + 6);
        self.shown(state).len().max(picker).max(1)
    }
}

/// Draws the tab in `area`, which starts under the panel's tabs. `on_tabs` is whether the
/// panel's tabs have the keys, which the hint line says.
pub fn draw(tab: &mut ConfigTab, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>, on_tabs: bool) {
    let kit = &Kit::new(ctx.theme);
    let state = ctx.state;
    tab.read_outcomes(state);
    let width = usize::from(area.width);
    if area.height < 10 || width < 8 {
        return;
    }
    let mut y = area.y + 2;
    let mut put = |y: &mut u16, spans: Vec<Span<'static>>| {
        let line = Line::from(truncate_spans(&spans, width));
        buf.set_line(area.x, *y, &line, area.width);
        *y += 1;
    };
    for line in search_box(tab, kit, ctx.theme.glyphs.rule, width, on_tabs) {
        put(&mut y, line);
    }
    y += 1;
    // A Behaviors row's picker shows in the rows' place.
    if let Some(picking) = tab.picking.as_mut() {
        picking.sync(&state.names);
    }
    if let Some(editor) = tab.picker() {
        let bottom = area.y + area.height;
        let rows = Rect::new(area.x, y, area.width, bottom.saturating_sub(y));
        crate::views::settings::draw_editor(editor, rows, buf, ctx);
        return;
    }

    let count = tab.shown(state).len();
    tab.pointer = tab.pointer.min(count.saturating_sub(1));
    let room = usize::from(area.height).saturating_sub(10).min(MOST_ROWS);
    if tab.pointer < tab.offset {
        tab.offset = tab.pointer;
    } else if room > 0 && tab.pointer >= tab.offset + room {
        tab.offset = tab.pointer + 1 - room;
    }
    tab.offset = tab.offset.min(count.saturating_sub(room.max(1)));
    let tab = &*tab;
    let shown = tab.shown(state);
    let pointed = (tab.focus == Focus::List && !on_tabs).then_some(tab.pointer);
    for at in 0..room {
        let line = match shown.get(tab.offset + at) {
            Some(row) => {
                let pointed = Some(tab.offset + at) == pointed;
                row_line(tab, row, pointed, (kit, state), width)
            }
            None if at == 0 && shown.is_empty() => {
                let query = crate::text::sanitize_line(tab.query.trim()).into_owned();
                vec![kit.dim(format!("{NO_POINTER}No settings match \"{query}\""))]
            }
            None => Vec::new(),
        };
        put(&mut y, line);
    }
    put(&mut y, vec![kit.dim(more_words(tab, &shown, room))]);
    y += 1;
    put(&mut y, vec![kit.dim(format!("  {}", tab.hints(on_tabs)))]);
    let rule = ctx.theme.glyphs.rule.repeat(width);
    put(&mut y, vec![Span::styled(rule, kit.style(Role::Rule))]);
}

/// The rounded box with the search in it: lit while it has the keys, as Claude Code's is.
fn search_box(
    tab: &ConfigTab,
    kit: &Kit<'_>,
    rule: &str,
    width: usize,
    on_tabs: bool,
) -> Vec<Vec<Span<'static>>> {
    let focused = tab.focus == Focus::Search && !on_tabs;
    let border = kit.style(if focused { Role::Question } else { Role::Rule });
    let inner = width.saturating_sub(6);
    let edge = rule.repeat(inner);
    let indent = Span::raw("  ");
    let mut text = vec![kit.dim("⌕ ")];
    let typed = crate::text::sanitize_line(&tab.query).into_owned();
    if typed.is_empty() {
        if focused {
            // The cursor sits on the placeholder's first letter, as in Claude Code.
            let mut chars = PLACEHOLDER.chars();
            let first = chars.next().map(String::from).unwrap_or_default();
            text.push(Span::styled(
                first,
                kit.style(Role::Dim).add_modifier(Modifier::REVERSED),
            ));
            text.push(kit.dim(chars.as_str().to_owned()));
        } else {
            text.push(kit.dim(PLACEHOLDER));
        }
    } else {
        text.push(kit.span(typed, Role::Text));
        if focused {
            text.push(Span::styled(
                " ",
                kit.style(Role::Text).add_modifier(Modifier::REVERSED),
            ));
        }
    }
    let used: usize = text.iter().map(|span| str_width(&span.content)).sum();
    let pad = inner.saturating_sub(2 + used);
    let mut middle = vec![indent.clone(), Span::styled("│ ", border)];
    middle.extend(truncate_spans(&text, inner.saturating_sub(2)));
    middle.push(Span::raw(" ".repeat(pad)));
    middle.push(Span::styled(" │", border));
    vec![
        vec![indent.clone(), Span::styled(format!("╭{edge}╮"), border)],
        middle,
        vec![indent, Span::styled(format!("╰{edge}╯"), border)],
    ]
}

/// One row: the pointer, the label in its column, and the value as its kind draws it, in
/// `width` cells. A value too long for the row is cut before its marks, so a link keeps its
/// `›`.
fn row_line(
    tab: &ConfigTab,
    row: &Shown<'_>,
    pointed: bool,
    (kit, state): (&Kit<'_>, &AppState),
    width: usize,
) -> Vec<Span<'static>> {
    let lit = kit.style(Role::Question);
    let text = if pointed { lit } else { kit.style(Role::Text) };
    let label = tab.label(row.row);
    let mut line = vec![Span::styled(
        if pointed { POINTER } else { NO_POINTER },
        lit,
    )];
    if row.row.control == Control::AllSettings {
        line.push(Span::styled(format!("{label} ›"), text));
        return line;
    }
    let pad = LABEL_COLUMN.saturating_sub(str_width(&label)).max(1);
    line.push(Span::styled(format!("{label}{}", " ".repeat(pad)), text));
    let words = tab.words(row, state);
    let (before, after) = match row.row.control {
        Control::Choice(_) if pointed => ("‹ ", " ›"),
        Control::Opens(_) | Control::Picks(_) if words.is_empty() => ("", "›"),
        Control::Opens(_) | Control::Picks(_) => ("", " ›"),
        _ => ("", ""),
    };
    let held = row.held_by().map(|who| format!(" · set by {who}"));
    let used = spans_width(&line)
        + str_width(before)
        + str_width(after)
        + held.as_deref().map_or(0, str_width);
    let words = cut(&words, width.saturating_sub(used));
    let style = match row.row.control {
        Control::Choice(_) if pointed => lit.add_modifier(Modifier::BOLD),
        _ => text,
    };
    line.push(Span::styled(format!("{before}{words}{after}"), style));
    if let Some(held) = held {
        line.push(kit.dim(held));
    }
    if let Some(note) = note(row.row.key).map(|note| format!("{DESCRIBED_GAP}{note}")) {
        if spans_width(&line) + str_width(&note) <= width {
            line.push(kit.dim(note));
        }
    } else if let Some(meaning) = row.meaning() {
        let room = width.saturating_sub(spans_width(&line) + DESCRIBED_GAP.len());
        if room > 0 {
            line.push(kit.dim(format!("{DESCRIBED_GAP}{}", cut(meaning, room))));
        }
    }
    line
}

/// What a row's setting does, dim after its value while the row has room for all of it.
fn note(key: &str) -> Option<&'static str> {
    match key {
        "telemetry.enabled" => Some("one anonymous report a day · PRIVACY.md"),
        _ => None,
    }
}

/// What the rows on screen leave out: `↓ 25 more below`, with the last row's name while it
/// is out of sight, or `↑ 3 more above`.
fn more_words(tab: &ConfigTab, shown: &[Shown<'_>], room: usize) -> String {
    let below = shown.len().saturating_sub(tab.offset + room);
    if below > 0 {
        let last = shown
            .last()
            .filter(|row| row.row.control == Control::AllSettings)
            .map(|row| format!(" · the last row is \"{} ›\"", tab.label(row.row)))
            .unwrap_or_default();
        format!("{NO_POINTER}↓  {below} more below{last}")
    } else if tab.offset > 0 {
        format!("{NO_POINTER}↑  {} more above", tab.offset)
    } else {
        String::new()
    }
}
