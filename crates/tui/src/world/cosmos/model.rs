//! What the universe shows, worked out once from the world's facts: each maker a galaxy, each of
//! its models one body by how you use it, each model's busiest behaviors its planets, and what
//! flies between them. Nothing here draws; the levels read it.
//!
//! The indices here are the ones the universe's [`Pointed`](crate::world::Pointed) marks carry:
//! galaxies biggest first, stars by galaxy and then most used first, and the bodies star by star
//! (planets, belt, moons, flares) before the comets and the tidal streams.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use argonauts_core::behaviors::behavior_world::{ModelFacts, WorldFacts};
use argonauts_core::look::logos::{Logo, LogoMark, logo_for};
use time::Date;

use super::rules;
use crate::world::raster::noise::Rng;

/// One of the period's actions, as the universe places it: a point on its model's arm, a rock in
/// its belt, or a meteor when it was in the last 10 minutes.
#[derive(Debug, Clone, PartialEq)]
pub struct StarAction {
    /// Its index among the map's actions, which the pointer names.
    pub index: u32,
    /// Its model's index among the world's models.
    pub model: usize,
    pub behavior: String,
    /// Days back from the end of the period, 0 for its last day.
    pub day: u32,
    /// Seconds before the end of the period.
    pub age: f64,
    pub failed: bool,
}

/// What a model's star is, by how you use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StarKind {
    /// An active model.
    Star,
    /// New to you: under 30 actions, first used in the last 14 days.
    Protostar,
    /// Once busy, now quiet: 100 or more actions, none in the last 7 days.
    RedGiant,
    /// An older model of a line whose newer model did more in the last 7 days.
    WhiteDwarf,
    /// 20 or more actions, and none of its work merged.
    BlackHole,
}

/// When a model's behavior was last used, as the map's land shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Climate {
    /// Used on 4 or more of the last 7 days.
    Rainforest,
    /// Used in the last 7 days.
    Forest,
    /// Last used 7 to 13 days ago.
    Grassland,
    /// Last used 14 to 20 days ago.
    Savanna,
    /// Last used 21 or more days ago.
    Desert,
}

impl Climate {
    pub fn of(last_used: u32, days_in_last_7: u8) -> Self {
        match last_used {
            _ if days_in_last_7 >= 4 => Climate::Rainforest,
            0..7 => Climate::Forest,
            7..14 => Climate::Grassland,
            14..21 => Climate::Savanna,
            _ => Climate::Desert,
        }
    }

    /// Seconds the innermost planet of this climate takes to go round: the more recently used,
    /// the faster. A desert planet stands still.
    pub fn orbit_seconds(self) -> f64 {
        match self {
            Climate::Rainforest => 20.0,
            Climate::Forest => 40.0,
            Climate::Grassland => 80.0,
            Climate::Savanna => 160.0,
            Climate::Desert => 0.0,
        }
    }

    /// The map's own colour for this land.
    pub fn rgb(self) -> [f64; 3] {
        match self {
            Climate::Rainforest => [32.0, 70.0, 34.0],
            Climate::Forest => [62.0, 92.0, 46.0],
            Climate::Grassland => [146.0, 140.0, 84.0],
            Climate::Savanna => [188.0, 160.0, 88.0],
            Climate::Desert => [206.0, 172.0, 118.0],
        }
    }

    /// The same land lit in open space, as a planet's ground shows it.
    pub(super) fn ground(self) -> [f64; 3] {
        match self {
            Climate::Rainforest => [58.0, 120.0, 62.0],
            Climate::Forest => [96.0, 132.0, 74.0],
            Climate::Grassland => [170.0, 160.0, 100.0],
            Climate::Savanna => [196.0, 166.0, 96.0],
            Climate::Desert => [204.0, 170.0, 118.0],
        }
    }

    /// Where its sea stops in its planet's noise: wetter land has more sea, a desert none.
    pub(super) fn sea_level(self) -> f64 {
        match self {
            Climate::Rainforest => 0.1,
            Climate::Forest => -0.05,
            Climate::Grassland => -0.25,
            Climate::Savanna => -0.5,
            Climate::Desert => -9.0,
        }
    }
}

