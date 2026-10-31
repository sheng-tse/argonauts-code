//! Bars and meters, each one row of cells.
//!
//! A stacked bar splits a whole into colored parts with edges on eighths of a cell. Every
//! non-zero part gets at least one cell, taken back from the largest part, so at most one
//! boundary falls inside a cell: the eighth block is the left part's color on the right part's.
//! Block meters are Claude Code's /usage meter. Meters that stack in back-to-back rows are half
//! meters instead, the lower half of each cell, since block rows merge into one slab in a
//! terminal. Without color they are line meters, `━` on `─`.

use ratatui::text::Span;

use super::{Kit, merge};
use crate::base::theme::Role;

/// The parts of a whole, drawn as one stacked bar or line.
#[derive(Debug, Clone, PartialEq)]
pub struct Stack {
    /// Each part's value and color, in order. A part in the track's role counts as free.
    pub parts: Vec<(u64, Role)>,
    pub total: u64,
    /// An amber mark where something happens, such as the compaction point.
    pub tick: Option<u64>,
    /// Free cells from here on draw in `held`, the part held back past the tick.
    pub held_from: Option<u64>,
    /// What's left of the bar after the parts.
    pub track: Role,
}

/// Under this many cells, parts smaller than a cell fold into one `other` part.
const FOLD_BELOW_CELLS: usize = 40;

/// Halves round to even, so a bar draws the same here as in the mockups.
fn round(x: f64) -> usize {
    x.round_ties_even().max(0.0) as usize
}

fn fraction(value: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        value as f64 / total as f64
    }
}

impl Stack {
    fn is_track(&self, role: Role) -> bool {
        role == self.track || role == Role::Held
    }

    /// The parts as drawn in `cells`: on a short bar, two or more parts smaller than a cell
    /// fold into one `other` part, placed before the free parts.
    fn folded(&self, cells: usize) -> Vec<(u64, Role)> {
        if cells >= FOLD_BELOW_CELLS {
            return self.parts.clone();
        }
        let cell_value = self.total as f64 / cells.max(1) as f64;
        let small = |&(value, role): &(u64, Role)| {
            value > 0 && (value as f64) < cell_value && !self.is_track(role)
        };
        if self.parts.iter().filter(|p| small(p)).count() < 2 {
            return self.parts.clone();
        }
        let other: u64 = self.parts.iter().filter(|p| small(p)).map(|p| p.0).sum();
        let mut out: Vec<(u64, Role)> = self.parts.iter().filter(|p| !small(p)).copied().collect();
        let at = out
            .iter()
            .position(|&(_, role)| self.is_track(role))
            .unwrap_or(out.len());
        out.insert(at, (other, Role::CatOther));
        out
    }

    /// Each part's share of `cells * unit` units, at least `unit` for a non-zero part, and the
    /// role of every unit in order, the track filling the rest.
    fn units(&self, cells: usize, unit: usize) -> Vec<Role> {
        let parts = self.folded(cells);
        let units = cells * unit;
        let mut raw: Vec<usize> = parts
            .iter()
            .map(|&(value, _)| {
                if value == 0 {
                    0
                } else {
                    round(fraction(value, self.total) * units as f64).max(unit)
                }
            })
            .collect();
        let mut over = raw.iter().sum::<usize>().saturating_sub(units);
        while over > 0 {
            // The first of the largest parts gives the room back, never below one cell.
            let Some(largest) = (0..raw.len()).max_by_key(|&k| (raw[k], std::cmp::Reverse(k)))
            else {
                break;
            };
            let spare = raw[largest].saturating_sub(unit);
            if spare == 0 {
                break;
            }
            let give = spare.min(over);
            raw[largest] -= give;
            over -= give;
        }
        let mut roles = Vec::with_capacity(units);
        for (n, &(_, role)) in raw.iter().zip(&parts) {
            roles.extend(std::iter::repeat_n(role, *n));
        }
        roles.resize(units, self.track);
        roles
    }

