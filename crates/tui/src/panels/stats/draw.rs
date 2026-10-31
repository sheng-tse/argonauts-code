//! What Stats draws with beyond the kit and its fills ([`super::fills`]): thin bars of parts on
//! a faint track, ten small squares for how work fared, a sparkline, the chart's columns
//! stacked by CLI or model, and the table of a bar's parts under it. A bar is a thin line
//! through the middle of its cells and a square is one cell wide in every font, so nothing
//! touches the row above or the cell beside it; a blank cell always sits between a bar or
//! squares and the words after them. A CLI's models share its hue, so a bar of models fills
//! each one's cells with its texture instead.

use argonauts_core::behaviors::crew_stats::StatsRole;
use argonauts_core::look::display_names::cut_at_word;
use argonauts_core::look::logos::Logo;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use super::fills::table_swatch;
pub(in crate::panels) use super::fills::{Fill, swatch};
use crate::base::kit::legend::{self, Columns, Figure, TableRow};
use crate::base::kit::logo::logo_spans;
use crate::base::kit::{Kit, merge};
use crate::base::text::{str_width, truncate_spans};
pub(in crate::panels) use crate::base::theme::shades::shade;
use crate::base::theme::{Brightness, ColorLevel, Role, nearest_ansi256};

/// The most cells of a model's name on a card or in a legend, the longest the drawings write
/// whole. A longer one is cut with `…`, as a provider's own id for a model often has to be.
pub(super) const MODEL_NAME: usize = 16;

/// A model's name as a legend writes it, cut as [`cut_name`] cuts it.
pub(super) fn model_name(name: &str) -> String {
    cut_name(name, MODEL_NAME)
}

