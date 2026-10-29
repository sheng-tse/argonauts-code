use std::sync::OnceLock;

use super::*;
use crate::world::map::fields::crews::{Crew, Key};
use crate::world::map::fields::{COARSE, HEIGHT, WIDTH};
use crate::world::map::land::{coast_at, stretch_at};

struct Laid {
    ground: Ground,
    placer: Placer,
    marks: Vec<LandMark>,
}

fn placed(key: Key) -> &'static Laid {
    static PLACED: OnceLock<Vec<Laid>> = OnceLock::new();
    let all = PLACED.get_or_init(|| {
        Key::ALL
            .iter()
            .map(|key| {
                let ground = Ground::grow(Crew::get(*key).facts());
                let mut placer = Placer::new(&ground);
                let marks = land_marks(&ground, &mut placer);
                Laid {
                    ground,
                    placer,
                    marks,
                }
            })
            .collect()
    });
    &all[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
}

fn of_kind(placed: &Laid, want: fn(&LandKind) -> bool) -> Vec<&LandMark> {
    placed
        .marks
        .iter()
        .filter(|mark| want(&mark.kind))
        .collect()
}

fn name(placed: &Laid, behavior: usize) -> &str {
    &placed.ground.facts.behaviors[behavior].name
}

#[test]
fn a_swamp_is_where_agents_waited_and_its_low_ground_is_wet() {
    let expected = [
        (Key::R, "ran a shell command", 7, 10),
        (Key::S, "built the project", 16, 60),
        (Key::T, "fetched from the web", 31, 75),
        (Key::U, "searched code by pattern", 6, 16),
    ];
    for (key, words, waits, of) in expected {
        let placed = placed(key);
        let swamps = of_kind(placed, |kind| matches!(kind, LandKind::Swamp { .. }));
        let swamp = swamps
            .iter()
            .find(|mark| name(placed, mark.behavior) == words)
            .unwrap_or_else(|| panic!("{key:?} has a swamp by {words}"));
        assert_eq!(swamp.kind, LandKind::Swamp { waited: waits });
        assert_eq!(placed.ground.count[swamp.behavior], of);
        assert_eq!(
            swamp.line(&placed.ground),
            format!(
                "Swamp · {waits} of its {of} ‘{words}’ actions came after a wait of 10 minutes \
                 or more in the same session"
            )
        );
        let mut canvas = Canvas::new(WIDTH as u32, HEIGHT as u32);
        swamp.draw(&placed.ground, &mut canvas);
        let (mut drawn, mut sampled) = (0, 0);
        for step in 0..32 {
            let a = f64::from(step) * 0.2;
            for r in (2..swamp.radius as i32).step_by(3) {
                let x = (swamp.x + a.cos() * f64::from(r)).round();
                let y = (swamp.y + a.sin() * f64::from(r)).round();
                sampled += 1;
                drawn += usize::from(canvas.pixel(x as u32, y as u32)[3] > 120);
            }
        }
        assert!(
            drawn * 2 > sampled,
            "{key:?}: {drawn} of {sampled} points wet"
        );
    }
}

#[test]
fn a_plateau_answers_the_pointer_on_its_mesa() {
    let placed = placed(Key::S);
    let plateaus = of_kind(placed, |kind| matches!(kind, LandKind::Plateau { .. }));
    assert_eq!(plateaus.len(), 1);
    let plateau = plateaus[0];
    assert_eq!(
        plateau.line(&placed.ground),
        "Plateau · ‘tried an edit that didn't apply’ · 55 actions, 11 to 17 in every week of \
         the period: a steady habit"
    );
    let (cx, cy) = placed.ground.centre[plateau.behavior];
    let found = (0..21).any(|step| {
        let a = f64::from(step) * 0.3;
        placed
            .ground
            .plateau_at(cx + a.cos() * 30.0, cy + a.sin() * 30.0)
            == Some(0)
    });
    assert!(found);
    assert_eq!(placed.ground.plateau_at(20.0, 20.0), None);
}

#[test]
fn a_plateaus_flat_top_shows_round_its_dots() {
    let ground = &placed(Key::S).ground;
    let plateau = &ground.plateaus[0];
    let mesa = ground.mesa.as_ref().expect("a mesa");
    let top = mesa.values().iter().filter(|v| **v >= 0.999).count() as f64;
    let cliff = mesa
        .values()
        .iter()
        .filter(|v| **v > 0.02 && **v < 0.999)
        .count() as f64;
    let cell = (COARSE * COARSE) as f64;
    let (cx, cy) = ground.centre[plateau.behavior];
    let mut out: Vec<f64> = ground
        .facts
        .dots
        .iter()
        .zip(&ground.dots)
        .filter(|(dot, _)| dot.behavior == plateau.behavior)
        .map(|(_, &(x, y))| (f64::from(x) - cx).hypot(f64::from(y) - cy))
        .collect();
    out.sort_by(f64::total_cmp);
    let r80 = out[(out.len() as f64 * 0.8) as usize];
    let top_area = top * cell;
    let dots_area = std::f64::consts::PI * r80 * r80;
    let cliff_width = cliff * cell / (2.0 * (std::f64::consts::PI * top_area).sqrt());
    assert_eq!(top_area, 3536.0);
    assert!(top_area >= 1.5 * dots_area, "{top_area} over {dots_area}");
    assert!(cliff_width <= 6.0, "{cliff_width}");
}

