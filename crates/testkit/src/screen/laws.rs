//! The laws every drawn screen keeps, whichever panel drew it:
//!
//! 1. **The selected row is drawn:** while a list has a selection, the pointer shows on exactly
//!    one row, the selected one.
//! 2. **Columns keep their room,** counted as Ghostty with Iosevka paints the cells: two columns
//!    of words stand 4 blanks apart; a column of numbers or of bars has 3 before it, an amount's
//!    share 2; the entries of a legend that runs across a row stand 6 apart, on a row of its own
//!    too, and a CLI's dot or logo starts no legend's entry. A mark keeps to its name, never
//!    touching it nor standing off from it: a logo, a dot or a one-cell swatch has 1 blank
//!    before the name, a table's two-cell swatch 2. A badge, a word on a background of its
//!    own, is a mark too. A run of marks, as a typing bubble's `• • •`, and a mark in the
//!    gutter beside text that goes on from the next row name no word. A swatch's glyph alone in
//!    a column of bars is a short bar. Figures written in a sentence, and a label's count
//!    right-aligned after it, make no column of their own.
//! 3. **No two models look alike:** two models' swatches never share both glyph and colour. A
//!    CLI's models share its hue on their dots and names, and the models past a CLI's first
//!    few share the solid grey of the rest, so dots and greys may repeat.
//! 4. **Nothing is cut while there's room:** a name cut with `…` has, on its row, fewer free
//!    cells than the rest of it needs.
//! 5. **A list that hides rows says so:** when fewer of a list's rows show than it holds, the
//!    panel says how many are hidden, as `↓ N more` (with `↑ N more` above, counted together),
//!    or which of how many it shows, as `1–10 of 17`.
//!
//! A screen says what the laws need to know through a [`Look`]: where the panel is, what's
//! selected, which names are models, which texts may be cut, and which rows a list holds.

use std::fmt;

use super::colour::Colour;
use super::grid::{Area, Grid, PLACEHOLDER, text_width};
use super::tokens::{Kind, Token, is_figure, tokens};

/// The pointer a list draws before its selected row.
pub const POINTER: &str = "❯";

/// Room between columns, in blanks.
pub const WORDS: u16 = 4;
pub const NUMBERS: u16 = 3;
pub const BARS: u16 = 3;
pub const SHARE: u16 = 2;
pub const LEGEND_ENTRIES: u16 = 6;
/// After a logo's two cells, a dot, or a legend's one-cell swatch, before its name.
pub const MARK: u16 = 1;
/// After a table's two-cell swatch, before its name.
pub const SWATCH: u16 = 2;
/// After a run's or a step's one-cell state, before its name: `⣾  Rate limit the API`.
pub const STATE: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Law {
    SelectedRowDrawn,
    ColumnsKeepTheirRoom,
    ModelsLookApart,
    NothingCutWithRoom,
    HiddenRowsCounted,
}

impl fmt::Display for Law {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Law::SelectedRowDrawn => "the selected row is drawn",
            Law::ColumnsKeepTheirRoom => "columns keep their room",
            Law::ModelsLookApart => "no two models look alike",
            Law::NothingCutWithRoom => "nothing is cut while there's room",
            Law::HiddenRowsCounted => "a list that hides rows says so",
        })
    }
}

/// Where and how a screen breaks a law. Rows and columns count from 0 at the grid's top left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breach {
    pub law: Law,
    pub row: u16,
    pub col: u16,
    pub what: String,
}

impl fmt::Display for Breach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (row {}, column {}): {}",
            self.law, self.row, self.col, self.what
        )
    }
}

/// What the laws need to know about a screen beyond its cells.
#[derive(Debug, Clone, Default)]
pub struct Look {
    area: Option<Area>,
    selected: Option<Selected>,
    models: Vec<String>,
    whole: Vec<String>,
    list: Vec<String>,
}

#[derive(Debug, Clone)]
enum Selected {
    /// Some row is selected.
    Any,
    /// The row showing this text is.
    Row(String),
}

impl Look {
    pub fn new() -> Look {
        Look::default()
    }

    /// The panel's cells, when it doesn't fill the grid.
    pub fn within(mut self, area: Area) -> Look {
        self.area = Some(area);
        self
    }

    /// A list on the screen has a selection.
    pub fn selection(mut self) -> Look {
        self.selected = Some(Selected::Any);
        self
    }

