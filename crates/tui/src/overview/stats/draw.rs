//! What Stats draws with beyond the kit: a model's shade of its CLI's color, thin bars of
//! shaded parts on a faint track, ten small squares for how work fared, a sparkline, the
//! chart's columns stacked by CLI, and a legend that flows. A bar is a thin line through the
//! middle of its cells and a square is one cell wide in every font, so nothing touches the
//! row above or the cell beside it; a blank cell always sits between a bar or squares and
//! the words after them.

use argonauts_core::crew_stats::StatsRole;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use crate::kit::{Kit, merge};
pub(in crate::overview) use crate::overview::shades::shade;
use crate::text::spans_width;
use crate::theme::{Brightness, ColorLevel, Role, nearest_ansi256};
use crate::views::layout::cut;

/// The most cells of a model's name on a card or in a legend, the longest the drawings write
/// whole. A longer one is cut with `…`, as a provider's own id for a model often has to be.
pub(super) const MODEL_NAME: usize = 16;

/// The fewest cells of a name a legend keeps on a row too narrow for the whole entry.
const LEAST_LEGEND_NAME: usize = 4;

/// A model's name as a legend writes it.
pub(super) fn model_name(name: &str) -> String {
    cut(name, MODEL_NAME)
}

/// The colour a role draws in wherever Stats shows the roles side by side. The workers, the
/// reviewers, the judge and the merger take four of the theme's agent colours. Main's lime
/// and the orchestrator's pink are Stats' own: they stay apart from the green and the red of
/// how work fared, which the theme's green and pink don't.
pub(super) fn role_style(kit: &Kit<'_>, role: StatsRole) -> Style {
    match role {
        StatsRole::Main => own(
            kit,
            (0xa3, 0xd9, 0x77),
            (0x4d, 0x7c, 0x0f),
            Role::AgentGreen,
        ),
        StatsRole::Orchestrator => {
            own(kit, (0xff, 0x7a, 0xb6), (0xbe, 0x18, 0x5d), Role::AgentPink)
        }
        StatsRole::Worker => kit.fg(Role::AgentOrange),
        StatsRole::Reviewer => kit.fg(Role::AgentBlue),
        StatsRole::Judge => kit.fg(Role::AgentPurple),
        StatsRole::Merger => kit.fg(Role::AgentCyan),
    }
}

/// A colour outside the theme, `dark` on a dark screen and `light` on a light one, as near
/// as the terminal's colours come. With sixteen colours or none it's `fallback`'s.
fn own(kit: &Kit<'_>, dark: (u8, u8, u8), light: (u8, u8, u8), fallback: Role) -> Style {
    let theme = kit.theme;
    let rgb = match theme.brightness {
        Brightness::Dark => dark,
        Brightness::Light => light,
    };
    match theme.level {
        ColorLevel::TrueColor => Style::new().fg(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        ColorLevel::Ansi256 => Style::new().fg(Color::Indexed(nearest_ansi256(rgb))),
        ColorLevel::Ansi16 | ColorLevel::None => kit.fg(fallback),
    }
}

/// Cells for each part of `total` in `cells`: each part its share rounded down, and the cells
/// left over to the parts that lost the most by it, the largest remainder first.
fn by_largest_remainder(values: &[u64], total: u64, cells: usize) -> Vec<usize> {
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
    out
}

/// Cells for each part of `total` in `cells`, largest remainder first. Every part over
/// nothing gets a cell, taken from the largest.
pub(in crate::overview) fn cells_of(values: &[u64], total: u64, cells: usize) -> Vec<usize> {
    let mut out = by_largest_remainder(values, total, cells);
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

/// A thin bar's glyph, and the track's when there's no color to tell them apart.
const THIN: &str = "━";
const THIN_TRACK: &str = "─";
/// A stacked bar's parts without color, each its own line.
const THIN_PARTS: [&str; 4] = ["━", "═", "─", "╌"];

/// The cells `value` of `whole` takes of `cells`: its share in whole percents, as a legend
/// writes it, so a bar and the numbers beside it agree.
fn cells_for(value: u64, whole: u64, cells: usize) -> usize {
    let percent = (value as f64 / whole.max(1) as f64 * 100.0).round();
    (percent / 100.0 * cells as f64).round() as usize
}

/// `value` of `whole` as a thin bar `cells` wide: its share rounded, at least a cell when
/// it has any, in `style`, the rest on the faint track.
pub(in crate::overview) fn thin(
    kit: &Kit<'_>,
    value: u64,
    whole: u64,
    cells: usize,
    style: Style,
) -> Vec<Span<'static>> {
    let mut filled = cells_for(value, whole, cells).min(cells);
    if value > 0 && filled == 0 {
        filled = cells.min(1);
    }
    let mut out = Vec::new();
    if filled > 0 {
        out.push(Span::styled(THIN.repeat(filled), fill_style(kit, style)));
    }
    if filled < cells {
        out.push(if kit.has_color() {
            Span::styled(THIN.repeat(cells - filled), kit.fg(Role::Faint))
        } else {
            Span::raw(THIN_TRACK.repeat(cells - filled))
        });
    }
    out
}

/// Parts side by side in one thin bar exactly `cells` wide, each its share of the cells by
/// largest remainder. A part too small for a cell of its own has none: its number is in the
/// legend.
pub(in crate::overview) fn stacked(
    kit: &Kit<'_>,
    parts: &[(u64, Style)],
    cells: usize,
) -> Vec<Span<'static>> {
    let total: u64 = parts.iter().map(|(value, _)| value).sum();
    if total == 0 {
        return thin(kit, 0, 1, cells, Style::new());
    }
    let values: Vec<u64> = parts.iter().map(|(value, _)| *value).collect();
    let widths = by_largest_remainder(&values, total, cells);
    let mut out = Vec::new();
    for (k, ((_, style), width)) in parts.iter().zip(widths).enumerate() {
        if width == 0 {
            continue;
        }
        out.push(if kit.has_color() {
            Span::styled(THIN.repeat(width), fill_style(kit, *style))
        } else {
            Span::raw(THIN_PARTS[k % THIN_PARTS.len()].repeat(width))
        });
    }
    merge(out)
}

