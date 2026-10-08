//! What goes under a card's header. Each body takes plain rows and draws them `width` cells
//! wide, with fixed slots so values never move a column.
//!
//! | Body | Looks like |
//! |---|---|
//! | stack | a stacked bar and its legend |
//! | half stack | the same, its bar in the lower half of its row |
//! | windows | Claude Code's /usage block per window: bold title, block meter with `71% used`, dim reset |
//! | meters | one half meter a row: dim label, meter, bold number, dim note |
//! | list | a state glyph, a name, its word, pills, and right-aligned numbers |
//! | columns | a two-row column chart over dim day letters |

use ratatui::layout::Rect;
use ratatui::text::Span;

use super::bars::{self, Stack};
use super::legend::{self, Entry};
use super::{Drawn, Kit, Level, State, States, fit, ljust, rjust, spread};
use crate::text::{str_width, truncate_spans};
use crate::theme::Role;

/// A percent and the space before it, as one right-aligned field: ` 71%`, `100%`.
const PERCENT_FIELD: usize = 4;

fn percent_field(ratio: f64) -> String {
    rjust(&format!("{:.0}%", ratio.max(0.0) * 100.0), PERCENT_FIELD)
}

/// A stacked bar, drawn `bar_rows` high, and its legend, up to `max_cols` to a row. No
/// entries, no legend, as in a compact card.
pub fn stack(
    kit: &Kit<'_>,
    stack: &Stack,
    entries: &[Entry],
    width: usize,
    bar_rows: usize,
    max_cols: usize,
) -> Drawn {
    let mut body = Drawn::new();
    body.push_meter(bars::stacked(kit, stack, width), 0, width, bar_rows.max(1));
    for row in legend::legend(kit, entries, width, max_cols) {
        body.push(row);
    }
    body
}

/// The stack body with its bar in the lower half of its row, for a card that sits among
/// half meters.
pub fn half_stack(
    kit: &Kit<'_>,
    stack: &Stack,
    entries: &[Entry],
    width: usize,
    max_cols: usize,
) -> Drawn {
    let mut body = Drawn::new();
    body.push(bars::half_stacked(kit, stack, width));
    for row in legend::legend(kit, entries, width, max_cols) {
        body.push(row);
    }
    body
}

/// One quota window, as Claude Code's /usage draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Bold, such as `Current session`.
    pub title: String,
    /// Used, as a ratio of the window. Past 1 the meter is full and the number keeps counting.
    pub used: f64,
    pub level: Level,
    /// `Resets 4:40pm`, already worded.
    pub resets: Option<String>,
}

/// Content this wide fits two windows side by side.
const WINDOWS_PAIR_FROM: usize = 74;
/// Under this, the reset lines go.
const WINDOWS_RESETS_FROM: usize = 56;
const WINDOWS_GAP: usize = 4;

/// Windows two to a row where they fit, each a title, a meter, and its reset. The text rows
/// keep every block meter off its neighbor's rows.
pub fn windows(kit: &Kit<'_>, windows: &[Window], width: usize) -> Drawn {
    let per_row = if width >= WINDOWS_PAIR_FROM { 2 } else { 1 };
    let cell = if per_row == 2 {
        (width - WINDOWS_GAP) / 2
    } else {
        width
    };
    let resets = width >= WINDOWS_RESETS_FROM;
    let mut body = Drawn::new();
    for (row, pair) in windows.chunks(per_row).enumerate() {
        if row > 0 && !resets {
            // Without the reset row, a blank one keeps the meters apart.
            body.push(Vec::new());
        }
        // One row of the pair, each window's part in its own cell.
        let side_by_side = |part: &dyn Fn(&Window) -> Vec<Span<'static>>| {
            let mut out = Vec::new();
            for (i, window) in pair.iter().enumerate() {
                if i + 1 < pair.len() {
                    out.extend(fit(part(window), cell));
                    out.push(Span::raw(" ".repeat(WINDOWS_GAP)));
                } else {
                    out.extend(part(window));
                }
            }
            out
        };
        body.push(side_by_side(&|w| {
            truncate_spans(&[kit.strong(w.title.clone())], cell)
        }));
        let meter_cells = cell.saturating_sub(PERCENT_FIELD + str_width(" used"));
        let top = body.height() as u16;
        let meters = side_by_side(&|w| {
            let mut spans = bars::block_meter(
                kit,
                w.used,
                meter_cells,
                w.level.fill_role(),
                Role::BarTrack,
            );
            spans.push(Span::styled(
                percent_field(w.used),
                kit.bold(w.level.strong_role()),
            ));
            spans.push(kit.dim(" used"));
            spans
        });
        for (i, _) in pair.iter().enumerate() {
            let left = i * (cell + WINDOWS_GAP);
            body.meters
                .push(Rect::new(left as u16, top, meter_cells as u16, 1));
        }
        body.push(meters);
        if resets {
            body.push(side_by_side(&|w| {
                vec![kit.dim(w.resets.clone().unwrap_or_default())]
            }));
        }
    }
    body
}