    /// The selected row is the one showing `text`.
    pub fn selected(mut self, text: &str) -> Look {
        self.selected = Some(Selected::Row(text.to_owned()));
        self
    }

    /// The names the screen gives its models, as it writes them.
    pub fn models<S: AsRef<str>>(mut self, names: &[S]) -> Look {
        self.models
            .extend(names.iter().map(|name| name.as_ref().to_owned()));
        self
    }

    /// Texts the screen should show whole wherever its row has room: names, mostly.
    pub fn whole<S: AsRef<str>>(mut self, texts: &[S]) -> Look {
        self.whole
            .extend(texts.iter().map(|text| text.as_ref().to_owned()));
        self
    }

    /// Every row a list on the screen holds, as the list writes it, shown or not.
    pub fn list<S: AsRef<str>>(mut self, rows: &[S]) -> Look {
        self.list
            .extend(rows.iter().map(|row| row.as_ref().to_owned()));
        self
    }

    pub fn area(&self, grid: &Grid) -> Area {
        self.area.unwrap_or_else(|| grid.area())
    }
}

/// Every law's breaches on the screen, empty when it keeps them all.
pub fn check(grid: &Grid, look: &Look) -> Vec<Breach> {
    let mut breaches = selected_row(grid, look);
    breaches.extend(columns(grid, look));
    breaches.extend(models(grid, look));
    breaches.extend(cut(grid, look));
    breaches.extend(hidden_rows(grid, look));
    breaches
}

