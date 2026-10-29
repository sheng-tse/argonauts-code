//! The list under the map: the compared group's behaviors, the selected one lit on the map, a
//! behavior's actions newest first with the one the keys are on ringed. Each list scrolls by the
//! rows it draws in the room it has, and keeps the cursor in view.

use argonauts_core::behavior_map::{BehaviorRow, Comparison, Placing};
use ratatui::text::Span;

use super::window::Window;
use super::{At, BehaviorsTab, summary, words};
use crate::kit::Drawn;

/// A screen reader hears this many of the list's behaviors.
pub(super) const LIST_ROWS: usize = 6;

impl BehaviorsTab {
    /// Whether `at` is still a place on the map now taken.
    pub(super) fn still_there(&self, at: At) -> bool {
        let rows = self.comparison().map_or(0, |c| c.rows.len());
        match at {
            At::Map => true,
            At::List(row) => row < rows,
            At::Actions { row, action } | At::Card { row, action } => {
                row < rows && action < self.actions_of(row).len()
            }
        }
    }

    pub(super) fn comparison(&self) -> Option<&Comparison> {
        self.map.as_ref()?.comparisons.get(self.compared?)
    }

    pub(super) fn row(&self, row: usize) -> Option<&BehaviorRow> {
        self.comparison()?.rows.get(row)
    }

    /// The actions Enter lists for a behavior: the compared group's, newest first.
    pub(super) fn actions_of(&self, row: usize) -> Vec<usize> {
        let (Some(map), Some(group), Some(row)) = (&self.map, self.compared, self.row(row)) else {
            return Vec::new();
        };
        let mut mine: Vec<usize> = row
            .lit
            .iter()
            .copied()
            .filter(|at| map.actions.get(*at).is_some_and(|a| a.group == group))
            .collect();
        // A row of Summaries lists its actions nearest first, as it lights them; a label's
        // actions come newest first.
        if map.placing == Placing::Labels {
            mine.sort_by(|a, b| map.actions[*b].at.cmp(&map.actions[*a].at).then(a.cmp(b)));
        }
        mine
    }

    /// The lists start again from their tops.
    pub(super) fn scroll_to_top(&mut self) {
        self.first_row.set(0);
        self.first_action.set(0);
    }

    /// A click on the map opens the card of the dot nearest the pointer.
    pub(super) fn open_pointed_card(&mut self) -> bool {
        let (Some(at), Some(map)) = (self.hovered(), &self.map) else {
            return false;
        };
        let group = map.actions[at].group;
        let behavior = map.actions[at].behavior.clone();
        self.compared.get_or_insert(group);
        let Some(row) = self
            .comparison()
            .and_then(|c| c.rows.iter().position(|r| r.behavior == behavior))
        else {
            return false;
        };
        let Some(action) = self.actions_of(row).iter().position(|a| *a == at) else {
            return false;
        };
        self.pointed = None;
        self.at = At::Card { row, action };
        true
    }

    /// What lights on the map: the selected behavior's dots, or its actions.
    pub fn lit(&self) -> Vec<usize> {
        match self.at {
            At::List(row) => self.row(row).map(|r| r.lit.clone()).unwrap_or_default(),
            At::Actions { row, .. } => self.actions_of(row),
            _ => Vec::new(),
        }
    }

    /// The action ringed on the map.
    pub fn ringed(&self) -> Option<usize> {
        match self.at {
            At::Actions { row, action } => self.actions_of(row).get(action).copied(),
            _ => None,
        }
    }

    /// What the keys rest on, said in the pointer's place: the lit behavior, or the ringed
    /// action. Nothing while the keys are on the map.
    pub(super) fn cursor_line(&self, look: &words::Look<'_>) -> Option<Vec<Span<'static>>> {
        match self.at {
            At::List(row) => Some(look.lit_line(self.row(row)?)),
            At::Actions { row, action } => {
                let at = *self.actions_of(row).get(action)?;
                Some(look.ringed_line(look.map.actions.get(at)?))
            }
            _ => None,
        }
    }

    /// The rows under the map's headline, in `room` rows: the most common behaviors while
    /// nothing is compared, else the compared group's list or a behavior's actions, each a
    /// heading over a window that scrolls with the cursor. A heading gives way where its rows
    /// would leave the window too few to show the cursor's row and say what it hides.
    pub(super) fn listed(&self, look: &words::Look<'_>, room: usize) -> Drawn {
        let mut out = Drawn::new();
        // Summaries are offered on the map itself; in the list their rows go to the list.
        let offer = match look.map.offer_summaries && self.at == At::Map {
            true => look.offer(),
            false => Vec::new(),
        };
        // The offer and the blank above it.
        let offer_rows = match offer.len() {
            0 => 0,
            rows => rows + 1,
        };
        match (self.at, self.comparison()) {
            (_, None) => {
                for line in summary::most_common_rows(look.kit, &self.most_common, look.width, room)
                {
                    out.push(line);
                }
            }
            (At::Actions { row, action }, Some(_)) => {
                let actions = self.actions_of(row);
                let behavior = self.row(row).map_or("", |r| r.behavior.as_str());
                let heading = look.actions_heading(behavior, actions.len());
                if keeps_heading(heading.len(), actions.len(), room) {
                    for line in heading {
                        out.push(line);
                    }
                }
                let window = Window::follow(
                    actions.len(),
                    room.saturating_sub(out.height()),
                    self.first_action.get(),
                    action,
                );
                self.first_action.set(window.first);
                push_window(
                    &mut out,
                    look,
                    window,
                    look.actions(&actions, action, window),
                );
            }
            (at, Some(comparison)) => {
                let selected = match at {
                    At::List(row) => Some(row),
                    _ => None,
                };
                if let Some(why) = &comparison.why_not
                    && comparison.rows.is_empty()
                {
                    out.push(vec![look.kit.dim(format!(" {why}."))]);
                }
                let rows = comparison.rows.len();
                if rows > 0 && keeps_heading(1, rows, room.saturating_sub(offer_rows)) {
                    out.push(look.list_heading(comparison));
                }
                // On the map the list shows from its top; in it, the window follows the cursor.
                let window = Window::follow(
                    comparison.rows.len(),
                    room.saturating_sub(out.height() + offer_rows),
                    selected.map_or(0, |_| self.first_row.get()),
                    selected.unwrap_or(0),
                );
                self.first_row.set(window.first);
                push_window(
                    &mut out,
                    look,
                    window,
                    look.list(comparison, selected, window),
                );
            }
        }
        if !offer.is_empty() {
            out.push(Vec::new());
            for row in offer {
                out.push(row);
            }
        }
        out
    }
}

/// Whether a heading of `heading` rows leaves a list of `rows` the rows it needs in `room`: every
/// row, or two where some are hidden, the cursor's and the one that counts the rest.
fn keeps_heading(heading: usize, rows: usize, room: usize) -> bool {
    room >= heading + rows.min(2)
}

/// A window's rows between its marks.
fn push_window(
    out: &mut Drawn,
    look: &words::Look<'_>,
    window: Window,
    rows: Vec<Vec<Span<'static>>>,
) {
    if let Some(above) = window.above_row(look.kit, " ") {
        out.push(above);
    }
    for row in rows {
        out.push(row);
    }
    if let Some(below) = window.below_row(look.kit, " ") {
        out.push(below);
    }
}
