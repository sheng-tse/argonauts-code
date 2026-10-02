//! A scrolling list with a selection, shared by the list-shaped views: the resume picker,
//! help, `/config`, `/approvals`, and `/context`. Also the keys every list shares, and what
//! vim keys stand for in them with `ui.keys = "vim"`.

use argonauts_core::KeyScheme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::state::AppState;
use crate::text::str_width;

use super::layout::{left, right};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListState {
    pub selected: usize,
    /// The first visible row.
    pub offset: usize,
}

impl ListState {
    /// Moves by `delta` rows within `len`, scrolling so the selection stays in `visible` rows.
    pub fn move_by(&mut self, delta: isize, len: usize, visible: usize) {
        if len == 0 {
            *self = Self::default();
            return;
        }
        let last = len - 1;
        self.selected = self
            .selected
            .min(last)
            .saturating_add_signed(delta)
            .min(last);
        self.keep_visible(len, visible);
    }

    pub fn move_to(&mut self, index: usize, len: usize, visible: usize) {
        self.selected = index.min(len.saturating_sub(1));
        self.keep_visible(len, visible);
    }

    pub fn apply(&mut self, nav: Nav, len: usize, visible: usize) {
        match nav {
            Nav::By(delta) => self.move_by(delta, len, visible),
            Nav::Top => self.move_to(0, len, visible),
            Nav::Bottom => self.move_to(len.saturating_sub(1), len, visible),
        }
    }

    /// Scrolls the least that brings the selection into view, and never past the end, so a
    /// resize or a shorter list leaves no blank rows under the last one.
    pub fn keep_visible(&mut self, len: usize, visible: usize) {
        if len == 0 {
            *self = Self::default();
            return;
        }
        self.selected = self.selected.min(len - 1);
        let visible = visible.max(1);
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + visible {
            self.offset = self.selected + 1 - visible;
        }
        self.offset = self.offset.min(len.saturating_sub(visible));
    }
}

/// A movement a key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    By(isize),
    Top,
    Bottom,
}

/// Reads the movement keys every list shares: arrows, PgUp and PgDn by `page` rows, Home and
/// End. Vim keys arrive here as these keys, through [`standard`].
pub fn nav_key(key: &KeyEvent, page: usize) -> Option<Nav> {
    let page = isize::try_from(page.max(1)).unwrap_or(isize::MAX);
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(Nav::By(-1)),
        KeyCode::Down => Some(Nav::By(1)),
        KeyCode::PageUp => Some(Nav::By(-page)),
        KeyCode::PageDown => Some(Nav::By(page)),
        KeyCode::Home => Some(Nav::Top),
        KeyCode::End => Some(Nav::Bottom),
        _ => None,
    }
}

/// The key a list's vim key stands for: `j` and `k` the arrows, `g` and `G` Home and End, `h`
/// the list's `back` key (Esc, or ← where ← goes up a level), and `l` Enter.
pub fn vim_stand_in(key: &KeyEvent, back: KeyCode) -> Option<KeyEvent> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    let code = match key.code {
        KeyCode::Char('j') => KeyCode::Down,
        KeyCode::Char('k') => KeyCode::Up,
        KeyCode::Char('g') => KeyCode::Home,
        KeyCode::Char('G') => KeyCode::End,
        KeyCode::Char('h') => back,
        KeyCode::Char('l') => KeyCode::Enter,
        _ => return None,
    };
    Some(KeyEvent::new(code, KeyModifiers::NONE))
}

/// `key` as the list reads it: with vim keys on, the key it stands for ([`vim_stand_in`]).
pub fn standard(key: KeyEvent, vim: bool, back: KeyCode) -> KeyEvent {
    vim.then(|| vim_stand_in(&key, back))
        .flatten()
        .unwrap_or(key)
}

/// What a key does in a list that filters as you type, once vim keys are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKey {
    /// `/`: typing goes to the filter from now on.
    Start,
    /// Esc while typing the filter: it's dropped, and the list moves with its keys again.
    Leave,
    /// A letter with no use here, kept out of the filter.
    Nothing,
    /// Handled as the list handles it without vim keys.
    Key(KeyEvent),
}

/// With vim keys, `/` starts a list's filter and Esc leaves it. Until then letters never
/// filter: the vim ones move and choose, and the rest do nothing.
pub fn filter_key(key: KeyEvent, filtering: bool) -> FilterKey {
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc if filtering => FilterKey::Leave,
        _ if filtering => FilterKey::Key(key),
        KeyCode::Char('/') if plain => FilterKey::Start,
        KeyCode::Char(_) if plain => {
            vim_stand_in(&key, KeyCode::Esc).map_or(FilterKey::Nothing, FilterKey::Key)
        }
        _ => FilterKey::Key(key),
    }
}

