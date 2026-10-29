//! Finding nature in the facts and placing each mark, in the one order the world keeps: storms and
//! waves first, then what each behavior's breaks and outcomes make, the lakes, the land's own
//! kinds, the rivers and currents, and last the atolls and shoals.

use std::f64::consts::PI;

use argonauts_core::behavior_world::Crash;
use time::Duration;

use super::water::{arc, tsunami_run, valley_path};
use super::{Feature, Hurricane, Kind, Nature, NatureFacts, Rift, Shoal, Stream, Wave, wind};
use crate::world::map::fields::{Ground, HEIGHT, Look, WIDTH, map_pixel};
use crate::world::map::land::{Placed, Placer, Want, land_marks};
use crate::world::raster::noise::hash3;

/// A hurricane's categories start at these many actions within an hour.
const HURRICANE: [u32; 5] = [25, 40, 60, 90, 130];
/// A tsunami needs this many agents within 5 minutes.
const TSUNAMI_AGENTS: u32 = 6;
/// Every hurricane keeps 5° off the equator, where no real storm turns, so the way it turns is
/// never in doubt: 16 px on the map.
pub const EQUATOR_GAP: f64 = 16.0;
/// Clouds show when at least this share of actions fits no label.
const CLOUD_FLOOR: f64 = 0.1;
/// At most this many lakes, rivers and currents on one map.
const LAKES: usize = 3;
const RIVERS: usize = 6;
const CURRENTS: usize = 4;

impl Nature {
    /// Finds nature on a crew's ground and places it. A gas giant has none yet.
    pub fn find(ground: &Ground, facts: &NatureFacts) -> Self {
        if ground.look == Look::Gas {
            return Self::default();
        }
        let mut placing = Placing {
            ground,
            facts,
            placer: Placer::new(ground),
            features: Vec::new(),
        };
        placing.bursts();
        placing.breaks();
        placing.lakes();
        let land = land_marks(ground, &mut placing.placer);
        placing
            .features
            .extend(land.into_iter().map(|mark| Feature {
                kind: Kind::Land(mark.kind),
                behavior: mark.behavior,
                x: mark.x,
                y: mark.y,
                radius: mark.radius,
                reach: mark.reach,
            }));
        placing.streams();
        placing.atolls();
        let all = ground.dots.len() as f64 + f64::from(facts.unlabelled);
        let cover = if all > 0.0 {
            f64::from(facts.unlabelled) / all
        } else {
            0.0
        };
        Self {
            features: placing.features,
            shoals: shoals(ground, facts),
            clouds: (cover >= CLOUD_FLOOR).then_some(cover),
        }
    }
}

struct Placing<'a> {
    ground: &'a Ground,
    facts: &'a NatureFacts,
    placer: Placer,
    features: Vec<Feature>,
}