    /// The cell a value falls in.
    fn cell_of(&self, value: u64, cells: usize) -> usize {
        round(fraction(value, self.total) * cells as f64).min(cells.saturating_sub(1))
    }
}

/// A stacked bar `cells` wide.
pub fn stacked(kit: &Kit<'_>, stack: &Stack, cells: usize) -> Vec<Span<'static>> {
    let mut eighths = stack.units(cells, 8);
    if let Some(from) = stack.held_from {
        let start = round(fraction(from, stack.total) * cells as f64) * 8;
        for role in eighths.iter_mut().skip(start) {
            if *role == stack.track {
                *role = Role::Held;
            }
        }
    }
    let tick = stack.tick.map(|at| stack.cell_of(at, cells));
    let glyphs = kit.glyphs;
    let mut out = Vec::with_capacity(cells);
    for (c, cell) in eighths.chunks(8).enumerate() {
        let (first, last) = (cell[0], cell[7]);
        let used = cell.iter().filter(|r| !stack.is_track(**r)).count() >= 4;
        let span = if !kit.has_color() {
            let glyph = match (Some(c) == tick, used) {
                (true, _) => glyphs.tick,
                (false, true) => glyphs.used,
                (false, false) => glyphs.free,
            };
            Span::raw(glyph)
        } else if Some(c) == tick {
            let mark = if glyphs.eighths.is_some() {
                "▏"
            } else {
                glyphs.tick
            };
            Span::styled(mark, kit.cell(Role::Tick, last))
        } else if first == last {
            Span::styled(" ", kit.fill(first))
        } else {
            match glyphs.eighths {
                Some(blocks) => {
                    let k = cell.iter().position(|r| *r == last).unwrap_or(0);
                    Span::styled(blocks[k], kit.cell(first, last))
                }
                // Without eighths a cell takes the color that covers most of it.
                None => {
                    let k = cell.iter().position(|r| *r == last).unwrap_or(0);
                    Span::styled(" ", kit.fill(if k >= 4 { first } else { last }))
                }
            }
        };
        out.push(span);
    }
    merge(out)
}

/// A stacked bar in the lower half of each cell, for a card whose meters are half meters, so
/// every bar on it is one height. Each part ends on a cell. Where color can't tell the parts
/// apart it is the stacked bar in its own colorless form.
pub fn half_stacked(kit: &Kit<'_>, stack: &Stack, cells: usize) -> Vec<Span<'static>> {
    let glyphs = kit.glyphs;
    let (Some([_, whole]), true) = (glyphs.halves, kit.has_color()) else {
        return stacked(kit, stack, cells);
    };
    let held_from = stack
        .held_from
        .map(|from| round(fraction(from, stack.total) * cells as f64));
    let tick = stack.tick.map(|at| stack.cell_of(at, cells));
    let out = stack
        .units(cells, 1)
        .into_iter()
        .enumerate()
        .map(|(c, role)| {
            if Some(c) == tick {
                return Span::styled(glyphs.line_tick, kit.fg(Role::Tick));
            }
            let held = role == stack.track && held_from.is_some_and(|from| c >= from);
            Span::styled(whole, kit.fg(if held { Role::Held } else { role }))
        })
        .collect();
    merge(out)
}

/// Claude Code's /usage meter: fill on a darker track, the edge cell in eighths.
pub fn block_meter(
    kit: &Kit<'_>,
    ratio: f64,
    cells: usize,
    fill: Role,
    track: Role,
) -> Vec<Span<'static>> {
    let edge = round(ratio.clamp(0.0, 1.0) * cells as f64 * 8.0);
    let glyphs = kit.glyphs;
    let out = (0..cells)
        .map(|c| {
            let n = edge.saturating_sub(c * 8).min(8);
            if !kit.has_color() {
                return Span::raw(if n >= 4 { glyphs.used } else { glyphs.free });
            }
            match (n, glyphs.eighths) {
                (8, _) => Span::styled(" ", kit.fill(fill)),
                (0, _) => Span::styled(" ", kit.fill(track)),
                (n, Some(blocks)) => Span::styled(blocks[n], kit.cell(fill, track)),
                (n, None) => Span::styled(" ", kit.fill(if n >= 4 { fill } else { track })),
            }
        })
        .collect();
    merge(out)
}

/// The cell a meter's tick lands in, for a ratio along `cells`.
fn tick_cell(tick: Option<f64>, cells: usize) -> Option<usize> {
    tick.map(|at| round(at.clamp(0.0, 1.0) * cells as f64).min(cells.saturating_sub(1)))
}

/// The meter of rows that sit back to back, as the bottom line draws its bars: the lower
/// half of each cell, fill on a darker track, its edge to half a cell, and `┃` at `tick`, a
/// ratio. The upper half stays empty, so rows of them never merge. Where color can't tell
/// fill from track it is the line meter, whose characters do.
pub fn half_meter(
    kit: &Kit<'_>,
    ratio: f64,
    cells: usize,
    fill: Role,
    tick: Option<f64>,
) -> Vec<Span<'static>> {
    let glyphs = kit.glyphs;
    let (Some([half, whole]), true) = (glyphs.halves, kit.has_color()) else {
        return line_meter(kit, ratio, cells, fill, tick);
    };
    let halves = round(ratio.clamp(0.0, 1.0) * cells as f64 * 2.0);
    let tick = tick_cell(tick, cells);
    let out = (0..cells)
        .map(|c| {
            if Some(c) == tick {
                return Span::styled(glyphs.line_tick, kit.fg(Role::Tick));
            }
            match halves.saturating_sub(c * 2) {
                0 => Span::styled(whole, kit.fg(Role::BarTrack)),
                1 => Span::styled(half, kit.fg(fill)),
                _ => Span::styled(whole, kit.fg(fill)),
            }
        })
        .collect();
    merge(out)
}

