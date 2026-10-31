//! Nature's facts for the made-up crews, worked out from each action's made-up detail the way the
//! app works them out from its records, and each crew's world grown and found once for the tests.

use std::sync::OnceLock;

use argonauts_core::behaviors::LABELS;
use argonauts_core::behaviors::behavior_world::{BehaviorFacts, Burst, Crash, RisenWonder, Wonder};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use super::{Nature, NatureFacts, Unused};
use crate::world::map::fields::Ground;
use crate::world::map::fields::crews::detail::{Ended, Outcome};
use crate::world::map::fields::crews::{Crew, Key};

/// Midnight before each made-up crew's first day.
pub(crate) const START: OffsetDateTime = datetime!(2026-08-03 0:00 UTC);

/// A crew's ground, nature's facts and what nature found on it.
pub(crate) struct World {
    pub ground: Ground,
    pub facts: NatureFacts,
    pub nature: Nature,
}

/// Each crew's world, grown once and kept.
pub(crate) fn world(key: Key) -> &'static World {
    static WORLDS: OnceLock<Vec<World>> = OnceLock::new();
    let worlds = WORLDS.get_or_init(|| {
        Key::ALL
            .iter()
            .map(|key| {
                let crew = Crew::get(*key);
                let ground = Ground::grow(crew.facts());
                let facts = facts(crew);
                let nature = Nature::find(&ground, &facts);
                World {
                    ground,
                    facts,
                    nature,
                }
            })
            .collect()
    });
    &worlds[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
}

/// When a crew's action happened, from its day and its minute, as its record keeps them. A late
/// session's minutes can run past midnight; the action still counts on the day it began.
pub(crate) fn action_at(crew: &Crew, action: usize) -> OffsetDateTime {
    let detail = &crew.detail;
    let minute = f64::from(detail.minute[action]).min(1439.0);
    let minutes = f64::from((detail.period - 1 - detail.day[action]) * 1440) + minute;
    START + Duration::seconds_f64(minutes * 60.0)
}

/// A crew's nature facts, every label a behavior in the list's order.
pub(crate) fn facts(crew: &Crew) -> NatureFacts {
    let detail = &crew.detail;
    let period = detail.period;
    let latest = detail.time.iter().copied().fold(f32::MIN, f32::max);
    let now = f64::from((period - 1) * 1440 + 20 * 60).max(f64::from(latest));
    let end = START + Duration::seconds_f64(now * 60.0);
    let at = |action: usize| action_at(crew, action);
    let labels = crew.labels.len();
    let mut behaviors: Vec<BehaviorFacts> = crew
        .labels
        .iter()
        .enumerate()
        .map(|(label, name)| {
            let prior = crew
                .last_period
                .iter()
                .find(|(l, _)| *l == label)
                .map_or(0, |(_, count)| *count);
            let before = if crew.habits.before.contains(&label) {
                10
            } else {
                0
            };
            BehaviorFacts {
                behavior: name.clone(),
                last_period: prior.max(before),
                ..BehaviorFacts::default()
            }
        })
        .collect();

    for (label, facts) in behaviors.iter_mut().enumerate() {
        let mut times: Vec<(f32, usize)> = (0..crew.len())
            .filter(|action| crew.l[*action] == label)
            .map(|action| (detail.time[action], action))
            .collect();
        facts.count = times.len() as u32;
        times.sort_by(|p, q| p.0.total_cmp(&q.0));
        let (mut best, mut best_at, mut start) = (0, 0, 0);
        for end in 0..times.len() {
            while times[end].0 - times[start].0 > 60.0 {
                start += 1;
            }
            if end - start + 1 > best {
                best = end - start + 1;
                best_at = times[start].1;
            }
        }
        if best > 0 {
            facts.busiest_hour = Some(Burst {
                count: best as u32,
                at: at(best_at),
            });
        }
        let (mut fan, mut fan_at) = (0, 0);
        for start in 0..times.len() {
            let mut sessions: Vec<usize> = Vec::new();
            for end in start..times.len() {
                if times[end].0 - times[start].0 > 5.0 {
                    break;
                }
                let session = detail.session[times[end].1];
                if !sessions.contains(&session) {
                    sessions.push(session);
                }
            }
            if sessions.len() > fan {
                fan = sessions.len();
                fan_at = times[start].1;
            }
        }
        if fan > 0 {
            facts.most_agents_in_5_minutes = Some(Burst {
                count: fan as u32,
                at: at(fan_at),
            });
        }
    }

    let mut crashes: Vec<(usize, Crash)> = Vec::new();
    for session in &detail.sessions {
        let actions = &session.actions;
        let mut q = 0;
        while q < actions.len() {
            let mut e = q;
            while e + 1 < actions.len() && crew.l[actions[e + 1]] == crew.l[actions[q]] {
                e += 1;
            }
            let run = (e - q + 1) as u32;
            let facts = &mut behaviors[crew.l[actions[q]]];
            if run >= 5 {
                facts.runs_in_a_row += 1;
                facts.longest_run = facts.longest_run.max(run);
            }
            q = e + 1;
        }
        let Some(&last) = actions.last() else {
            continue;
        };
        let facts = &mut behaviors[crew.l[last]];
        facts.session_ends += 1;
        match session.ended {
            Ended::Failed => {
                facts.cli_failed += 1;
                let model = &crew.groups[crew.g[last]].name;
                crashes.push((
                    crew.l[last],
                    Crash {
                        at: at(last),
                        agent: cli_of(model).to_owned(),
                        model: Some(model.clone()),
                    },
                ));
            }
            Ended::Stopped => facts.stopped += 1,
            Ended::Completed => {}
        }
    }
    crashes.sort_by_key(|(_, crash)| std::cmp::Reverse(crash.at));
    for (label, crash) in crashes {
        behaviors[label].crashes.push(crash);
    }

    for action in 0..crew.len() {
        let facts = &mut behaviors[crew.l[action]];
        let day = detail.day[action];
        let sooner = |last: Option<u32>| Some(last.map_or(day, |last| last.min(day)));
        if !detail.ok[action] {
            facts.failed += 1;
            facts.last_failed = sooner(facts.last_failed);
        }
        match detail.outcome[action] {
            Outcome::SentBack => facts.sent_back += 1,
            Outcome::Dropped => {
                facts.dropped += 1;
                facts.last_dropped = sooner(facts.last_dropped);
            }
            Outcome::Merged | Outcome::None => {}
        }
    }
    debug_assert_eq!(behaviors.len(), labels);

    NatureFacts {
        behaviors,
        end,
        storm: Default::default(),
        unlabelled: match crew.key {
            Key::R | Key::U => 38,
            Key::S => 12,
            Key::T => 90,
        },
        unused: LABELS
            .iter()
            .filter(|label| !crew.labels.iter().any(|name| name == label.words))
            .map(|label| Unused {
                name: label.words.to_owned(),
                place: (f64::from(label.place.0), 1.0 - f64::from(label.place.1)),
            })
            .collect(),
    }
}