/// A row of the meters body.
#[derive(Debug, Clone, PartialEq)]
pub enum MeterRow {
    Meter {
        /// Dim, in a slot as wide as the widest label.
        label: String,
        ratio: f64,
        level: Level,
        /// Words after the number, such as ` used` or ` time`.
        unit: String,
        /// Dim, at the right, such as `Resets 4:40pm`.
        note: String,
        /// Where an amber `┃` marks the soft stop, as a ratio.
        tick: Option<f64>,
    },
    /// A row that says why there's no meter: a dim label, then its words.
    Text {
        label: String,
        words: Vec<Span<'static>>,
    },
}

/// The least cells the meters body gives its label and note columns, so each holds the
/// widest value it can show, such as `Resets Tue 10pm`, and not just the widest shown now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MeterSlots {
    pub label: usize,
    pub note: usize,
}

/// One half meter a row, which may stack since half meters never merge.
pub fn meters(kit: &Kit<'_>, rows: &[MeterRow], width: usize, slots: MeterSlots) -> Drawn {
    let widest =
        |f: &dyn Fn(&MeterRow) -> Option<usize>| rows.iter().filter_map(f).max().unwrap_or(0);
    let label_w = widest(&|row| match row {
        MeterRow::Meter { label, .. } | MeterRow::Text { label, .. } => Some(str_width(label)),
    })
    .max(slots.label);
    let unit_w = widest(&|row| match row {
        MeterRow::Meter { unit, .. } => Some(str_width(unit)),
        MeterRow::Text { .. } => None,
    });
    let note_w = widest(&|row| match row {
        MeterRow::Meter { note, .. } => Some(str_width(note)),
        MeterRow::Text { .. } => None,
    })
    .max(slots.note);
    let fixed = label_w + 1 + PERCENT_FIELD + unit_w;
    // On a narrow card the notes go before the meter gets too short to read.
    let note_w = if width >= fixed + 2 + note_w + 10 {
        note_w
    } else {
        0
    };
    let notes = if note_w > 0 { 2 + note_w } else { 0 };
    let meter_w = width.saturating_sub(fixed + notes);
    let mut body = Drawn::new();
    for row in rows {
        let mut line = Vec::new();
        match row {
            MeterRow::Meter {
                label,
                ratio,
                level,
                unit,
                note,
                tick,
            } => {
                line.push(kit.dim(ljust(label, label_w)));
                line.push(Span::raw(" "));
                line.extend(bars::half_meter(
                    kit,
                    *ratio,
                    meter_w,
                    level.fill_role(),
                    *tick,
                ));
                line.push(Span::styled(
                    percent_field(*ratio),
                    kit.bold(level.strong_role()),
                ));
                line.push(kit.dim(ljust(unit, unit_w)));
                if note_w > 0 {
                    line.push(Span::raw("  "));
                    line.push(kit.dim(ljust(note, note_w)));
                }
            }
            MeterRow::Text { label, words } => {
                line.push(kit.dim(ljust(label, label_w)));
                line.push(Span::raw(" "));
                line.extend(words.iter().cloned());
            }
        }
        body.push(truncate_spans(&line, width));
    }
    body
}

/// A row of the list body.
#[derive(Debug, Clone, PartialEq)]
pub struct ListRow {
    pub state: State,
    pub name: String,
    /// The state's word here, such as `merged`, in the state's color.
    pub word: String,
    /// Dim, after the word, such as `1/4 cards`.
    pub note: String,
    /// Pills for this row's share of the largest, where it has one.
    pub ratio: Option<f64>,
    /// Right-aligned in columns: the first bold, the rest dim.
    pub numbers: Vec<String>,
}

