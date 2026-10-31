//! What the ground grows from: each action's dot, and the days behind each behavior.

use std::collections::HashMap;

use argonauts_core::behaviors::LABELS;
use argonauts_core::behaviors::behavior_map::BehaviorMap;
use argonauts_core::behaviors::behavior_world::QuietStretch;
use time::OffsetDateTime;

/// A crew's actions as the ground reads them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroundFacts {
    /// Seeds the noise the coasts, ridges and grain are drawn with, so a crew's world keeps
    /// its shape from one drawing to the next.
    pub seed: u32,
    pub dots: Vec<Dot>,
    pub behaviors: Vec<BehaviorDays>,
    /// How many groups the dots are coloured by.
    pub groups: usize,
    /// Days back to the crew's last action: the northern ice cap.
    pub idle_days: u32,
    /// Days in the period with no action at all: the southern ice.
    pub quiet_days: u32,
    /// Each stretch of 2 or more quiet days in a row, the latest first: an iceberg each.
    pub quiet_stretches: Vec<QuietStretch>,
    /// How often agents went from one behavior straight to another in the same session.
    pub moves: Vec<Moved>,
}

/// One action: where its dot is on the land's unit square, `x` east and `y` north, and which
/// behavior and group it counts under, by index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dot {
    pub x: f64,
    pub y: f64,
    pub behavior: usize,
    pub group: usize,
}

/// Agents went from behavior `from` straight to `to` this many times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved {
    pub from: usize,
    pub to: usize,
    pub count: u32,
}

/// One behavior, with what its climate and its land kinds are read from. Day counts go back
/// from the period's last day, 0 being that day.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BehaviorDays {
    pub name: String,
    /// Its label's own place on the land's unit square, where it stands without dots.
    pub place: Option<(f64, f64)>,
    pub last_used: Option<u32>,
    pub days_in_last_7: u32,
    /// Its actions in each full week of the period, the latest week first.
    pub weeks: Vec<u32>,
    /// Its actions that came 10 minutes or more after the one before in the same session.
    pub after_a_wait: u32,
    /// Its actions in the period before.
    pub last_period: u32,
}

impl GroundFacts {
    /// The facts of a map and the world the app worked out for it. Behaviors come in the label
    /// list's order, then the rest as their dots first appear, so the same map always grows the
    /// same ground. Without the world's facts, each behavior's days are read from its dots'
    /// times, counted back from the latest.
    pub fn from_map(map: &BehaviorMap, seed: u32) -> Self {
        let world = map.world.as_deref();
        let listed = |name: &str| LABELS.iter().position(|label| label.words == name);
        let mut first_seen: HashMap<&str, usize> = HashMap::new();
        let from_dots = map.actions.iter().map(|action| action.behavior.as_str());
        let from_facts = world
            .map(|world| world.behaviors.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|facts| facts.behavior.as_str());
        for name in from_dots.chain(from_facts) {
            let next = first_seen.len();
            first_seen.entry(name).or_insert(next);
        }
        let mut names: Vec<&str> = first_seen.keys().copied().collect();
        names.sort_by_key(|name| (listed(name).unwrap_or(usize::MAX), first_seen[name]));
        let index: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(at, name)| (*name, at))
            .collect();

        let dots = map
            .actions
            .iter()
            .map(|action| Dot {
                x: f64::from(action.x),
                y: f64::from(action.y),
                behavior: index[action.behavior.as_str()],
                group: action.group,
            })
            .collect();
        let end = world
            .map(|world| world.end)
            .filter(|end| *end != OffsetDateTime::UNIX_EPOCH)
            .or_else(|| map.actions.iter().map(|action| action.at).max());
        let behaviors = names
            .iter()
            .map(|name| {
                let place = listed(name).map(|at| {
                    let (x, y) = LABELS[at].place;
                    (f64::from(x), 1.0 - f64::from(y))
                });
                match world.and_then(|world| world.behavior(name)) {
                    Some(facts) => BehaviorDays {
                        name: (*name).to_owned(),
                        place,
                        last_used: facts.last_used,
                        days_in_last_7: u32::from(facts.days_in_last_7),
                        weeks: facts.weeks.clone(),
                        after_a_wait: facts.after_a_wait,
                        last_period: facts.last_period,
                    },
                    None => days_from_dots(map, name, place, end),
                }
            })
            .collect();
        let crew = world.map(|world| &world.crew);
        let mut quiet_stretches = crew
            .map(|crew| crew.quiet_stretches.clone())
            .unwrap_or_default();
        quiet_stretches.sort_by_key(|stretch| stretch.ends);
        let moves = world
            .map(|world| {
                world
                    .moves
                    .iter()
                    .filter_map(|moved| {
                        Some(Moved {
                            from: *index.get(moved.from.as_str())?,
                            to: *index.get(moved.to.as_str())?,
                            count: moved.count,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            seed,
            dots,
            behaviors,
            groups: map.groups.len(),
            idle_days: crew.map_or(0, |crew| crew.idle_days),
            quiet_days: crew.map_or(0, |crew| crew.quiet_days),
            quiet_stretches,
            moves,
        }
    }
}

/// A behavior's days read from its dots' times, for a map the app worked no world out for.
fn days_from_dots(
    map: &BehaviorMap,
    name: &str,
    place: Option<(f64, f64)>,
    end: Option<OffsetDateTime>,
) -> BehaviorDays {
    let mut days: Vec<u32> = match end {
        Some(end) => map
            .actions
            .iter()
            .filter(|action| action.behavior == name)
            .map(|action| days_back(action.at, end))
            .collect(),
        None => Vec::new(),
    };
    days.sort_unstable();
    days.dedup();
    BehaviorDays {
        name: name.to_owned(),
        place,
        last_used: days.first().copied(),
        days_in_last_7: days.iter().filter(|day| **day < 7).count() as u32,
        ..BehaviorDays::default()
    }
}

/// Whole days from `at` back to `end`, by the calendar where `end` is.
fn days_back(at: OffsetDateTime, end: OffsetDateTime) -> u32 {
    let at = at.to_offset(end.offset()).date();
    (end.date() - at).whole_days().max(0) as u32
}