#[test]
fn a_basin_lies_flat_and_low_on_its_own_behaviors_land() {
    let expected = [
        (Key::R, "returned its structured answer", 6),
        (Key::R, "tried an edit that didn't apply", 3),
        (Key::S, "edited a source file", 6),
        (Key::S, "ran the test suite", 5),
        (Key::T, "read a doc", 4),
        (Key::T, "searched code by pattern", 4),
        (Key::U, "returned its structured answer", 7),
        (Key::U, "wrote a doc", 4),
    ];
    for (key, words, sources) in expected {
        let placed = placed(key);
        let ground = &placed.ground;
        let basin = of_kind(placed, |kind| matches!(kind, LandKind::Basin { .. }))
            .into_iter()
            .find(|mark| name(placed, mark.behavior) == words)
            .unwrap_or_else(|| panic!("{key:?} has a basin by {words}"));
        let LandKind::Basin { from, total } = &basin.kind else {
            unreachable!()
        };
        assert_eq!(from.len(), sources, "{key:?} {words}");
        assert_eq!(
            basin.line(ground),
            format!(
                "Basin · agents came to ‘{words}’ from {sources} other behaviors, {total} times"
            )
        );
        assert!(ground.land_at(basin.x, basin.y) >= 1.1);
        assert!(ground.height_at(basin.x, basin.y) < 4.0);
        let home = placed.placer.home(basin.behavior);
        assert!(home > 0 && placed.placer.landmass_at(basin.x, basin.y) == home);
        let stretch = stretch_at(ground, basin.x, basin.y, basin.radius);
        assert!((stretch - 1.0).abs() <= 0.25, "{key:?} {words}: {stretch}");
    }
}

#[test]
fn an_oasis_is_a_fresh_behavior_in_dry_land() {
    let u = placed(Key::U);
    let oases = of_kind(u, |kind| matches!(kind, LandKind::Oasis));
    assert_eq!(oases.len(), 1);
    let oasis = oases[0];
    assert_eq!(name(u, oasis.behavior), "fetched from the web");
    assert!(u.ground.land_at(oasis.x, oasis.y) >= 1.1);
    assert_eq!(
        oasis.line(&u.ground),
        "Oasis · ‘fetched from the web’ was used 4 days ago, in dry land: the behaviors round \
         it were last used 14 or more days ago"
    );
    for key in [Key::R, Key::S, Key::T] {
        assert!(of_kind(placed(key), |kind| matches!(kind, LandKind::Oasis)).is_empty());
    }
}

#[test]
fn what_lies_flat_keeps_to_the_land() {
    let mut checked = 0;
    for key in Key::ALL {
        let placed = placed(key);
        for mark in &placed.marks {
            if matches!(mark.kind, LandKind::Plateau { .. }) {
                continue;
            }
            checked += 1;
            let mut canvas = Canvas::new(WIDTH as u32, HEIGHT as u32);
            mark.draw(&placed.ground, &mut canvas);
            let mut spilled = 0;
            let rows =
                (mark.y - 34.0).max(0.0) as usize..(mark.y + 34.0).min(HEIGHT as f64) as usize;
            for y in rows {
                let columns =
                    (mark.x - 34.0).max(0.0) as usize..(mark.x + 34.0).min(WIDTH as f64) as usize;
                for x in columns {
                    let at_sea = coast_at(&placed.ground, x as f64 + 0.5, y as f64 + 0.5) < -2.0;
                    if at_sea && canvas.pixel(x as u32, y as u32)[3] > 24 {
                        spilled += 1;
                    }
                }
            }
            assert!(spilled < 6, "{key:?} {:?}: {spilled}", mark.kind);
        }
    }
    assert!(checked >= 6);
}