impl Placing<'_> {
    fn add(&mut self, feature: Feature, flat: bool) {
        self.placer.add(Placed {
            x: feature.x,
            y: feature.y,
            radius: feature.radius,
            flat,
        });
        self.features.push(feature);
    }

    /// Bursts in time: a hurricane where one behavior crowded an hour, a tsunami where many
    /// agents took it up within 5 minutes.
    fn bursts(&mut self) {
        for behavior in 0..self.ground.behaviors() {
            let facts = self.facts.of(behavior);
            if let Some(burst) = facts.busiest_hour.filter(|b| b.count >= HURRICANE[0]) {
                let category = HURRICANE.iter().filter(|v| burst.count >= **v).count() as u32;
                let radius = 30.0 + 9.0 * f64::from(category);
                let off_equator = |_: f64, y: f64| {
                    if (y - HEIGHT as f64 / 2.0).abs() < EQUATOR_GAP {
                        f64::NEG_INFINITY
                    } else {
                        0.0
                    }
                };
                let (near, keep) = (radius * 0.55, radius * 0.95);
                let spot = self
                    .placer
                    .spot(
                        self.ground,
                        behavior,
                        near,
                        Want::Sea,
                        0.8,
                        keep,
                        &off_equator,
                    )
                    .or_else(|| {
                        spot_anywhere(
                            &self.placer,
                            self.ground,
                            behavior,
                            near,
                            0.9,
                            keep,
                            &off_equator,
                        )
                    });
                if let Some((x, y)) = spot {
                    let hurricane = Hurricane {
                        actions: burst.count,
                        category,
                        south: y > HEIGHT as f64 / 2.0,
                        at: burst.at,
                    };
                    let feature = Feature {
                        kind: Kind::Hurricane(hurricane),
                        behavior,
                        x,
                        y,
                        radius,
                        reach: 16.0,
                    };
                    self.add(feature, false);
                }
            }
            let fan = facts
                .most_agents_in_5_minutes
                .filter(|b| b.count >= TSUNAMI_AGENTS);
            if let Some(burst) = fan
                && let Some(run) = tsunami_run(self.ground, behavior, &self.placer.placed)
            {
                let size = f64::from((40 + 3 * (burst.count - TSUNAMI_AGENTS)).min(60));
                let wave = Wave {
                    agents: burst.count,
                    at: burst.at,
                    from: run.from,
                    to: run.to,
                    out: run.out,
                    coast: run.coast,
                    size,
                    face: if run.out.0 > 0.0 { -1.0 } else { 1.0 },
                };
                let feature = Feature {
                    kind: Kind::Tsunami(wave),
                    behavior,
                    x: (run.from.0 + run.to.0) / 2.0,
                    y: (run.from.1 + run.to.1) / 2.0,
                    radius: 22.0,
                    reach: size * 0.55,
                };
                self.add(feature, false);
            }
        }
    }

    /// Loops, breaks and outcomes, behavior by behavior: volcanoes, floods, wildfires, tornadoes,
    /// thunderstorms, and faults that shake while a crash is fresh.
    fn breaks(&mut self) {
        let ground = self.ground;
        for behavior in 0..ground.behaviors() {
            let count = ground.count[behavior];
            if count == 0 {
                continue;
            }
            let facts = self.facts.of(behavior).clone();
            let share = |n: u32| f64::from(n) / f64::from(count);
            let centre = ground.centre[behavior];
            let none = |_: f64, _: f64| 0.0;

            if facts.failed >= 3 && share(facts.failed) >= 0.1 {
                let radius = (7.0 + 2.2 * f64::from(facts.failed).sqrt()).min(20.0);
                if let Some((x, y)) =
                    self.placer
                        .spot(ground, behavior, radius, Want::Land, 1.0, radius, &none)
                {
                    let kind = Kind::Volcano {
                        failed: facts.failed,
                        smoking: facts.last_failed.is_some_and(|days| days < 7),
                        last: facts.last_failed,
                    };
                    self.add(feature(kind, behavior, x, y, radius, radius + 3.0), false);
                }
            }
            if facts.sent_back >= 5 && share(facts.sent_back) >= 0.25 {
                let kind = Kind::Flood {
                    sent_back: facts.sent_back,
                };
                self.add(feature(kind, behavior, centre.0, centre.1, 0.0, 0.0), false);
            }
            if facts.dropped >= 5 && share(facts.dropped) >= 0.25 {
                let kind = Kind::Wildfire {
                    dropped: facts.dropped,
                    burning: facts.last_dropped.is_some_and(|days| days < 7),
                    last: facts.last_dropped,
                };
                self.add(feature(kind, behavior, centre.0, centre.1, 0.0, 0.0), false);
            }
            if facts.runs_in_a_row > 0 {
                let funnel = 6.0 + (f64::from(facts.longest_run) - 4.0).min(6.0);
                let spot = spot_anywhere(
                    &self.placer,
                    ground,
                    behavior,
                    funnel + 4.0,
                    1.0,
                    funnel + 4.0,
                    &none,
                );
                if let Some((x, y)) = spot {
                    let kind = Kind::Tornado {
                        times: facts.runs_in_a_row,
                        longest: facts.longest_run,
                    };
                    self.add(
                        feature(kind, behavior, x, y, funnel + 4.0, funnel + 6.0),
                        false,
                    );
                }
            }
            if facts.stopped >= 2 {
                let radius = 10.0 + 3.0 * f64::from(facts.stopped).sqrt();
                let spot =
                    spot_anywhere(&self.placer, ground, behavior, radius, 1.0, radius, &none);
                if let Some((x, y)) = spot {
                    let kind = Kind::Thunderstorm {
                        sessions: facts.stopped,
                    };
                    self.add(feature(kind, behavior, x, y, radius, radius + 2.0), false);
                }
            }
            if facts.cli_failed >= 1 {
                let length = 26.0 + 16.0 * f64::from(facts.cli_failed).sqrt();
                // Clear of dots, so its rings show.
                let placer = &self.placer;
                let clear = |x: f64, y: f64| -3.0 * f64::from(placer.dots_near(x, y, 22.0));
                let near = length * 0.4;
                let spot = placer.spot(ground, behavior, near, Want::Land, 1.0, near, &clear);
                if let Some((x, y)) = spot {
                    let mut crashes = facts.crashes.clone();
                    crashes.sort_by_key(|crash| std::cmp::Reverse(crash.at));
                    let recent: Vec<Crash> = crashes
                        .iter()
                        .filter(|crash| self.facts.end - crash.at <= Duration::DAY)
                        .cloned()
                        .collect();
                    let rift = Rift {
                        sessions: facts.cli_failed,
                        length,
                        angle: (y - centre.1).atan2(x - centre.0) + PI / 2.0,
                        last: crashes.first().map(|crash| crash.at),
                        recent,
                    };
                    let flat = rift.shaking();
                    let kind = Kind::Rift(rift);
                    self.add(feature(kind, behavior, x, y, length * 0.45, 12.0), flat);
                }
            }
        }
    }

    /// Lakes where sessions end: the three behaviors most ended on, 6 sessions or more, each on
    /// flat ground of its own land, near it.
    fn lakes(&mut self) {
        let ground = self.ground;
        let mut ends: Vec<(u32, usize)> = (0..ground.behaviors())
            .map(|behavior| (self.facts.of(behavior).session_ends, behavior))
            .filter(|(ends, _)| *ends >= 6)
            .collect();
        ends.sort_by_key(|(ends, _)| std::cmp::Reverse(*ends));
        for (sessions, behavior) in ends.into_iter().take(LAKES) {
            let radius = (5.0 + 1.6 * f64::from(sessions).sqrt()).min(16.0);
            let (cx, cy) = ground.centre[behavior];
            let near = |x: f64, y: f64| -0.12 * (x - cx).hypot(y - cy);
            let spot = self
                .placer
                .flat_spot(ground, behavior, radius, 1.0, Some(&near));
            if let Some((x, y)) = spot {
                let kind = Kind::Lake { sessions };
                self.add(feature(kind, behavior, x, y, radius, radius + 3.0), true);
            }
        }
    }

    /// Rivers and currents: the most common moves from one behavior to the next, at most six
    /// rivers over land and four currents over the sea.
    fn streams(&mut self) {
        let ground = self.ground;
        let mut moves: Vec<_> = ground
            .facts
            .moves
            .iter()
            .filter(|moved| moved.count >= 6)
            .collect();
        moves.sort_by_key(|moved| std::cmp::Reverse(moved.count));
        let (mut rivers, mut currents) = (0, 0);
        for moved in moves.into_iter().take(14) {
            let (from, to) = (moved.from, moved.to);
            if from == to || ground.count[from] == 0 || ground.count[to] == 0 {
                continue;
            }
            let seed = (from * 1000 + to) as f64;
            let ends = (ground.centre[from], ground.centre[to]);
            let bed = valley_path(ground, ends.0, ends.1, seed);
            let over_land = bed.is_some();
            if over_land && rivers >= RIVERS || !over_land && currents >= CURRENTS {
                continue;
            }
            let path = match bed {
                Some(bed) => {
                    rivers += 1;
                    bed
                }
                None => {
                    currents += 1;
                    arc(ends.0, ends.1, seed)
                }
            };
            let (x, y) = path[path.len() >> 1];
            let stream = Stream {
                to,
                moves: moved.count,
                width: (1.0 + 0.45 * f64::from(moved.count).sqrt()).min(4.2),
                path,
            };
            let kind = if over_land {
                Kind::River(stream)
            } else {
                Kind::Current(stream)
            };
            self.add(feature(kind, from, x, y, 0.0, 0.0), false);
        }
    }

    /// Atolls where behaviors used last period and not in this one stood, now at sea.
    fn atolls(&mut self) {
        let ground = self.ground;
        for (behavior, days) in ground.facts.behaviors.iter().enumerate() {
            if ground.count[behavior] > 0 || self.facts.of(behavior).last_period == 0 {
                continue;
            }
            let Some((px, py)) = days.place else {
                continue;
            };
            let (x, y) = map_pixel(px, py);
            if ground.land_at(x, y) < 0.7 {
                self.add(feature(Kind::Atoll, behavior, x, y, 12.0, 14.0), true);
            }
        }
    }
}