/// A maker's galaxy.
#[derive(Debug, Clone, PartialEq)]
pub struct Maker {
    /// As the registry names providers, such as `anthropic`.
    pub key: String,
    pub actions: u32,
    pub failed: u32,
    pub last_7_days: u32,
    pub last_24_hours: u32,
    /// Its share of every model's actions together.
    pub share: f64,
    /// Under 3% of your actions: a dwarf galaxy, with no arms.
    pub dwarf: bool,
    /// How fast it turns, in radians a second: once a minute when all its actions came in the
    /// last 7 days, slower in proportion, 0 when none did.
    pub turn: f64,
    /// Its models' stars, most used first.
    pub stars: Vec<usize>,
    pub rgb: [u8; 3],
    /// Its own logo where the brand folder has one, its own CLI's where it makes one, else its
    /// colour dot.
    pub logo: Logo,
}

/// Two models of one maker that share runs far more often than chance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Binary {
    /// The other star.
    pub with: usize,
    pub runs: u32,
    pub lift: f32,
    /// This one is the smaller, which rides round the other.
    pub companion: bool,
}

/// A model's star.
#[derive(Debug, Clone, PartialEq)]
pub struct Star {
    /// Its model's index among the world's models.
    pub model: usize,
    pub maker: usize,
    pub kind: StarKind,
    pub rgb: [u8; 3],
    /// Whoever serves the model.
    pub logo: Logo,
    pub actions: u32,
    pub failed: u32,
    pub last_7_days: u32,
    /// Seconds a breath takes: worked in the last hour, or only in the last day. `None` keeps a
    /// steady glow.
    pub pulse: Option<f64>,
    /// The most sessions its CLI failed in on one day, and that day, where they are 3 or more.
    pub supernova: Option<(usize, Date)>,
    pub binary: Option<Binary>,
    /// The star of its line's newer model, where that did more in the last 7 days, and days
    /// back to when it came.
    pub newer: Option<(usize, u32)>,
    pub system: System,
    /// Every body round it, in the order of the universe's list: planets, belt, moons, flares.
    pub bodies: Vec<usize>,
}

impl Star {
    /// The star's light, a little dimmer when it did nothing in the last 7 days.
    pub fn quiet(&self) -> bool {
        self.last_7_days == 0
    }
}

/// One model's own system: its busiest behaviors as planets, the rest as a belt of rocks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct System {
    pub planets: Vec<Planet>,
    /// Actions of its other behaviors, a rock each.
    pub belt: u32,
    /// The rocks the period's actions place, by their index among the universe's actions.
    pub rocks: Vec<usize>,
    /// The sessions its CLI failed in that flare, newest first, by index among its model's.
    pub flares: Vec<usize>,
    /// A sunspot for every 2% of its actions that failed.
    pub sunspots: u32,
}

/// One of a model's busiest behaviors, going round its star.
#[derive(Debug, Clone, PartialEq)]
pub struct Planet {
    /// Its index among its model's behaviors.
    pub behavior: usize,
    pub count: u32,
    pub climate: Climate,
    /// 30% or more of its actions failed.
    pub lava: bool,
    /// Times an agent looped on it: its rings.
    pub loops: u32,
    /// Its finer behaviors.
    pub moons: Vec<Moon>,
    /// Seconds a turn round the star takes, 0 for one that stands still.
    pub period: f64,
}

/// A finer behavior going round its planet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moon {
    /// Its index among its model's behaviors.
    pub behavior: usize,
    pub count: u32,
}

/// Anything round a star or between galaxies that the pointer can find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    Planet {
        star: usize,
        planet: usize,
    },
    Moon {
        star: usize,
        planet: usize,
        moon: usize,
    },
    Belt {
        star: usize,
    },
    Flare {
        star: usize,
        flare: usize,
    },
    Comet {
        comet: usize,
    },
    Stream {
        stream: usize,
    },
}

/// Two makers' models that share runs far more often than chance: a faint stream between them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stream {
    pub stars: [usize; 2],
    /// Its index among the world's model pairs.
    pub pair: usize,
    pub runs: u32,
    pub lift: f32,
}

/// A run of the last 24 hours with two makers' models, flying from the maker that started it
/// to the one that joined.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comet {
    /// Its index among the world's mixed runs.
    pub run: usize,
    pub from: usize,
    pub to: usize,
    /// Where in its loop it starts, so comets don't fly together.
    pub phase: f64,
}