/// `name` in `cells`: whole where it fits, else cut at a word, with a blank left after the `…`
/// for fonts that draw it two cells wide.
pub(super) fn cut_name(name: &str, cells: usize) -> String {
    if str_width(name) <= cells {
        name.to_owned()
    } else {
        cut_at_word(name, cells.saturating_sub(1))
    }
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
pub(in crate::panels) fn cells_of(values: &[u64], total: u64, cells: usize) -> Vec<usize> {
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
pub(in crate::panels) fn bar(
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
pub(super) const THIN_PARTS: [&str; 4] = ["━", "═", "─", "╌"];

/// The cells `value` of `whole` takes of `cells`: its share in whole percents, as a legend
/// writes it, so a bar and the numbers beside it agree.
fn cells_for(value: u64, whole: u64, cells: usize) -> usize {
    let percent = (value as f64 / whole.max(1) as f64 * 100.0).round();
    (percent / 100.0 * cells as f64).round() as usize
}

/// `value` of `whole` as a thin bar `cells` wide: its share rounded, at least a cell when
/// it has any, in `style`, the rest on the faint track.
pub(in crate::panels) fn thin(
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
/// legend. A model's part fills its cells with its texture.
pub(in crate::panels) fn stacked<F: Into<Fill> + Copy>(
    kit: &Kit<'_>,
    parts: &[(u64, F)],
    cells: usize,
) -> Vec<Span<'static>> {
    let total: u64 = parts.iter().map(|(value, _)| value).sum();
    if total == 0 {
        return thin(kit, 0, 1, cells, Style::new());
    }
    let values: Vec<u64> = parts.iter().map(|(value, _)| *value).collect();
    let widths = by_largest_remainder(&values, total, cells);
    let mut out = Vec::new();
    for (k, ((_, fill), width)) in parts.iter().zip(widths).enumerate() {
        if width == 0 {
            continue;
        }
        let fill: Fill = (*fill).into();
        out.push(match fill.texture {
            Some(texture) => Span::styled(texture.repeat(width), fill_style(kit, fill.style)),
            None if kit.has_color() => Span::styled(THIN.repeat(width), fill.style),
            None => Span::raw(THIN_PARTS[k % THIN_PARTS.len()].repeat(width)),
        });
    }
    merge(out)
}

/// A part of a bar the keys may light only some of: what it counts, how much of that is lit,
/// and its fill.
#[derive(Debug, Clone, Copy)]
pub(super) struct Lit {
    pub value: u64,
    pub lit: u64,
    pub fill: Fill,
}

/// [`stacked`] where the keys point at one thing that each part holds some of: a part's
/// cells split between what of it is lit, in its fill, and the rest in grey. With `apart` a
/// blank cell sits between parts, as between CLIs, and every part over nothing gets a cell.
pub(super) fn stacked_lit(
    kit: &Kit<'_>,
    parts: &[Lit],
    apart: bool,
    cells: usize,
) -> Vec<Span<'static>> {
    let total: u64 = parts.iter().map(|part| part.value).sum();
    if total == 0 {
        return thin(kit, 0, 1, cells, Style::new());
    }
    let values: Vec<u64> = parts.iter().map(|part| part.value).collect();
    let widths = if apart {
        let gaps = values
            .iter()
            .filter(|value| **value > 0)
            .count()
            .saturating_sub(1);
        let mut widths = cells_of(&values, total, cells.saturating_sub(gaps));
        // A part too narrow a bar to keep a cell needs no blank before it either: the blank
        // goes to the widest part, so the bar keeps its width.
        let drawn = widths.iter().filter(|width| **width > 0).count();
        let spare = gaps.saturating_sub(drawn.saturating_sub(1));
        if let Some(widest) = (0..widths.len()).max_by_key(|at| widths[*at]) {
            widths[widest] += spare;
        }
        widths
    } else {
        by_largest_remainder(&values, total, cells)
    };
    let grey = fill_style(kit, kit.fg(Role::Faint));
    let mut out = Vec::new();
    for (k, (part, width)) in parts.iter().zip(widths).enumerate() {
        if width == 0 {
            continue;
        }
        if apart && !out.is_empty() {
            out.push(Span::raw(" "));
        }
        let glyph = part.fill.texture.unwrap_or(if kit.has_color() {
            THIN
        } else {
            THIN_PARTS[k % THIN_PARTS.len()]
        });
        let lit = part.lit.min(part.value);
        let split = cells_of(&[lit, part.value - lit], part.value, width);
        let styles = [fill_style(kit, part.fill.style), grey];
        for (cells, style) in split.into_iter().zip(styles) {
            if cells > 0 {
                out.push(Span::styled(glyph.repeat(cells), style));
            }
        }
    }
    merge(out)
}

/// A part of a bar and its row in the table under it: its swatch, a logo before it where one
/// stands for the part, its name, and its amount and share.
#[derive(Debug, Clone)]
pub(super) struct Legend {
    pub part: Lit,
    pub logo: Option<Logo>,
    pub name: String,
    pub figure: Figure,
    /// The keys are on its row.
    pub pointed: bool,
}

impl Legend {
    /// A part in `fill` that no logo stands for and the keys don't point at: `value` of
    /// `whole`, of which `lit` stays lit, written as `amount`.
    pub fn of(
        fill: impl Into<Fill>,
        name: &str,
        (value, lit, whole): (u64, u64, u64),
        amount: String,
    ) -> Self {
        Self {
            part: Lit {
                value,
                lit,
                fill: fill.into(),
            },
            logo: None,
            name: name.to_owned(),
            figure: Figure {
                amount,
                share: legend::share(value, whole),
            },
            pointed: false,
        }
    }
}

/// The columns of tables of one measure that share them, `width` cells wide: the names as
/// wide as the longest of `legends`, and the amounts as the widest. Their emblems keep cells for
/// a logo where any of them has one, and with `pointer` the rows keep cells for the keys'.
pub(super) fn table_columns<'a>(
    legends: impl Iterator<Item = &'a Legend> + Clone,
    width: usize,
    pointer: bool,
) -> Columns {
    let widest = (legends.clone())
        .map(|row| str_width(&row.figure.amount))
        .max();
    let longest = (legends.clone()).map(|row| str_width(&row.name)).max();
    let logos = legends.into_iter().any(|row| row.logo.is_some());
    let emblem = if logos { EMBLEM } else { legend::SWATCH };
    let (widest, longest) = (widest.unwrap_or(0), longest.unwrap_or(0));
    let columns = Columns::packed(width, emblem, 1, widest, longest);
    if pointer {
        columns
    } else {
        columns.unpointed()
    }
}

/// A table's rows in `columns`, one for each of `legends`, the parts of one bar in its order:
/// the swatch of each, its logo first where the table has logos, its name, amount and share.
/// With `zeros` off, a part of nothing has no row.
pub(super) fn table(
    kit: &Kit<'_>,
    legends: &[Legend],
    columns: Columns,
    zeros: bool,
) -> Vec<Vec<Span<'static>>> {
    (legends.iter().enumerate())
        .filter(|(_, row)| zeros || row.part.value > 0)
        .map(|(k, row)| {
            let swatch = table_swatch(kit, row.part.fill, k);
            let emblem = if columns.emblem >= EMBLEM {
                emblem(kit, row.logo.as_ref(), swatch)
            } else {
                vec![swatch, Span::raw("  ")]
            };
            let row = TableRow {
                emblem,
                name: vec![Span::raw(row.name.clone())],
                figures: vec![row.figure.clone()],
                pointed: row.pointed,
            };
            legend::table_row(kit, columns, &row)
        })
        .collect()
}

