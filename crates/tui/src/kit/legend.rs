//! A bar in words: a `▌` swatch, a name, a bold amount, and a dim share, up to three entries
//! to a row. Each column of entries is as wide as its longest name, amount and share, with 3
//! blanks before the amount, 2 before the share and 6 between two entries, so an entry reads
//! as one and its numbers never touch the next. An entry with a share and no amount has its
//! share 3 blanks after its name, as any first number. An entry for a CLI has its logo between
//! the swatch and its name.
//!
//! A legend of many parts measured several ways is a table instead ([`table_row`]): a row for
//! each part, what stands for it before its name, and a column for each measure with the
//! amount and its share, every column right-aligned in the same place whatever the values.

use argonauts_core::logos::Logo;
use ratatui::text::Span;

use super::logo::logo_before;
use super::{Kit, fit, ljust, rjust};
use crate::format::Figures;
use crate::text::{At, cut, cut_spans, spans_width, str_width};
use crate::theme::{Role, Theme};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub role: Role,
    /// The logo of the CLI the entry stands for, drawn before its name.
    pub logo: Option<Logo>,
    pub name: String,
    /// The amount as it reads, such as `3.4k` or `–` before a reading.
    pub amount: String,
    /// Its share of the whole, such as `0.3%`, or empty.
    pub share: String,
}

impl Entry {
    pub fn new(role: Role, name: impl Into<String>, amount: impl Into<String>) -> Self {
        Self {
            role,
            logo: None,
            name: name.into(),
            amount: amount.into(),
            share: String::new(),
        }
    }

    pub fn shared(mut self, share: impl Into<String>) -> Self {
        self.share = share.into();
        self
    }

    pub fn with_logo(mut self, logo: Logo) -> Self {
        self.logo = Some(logo);
        self
    }

    /// The logo's cells and the blank after them where its picture draws, else nothing.
    fn logo_cells(&self, theme: &Theme) -> Vec<Span<'static>> {
        self.logo
            .as_ref()
            .map(|logo| logo_before(logo, theme))
            .unwrap_or_default()
    }

    /// Cells of the logo and the name.
    fn name_width(&self, theme: &Theme) -> usize {
        spans_width(&self.logo_cells(theme)) + str_width(&self.name)
    }
}

/// Blanks between two columns of words, such as a name and the CLI it ran on.
pub const BETWEEN_WORDS: usize = 4;
/// Blanks before a column of numbers or bars, counted from the longest entry before it.
pub const BEFORE_NUMBERS: usize = 3;
/// Blanks between an amount and its share.
pub const BEFORE_SHARE: usize = 2;
/// The share's fewest cells, right-aligned: `<1%` and `100%` fit.
pub const SHARE_CELLS: usize = 4;
/// Blanks before an entry's amount, as before any column of numbers.
const BEFORE_AMOUNT: usize = BEFORE_NUMBERS;
/// Blanks between two entries of a row: more than inside an entry, so each reads as one.
const BETWEEN_ENTRIES: usize = 6;
/// The swatch's cell and the blank after it.
const MARK: usize = 2;

/// The cells an amount and a share are counted at, at least, when choosing how many entries
/// share a row, so a reading changing never moves an entry to another row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counted {
    pub amount: usize,
    pub share: usize,
}

impl Counted {
    /// `3.4k` to `48.2k`, and `0.4%` to `100%`.
    pub const SHORT: Self = Self {
        amount: 5,
        share: SHARE_CELLS,
    };
    /// The widest a count of tokens reads, `99.9k`, and the widest share, `<0.1%`.
    pub const WIDEST: Self = Self {
        amount: 5,
        share: 5,
    };
}

/// The cells of one column of a legend's entries.
#[derive(Debug, Clone, Copy, Default)]
struct Slots {
    name: usize,
    amount: usize,
    share: usize,
}