fn fill_style(kit: &Kit<'_>, style: Style) -> Style {
    if kit.has_color() { style } else { Style::new() }
}

/// Ten small squares, or `n`, for how work fared: green for the share that passed first
/// time, amber for the share sent back once, and red for the rest.
pub(super) fn tenths(kit: &Kit<'_>, fared: Fared, n: usize) -> Vec<Span<'static>> {
    let total = f64::from(fared.total());
    let (first, once) = if total > 0.0 {
        let share = |count: u32| (n as f64 * f64::from(count) / total).round() as usize;
        let first = share(fared.first).min(n);
        (first, share(fared.once).min(n - first))
    } else {
        (0, 0)
    };
    let more = if total > 0.0 { n - first - once } else { 0 };
    let glyph = if kit.glyphs.halves.is_some() {
        "▪"
    } else {
        "#"
    };
    let mut out = Vec::new();
    for (count, role, plain) in [
        (first, Role::Success, "#"),
        (once, Role::Warn, "!"),
        (more, Role::Danger, "x"),
    ] {
        if count == 0 {
            continue;
        }
        out.push(if kit.has_color() {
            Span::styled(glyph.repeat(count), kit.fg(role))
        } else {
            Span::raw(plain.repeat(count))
        });
    }
    out
}

/// A cell of a sparkline for each value, in eighths of `top`. An idle slot is the lowest
/// cell on the grey track, and a slot still ahead is blank.
pub(in crate::overview) fn spark(
    kit: &Kit<'_>,
    values: &[Option<u64>],
    top: u64,
    style: Style,
) -> Vec<Span<'static>> {
    let top = values
        .iter()
        .flatten()
        .copied()
        .max()
        .unwrap_or(0)
        .max(top)
        .max(1);
    let mut out = Vec::new();
    for value in values {
        out.push(match value {
            None => Span::raw(" "),
            Some(0) => Span::styled(kit.glyphs.columns[0], kit.fg(Role::Faint)),
            Some(value) => {
                let level = (*value as f64 / top as f64 * 8.0).round() as usize;
                Span::styled(
                    kit.glyphs.columns[level.clamp(1, 8) - 1],
                    fill_style(kit, style),
                )
            }
        });
    }
    merge(out)
}

/// [`spark`] with a blank cell between its cells, so each reads as a bar of its own over the
/// letter of its day.
pub(super) fn spark_apart(
    kit: &Kit<'_>,
    values: &[Option<u64>],
    top: u64,
    style: Style,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (k, value) in values.iter().enumerate() {
        if k > 0 {
            out.push(Span::raw(" "));
        }
        out.extend(spark(
            kit,
            std::slice::from_ref(value),
            top.max(peak(values)),
            style,
        ));
    }
    out
}

