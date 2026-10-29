//! What the crew did as a whole: how long it has been idle, its quiet days, its night work and
//! the months before the period.

use std::collections::BTreeMap;

use argonauts_core::behavior_world::{CrewFacts, MonthActions, QuietStretch};

use super::{Read, storm};

/// Night is from 10:00 PM to 5:00 AM on your clock.
const NIGHT_FROM: u8 = 22;
const NIGHT_TO: u8 = 5;

pub(super) fn facts(read: &Read<'_>, zone: Option<&str>) -> CrewFacts {
    let idle_days = read.active_days.first().copied().unwrap_or(0);
    let quiet = quiet_days(read, idle_days);
    CrewFacts {
        idle_days,
        quiet_days: quiet.len() as u32,
        quiet_stretches: stretches(&quiet),
        night_actions: read
            .in_period
            .iter()
            .filter(|recorded| {
                let hour = read.period.local(recorded.at).hour();
                !(NIGHT_TO..NIGHT_FROM).contains(&hour)
            })
            .count() as u32,
        earlier_months: earlier_months(read),
        storm: storm::word(zone),
    }
}

/// The period's days with nothing done, latest first: none among the idle days since the last
/// action, which the ice cap tells, nor before the first action ever recorded.
fn quiet_days(read: &Read<'_>, idle_days: u32) -> Vec<u32> {
    let Some(first) = read.active_days.last().copied() else {
        return Vec::new();
    };
    (idle_days..read.span.min(first + 1))
        .filter(|day| !read.active_days.contains(day))
        .collect()
}

/// Each stretch of 2 or more quiet days in a row, latest first.
fn stretches(quiet: &[u32]) -> Vec<QuietStretch> {
    let mut stretches = Vec::new();
    let mut start = 0;
    while start < quiet.len() {
        let mut end = start;
        while end + 1 < quiet.len() && quiet[end + 1] == quiet[end] + 1 {
            end += 1;
        }
        if end > start {
            stretches.push(QuietStretch {
                ends: quiet[start],
                days: (end - start + 1) as u32,
            });
        }
        start = end + 1;
    }
    stretches
}

/// The labelled actions of each month before the period, on your clock, the month just
/// before it first.
fn earlier_months(read: &Read<'_>) -> Vec<MonthActions> {
    let Some(start) = read.period.start() else {
        return Vec::new();
    };
    let mut months: BTreeMap<(i32, u8), u32> = BTreeMap::new();
    for recorded in read.labelled.iter().filter(|recorded| recorded.at < start) {
        let local = read.period.local(recorded.at);
        *months
            .entry((local.year(), u8::from(local.month())))
            .or_default() += 1;
    }
    months
        .into_iter()
        .rev()
        .map(|((year, month), actions)| MonthActions {
            month: format!("{year:04}-{month:02}"),
            actions,
        })
        .collect()
}
