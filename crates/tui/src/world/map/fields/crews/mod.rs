//! Four made-up crews on the app's label places, for the world's tests: R, a busy crew; S, a
//! testing crew that works nights; T, a reading crew quiet for 9 days; and U, crew R after it
//! stopped changing git state itself 34 days ago. Their counts are made up and every detail of
//! each action (its day, its time, its session) is drawn from the crew's seed, so the same crew
//! always grows the same world.

pub mod detail;

use std::sync::OnceLock;

pub use detail::Detail;

use super::{BehaviorDays, Dot, GroundFacts, Moved};
use crate::world::raster::noise::Rng;
use argonauts_core::behaviors::behavior_world::QuietStretch;

/// Crew R's dots on the label places, and the labels, places and groups every crew shares.
const CREW_R: &str = include_str!("crew-r.json");

#[derive(serde::Deserialize)]
struct Recorded {
    labels: Vec<String>,
    places: Vec<(f64, f64)>,
    groups: Vec<Group>,
    x: Vec<f64>,
    y: Vec<f64>,
    g: Vec<usize>,
    l: Vec<usize>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Group {
    pub name: String,
    pub rgb: [u8; 3],
}

/// Which made-up crew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    R,
    S,
    T,
    U,
}

impl Key {
    pub const ALL: [Key; 4] = [Key::R, Key::S, Key::T, Key::U];
}

/// A busy hour: on `day`, some labels' actions crowd into the afternoon, and the first `fan`
/// sessions start together on `fan_label`.
pub struct Crunch {
    pub day: u32,
    pub labels: &'static [(usize, f64)],
    pub fan_label: usize,
    pub fan: usize,
}