/// The most any slot of `values` holds.
fn peak(values: &[Option<u64>]) -> u64 {
    values.iter().flatten().copied().max().unwrap_or(0)
}

/// A legend's swatch: a square, then a blank cell before the name.
pub(in crate::overview) fn swatch(kit: &Kit<'_>, style: Style) -> Vec<Span<'static>> {
    let square = if kit.glyphs.halves.is_some() {
        "■"
    } else {
        "#"
    };
    vec![Span::styled(square, fill_style(kit, style)), Span::raw(" ")]
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
    // Side by side, a square must fit its own cell.
    let glyph = if kit.glyphs.halves.is_some() {
        "▪"
    } else {
        "#"
    };
    let roles = [Role::Success, Role::Warn, Role::Danger];
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

/// Columns `rows` high, each its parts stacked from the bottom in order, a part taking its
/// share of the fullest column's height in whole cells. A column is `width` cells with the
/// blank after it: two cells and a blank, or one cell. A column with no parts stays empty.
pub(super) fn columns(
    kit: &Kit<'_>,
    columns: &[Vec<(u64, Style)>],
    rows: usize,
    width: usize,
) -> Vec<Vec<Span<'static>>> {
    let top = columns
        .iter()
        .map(|parts| parts.iter().map(|(value, _)| value).sum::<u64>())
        .max()
        .unwrap_or(0)
        .max(1);
    // Each column's cells from the bottom, as the part filling each.
    let stacks: Vec<Vec<(usize, Style)>> = columns
        .iter()
        .map(|parts| {
            let mut cells = Vec::new();
            for (k, (value, style)) in parts.iter().enumerate() {
                let height = (*value as f64 / top as f64 * rows as f64).round() as usize;
                cells.extend(std::iter::repeat_n((k, *style), height));
            }
            cells.truncate(rows);
            cells
        })
        .collect();
    let block = if width > 1 { "██" } else { "█" };
    const FILLS: [&str; 4] = ["█", "▓", "▒", "░"];
    let mut out = Vec::new();
    for row in (0..rows).rev() {
        let mut line = Vec::new();
        for (k, stack) in stacks.iter().enumerate() {
            if k > 0 && width > 1 {
                line.push(Span::raw(" "));
            }
            line.push(match stack.get(row) {
                None => Span::raw(" ".repeat(block.chars().count())),
                Some((_, style)) if kit.has_color() => Span::styled(block, *style),
                Some((part, _)) => {
                    Span::raw(FILLS[part % FILLS.len()].repeat(block.chars().count()))
                }
            });
        }
        out.push(merge(line));
    }
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
    legend_apart(kit, entries, tail, width, 3)
}

/// [`legend`] with the entries `gap` cells apart.
pub(super) fn legend_apart(
    kit: &Kit<'_>,
    entries: &[Entry],
    tail: Option<String>,
    width: usize,
    gap: usize,
) -> Vec<Vec<Span<'static>>> {
    let rows = flow(kit, entries, tail.clone(), width, gap);
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
    let shorter = flow(kit, &bare, tail, width, gap);
    if shorter.len() < rows.len() {
        shorter
    } else {
        rows
    }
}

/// Entries on as few rows of `width` as they need, then shared evenly between those rows, so
/// six that need two rows read three and three. Where an even row would run over, they flow.
pub(super) fn legend_even(
    kit: &Kit<'_>,
    entries: &[Entry],
    width: usize,
    gap: usize,
) -> Vec<Vec<Span<'static>>> {
    let flowed = flow(kit, entries, None, width, gap);
    if flowed.len() < 2 {
        return flowed;
    }
    let even: Vec<Vec<Span<'static>>> = entries
        .chunks(entries.len().div_ceil(flowed.len()))
        .flat_map(|row| flow(kit, row, None, usize::MAX, gap))
        .collect();
    if even.iter().all(|row| spans_width(row) <= width) {
        even
    } else {
        flowed
    }
}

