use std::collections::BTreeMap;
use std::fmt::Write as _;

use argonauts_core::behavior_world::WorldFacts;
use argonauts_core::display_names::DisplayNames;
use time::Duration;
use time::macros::datetime;

use super::super::read::{Outcome, SessionEnded};
use super::made_up::{MadeUp, crew};

/// What the generator dealt: the period's labelled actions, its sessions, the sum of their
/// times in milliseconds from the period's first midnight, the failed actions, the actions of
/// each outcome (none, merged, sent back, dropped) and how the sessions ended (completed,
/// stopped, crashed).
fn dealt(made_up: &MadeUp) -> (usize, usize, i64, usize, [usize; 4], [usize; 3]) {
    let first = datetime!(2026-09-01 00:00 UTC);
    let in_period: Vec<_> = made_up
        .records
        .iter()
        .filter(|r| r.behavior.is_labelled() && r.at >= first)
        .collect();
    let mut outcomes = [0; 4];
    let mut sessions: BTreeMap<&str, Option<SessionEnded>> = BTreeMap::new();
    for recorded in &in_period {
        outcomes[match recorded.outcome {
            None => 0,
            Some(Outcome::Merged) => 1,
            Some(Outcome::SentBack) => 2,
            Some(Outcome::Dropped) => 3,
        }] += 1;
        sessions.insert(&recorded.session, recorded.ended);
    }
    let mut ends = [0; 3];
    for ended in sessions.values() {
        ends[match ended {
            Some(SessionEnded::Stopped) => 1,
            Some(SessionEnded::Crashed) => 2,
            _ => 0,
        }] += 1;
    }
    let millis = in_period
        .iter()
        .map(|r| (r.at - first).whole_milliseconds() as i64)
        .sum();
    let failed = in_period.iter().filter(|r| r.failed).count();
    (
        in_period.len(),
        sessions.len(),
        millis,
        failed,
        outcomes,
        ends,
    )
}

#[test]
fn each_made_up_crew_is_dealt_the_same_every_run() {
    assert_eq!(
        dealt(&crew('R')),
        (
            961,
            68,
            1_585_984_348_491,
            163,
            [241, 496, 115, 109],
            [55, 10, 3]
        )
    );
    assert_eq!(
        dealt(&crew('S')),
        (
            990,
            79,
            1_652_288_738_959,
            185,
            [130, 650, 179, 31],
            [61, 8, 10]
        )
    );
    assert_eq!(
        dealt(&crew('T')),
        (775, 55, 905_150_848_980, 7, [170, 376, 21, 208], [42, 8, 5])
    );
    assert_eq!(
        dealt(&crew('U')),
        (
            911,
            70,
            1_487_914_168_344,
            161,
            [235, 421, 135, 120],
            [48, 16, 6]
        )
    );
}

fn world_of(made_up: &MadeUp) -> WorldFacts {
    super::facts(
        &made_up.records,
        &made_up.period,
        &DisplayNames::default(),
        None,
    )
}