/// An action of the last 10 minutes, falling from its maker's galaxy onto your planet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meteor {
    pub star: usize,
    /// Seconds before the end of the period.
    pub age: f64,
}

/// An earlier month's actions, a cluster of faint stars in the sky.
#[derive(Debug, Clone, PartialEq)]
pub struct Month {
    /// Such as `2026-08`.
    pub month: String,
    pub actions: u32,
    /// The stars it is drawn with: its actions, scaled down when all the months hold more than
    /// the sky does.
    pub stars: u32,
}

/// The universe worked out from the world's facts.
#[derive(Debug, Clone, PartialEq)]
pub struct Universe {
    pub makers: Vec<Maker>,
    pub stars: Vec<Star>,
    pub bodies: Vec<Body>,
    pub streams: Vec<Stream>,
    pub comets: Vec<Comet>,
    pub meteors: Vec<Meteor>,
    /// The months before the period, the month just before it first.
    pub months: Vec<Month>,
    /// The period's actions the universe places.
    pub actions: Vec<StarAction>,
    /// Every model's actions together.
    pub total: u32,
    /// The days the period's actions span, which a point's place on its arm is measured by.
    pub days: u32,
}

impl Universe {
    /// `colours` holds each model's colour in the order of the world's models, as every tab
    /// draws it; a model past its end draws grey.
    pub fn new(facts: &WorldFacts, colours: &[[u8; 3]], actions: Vec<StarAction>) -> Self {
        let models = &facts.models;
        let mut universe = Universe {
            makers: makers_of(models),
            stars: Vec::new(),
            bodies: Vec::new(),
            streams: Vec::new(),
            comets: Vec::new(),
            meteors: Vec::new(),
            months: months_of(facts),
            actions: actions
                .into_iter()
                .filter(|action| action.model < models.len())
                .collect(),
            total: models.iter().map(|m| m.actions).sum(),
            days: models
                .iter()
                .map(|m| m.first_used + 1)
                .max()
                .unwrap_or(1)
                .max(7),
        };
        let total = f64::from(universe.total.max(1));
        let maker_count = universe.makers.len();
        for maker_at in 0..maker_count {
            let mut order = std::mem::take(&mut universe.makers[maker_at].stars);
            order.sort_by(|a, b| by_use(&models[*a], &models[*b]));
            for model in order {
                let star = universe.stars.len();
                universe
                    .stars
                    .push(star_of(models, model, maker_at, colours));
                universe.makers[maker_at].stars.push(star);
            }
            let maker = &mut universe.makers[maker_at];
            maker.share = f64::from(maker.actions) / total;
            maker.dwarf = maker.share < rules::DWARF_SHARE;
            let recent = f64::from(maker.last_7_days) / f64::from(maker.actions.max(1));
            maker.turn = if recent > 0.0 {
                std::f64::consts::TAU / (rules::TURN_SECONDS / recent)
            } else {
                0.0
            };
        }
        universe.find_dwarfs(models);
        universe.pair_up(facts);
        universe.fly(facts);
        universe.build_systems(models);
        universe
    }

    /// The star of the world's model at `model`, if it has one.
    pub fn star_of_model(&self, model: usize) -> Option<usize> {
        self.stars.iter().position(|star| star.model == model)
    }

    /// The level-free name of the star of a model by its id, for pairs and runs that name them.
    fn star_by_id(&self, models: &[ModelFacts], id: &str) -> Option<usize> {
        let model = models.iter().position(|m| m.model == id)?;
        self.star_of_model(model)
    }

    /// White dwarfs: a model whose line's newer model did more in the last 7 days. Kept apart
    /// from each star's own kind, since it reads another star.
    fn find_dwarfs(&mut self, models: &[ModelFacts]) {
        for at in 0..self.stars.len() {
            let facts = &models[self.stars[at].model];
            let Some(newer) = facts.newer.as_ref() else {
                continue;
            };
            let Some(newer_star) = self.star_by_id(models, &newer.model) else {
                continue;
            };
            if newer_star == at {
                continue;
            }
            if models[self.stars[newer_star].model].last_7_days > facts.last_7_days {
                let star = &mut self.stars[at];
                star.kind = StarKind::WhiteDwarf;
                star.newer = Some((newer_star, newer.came));
            }
        }
    }