fn flow(
    kit: &Kit<'_>,
    entries: &[Entry],
    tail: Option<String>,
    width: usize,
    gap: usize,
) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut place = |spans: Vec<Span<'static>>, row: &mut Vec<Span<'static>>| {
        let needs = spans_width(&spans) + if row.is_empty() { 0 } else { gap };
        if !row.is_empty() && spans_width(row) + needs > width {
            rows.push(std::mem::take(row));
        }
        if !row.is_empty() {
            row.push(Span::raw(" ".repeat(gap)));
        }
        row.extend(spans);
    };
    for entry in entries {
        let mut numbers = Vec::new();
        if !entry.amount.is_empty() {
            numbers.push(Span::raw(" "));
            numbers.push(kit.strong(entry.amount.clone()));
        }
        if !entry.note.is_empty() {
            numbers.push(kit.dim(format!(" {}", entry.note)));
        }
        let mut spans = swatch(kit, entry.style);
        // On a row too narrow for the whole entry the name gives way, never its numbers.
        let room = width.saturating_sub(spans_width(&spans) + spans_width(&numbers));
        spans.push(Span::raw(cut(&entry.name, room.max(LEAST_LEGEND_NAME))));
        spans.extend(numbers);
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
    use ratatui::style::Color;

    use super::*;
    use crate::theme::{ColorLevel, Theme};

    fn colored() -> Theme {
        let mut theme = Theme::dark().with_level(ColorLevel::TrueColor);
        theme.glyphs = crate::theme::Glyphs::UNICODE;
        theme
    }

    #[test]
    fn an_idle_slot_is_the_grey_track_and_one_ahead_is_blank() {
        let theme = colored();
        let kit = Kit::new(&theme);
        let style = kit.fg(Role::AgentBlue);
        let line = spark(&kit, &[Some(0), Some(4), Some(8), None], 8, style);
        let text: String = line.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "▁▄█ ");
        assert_eq!(line[0].style, kit.fg(Role::Faint));
        assert_eq!(line[1].style, style);
    }

    #[test]
    fn ten_squares_never_overrun_and_say_how_work_fared() {
        let theme = colored();
        let kit = Kit::new(&theme);
        let text = |fared: Fared| -> Vec<(String, Option<Color>)> {
            tenths(&kit, fared, 10)
                .into_iter()
                .map(|span| (span.content.into_owned(), span.style.fg))
                .collect()
        };
        let green = kit.fg(Role::Success).fg;
        let amber = kit.fg(Role::Warn).fg;
        let first = |first, once, more| Fared { first, once, more };
        assert_eq!(text(first(0, 2, 0)), [("▪".repeat(10), amber)]);
        assert_eq!(
            text(first(7, 2, 0)),
            [("▪".repeat(8), green), ("▪".repeat(2), amber)]
        );
        // Halves that both round up still make ten.
        let squares: usize = tenths(&kit, first(1, 1, 0), 10)
            .iter()
            .map(|span| span.content.chars().count())
            .sum();
        assert_eq!(squares, 10);
    }

    #[test]
    fn a_stacked_bar_is_exactly_its_width_with_what_is_left_over_by_largest_remainder() {
        let theme = colored();
        let kit = Kit::new(&theme);
        let roles = [Role::Success, Role::Warn, Role::Danger, Role::AgentBlue];
        let cells = |values: &[u64], width: usize| -> Vec<usize> {
            let parts: Vec<(u64, Style)> = values
                .iter()
                .zip(roles)
                .map(|(value, role)| (*value, kit.fg(role)))
                .collect();
            stacked(&kit, &parts, width)
                .iter()
                .map(|span| span.content.chars().count())
                .collect()
        };
        // 58.26, 1.5, 0.18 and 0.06 cells: the one cell left over goes to the 1.5, not to
        // the last part.
        assert_eq!(cells(&[971, 25, 3, 1], 60), [58, 2]);
        assert_eq!(cells(&[880, 115, 5], 36), [32, 4]);
        assert_eq!(cells(&[92, 8], 114), [105, 9]);
        // 6.67 cells each: the first two take the two left over.
        assert_eq!(cells(&[1, 1, 1], 20), [7, 7, 6]);
        assert_eq!(cells(&[990, 9, 1], 20), [20]);
    }

    #[test]
    fn every_part_gets_a_cell_and_the_cells_add_up() {
        assert_eq!(cells_of(&[58, 31, 11], 100, 20), [12, 6, 2]);
        assert_eq!(cells_of(&[990, 9, 1], 1000, 10), [8, 1, 1]);
        assert_eq!(cells_of(&[1, 1], 0, 10), [0, 0]);
        // Less than the whole leaves the track.
        assert_eq!(cells_of(&[25], 100, 8), [2]);
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
        assert_eq!(text, "▪▪▪▪▪▪▪");
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