impl Slots {
    /// As wide as the widest of `entries` in each part. A column with no shares keeps no
    /// room for one.
    fn of<'e>(theme: &Theme, entries: impl Iterator<Item = &'e Entry>) -> Self {
        entries.fold(Self::default(), |slots, entry| Self {
            name: slots.name.max(entry.name_width(theme)),
            amount: slots.amount.max(str_width(&entry.amount)),
            share: match str_width(&entry.share) {
                0 => slots.share,
                share => slots.share.max(share).max(SHARE_CELLS),
            },
        })
    }

    fn entry(&self) -> usize {
        let room = |cells: usize, before: usize| if cells > 0 { before + cells } else { 0 };
        MARK + self.name + room(self.amount, BEFORE_AMOUNT) + room(self.share, self.before_share())
    }

    /// Blanks before the share: an amount's share's, or a first number's where there's no
    /// amount.
    fn before_share(&self) -> usize {
        if self.amount > 0 {
            BEFORE_SHARE
        } else {
            BEFORE_AMOUNT
        }
    }
}

/// Each column's cells where `entries` run `cols` to a row.
fn slots(theme: &Theme, entries: &[Entry], cols: usize) -> Vec<Slots> {
    (0..cols.min(entries.len()))
        .map(|col| Slots::of(theme, entries.iter().skip(col).step_by(cols)))
        .collect()
}

/// The cells a row of entries in `slots` takes.
fn row_cells(slots: &[Slots]) -> usize {
    let entries: usize = slots.iter().map(Slots::entry).sum();
    entries + BETWEEN_ENTRIES * slots.len().saturating_sub(1)
}

/// Each column's cells as they count toward the row's room: every amount and share at least
/// as `counted` says, so how many entries share a row comes from the names and the width,
/// never from the values. A column of entries that never write an amount, only a share,
/// keeps no room for one.
fn counted(theme: &Theme, entries: &[Entry], cols: usize, counted: Counted) -> Vec<Slots> {
    let at_least = |slots: Slots| Slots {
        amount: match slots.amount {
            0 => 0,
            amount => amount.max(counted.amount),
        },
        share: slots.share.max(counted.share),
        ..slots
    };
    slots(theme, entries, cols)
        .into_iter()
        .map(at_least)
        .collect()
}

/// How many columns `entries` take in `width` cells, at most `max_cols`.
fn columns(theme: &Theme, entries: &[Entry], width: usize, max_cols: usize, at: Counted) -> usize {
    (2..=max_cols.max(1))
        .rev()
        .find(|cols| row_cells(&counted(theme, entries, *cols, at)) <= width)
        .unwrap_or(1)
}

/// The cells `entries` take packed to the left: as many columns as `width` holds, at most
/// `max_cols`, each counted as [`Counted::SHORT`] counts it. With one column it is all of
/// `width`.
pub fn packed_width(kit: &Kit<'_>, entries: &[Entry], width: usize, max_cols: usize) -> usize {
    let cols = columns(kit.theme, entries, width, max_cols, Counted::SHORT);
    if cols < 2 {
        return width;
    }
    row_cells(&counted(kit.theme, entries, cols, Counted::SHORT)).min(width)
}

/// The fewest cells `entries` take in at most `rows` rows, each counted as [`Counted::SHORT`]
/// counts it: a [`legend`] that wide or wider takes no more rows.
pub fn width_in_rows(kit: &Kit<'_>, entries: &[Entry], rows: usize) -> usize {
    let cols = entries.len().div_ceil(rows.max(1)).max(1);
    row_cells(&counted(kit.theme, entries, cols, Counted::SHORT))
}

/// The legend's rows in `width` cells, at most `max_cols` entries to a row, as many as fit
/// counted as [`Counted::SHORT`] counts them.
pub fn legend(
    kit: &Kit<'_>,
    entries: &[Entry],
    width: usize,
    max_cols: usize,
) -> Vec<Vec<Span<'static>>> {
    legend_counted(kit, entries, width, max_cols, Counted::SHORT)
}

