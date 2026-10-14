//! What Stats draws with beyond the kit: a model's shade of its CLI's color, bars of shaded
//! parts, a square per task, a sparkline, the days' columns stacked by CLI, and a legend that
//! flows. Every bar here is the lower half of its cells, as on the Usage tab, so rows of
//! them never merge.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::text::spans_width;
use crate::theme::{Brightness, ColorLevel, Role, Theme, nearest_ansi256};
use crate::widgets::{Kit, merge};

/// How far each further model of a CLI is mixed toward the paper: its first model is the
/// CLI's own color, its second a light shade, and so on.
const SHADES: [f64; 4] = [0.0, 0.55, 0.3, 0.75];

/// The color a CLI's `nth` model draws in: the CLI's own for the first, then lighter shades
/// on a dark screen and darker ones on a light screen. Sixteen colors and none have no
/// shades, so every model draws in the CLI's color and its name tells them apart.
pub(in crate::overview) fn shade(kit: &Kit<'_>, base: Role, nth: usize) -> Style {
    let own = kit.fg(base);
    let mix = SHADES[nth % SHADES.len()];
    let theme = kit.theme;
    if mix == 0.0 || matches!(theme.level, ColorLevel::Ansi16 | ColorLevel::None) {
        return own;
    }
    let rgb = match theme.color(base) {
        Some(Color::Rgb(r, g, b)) => (r, g, b),
        _ => match Theme::for_brightness(theme.brightness).color(base) {
            Some(Color::Rgb(r, g, b)) => (r, g, b),
            _ => return own,
        },
    };
    let paper = match theme.brightness {
        Brightness::Dark => 255.0,
        Brightness::Light => 0.0,
    };
    let toward = |c: u8| (f64::from(c) + (paper - f64::from(c)) * mix).round() as u8;
    let mixed = (toward(rgb.0), toward(rgb.1), toward(rgb.2));
    let color = match theme.level {
        ColorLevel::TrueColor => Color::Rgb(mixed.0, mixed.1, mixed.2),
        _ => Color::Indexed(nearest_ansi256(mixed)),
    };
    Style::new().fg(color)
}

/// Cells for each part of `total` in `cells`, largest remainder first. Every part over
/// nothing gets a cell, taken from the largest.
pub(in crate::overview) fn cells_of(values: &[u64], total: u64, cells: usize) -> Vec<usize> {
    if total == 0 || cells == 0 {
        return vec![0; values.len()];
    }
    let exact: Vec<f64> = values
        .iter()
        .map(|&value| value as f64 / total as f64 * cells as f64)
        .collect();
    let mut out: Vec<usize> = exact.iter().map(|x| x.floor() as usize).collect();
    let mut left = cells.saturating_sub(out.iter().sum());
    let shown: u64 = values.iter().sum();
    if shown < total {
        // A bar of less than its whole leaves the rest as track.
        left = ((shown as f64 / total as f64 * cells as f64).round() as usize)
            .saturating_sub(out.iter().sum());
    }
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        let ra = exact[a] - exact[a].floor();
        let rb = exact[b] - exact[b].floor();
        rb.total_cmp(&ra)
    });
    for &at in order.iter().take(left) {
        out[at] += 1;
    }
    for at in 0..values.len() {
        if values[at] > 0 && out[at] == 0 {
            let largest = (0..values.len()).max_by_key(|&k| out[k]).unwrap_or(at);
            if out[largest] > 1 {
                out[largest] -= 1;
                out[at] = 1;
            }
        }
    }
    out
}

/// A bar `cells` wide of colored parts out of `total`, in the lower half of each cell, on the
/// bar's track. Without color each part takes a fill of its own.
pub(in crate::overview) fn bar(
    kit: &Kit<'_>,
    parts: &[(u64, Style)],
    total: u64,
    cells: usize,
) -> Vec<Span<'static>> {
    let values: Vec<u64> = parts.iter().map(|(value, _)| *value).collect();
    let widths = cells_of(&values, total, cells);
    let colored = kit.has_color() && kit.glyphs.halves.is_some();
    let whole = kit.glyphs.halves.map_or("#", |[_, whole]| whole);
    const FILLS: [&str; 4] = ["█", "▓", "▒", "#"];
    let mut out = Vec::new();
    for (k, ((_, style), width)) in parts.iter().zip(&widths).enumerate() {
        if *width == 0 {
            continue;
        }
        let span = if colored {
            Span::styled(whole.repeat(*width), *style)
        } else if kit.glyphs.halves.is_some() {
            Span::raw(FILLS[k % FILLS.len()].repeat(*width))
        } else {
            Span::raw(["#", "=", "+", "*"][k % 4].repeat(*width))
        };
        out.push(span);
    }
    let used: usize = widths.iter().sum();
    if used < cells {
        let rest = cells - used;
        out.push(if colored {
            Span::styled(whole.repeat(rest), kit.fg(Role::BarTrack))
        } else {
            Span::raw(kit.glyphs.free.repeat(rest))
        });
    }
    merge(out)
}