/// What sets each crew apart besides its counts.
pub struct Habits {
    pub seed: u32,
    pub idle: u32,
    pub span: u32,
    pub quiet: &'static [u32],
    pub night: f64,
    pub loops: f64,
    pub sent: Option<&'static str>,
    pub sent_k: f64,
    pub dropped: Option<&'static str>,
    pub dropped_k: f64,
    pub crash: f64,
    pub stop: f64,
    pub fail: Option<(&'static str, f64)>,
    pub crunch: Option<Crunch>,
    /// Labels whose actions all fall within `[from, to]` days back.
    pub era: &'static [(usize, u32, u32)],
    /// Labels whose actions often come after a long wait, and how often.
    pub bog: Option<(&'static str, f64)>,
    /// Labels used last period and not in this one.
    pub before: &'static [usize],
}

/// A made-up crew: its dots, the habits its detail is drawn from, and that detail.
pub struct Crew {
    pub key: Key,
    pub labels: Vec<String>,
    pub places: Vec<(f64, f64)>,
    pub groups: Vec<Group>,
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub g: Vec<usize>,
    pub l: Vec<usize>,
    pub habits: Habits,
    /// Days back to each label's last use, where it lies past the period's own actions.
    pub last_used: Vec<(usize, u32)>,
    /// Each label's actions in the period before, where the crew says.
    pub last_period: Vec<(usize, u32)>,
    pub detail: Detail,
}

impl Crew {
    /// The crew, made once and kept, since its detail takes a moment to draw.
    pub fn get(key: Key) -> &'static Crew {
        static CREWS: OnceLock<[Crew; 4]> = OnceLock::new();
        let crews = CREWS.get_or_init(|| Key::ALL.map(Crew::make));
        &crews[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
    }

    fn make(key: Key) -> Crew {
        let recorded: Recorded = serde_json::from_str(CREW_R).expect("crew R's dots read");
        let habits = habits(key);
        let used = |label: usize| recorded.l.iter().filter(|l| **l == label).count();
        let (last_used, last_period) = match key {
            Key::U => {
                let mut prior: Vec<(usize, u32)> = (0..recorded.labels.len())
                    .map(|label| (label, (0.92 * used(label) as f64).round() as u32))
                    .collect();
                prior[CHANGED_GIT_STATE].1 = 130;
                (vec![(CHANGED_GIT_STATE, 34)], prior)
            }
            _ => (
                Vec::new(),
                habits.before.iter().map(|label| (*label, 10)).collect(),
            ),
        };
        let (x, y, g, l) = match key {
            Key::R => (recorded.x, recorded.y, recorded.g, recorded.l),
            Key::S => cloud(&recorded.places, &spec(SPEC_S), 71),
            Key::T => cloud(&recorded.places, &spec(SPEC_T), 83),
            Key::U => {
                let keep: Vec<usize> = (0..recorded.l.len())
                    .filter(|at| recorded.l[*at] != CHANGED_GIT_STATE)
                    .collect();
                let pick = |values: &[f64]| keep.iter().map(|at| values[*at]).collect();
                let pick_index = |values: &[usize]| keep.iter().map(|at| values[*at]).collect();
                (
                    pick(&recorded.x),
                    pick(&recorded.y),
                    pick_index(&recorded.g),
                    pick_index(&recorded.l),
                )
            }
        };
        let detail = Detail::draw(&recorded.labels, &l, &habits);
        Crew {
            key,
            labels: recorded.labels,
            places: recorded.places,
            groups: recorded.groups,
            x,
            y,
            g,
            l,
            habits,
            last_used,
            last_period,
            detail,
        }
    }

    pub fn len(&self) -> usize {
        self.l.len()
    }

    /// The crew as the ground reads it, every label a behavior in the list's order.
    pub fn facts(&self) -> GroundFacts {
        let detail = &self.detail;
        let full_weeks = (detail.period / 7) as usize;
        let behaviors = (0..self.labels.len())
            .map(|label| {
                let days: Vec<u32> = (0..self.len())
                    .filter(|at| self.l[*at] == label)
                    .map(|at| detail.day[at])
                    .collect();
                let mut weeks = vec![0_u32; full_weeks];
                for day in &days {
                    if let Some(week) = weeks.get_mut((day / 7) as usize) {
                        *week += 1;
                    }
                }
                let mut recent: Vec<u32> = days.iter().copied().filter(|day| *day < 7).collect();
                recent.sort_unstable();
                recent.dedup();
                let past = self
                    .last_used
                    .iter()
                    .find(|(l, _)| *l == label)
                    .map(|(_, day)| *day);
                BehaviorDays {
                    name: self.labels[label].clone(),
                    place: Some(self.places[label]),
                    last_used: days.iter().copied().min().or(past),
                    days_in_last_7: recent.len() as u32,
                    weeks,
                    after_a_wait: detail.waited(&self.l, label),
                    last_period: self
                        .last_period
                        .iter()
                        .find(|(l, _)| *l == label)
                        .map_or(0, |(_, count)| *count),
                }
            })
            .collect();
        GroundFacts {
            seed: self.habits.seed,
            dots: (0..self.len())
                .map(|at| Dot {
                    x: self.x[at],
                    y: self.y[at],
                    behavior: self.l[at],
                    group: self.g[at],
                })
                .collect(),
            behaviors,
            groups: self.groups.len(),
            idle_days: self.habits.idle,
            quiet_days: detail.quiet_days,
            quiet_stretches: detail
                .quiet_stretches
                .iter()
                .map(|&(ends, days)| QuietStretch { ends, days })
                .collect(),
            moves: detail
                .moves(&self.l)
                .into_iter()
                .map(|((from, to), count)| Moved { from, to, count })
                .collect(),
        }
    }

    /// The label's index by its words.
    pub fn label(&self, words: &str) -> usize {
        self.labels
            .iter()
            .position(|label| label == words)
            .unwrap_or_else(|| panic!("crew {:?} has no label {words}", self.key))
    }
}

/// The label crew U stopped using: "changed git state".
const CHANGED_GIT_STATE: usize = 14;

/// Dots strewn round the label places: `(label, group, count)` each.
fn cloud(
    places: &[(f64, f64)],
    spec: &[(usize, usize, usize)],
    seed: u32,
) -> (Vec<f64>, Vec<f64>, Vec<usize>, Vec<usize>) {
    let mut rng = Rng::new(seed);
    let (mut x, mut y, mut g, mut l) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for &(label, group, count) in spec {
        let (px, py) = places[label];
        for _ in 0..count {
            x.push((px + rng.normal() * 0.018).clamp(0.0, 1.0));
            y.push((py + rng.normal() * 0.018).clamp(0.0, 1.0));
            g.push(group);
            l.push(label);
        }
    }
    (x, y, g, l)
}

fn spec(parts: &[(usize, &[(usize, usize)])]) -> Vec<(usize, usize, usize)> {
    parts
        .iter()
        .flat_map(|(label, groups)| groups.iter().map(|(group, n)| (*label, *group, *n)))
        .collect()
}

type Spec = &'static [(usize, &'static [(usize, usize)])];