/// A meter in thin lines: `━` fill on a `─` track, `╸` for a half cell, and `┃` at `tick`, a
/// ratio. The same characters without color.
pub fn line_meter(
    kit: &Kit<'_>,
    ratio: f64,
    cells: usize,
    fill: Role,
    tick: Option<f64>,
) -> Vec<Span<'static>> {
    let glyphs = kit.glyphs;
    let halves = round(ratio.clamp(0.0, 1.0) * cells as f64 * 2.0);
    let (full, half) = (halves / 2, halves % 2 == 1);
    let tick = tick_cell(tick, cells);
    let out = (0..cells)
        .map(|c| {
            if Some(c) == tick {
                Span::styled(glyphs.line_tick, kit.fg(Role::Tick))
            } else if c < full {
                Span::styled(glyphs.line_fill, kit.fg(fill))
            } else if c == full && half {
                Span::styled(glyphs.line_half, kit.fg(fill))
            } else {
                Span::styled(glyphs.line_track, kit.fg(Role::BarTrack))
            }
        })
        .collect();
    merge(out)
}

/// A plan window's meter, as the bottom line and the pickers draw a window: the lower half of
/// each cell, its edge to half a cell and at least half a cell for anything used, over a thin
/// faint line at the foot of the cells it leaves, so a row of them reads as marks beside their
/// numbers rather than blocks, and by its shape without colour too. Only a used-up window
/// fills its last half cell. Without colour or block glyphs it is the block meter, which has a
/// form for both.
pub fn window_meter(kit: &Kit<'_>, ratio: f64, cells: usize, fill: Role) -> Vec<Span<'static>> {
    let ratio = ratio.clamp(0.0, 1.0);
    if !kit.has_color() || kit.theme.glyphs.is_ascii() {
        return block_meter(kit, ratio, cells, fill, Role::BarTrack);
    }
    let mut halves = short_of_full(round(ratio * (cells * 2) as f64), ratio, cells * 2);
    if ratio > 0.0 {
        halves = halves.max(1);
    }
    let cell = |at: usize| match halves.saturating_sub(at * 2) {
        0 => Span::styled("▁", kit.fg(Role::Faint)),
        1 => Span::styled("▖", kit.fg(fill)),
        _ => Span::styled("▄", kit.fg(fill)),
    };
    merge((0..cells).map(cell).collect())
}

/// A plan window's meter in whole cells, as the bottom line draws a window: [`window_meter`]
/// with its edge on a cell's edge, and at least one cell for any use, so it never leaves half a
/// cell blank before the track. The last cell fills only at 100%: 92% of six cells shows five.
pub fn whole_window_meter(
    kit: &Kit<'_>,
    share: f64,
    cells: usize,
    fill: Role,
) -> Vec<Span<'static>> {
    let share = share.clamp(0.0, 1.0);
    let mut filled = short_of_full(round(share * cells as f64), share, cells);
    if share > 0.0 {
        filled = filled.max(1);
    }
    window_meter(kit, filled as f64 / cells.max(1) as f64, cells, fill)
}

