use argonauts_core::display_names::DisplayNames;

use super::crews::world;
use super::*;
use crate::world::map::fields::HEIGHT;
use crate::world::map::fields::crews::{Crew, Key};

/// Where each made-up crew's nature stands, in the order it is placed: crew, kind, behavior, x,
/// y, radius, reach, the points of its path, and its line.
const PLACED: &str = include_str!("placed.txt");

/// A CLI as a line names it where no logos show: by its name.
fn named(cli: &str) -> String {
    DisplayNames::default().cli(cli)
}

/// Each crew's nature as `placed.txt` writes it, a line a feature.
fn placed_now() -> String {
    let mut text = String::new();
    for key in Key::ALL {
        let world = world(key);
        for feature in &world.nature.features {
            let points = feature.stream().map_or(0, |stream| stream.path.len());
            let said = feature.line(&world.ground, &world.facts, &mut named);
            text.push_str(&format!(
                "{key:?} {} {} {:.3} {:.3} {:.3} {:.3} {points} | {said}\n",
                feature.kind.word(),
                feature.behavior,
                feature.x,
                feature.y,
                feature.radius,
                feature.reach,
            ));
        }
    }
    text
}

/// Whether two lines of `placed.txt` say the same: the same words, and numbers within a
/// hundredth, as two machines' floating point may not agree in the last place.
fn same_place(one: &str, other: &str) -> bool {
    let (one, one_said) = one.split_once(" | ").unwrap_or((one, ""));
    let (other, other_said) = other.split_once(" | ").unwrap_or((other, ""));
    let (one, other): (Vec<&str>, Vec<&str>) =
        (one.split(' ').collect(), other.split(' ').collect());
    one_said == other_said
        && one.len() == other.len()
        && one.iter().zip(&other).all(|(a, b)| {
            a == b
                || matches!((a.parse::<f64>(), b.parse::<f64>()), (Ok(a), Ok(b)) if (a - b).abs() < 0.01)
        })
}

/// Where each crew's nature stands and what it says, as `placed.txt` has it. When it moves,
/// the failure gives the whole file as it now reads, to look over and keep.
#[test]
fn each_crews_nature_stands_where_it_is_placed_and_says_why() {
    let now = placed_now();
    let (now_lines, placed): (Vec<&str>, Vec<&str>) =
        (now.lines().collect(), PLACED.lines().collect());
    let moved = now_lines
        .iter()
        .zip(&placed)
        .position(|(one, other)| !same_place(one, other))
        .or_else(|| (now_lines.len() != placed.len()).then(|| now_lines.len().min(placed.len())));
    if let Some(at) = moved {
        panic!(
            "line {} of placed.txt isn't as placed; the file now reads:\n{now}",
            at + 1
        );
    }
}

#[test]
fn a_shoal_lies_where_an_unused_place_falls_at_sea() {
    let expected: [(Key, &[&str]); 2] = [
        (
            Key::R,
            &[
                "failed to read a file",
                "edited a test file: changed an assertion",
                "asked you a question",
                "presented a plan",
                "ran a Python snippet",
            ],
        ),
        (
            Key::S,
            &[
                "failed to read a file",
                "wrote a config file",
                "asked you a question",
                "presented a plan",
                "listed Argonauts' own run records",
            ],
        ),
    ];
    for (key, names) in expected {
        let shoals = &world(key).nature.shoals;
        let got: Vec<&str> = shoals.iter().map(|shoal| shoal.name.as_str()).collect();
        assert_eq!(&got[..names.len()], names, "{key:?}");
        for shoal in shoals {
            assert!(
                world(key).ground.land_at(shoal.x, shoal.y) < 0.7,
                "{}",
                shoal.name
            );
        }
    }
    let first = &world(Key::R).nature.shoals[0];
    assert!((first.x - 196.931).abs() < 0.01 && (first.y - 201.631).abs() < 0.01);
    assert_eq!(
        first.line(),
        "Shoal · on the list, never used · ‘failed to read a file’"
    );
    assert_eq!(world(Key::S).nature.shoals.len(), 24);
    assert_eq!(world(Key::T).nature.shoals.len(), 29);
}

#[test]
fn the_shoals_all_lie_along_the_wind() {
    let (wx, wy) = wind();
    let along = wy.atan2(wx);
    let shoals = &world(Key::T).nature.shoals;
    assert!(shoals.len() >= 10);
    for shoal in shoals {
        let off = (shoal.angle - along)
            .sin()
            .atan2((shoal.angle - along).cos());
        assert!(off.abs() <= 0.27, "{} lies {off} off the wind", shoal.name);
    }
}