const SPEC_S: Spec = &[
    (8, &[(2, 60), (6, 45), (4, 25), (3, 20)]),
    (9, &[(2, 35), (6, 30), (4, 15), (3, 15)]),
    (18, &[(2, 50), (6, 40), (4, 20), (3, 10)]),
    (19, &[(2, 20), (6, 15), (3, 20)]),
    (20, &[(2, 30), (6, 25), (4, 15)]),
    (21, &[(2, 20), (6, 15), (4, 10)]),
    (10, &[(2, 25), (6, 20), (4, 15)]),
    (11, &[(6, 20), (4, 15)]),
    (15, &[(2, 15), (3, 20), (0, 10)]),
    (13, &[(2, 20), (6, 20), (0, 10)]),
    (14, &[(2, 15), (6, 10)]),
    (30, &[(2, 30), (6, 25), (4, 15)]),
    (1, &[(2, 15), (6, 15), (3, 10)]),
    (0, &[(2, 20), (6, 20), (3, 15)]),
    (35, &[(2, 20)]),
    (32, &[(2, 15), (6, 10)]),
    (6, &[(2, 15), (3, 15)]),
];

const SPEC_T: Spec = &[
    (0, &[(7, 50), (10, 30), (8, 30)]),
    (3, &[(7, 40), (9, 25), (8, 25)]),
    (4, &[(7, 30), (10, 25)]),
    (6, &[(7, 45), (10, 30), (9, 25)]),
    (7, &[(7, 20), (9, 20)]),
    (28, &[(7, 30), (10, 25), (8, 20)]),
    (29, &[(7, 40), (10, 25), (9, 20)]),
    (24, &[(8, 30), (7, 25)]),
    (26, &[(8, 25), (7, 20)]),
    (30, &[(7, 20), (8, 15), (10, 15)]),
    (32, &[(8, 20)]),
    (33, &[(8, 15)]),
    (38, &[(0, 20)]),
    (39, &[(0, 15)]),
];

const R_ERA: &[(usize, u32, u32)] = &[
    (12, 15, 29),
    (27, 15, 29),
    (28, 15, 29),
    (29, 15, 29),
    (33, 15, 29),
    (7, 15, 29),
    (3, 15, 29),
    (26, 8, 13),
    (23, 8, 13),
    (36, 8, 13),
    (0, 8, 13),
];

const U_ERA: &[(usize, u32, u32)] = &[
    (12, 15, 29),
    (27, 15, 29),
    (29, 21, 29),
    (33, 15, 29),
    (7, 15, 29),
    (3, 15, 29),
    (26, 8, 13),
    (23, 8, 13),
    (36, 8, 13),
    (0, 8, 13),
];

fn habits(key: Key) -> Habits {
    let r_like = |era, before| Habits {
        seed: 41,
        idle: 0,
        span: 30,
        quiet: &[5, 6, 12, 13, 19, 20, 26, 27],
        night: 0.04,
        loops: 0.12,
        sent: Some("^built the project"),
        sent_k: 2.6,
        dropped: Some("wrote a doc"),
        dropped_k: 1.4,
        crash: 0.06,
        stop: 0.2,
        fail: None,
        crunch: Some(Crunch {
            day: 8,
            labels: &[(30, 0.3), (19, 0.22), (1, 0.5)],
            fan_label: 1,
            fan: 8,
        }),
        era,
        bog: Some(("^ran a shell command$|^searched code", 0.75)),
        before,
    };
    match key {
        Key::R => r_like(R_ERA, &[2, 20]),
        Key::U => r_like(U_ERA, &[2, 20, CHANGED_GIT_STATE]),
        Key::S => Habits {
            seed: 71,
            idle: 0,
            span: 30,
            quiet: &[],
            night: 0.3,
            loops: 0.45,
            sent: Some("edited a test file"),
            sent_k: 1.1,
            dropped: None,
            dropped_k: 1.1,
            crash: 0.1,
            stop: 0.07,
            fail: Some(("shell|Python|built", 0.2)),
            crunch: Some(Crunch {
                day: 2,
                labels: &[(8, 0.25), (9, 0.3)],
                fan_label: 8,
                fan: 7,
            }),
            era: &[],
            bog: Some(("^built the project$", 0.3)),
            before: &[],
        },
        Key::T => Habits {
            seed: 83,
            idle: 9,
            span: 21,
            quiet: &[11, 12, 15, 18, 19, 25, 26, 27],
            night: 0.01,
            loops: 0.06,
            sent: None,
            sent_k: 1.1,
            dropped: Some("doc|web"),
            dropped_k: 0.7,
            crash: 0.03,
            stop: 0.16,
            fail: None,
            crunch: Some(Crunch {
                day: 13,
                labels: &[(29, 0.35), (6, 0.3)],
                fan_label: 29,
                fan: 6,
            }),
            era: &[
                (28, 12, 20),
                (29, 12, 20),
                (24, 10, 16),
                (26, 10, 16),
                (33, 16, 29),
                (39, 16, 29),
            ],
            bog: Some(("^fetched from the web$", 0.45)),
            before: &[8, 10, 18, 9],
        },
    }
}
