//! The table under By model's and By role's bars: a row for each model or role in a window of
//! nine, whose first and last rows hold the sums of the rows out of view, so what shows always
//! adds up to the totals. ↓ from the sub-tabs puts the keys on it: ↑↓ point at a row, which
//! lights its part in every bar, ←→ step the sort, `/` filters the rows, and Enter opens the
//! pointed row's card. Enter on a row of sums shows every row.

use std::cell::Cell;
use std::ops::Range;

use argonauts_core::Spend;
use crossterm::event::KeyCode;

use super::data::{Cli, Model};

/// Rows the table shows at once, its rows of sums among them.
pub const WINDOW: usize = 9;

/// How the rows are sorted, in the order ←→ step through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// The way the bars run: each CLI's models together, the CLIs in the bars' order.
    Bars,
    Tokens,
    Spend,
    Time,
    Name,
}

impl Sort {
    const ALL: [Sort; 5] = [
        Sort::Bars,
        Sort::Tokens,
        Sort::Spend,
        Sort::Time,
        Sort::Name,
    ];

    /// What the key line says the rows are sorted by.
    pub fn word(self) -> &'static str {
        match self {
            Sort::Bars => "bar order",
            Sort::Tokens => "tokens",
            Sort::Spend => "spend",
            Sort::Time => "time",
            Sort::Name => "name",
        }
    }

    fn step(self, by: isize) -> Sort {
        let at = Self::ALL.iter().position(|sort| *sort == self).unwrap_or(0);
        let next = (at as isize + by).rem_euclid(Self::ALL.len() as isize) as usize;
        Self::ALL[next]
    }
}

/// What the table reads of a model or a role: its name, the words a search finds it by, and
/// what it used.
#[derive(Debug, Clone)]
pub(super) struct Counted {
    pub name: String,
    /// Its name, its model's id and its CLI's name, in lower case.
    pub search: String,
    pub spent: Spend,
    pub took_secs: u64,
}

impl Counted {
    /// A model as the table reads it: a search finds it by its name, its id and its CLI.
    pub fn model(cli: &Cli, model: &Model) -> Self {
        let id = model.model.as_deref().unwrap_or_default();
        Self {
            name: model.name.clone(),
            search: format!("{} {id} {}", model.name, cli.name).to_lowercase(),
            spent: model.spent,
            took_secs: model.took_secs(),
        }
    }

    pub fn tokens(&self) -> u64 {
        self.spent.total_tokens()
    }

    /// Spend in hundredths of a cent, so a part under a cent keeps its share and its place.
    pub fn spend(&self) -> u64 {
        fine(&self.spent)
    }
}

/// Spend in hundredths of a cent.
pub(super) fn fine(spent: &Spend) -> u64 {
    (spent.cost_usd * 10_000.0).round() as u64
}

/// A row of sums: the one over the window for the rows above it, or the one under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Above,
    Below,
}

/// Which rows the window shows of the rows in order: the ones above it and below it go into
/// their marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub rows: Range<usize>,
    pub above: usize,
    pub below: usize,
}

impl Window {
    /// The window over `count` rows from `top`: every row when they fit or every one was asked
    /// for, else eight and a mark at either end, a mark at both ends and seven between them.
    pub fn of(count: usize, top: usize, every_row: bool) -> Self {
        if every_row || count <= WINDOW {
            return Self {
                rows: 0..count,
                above: 0,
                below: 0,
            };
        }
        let top = top.min(count - (WINDOW - 1));
        let room = WINDOW - usize::from(top > 0);
        let end = if top + room >= count {
            count
        } else {
            top + room - 1
        };
        Self {
            rows: top..end,
            above: top,
            below: count - end,
        }
    }
}