    /// Binaries within a maker, strongest first, each star in one at most; tidal streams
    /// between makers, the strongest three.
    fn pair_up(&mut self, facts: &WorldFacts) {
        let mut pairs: Vec<(usize, usize, usize)> = Vec::new();
        for (at, pair) in facts.pairs.iter().enumerate() {
            if pair.runs < rules::TOGETHER_RUNS || pair.lift < rules::TOGETHER_LIFT {
                continue;
            }
            let (Some(a), Some(b)) = (
                self.star_by_id(&facts.models, &pair.models[0]),
                self.star_by_id(&facts.models, &pair.models[1]),
            ) else {
                continue;
            };
            if a != b {
                pairs.push((at, a, b));
            }
        }
        pairs.sort_by(|x, y| {
            let (lx, ly) = (facts.pairs[x.0].lift, facts.pairs[y.0].lift);
            ly.partial_cmp(&lx)
                .unwrap_or(Ordering::Equal)
                .then(x.0.cmp(&y.0))
        });
        for &(at, a, b) in &pairs {
            let pair = &facts.pairs[at];
            if self.stars[a].maker == self.stars[b].maker {
                if self.stars[a].binary.is_some() || self.stars[b].binary.is_some() {
                    continue;
                }
                // The smaller star rides round the bigger one.
                let (big, small) = if self.stars[a].actions >= self.stars[b].actions {
                    (a, b)
                } else {
                    (b, a)
                };
                for (star, with, companion) in [(big, small, false), (small, big, true)] {
                    self.stars[star].binary = Some(Binary {
                        with,
                        runs: pair.runs,
                        lift: pair.lift,
                        companion,
                    });
                }
            } else if self.streams.len() < rules::STREAMS_MAX {
                self.streams.push(Stream {
                    stars: [a, b],
                    pair: at,
                    runs: pair.runs,
                    lift: pair.lift,
                });
            }
        }
    }

    /// Comets from the latest runs that mixed makers, and meteors from the last 10 minutes.
    fn fly(&mut self, facts: &WorldFacts) {
        let mut runs: Vec<usize> = (0..facts.mixed_runs.len()).collect();
        runs.sort_by(|a, b| facts.mixed_runs[*b].at.cmp(&facts.mixed_runs[*a].at));
        for run in runs {
            if self.comets.len() >= rules::COMETS_MAX {
                break;
            }
            let mixed = &facts.mixed_runs[run];
            let age = facts.end - mixed.at;
            if age.is_negative() || age > time::Duration::DAY {
                continue;
            }
            let maker = |key: Option<&String>| {
                let key = key?;
                self.makers.iter().position(|m| &m.key == key)
            };
            let (Some(from), Some(to)) = (maker(mixed.makers.first()), maker(mixed.makers.get(1)))
            else {
                continue;
            };
            if from == to {
                continue;
            }
            let k = self.comets.len() as f64;
            self.comets.push(Comet {
                run,
                from,
                to,
                phase: (k * 0.37).fract(),
            });
        }
        let window = rules::METEOR_MINUTES * 60.0;
        let mut meteors: Vec<Meteor> = self
            .actions
            .iter()
            .filter(|action| action.age >= 0.0 && action.age < window)
            .filter_map(|action| {
                Some(Meteor {
                    star: self.star_of_model(action.model)?,
                    age: action.age,
                })
            })
            .collect();
        meteors.sort_by(|a, b| b.age.partial_cmp(&a.age).unwrap_or(Ordering::Equal));
        self.meteors = meteors;
    }

