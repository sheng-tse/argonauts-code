//! Each behavior's facts: when it was used, how its sessions went on and ended, what failed,
//! what became of its work, its busiest stretches, and the moves agents made between
//! behaviors.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::behaviors::behavior_world::{
    BEHAVIOR_RUN_LENGTH, BehaviorFacts, Burst, Crash, Move,
};
use time::{Duration, OffsetDateTime};

use super::super::read::{Outcome, Recorded, SessionEnded};
use super::{Read, behavior_of};

/// An action this long or longer after the one before it in its session came after a wait.
const WAIT: Duration = Duration::minutes(10);
const HOUR: Duration = Duration::HOUR;
const FIVE_MINUTES: Duration = Duration::minutes(5);

pub(super) fn facts(read: &Read<'_>) -> (Vec<BehaviorFacts>, Vec<Move>) {
    let mut all: BTreeMap<String, BehaviorFacts> = BTreeMap::new();
    if let Some((from, to)) = read.period.before() {
        for recorded in read.labelled.iter().filter(|r| r.at >= from && r.at < to) {
            entry(&mut all, recorded).last_period += 1;
        }
    }
    for recorded in &read.in_period {
        count(entry(&mut all, recorded), recorded, read);
    }
    // When each was last used reads past the period's start, whatever the period holds.
    let mut unknown = all.len();
    for recorded in read.labelled.iter().rev() {
        if unknown == 0 {
            break;
        }
        if let Some(facts) = all.get_mut(&behavior_of(recorded))
            && facts.last_used.is_none()
        {
            facts.last_used = Some(read.day(recorded.at));
            unknown -= 1;
        }
    }
    let mut moves: BTreeMap<(String, String), u32> = BTreeMap::new();
    for session in &read.sessions {
        follow(session, &mut all, &mut moves);
    }
    let by_behavior = group(&read.in_period);
    for (behavior, actions) in &by_behavior {
        if let Some(facts) = all.get_mut(behavior) {
            let times: Vec<OffsetDateTime> = actions.iter().map(|r| r.at).collect();
            facts.busiest_hour = busiest(&times, HOUR);
            facts.most_agents_in_5_minutes = most_agents(actions, FIVE_MINUTES);
            facts.weeks = weeks(actions, read);
            let last_7: BTreeSet<u32> = actions
                .iter()
                .map(|recorded| read.day(recorded.at))
                .filter(|day| *day < 7)
                .collect();
            facts.days_in_last_7 = last_7.len() as u8;
        }
    }
    let mut facts: Vec<BehaviorFacts> = all.into_values().collect();
    for behavior in &mut facts {
        behavior.crashes.sort_by_key(|crash| Reverse(crash.at));
    }
    // Most used first; a tie stays in the order of their words.
    facts.sort_by_key(|behavior| Reverse(behavior.count));
    let mut moves: Vec<Move> = moves
        .into_iter()
        .map(|((from, to), count)| Move { from, to, count })
        .collect();
    moves.sort_by_key(|step| Reverse(step.count));
    (facts, moves)
}

fn entry<'a>(
    all: &'a mut BTreeMap<String, BehaviorFacts>,
    recorded: &Recorded,
) -> &'a mut BehaviorFacts {
    let behavior = behavior_of(recorded);
    all.entry(behavior.clone())
        .or_insert_with(|| BehaviorFacts {
            behavior,
            ..BehaviorFacts::default()
        })
}

/// What one action of the period adds to its behavior's facts.
fn count(facts: &mut BehaviorFacts, recorded: &Recorded, read: &Read<'_>) {
    let day = read.day(recorded.at);
    facts.count += 1;
    if recorded.failed {
        facts.failed += 1;
        facts.last_failed = Some(facts.last_failed.map_or(day, |last| last.min(day)));
    }
    match recorded.outcome {
        Some(Outcome::SentBack) => facts.sent_back += 1,
        Some(Outcome::Dropped) => {
            facts.dropped += 1;
            let dropped = read.day(recorded.settled_at.unwrap_or(recorded.at));
            facts.last_dropped = Some(facts.last_dropped.map_or(dropped, |l| l.min(dropped)));
        }
        Some(Outcome::Merged) | None => {}
    }
}