/// Fails with every breach and the screen, saying which screen it is.
#[track_caller]
pub fn assert_kept(grid: &Grid, look: &Look, screen: &str) {
    let breaches = check(grid, look);
    assert!(
        breaches.is_empty(),
        "{screen} breaks the screen laws:\n{}\n\n{}",
        breaches
            .iter()
            .map(Breach::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        grid.as_text(look.area(grid))
    );
}

/// Law 1: while something is selected, the pointer shows on exactly one row, the one showing
/// the selected text when the look names it.
pub fn selected_row(grid: &Grid, look: &Look) -> Vec<Breach> {
    let Some(selected) = &look.selected else {
        return Vec::new();
    };
    let area = look.area(grid);
    let pointed: Vec<(u16, u16)> = (area.y..area.bottom())
        .flat_map(|row| {
            grid.find_in_row(area, row, POINTER)
                .into_iter()
                .map(move |col| (row, col))
        })
        .collect();
    let breach = |row, col, what: String| Breach {
        law: Law::SelectedRowDrawn,
        row,
        col,
        what,
    };
    match (pointed.as_slice(), selected) {
        ([], Selected::Any) => vec![breach(area.y, area.x, "no row shows the pointer".into())],
        ([], Selected::Row(text)) => vec![breach(
            area.y,
            area.x,
            format!("no row shows the pointer, so “{text}” isn't drawn as selected"),
        )],
        ([(row, col)], Selected::Row(text)) => {
            let shown = grid.row_text(area, *row);
            if shown.contains(text.as_str()) {
                Vec::new()
            } else {
                vec![breach(
                    *row,
                    *col,
                    format!("the pointer is on “{}”, not on “{text}”", shown.trim()),
                )]
            }
        }
        ([_], Selected::Any) => Vec::new(),
        (many, _) => many
            .iter()
            .skip(1)
            .map(|&(row, col)| breach(row, col, format!("{} rows show the pointer", many.len())))
            .collect(),
    }
}

/// Law 2: the blanks before each entry of a column, and between each mark and its name.
pub fn columns(grid: &Grid, look: &Look) -> Vec<Breach> {
    let area = look.area(grid);
    let mut breaches = lined_up(grid, area);
    for breach in marks(grid, area) {
        let known = breaches
            .iter()
            .any(|seen| (seen.row, seen.col) == (breach.row, breach.col));
        if !known {
            breaches.push(breach);
        }
    }
    breaches
}

/// An entry belongs to a column when another row of its block (the rows between two blank
/// ones or rules) has an entry of the same kind starting at its first cell or, for figures,
/// ending at its last. Room is counted after words and figures here; [`marks`] counts it after
/// a mark.
fn lined_up(grid: &Grid, area: Area) -> Vec<Breach> {
    let mut breaches = Vec::new();
    for block in blocks(grid, area) {
        let mut entries: Vec<Entry> = block.iter().flat_map(|row| entries(row)).collect();
        // A swatch's glyph that names nothing, in a column where another row draws a bar, is a
        // short bar in a model's pattern, as a day's small spend is.
        let bars: Vec<u16> = entries
            .iter()
            .filter(|entry| entry.column == Column::Bars)
            .map(|entry| entry.token.start)
            .collect();
        for entry in &mut entries {
            if entry.column == Column::Marks && !entry.names && bars.contains(&entry.token.start) {
                entry.column = Column::Bars;
            }
        }
        for entry in &entries {
            let lined_up: Vec<&Entry> = entries
                .iter()
                .filter(|other| {
                    other.token.row != entry.token.row
                        && other.column.lines_up_with(entry.column)
                        && (other.token.start == entry.token.start
                            || (entry.column.is_figures() && other.token.end == entry.token.end))
                })
                .collect();
            let need = entry.need();
            if !lined_up.is_empty() && entry.gap < need && !entry.repeats_its_label(&lined_up) {
                breaches.push(Breach {
                    law: Law::ColumnsKeepTheirRoom,
                    row: entry.token.row,
                    col: entry.token.start,
                    what: format!(
                        "“{}” has {} blank{} after “{}”, and {} takes {need}",
                        written(&entry.token.text),
                        entry.gap,
                        if entry.gap == 1 { "" } else { "s" },
                        written(&entry.before.text),
                        entry.what(),
                    ),
                });
            }
        }
    }
    breaches
}

/// The marks that keep to a name, row by row: a swatch, a dot or a logo has just its room
/// before the name, a run's or a step's state as much as [`STATE`], and a legend's next entry,
/// a swatch after the figures of the entry before, stands [`LEGEND_ENTRIES`] after them
/// whether or not another row lines up with it. A CLI's mark, its dot or its logo, starts no
/// legend's entry: the bottom line's chips (the mark, the CLI's name or letters, its share and
/// maybe a meter) stand closer. Neither do entries that hold a meter.
fn marks(grid: &Grid, area: Area) -> Vec<Breach> {
    let mut breaches = Vec::new();
    let rows: Vec<Vec<Token>> = (area.y..area.bottom())
        .map(|row| tokens(grid, area, row))
        .collect();
    for (index, row) in rows.iter().enumerate() {
        // Whether a mark has started an entry on the row, so a mark after a figure starts the
        // next one, and whether that entry holds a meter.
        let mut in_legend = false;
        let mut metered = false;
        for (at, token) in row.iter().enumerate() {
            let Some(mark) = Mark::leading(grid, area, token) else {
                let cells = token.end - token.start;
                metered |= token.kind == Kind::Mark && (is_bar(&token.text) || cells > 2);
                continue;
            };
            if in_a_run(row, at) {
                continue;
            }
            let name = match &mark.touches {
                Some(name) => Some((name.clone(), 0, token.start + mark.cells)),
                None => row
                    .get(at + 1)
                    .filter(|next| next.kind == Kind::Word)
                    .filter(|next| !(at == 0 && in_a_gutter(&rows, index, next.start)))
                    .map(|next| (next.text.clone(), token.gap_to(next), next.start)),
            };
            let Some((name, gap, col)) = name else {
                continue;
            };
            let breach = |col, what| Breach {
                law: Law::ColumnsKeepTheirRoom,
                row: token.row,
                col,
                what,
            };
            // A word a column's room away is the next column's, not the mark's name.
            let standing_off = gap > mark.most() && gap < WORDS;
            if gap < mark.need() || standing_off {
                breaches.push(breach(
                    col,
                    format!(
                        "“{name}” has {gap} blank{} after its {}, and {} takes {}",
                        if gap == 1 { "" } else { "s" },
                        mark.text,
                        mark.what(),
                        mark.room()
                    ),
                ));
            }
            let legend = mark.starts_a_legend_entry();
            if let Some(before) = at.checked_sub(1).and_then(|before| row.get(before))
                && legend
                && in_legend
                && !metered
                && before.kind == Kind::Figure
            {
                let gap = before.gap_to(token);
                if gap < LEGEND_ENTRIES {
                    breaches.push(breach(
                        token.start,
                        format!(
                            "“{}” has {gap} blank{} after “{}”, and a legend's next entry takes \
                             {LEGEND_ENTRIES}",
                            mark.text,
                            if gap == 1 { "" } else { "s" },
                            before.text,
                        ),
                    ));
                }
            }
            in_legend = legend;
            metered = false;
        }
    }
    breaches
}

/// A token as a breach names it: a logo's placeholders as `logo`.
fn written(text: &str) -> &str {
    if text.starts_with(PLACEHOLDER) {
        "logo"
    } else {
        text
    }
}

/// Whether the mark at `at` repeats the one a blank before it, as the dots of a typing bubble's
/// `• • •` do: a run of marks names nothing.
fn in_a_run(row: &[Token], at: usize) -> bool {
    let Some(token) = row.get(at) else {
        return false;
    };
    at.checked_sub(1)
        .and_then(|before| row.get(before))
        .is_some_and(|before| before.text == token.text && before.gap_to(token) == 1)
}

/// Whether a mark that starts its row stands in a gutter, left of text that goes on from the
/// row above or below at the same cell, as a speaker's logo stands beside the last line of its
/// message: the word after it is that text's, not the mark's name.
fn in_a_gutter(rows: &[Vec<Token>], index: usize, text_starts: u16) -> bool {
    let starts_there = |row: Option<&Vec<Token>>| {
        row.and_then(|row| row.first())
            .is_some_and(|first| first.start == text_starts)
    };
    starts_there(index.checked_sub(1).and_then(|above| rows.get(above)))
        || starts_there(rows.get(index + 1))
}

/// A mark at the start of a token that keeps to the name after it.
struct Mark {
    text: String,
    /// The cells its glyphs take, without the blank a wide-painted one covers.
    cells: u16,
    logo: bool,
    /// The name it touches, when nothing stands between them.
    touches: Option<String>,
}

impl Mark {
    /// The swatch, dot or logo `token` starts with: one cell, or two of the same glyph. A
    /// longer run of such glyphs is a bar, and two different ones are part of a drawing.
    fn leading(grid: &Grid, area: Area, token: &Token) -> Option<Mark> {
        let mut text = String::new();
        let mut glyphs = Vec::new();
        let mut cells = 0;
        let mut logo = false;
        let mut col = token.start;
        while col < area.right() {
            let cell = grid.cell(token.row, col);
            if cell.is_covered() {
                col += 1;
                continue;
            }
            let glyph = cell.text.as_str();
            let picture = cell.picture.is_some();
            if cell.is_blank() || !(picture || is_swatch(glyph)) {
                break;
            }
            logo |= picture;
            if !picture {
                text.push_str(glyph);
                glyphs.push(glyph);
            }
            cells += 1;
            col += 1;
        }
        let drawing = glyphs.windows(2).any(|pair| pair[0] != pair[1]);
        if cells == 0 || cells > 2 || drawing {
            return None;
        }
        let rest = grid.text(token.row, col, token.end.min(area.right()));
        let rest = rest.trim_end();
        let touches = match rest.chars().next() {
            None => None,
            Some(c) if c.is_alphabetic() => Some(rest.to_owned()),
            Some(_) => return None,
        };
        if logo {
            text = "logo".to_owned();
        }
        Some(Mark {
            text,
            cells,
            logo,
            touches,
        })
    }

    /// Whether the mark is a swatch, as a legend's entry starts, rather than a CLI's own.
    fn starts_a_legend_entry(&self) -> bool {
        !self.logo && !is_clis_mark(&self.text)
    }

    fn need(&self) -> u16 {
        if self.cells == 2 && !self.logo {
            SWATCH
        } else {
            MARK
        }
    }

    /// The most blanks the mark may stand before its name. A one-cell mark may be a state,
    /// which stands [`STATE`] off, and its glyph can't say so: `○` is a model's dot and a step
    /// that waits, `◆` a swatch and a run that needs you.
    fn most(&self) -> u16 {
        if self.cells == 1 && !self.logo {
            STATE
        } else {
            self.need()
        }
    }

    fn room(&self) -> String {
        if self.most() > self.need() {
            format!("{} or {}", self.need(), self.most())
        } else {
            self.need().to_string()
        }
    }

    fn what(&self) -> &'static str {
        if self.logo {
            "a logo's name"
        } else if self.cells == 2 {
            "a name after a table's swatch"
        } else {
            "a name after its mark"
        }
    }
}