/// `units` of `whole`, one short of all of them unless the share is the whole window.
fn short_of_full(units: usize, share: f64, whole: usize) -> usize {
    if share < 1.0 {
        units.min(whole.saturating_sub(1))
    } else {
        units
    }
}

/// A thin bar of `━`: the share in `fill`, the rest of the track faint, and at least one
/// cell of fill for any share above nothing. Without colour it is the line meter.
pub fn thin_bar(kit: &Kit<'_>, ratio: f64, cells: usize, fill: Role) -> Vec<Span<'static>> {
    if !kit.has_color() {
        return line_meter(kit, ratio, cells, fill, None);
    }
    let ratio = ratio.clamp(0.0, 1.0);
    let mut filled = round(ratio * cells as f64).min(cells);
    if ratio > 0.0 && filled == 0 && cells > 0 {
        filled = 1;
    }
    let line = kit.glyphs.line_fill;
    merge(vec![
        Span::styled(line.repeat(filled), kit.fg(fill)),
        Span::styled(line.repeat(cells - filled), kit.fg(Role::Faint)),
    ])
}

/// The characters bars, meters and squares are drawn with.
pub const BAR_CELLS: &[char] = &[
    '━', '╸', '┃', '█', '▉', '▊', '▋', '▌', '▍', '▎', '▏', '▐', '▀', '▄', '▁', '▂', '▃', '▅', '▆',
    '▇', '░', '▒', '▓', '▖', '▗', '▘', '▝', '▙', '▛', '▜', '▟', '▚', '▞', '▪', '■', '⛀', '⛁', '⛶',
];

fn is_bar(ch: char) -> bool {
    BAR_CELLS.contains(&ch)
}

/// Puts `next` after `row`, with one blank cell between when the row ends in a bar and `next`
/// starts with anything but a blank, so no number or name ever touches a bar.
pub fn then(row: &mut Vec<Span<'static>>, next: Vec<Span<'static>>) {
    let ends_in_bar = row
        .iter()
        .rev()
        .find_map(|span| span.content.chars().next_back())
        .is_some_and(is_bar);
    let starts_blank = next
        .iter()
        .find_map(|span| span.content.chars().next())
        .is_none_or(char::is_whitespace);
    if ends_in_bar && !starts_blank {
        row.push(Span::raw(" "));
    }
    row.extend(next);
}

/// `row` cut to `width` without cutting into a bar or squares: words that don't fit are cut
/// with `…` a cell short, since Iosevka paints the `…` two cells wide, or left out, and a bar
/// that doesn't fit whole goes with all that follows it.
pub fn fit_keeping_bars(row: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    if crate::base::text::spans_width(&row) <= width {
        return row;
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for span in row {
        let cells = crate::base::text::str_width(&span.content);
        if used + cells <= width {
            used += cells;
            kept.push(span);
            continue;
        }
        if !span.content.chars().any(is_bar) && width > used + 1 {
            let cut =
                crate::base::text::cut_spans(&[span], width - used, crate::base::text::At::Char);
            kept.extend(cut);
        }
        break;
    }
    // A bar left last keeps no blank after it.
    while kept
        .last()
        .is_some_and(|span: &Span<'static>| span.content.trim().is_empty())
    {
        kept.pop();
    }
    kept
}

/// Where a bar touches a character that is neither a blank nor part of a bar, on either side,
/// by its column: what [`then`] keeps from happening.
pub fn touching_bar(line: &str) -> Option<usize> {
    let cells: Vec<char> = line.chars().collect();
    cells.windows(2).position(|pair| {
        let [a, b] = [pair[0], pair[1]];
        // The year of days marks a day with no work `·`, a square of the map like the others.
        let text = |ch: char| !ch.is_whitespace() && !is_bar(ch) && ch != '·';
        (is_bar(a) && text(b)) || (text(a) && is_bar(b))
    })
}

/// A stacked bar drawn as a line meter: each cell `━` in its part's color, free as `─`.
pub fn stacked_line(kit: &Kit<'_>, stack: &Stack, cells: usize) -> Vec<Span<'static>> {
    let glyphs = kit.glyphs;
    let halves = stack.units(cells, 2);
    let tick = stack.tick.map(|at| stack.cell_of(at, cells));
    let out = halves
        .chunks(2)
        .enumerate()
        .map(|(c, pair)| {
            let (first, last) = (pair[0], pair[1]);
            if Some(c) == tick {
                return Span::styled(glyphs.line_tick, kit.fg(Role::Tick));
            }
            let role = if stack.is_track(first) { last } else { first };
            let glyph = if stack.is_track(role) {
                glyphs.line_track
            } else if stack.is_track(first) || stack.is_track(last) {
                glyphs.line_half
            } else {
                glyphs.line_fill
            };
            let style = if stack.is_track(role) {
                kit.fg(Role::BarTrack)
            } else {
                kit.fg(role)
            };
            Span::styled(glyph, style)
        })
        .collect();
    merge(out)
}

/// `▰▰▰▱▱`, a meter inside a line of text. Any amount over nothing shows at least one.
pub fn pills(kit: &Kit<'_>, ratio: f64, count: usize, fill: Role) -> Vec<Span<'static>> {
    let mut on = round(ratio.max(0.0) * count as f64).min(count);
    if ratio > 0.0 && on == 0 {
        on = 1;
    }
    let mut out = Vec::new();
    if on > 0 {
        out.push(Span::styled(kit.glyphs.pill_on.repeat(on), kit.fg(fill)));
    }
    if on < count {
        out.push(Span::styled(
            kit.glyphs.pill_off.repeat(count - on),
            kit.style(Role::Faint),
        ));
    }
    out
}

/// A column chart `rows` high, top row first: one cell per value and a blank between. The
/// last column can take its own role, to mark today.
pub fn columns(
    kit: &Kit<'_>,
    values: &[f64],
    rows: usize,
    role: Role,
    last_role: Option<Role>,
) -> Vec<Vec<Span<'static>>> {
    let top = values.iter().copied().fold(0.0, f64::max);
    let top = if top > 0.0 { top } else { 1.0 };
    let levels: Vec<usize> = values
        .iter()
        .map(|v| round(v.max(0.0) / top * (rows * 8) as f64))
        .collect();
    (0..rows)
        .rev()
        .map(|r| {
            let mut line = Vec::new();
            for (k, level) in levels.iter().enumerate() {
                let m = level.saturating_sub(r * 8).min(8);
                let glyph = if m == 0 {
                    " "
                } else {
                    kit.glyphs.columns[m - 1]
                };
                let role = match last_role {
                    Some(last) if k + 1 == levels.len() => last,
                    _ => role,
                };
                line.push(Span::styled(glyph, kit.fg(role)));
                if k + 1 < levels.len() {
                    line.push(Span::raw(" "));
                }
            }
            merge(line)
        })
        .collect()
}

#[cfg(test)]
mod tests;