/// One part of `total`, as a meter on the track.
pub(in crate::overview) fn meter(
    kit: &Kit<'_>,
    value: u64,
    total: u64,
    cells: usize,
    style: Style,
) -> Vec<Span<'static>> {
    bar(kit, &[(value, style)], total.max(value), cells)
}

/// How a CLI's or a model's tasks fared in review: passed first time, sent back once, and
/// sent back twice or more.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Fared {
    pub first: u32,
    pub once: u32,
    pub more: u32,
}

impl Fared {
    pub fn add(&mut self, sent_back: u32) {
        match sent_back {
            0 => self.first += 1,
            1 => self.once += 1,
            _ => self.more += 1,
        }
    }

    pub fn total(&self) -> u32 {
        self.first + self.once + self.more
    }

    /// The share that passed first time.
    pub fn first_pass(&self) -> f64 {
        f64::from(self.first) / f64::from(self.total().max(1))
    }
}

/// A square for each task, green passed review first time, amber sent back once, red twice
/// or more, in that order. More tasks than `room` are drawn as `room` squares in the same
/// shares.
pub(super) fn squares(kit: &Kit<'_>, fared: Fared, room: usize) -> Vec<Span<'static>> {
    let counts = [fared.first, fared.once, fared.more].map(u64::from);
    let total: u64 = counts.iter().sum();
    let widths = if total as usize <= room {
        counts.map(|count| count as usize).to_vec()
    } else {
        cells_of(&counts, total, room)
    };
    let glyph = if kit.glyphs.halves.is_some() {
        "■"
    } else {
        "#"
    };
    let roles = [Role::Success, Role::Warning, Role::Error];
    let without = ["#", "!", "x"];
    let mut out = Vec::new();
    for ((width, role), plain) in widths.iter().zip(roles).zip(without) {
        if *width == 0 {
            continue;
        }
        out.push(if kit.has_color() {
            Span::styled(glyph.repeat(*width), kit.fg(role))
        } else {
            Span::raw(plain.repeat(*width))
        });
    }
    out
}

/// A sparkline, one cell for each value, in eighths. A day with nothing is the lowest cell.
pub(super) fn sparkline(kit: &Kit<'_>, values: &[u64], style: Style) -> Span<'static> {
    let top = values.iter().copied().max().unwrap_or(0).max(1);
    let line: String = values
        .iter()
        .map(|&value| {
            let level = (value as f64 / top as f64 * 8.0).round() as usize;
            kit.glyphs.columns[level.clamp(1, 8) - 1]
        })
        .collect();
    Span::styled(line, style)
}

/// The days' columns, stacked by part from the bottom, `rows` high and two cells wide with a
/// cell between, over the days' letters. Each cell holds two halves, so parts end on half a
/// row. `days` holds each day's parts in drawing order, bottom first; the last day is today,
/// whose letter takes the accent.
pub(super) fn day_columns(
    kit: &Kit<'_>,
    days: &[(char, Vec<(u64, Style)>)],
    rows: usize,
) -> Vec<Vec<Span<'static>>> {
    let top = days
        .iter()
        .map(|(_, parts)| parts.iter().map(|(value, _)| value).sum::<u64>())
        .max()
        .unwrap_or(0)
        .max(1);
    // Each day's halves, bottom first, as the style of the part filling each.
    let halves: Vec<Vec<Style>> = days
        .iter()
        .map(|(_, parts)| {
            let values: Vec<u64> = parts.iter().map(|(value, _)| *value).collect();
            let widths = cells_of(&values, top, rows * 2);
            parts
                .iter()
                .zip(widths)
                .flat_map(|((_, style), width)| std::iter::repeat_n(*style, width))
                .collect()
        })
        .collect();
    let mut out = Vec::new();
    for row in (0..rows).rev() {
        let mut line = Vec::new();
        for (k, column) in halves.iter().enumerate() {
            if k > 0 {
                line.push(Span::raw(" "));
            }
            let lower = column.get(row * 2);
            let upper = column.get(row * 2 + 1);
            line.push(match (lower, upper) {
                (None, _) => Span::raw("  "),
                _ if !kit.has_color() => Span::raw(kit.glyphs.used.repeat(2)),
                (Some(lower), None) => {
                    let glyph = kit.glyphs.halves.map_or(" ", |[_, whole]| whole);
                    Span::styled(glyph.repeat(2), *lower)
                }
                (Some(lower), Some(upper)) if lower == upper => {
                    Span::styled(kit.glyphs.columns[7].repeat(2), *lower)
                }
                (Some(lower), Some(upper)) => {
                    let glyph = kit.glyphs.halves.map_or(" ", |[_, whole]| whole);
                    let mut style = *lower;
                    style.bg = upper.fg;
                    Span::styled(glyph.repeat(2), style)
                }
            });
        }
        out.push(merge(line));
    }
    let mut letters = Vec::new();
    for (k, (letter, _)) in days.iter().enumerate() {
        if k > 0 {
            letters.push(Span::raw(" "));
        }
        let today = k + 1 == days.len();
        letters.push(if today {
            Span::styled(
                format!("{letter} "),
                kit.fg(Role::Accent).add_modifier(Modifier::BOLD),
            )
        } else {
            kit.dim(format!("{letter} "))
        });
    }
    out.push(letters);
    out
}

/// One entry of a flowing legend: its color's swatch, its name, a bold amount and a dim
/// share or note.
#[derive(Clone)]
pub(super) struct Entry {
    pub style: Style,
    pub name: String,
    pub amount: String,
    pub note: String,
}

/// Entries one after another, three cells apart, onto as many rows of `width` as they need.
/// When the notes would take a row of their own, the entries go without them.
pub(super) fn legend(
    kit: &Kit<'_>,
    entries: &[Entry],
    tail: Option<String>,
    width: usize,
) -> Vec<Vec<Span<'static>>> {
    let rows = flow(kit, entries, tail.clone(), width);
    if rows.len() < 2 || entries.iter().all(|entry| entry.note.is_empty()) {
        return rows;
    }
    let bare: Vec<Entry> = entries
        .iter()
        .map(|entry| Entry {
            note: String::new(),
            ..entry.clone()
        })
        .collect();
    let shorter = flow(kit, &bare, tail, width);
    if shorter.len() < rows.len() {
        shorter
    } else {
        rows
    }
}