/// A glyph that marks what a name is: a block, a square, a circle, a diamond, a braille cell
/// or a bullet. A triangle points or sorts, as `▾tokens` does on a column's head.
fn is_swatch(glyph: &str) -> bool {
    let mut chars = glyph.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let triangle = ('\u{25b2}'..='\u{25c5}').contains(&first);
    chars.next().is_none()
        && !triangle
        && (('\u{2580}'..='\u{25ff}').contains(&first)
            || ('\u{2800}'..='\u{28ff}').contains(&first)
            || first == '•')
}

/// What kind of column an entry starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Words,
    Figures,
    /// A figure right after another, as an amount's share is.
    Share,
    Bars,
    /// A mark starting an entry: a legend's next entry, a CLI's logo or dot.
    Marks,
}

impl Column {
    fn is_figures(self) -> bool {
        matches!(self, Column::Figures | Column::Share)
    }

    fn lines_up_with(self, other: Column) -> bool {
        self == other || (self.is_figures() && other.is_figures())
    }
}

struct Entry {
    token: Token,
    before: Token,
    gap: u16,
    column: Column,
    /// Whether a word follows it within a swatch's room, as a legend's entry names its swatch.
    names: bool,
}

impl Entry {
    /// Whether the entry is a figure every row it lines up with writes after the same word,
    /// that word's count: one blank after it, as in "resets in 2h" over "resets in 5h", or
    /// after the word set in the same place, as a list's "others   3" over "others  10",
    /// whose counts right-align after their label.
    fn repeats_its_label(&self, lined_up: &[&Entry]) -> bool {
        let labelled = |entry: &Entry| {
            entry.column.is_figures() && entry.before.text.chars().all(char::is_alphabetic)
        };
        let same_label = |other: &&Entry| {
            labelled(other)
                && other.before.text == self.before.text
                && ((self.gap == 1 && other.gap == 1) || other.before.start == self.before.start)
        };
        labelled(self) && lined_up.iter().all(same_label)
    }