/// [`legend`], with the entries a row chosen with every amount and share counted at least
/// as `at` says.
pub fn legend_counted(
    kit: &Kit<'_>,
    entries: &[Entry],
    width: usize,
    max_cols: usize,
    at: Counted,
) -> Vec<Vec<Span<'static>>> {
    if entries.is_empty() {
        return Vec::new();
    }
    let cols = columns(kit.theme, entries, width, max_cols, at);
    let mut slots = slots(kit.theme, entries, cols);
    // One column too narrow for the longest name: names are cut with `…` in a shorter slot.
    if let [only] = slots.as_mut_slice() {
        let over = only.entry().saturating_sub(width);
        only.name = only.name.saturating_sub(over).max(1);
    }
    entries
        .chunks(cols)
        .map(|row| {
            let mut line = Vec::new();
            for (k, entry) in row.iter().enumerate() {
                if k > 0 {
                    line.push(Span::raw(" ".repeat(BETWEEN_ENTRIES)));
                }
                line.extend(draw_entry(kit, entry, &slots[k]));
            }
            line
        })
        .collect()
}

fn draw_entry(kit: &Kit<'_>, entry: &Entry, slots: &Slots) -> Vec<Span<'static>> {
    let logo = entry.logo_cells(kit.theme);
    let room = slots.name.saturating_sub(spans_width(&logo));
    let name = cut(&entry.name, room, At::Word);
    let mut cells = vec![
        Span::styled(kit.glyphs.swatch, kit.fg(entry.role)),
        Span::raw(" "),
    ];
    cells.extend(logo);
    cells.push(Span::raw(ljust(&name, room)));
    if slots.amount > 0 {
        cells.push(Span::raw(" ".repeat(BEFORE_AMOUNT)));
        cells.push(kit.strong(rjust(&entry.amount, slots.amount)));
    }
    if slots.share > 0 {
        cells.push(Span::raw(" ".repeat(slots.before_share())));
        cells.push(kit.dim(rjust(&entry.share, slots.share)));
    }
    cells
}

/// Cells before a table's rows for the pointer, `❯ ` on the row the keys are on.
pub const POINTER: usize = 2;
/// The cells before a name in a table without logos: a swatch two cells wide and two blanks.
pub const SWATCH: usize = 4;
/// The fewest cells of a measure's amount in a table, right-aligned: `~$1,204.50` fits.
pub const TABLE_AMOUNT: usize = 10;
/// The fewest blanks before an amount, after the column on its left.
const TABLE_GAP: usize = BEFORE_NUMBERS;

/// A part's share of a whole in whole percents, as a table writes it: `26%`, `<1%` for a part
/// too small to round to one, and `0%` for nothing.
pub fn share(part: u64, whole: u64) -> String {
    Figures::Table.share(part, whole)
}

/// One measure of a table's row: the amount as it reads, such as `2.60M`, and its share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Figure {
    pub amount: String,
    pub share: String,
}

/// A row of a table: what stands for its part before the name, such as a logo and a swatch,
/// the name, and a figure for each measure. A row with no emblem, such as one that holds
/// the sums of the rows out of view, writes its words from where the emblem would start.
#[derive(Debug, Clone, Default)]
pub struct TableRow {
    pub emblem: Vec<Span<'static>>,
    pub name: Vec<Span<'static>>,
    pub figures: Vec<Figure>,
    pub pointed: bool,
}

/// Where a table's columns fall: the pointer's, the emblems', the names', and a column for each
/// of its measures, each its amount, two blanks and its share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    /// Cells before the rows for the pointer: [`POINTER`], or none in a table the keys never
    /// go down.
    pub pointer: usize,
    pub emblem: usize,
    pub name: usize,
    pub measures: usize,
    /// Cells of each amount: [`TABLE_AMOUNT`], or the widest amount of any row and the blanks
    /// before it where that's wider, so every row's numbers stay in the same columns.
    pub amount: usize,
}