/// The marks of nature the world's rules raise from a crew's facts, each as its kind, its
/// behavior and its number, sorted. Every basin a behavior could hold is listed, where the map
/// draws two at most.
fn marks(made_up: &MadeUp, world: &WorldFacts) -> Vec<String> {
    let mut marks = Vec::new();
    let mut mark =
        |kind: &str, behavior: &str, n: u32| marks.push(format!("{kind} | {behavior} | {n}"));
    let share = |part: u32, whole: u32| f64::from(part) / f64::from(whole.max(1));
    let mut ends = Vec::new();
    for facts in world.behaviors.iter().filter(|facts| facts.count > 0) {
        let b = facts.behavior.as_str();
        if let Some(hour) = facts.busiest_hour.filter(|hour| hour.count >= 25) {
            let category = [25, 40, 60, 90, 130]
                .iter()
                .filter(|at| hour.count >= **at)
                .count();
            mark(&format!("hurricane {category}"), b, hour.count);
        }
        if let Some(wave) = facts
            .most_agents_in_5_minutes
            .filter(|wave| wave.count >= 6)
        {
            mark("tsunami", b, wave.count);
        }
        if facts.failed >= 3 && share(facts.failed, facts.count) >= 0.1 {
            let smoking = facts.last_failed.is_some_and(|day| day < 7);
            mark(
                if smoking {
                    "volcano smoking"
                } else {
                    "volcano quiet"
                },
                b,
                facts.failed,
            );
        }
        if facts.sent_back >= 5 && share(facts.sent_back, facts.count) >= 0.25 {
            mark("flood", b, facts.sent_back);
        }
        if facts.dropped >= 5 && share(facts.dropped, facts.count) >= 0.25 {
            let burning = facts.last_dropped.is_some_and(|day| day < 7);
            mark(
                if burning {
                    "wildfire burning"
                } else {
                    "burn scar"
                },
                b,
                facts.dropped,
            );
        }
        if facts.runs_in_a_row > 0 {
            mark("tornado", b, facts.runs_in_a_row);
        }
        if facts.stopped >= 2 {
            mark("thunderstorm", b, facts.stopped);
        }
        if !facts.crashes.is_empty() {
            let shaking = facts
                .crashes
                .iter()
                .any(|crash| world.end - crash.at <= Duration::DAY);
            mark(
                if shaking { "earthquake" } else { "fault line" },
                b,
                facts.crashes.len() as u32,
            );
        }
        if facts.after_a_wait >= 5 && share(facts.after_a_wait, facts.count) >= 0.2 {
            mark("swamp", b, facts.after_a_wait);
        }
        let weeks = facts.weeks.len() as f64;
        let average = f64::from(facts.weeks.iter().sum::<u32>()) / weeks;
        if facts.count >= 30
            && facts.weeks.len() >= 2
            && facts
                .weeks
                .iter()
                .all(|week| (f64::from(*week) - average).abs() <= 0.25 * average)
        {
            mark("plateau", b, facts.count);
        }
        if facts.session_ends >= 6 {
            ends.push((facts.session_ends, b));
        }
    }
    ends.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    for (n, b) in ends.into_iter().take(3) {
        mark("lake", b, n);
    }
    let count = |b: &str| world.behavior(b).map_or(0, |facts| facts.count);
    let mut into: BTreeMap<&str, u32> = BTreeMap::new();
    for step in world
        .moves
        .iter()
        .filter(|m| m.from != m.to && m.count >= 8)
    {
        if count(&step.from) > 0 && count(&step.to) > 0 {
            *into.entry(step.to.as_str()).or_default() += 1;
        }
    }
    let basins: Vec<(u32, &str)> = into
        .into_iter()
        .filter(|(_, n)| *n >= 3)
        .map(|(b, n)| (n, b))
        .collect();
    for (n, b) in basins {
        mark("basin", b, n);
    }
    for facts in world
        .behaviors
        .iter()
        .filter(|f| f.count == 0 && f.last_period > 0)
    {
        mark("atoll", &facts.behavior, 1);
    }
    let crew = &world.crew;
    if crew.idle_days >= 3 {
        mark("ice cap", "-", crew.idle_days);
    }
    if crew.quiet_days > 0 {
        mark("south ice", "-", crew.quiet_days);
    }
    for stretch in &crew.quiet_stretches {
        mark("iceberg", "-", stretch.days);
    }
    let labelled: u32 = world.behaviors.iter().map(|facts| facts.count).sum();
    if share(crew.night_actions, labelled) >= 0.03 {
        mark("aurora", "-", crew.night_actions);
    }
    let start = made_up.period.start().unwrap();
    let unlabelled = made_up
        .records
        .iter()
        .filter(|r| !r.behavior.is_labelled() && r.at >= start)
        .count() as u32;
    if share(unlabelled, labelled + unlabelled) >= 0.1 {
        mark("clouds", "-", unlabelled);
    }
    marks.sort();
    marks
}

/// Each crew's world as text: its marks of nature, its commonest moves, its models, the models
/// that share runs far more than chance, the last day's runs of mixed makers, and its months
/// before the period.
fn written(key: char) -> String {
    let made_up = crew(key);
    let world = world_of(&made_up);
    let mut text = String::new();
    let labelled: u32 = world.behaviors.iter().map(|facts| facts.count).sum();
    let _ = writeln!(text, "== {key}: {labelled} actions, ending {}", world.end);
    for mark in marks(&made_up, &world) {
        let _ = writeln!(text, "mark {mark}");
    }
    for step in world.moves.iter().filter(|m| m.count >= 6).take(10) {
        let _ = writeln!(text, "move {} -> {} | {}", step.from, step.to, step.count);
    }
    for model in &world.models {
        let mut failures: BTreeMap<u32, u32> = BTreeMap::new();
        for failure in &model.cli_failures {
            *failures.entry(made_up.period.day(failure.at)).or_default() += 1;
        }
        let _ = writeln!(
            text,
            "model {} | {} | {} actions, {} in 7 days, {} in 24 hours, {} in the hour, {} in 10 \
             minutes | first {} days back | {} merged | {} CLI failures, {} on one day",
            model.model,
            model.maker,
            model.actions,
            model.last_7_days,
            model.last_24_hours,
            model.last_hour,
            model.last_10_minutes,
            model.first_used,
            model.merged,
            model.cli_failures.len(),
            failures.values().max().unwrap_or(&0),
        );
    }
    for pair in world.pairs.iter().filter(|p| p.runs >= 5 && p.lift >= 1.25) {
        let [one, other] = &pair.models;
        let _ = writeln!(
            text,
            "pair {one} ~ {other} | {} runs | {:.2}",
            pair.runs, pair.lift
        );
    }
    for run in world.mixed_runs.iter().take(6) {
        let _ = writeln!(
            text,
            "mixed {} | {} actions | {}",
            run.run,
            run.actions,
            run.makers.join(", ")
        );
    }
    for month in &world.crew.earlier_months {
        let _ = writeln!(text, "month {} | {}", month.month, month.actions);
    }
    text
}

