//! A model's own system: its star, its planets going round in depth, its belt, flares and
//! partner, and what the pointer finds.

use super::pictures::{brightest, brightness, find, marked, pixel, still, view, with};
use crate::world::cosmos::{Body, Level, View};
use crate::world::{Depth, PartId, Pointed, Scene};

/// R's Haiku: its most used model, with a lava world, flares and a belt.
const HAIKU: usize = 0;

fn star_of(crew: &super::crews::Crew, cosmos: &crate::world::cosmos::Cosmos, id: &str) -> usize {
    let model = crew
        .facts
        .models
        .iter()
        .position(|m| m.model == id)
        .unwrap();
    cosmos.universe().star_of_model(model).unwrap()
}

fn planet_sprites(scene: &Scene) -> Vec<(u32, Depth, (i32, i32))> {
    (0..6)
        .filter_map(|at| {
            scene
                .sprites()
                .iter()
                .find(|s| s.id == PartId::new("cosmos-planet", at))
                .map(|s| (at, s.depth, s.at))
        })
        .collect()
}

#[test]
fn a_still_system_is_one_picture_and_a_moving_one_is_drawn_in_depth() {
    with('R', |crew, cosmos| {
        assert_eq!(star_of(crew, cosmos, "claude-haiku-5-5"), HAIKU);
        let scene = cosmos.scene(&still(Level::System(HAIKU)), 0.0);
        assert_eq!(scene.layers().len(), 1);
        assert!(scene.sprites().is_empty());
        assert!(!scene.moves);
        let scene = cosmos.scene(&view(Level::System(HAIKU)), 0.0);
        let depths: Vec<Depth> = scene.layers().iter().map(|l| l.depth).collect();
        assert_eq!(depths, [Depth::Sky, Depth::Weather]);
        assert!(
            scene.layers()[0]
                .picture
                .image
                .pixels()
                .chunks_exact(4)
                .all(|p| p[3] == 255)
        );
        let sun = scene
            .sprites()
            .iter()
            .find(|s| s.id == PartId::new("cosmos-sun", 0));
        assert_eq!(sun.map(|s| s.depth), Some(Depth::Nature));
        let planets = cosmos.universe().stars[HAIKU].system.planets.len();
        assert_eq!(planet_sprites(&scene).len(), planets);
        assert!(scene.moves);
    });
}

#[test]
fn a_planet_goes_round_passing_behind_the_star_and_in_front_again() {
    with('R', |_, cosmos| {
        let period = cosmos.universe().stars[HAIKU].system.planets[0].period;
        assert_eq!(period, 20.0);
        let mut depths = Vec::new();
        let mut places = Vec::new();
        for k in 0..8 {
            let scene = cosmos.scene(&view(Level::System(HAIKU)), period * f64::from(k) / 8.0);
            let (_, depth, at) = planet_sprites(&scene)[0];
            depths.push(depth);
            places.push(at);
        }
        assert!(
            depths.contains(&Depth::Ground) && depths.contains(&Depth::Dots),
            "{depths:?}"
        );
        places.dedup();
        assert_eq!(places.len(), 8);
        // A whole turn on, it is back where it started.
        let again = cosmos.scene(&view(Level::System(HAIKU)), period);
        assert_eq!(
            planet_sprites(&again)[0].2,
            planet_sprites(&cosmos.scene(&view(Level::System(HAIKU)), 0.0))[0].2
        );
    });
}

#[test]
fn a_still_system_holds_still() {
    with('R', |_, cosmos| {
        let a = cosmos.scene(&still(Level::System(HAIKU)), 0.0).flatten();
        let b = cosmos.scene(&still(Level::System(HAIKU)), 9.0).flatten();
        assert_eq!(a.pixels(), b.pixels());
    });
}

#[test]
fn the_pointer_finds_the_star_each_planet_moon_and_rock_and_the_belt() {
    with('S', |crew, cosmos| {
        let opus = star_of(crew, cosmos, "claude-opus-5-5");
        let scene = cosmos.scene(&still(Level::System(opus)), 0.0);
        let (w, h) = (scene.width() as f64, scene.height() as f64);
        let centre = (w / 2.0, h / 2.0 + 6.0);
        assert_eq!(
            find(&scene, centre.0, centre.1),
            Some(Pointed::Star(opus as u32))
        );
        let universe = cosmos.universe();
        let bodies = marked(&scene, |p| matches!(p, Pointed::Body(_)));
        let mut planets = 0;
        let mut moons = 0;
        for (pointed, at) in &bodies {
            let Pointed::Body(index) = pointed else {
                continue;
            };
            match universe.body(*index) {
                Some(Body::Planet { star, .. }) => {
                    assert_eq!(star, opus);
                    planets += 1;
                    assert_eq!(find(&scene, at.0, at.1), Some(*pointed));
                }
                Some(Body::Moon { star, .. }) => {
                    assert_eq!(star, opus);
                    moons += 1;
                    assert_eq!(find(&scene, at.0, at.1), Some(*pointed));
                }
                Some(Body::Belt { star }) => assert_eq!(star, opus),
                Some(Body::Flare { star, .. }) => assert_eq!(star, opus),
                other => panic!("{other:?} in a system"),
            }
        }
        assert_eq!(planets, universe.stars[opus].system.planets.len());
        assert_eq!(moons, 1);
        let rocks = marked(&scene, |p| matches!(p, Pointed::Action(_)));
        assert_eq!(rocks.len(), universe.stars[opus].system.rocks.len());
        let alone = rocks
            .iter()
            .filter(|(_, at)| matches!(find(&scene, at.0, at.1), Some(Pointed::Action(_))))
            .count();
        assert!(alone > 0);
        // Out at the belt's side, where no rock lies, the belt answers.
        let belt = universe.body_index(Body::Belt { star: opus }).unwrap();
        let found = (0..40).find_map(|k| {
            let x = w - 40.0 - f64::from(k);
            (find(&scene, x, centre.1) == Some(Pointed::Body(belt))).then_some(x)
        });
        assert!(found.is_some());
    });
}