/// The table's keys and what they chose, kept while Stats stays open.
#[derive(Debug, Clone, Default)]
pub struct Table {
    /// The sort ←→ chose. Until then the rows run as the bars do where every one fits the
    /// window, and the most tokens first where they don't.
    pub sort: Option<Sort>,
    /// The row the keys are on, among the rows the filter keeps, in the table's order.
    pub at: usize,
    /// The keys are on a row of sums instead.
    pub on_mark: Option<Mark>,
    /// The first row in the window.
    pub top: usize,
    /// What `/` filters the rows by, and whether it's still being typed.
    pub query: Option<String>,
    pub typing: bool,
    /// Enter kept the filter: no row is pointed at until ↑ or ↓, so every match shows alike.
    pub waiting: bool,
    /// Enter on a row of sums showed every row.
    pub every_row: bool,
    /// How many rows the table had when last drawn, which sets the sort until ←→ choose one.
    parts: Cell<usize>,
}

/// What a key in the table asks of the tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKey {
    Moved,
    /// Enter on the row of this part, by its place among the parts in the bars' order.
    Open(usize),
    /// Esc with nothing left to clear: the keys go back up to the sub-tabs.
    Leave,
}

impl Table {
    /// Notes that the table was drawn with `count` rows, for the key line's sort.
    pub fn seen(&self, count: usize) {
        self.parts.set(count);
    }

    /// The rows the table had when last drawn.
    pub fn parts(&self) -> usize {
        self.parts.get()
    }

    /// The sort the rows are in for `count` parts in all.
    pub fn sort_for(&self, count: usize) -> Sort {
        self.sort.unwrap_or(if count <= WINDOW {
            Sort::Bars
        } else {
            Sort::Tokens
        })
    }

    /// The parts the filter keeps, by their place in `parts`, in the table's order.
    pub(super) fn order(&self, parts: &[Counted]) -> Vec<usize> {
        let query = self.query.as_deref().map(str::to_lowercase);
        let mut order: Vec<usize> = (0..parts.len())
            .filter(|at| match &query {
                Some(query) => parts[*at].search.contains(query.as_str()),
                None => true,
            })
            .collect();
        let most = |key: fn(&Counted) -> u64| {
            move |a: &usize, b: &usize| key(&parts[*b]).cmp(&key(&parts[*a])).then(a.cmp(b))
        };
        match self.sort_for(parts.len()) {
            Sort::Bars => {}
            Sort::Tokens => order.sort_by(most(Counted::tokens)),
            Sort::Spend => order.sort_by(most(Counted::spend)),
            Sort::Time => order.sort_by(most(|part| part.took_secs)),
            Sort::Name => order.sort_by(|a, b| {
                let name = |at: &usize| parts[*at].name.to_lowercase();
                name(a).cmp(&name(b)).then(a.cmp(b))
            }),
        }
        order
    }

    /// Which parts get cards, the `cards` with the most tokens, and which get a line each,
    /// both in the table's order, whatever the filter keeps.
    pub(super) fn carded(&self, parts: &[Counted], cards: usize) -> (Vec<usize>, Vec<usize>) {
        let mut most: Vec<usize> = (0..parts.len()).collect();
        most.sort_by(|a, b| parts[*b].tokens().cmp(&parts[*a].tokens()).then(a.cmp(b)));
        most.truncate(cards);
        let unfiltered = Self {
            query: None,
            ..self.clone()
        };
        unfiltered
            .order(parts)
            .into_iter()
            .partition(|at| most.contains(at))
    }

    /// The window over `count` rows in order. With the keys on a row, it's moved the least
    /// that shows that row.
    pub fn window(&self, count: usize) -> Window {
        let top = match self.on_mark {
            None if count > 0 => showing(self.top, self.at.min(count - 1), count),
            _ => self.top,
        };
        Window::of(count, top, self.every_row)
    }

    /// The part the keys are on, by its place in `parts`, when they're on a row.
    pub(super) fn pointed(&self, parts: &[Counted]) -> Option<usize> {
        if self.on_mark.is_some() || self.waiting {
            return None;
        }
        let order = self.order(parts);
        order
            .get(self.at.min(order.len().saturating_sub(1)))
            .copied()
    }

    /// Whether the pointer shows: the keys are on the table and on a row or a mark.
    pub fn pointing(&self, in_table: bool) -> bool {
        in_table && !self.waiting
    }