    /// Each star's planets, moons, belt and flares, and the universe's list of bodies.
    fn build_systems(&mut self, models: &[ModelFacts]) {
        let mut by_model: Vec<Vec<usize>> = vec![Vec::new(); models.len()];
        for (at, action) in self.actions.iter().enumerate() {
            by_model[action.model].push(at);
        }
        for star_at in 0..self.stars.len() {
            let model = self.stars[star_at].model;
            let system = system_of(&models[model], &self.actions, &by_model[model]);
            self.stars[star_at].system = system;
        }
        for star_at in 0..self.stars.len() {
            let system = &self.stars[star_at].system;
            let mut bodies = Vec::new();
            let mut add = |body: Body, bodies: &mut Vec<usize>| {
                bodies.push(self.bodies.len());
                self.bodies.push(body);
            };
            let star = star_at;
            for planet in 0..system.planets.len() {
                add(Body::Planet { star, planet }, &mut bodies);
            }
            if system.belt > 0 {
                add(Body::Belt { star }, &mut bodies);
            }
            for (planet, p) in system.planets.iter().enumerate() {
                for moon in 0..p.moons.len() {
                    add(Body::Moon { star, planet, moon }, &mut bodies);
                }
            }
            for flare in 0..system.flares.len() {
                add(Body::Flare { star, flare }, &mut bodies);
            }
            self.stars[star_at].bodies = bodies;
        }
        for comet in 0..self.comets.len() {
            self.bodies.push(Body::Comet { comet });
        }
        for stream in 0..self.streams.len() {
            self.bodies.push(Body::Stream { stream });
        }
    }

    /// The body at `at` in the universe's list, if any.
    pub fn body(&self, at: u32) -> Option<Body> {
        self.bodies.get(usize::try_from(at).ok()?).copied()
    }

    /// Where in the list a body is, for the mark that names it.
    pub fn body_index(&self, body: Body) -> Option<u32> {
        let at = self.bodies.iter().position(|b| *b == body)?;
        u32::try_from(at).ok()
    }

    /// Something in the universe moves: a star breathes or goes round its partner, a
    /// protostar's cloud turns, a comet or a meteor flies. A galaxy's own turn is told, not
    /// drawn, so its disc stays one picture.
    pub fn moves(&self) -> bool {
        self.stars
            .iter()
            .any(|s| s.pulse.is_some() || s.binary.is_some() || s.kind == StarKind::Protostar)
            || !self.comets.is_empty()
            || !self.meteors.is_empty()
    }

    /// Something in a maker's galaxy moves: its stars breathe, a binary goes round, a
    /// protostar's cloud turns.
    pub fn galaxy_moves(&self, maker: usize) -> bool {
        self.makers.get(maker).is_some_and(|m| {
            m.stars.iter().any(|s| {
                let star = &self.stars[*s];
                star.pulse.is_some() || star.binary.is_some() || star.kind == StarKind::Protostar
            })
        })
    }

    /// Something in a model's system moves: its star breathes, its partner goes round, its
    /// cloud turns, a flare rises, a planet goes round.
    pub fn system_moves(&self, star: usize) -> bool {
        self.stars.get(star).is_some_and(|s| {
            s.pulse.is_some()
                || s.binary.is_some()
                || s.kind == StarKind::Protostar
                || (!s.system.flares.is_empty() && flares_shown(s.kind))
                || s.system.planets.iter().any(|p| p.period > 0.0)
        })
    }
}

/// Whether a star of this kind shows its flares: a protostar's cloud and the dark of a dwarf or
/// a black hole leave no surface for them.
pub(super) fn flares_shown(kind: StarKind) -> bool {
    matches!(kind, StarKind::Star | StarKind::RedGiant)
}

/// Most used first, then by the model's id, so the order never changes between two draws.
fn by_use(a: &ModelFacts, b: &ModelFacts) -> Ordering {
    b.actions
        .cmp(&a.actions)
        .then_with(|| a.model.cmp(&b.model))
}

/// The makers, biggest first, each with its models' indices among the world's.
fn makers_of(models: &[ModelFacts]) -> Vec<Maker> {
    let mut makers: Vec<Maker> = Vec::new();
    for (at, model) in models.iter().enumerate() {
        let maker = match makers.iter().position(|m| m.key == model.maker) {
            Some(found) => found,
            None => {
                makers.push(Maker {
                    key: model.maker.clone(),
                    actions: 0,
                    failed: 0,
                    last_7_days: 0,
                    last_24_hours: 0,
                    share: 0.0,
                    dwarf: false,
                    turn: 0.0,
                    stars: Vec::new(),
                    rgb: maker_rgb(&model.maker),
                    logo: maker_logo(&model.maker),
                });
                makers.len() - 1
            }
        };
        let maker = &mut makers[maker];
        maker.actions += model.actions;
        maker.failed += model.failed;
        maker.last_7_days += model.last_7_days;
        maker.last_24_hours += model.last_24_hours;
        maker.stars.push(at);
    }
    makers.sort_by(|a, b| b.actions.cmp(&a.actions).then_with(|| a.key.cmp(&b.key)));
    makers
}