    /// Whether the entry's mark is a swatch, as a legend's entry starts, rather than a CLI's
    /// own.
    fn starts_a_legend_entry(&self) -> bool {
        !is_clis_mark(&self.token.text)
    }

    fn need(&self) -> u16 {
        match (self.column, self.before.kind) {
            (Column::Words, Kind::Figure) => NUMBERS,
            (Column::Words, _) => WORDS,
            (Column::Figures, _) => NUMBERS,
            (Column::Share, _) => SHARE,
            (Column::Bars, _) => BARS,
            (Column::Marks, Kind::Figure) if !self.starts_a_legend_entry() => WORDS,
            (Column::Marks, Kind::Figure) => LEGEND_ENTRIES,
            (Column::Marks, _) => WORDS,
        }
    }

    fn what(&self) -> &'static str {
        match (self.column, self.before.kind) {
            (Column::Words, Kind::Figure) => "words after a column of numbers",
            (Column::Words, _) => "a column of words",
            (Column::Figures, _) => "a column of numbers",
            (Column::Share, _) => "a share after its amount",
            (Column::Bars, _) => "a column of bars",
            (Column::Marks, Kind::Figure) if !self.starts_a_legend_entry() => {
                "a column that starts with a mark"
            }
            (Column::Marks, Kind::Figure) => "a legend's next entry",
            (Column::Marks, _) => "a column that starts with a mark",
        }
    }
}

/// The runs of rows with something on them, split at blank rows and rules, each row as its
/// tokens. A block of one row has no columns to line up.
fn blocks(grid: &Grid, area: Area) -> Vec<Vec<Vec<Token>>> {
    let mut blocks = vec![Vec::new()];
    for row in area.y..area.bottom() {
        let row = tokens(grid, area, row);
        let parts = row.iter().any(|token| !is_frame(&token.text));
        if parts {
            if let Some(block) = blocks.last_mut() {
                block.push(row);
            }
        } else {
            blocks.push(Vec::new());
        }
    }
    blocks.retain(|block| block.len() > 1);
    blocks
}