    /// The parts whose shares stay lit in the bars and the chart: the pointed row's, or the
    /// rows a pointed mark holds. `None` lights every part, as a filter does.
    pub(super) fn lit(&self, parts: &[Counted], in_table: bool) -> Option<Vec<bool>> {
        let order = self.order(parts);
        let lit = |rows: &[usize]| {
            let mut lit = vec![false; parts.len()];
            for at in rows {
                lit[*at] = true;
            }
            Some(lit)
        };
        if !self.pointing(in_table) || order.is_empty() {
            return None;
        }
        let window = self.window(order.len());
        match self.on_mark {
            Some(Mark::Above) => lit(&order[..window.rows.start]),
            Some(Mark::Below) => lit(&order[window.rows.end..]),
            None => lit(&order[self.at.min(order.len() - 1)..][..1]),
        }
    }

    /// Puts the keys on the row of part `part`, clearing a filter that leaves it out.
    pub(super) fn point_at(&mut self, part: usize, parts: &[Counted]) {
        if !self.order(parts).contains(&part) {
            self.query = None;
            self.typing = false;
        }
        let order = self.order(parts);
        self.at = order.iter().position(|at| *at == part).unwrap_or(0);
        self.keep_in_view(order.len());
    }

    /// Whether the keys are on the first row, so ↑ goes back up to the sub-tabs.
    pub fn on_first(&self) -> bool {
        self.at == 0 && self.on_mark.is_none()
    }

    /// The table takes `code` itself, before the panel reads it as one of its own keys: the
    /// letters while a filter is typed, the side arrows for the sort, `/`, and Esc while there
    /// is something to clear.
    pub fn claims(&self, code: KeyCode) -> bool {
        if self.typing {
            return !matches!(code, KeyCode::Tab | KeyCode::BackTab);
        }
        matches!(
            code,
            KeyCode::Left | KeyCode::Right | KeyCode::Char('/') | KeyCode::Esc
        )
    }

    /// A key while the keys are on the table, whose parts are `parts` in the bars' order.
    pub(super) fn key(&mut self, code: KeyCode, parts: &[Counted]) -> Option<TableKey> {
        if self.typing && self.type_key(code) {
            self.at = 0;
            self.top = 0;
            self.on_mark = None;
            return Some(TableKey::Moved);
        }
        // After the filter was kept, the first step or Enter shows the pointer where it is.
        if self.waiting && matches!(code, KeyCode::Up | KeyCode::Down | KeyCode::Enter) {
            self.waiting = false;
            return Some(TableKey::Moved);
        }
        let order = self.order(parts);
        let count = order.len();
        // The window stays where it shows, so a step onto a mark doesn't move it.
        if self.on_mark.is_none() && count > 0 {
            self.top = self.window(count).rows.start;
        }
        match code {
            KeyCode::Up => self.up(count),
            KeyCode::Down => self.down(count),
            KeyCode::Home => {
                self.at = 0;
                self.top = 0;
                self.on_mark = None;
            }
            KeyCode::End => {
                self.at = count.saturating_sub(1);
                self.on_mark = None;
                self.top = count;
            }
            KeyCode::Left | KeyCode::Right => {
                let by = if code == KeyCode::Left { -1 } else { 1 };
                let pointed = self.pointed(parts);
                self.sort = Some(self.sort_for(parts.len()).step(by));
                if let Some(part) = pointed {
                    let order = self.order(parts);
                    self.at = order.iter().position(|at| *at == part).unwrap_or(0);
                    self.keep_in_view(order.len());
                }
            }
            KeyCode::Char('/') => {
                self.query = Some(String::new());
                self.typing = true;
                self.waiting = false;
                self.at = 0;
                self.top = 0;
                self.on_mark = None;
            }
            KeyCode::Enter => match self.on_mark {
                Some(_) => {
                    self.every_row = true;
                    self.on_mark = None;
                    self.at = self.at.min(count.saturating_sub(1));
                }
                None => return self.pointed(parts).map(TableKey::Open),
            },
            KeyCode::Esc if self.every_row => {
                self.every_row = false;
                self.keep_in_view(count);
            }
            KeyCode::Esc if self.query.is_some() => {
                self.query = None;
                self.typing = false;
                self.waiting = false;
                self.at = 0;
                self.top = 0;
                self.on_mark = None;
            }
            KeyCode::Esc => return Some(TableKey::Leave),
            _ => return None,
        }
        Some(TableKey::Moved)
    }