/// What nature sets down on crew U's map before the land's kinds, in the order it places them:
/// tsunamis and hurricanes, volcanoes, tornadoes, a quake, storms, faults and lakes, each as x,
/// y, radius and whether it lies flat. Fires and floods reach no radius, so no mark keeps clear
/// of them, and they are left out.
const NATURE_FIRST_ON_U: [(f64, f64, f64, bool); 21] = [
    (191.9, 97.9, 22.0, false),
    (581.7, 476.3, 39.0, false),
    (458.8, 386.3, 22.0, false),
    (831.7, 299.9, 57.0, false),
    (1009.0, 358.6, 22.0, false),
    (593.0, 183.7, 18.4, false),
    (632.6, 360.2, 20.0, false),
    (514.4, 409.3, 12.0, false),
    (583.9, 278.4, 18.9, true),
    (756.8, 358.6, 11.0, false),
    (734.3, 273.0, 14.2, false),
    (861.7, 401.2, 11.0, false),
    (870.5, 467.6, 17.9, false),
    (918.3, 332.7, 21.9, false),
    (958.0, 386.5, 17.1, false),
    (826.5, 438.3, 18.9, false),
    (250.2, 408.1, 18.9, false),
    (835.5, 136.8, 18.9, false),
    (864.1, 419.2, 13.6, true),
    (645.7, 376.3, 10.1, true),
    (694.2, 379.2, 9.2, true),
];

/// Crew U's land kinds, placed after what nature sets down first.
fn after_nature() -> (Placer, Vec<LandMark>) {
    let ground = &placed(Key::U).ground;
    let mut placer = Placer::new(ground);
    for (x, y, radius, flat) in NATURE_FIRST_ON_U {
        placer.add(Placed { x, y, radius, flat });
    }
    let marks = land_marks(ground, &mut placer);
    (placer, marks)
}

#[test]
fn the_kinds_keep_clear_of_what_nature_placed_first() {
    let u = placed(Key::U);
    let at = |marks: &[LandMark]| -> Vec<(String, usize, i32, i32)> {
        marks
            .iter()
            .map(|mark| {
                let kind = format!("{:?}", mark.kind);
                let kind = kind.split([' ', '{']).next().unwrap_or("").to_owned();
                (
                    kind,
                    mark.behavior,
                    mark.x.round() as i32,
                    mark.y.round() as i32,
                )
            })
            .collect()
    };
    let (placer, marks) = after_nature();
    assert_eq!(
        at(&marks),
        [
            ("Swamp".to_owned(), 6, 254, 185),
            ("Basin".to_owned(), 30, 940, 294),
            ("Basin".to_owned(), 24, 678, 311),
            ("Oasis".to_owned(), 28, 950, 263),
        ]
    );
    let flat_before = &placer.placed[..NATURE_FIRST_ON_U.len()];
    for mark in &marks {
        for other in flat_before.iter().filter(|placed| placed.flat) {
            let gap = (other.x - mark.x).hypot(other.y - mark.y) - other.radius - mark.radius;
            assert!(
                gap >= 4.0,
                "{:?} by {} lies on a lake",
                mark.kind,
                mark.behavior
            );
        }
    }
    // Alone, the basin by behavior 24 takes the ground the lake by it was set on.
    let alone = u
        .marks
        .iter()
        .find(|mark| mark.behavior == 24 && matches!(mark.kind, LandKind::Basin { .. }))
        .expect("a basin by behavior 24");
    let (lake_x, lake_y, lake_radius, _) = NATURE_FIRST_ON_U[20];
    assert!((alone.x - lake_x).hypot(alone.y - lake_y) < lake_radius + alone.radius);
}

#[test]
fn crew_us_land_kinds_lie_on_its_ground_as_drawn() {
    use crate::world::map::land::{Marks, Polar};
    use crate::world::map::relief::cells::{compare, composed};
    use crate::world::map::relief::{
        DotStyle, Dots, Light, LitGround, Projected, Relief, STANDING,
    };

    let ground = &placed(Key::U).ground;
    let (_, marks) = after_nature();
    let mut laid = Canvas::new(WIDTH as u32, HEIGHT as u32);
    for mark in &marks {
        mark.draw(ground, &mut laid);
    }
    let laid = laid.to_rgba();
    let relief = Relief::new(ground);
    let polar = Polar::of(ground);
    let light = Light::new(ground, &relief, STANDING);
    let on_the_ground = Marks {
        ground: Some(&laid),
        ..Marks::default()
    };
    let lit = LitGround::new(ground, &relief, &light, None, on_the_ground, Some(&polar));
    let land = Projected::new(&relief, &lit, STANDING, 2);
    let colours: Vec<_> = Crew::get(Key::U)
        .groups
        .iter()
        .map(|group| group.rgb.map(f64::from))
        .collect();
    let style = DotStyle {
        groups: &colours,
        compared: None,
    };
    let dots = Dots::new(ground, &relief, style, STANDING);
    compare(
        "ground-u-kinds",
        &composed(&land, &dots),
        include_str!("golden/ground-u-kinds.txt"),
    );
}