/// A dim row under a table, from where its swatches start, such as what its parts are parts
/// of: the rest of a row `width` wide, cut with `…` where it's longer.
pub(super) fn table_note(
    kit: &Kit<'_>,
    columns: Columns,
    words: String,
    width: usize,
) -> Vec<Span<'static>> {
    let mut row = vec![Span::raw(" ".repeat(columns.pointer))];
    let room = width.saturating_sub(columns.pointer);
    row.extend(truncate_spans(&[kit.dim(words)], room));
    row
}

/// The cells of what stands before a name in a table row with logos: a logo, a blank, a
/// swatch and two blanks. A table without logos keeps [`legend::SWATCH`]'s alone.
pub(super) const EMBLEM: usize = 7;

/// What stands before a name in a table row, [`EMBLEM`] cells: a logo, a blank, `swatch` two
/// cells wide and two blanks. A row with no logo keeps its cells blank, so every name of the
/// table starts in one column.
pub(super) fn emblem(
    kit: &Kit<'_>,
    logo: Option<&Logo>,
    swatch: Span<'static>,
) -> Vec<Span<'static>> {
    let mut emblem = match logo {
        Some(logo) => logo_spans(logo, kit.theme),
        None => vec![Span::raw("  ")],
    };
    emblem.push(Span::raw(" "));
    emblem.push(swatch);
    emblem.push(Span::raw("  "));
    emblem
}

