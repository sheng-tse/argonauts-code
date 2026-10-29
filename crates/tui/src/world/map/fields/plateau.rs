//! Plateaus: the behaviors done about as often every week. Their mountains stand as flat-topped
//! mesas at their counts, so the height still says the count and the shape says the weeks were
//! alike.

use super::GroundFacts;

/// The fewest actions a plateau needs, and how far a week may stray from the weekly average.
const PLATEAU_ACTIONS: u32 = 30;
const PLATEAU_SPREAD: f64 = 0.25;

/// Of its peak's height: where a plateau's flat top ends, and where its cliffs' foot is.
pub const MESA_TOP: f64 = 0.13;
pub const MESA_FOOT: f64 = 0.07;

/// A steady habit: its behavior and its actions in each full week, the latest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plateau {
    pub behavior: usize,
    pub weeks: Vec<u32>,
}

impl Plateau {
    /// Its fewest and most actions in a week.
    pub fn range(&self) -> (u32, u32) {
        let low = self.weeks.iter().copied().min().unwrap_or(0);
        let high = self.weeks.iter().copied().max().unwrap_or(0);
        (low, high)
    }
}

/// The behaviors with 30 or more actions whose every full week lies within a quarter of their
/// weekly average, over two weeks or more.
pub fn plateaus(facts: &GroundFacts, count: &[u32]) -> Vec<Plateau> {
    facts
        .behaviors
        .iter()
        .enumerate()
        .filter(|(at, days)| count[*at] >= PLATEAU_ACTIONS && days.weeks.len() >= 2)
        .filter(|(_, days)| {
            let average = days.weeks.iter().map(|week| f64::from(*week)).sum::<f64>()
                / days.weeks.len() as f64;
            average > 0.0
                && days
                    .weeks
                    .iter()
                    .all(|week| (f64::from(*week) - average).abs() <= PLATEAU_SPREAD * average)
        })
        .map(|(behavior, days)| Plateau {
            behavior,
            weeks: days.weeks.clone(),
        })
        .collect()
}