/// The new top row of a view that scrolls as a whole, `rows` long with `page` rows shown.
pub fn scroll(offset: usize, nav: Nav, rows: usize, page: usize) -> usize {
    let mut top = ListState {
        selected: offset,
        offset,
    };
    top.apply(nav, rows.saturating_sub(page) + 1, 1);
    top.selected
}

/// One column of a list-shaped view.
#[derive(Debug, Clone)]
pub struct Column {
    pub heading: &'static str,
    /// One per row, already sanitized.
    pub cells: Vec<String>,
    /// Right-aligned, like numbers and times.
    pub numeric: bool,
    /// Takes the room the others leave, cut with `…`. At most one column flexes.
    pub flex: bool,
    /// Narrow screens drop the lowest first; 0 is never dropped.
    pub keep: u8,
    pub width: usize,
    /// The least room a flexing column keeps before others give way.
    pub min: usize,
}

/// The least room a flexing column keeps before others give way.
const FLEX_MIN: usize = 12;

impl Column {
    pub fn new(heading: &'static str, cells: Vec<String>) -> Self {
        let width = cells
            .iter()
            .map(|cell| str_width(cell))
            .chain([str_width(heading)])
            .max()
            .unwrap_or(0);
        Self {
            heading,
            cells,
            numeric: false,
            flex: false,
            keep: 0,
            width,
            min: FLEX_MIN,
        }
    }

    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    pub fn flex(mut self) -> Self {
        self.flex = true;
        self
    }

    pub fn keep(mut self, keep: u8) -> Self {
        self.keep = keep;
        self
    }

    /// Flexes, keeping at least `min` cells before other columns give way.
    pub fn flex_min(mut self, min: usize) -> Self {
        self.flex = true;
        self.min = min;
        self
    }

    /// `text` in this column's slot: padded, aligned, and cut to fit.
    pub fn cell(&self, text: &str) -> String {
        if self.numeric {
            right(text, self.width)
        } else {
            left(text, self.width)
        }
    }
}

/// Fits `columns` into `width` cells, `gap` cells apart. Columns give way from the lowest
/// `keep` until the rest fit with the flexing one at its least, then the flexing column takes
/// what's left and the last column standing is cut.
pub fn fit_columns(mut columns: Vec<Column>, width: usize, gap: usize) -> Vec<Column> {
    let total = |columns: &[Column]| {
        let widths: usize = columns
            .iter()
            .map(|c| if c.flex { c.width.min(c.min) } else { c.width })
            .sum();
        widths + gap * columns.len().saturating_sub(1)
    };
    while total(&columns) > width {
        let Some(drop) = columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.keep > 0)
            .min_by_key(|(i, c)| (c.keep, usize::MAX - i))
            .map(|(i, _)| i)
        else {
            break;
        };
        columns.remove(drop);
    }
    let fixed: usize = columns.iter().filter(|c| !c.flex).map(|c| c.width).sum();
    let room = width.saturating_sub(fixed + gap * columns.len().saturating_sub(1));
    if let Some(flex) = columns.iter_mut().find(|c| c.flex) {
        flex.width = flex.width.min(room);
    }
    // Still too wide with nothing left to drop: the widest cuts.
    let over = total_width(&columns, gap).saturating_sub(width);
    if over > 0
        && let Some(widest) = columns.iter_mut().max_by_key(|c| c.width)
    {
        widest.width = widest.width.saturating_sub(over);
    }
    columns
}

/// The end of `text` in `width` cells, with `…` in front when it had to cut, so the text
/// you type stays in view.
pub fn tail(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_owned();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1;
    for c in text.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width {
            break;
        }
        kept.push(c);
        used += w;
    }
    let tail: String = kept.into_iter().rev().collect();
    if width == 0 {
        tail
    } else {
        format!("…{tail}")
    }
}

/// The cells `columns` take with their gaps.
pub fn total_width(columns: &[Column], gap: usize) -> usize {
    columns.iter().map(|c| c.width).sum::<usize>() + gap * columns.len().saturating_sub(1)
}

/// Whether `ui.keys` is vim, which adds `hjkl`, `g`, `G`, and `/` to every list.
pub fn vim(state: &AppState) -> bool {
    state
        .ui
        .as_ref()
        .is_some_and(|ui| ui.keys == KeyScheme::Vim)
}