/// The row's tokens that could start an entry of a column, each with the token before it.
fn entries(row: &[Token]) -> Vec<Entry> {
    let mut found = Vec::new();
    for (at, pair) in row.windows(2).enumerate() {
        let (before, token) = (&pair[0], &pair[1]);
        if matches!(before.kind, Kind::Mark | Kind::Badge) || in_a_sentence(row, at + 1) {
            continue;
        }
        let gap = before.gap_to(token);
        // A figure inside a phrase, as in "GPT 6 Luna", is part of the words around it; one
        // with a figure after it, as an amount before its share, ends its own.
        let ends_phrase = row
            .get(at + 2)
            .is_none_or(|next| token.gap_to(next) >= 2 || next.kind == Kind::Figure);
        let column = match token.kind {
            Kind::Figure if !ends_phrase => continue,
            Kind::Figure if before.kind == Kind::Figure && is_share(&token.text) => Column::Share,
            Kind::Figure => Column::Figures,
            Kind::Word if gap >= 2 => Column::Words,
            Kind::Mark if gap >= 2 && is_bar(&token.text) => Column::Bars,
            Kind::Mark if gap >= 2 && !is_frame(&token.text) => Column::Marks,
            _ => continue,
        };
        let names = row
            .get(at + 2)
            .is_some_and(|next| next.kind == Kind::Word && token.gap_to(next) <= SWATCH);
        found.push(Entry {
            token: token.clone(),
            before: before.clone(),
            gap,
            column,
            names,
        });
    }
    found
}

/// Whether the figure at `at` is a word of a sentence, as Claude Code's `/context` writes its
/// figures: a label's value one blank after its colon (`Free space: 934.7k`), or a share in
/// parentheses one blank after the tokens it counts or after such a value (`6.4k tokens
/// (0.6%)`, `32.3k/1m tokens (3%)`, `934.7k (93.5%)`).
fn in_a_sentence(row: &[Token], at: usize) -> bool {
    let after_one_blank = |at: usize| {
        at.checked_sub(1)
            .and_then(|before| row.get(before))
            .filter(|before| row.get(at).is_some_and(|token| before.gap_to(token) == 1))
    };
    let value = |at: usize| {
        row.get(at).is_some_and(|token| token.kind == Kind::Figure)
            && after_one_blank(at).is_some_and(|label| label.text.ends_with(':'))
    };
    let Some(token) = row.get(at).filter(|token| token.kind == Kind::Figure) else {
        return false;
    };
    let share = token.text.starts_with('(') && is_share(&token.text);
    let of_what = after_one_blank(at).is_some_and(|before| {
        before.text == "tokens" || (before.kind == Kind::Figure && value(at - 1))
    });
    value(at) || (share && of_what)
}

fn is_share(text: &str) -> bool {
    text.trim_end_matches(')').ends_with('%')
}

/// A bar, a day's small bar or a meter: the eighths, the solid block, a meter's rule, a
/// progress bar's `▰▱`, or a run of squares. One square alone is a legend's mark.
fn is_bar(text: &str) -> bool {
    let squares = text.chars().count() > 1 && text.chars().all(|c| matches!(c, '▪' | '■'));
    squares
        || text
            .chars()
            .all(|c| ('\u{2581}'..='\u{2588}').contains(&c) || matches!(c, '━' | '╸' | '▰' | '▱'))
}

/// Box drawing: a frame's edges, rules and the corners between them.
fn is_frame(text: &str) -> bool {
    text.chars()
        .all(|c| ('\u{2500}'..='\u{257f}').contains(&c) && !matches!(c, '━' | '╸'))
}

/// Glyphs a model's dot may take; a CLI's models share its hue on them.
const DOTS: [char; 5] = ['●', '○', '•', '◉', '◦'];

/// Whether a mark is a CLI's own, its dot or its logo's placeholders.
fn is_clis_mark(text: &str) -> bool {
    text.starts_with(PLACEHOLDER) || text.chars().all(|c| DOTS.contains(&c))
}

/// A swatch seen beside a model's name.
struct Swatch<'a> {
    model: &'a str,
    glyph: String,
    colour: Colour,
    row: u16,
    col: u16,
}

