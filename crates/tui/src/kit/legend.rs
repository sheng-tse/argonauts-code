//! A bar in words: a `▌` swatch, a name, a bold amount, and a dim share, in fixed slots up to
//! three to a row. The column count comes from the widest entry and the width, never from the
//! values, so a number changing never moves a column.

use ratatui::text::Span;

use super::{Kit, fit, ljust, rjust};
use crate::text::{str_width, truncate_spans};
use crate::theme::Role;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub role: Role,
    pub name: String,
    /// The amount as it reads, such as `3.4k` or `–` before a reading.
    pub amount: String,
    /// Its share of the whole, such as `0.3%`, or empty.
    pub share: String,
}

/// Cells between columns.
const GAP: usize = 3;
/// The amount's slot: `3.4k` to `1.2M`, and `48.2k`.
const AMOUNT_CELLS: usize = 5;
/// The share's slot: `0.3%` and ` 19%`. A `<0.1%` takes the space before it.
const SHARE_CELLS: usize = 4;

struct Slots {
    name: usize,
    amount: usize,
    share: usize,
}

impl Slots {
    fn of(entries: &[Entry]) -> Self {
        let widest =
            |f: fn(&Entry) -> &str| entries.iter().map(|e| str_width(f(e))).max().unwrap_or(0);
        Self {
            name: widest(|e| &e.name),
            // An estimate's `~` is the one thing that widens the amount, and it marks the
            // whole reading, so every entry has it or none does.
            amount: widest(|e| &e.amount).max(AMOUNT_CELLS),
            share: SHARE_CELLS,
        }
    }

    /// Swatch, space, name, space, amount, space, share.
    fn entry(&self) -> usize {
        2 + self.name + 1 + self.amount + 1 + self.share
    }
}

/// How many columns `entries` take in `width` cells, at most `max_cols`.
pub fn columns(entries: &[Entry], width: usize, max_cols: usize) -> usize {
    let entry = Slots::of(entries).entry();
    let mut cols = max_cols.max(1);
    while cols > 1 && cols * entry + (cols - 1) * GAP > width {
        cols -= 1;
    }
    cols
}

/// The cells `entries` take packed to the left: as many columns as `width` holds, at most
/// `max_cols`, each as wide as the widest entry. With one column it is all of `width`.
pub fn packed_width(entries: &[Entry], width: usize, max_cols: usize) -> usize {
    let cols = columns(entries, width, max_cols);
    if cols < 2 {
        return width;
    }
    (cols * Slots::of(entries).entry() + (cols - 1) * GAP).min(width)
}

pub fn legend(
    kit: &Kit<'_>,
    entries: &[Entry],
    width: usize,
    max_cols: usize,
) -> Vec<Vec<Span<'static>>> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut slots = Slots::of(entries);
    let cols = columns(entries, width, max_cols);
    let col_width = if cols > 1 {
        (width - (cols - 1) * GAP) / cols
    } else {
        width
    };
    // One column too narrow for the longest name: names are cut with `…` in a shorter slot.
    if slots.entry() > col_width {
        let rest = slots.entry() - slots.name;
        slots.name = col_width.saturating_sub(rest).max(1);
    }
    entries
        .chunks(cols)
        .map(|row| {
            let mut line = Vec::new();
            for (k, entry) in row.iter().enumerate() {
                let mut cell = draw_entry(kit, entry, &slots);
                if k + 1 < row.len() {
                    cell = fit(cell, col_width);
                    cell.push(Span::raw(" ".repeat(GAP)));
                }
                line.extend(cell);
            }
            line
        })
        .collect()
}

fn draw_entry(kit: &Kit<'_>, entry: &Entry, slots: &Slots) -> Vec<Span<'static>> {
    let name = truncate_spans(&[Span::raw(entry.name.clone())], slots.name);
    let name: String = name.iter().map(|s| s.content.as_ref()).collect();
    // A share wider than its slot borrows from the amount's, so the row keeps its width.
    let share_cells = str_width(&entry.share).max(slots.share);
    let amount_cells = (slots.amount + slots.share).saturating_sub(share_cells);
    vec![
        Span::styled(kit.glyphs.swatch, kit.fg(entry.role)),
        Span::raw(format!(" {} ", ljust(&name, slots.name))),
        kit.strong(rjust(&entry.amount, amount_cells)),
        Span::raw(" "),
        kit.dim(rjust(&entry.share, share_cells)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::spans_width;
    use crate::theme::{ColorLevel, Theme};

    fn entry(name: &str, amount: &str, share: &str) -> Entry {
        Entry {
            role: Role::CatTools,
            name: name.to_owned(),
            amount: amount.to_owned(),
            share: share.to_owned(),
        }
    }

    fn text(rows: &[Vec<Span<'_>>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn a_packed_legend_takes_only_the_width_its_columns_need() {
        let entries = [
            entry("passed", "7", "78%"),
            entry("running", "1", "11%"),
            entry("failed", "1", "11%"),
        ];
        // The swatch and a space, the widest name, then the amount's and the share's slots.
        let one = 2 + "running".len() + 1 + AMOUNT_CELLS + 1 + SHARE_CELLS;
        assert_eq!(packed_width(&entries, 200, 3), 3 * one + 2 * GAP);
        assert_eq!(packed_width(&entries, 50, 3), 2 * one + GAP);
        // One column takes the whole width, as the legend always does.
        assert_eq!(packed_width(&entries, 30, 3), 30);
    }

    #[test]
    fn columns_come_from_the_widest_entry_not_the_values() {
        let small = [entry("tools", "12k", "1%"), entry("free", "788k", "")];
        let tiny = [entry("tools", "500", "<0.1%"), entry("free", "1M", "")];
        for width in [20, 40, 60, 90] {
            assert_eq!(columns(&small, width, 3), columns(&tiny, width, 3));
        }
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let a = text(&legend(&kit, &small, 60, 3));
        let b = text(&legend(&kit, &tiny, 60, 3));
        // A `<0.1%` takes the space before its slot, so the row keeps its columns.
        assert_eq!(a, ["▌ tools   12k   1%   ▌ free   788k     "]);
        assert_eq!(b, ["▌ tools  500 <0.1%   ▌ free     1M     "]);
    }

    #[test]
    fn long_names_are_cut_in_their_slot() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let rows = legend(
            &kit,
            &[entry("修复导出测试中的竞态条件", "3.4k", "0.3%")],
            20,
            3,
        );
        assert_eq!(spans_width(&rows[0]), 20);
        assert!(text(&rows)[0].contains('…'));
    }
}