/// The four crews' worlds as written down once they were checked. Where a session runs past
/// midnight its later actions count on the day they were taken: so S's tried edits fall one
/// action short of a steady week for a plateau, S's last 7 days hold the night's work of the
/// 8th day back, and T's 19th day back isn't quiet.
#[test]
fn each_made_up_crew_s_world_reads_as_written() {
    let written: String = ['R', 'S', 'T', 'U'].into_iter().map(written).collect();
    let expected = include_str!("made_up_worlds.txt");
    if written != expected {
        let at = written
            .lines()
            .zip(expected.lines())
            .position(|(one, other)| one != other)
            .unwrap_or(0);
        panic!(
            "line {} isn't as written; the worlds now read:\n{written}",
            at + 1
        );
    }
}

/// U stopped changing git state itself 34 days ago, after doing it 130 times the period before,
/// one of its five busiest behaviors over the two, and kept working.
#[test]
fn u_s_crew_left_a_busy_behavior_behind_and_kept_working() {
    let world = world_of(&crew('U'));
    let changed = world.behavior("changed git state").unwrap();
    assert_eq!(
        (changed.count, changed.last_period, changed.last_used),
        (0, 130, Some(34))
    );
    let mut busiest: Vec<(u32, &str)> = world
        .behaviors
        .iter()
        .map(|facts| (facts.count + facts.last_period, facts.behavior.as_str()))
        .collect();
    busiest.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    assert!(busiest[..5].iter().any(|(_, b)| *b == "changed git state"));
    assert_eq!(world.crew.idle_days, 0);
}

/// T has been quiet for 9 days: its ice cap, and no behavior used on any of the last 7 days.
#[test]
fn t_s_quiet_crew_has_an_ice_cap_and_nothing_fresh() {
    let world = world_of(&crew('T'));
    assert_eq!(world.crew.idle_days, 9);
    assert!(
        world
            .behaviors
            .iter()
            .all(|facts| facts.days_in_last_7 == 0)
    );
    let giants: Vec<&str> = world
        .models
        .iter()
        .filter(|m| m.actions >= 100 && m.last_7_days == 0)
        .map(|m| m.model.as_str())
        .collect();
    assert_eq!(
        giants,
        [
            "gemini-3.8-flash",
            "claude-sonnet-5-5",
            "moonshotai/kimi-k2.6"
        ],
        "once busy, quiet for a week"
    );
}

/// S works nights: its share of actions between 10:00 PM and 5:00 AM is well over 3%.
#[test]
fn s_s_crew_works_nights() {
    let world = world_of(&crew('S'));
    let labelled: u32 = world.behaviors.iter().map(|facts| facts.count).sum();
    assert_eq!((world.crew.night_actions, labelled), (223, 990));
}

/// Two models of one maker that share runs far more than chance: in R, 6 Luna and 6 Astra.
#[test]
fn r_s_crew_pairs_6_luna_with_6_astra() {
    let world = world_of(&crew('R'));
    let maker = |model: &str| {
        world
            .models
            .iter()
            .find(|m| m.model == model)
            .map(|m| m.maker.clone())
    };
    let mut paired: Vec<&str> = Vec::new();
    let mut binaries = Vec::new();
    for pair in world.pairs.iter().filter(|p| p.runs >= 5 && p.lift >= 1.25) {
        let [one, other] = &pair.models;
        if maker(one) == maker(other)
            && !paired.contains(&one.as_str())
            && !paired.contains(&other.as_str())
        {
            paired.extend([one.as_str(), other.as_str()]);
            binaries.push((one.as_str(), other.as_str()));
        }
    }
    assert_eq!(binaries, [("gpt-6-luna", "gpt-6-astra")]);
}