/// A maker's galaxy colour.
pub fn maker_rgb(maker: &str) -> [u8; 3] {
    match maker {
        "anthropic" => [0xe0, 0x8a, 0x66],
        "openai" => [0x6a, 0xa6, 0xff],
        "google" => [0x36, 0xcf, 0xe6],
        "alibaba" | "qwen" => [0xff, 0x7a, 0xc6],
        "zhipu" | "zai" | "z-ai" => [0xc0, 0x9a, 0xff],
        "moonshot" | "moonshotai" => [0x9a, 0xd0, 0x8a],
        "mistral" => [0xff, 0x9a, 0x4a],
        "meta" => [0x5a, 0x8c, 0xff],
        _ => [150, 152, 170],
    }
}

/// The logo a maker's galaxy carries: its own where the brand folder has one, its own CLI's
/// where it makes one and has no logo of its own, else its colour dot.
pub fn maker_logo(maker: &str) -> Logo {
    let mark = match maker {
        "anthropic" => Some(LogoMark::ClaudeCode),
        "openai" => Some(LogoMark::OpenAi),
        "google" => Some(LogoMark::Antigravity),
        "alibaba" | "qwen" => Some(LogoMark::Qwen),
        "moonshot" | "moonshotai" => Some(LogoMark::Kimi),
        "mistral" => Some(LogoMark::Mistral),
        "meta" => Some(LogoMark::Meta),
        _ => None,
    };
    Logo {
        mark,
        ..Logo::dot(maker)
    }
}

/// The logo of whoever serves a model: its CLI for Claude Code, Codex and agy; for a model
/// OpenCode runs, the provider behind it, such as Ollama for a local model or Kimi, with its
/// colour dot where that provider has no logo. A model the facts know only by its CLI is the
/// CLI's own default, so OpenCode's frame stands for it.
pub fn served_logo(facts: &ModelFacts) -> Logo {
    let model = (facts.model != facts.agent).then_some(facts.model.as_str());
    logo_for(&facts.agent, model)
}

const GREY: [u8; 3] = [150, 152, 170];

fn star_of(models: &[ModelFacts], model: usize, maker: usize, colours: &[[u8; 3]]) -> Star {
    let facts = &models[model];
    let kind = if facts.actions >= rules::HOLE_MIN
        && facts.merged == 0
        && facts.sent_back + facts.dropped > 0
    {
        StarKind::BlackHole
    } else if facts.actions >= rules::GIANT_MIN && facts.last_7_days == 0 {
        StarKind::RedGiant
    } else if facts.actions < rules::PROTO_MAX && facts.first_used < rules::PROTO_DAYS {
        StarKind::Protostar
    } else {
        StarKind::Star
    };
    let pulse = if facts.last_hour > 0 {
        Some(rules::PULSE_HOUR)
    } else if facts.last_24_hours > 0 {
        Some(rules::PULSE_DAY)
    } else {
        None
    };
    let mut days: BTreeMap<Date, usize> = BTreeMap::new();
    for failure in &facts.cli_failures {
        *days.entry(failure.at.date()).or_default() += 1;
    }
    // The newest of the busiest days, so a later burst names its own day.
    let supernova = days
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(day, count)| (*count, *day))
        .filter(|(count, _)| *count >= rules::SUPERNOVA_FAILURES);
    Star {
        model,
        maker,
        kind,
        rgb: colours.get(model).copied().unwrap_or(GREY),
        logo: served_logo(facts),
        actions: facts.actions,
        failed: facts.failed,
        last_7_days: facts.last_7_days,
        pulse,
        supernova,
        binary: None,
        newer: None,
        system: System::default(),
        bodies: Vec::new(),
    }
}

