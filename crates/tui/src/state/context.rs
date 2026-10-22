//! The conversation's context as its CLI last counted it, and how fast its turns fill it.

use std::collections::VecDeque;

use argonauts_core::events::context::ContextUsage;
use time::OffsetDateTime;

/// The conversation session's context, as its CLI last counted it.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextCount {
    pub usage: ContextUsage,
    /// When the count came, by the wall clock.
    pub counted: OffsetDateTime,
}

/// The conversation session's last step as its CLI said it, for a CLI that says how full its
/// context is and nothing of what fills it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LastStep {
    /// When its CLI last said how full the session is, by the wall clock.
    pub at: Option<OffsetDateTime>,
    /// What the step read from cache, of all it read.
    pub cached: u64,
    /// What it wrote, reasoning included.
    pub output: u64,
}

/// How many of the last turns the average is taken over.
const TURNS_KEPT: usize = 8;

/// What each of the conversation's last turns added to its context, from the fill each turn
/// ended at. A turn that ended lower than it began, as after compacting, adds nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextGrowth {
    /// The fill the last turn ended at.
    ended_at: Option<u64>,
    /// The running turn's latest fill.
    latest: Option<u64>,
    /// What each turn added, oldest first.
    added: VecDeque<u64>,
}

impl ContextGrowth {
    pub(super) fn filled(&mut self, used: u64) {
        self.latest = Some(used);
    }

    pub(super) fn turn_ended(&mut self) {
        let Some(end) = self.latest.take() else {
            return;
        };
        if let Some(begin) = self.ended_at
            && end > begin
        {
            self.added.push_back(end - begin);
            if self.added.len() > TURNS_KEPT {
                self.added.pop_front();
            }
        }
        self.ended_at = Some(end);
    }

    /// What a turn adds, averaged over the last few. `None` until one turn has been measured
    /// from the turn before it.
    pub fn per_turn(&self) -> Option<u64> {
        let turns = u64::try_from(self.added.len()).ok().filter(|n| *n > 0)?;
        Some(self.added.iter().sum::<u64>() / turns).filter(|tokens| *tokens > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(growth: &mut ContextGrowth, end: u64) {
        growth.filled(end / 2);
        growth.filled(end);
        growth.turn_ended();
    }

    #[test]
    fn a_turn_adds_what_it_ended_above_the_turn_before() {
        let mut growth = ContextGrowth::default();
        turn(&mut growth, 30_000);
        assert_eq!(
            growth.per_turn(),
            None,
            "the first turn has nothing before it"
        );
        turn(&mut growth, 40_000);
        turn(&mut growth, 48_000);
        assert_eq!(growth.per_turn(), Some(9_000));
    }

    #[test]
    fn a_compacted_turn_adds_nothing_and_the_next_counts_from_it() {
        let mut growth = ContextGrowth::default();
        turn(&mut growth, 900_000);
        turn(&mut growth, 120_000);
        assert_eq!(growth.per_turn(), None);
        turn(&mut growth, 126_000);
        assert_eq!(growth.per_turn(), Some(6_000));
    }

    #[test]
    fn a_turn_with_no_fill_changes_nothing() {
        let mut growth = ContextGrowth::default();
        turn(&mut growth, 10_000);
        growth.turn_ended();
        turn(&mut growth, 14_000);
        assert_eq!(growth.per_turn(), Some(4_000));
    }

    #[test]
    fn only_the_last_turns_count() {
        let mut growth = ContextGrowth::default();
        turn(&mut growth, 1_000);
        turn(&mut growth, 101_000);
        for n in 1..=TURNS_KEPT as u64 {
            turn(&mut growth, 101_000 + n * 2_000);
        }
        assert_eq!(growth.per_turn(), Some(2_000));
    }
}