fn flow(
    kit: &Kit<'_>,
    entries: &[Entry],
    tail: Option<String>,
    width: usize,
) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut place = |spans: Vec<Span<'static>>, row: &mut Vec<Span<'static>>| {
        let needs = spans_width(&spans) + if row.is_empty() { 0 } else { 3 };
        if !row.is_empty() && spans_width(row) + needs > width {
            rows.push(std::mem::take(row));
        }
        if !row.is_empty() {
            row.push(Span::raw("   "));
        }
        row.extend(spans);
    };
    for entry in entries {
        let swatch = if kit.has_color() {
            Span::styled(kit.glyphs.swatch, entry.style)
        } else {
            Span::raw(kit.glyphs.swatch)
        };
        let mut spans = vec![swatch, Span::raw(" "), Span::raw(entry.name.clone())];
        if !entry.amount.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(kit.strong(entry.amount.clone()));
        }
        if !entry.note.is_empty() {
            spans.push(kit.dim(format!(" {}", entry.note)));
        }
        place(spans, &mut row);
    }
    if let Some(tail) = tail {
        place(vec![kit.dim(tail)], &mut row);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_part_gets_a_cell_and_the_cells_add_up() {
        assert_eq!(cells_of(&[58, 31, 11], 100, 20), [12, 6, 2]);
        assert_eq!(cells_of(&[990, 9, 1], 1000, 10), [8, 1, 1]);
        assert_eq!(cells_of(&[1, 1], 0, 10), [0, 0]);
        // Less than the whole leaves the track.
        assert_eq!(cells_of(&[25], 100, 8), [2]);
    }

    #[test]
    fn a_second_model_is_a_lighter_shade_of_its_clis_color() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let own = shade(&kit, Role::AgentOrange, 0);
        let second = shade(&kit, Role::AgentOrange, 1);
        assert_eq!(own.fg, Some(Color::Rgb(0xfb, 0x92, 0x3c)));
        let Some(Color::Rgb(r, g, b)) = second.fg else {
            panic!("{second:?}");
        };
        assert!(r >= 0xfb && g > 0x92 && b > 0x3c, "{r} {g} {b}");
        let plain = Theme::dark().with_level(ColorLevel::Ansi16);
        let kit = Kit::new(&plain);
        assert_eq!(shade(&kit, Role::AgentOrange, 1), kit.fg(Role::AgentOrange));
    }

    #[test]
    fn a_square_is_a_task_until_they_outnumber_the_room() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let fared = Fared {
            first: 6,
            once: 1,
            more: 0,
        };
        let text: String = squares(&kit, fared, 10)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "■■■■■■■");
        let many = Fared {
            first: 33,
            once: 2,
            more: 0,
        };
        let drawn = squares(&kit, many, 10);
        assert_eq!(spans_width(&drawn), 10);
        assert_eq!(drawn.len(), 2, "green, then amber");
    }
}