impl Columns {
    /// The narrowest names column a table is drawn with: a longer name is cut with `…`.
    pub const LEAST_NAME: usize = 16;

    /// The columns of a table `width` cells wide whose rows start with `emblem` cells and whose
    /// widest amount is `widest` cells, the names taking what the measures leave.
    pub fn new(width: usize, emblem: usize, measures: usize, widest: usize) -> Self {
        let amount = (widest + TABLE_GAP).max(TABLE_AMOUNT);
        Self {
            pointer: POINTER,
            emblem,
            name: width.saturating_sub(POINTER + emblem + measures * measure(amount)),
            measures,
            amount,
        }
    }

    /// [`Columns::new`] with as many of the `measures` as leave the names
    /// [`Columns::LEAST_NAME`] cells, the last ones given up first, and always the first.
    pub fn fitting(width: usize, emblem: usize, measures: usize, widest: usize) -> Self {
        (1..=measures)
            .rev()
            .map(|shown| Self::new(width, emblem, shown, widest))
            .find(|columns| columns.name >= Self::LEAST_NAME)
            .unwrap_or_else(|| Self::new(width, emblem, 1, widest))
    }

    /// [`Columns::new`] with the names only as wide as the longest of them, `longest` cells and
    /// at least [`Columns::LEAST_NAME`], so a table of a measure or two keeps its numbers by
    /// its names and leaves the rest of the row blank.
    pub fn packed(
        width: usize,
        emblem: usize,
        measures: usize,
        widest: usize,
        longest: usize,
    ) -> Self {
        let mut columns = Self::new(width, emblem, measures, widest);
        columns.name = columns.name.min(longest.max(Self::LEAST_NAME));
        columns
    }

    /// The columns with no cells for a pointer, for a table the keys never go down.
    pub fn unpointed(self) -> Self {
        Self { pointer: 0, ..self }
    }

    /// The fewest cells a table of `measures` whose rows start with `emblem` cells takes.
    pub fn least(emblem: usize, measures: usize) -> usize {
        POINTER + emblem + Self::LEAST_NAME + measures * measure(TABLE_AMOUNT)
    }
}

/// A measure's column: its amount, two blanks, and its share.
fn measure(amount: usize) -> usize {
    amount + BEFORE_SHARE + SHARE_CELLS
}

/// Which column a table's rows are sorted by, marked `▾` in the cell before its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sorted {
    Name,
    Measure(usize),
}

/// A table's titles: `title` over the names and each of `measures` over its amounts, with `▾`
/// in the cell before the one the rows are sorted by.
pub fn table_head(
    columns: Columns,
    title: &str,
    measures: &[&str],
    sorted: Option<Sorted>,
) -> Vec<Span<'static>> {
    let mark = |on: bool| if on { "▾" } else { " " };
    // The mark before the names' title takes the emblem's last cell, a blank in every row.
    let (before, name) = match columns.emblem {
        0 => (columns.pointer, ljust(title, columns.name)),
        emblem => {
            let marked = format!("{}{title}", mark(sorted == Some(Sorted::Name)));
            (
                columns.pointer + emblem - 1,
                ljust(&marked, columns.name + 1),
            )
        }
    };
    let mut row = vec![Span::raw(" ".repeat(before)), Span::raw(name)];
    for (at, measure) in measures.iter().take(columns.measures).enumerate() {
        let marked = format!("{}{measure}", mark(sorted == Some(Sorted::Measure(at))));
        row.push(Span::raw(rjust(&marked, columns.amount)));
        row.push(Span::raw(" ".repeat(BEFORE_SHARE + SHARE_CELLS)));
    }
    row
}