/// Law 3: each model's swatches, the marks just left of its name, by glyph and colour. Two
/// models with the same swatch break it, unless it's a dot or grey.
pub fn models(grid: &Grid, look: &Look) -> Vec<Breach> {
    let area = look.area(grid);
    let mut names: Vec<&str> = look.models.iter().map(String::as_str).collect();
    // The longer name first, so "GPT 6 Luna" is found before a model named "GPT 6".
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let mut seen: Vec<Swatch> = Vec::new();
    let mut breaches = Vec::new();
    for row in area.y..area.bottom() {
        let mut taken: Vec<(u16, u16)> = Vec::new();
        for &name in &names {
            for start in grid.find_in_row(area, row, name) {
                let end = start.saturating_add(u16::try_from(text_width(name)).unwrap_or(u16::MAX));
                if taken.iter().any(|&(s, e)| start < e && s < end) {
                    continue;
                }
                taken.push((start, end));
                for (glyph, colour, col) in swatch(grid, area, row, start) {
                    let alike = |other: &&Swatch| other.glyph == glyph && other.colour == colour;
                    if let Some(other) = seen.iter().filter(alike).find(|o| o.model != name) {
                        breaches.push(Breach {
                            law: Law::ModelsLookApart,
                            row,
                            col,
                            what: format!(
                                "{name} draws {glyph} in {colour}, as {} does at row {}, \
                                 column {}",
                                other.model, other.row, other.col
                            ),
                        });
                    } else if !seen.iter().filter(alike).any(|o| o.model == name) {
                        seen.push(Swatch {
                            model: name,
                            glyph,
                            colour,
                            row,
                            col,
                        });
                    }
                }
            }
        }
    }
    breaches
}

/// The swatch cells just left of a name starting at `start`: the marks up to three blanks
/// before it, with greys, dots and pictures left out, and marks in the terminal's own colour,
/// which a screen drawn without colour gives every model alike.
fn swatch(grid: &Grid, area: Area, row: u16, start: u16) -> Vec<(String, Colour, u16)> {
    let mut found = Vec::new();
    let mut col = start;
    let mut blanks = 0;
    while col > area.x && blanks <= 3 {
        col -= 1;
        let cell = grid.cell(row, col);
        if cell.is_covered() {
            continue;
        }
        if cell.is_blank() {
            blanks += 1;
            continue;
        }
        let glyph = cell.text.as_str();
        if glyph.chars().any(char::is_alphanumeric) || is_figure(glyph) || is_frame(glyph) {
            break;
        }
        blanks = 0;
        let picture = cell.picture.is_some();
        let dot = glyph.chars().all(|c| DOTS.contains(&c));
        let coloured = cell.ink != Colour::Own && !cell.ink.is_grey();
        if !picture && !dot && coloured {
            found.push((glyph.to_owned(), cell.ink, col));
        }
    }
    found
}

/// Law 4: wherever one of the look's texts shows cut with `…`, the stretch of its row after the
/// cut has fewer free cells than the rest of the text would take.
pub fn cut(grid: &Grid, look: &Look) -> Vec<Breach> {
    let area = look.area(grid);
    let mut breaches = Vec::new();
    for row in area.y..area.bottom() {
        for at in grid.find_in_row(area, row, "…") {
            let before = grid.text(row, area.x, at);
            let Some((text, shown)) = look
                .whole
                .iter()
                .find_map(|text| cut_start(&before, text).map(|shown| (text, shown)))
            else {
                continue;
            };
            // Whole, the text takes its own cells; cut, the start and the `…`.
            let missing = text_width(text).saturating_sub(text_width(shown) + 1);
            let free = free_after(grid, area, row, at + 1);
            if free >= missing {
                breaches.push(Breach {
                    law: Law::NothingCutWithRoom,
                    row,
                    col: at,
                    what: format!(
                        "“{text}” is cut to “{shown}…” with {free} free cells after it, and \
                         the rest takes {missing}"
                    ),
                });
            }
        }
    }
    breaches
}

/// The longest start of `text`, cut short, that `before` (a row up to a `…`) ends with, where
/// that start begins a cell of the row ([`starts_a_cell`]).
fn cut_start<'t>(before: &str, text: &'t str) -> Option<&'t str> {
    let shown = before.trim_end();
    text.char_indices()
        .skip(2)
        .map(|(at, _)| text[..at].trim_end())
        .filter(|start| {
            shown.ends_with(start) && starts_a_cell(&shown[..shown.len() - start.len()])
        })
        .last()
}