/// What one session's order says: the waits in it, its runs of one behavior, the moves from
/// one behavior to the next, and the behavior it ended on.
fn follow(
    session: &[&Recorded],
    all: &mut BTreeMap<String, BehaviorFacts>,
    moves: &mut BTreeMap<(String, String), u32>,
) {
    for pair in session.windows(2) {
        if pair[1].behavior.is_labelled()
            && pair[1].at - pair[0].at >= WAIT
            && let Some(facts) = all.get_mut(&behavior_of(pair[1]))
        {
            facts.after_a_wait += 1;
        }
    }
    let labelled: Vec<String> = session
        .iter()
        .filter(|recorded| recorded.behavior.is_labelled())
        .map(|recorded| behavior_of(recorded))
        .collect();
    let mut runs = labelled.chunk_by(|a, b| a == b).peekable();
    while let Some(run) = runs.next() {
        let behavior = &run[0];
        if run.len() >= BEHAVIOR_RUN_LENGTH
            && let Some(facts) = all.get_mut(behavior)
        {
            facts.runs_in_a_row += 1;
            facts.longest_run = facts.longest_run.max(run.len() as u32);
        }
        if let Some(next) = runs.peek() {
            *moves
                .entry((behavior.clone(), next[0].clone()))
                .or_default() += 1;
        }
    }
    let Some(last) = session.iter().rev().find(|r| r.behavior.is_labelled()) else {
        return;
    };
    let Some(how) = last.ended else {
        return;
    };
    let Some(facts) = all.get_mut(&behavior_of(last)) else {
        return;
    };
    facts.session_ends += 1;
    match how {
        SessionEnded::Stopped => facts.stopped += 1,
        SessionEnded::Failed => facts.cli_failed += 1,
        SessionEnded::Crashed => {
            facts.cli_failed += 1;
            facts.crashes.push(Crash {
                at: last.ended_at.unwrap_or(last.at),
                agent: last.cli.clone(),
                model: last.model.clone(),
            });
        }
        SessionEnded::Completed => {}
    }
}

fn group<'a>(actions: &[&'a Recorded]) -> BTreeMap<String, Vec<&'a Recorded>> {
    let mut by: BTreeMap<String, Vec<&Recorded>> = BTreeMap::new();
    for recorded in actions {
        by.entry(behavior_of(recorded)).or_default().push(recorded);
    }
    by
}

/// The most of `times`, oldest first, within `within` of the first of them, and when that
/// stretch began: the earliest such stretch when two hold as many.
fn busiest(times: &[OffsetDateTime], within: Duration) -> Option<Burst> {
    let mut best: Option<Burst> = None;
    let mut first = 0;
    for (last, at) in times.iter().enumerate() {
        while *at - times[first] > within {
            first += 1;
        }
        let count = (last - first + 1) as u32;
        if best.is_none_or(|best| count > best.count) {
            best = Some(Burst {
                count,
                at: times[first],
            });
        }
    }
    best
}

/// The most sessions that took one of `actions`, oldest first, within `within` of the first.
fn most_agents(actions: &[&Recorded], within: Duration) -> Option<Burst> {
    let mut best: Option<Burst> = None;
    for (first, start) in actions.iter().enumerate() {
        let agents: BTreeSet<&str> = actions[first..]
            .iter()
            .take_while(|recorded| recorded.at - start.at <= within)
            .map(|recorded| recorded.session.as_str())
            .collect();
        let count = agents.len() as u32;
        if best.is_none_or(|best| count > best.count) {
            best = Some(Burst {
                count,
                at: start.at,
            });
        }
    }
    best
}

/// Its actions in each full week of the period, counted back from the end, latest first.
fn weeks(actions: &[&Recorded], read: &Read<'_>) -> Vec<u32> {
    let mut weeks = vec![0; (read.span / 7) as usize];
    for recorded in actions {
        if let Some(week) = weeks.get_mut((read.day(recorded.at) / 7) as usize) {
            *week += 1;
        }
    }
    weeks
}
