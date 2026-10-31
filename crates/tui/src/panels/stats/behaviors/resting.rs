//! Where the pointer rests while the keys are in the map: on its busiest place, so the map shows
//! it has the keys and the line under it says what is there, as it does for the mouse.

use argonauts_core::behaviors::behavior_map::BehaviorMap;

use super::braille::Marks;
use super::layout::Fit;
use super::{At, BehaviorsTab, Level, Stop};
use crate::world::Pointed;

/// The action at the map's busiest place among `group`'s, or every group's: the braille cell
/// with the most actions in it and the eight round it, a tie going to the cell with more in
/// itself and then to the first. The action is the first of that cell's in the map's order.
pub(super) fn busiest(map: &BehaviorMap, fit: Fit, group: Option<usize>) -> Option<usize> {
    let (columns, rows) = (fit.columns.max(1), fit.rows.max(1));
    let cell_of = |x: f32, y: f32| {
        let column = ((x * (columns * 2) as f32) as usize).min(columns * 2 - 1) / 2;
        let row = (((1.0 - y) * (rows * 4) as f32) as usize).min(rows * 4 - 1) / 4;
        row * columns + column
    };
    let chosen = |group_of: usize| group.is_none_or(|g| g == group_of);
    let mut counts = vec![0u32; columns * rows];
    for action in map.actions.iter().filter(|a| chosen(a.group)) {
        counts[cell_of(action.x, action.y)] += 1;
    }
    let round = |at: usize| {
        let (row, column) = (at / columns, at % columns);
        let mut sum = 0;
        for r in row.saturating_sub(1)..=(row + 1).min(rows - 1) {
            for c in column.saturating_sub(1)..=(column + 1).min(columns - 1) {
                sum += counts[r * columns + c];
            }
        }
        sum
    };
    let best = (0..counts.len())
        .filter(|at| counts[*at] > 0)
        .max_by_key(|at| (round(*at), counts[*at], std::cmp::Reverse(*at)))?;
    map.actions
        .iter()
        .position(|a| chosen(a.group) && cell_of(a.x, a.y) == best)
}

impl BehaviorsTab {
    /// What braille marks among the dots: the list's lit dots and ring, and the pointer's dot.
    pub(super) fn marks<'a>(&self, lit: &'a [usize]) -> Marks<'a> {
        Marks {
            lit,
            ringed: self.ringed(),
            pointer: self.hovered(),
        }
    }

    /// The keys are on the map itself, at the map's level, nothing picked in the list.
    fn keys_on_map(&self) -> bool {
        self.stop == Stop::Picture && self.at == At::Map && self.level == Level::Map
    }

    /// The pointer goes to the busiest place of the group compared, or of them all, while the
    /// keys are on the map, and lets go of a place the keys gave it once they leave.
    pub(super) fn rest_pointer(&mut self) {
        if !self.keys_on_map() {
            self.release_pointer();
            return;
        }
        let place = self
            .map
            .as_ref()
            .and_then(|map| busiest(map, self.fit(), self.compared));
        self.pointed = place
            .and_then(|at| u32::try_from(at).ok())
            .map(Pointed::Action);
        self.resting = self.pointed.is_some();
    }

    /// The keys left the map: the place they gave the pointer goes, and the mouse's stays.
    pub(super) fn release_pointer(&mut self) {
        if std::mem::take(&mut self.resting) {
            self.pointed = None;
        }
    }
}