fn feature(kind: Kind, behavior: usize, x: f64, y: f64, radius: f64, reach: f64) -> Feature {
    Feature {
        kind,
        behavior,
        x,
        y,
        radius,
        reach,
    }
}

/// The best spot by a behavior on any ground, land or sea: rings of 24 spots further and further
/// out, scored by being near, clear of dots and of the marks placed before, and by `bias`.
#[allow(clippy::too_many_arguments)]
fn spot_anywhere(
    placer: &Placer,
    ground: &Ground,
    behavior: usize,
    radius: f64,
    far: f64,
    keep: f64,
    bias: &dyn Fn(f64, f64) -> f64,
) -> Option<(f64, f64)> {
    let (cx, cy) = ground.centre[behavior];
    let (width, height) = (WIDTH as f64, HEIGHT as f64);
    let mut best = None;
    let mut best_score = -1e9;
    for out in [20.0, 32.0, 48.0, 68.0, 92.0, 120.0] {
        let distance = (radius + out) * far;
        for k in 0..24 {
            let angle = f64::from(k) * PI / 12.0 + 0.13;
            let (x, y) = (cx + angle.cos() * distance, cy + angle.sin() * distance);
            if x < keep + 6.0 || y < keep + 6.0 || x > width - keep - 6.0 || y > height - keep - 6.0
            {
                continue;
            }
            let mut score =
                -distance * 0.03 - f64::from(placer.dots_near(x, y, radius + 6.0)) * 2.2;
            score += bias(x, y);
            for placed in placer.placed.iter().filter(|p| p.radius != 0.0) {
                let gap = (placed.x - x).hypot(placed.y - y) - placed.radius - radius;
                if gap < 10.0 {
                    score -= (10.0 - gap) * 4.0;
                }
            }
            if score > best_score {
                best_score = score;
                best = Some((x, y));
            }
        }
    }
    best
}

/// A shoal for each label place never used that falls at sea, lying along the wind.
fn shoals(ground: &Ground, facts: &NatureFacts) -> Vec<Shoal> {
    let unused_behaviors = ground
        .facts
        .behaviors
        .iter()
        .enumerate()
        .filter(|(at, _)| ground.count[*at] == 0 && facts.of(*at).last_period == 0)
        .filter_map(|(_, days)| Some((days.name.clone(), days.place?)));
    let listed = facts
        .unused
        .iter()
        .filter(|unused| {
            !ground
                .facts
                .behaviors
                .iter()
                .any(|days| days.name == unused.name)
        })
        .map(|unused| (unused.name.clone(), unused.place));
    let (wx, wy) = wind();
    listed
        .chain(unused_behaviors)
        .filter_map(|(name, (px, py))| {
            let (x, y) = map_pixel(px, py);
            if ground.land_at(x, y) >= 0.7 {
                return None;
            }
            let turn = hash3(x.round(), y.round(), 5.0, 7.0) - 0.5;
            Some(Shoal {
                name,
                x,
                y,
                angle: wy.atan2(wx) + turn * 0.5,
            })
        })
        .collect()
}
