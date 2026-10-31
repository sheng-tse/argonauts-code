//! A list in fewer rows than it holds: the rows it shows, kept around the cursor, with a row
//! above saying how many it passed and a row below saying how many are left. The behaviors under
//! the map, a behavior's actions and the legend beside the map all scroll by this one rule.

use std::ops::Range;

use ratatui::text::Span;

use crate::base::kit::Kit;

/// Which of a list's rows show in its room. The marks take rows of the room: a list that runs
/// on shows one row fewer at the top and the bottom, and two fewer in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Window {
    pub first: usize,
    pub shown: usize,
    /// Rows passed above the first shown, and rows left below the last.
    pub above: usize,
    pub below: usize,
    /// Both counts on the one row under the cursor's, where the room has two rows.
    one_mark: bool,
}

impl Window {
    /// The window over `rows` in `room` rows, from where it last started, `first`, moved only
    /// as far as keeps `cursor` in view. A room of two rows holds the cursor's row over one mark
    /// that counts both ways; a room of one holds the cursor's row alone.
    pub fn follow(rows: usize, room: usize, first: usize, cursor: usize) -> Window {
        if rows <= room {
            return Window {
                first: 0,
                shown: rows,
                above: 0,
                below: 0,
                one_mark: false,
            };
        }
        let cursor = cursor.min(rows - 1);
        if room < 3 {
            let (above, below) = match room {
                2 => (cursor, rows - 1 - cursor),
                _ => (0, 0),
            };
            return Window {
                first: cursor,
                shown: room.min(1),
                above,
                below,
                one_mark: true,
            };
        }
        // Past `rows + 1 - room` the list would end with rows to spare.
        let mut first = first.min(cursor).min(rows + 1 - room);
        while cursor >= first + shown_from(rows, room, first) {
            first += 1;
        }
        let shown = shown_from(rows, room, first);
        Window {
            first,
            shown,
            above: first,
            below: rows - (first + shown),
            one_mark: false,
        }
    }

    pub fn rows(&self) -> Range<usize> {
        self.first..self.first + self.shown
    }

    /// `↑  3 more above`, where the window passed rows, after `inset`.
    pub fn above_row(&self, kit: &Kit<'_>, inset: &str) -> Option<Vec<Span<'static>>> {
        (self.above > 0 && !self.one_mark).then(|| vec![kit.dim(format!("{inset}{}", self.up()))])
    }

    /// `↓  2 more below`, where rows are left, after `inset`. In a room of two rows it says
    /// what's above too: `↑  3 more above · ↓  2 more below`.
    pub fn below_row(&self, kit: &Kit<'_>, inset: &str) -> Option<Vec<Span<'static>>> {
        let up = (self.one_mark && self.above > 0).then(|| self.up());
        let down = (self.below > 0).then(|| format!("↓  {} more below", self.below));
        let said: Vec<String> = up.into_iter().chain(down).collect();
        (!said.is_empty()).then(|| vec![kit.dim(format!("{inset}{}", said.join(" · ")))])
    }

    fn up(&self) -> String {
        format!("↑  {} more above", self.above)
    }

    /// The rows the marks take.
    #[cfg(test)]
    fn marks(&self) -> usize {
        match self.one_mark {
            true => usize::from(self.above + self.below > 0),
            false => usize::from(self.above > 0) + usize::from(self.below > 0),
        }
    }
}

/// How many rows show from `first` in `room`, where `rows` run on past it.
fn shown_from(rows: usize, room: usize, first: usize) -> usize {
    match first {
        0 => room - 1,
        _ if first + room > rows => rows - first,
        _ => room - 2,
    }
}

#[cfg(test)]
mod tests {
    use super::Window;

    /// However many rows and however small the room, the window never takes more rows than
    /// the room, marks included; the cursor always shows; every row is shown or counted from a
    /// room of two rows up.
    #[test]
    fn the_window_keeps_to_its_room_and_the_cursor_in_view() {
        for rows in 1..14 {
            for room in 1..14 {
                for cursor in 0..rows {
                    for first in 0..rows {
                        let window = Window::follow(rows, room, first, cursor);
                        let case = format!("{rows} rows, room {room}, from {first} to {cursor}");
                        assert!(window.shown + window.marks() <= room, "{case}: {window:?}");
                        assert!(window.rows().contains(&cursor), "{case}: {window:?}");
                        if room >= 2 || rows <= room {
                            let counted = window.above + window.shown + window.below;
                            assert_eq!(counted, rows, "{case}: {window:?}");
                        }
                    }
                }
            }
        }
    }

    /// Eight rows in six: five and the mark below at the top, a mark either side of four in
    /// between, the mark above and five at the end; moving back up keeps the window still until
    /// the cursor reaches its top.
    #[test]
    fn the_window_moves_only_as_far_as_the_cursor_needs() {
        let at = |first, cursor| {
            let window = Window::follow(8, 6, first, cursor);
            (window.above, window.rows(), window.below)
        };
        assert_eq!(at(0, 4), (0, 0..5, 3));
        assert_eq!(at(0, 5), (2, 2..6, 2));
        assert_eq!(at(2, 7), (3, 3..8, 0));
        assert_eq!(at(3, 4), (3, 3..8, 0));
        assert_eq!(at(3, 2), (2, 2..6, 2));
        assert_eq!(at(2, 0), (0, 0..5, 3));
    }

    /// Twelve rows in two: the cursor's row and one mark counting the rows on both sides.
    #[test]
    fn two_rows_hold_the_cursor_and_one_mark_for_both_ways() {
        let window = Window::follow(12, 2, 0, 5);
        assert_eq!((window.above, window.rows(), window.below), (5, 5..6, 6));
        assert_eq!(window.marks(), 1);
        assert_eq!(window.up(), "↑  5 more above");
        let at_the_top = Window::follow(12, 2, 0, 0);
        assert_eq!(
            (at_the_top.above, at_the_top.below, at_the_top.marks()),
            (0, 11, 1)
        );
    }
}