#[test]
fn crew_r_has_nature_of_many_kinds() {
    let mut kinds: Vec<&str> = world(Key::R)
        .nature
        .features
        .iter()
        .map(|feature| feature.kind.word())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert!(kinds.len() >= 8, "{kinds:?}");
}

#[test]
fn a_volcano_sits_on_land_by_a_behavior_whose_actions_failed() {
    let world = world(Key::R);
    let volcano = world
        .nature
        .features
        .iter()
        .find(|feature| matches!(feature.kind, Kind::Volcano { .. }))
        .expect("crew R has a volcano");
    assert!(world.ground.land_at(volcano.x, volcano.y) >= 1.0);
    let Kind::Volcano { failed, .. } = volcano.kind else {
        unreachable!()
    };
    assert!(failed >= 3);
}

#[test]
fn a_hurricane_is_25_or_more_actions_within_an_hour_and_turns_off_the_equator() {
    let mut seen = 0;
    for key in Key::ALL {
        let world = world(key);
        for feature in &world.nature.features {
            let Kind::Hurricane(storm) = feature.kind else {
                continue;
            };
            seen += 1;
            let busiest = world
                .facts
                .of(feature.behavior)
                .busiest_hour
                .map(|b| b.count);
            assert_eq!(busiest, Some(storm.actions));
            assert!(storm.actions >= 25);
            assert!((feature.y - HEIGHT as f64 / 2.0).abs() >= 16.0);
            assert_eq!(storm.south, feature.y > HEIGHT as f64 / 2.0);
        }
    }
    assert!(seen >= 4);
}

#[test]
fn a_river_runs_over_land_from_one_behavior_to_the_next() {
    for key in Key::ALL {
        let world = world(key);
        for feature in &world.nature.features {
            if let Kind::River(stream) = &feature.kind {
                let wet = stream
                    .path
                    .iter()
                    .filter(|(x, y)| world.ground.land_at(*x, *y) < 1.0)
                    .count();
                assert!(wet * 10 <= stream.path.len(), "{key:?}: {wet} wet points");
                assert_eq!(
                    stream.path.first(),
                    Some(&world.ground.centre[feature.behavior])
                );
                assert_eq!(stream.path.last(), Some(&world.ground.centre[stream.to]));
            }
        }
    }
}

#[test]
fn a_crash_in_the_last_day_shakes_the_ground_and_an_older_one_leaves_a_fault() {
    let world = world(Key::S);
    let quake = world
        .nature
        .features
        .iter()
        .find_map(|feature| match &feature.kind {
            Kind::Rift(rift) if rift.shaking() => Some((feature, rift)),
            _ => None,
        })
        .expect("crew S has an earthquake");
    let crashes = &world.facts.of(quake.0.behavior).crashes;
    let day = time::Duration::DAY;
    assert_eq!(
        quake.1.recent.len(),
        crashes
            .iter()
            .filter(|c| world.facts.end - c.at <= day)
            .count()
    );
    assert!(quake.0.is_flat());
    for feature in &world.nature.features {
        if let Kind::Rift(rift) = &feature.kind
            && !rift.shaking()
        {
            assert!(rift.last.is_some_and(|at| world.facts.end - at > day));
            assert!(!feature.is_flat());
        }
    }
}

#[test]
fn the_same_actions_always_make_the_same_nature() {
    let crew = Crew::get(Key::U);
    let ground = crate::world::map::fields::Ground::grow(crew.facts());
    let facts = super::crews::facts(crew);
    assert_eq!(Nature::find(&ground, &facts), world(Key::U).nature);
}

#[test]
fn the_storms_word_follows_the_time_zone() {
    use argonauts_core::behavior_world::StormWord;
    let world = world(Key::R);
    let storm = world
        .nature
        .features
        .iter()
        .find(|feature| matches!(feature.kind, Kind::Hurricane(_)))
        .expect("crew R has a hurricane");
    for (word, start) in [
        (StormWord::Typhoon, "Typhoon, category"),
        (StormWord::Cyclone, "Cyclone, category"),
        (StormWord::Hurricane, "Hurricane, category"),
    ] {
        let facts = NatureFacts {
            storm: word,
            ..world.facts.clone()
        };
        assert!(
            storm
                .line(&world.ground, &facts, &mut named)
                .starts_with(start)
        );
    }
}

#[test]
fn a_gas_giant_has_no_nature_yet() {
    let crew = Crew::get(Key::R);
    let mut facts = crew.facts();
    facts.dots.truncate(30);
    let ground = crate::world::map::fields::Ground::grow(facts);
    assert_eq!(ground.look, crate::world::map::fields::Look::Gas);
    let nature = Nature::find(&ground, &super::crews::facts(crew));
    assert!(nature.features.is_empty() && nature.shoals.is_empty());
}