const LIST_PILLS: usize = 10;

/// The least cells each list column takes, its gap after it included, so a column holds its
/// widest possible value. `numbers` go in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListSlots {
    pub name: usize,
    pub word: usize,
    pub note: usize,
    pub numbers: Vec<usize>,
}

/// Rows of a glyph, a name, its word, its note, and pills at the left, and numbers at the
/// right. The name gives way first when the row is narrow.
pub fn list(
    kit: &Kit<'_>,
    states: &States,
    rows: &[ListRow],
    width: usize,
    slots: &ListSlots,
) -> Drawn {
    // Text columns take two cells after their widest value, or their slot.
    let column = |f: fn(&ListRow) -> &str, least: usize| {
        let widest = rows.iter().map(|r| str_width(f(r))).max().unwrap_or(0);
        if widest == 0 {
            0
        } else {
            (widest + 2).max(least)
        }
    };
    let (word_w, note_w) = (
        column(|r| &r.word, slots.word),
        column(|r| &r.note, slots.note),
    );
    let count = rows.iter().map(|r| r.numbers.len()).max().unwrap_or(0);
    let numbers: Vec<usize> = (0..count)
        .map(|k| {
            let widest = rows
                .iter()
                .filter_map(|r| r.numbers.get(k))
                .map(|n| str_width(n))
                .max()
                .unwrap_or(0);
            widest.max(slots.numbers.get(k).copied().unwrap_or(0))
        })
        .collect();
    let has_pills = rows.iter().any(|r| r.ratio.is_some());
    let right_w =
        if has_pills { LIST_PILLS } else { 0 } + numbers.iter().map(|w| w + 2).sum::<usize>();
    let name_room = width.saturating_sub(2 + word_w + note_w + right_w);
    let name_w = column(|r| &r.name, slots.name).min(name_room);
    let mut body = Drawn::new();
    for row in rows {
        let name = truncate_spans(&[Span::raw(row.name.clone())], name_w.saturating_sub(2));
        let name: String = name.iter().map(|s| s.content.as_ref()).collect();
        let mut left = vec![
            states.span(kit, row.state),
            Span::raw(format!(" {}", ljust(&name, name_w))),
            kit.span(ljust(&row.word, word_w), row.state.role()),
            kit.dim(ljust(&row.note, note_w)),
        ];
        if has_pills {
            match row.ratio {
                Some(ratio) => left.extend(bars::pills(kit, ratio, LIST_PILLS, Role::BarFill)),
                None => left.push(Span::raw(" ".repeat(LIST_PILLS))),
            }
        }
        let mut right = Vec::new();
        for (k, slot) in numbers.iter().enumerate() {
            let number = rjust(row.numbers.get(k).map_or("", String::as_str), *slot);
            right.push(Span::raw("  "));
            right.push(if k == 0 {
                kit.strong(number)
            } else {
                kit.dim(number)
            });
        }
        body.push(truncate_spans(&spread(left, right, width), width));
    }
    body
}

/// A column chart: its top value as a dim label, two rows of columns, and a letter under each.
#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub values: Vec<f64>,
    /// The top row's label, such as `$3.10`.
    pub label: String,
    /// One letter under each column, such as the day's.
    pub letters: Vec<String>,
    pub role: Role,
    /// The last column and its letter, which is today.
    pub last_role: Role,
}

pub fn columns(kit: &Kit<'_>, chart: &Chart) -> Drawn {
    let pad = str_width(&chart.label) + 1;
    let rows = bars::columns(kit, &chart.values, 2, chart.role, Some(chart.last_role));
    let mut body = Drawn::new();
    for (k, row) in rows.into_iter().enumerate() {
        let lead = if k == 0 {
            kit.dim(format!("{} ", chart.label))
        } else {
            Span::raw(" ".repeat(pad))
        };
        body.push([vec![lead], row].concat());
    }
    let mut letters = vec![Span::raw(" ".repeat(pad))];
    for (k, letter) in chart.letters.iter().enumerate() {
        if k > 0 {
            letters.push(Span::raw(" "));
        }
        letters.push(if k + 1 == chart.letters.len() {
            Span::styled(letter.clone(), kit.bold(chart.last_role))
        } else {
            kit.dim(letter.clone())
        });
    }
    body.push(letters);
    body
}
