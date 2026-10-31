//! One setting per row, as `/config` and `/approvals` list them: the key or its name, its
//! value, the layer it comes from in words, and a dim description, with `!` on safety keys
//! where only some keys are.

use argonauts_core::app::SettingRow;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::base::draw::layout::{cut, left};
use crate::base::text::words::{layer_words, row_value_words};
use crate::base::text::{sanitize_line, str_width};
use crate::base::theme::{Role, Theme};

const GAP: usize = 2;
/// The pointer, two cells, and the safety mark when a list has one, two more.
const POINTER: usize = 2;
pub const PREFIX: usize = 4;
const MAX_LABEL: usize = 40;
const MAX_VALUE: usize = 24;
const MAX_LAYER: usize = 24;
/// A description cut shorter than this says nothing, so it's left out instead.
const MIN_DESCRIPTION: usize = 16;
const MIN_NOTE: usize = 8;
/// What the value and then the label give up so the description gets a third of the row: down
/// to these widths and no further. The layer keeps its width, since it says where to change it.
const ROOMY_FLOORS: [(usize, usize); 2] = [(1, 16), (0, 24)];

/// A row's text, as the list words it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cells {
    pub label: String,
    pub value: String,
    /// An unset value is dim.
    pub unset: bool,
    pub layer: String,
    pub description: String,
    pub safety: bool,
}

impl Cells {
    /// As `/config` lists a row: `label`, then the value and layer in words.
    pub fn of(label: &str, row: &SettingRow) -> Self {
        Self {
            label: sanitize_line(label).into_owned(),
            value: sanitize_line(&row_value_words(&row.key, &row.value)).into_owned(),
            unset: row.value.trim().is_empty(),
            layer: layer_words(&row.layer),
            description: sanitize_line(&row.description).trim().to_owned(),
            safety: row.safety,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Widths {
    /// Cells before the label: the pointer, and the safety mark's column when a list has one.
    pub prefix: usize,
    pub label: usize,
    pub value: usize,
    pub layer: usize,
    /// Zero when there's no room for it.
    pub description: usize,
}

impl Widths {
    /// Fits the columns in `width` cells. The layer column gives way last, since it says
    /// where to change the value. The description takes the room left, and the other columns
    /// narrow a little so that's a third of the row when it needs it.
    pub fn fit<'a>(rows: impl Iterator<Item = (&'a str, &'a SettingRow)>, width: usize) -> Self {
        let cells: Vec<Cells> = rows.map(|(label, row)| Cells::of(label, row)).collect();
        Self::fit_cells(&cells, width, true)
    }

    /// Like [`Widths::fit`], for rows already in words. Without `marks`, no column is kept for
    /// `!`, as where every key is a safety key and the mark would tell nothing. Such a list goes
    /// by name, which says what a row does, so its description only takes the room left and
    /// never narrows a name or a value.
    pub fn fit_cells(cells: &[Cells], width: usize, marks: bool) -> Self {
        let prefix = if marks { PREFIX } else { POINTER };
        let (mut label, mut value, mut layer, mut longest) = (1, 1, 1, 0);
        for cell in cells {
            label = label.max(str_width(&cell.label));
            value = value.max(str_width(&cell.value));
            layer = layer.max(str_width(&cell.layer));
            longest = longest.max(str_width(&cell.description));
        }
        // Label, value, layer.
        let mut columns = [
            label.min(MAX_LABEL),
            value.min(MAX_VALUE),
            layer.min(MAX_LAYER),
        ];
        let room = width.saturating_sub(prefix + 2 * GAP);
        for (column, floor) in [(1, 8), (0, 12), (2, 12), (1, 3), (0, 6), (2, 4)] {
            let over = columns.iter().sum::<usize>().saturating_sub(room);
            let current = columns[column];
            columns[column] = current.saturating_sub(over).max(floor.min(current));
        }
        let left_over =
            |columns: &[usize; 3]| room.saturating_sub(columns.iter().sum::<usize>() + GAP);
        let wanted = longest.min(room / 3);
        let mut narrowed = columns;
        for (column, floor) in ROOMY_FLOORS {
            let short = wanted.saturating_sub(left_over(&narrowed));
            let current = narrowed[column];
            narrowed[column] = current.saturating_sub(short).max(floor.min(current));
        }
        // Narrowing is only worth it when it makes room for a description.
        if marks && left_over(&narrowed) >= MIN_DESCRIPTION {
            columns = narrowed;
        }
        let description = match left_over(&columns) {
            room if room >= MIN_DESCRIPTION => room,
            _ => 0,
        };
        let [label, value, layer] = columns;
        Self {
            prefix,
            label,
            value,
            layer,
            description,
        }
    }
}

/// A setting's row. `label` is the key, or the part of it under a heading.
pub fn setting_line(
    label: &str,
    row: &SettingRow,
    widths: &Widths,
    pointed: bool,
    theme: &Theme,
) -> Line<'static> {
    cells_line(&Cells::of(label, row), widths, pointed, theme)
}

pub fn cells_line(cells: &Cells, widths: &Widths, pointed: bool, theme: &Theme) -> Line<'static> {
    let dim = theme.style(Role::Dim);
    let pointer = if pointed {
        Span::styled(
            format!("{} ", theme.glyphs.prompt),
            theme.style(Role::Question),
        )
    } else {
        Span::raw("  ")
    };
    let mut spans = vec![pointer];
    if widths.prefix > POINTER {
        spans.push(if cells.safety {
            Span::styled("! ", theme.style(Role::Warn).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("  ")
        });
    }
    let label_style = if pointed {
        theme.style(Role::Question)
    } else {
        theme.style(Role::Text)
    };
    let gap = " ".repeat(GAP);
    let value_style = if cells.unset {
        dim
    } else {
        theme.style(Role::Text)
    };
    spans.extend([
        Span::styled(left(&cells.label, widths.label), label_style),
        Span::raw(gap.clone()),
        Span::styled(left(&cells.value, widths.value), value_style),
        Span::raw(gap.clone()),
        Span::styled(left(&cells.layer, widths.layer), dim),
    ]);
    if widths.description > 0 && !cells.description.is_empty() {
        spans.push(Span::raw(gap));
        spans.push(Span::styled(
            cut(&cells.description, widths.description),
            dim,
        ));
    }
    Line::from(spans)
}

/// A group's name over its rows, with a dim note at the right such as an auto preview record.
pub fn heading_line(name: &str, note: &str, width: usize, theme: &Theme) -> Line<'static> {
    let style = theme.style(Role::Label).add_modifier(Modifier::BOLD);
    let name = cut(sanitize_line(name).trim(), width);
    let note = sanitize_line(note).trim().to_owned();
    let room = width.saturating_sub(str_width(&name) + GAP);
    if note.is_empty() || room < MIN_NOTE {
        return Line::from(Span::styled(name, style));
    }
    let note = cut(&note, room);
    let pad = width.saturating_sub(str_width(&name) + str_width(&note));
    Line::from(vec![
        Span::styled(name, style),
        Span::raw(" ".repeat(pad)),
        Span::styled(note, theme.style(Role::Dim)),
    ])
}