/// Whether text after `before` starts a cell of its own: at the row's start, after a column's
/// gap, a frame's edge or a mark such as `█ `. A name after a word or a stop and one blank is
/// in a sentence, which a row may cut at its edge.
fn starts_a_cell(before: &str) -> bool {
    if before.trim().is_empty() || before.ends_with("  ") {
        return true;
    }
    let glyph = before.strip_suffix(' ').unwrap_or(before).chars().last();
    glyph.is_some_and(|c| !c.is_alphanumeric() && !c.is_ascii_punctuation() && c != ' ')
}

/// The blank cells at the end of the row's stretch from `col`, up to a frame's edge or the
/// panel's.
fn free_after(grid: &Grid, area: Area, row: u16, col: u16) -> usize {
    let mut end = col;
    while end < area.right() && !is_frame(&grid.cell(row, end).text) {
        end += 1;
    }
    let mut at = end;
    while at > col && grid.cell(row, at - 1).is_blank() {
        at -= 1;
    }
    let free = usize::from(end - at);
    // Iosevka paints the `…` two cells wide, so a stretch left blank gives its first cell to it.
    if at == col {
        free.saturating_sub(1)
    } else {
        free
    }
}

/// Law 5: when the look names a list's rows and fewer of them show than it holds, the panel
/// counts the hidden ones with a `N more`, alone or with every other `N more` there added up
/// (`↑ 2 more` above, `↓ 3 more` below), or gives the rows it shows of the list's whole length
/// (`6–15 of 17`).
pub fn hidden_rows(grid: &Grid, look: &Look) -> Vec<Breach> {
    if look.list.is_empty() {
        return Vec::new();
    }
    let area = look.area(grid);
    let lines = grid.lines(area);
    let total = look.list.len();
    let shown: Vec<u16> = look
        .list
        .iter()
        .filter_map(|row| {
            lines
                .iter()
                .zip(area.y..)
                .find_map(|(line, at)| shows(line, row).then_some(at))
        })
        .collect();
    let hidden = total - shown.len();
    let counts = more(&lines);
    let ranges = ranges(&lines);
    let counted = counts.contains(&hidden) || counts.iter().sum::<usize>() == hidden;
    let ranged = ranges
        .iter()
        .any(|&(first, last, of)| of == total && last + 1 == first + shown.len());
    if hidden == 0 || counted || ranged {
        return Vec::new();
    }
    let said: Vec<String> = counts
        .iter()
        .map(|count| format!("“{count} more”"))
        .chain(
            ranges
                .iter()
                .map(|(first, last, of)| format!("“{first}–{last} of {of}”")),
        )
        .collect();
    let said = if said.is_empty() {
        "nothing says so".to_owned()
    } else {
        format!("it says {}", said.join(" and "))
    };
    let below = shown.iter().max().map_or(area.y, |last| {
        (last + 1).min(area.bottom().saturating_sub(1))
    });
    vec![Breach {
        law: Law::HiddenRowsCounted,
        row: below,
        col: area.x,
        what: format!(
            "the list shows {} of its {total} rows and hides {hidden}, and {said}",
            shown.len()
        ),
    }]
}

/// Whether a line of the screen shows a list's `row` as words of their own, whole or cut with
/// `…`.
fn shows(line: &str, row: &str) -> bool {
    let whole = line.match_indices(row).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + row.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    });
    whole
        || line
            .match_indices('…')
            .any(|(at, _)| cut_start(&line[..at], row).is_some())
}

/// The count of every `N more` on the lines.
fn more(lines: &[String]) -> Vec<usize> {
    lines
        .iter()
        .flat_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words
                .windows(2)
                .filter(|pair| pair[1].starts_with("more"))
                .filter_map(|pair| count(pair[0]))
                .collect::<Vec<usize>>()
        })
        .collect()
}

/// Every `first–last of total` on the lines.
fn ranges(lines: &[String]) -> Vec<(usize, usize, usize)> {
    lines
        .iter()
        .flat_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words
                .windows(3)
                .filter(|three| three[1] == "of")
                .filter_map(|three| {
                    let (first, last) = three[0].split_once(['–', '-'])?;
                    Some((count(first)?, count(last)?, count(three[2])?))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A count as a screen writes it, `1,204` for a thousand and more.
fn count(word: &str) -> Option<usize> {
    word.replace(',', "").parse().ok()
}