/// A state with vim keys on or off, for the tests of lists that read it.
#[cfg(test)]
pub(crate) fn keys_state(vim: bool) -> AppState {
    let keys = if vim {
        KeyScheme::Vim
    } else {
        KeyScheme::Standard
    };
    let ui = argonauts_core::settings::ui::UiSettings {
        keys,
        ..Default::default()
    };
    let mut state = AppState::default();
    state.ui = Some(ui);
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn list_state_keeps_selection_visible() {
        let mut list = ListState::default();
        list.move_by(7, 20, 5);
        assert_eq!((list.selected, list.offset), (7, 3));
        list.move_by(-6, 20, 5);
        assert_eq!((list.selected, list.offset), (1, 1));
        list.move_by(100, 20, 5);
        assert_eq!((list.selected, list.offset), (19, 15));
        list.move_by(-100, 20, 5);
        assert_eq!((list.selected, list.offset), (0, 0));

        // A shorter list or a taller screen never leaves the selection off screen.
        list.move_to(19, 20, 5);
        list.keep_visible(10, 5);
        assert_eq!((list.selected, list.offset), (9, 5));
        list.keep_visible(10, 30);
        assert_eq!((list.selected, list.offset), (9, 0));
        list.move_by(1, 0, 5);
        assert_eq!(list, ListState::default());
        list.move_by(3, 4, 0);
        assert_eq!((list.selected, list.offset), (3, 3));
    }

    #[test]
    fn narrow_screens_drop_the_least_telling_columns() {
        let columns = || {
            vec![
                Column::new("Run", vec!["run-01".into()]).keep(0),
                Column::new("Title", vec!["x".repeat(40)]).flex(),
                Column::new("When", vec!["2h ago".into()]).numeric().keep(1),
                Column::new("Spent", vec!["$0.46".into()]).numeric().keep(2),
            ]
        };
        let wide = fit_columns(columns(), 100, 2);
        assert_eq!(wide.len(), 4);
        assert_eq!(wide[1].width, 40);
        let narrow = fit_columns(columns(), 30, 2);
        let names: Vec<_> = narrow.iter().map(|c| c.heading).collect();
        assert_eq!(names, ["Run", "Title", "Spent"]);
        assert_eq!(total_width(&narrow, 2), 30);
        let tiny = fit_columns(columns(), 10, 2);
        assert!(total_width(&tiny, 2) <= 10);
        assert_eq!(wide[2].cell("2h"), "    2h");
    }

    #[test]
    fn vim_keys_stand_in_for_the_list_keys_only_in_vim_mode() {
        let stand_in = |c: char, back: KeyCode| standard(key(KeyCode::Char(c)), true, back).code;
        assert_eq!(stand_in('j', KeyCode::Esc), KeyCode::Down);
        assert_eq!(stand_in('k', KeyCode::Esc), KeyCode::Up);
        assert_eq!(stand_in('g', KeyCode::Esc), KeyCode::Home);
        assert_eq!(stand_in('G', KeyCode::Esc), KeyCode::End);
        assert_eq!(stand_in('h', KeyCode::Esc), KeyCode::Esc);
        assert_eq!(stand_in('h', KeyCode::Left), KeyCode::Left);
        assert_eq!(stand_in('l', KeyCode::Esc), KeyCode::Enter);
        assert_eq!(stand_in('x', KeyCode::Esc), KeyCode::Char('x'));
        // Shift comes with `G` in most terminals; Ctrl and Alt letters are other keys.
        let shifted = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(standard(shifted, true, KeyCode::Esc).code, KeyCode::End);
        let ctrl = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(standard(ctrl, true, KeyCode::Esc), ctrl);
        // Without vim keys they're letters.
        for c in ['j', 'k', 'g', 'G', 'h', 'l'] {
            let letter = key(KeyCode::Char(c));
            assert_eq!(standard(letter, false, KeyCode::Esc), letter);
        }

        assert_eq!(nav_key(&key(KeyCode::Down), 5), Some(Nav::By(1)));
        assert_eq!(nav_key(&key(KeyCode::Home), 5), Some(Nav::Top));
        assert_eq!(nav_key(&key(KeyCode::End), 5), Some(Nav::Bottom));
        assert_eq!(nav_key(&key(KeyCode::PageUp), 5), Some(Nav::By(-5)));
        assert_eq!(nav_key(&key(KeyCode::Char('j')), 5), None);
    }

    #[test]
    fn slash_starts_the_filter_and_esc_leaves_it() {
        let letter = |c| key(KeyCode::Char(c));
        assert_eq!(filter_key(letter('/'), false), FilterKey::Start);
        assert_eq!(filter_key(letter('x'), false), FilterKey::Nothing);
        assert_eq!(
            filter_key(letter('j'), false),
            FilterKey::Key(key(KeyCode::Down))
        );
        assert_eq!(
            filter_key(key(KeyCode::Esc), false),
            FilterKey::Key(key(KeyCode::Esc))
        );
        // While typing the filter every letter is text, and Esc leaves.
        assert_eq!(filter_key(letter('j'), true), FilterKey::Key(letter('j')));
        assert_eq!(filter_key(letter('/'), true), FilterKey::Key(letter('/')));
        assert_eq!(filter_key(key(KeyCode::Esc), true), FilterKey::Leave);
        assert_eq!(
            filter_key(key(KeyCode::Down), true),
            FilterKey::Key(key(KeyCode::Down))
        );
    }
}