pub(super) fn fill_style(kit: &Kit<'_>, style: Style) -> Style {
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
pub(in crate::panels) fn spark(
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

/// One part of `total`, as a meter on the track.
pub(in crate::panels) fn meter(
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
/// A model's part is drawn in its texture.
pub(super) fn columns(
    kit: &Kit<'_>,
    columns: &[Vec<(u64, Fill)>],
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
    let stacks: Vec<Vec<(usize, Fill)>> = columns
        .iter()
        .map(|parts| {
            let mut cells = Vec::new();
            for (k, (value, fill)) in parts.iter().enumerate() {
                let height = (*value as f64 / top as f64 * rows as f64).round() as usize;
                cells.extend(std::iter::repeat_n((k, *fill), height));
            }
            cells.truncate(rows);
            cells
        })
        .collect();
    let cells = if width > 1 { 2 } else { 1 };
    const FILLS: [&str; 4] = ["█", "▓", "▒", "░"];
    let mut out = Vec::new();
    for row in (0..rows).rev() {
        let mut line = Vec::new();
        for (k, stack) in stacks.iter().enumerate() {
            if k > 0 && width > 1 {
                line.push(Span::raw(" "));
            }
            line.push(match stack.get(row) {
                None => Span::raw(" ".repeat(cells)),
                Some((part, fill)) => {
                    let glyph = fill.texture.unwrap_or(if kit.has_color() {
                        "█"
                    } else {
                        FILLS[part % FILLS.len()]
                    });
                    Span::styled(glyph.repeat(cells), fill_style(kit, fill.style))
                }
            });
        }
        out.push(merge(line));
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::base::text::spans_width;
    use crate::base::theme::{ColorLevel, Theme};

    fn colored() -> Theme {
        let mut theme = Theme::dark().with_level(ColorLevel::TrueColor);
        theme.glyphs = crate::base::theme::Glyphs::UNICODE;
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

    /// The text of `spans` and the color of each, without the empty ones.
    fn runs(spans: &[Span<'_>]) -> Vec<(String, Option<Color>)> {
        (spans.iter())
            .filter(|span| !span.content.is_empty())
            .map(|span| (span.content.to_string(), span.style.fg))
            .collect()
    }

    #[test]
    fn parts_apart_keep_a_blank_between_them_and_the_bar_its_width() {
        let theme = colored();
        let kit = Kit::new(&theme);
        let part = |value: u64, role: Role| Lit {
            value,
            lit: value,
            fill: kit.fg(role).into(),
        };
        let parts = [
            part(58, Role::Success),
            part(31, Role::Warn),
            part(11, Role::Danger),
        ];
        let bar = runs(&stacked_lit(&kit, &parts, true, 30));
        let shape: Vec<&str> = bar.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            shape,
            [
                "━".repeat(16),
                " ".into(),
                "━".repeat(9),
                " ".into(),
                "━".repeat(3)
            ]
        );
        // A part too small for a cell gets one, and the bar stays as wide.
        let parts = [
            part(990, Role::Success),
            part(9, Role::Warn),
            part(1, Role::Danger),
        ];
        let bar = stacked_lit(&kit, &parts, true, 20);
        let width: usize = bar.iter().map(|span| span.content.chars().count()).sum();
        assert_eq!(width, 20);
        assert_eq!(runs(&bar).len(), 5, "{bar:?}");
    }

    #[test]
    fn a_part_lit_in_part_keeps_the_rest_of_its_cells_in_grey() {
        let theme = colored();
        let kit = Kit::new(&theme);
        let green = kit.fg(Role::Success);
        let parts = [
            Lit {
                value: 60,
                lit: 15,
                fill: green.into(),
            },
            Lit {
                value: 40,
                lit: 0,
                fill: kit.fg(Role::Warn).into(),
            },
        ];
        // The first part's 12 cells split 3 lit and 9 grey, and the second's 8 are grey.
        let grey = kit.fg(Role::Faint).fg;
        assert_eq!(
            runs(&stacked_lit(&kit, &parts, false, 20)),
            [("━".repeat(3), green.fg), ("━".repeat(17), grey)]
        );
    }

    #[test]
    fn without_color_a_table_swatch_is_the_line_its_part_draws_in() {
        let mut theme = Theme::dark().with_level(ColorLevel::None);
        theme.glyphs = crate::base::theme::Glyphs::UNICODE;
        let kit = Kit::new(&theme);
        let fill: Fill = kit.fg(Role::Success).into();
        let swatches: Vec<String> = (0..4)
            .map(|k| table_swatch(&kit, fill, k).content.into_owned())
            .collect();
        assert_eq!(swatches, ["━━", "══", "──", "╌╌"]);
        let theme = colored();
        let kit = Kit::new(&theme);
        assert_eq!(table_swatch(&kit, fill, 2).content, "██");
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
    fn a_clis_models_fill_their_parts_and_swatches_with_their_textures() {
        let theme = colored();
        let kit = Kit::new(&theme);
        let hue = kit.fg(Role::CliCodex);
        let models: Vec<(u64, Fill)> = (0..5)
            .map(|place| {
                (
                    10,
                    Fill::model(&kit, shade(&kit, Role::CliCodex, place), place),
                )
            })
            .collect();
        let bar = stacked(&kit, &models, 10);
        let drawn: Vec<(&str, Style)> = bar
            .iter()
            .map(|span| (span.content.as_ref(), span.style))
            .collect();
        // The fourth and the fifth share the rest's solid grey, as one run.
        let grey = kit.fg(Role::CatOther);
        assert_eq!(drawn, [("██▚▚⣿⣿", hue), ("████", grey)]);
        let swatches: Vec<String> = models
            .iter()
            .map(|(_, fill)| swatch(&kit, *fill)[0].content.to_string())
            .collect();
        assert_eq!(swatches, ["█", "▚", "⣿", "█", "█"]);

        // A CLI or a role draws solid, as before.
        let solid = stacked(&kit, &[(1, hue), (1, grey)], 4);
        let text: String = solid.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "━━━━");
        assert_eq!(swatch(&kit, hue)[0].content, "■");
    }

    /// A long model name stops at a word, and its `…` keeps a blank after it.
    #[test]
    fn a_long_model_name_is_cut_at_a_word() {
        assert_eq!(cut_name("GLM 5.3 Flash Free", 16), "GLM 5.3 Flash…");
        assert_eq!(cut_name("DeepSeek V4.1 Flash Free", 16), "DeepSeek V4.1…");
        assert_eq!(cut_name("Qwen3 Coder 30B", 16), "Qwen3 Coder 30B");
    }

    #[test]
    fn without_color_a_models_texture_still_tells_it_apart() {
        let mut theme = colored();
        theme.level = ColorLevel::None;
        let kit = Kit::new(&theme);
        let models: Vec<(u64, Fill)> = (0..3)
            .map(|place| (1, Fill::model(&kit, Style::new(), place)))
            .collect();
        let text: String = stacked(&kit, &models, 6)
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "██▚▚⣿⣿");
        let column = columns(&kit, std::slice::from_ref(&models), 3, 1);
        let rows: Vec<String> = column
            .iter()
            .map(|row| row.iter().map(|span| span.content.as_ref()).collect())
            .collect();
        assert_eq!(rows, ["⣿", "▚", "█"]);
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
