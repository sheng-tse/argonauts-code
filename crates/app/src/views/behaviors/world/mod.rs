//! What the Behaviors world draws besides the dots, worked out from the same records as the
//! map: each behavior's facts, the moves between them, and the crew's as a whole. The screen
//! holds the world's rules; these say only what happened. Every day count goes back from the
//! period's end on your own clock, so a day is the one you lived it on.

mod crew;
mod models;
mod per_behavior;
pub(crate) mod storm;

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::behaviors::behavior_world::WorldFacts;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_tui::base::format::WallClock;
use time::{Date, Duration, OffsetDateTime};

use super::read::Recorded;

/// The stretch of time the map covers, and the clock its days are counted on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Period {
    /// Now, for the current one.
    pub end: OffsetDateTime,
    /// The days it spans, its last among them. `None` for all time.
    pub days: Option<i64>,
    clock: WallClock,
}

impl Period {
    pub fn new(end: OffsetDateTime, days: Option<i64>, clock: WallClock) -> Self {
        Self { end, days, clock }
    }

    /// Days back from the end's day to the day of `at`, 0 for the end's own.
    pub fn day(&self, at: OffsetDateTime) -> u32 {
        let back = (self.end_day() - self.clock.to_local(at).date()).whole_days();
        u32::try_from(back).unwrap_or(0)
    }

    /// Midnight starting its first day, or `None` for all time.
    pub fn start(&self) -> Option<OffsetDateTime> {
        self.days.map(|days| self.midnight(days - 1))
    }

    /// The period of the same length just before it.
    pub fn before(&self) -> Option<(OffsetDateTime, OffsetDateTime)> {
        let days = self.days?;
        Some((self.midnight(2 * days - 1), self.midnight(days - 1)))
    }

    pub fn local(&self, at: OffsetDateTime) -> OffsetDateTime {
        self.clock.to_local(at)
    }

    fn end_day(&self) -> Date {
        self.clock.to_local(self.end).date()
    }

    /// Midnight starting the day `back` days before the end's, in the offset of that day.
    fn midnight(&self, back: i64) -> OffsetDateTime {
        let day = self.end_day() - Duration::days(back);
        let offset = self
            .clock
            .to_local(self.end - Duration::days(back))
            .offset();
        day.midnight().assume_offset(offset)
    }

    fn holds(&self, at: OffsetDateTime) -> bool {
        at <= self.end && self.start().is_none_or(|start| at >= start)
    }
}

/// The world's facts from `all`, every action recorded, for `period`. A model's line comes
/// from its name in `names`, and the storm's word from `zone`, the time zone's name.
pub(crate) fn facts(
    all: &[Recorded],
    period: &Period,
    names: &DisplayNames,
    zone: Option<&str>,
) -> WorldFacts {
    let read = Read::of(all, period);
    let (behaviors, moves) = per_behavior::facts(&read);
    let models = models::facts(&read, names);
    WorldFacts {
        end: period.local(period.end),
        behaviors,
        moves,
        crew: crew::facts(&read, zone),
        models: models.models,
        pairs: models.pairs,
        mixed_runs: models.mixed_runs,
        wonders: Vec::new(),
    }
}

/// The records sorted as the facts read them.
struct Read<'a> {
    period: &'a Period,
    /// Every labelled action up to the period's end, of any time.
    labelled: Vec<&'a Recorded>,
    /// Those in the period, oldest first.
    in_period: Vec<&'a Recorded>,
    /// The period's sessions, each with every action it took in the period in its order,
    /// labelled or not.
    sessions: Vec<Vec<&'a Recorded>>,
    /// The days anything at all was done on, labelled or not, of any time.
    active_days: BTreeSet<u32>,
    /// The span in days the period's weeks and quiet days are counted over: its own length, or
    /// for all time, back to the first day anything was recorded.
    span: u32,
}

impl<'a> Read<'a> {
    fn of(all: &'a [Recorded], period: &'a Period) -> Self {
        let mut up_to_end: Vec<&Recorded> = all.iter().filter(|r| r.at <= period.end).collect();
        up_to_end.sort_by_key(|recorded| recorded.at);
        let mut sessions: BTreeMap<&str, Vec<&Recorded>> = BTreeMap::new();
        for recorded in up_to_end.iter().filter(|r| period.holds(r.at)) {
            sessions
                .entry(recorded.session.as_str())
                .or_default()
                .push(recorded);
        }
        let labelled: Vec<&Recorded> = up_to_end
            .iter()
            .copied()
            .filter(|recorded| recorded.behavior.is_labelled())
            .collect();
        let in_period = labelled
            .iter()
            .copied()
            .filter(|recorded| period.holds(recorded.at))
            .collect();
        let active_days: BTreeSet<u32> = up_to_end.iter().map(|r| period.day(r.at)).collect();
        let span = match period.days {
            Some(days) => u32::try_from(days).unwrap_or(0),
            None => active_days.last().map_or(1, |first| first + 1),
        };
        let mut sessions: Vec<Vec<&Recorded>> = sessions.into_values().collect();
        sessions.sort_by_key(|actions| actions.first().map(|first| first.at));
        Self {
            period,
            labelled,
            in_period,
            sessions,
            active_days,
            span,
        }
    }

    fn day(&self, at: OffsetDateTime) -> u32 {
        self.period.day(at)
    }
}

/// What a labelled action counts under: its label's words, as the label map names them.
fn behavior_of(recorded: &Recorded) -> String {
    recorded.behavior.words()
}

#[cfg(test)]
mod made_up;
#[cfg(test)]
mod tests;