    /// A key while the filter is typed: a letter adds to it, Backspace takes one off, Enter
    /// keeps it and Esc clears it. Whether the filter changed.
    fn type_key(&mut self, code: KeyCode) -> bool {
        let query = self.query.get_or_insert_with(String::new);
        match code {
            KeyCode::Char(ch) => query.push(ch),
            KeyCode::Backspace => {
                query.pop();
            }
            KeyCode::Enter => {
                self.typing = false;
                self.waiting = true;
            }
            KeyCode::Esc => {
                self.query = None;
                self.typing = false;
            }
            _ => return false,
        }
        true
    }

    fn down(&mut self, count: usize) {
        let window = self.window(count);
        match self.on_mark {
            Some(Mark::Above) => {
                self.on_mark = None;
                self.at = window.rows.start;
            }
            // On the mark below, ↓ brings the next row into view and stays on the mark until
            // the window reaches the last row.
            Some(Mark::Below) => {
                self.reveal(window.rows.end, count);
                if self.window(count).below == 0 {
                    self.on_mark = None;
                    self.at = count.saturating_sub(1);
                }
            }
            None if self.at + 1 < window.rows.end => self.at += 1,
            None if window.below > 0 => self.on_mark = Some(Mark::Below),
            None => {}
        }
    }

    fn up(&mut self, count: usize) {
        let window = self.window(count);
        match self.on_mark {
            Some(Mark::Below) => {
                self.on_mark = None;
                self.at = window.rows.end.saturating_sub(1);
            }
            Some(Mark::Above) => {
                self.reveal(window.rows.start.saturating_sub(1), count);
                if self.window(count).above == 0 {
                    self.on_mark = None;
                    self.at = 0;
                }
            }
            None if self.at > window.rows.start => self.at -= 1,
            None if window.above > 0 => self.on_mark = Some(Mark::Above),
            None => {}
        }
    }

    /// Moves the window the least that brings row `row` of `count` into view.
    fn reveal(&mut self, row: usize, count: usize) {
        self.top = showing(self.window(count).rows.start, row, count);
    }

    /// Moves the window so the row the keys are on shows.
    fn keep_in_view(&mut self, count: usize) {
        self.on_mark = None;
        self.reveal(self.at, count);
    }
}