/// What stands before a name in a table without logos, [`SWATCH`] cells: two cells of `role`'s
/// color, the one its bar fills with, and two blanks.
pub fn swatch(kit: &Kit<'_>, role: Role) -> Vec<Span<'static>> {
    let swatch = Span::styled(kit.glyphs.used.repeat(2), kit.fg(role));
    vec![swatch, Span::raw("  ")]
}

/// One row of a table in its columns: the pointer where the keys are on it, the emblem, the name
/// cut after a word with `…` where it's too long, and each figure's amount and share
/// right-aligned. A row whose figures are all empty, such as one for rows out of view that
/// hold nothing, leaves their columns blank.
pub fn table_row(kit: &Kit<'_>, columns: Columns, row: &TableRow) -> Vec<Span<'static>> {
    let mut out = vec![if row.pointed && columns.pointer > 0 {
        kit.span(format!("{} ", kit.theme.glyphs.prompt), Role::Question)
    } else {
        Span::raw(" ".repeat(columns.pointer))
    }];
    let words = if row.emblem.is_empty() {
        columns.emblem + columns.name
    } else {
        out.extend(fit(row.emblem.clone(), columns.emblem));
        columns.name
    };
    let name = cut_spans(&row.name, words, At::Word);
    let pad = words.saturating_sub(spans_width(&name));
    out.extend(name);
    out.push(Span::raw(" ".repeat(pad)));
    for figure in row.figures.iter().take(columns.measures) {
        out.push(Span::raw(rjust(&figure.amount, columns.amount)));
        out.push(Span::raw(" ".repeat(BEFORE_SHARE)));
        out.push(Span::raw(rjust(&figure.share, SHARE_CELLS)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::spans_width;
    use crate::theme::{ColorLevel, Theme};

    fn entry(name: &str, amount: &str, share: &str) -> Entry {
        Entry::new(Role::CatTools, name, amount).shared(share)
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
        // The swatch and its blank, the name, 3 blanks and the amount counted at its fewest
        // cells, 2 and the share.
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let one = |name: &str| 2 + name.len() + 3 + Counted::SHORT.amount + 2 + SHARE_CELLS;
        let three = one("passed") + one("running") + one("failed") + 2 * BETWEEN_ENTRIES;
        assert_eq!(packed_width(&kit, &entries, 200, 3), three);
        // Two to a row: "failed" goes under "passed", whose column it fits.
        let two = one("passed") + one("running") + BETWEEN_ENTRIES;
        assert_eq!(packed_width(&kit, &entries, 60, 3), two);
        // One column takes the whole width, as the legend always does.
        assert_eq!(packed_width(&kit, &entries, 30, 3), 30);
    }

    #[test]
    fn a_share_with_no_amount_stands_three_blanks_after_its_name() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let entries = [
            Entry::new(Role::CatTools, "Cache read", "").shared("90%"),
            Entry::new(Role::CatTools, "Write", "").shared("5%"),
        ];
        assert_eq!(
            text(&legend(&kit, &entries, 110, 4)),
            ["▌ Cache read    90%      ▌ Write     5%"]
        );
        // No amount is counted where none is written, so both stay on one row.
        let one = |name: &str| 2 + name.len() + 3 + SHARE_CELLS;
        let both = one("Cache read") + BETWEEN_ENTRIES + one("Write");
        assert_eq!(packed_width(&kit, &entries, both, 4), both);
    }

    #[test]
    fn a_legend_as_wide_as_its_rows_need_takes_no_more_rows() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let entries = [
            entry("from cache", "950k", "68%"),
            entry("new input", "380k", "27%"),
            entry("output", "70k", "5%"),
            entry("written", "2k", "<1%"),
        ];
        for rows in 1..=4 {
            let width = width_in_rows(&kit, &entries, rows);
            assert!(
                legend(&kit, &entries, width, 4).len() <= rows,
                "{rows} rows"
            );
            assert!(legend(&kit, &entries, width - 1, 4).len() > rows || rows == 4);
        }
    }

    #[test]
    fn entries_keep_three_blanks_before_an_amount_two_before_a_share_and_six_between() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let entries = [
            entry("from cache", "950k", "68%"),
            entry("new input", "380k", "27%"),
            entry("output", "70k", "5%"),
        ];
        assert_eq!(
            text(&legend(&kit, &entries, 110, 3)),
            ["▌ from cache   950k   68%      ▌ new input   380k   27%      ▌ output   70k    5%"]
        );
        // Two to a row, each column as wide as the longest of its entries.
        assert_eq!(
            text(&legend(&kit, &entries, 57, 3)),
            [
                "▌ from cache   950k   68%      ▌ new input   380k   27%",
                "▌ output        70k    5%",
            ]
        );
    }

    #[test]
    fn a_share_wider_than_its_slot_widens_its_column() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let entries = [
            entry("System prompt", "4.4k", "0.4%"),
            entry("Messages", "10", "<0.1%"),
            entry("free", "788k", ""),
        ];
        assert_eq!(
            text(&legend(&kit, &entries, 40, 1)),
            [
                "▌ System prompt   4.4k   0.4%",
                "▌ Messages          10  <0.1%",
                "▌ free            788k       ",
            ]
        );
    }

    #[test]
    fn a_narrow_table_gives_up_its_last_measures_before_its_names() {
        let wide = Columns::fitting(82, 7, 3, 6);
        assert_eq!((wide.name, wide.measures), (25, 3));
        let narrow = Columns::fitting(58, 7, 3, 6);
        assert_eq!((narrow.name, narrow.measures), (17, 2));
        assert_eq!(Columns::fitting(20, 7, 3, 6).measures, 1);
    }

    #[test]
    fn a_table_writes_every_row_in_the_same_columns() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let columns = Columns::new(60, 4, 2, 6);
        let row = |name: &str, amounts: [&str; 2], pointed: bool| TableRow {
            emblem: vec![Span::raw("██")],
            name: vec![Span::raw(name.to_owned())],
            figures: amounts
                .iter()
                .map(|amount| Figure {
                    amount: (*amount).to_owned(),
                    share: share(1, 3),
                })
                .collect(),
            pointed,
        };
        let rows = [
            table_head(
                columns,
                "model",
                &["tokens", "spend"],
                Some(Sorted::Measure(1)),
            ),
            table_row(&kit, columns, &row("Opus 5.5", ["2.60M", "~$7.10"], false)),
            table_row(
                &kit,
                columns,
                &row(
                    "a name far too long for its column",
                    ["0.07M", "$0.00"],
                    true,
                ),
            ),
        ];
        let text = text(&rows);
        assert_eq!(
            text[0],
            "      model                     tokens          ▾spend      "
        );
        assert_eq!(
            text[1],
            "  ██  Opus 5.5                   2.60M   33%    ~$7.10   33%"
        );
        // Cut one cell short, so a `…` drawn two cells wide keeps its blank.
        assert_eq!(
            text[2],
            "❯ ██  a name far too long…       0.07M   33%     $0.00   33%"
        );
        assert!(rows.iter().all(|row| spans_width(row) == 60));
    }

    #[test]
    fn a_long_name_in_a_table_stops_after_a_word() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let row = TableRow {
            emblem: vec![Span::raw("██")],
            name: vec![Span::raw("Nemotron 3.5 Super VL Free".to_owned())],
            figures: vec![Figure {
                amount: "1.2M".to_owned(),
                share: share(1, 3),
            }],
            pointed: false,
        };
        let drawn = table_row(&kit, Columns::new(38, 4, 1, 6), &row);
        assert_eq!(text(&[drawn]), ["  ██  Nemotron 3.5…         1.2M   33%"]);
    }

    #[test]
    fn a_share_too_small_to_round_to_one_still_shows() {
        assert_eq!(share(1, 0), "0%");
        assert_eq!(share(0, 10), "0%");
        assert_eq!(share(1, 1_000), "<1%");
        assert_eq!(share(26, 100), "26%");
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