/// The earlier months, at most twelve, scaled so the sky holds no more than its stars.
fn months_of(facts: &WorldFacts) -> Vec<Month> {
    let months: Vec<_> = facts
        .crew
        .earlier_months
        .iter()
        .filter(|m| m.actions > 0)
        .take(rules::MONTHS_MAX)
        .collect();
    let total: u64 = months.iter().map(|m| u64::from(m.actions)).sum();
    let scale = if total > u64::from(rules::EARLIER_MAX) {
        f64::from(rules::EARLIER_MAX) / total as f64
    } else {
        1.0
    };
    months
        .into_iter()
        .map(|m| Month {
            month: m.month.clone(),
            actions: m.actions,
            stars: ((f64::from(m.actions) * scale).round() as u32).max(1),
        })
        .collect()
}

/// The behavior a finer behavior belongs to: `edited a test file` for `edited a test file:
/// changed an assertion`.
fn parent_of(behavior: &str) -> Option<&str> {
    behavior.split_once(": ").map(|(parent, _)| parent)
}

fn system_of(facts: &ModelFacts, actions: &[StarAction], own: &[usize]) -> System {
    let behaviors = &facts.behaviors;
    let mut order: Vec<usize> = (0..behaviors.len()).collect();
    order.sort_by(|a, b| behaviors[*b].count.cmp(&behaviors[*a].count).then(a.cmp(b)));
    let used = |name: &str| behaviors.iter().any(|b| b.behavior == name);
    let mut planets: Vec<Planet> = Vec::new();
    for &at in &order {
        if planets.len() >= rules::PLANETS_MAX {
            break;
        }
        let behavior = &behaviors[at];
        // A finer behavior goes round its own behavior as a moon when the model did both.
        if parent_of(&behavior.behavior).is_some_and(used) {
            continue;
        }
        let share = f64::from(behavior.count) / f64::from(facts.actions.max(1));
        if behavior.count < rules::PLANET_MIN || share < rules::PLANET_SHARE {
            continue;
        }
        let lava = behavior.failed >= rules::LAVA_MIN
            && f64::from(behavior.failed) / f64::from(behavior.count) >= rules::LAVA_SHARE;
        planets.push(Planet {
            behavior: at,
            count: behavior.count,
            climate: Climate::of(behavior.last_used, behavior.days_in_last_7),
            lava,
            loops: behavior.runs_in_a_row,
            moons: Vec::new(),
            period: 0.0,
        });
    }
    let mut placed: Vec<usize> = planets.iter().map(|p| p.behavior).collect();
    for &at in &order {
        let Some(parent) = parent_of(&behaviors[at].behavior) else {
            continue;
        };
        if placed.contains(&at) {
            continue;
        }
        if let Some(planet) = planets
            .iter_mut()
            .find(|p| behaviors[p.behavior].behavior == parent)
        {
            planet.moons.push(Moon {
                behavior: at,
                count: behaviors[at].count,
            });
            placed.push(at);
        }
    }
    for (k, planet) in planets.iter_mut().enumerate() {
        // Each orbit further out takes 45% longer, so the system never turns as one wheel.
        let seconds = planet.climate.orbit_seconds();
        planet.period = (seconds * (1.0 + 0.45 * k as f64)).round();
    }
    let on_planets = |name: &str| placed.iter().any(|at| behaviors[*at].behavior == name);
    let belt = behaviors
        .iter()
        .filter(|b| !on_planets(&b.behavior))
        .map(|b| b.count)
        .sum();
    let rocks = own
        .iter()
        .copied()
        .filter(|at| !on_planets(&actions[*at].behavior))
        .collect();
    let flares = (0..facts.cli_failures.len().min(rules::FLARES_MAX)).collect();
    let share = f64::from(facts.failed) / f64::from(facts.actions.max(1));
    let sunspots = if facts.failed > 0 {
        ((share / rules::SUNSPOT_SHARE).round() as u32).clamp(1, rules::SUNSPOTS_MAX)
    } else {
        0
    };
    System {
        planets,
        belt,
        rocks,
        flares,
        sunspots,
    }
}

/// A seeded stream for something of the universe named `name`, the same in every run.
pub(super) fn rng_for(name: &str, salt: u32) -> Rng {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    Rng::new(hash ^ salt.wrapping_mul(0x9e37_79b9))
}