#[test]
fn flares_rise_on_the_star_for_the_sessions_its_cli_failed_in() {
    with('R', |_, cosmos| {
        let flares = cosmos.universe().stars[HAIKU].system.flares.len();
        assert_eq!(flares, 3);
        let scene = cosmos.scene(&still(Level::System(HAIKU)), 0.0);
        let marks = marked(&scene, |p| match p {
            Pointed::Body(at) => matches!(cosmos.universe().body(at), Some(Body::Flare { .. })),
            _ => false,
        });
        assert_eq!(marks.len(), 3);
        let image = scene.flatten();
        for (_, at) in marks {
            assert!(brightest(&image, at, 4.0) > 450);
        }
    });
}

#[test]
fn a_lava_world_glows_with_cracks_and_the_others_show_their_land() {
    with('R', |_, cosmos| {
        let system = cosmos.universe().stars[HAIKU].system.clone();
        let lava = system.planets.iter().position(|p| p.lava).unwrap();
        let scene = cosmos.scene(&view(Level::System(HAIKU)), 0.0);
        let sprite = scene
            .sprites()
            .iter()
            .find(|s| s.id == PartId::new("cosmos-planet", lava as u32))
            .unwrap();
        let image = &sprite.picture.image;
        let glowing = image
            .pixels()
            .chunks_exact(4)
            .filter(|p| p[3] > 200 && p[0] > 200 && p[1] < 170 && p[2] < 110)
            .count();
        assert!(glowing > 0, "no glowing crack");
        let land = system.planets.iter().position(|p| !p.lava).unwrap();
        let sprite = scene
            .sprites()
            .iter()
            .find(|s| s.id == PartId::new("cosmos-planet", land as u32))
            .unwrap();
        let (w, h) = (sprite.picture.image.width(), sprite.picture.image.height());
        assert!(brightness(pixel(&sprite.picture.image, w / 2, h / 2)) > 60);
    });
}

#[test]
fn a_binarys_partner_goes_round_outside_the_planets() {
    with('R', |crew, cosmos| {
        let luna = star_of(crew, cosmos, "gpt-6-luna");
        let astra = star_of(crew, cosmos, "gpt-6-astra");
        let first = cosmos.scene(&view(Level::System(luna)), 0.0);
        let partner = |scene: &Scene| {
            scene
                .sprites()
                .iter()
                .find(|s| s.id == PartId::new("cosmos-partner", 0))
                .map(|s| s.at)
        };
        let later = cosmos.scene(&view(Level::System(luna)), 3.0);
        assert_ne!(partner(&first), partner(&later));
        let still = cosmos.scene(&still(Level::System(luna)), 0.0);
        let at = marked(&still, |p| p == Pointed::Star(astra as u32));
        assert_eq!(at.len(), 1);
        assert_eq!(
            find(&still, at[0].1.0, at[0].1.1),
            Some(Pointed::Star(astra as u32))
        );
    });
}

#[test]
fn the_ring_marks_the_planet_picked() {
    with('R', |_, cosmos| {
        let planet = cosmos.universe().stars[HAIKU].bodies[1];
        let ringed = View {
            ringed: Some(Pointed::Body(planet as u32)),
            ..still(Level::System(HAIKU))
        };
        let scene = cosmos.scene(&ringed, 0.0);
        let ring = scene
            .sprites()
            .iter()
            .find(|s| s.depth == Depth::Pointer)
            .expect("a ring");
        let at = marked(&scene, |p| p == Pointed::Body(planet as u32))[0].1;
        let (w, h) = (
            ring.picture.image.width() as f64,
            ring.picture.image.height() as f64,
        );
        let centre = (
            f64::from(ring.at.0) + w / 2.0,
            f64::from(ring.at.1) + h / 2.0,
        );
        assert!((centre.0 - at.0).abs() <= 1.5 && (centre.1 - at.1).abs() <= 1.5);
    });
}

#[test]
fn a_suns_picture_changes_every_second_step_at_either_rate() {
    use crate::world::deliver::clock::Rate;
    with('R', |_, cosmos| {
        for rate in [Rate::QUICK, Rate::STEADY] {
            let view = View {
                rate,
                ..view(Level::System(HAIKU))
            };
            let suns: Vec<u64> = (0..16)
                .map(|step| {
                    let scene = cosmos.scene(&view, rate.seconds(step));
                    let sun = scene
                        .sprites()
                        .iter()
                        .find(|s| s.id == PartId::new("cosmos-sun", 0))
                        .map(|s| s.picture.key);
                    sun.unwrap()
                })
                .collect();
            for pair in suns.chunks(2) {
                assert_eq!(pair[0], pair[1], "{rate:?}");
            }
            let mut changes = suns.clone();
            changes.dedup();
            assert_eq!(changes.len(), 8, "{rate:?}");
        }
    });
}