#[test]
fn a_tsunami_runs_over_open_sea_to_its_own_behaviors_coast() {
    let mut seen = 0;
    for key in Key::ALL {
        let world = world(key);
        for feature in &world.nature.features {
            let Kind::Tsunami(wave) = feature.kind else {
                continue;
            };
            seen += 1;
            for step in 0..=20 {
                let u = f64::from(step) / 20.0;
                let x = wave.from.0 + (wave.to.0 - wave.from.0) * u;
                let y = wave.from.1 + (wave.to.1 - wave.from.1) * u;
                assert!(world.ground.on_sea(x, y), "{key:?}: the wave crosses land");
            }
            let inside = (wave.to.0 - wave.out.0 * 14.0, wave.to.1 - wave.out.1 * 14.0);
            assert!(world.ground.land_at(inside.0, inside.1) >= 0.95);
            assert_eq!(wave.size, f64::from((40 + 3 * (wave.agents - 6)).min(60)));
        }
    }
    assert!(seen >= 4);
}

#[test]
fn the_apps_facts_line_up_with_the_grounds_behaviors_by_name() {
    use argonauts_core::behavior_map::BehaviorMap;
    use argonauts_core::behavior_world::{BehaviorFacts, CrewFacts, StormWord, WorldFacts};
    use time::macros::datetime;

    use crate::world::map::fields::{Ground, GroundFacts};

    let end = datetime!(2026-10-20 18:00 UTC);
    let world = WorldFacts {
        end,
        behaviors: vec![BehaviorFacts {
            behavior: "ran a shell command".to_owned(),
            stopped: 3,
            last_period: 5,
            ..BehaviorFacts::default()
        }],
        crew: CrewFacts {
            storm: StormWord::Typhoon,
            ..CrewFacts::default()
        },
        ..WorldFacts::default()
    };
    let map = BehaviorMap {
        unlabelled: 7,
        world: Some(Box::new(world)),
        ..BehaviorMap::default()
    };
    let ground = Ground::grow(GroundFacts::from_map(&map, 41));
    let facts = NatureFacts::from_map(&map, &ground);
    let at = ground
        .facts
        .behaviors
        .iter()
        .position(|days| days.name == "ran a shell command")
        .expect("the behavior the facts name");
    assert_eq!(facts.of(at).stopped, 3);
    assert_eq!(facts.end, end);
    assert_eq!(facts.storm, StormWord::Typhoon);
    assert_eq!(facts.unlabelled, 7);
    let unused: Vec<&str> = facts.unused.iter().map(|u| u.name.as_str()).collect();
    assert!(!unused.contains(&"ran a shell command"));
    assert_eq!(unused.first(), Some(&"read a source file"));
    assert_eq!(
        facts.unused[0].place,
        (0.10_f32 as f64, 1.0 - 0.14_f32 as f64)
    );
    assert_eq!(facts.of(99), &BehaviorFacts::default());
}

/// Crew `key`'s land stood up with nature's marks on its ground, and its dots over it.
fn standing_with_marks(key: Key) -> crate::world::raster::Canvas {
    use crate::world::map::land::Polar;
    use crate::world::map::relief::cells::composed;
    use crate::world::map::relief::{
        DotStyle, Dots, Light, LitGround, Projected, Relief, STANDING,
    };

    let world = world(key);
    let ground = &world.ground;
    let marks = GroundMarks::new(ground, &world.nature, true, None);
    let relief = Relief::new(ground);
    let polar = Polar::of(ground);
    let light = Light::new(ground, &relief, STANDING);
    let lit = LitGround::new(ground, &relief, &light, None, marks.marks(), Some(&polar));
    let land = Projected::new(&relief, &lit, STANDING, 2);
    let colours: Vec<_> = Crew::get(key)
        .groups
        .iter()
        .map(|group| group.rgb.map(f64::from))
        .collect();
    let style = DotStyle {
        groups: &colours,
        compared: None,
    };
    let dots = Dots::new(ground, &relief, style, STANDING);
    composed(&land, &dots)
}

#[test]
fn crew_rs_ground_carries_its_water_lakes_scars_and_faults() {
    use crate::world::map::relief::cells::compare;
    compare(
        "nature-ground-r",
        &standing_with_marks(Key::R),
        include_str!("golden/nature-ground-r.txt"),
    );
}

#[test]
fn crew_ts_ground_carries_its_burn_scars_atolls_and_shoals() {
    use crate::world::map::relief::cells::compare;
    compare(
        "nature-ground-t",
        &standing_with_marks(Key::T),
        include_str!("golden/nature-ground-t.txt"),
    );
}