/// The first row of the window nearest the one from `top` that shows row `row` of `count`.
fn showing(top: usize, row: usize, count: usize) -> usize {
    let window = Window::of(count, top, false);
    if window.rows.contains(&row) || count <= WINDOW {
        return window.rows.start;
    }
    if row < window.rows.start {
        return row;
    }
    // With a mark at both ends the window holds seven rows, and at the end eight.
    let top = (row + 1).saturating_sub(WINDOW - 2);
    if Window::of(count, top, false).rows.contains(&row) {
        top
    } else {
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counted(name: &str, tokens: u64) -> Counted {
        Counted {
            name: name.to_owned(),
            search: name.to_lowercase(),
            spent: Spend {
                input_tokens: tokens,
                ..Spend::default()
            },
            took_secs: tokens,
        }
    }

    fn parts(count: u64) -> Vec<Counted> {
        (0..count)
            .map(|at| counted(&format!("model {at}"), 100 - at))
            .collect()
    }

    #[test]
    fn the_window_holds_nine_rows_with_its_marks() {
        assert_eq!(Window::of(9, 0, false).rows, 0..9);
        let first = Window::of(15, 0, false);
        assert_eq!((first.rows, first.above, first.below), (0..8, 0, 7));
        let middle = Window::of(15, 3, false);
        assert_eq!((middle.rows, middle.above, middle.below), (3..10, 3, 5));
        let last = Window::of(15, 99, false);
        assert_eq!((last.rows, last.above, last.below), (7..15, 7, 0));
        assert_eq!(Window::of(15, 3, true).rows, 0..15);
    }

    #[test]
    fn rows_run_as_the_bars_until_they_overflow_the_window() {
        let table = Table::default();
        assert_eq!(table.sort_for(9), Sort::Bars);
        assert_eq!(table.sort_for(10), Sort::Tokens);
        let mut parts = parts(3);
        parts[2].spent.input_tokens = 1_000;
        assert_eq!(table.order(&parts), [0, 1, 2]);
        let by_tokens = Table {
            sort: Some(Sort::Tokens),
            ..Table::default()
        };
        assert_eq!(by_tokens.order(&parts), [2, 0, 1]);
    }

    #[test]
    fn down_past_the_window_goes_onto_its_mark_then_scrolls_under_it() {
        let parts = parts(15);
        let mut table = Table::default();
        for _ in 0..7 {
            table.key(KeyCode::Down, &parts);
        }
        assert_eq!((table.at, table.on_mark), (7, None));
        table.key(KeyCode::Down, &parts);
        assert_eq!(table.on_mark, Some(Mark::Below));
        table.key(KeyCode::Down, &parts);
        let window = table.window(15);
        assert_eq!(window.rows, 2..9, "row 8 came into view");
        assert_eq!(table.on_mark, Some(Mark::Below));
        for _ in 0..10 {
            table.key(KeyCode::Down, &parts);
        }
        assert_eq!((table.at, table.on_mark), (14, None));
        assert_eq!(table.window(15).below, 0);
    }

    #[test]
    fn enter_on_a_mark_shows_every_row_and_esc_goes_back() {
        let parts = parts(15);
        let mut table = Table {
            on_mark: Some(Mark::Below),
            ..Table::default()
        };
        assert_eq!(table.key(KeyCode::Enter, &parts), Some(TableKey::Moved));
        assert_eq!(table.window(15).rows, 0..15);
        table.key(KeyCode::Esc, &parts);
        assert_eq!(table.window(15).rows, 0..8);
        assert_eq!(table.key(KeyCode::Esc, &parts), Some(TableKey::Leave));
    }

    #[test]
    fn a_filter_keeps_the_matches_and_esc_clears_it() {
        let mut parts = parts(12);
        parts[4].search.push_str(" gemini-3.8-flash");
        parts[9].search.push_str(" gemini-3.1-pro");
        let mut table = Table::default();
        table.key(KeyCode::Char('/'), &parts);
        for ch in "Gemini".chars() {
            table.key(KeyCode::Char(ch), &parts);
        }
        table.key(KeyCode::Enter, &parts);
        assert!(!table.typing);
        assert_eq!(table.order(&parts), [4, 9]);
        assert_eq!(table.pointed(&parts), None, "every match shows alike");
        assert_eq!(table.lit(&parts, true), None);
        table.key(KeyCode::Down, &parts);
        assert_eq!(table.pointed(&parts), Some(4));
        assert_eq!(table.key(KeyCode::Enter, &parts), Some(TableKey::Open(4)));
        table.key(KeyCode::Esc, &parts);
        assert_eq!(table.query, None);
        assert_eq!(table.order(&parts).len(), 12);
    }

    #[test]
    fn the_sort_keeps_the_pointed_row() {
        let mut parts = parts(4);
        parts[3].took_secs = 1_000;
        let mut table = Table {
            at: 3,
            ..Table::default()
        };
        table.key(KeyCode::Right, &parts);
        assert_eq!(table.sort, Some(Sort::Tokens));
        table.key(KeyCode::Right, &parts);
        table.key(KeyCode::Right, &parts);
        assert_eq!(table.sort, Some(Sort::Time));
        assert_eq!(table.pointed(&parts), Some(3));
        assert_eq!(table.at, 0, "the longest time is first");
    }
}