/// The CLI each made-up crew's model runs under, by its id, as the app's facts name it.
fn cli_of(model: &str) -> &'static str {
    match model {
        "Haiku 5.5" | "Opus 5.5" | "Sonnet 5.5" => "claude-code",
        "Gemini 3.8 Flash" => "antigravity-cli",
        "Gemma 3 12B" | "Qwen3 Coder 30B" | "GLM 5" | "Kimi K2.6" => "opencode",
        _ => "codex",
    }
}

/// A wonder risen by a behavior, with made-up words.
fn risen(wonder: Wonder, behavior: &str) -> RisenWonder {
    RisenWonder {
        wonder,
        behavior: behavior.to_owned(),
        risen: START,
        why: "made up for a test".to_owned(),
        models: Vec::new(),
    }
}

/// The wonders each made-up crew has raised, by the behaviors they stand by.
pub(crate) fn raised(key: Key) -> Vec<RisenWonder> {
    let mut list = vec![
        risen(Wonder::Argo, "wrote a workflow script"),
        risen(Wonder::Symplegades, "ran the test suite, which failed"),
        risen(Wonder::Pyramid, "returned its structured answer"),
        risen(Wonder::Gardens, "edited a test file: added tests"),
        risen(Wonder::Colossus, "started a subagent"),
        risen(Wonder::Pharos, "built the project"),
    ];
    match key {
        Key::R => list,
        Key::U => {
            list.push(risen(Wonder::Atlantis, "changed git state"));
            list
        }
        Key::S => vec![
            risen(Wonder::Symplegades, "ran the test suite, which failed"),
            risen(Wonder::Pyramid, "ran the test suite"),
            risen(Wonder::Gardens, "edited a test file: added tests"),
            risen(Wonder::Pharos, "ran the test suite"),
            risen(Wonder::Zeus, "asked another agent to review or judge"),
        ],
        Key::T => vec![
            risen(Wonder::Fleece, "read a source file"),
            risen(Wonder::Zeus, "asked another agent to review or judge"),
        ],
    }
}

/// A crew's map as drawn standing: its relief, the lit land with nature's marks on its ground,
/// its dots, its wonders and its weather.
pub(crate) struct Drawn {
    pub wonders: Vec<crate::world::wonders::Placed>,
    pub relief: crate::world::map::relief::Relief,
    pub land: crate::world::map::relief::Projected,
    pub dots: crate::world::map::relief::Dots,
    pub weather: crate::world::map::weather::Weather,
}