#[test]
fn an_ocean_current_is_a_band_only_at_sea_as_wide_as_its_move_is_common() {
    let mut stretches = 0;
    for key in Key::ALL {
        let world = world(key);
        for feature in &world.nature.features {
            let Kind::Current(stream) = &feature.kind else {
                continue;
            };
            for run in current_runs(&world.ground, stream) {
                stretches += 1;
                assert!(
                    run.points
                        .iter()
                        .all(|(x, y)| world.ground.land_at(*x, *y) < 0.9)
                );
                assert!((run.band - (8.0 + 2.4 * f64::from(stream.moves).sqrt())).abs() < 1e-9);
            }
        }
    }
    assert!(stretches >= 4, "{stretches} stretches of current");
}

#[test]
fn an_atoll_is_a_ring_of_white_sand_round_a_turquoise_lagoon() {
    let world = world(Key::T);
    let marks = GroundMarks::new(&world.ground, &world.nature, true, None);
    let laid = marks.laid().expect("nature shows");
    let pixel = |x: f64, y: f64| {
        let at = ((y.round() as usize) * crate::world::map::fields::WIDTH + x.round() as usize) * 4;
        [laid[at], laid[at + 1], laid[at + 2]]
    };
    let atolls: Vec<&Feature> = world
        .nature
        .features
        .iter()
        .filter(|feature| matches!(feature.kind, Kind::Atoll))
        .collect();
    assert!(atolls.len() >= 2);
    for atoll in atolls {
        let middle = pixel(atoll.x, atoll.y);
        assert!(
            middle[1] > 170 && middle[2] > 170 && middle[0] < 150,
            "{middle:?}"
        );
        let r = atoll.radius;
        let on_sand = (0..6)
            .map(|k| {
                let a = f64::from(k) + 0.4;
                pixel(
                    atoll.x + a.cos() * r * 0.84 * 1.12,
                    atoll.y + a.sin() * r * 0.84 * 0.9,
                )
            })
            .filter(|c| c[0] > 190 && c[1] > 180)
            .count();
        assert!(on_sand >= 3, "{on_sand} of 6 on sand");
    }
}

#[test]
fn a_shoal_is_a_pale_bank_under_the_water_palest_in_its_middle() {
    let world = world(Key::T);
    let abyss = [10.0, 22.0, 42.0];
    let light = |c: [f64; 3]| 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
    let deep: Vec<&Shoal> = world
        .nature
        .shoals
        .iter()
        .filter(|shoal| world.ground.land_at(shoal.x, shoal.y) < 0.3)
        .take(6)
        .collect();
    assert!(deep.len() >= 3);
    for shoal in deep {
        let middle = light(shoal_at(shoal, shoal.x, shoal.y, abyss));
        assert!(middle > light(abyss) + 40.0, "{}", shoal.name);
        for k in 0..8 {
            let a = f64::from(k);
            let round = shoal_at(
                shoal,
                shoal.x + a.cos() * 11.0,
                shoal.y + a.sin() * 11.0,
                abyss,
            );
            assert!(light(round) <= middle, "{} has a ring", shoal.name);
        }
    }
}

#[test]
fn a_lake_is_fresh_water_lighter_than_any_sea() {
    let surf = [34.0, 92.0, 108.0];
    let light = |c: [f64; 3]| 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
    let mut lakes = 0;
    for key in [Key::R, Key::S, Key::T] {
        let world = world(key);
        let marks = GroundMarks::new(&world.ground, &world.nature, true, None);
        let laid = marks.laid().expect("nature shows");
        for lake in world
            .nature
            .features
            .iter()
            .filter(|feature| matches!(feature.kind, Kind::Lake { .. }))
        {
            lakes += 1;
            let (x, y) = (
                lake.x.round() as usize,
                (lake.y + lake.radius * 0.3).round() as usize,
            );
            let at = (y * crate::world::map::fields::WIDTH + x) * 4;
            let colour = [laid[at], laid[at + 1], laid[at + 2]].map(f64::from);
            assert!(light(colour) > light(surf) + 30.0, "{key:?}: {colour:?}");
        }
    }
    assert!(lakes >= 2);
}

#[test]
fn hidden_nature_leaves_only_the_shoals() {
    let world = world(Key::R);
    let hidden = GroundMarks::new(&world.ground, &world.nature, false, None);
    let marks = hidden.marks();
    assert!(marks.ground.is_none() && marks.scar.is_none() && !marks.icebergs);
    let shoal = &world.nature.shoals[0];
    let abyss = [10.0, 22.0, 42.0];
    let under = marks.under_water.expect("the shoals stay");
    assert_ne!(under(shoal.x, shoal.y, abyss), abyss);
}