/// Each crew's map drawn standing, once and kept.
pub(crate) fn drawn(key: Key) -> &'static Drawn {
    use crate::world::map::land::Polar;
    use crate::world::map::relief::{
        DotStyle, Dots, Light, LitGround, Projected, Relief, STANDING,
    };
    use crate::world::map::weather::Weather;

    static DRAWN: OnceLock<Vec<Drawn>> = OnceLock::new();
    let all = DRAWN.get_or_init(|| {
        Key::ALL
            .iter()
            .map(|key| {
                let world = world(*key);
                let ground = &world.ground;
                let risen = raised(*key);
                let sunk = crate::world::wonders::sunk(ground, &world.facts, &risen);
                let marks = super::GroundMarks::new(ground, &world.nature, true, sunk);
                let relief = Relief::new(ground);
                let polar = Polar::of(ground);
                let light = Light::new(ground, &relief, STANDING);
                let lit =
                    LitGround::new(ground, &relief, &light, None, marks.marks(), Some(&polar));
                let land = Projected::new(&relief, &lit, STANDING, 2);
                let colours: Vec<_> = Crew::get(*key)
                    .groups
                    .iter()
                    .map(|group| group.rgb.map(f64::from))
                    .collect();
                let style = DotStyle {
                    groups: &colours,
                    compared: None,
                };
                let dots = Dots::new(ground, &relief, style, STANDING);
                let layers = crate::world::map::weather::FlatLayers::new(
                    ground,
                    &world.nature,
                    crate::world::map::weather::Keep::All,
                    sunk,
                );
                let wonders = crate::world::wonders::place(ground, &world.nature, &layers, &risen);
                let weather = Weather::new(ground, &relief, &world.nature, &land, &wonders);
                Drawn {
                    wonders,
                    relief,
                    land,
                    dots,
                    weather,
                }
            })
            .collect()
    });
    &all[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
}

/// A crew's whole map at a moment: the land, what moves and stands on it, the dots, and what
/// passes over them.
pub(crate) fn map_at(
    key: Key,
    moment: crate::world::map::motion::Moment,
) -> crate::world::raster::Canvas {
    use crate::world::raster::{Brush, Colour};
    let drawn = drawn(key);
    let mut picture = drawn.land.picture.clone();
    let white = Brush::new(Colour::WHITE);
    picture.draw(&drawn.weather.under_dots(moment), 0.0, 0.0, &white);
    picture.draw(&drawn.dots.picture, 0.0, 0.0, &white);
    picture.draw(&drawn.weather.over_dots(moment), 0.0, 0.0, &white);
    picture
}

/// A crew's whole map laid flat, seen from straight above, held still: the land, its nature,
/// the dots and the weather, all at their own places.
pub(crate) fn flat_map(key: Key) -> crate::world::raster::Canvas {
    use crate::world::map::land::Polar;
    use crate::world::map::motion::Moment;
    use crate::world::map::relief::{DotStyle, Dots, Light, LitGround, Projected, Relief};
    use crate::world::map::weather::{FlatLayers, Keep, Weather};
    use crate::world::raster::{Brush, Colour};

    let world = world(key);
    let ground = &world.ground;
    let risen = raised(key);
    let sunk = crate::world::wonders::sunk(ground, &world.facts, &risen);
    let marks = super::GroundMarks::new(ground, &world.nature, true, sunk);
    let relief = Relief::new(ground);
    let polar = Polar::of(ground);
    let light = Light::new(ground, &relief, 0.0);
    let lit = LitGround::new(ground, &relief, &light, None, marks.marks(), Some(&polar));
    let land = Projected::new(&relief, &lit, 0.0, 2);
    let colours: Vec<_> = Crew::get(key)
        .groups
        .iter()
        .map(|group| group.rgb.map(f64::from))
        .collect();
    let style = DotStyle {
        groups: &colours,
        compared: None,
    };
    let dots = Dots::new(ground, &relief, style, 0.0);
    let layers = FlatLayers::new(ground, &world.nature, Keep::All, sunk);
    let wonders = crate::world::wonders::place(ground, &world.nature, &layers, &risen);
    let weather = Weather::new(ground, &relief, &world.nature, &land, &wonders);
    let white = Brush::new(Colour::WHITE);
    let mut picture = land.picture.clone();
    picture.draw(&weather.under_dots(Moment::Still), 0.0, 0.0, &white);
    picture.draw(&dots.picture, 0.0, 0.0, &white);
    picture.draw(&weather.over_dots(Moment::Still), 0.0, 0.0, &white);
    picture
}
