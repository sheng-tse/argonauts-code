//! A maker's galaxy at its own level: each body in its own colour on the galaxy's light, the
//! rare kinds drawn by their rules, what moves, and what the pointer finds.

use super::crews::{self, Crew};
use super::pictures::{
    HEIGHT, brightest, brightness, cosmos, find, marked, pixel, still, view, with,
};
use crate::world::cosmos::glow::TWINKLES;
use crate::world::cosmos::{Cosmos, Level, StarKind};
use crate::world::{Depth, PartId, Pointed};

/// The bodies of a galaxy as the scene marks them: each star's index and where it is.
fn bodies(cosmos: &mut Cosmos, maker: usize) -> Vec<(usize, (f64, f64), f32)> {
    let scene = cosmos.scene(&still(Level::Galaxy(maker)), 0.0);
    scene
        .hits
        .marks()
        .iter()
        .filter_map(|m| match (m.pointed, &m.shape) {
            (Pointed::Star(star), crate::world::Shape::Disc { x, y, radius }) => {
                Some((star as usize, (f64::from(*x), f64::from(*y)), *radius))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_galaxy_is_one_picture_with_its_badge_and_a_body_for_each_model() {
    with('R', |_, cosmos| {
        for maker in 0..cosmos.universe().makers.len() {
            let scene = cosmos.scene(&still(Level::Galaxy(maker)), 0.0);
            assert_eq!(scene.layers().len(), 1);
            assert!(scene.sprites().is_empty());
            let found = bodies(cosmos, maker);
            assert_eq!(found.len(), cosmos.universe().makers[maker].stars.len());
            let image = scene.flatten();
            assert!(
                brightest(&image, (30.0, 30.0), 8.0) > 300,
                "galaxy {maker}'s badge"
            );
            for (star, at, _) in found {
                assert_eq!(find(&scene, at.0, at.1), Some(Pointed::Star(star as u32)));
            }
        }
    });
}

#[test]
fn every_star_and_red_giant_keeps_its_own_colour_on_the_white_core() {
    for letter in ['R', 'T', 'U'] {
        with(letter, |crew: &Crew, cosmos| {
            for maker in 0..cosmos.universe().makers.len() {
                let image = cosmos.scene(&still(Level::Galaxy(maker)), 0.0).flatten();
                for (star, (x, y), radius) in bodies(cosmos, maker) {
                    let s = &cosmos.universe().stars[star];
                    if !matches!(s.kind, StarKind::Star | StarKind::RedGiant) {
                        continue;
                    }
                    let r = f64::from(radius);
                    let sample = pixel(
                        &image,
                        (x + r * 0.45).round() as usize,
                        (y + r * 0.35).round() as usize,
                    );
                    let own = if s.kind == StarKind::RedGiant {
                        [246, 92, 44]
                    } else {
                        s.rgb
                    };
                    let distance = |a: [u8; 3], b: [u8; 3]| {
                        a.iter()
                            .zip(b)
                            .map(|(a, b)| (f64::from(*a) - f64::from(b)).powi(2))
                            .sum::<f64>()
                            .sqrt()
                    };
                    let name = &crew.facts.models[s.model].model;
                    assert!(
                        distance(sample, own) < distance(sample, [255, 255, 255]),
                        "{letter}: {name} washed out: {sample:?}"
                    );
                    assert!(brightness(sample) < 700, "{letter}: {name} washed white");
                    assert!(r >= 4.0);
                }
            }
        });
    }
}

#[test]
fn the_pointer_finds_a_galaxys_actions_and_its_disc_but_not_the_sky_beyond() {
    with('R', |_, cosmos| {
        let scene = cosmos.scene(&still(Level::Galaxy(0)), 0.0);
        let actions = marked(&scene, |p| matches!(p, Pointed::Action(_)));
        let alone = actions
            .iter()
            .filter(|(_, at)| matches!(find(&scene, at.0, at.1), Some(Pointed::Action(_))))
            .count();
        assert!(alone > actions.len() / 2, "{alone} of {}", actions.len());
        assert_eq!(find(&scene, 2.0, HEIGHT as f64 - 2.0), None);
    });
}

#[test]
fn a_galaxy_moves_where_its_bodies_breathe_or_go_round_and_holds_still_when_asked() {
    with('R', |_, cosmos| {
        // OpenAI's galaxy holds R's binary.
        let scene = cosmos.scene(&view(Level::Galaxy(1)), 0.0);
        assert!(scene.moves);
        let moving: Vec<_> = scene
            .sprites()
            .iter()
            .filter(|s| s.depth == Depth::Dots)
            .collect();
        assert!(!moving.is_empty());
        let first = scene.flatten();
        let later = cosmos.scene(&view(Level::Galaxy(1)), 2.0).flatten();
        assert_ne!(first.pixels(), later.pixels());
        let a = cosmos.scene(&still(Level::Galaxy(1)), 0.0).flatten();
        let b = cosmos.scene(&still(Level::Galaxy(1)), 2.0).flatten();
        assert_eq!(a.pixels(), b.pixels());
    });
    // T has been quiet for nine days: nothing of its galaxies breathes or goes round, yet
    // their stars twinkle, a glint laid over the still picture.
    with('T', |_, cosmos| {
        for maker in 0..cosmos.universe().makers.len() {
            let glints: Vec<PartId> = (0..TWINKLES)
                .map(|slot| PartId::new("cosmos-twinkle", maker as u32 * TWINKLES + slot))
                .collect();
            let lit = (0..12).any(|step| {
                let scene = cosmos.scene(&view(Level::Galaxy(maker)), f64::from(step) * 0.25);
                assert!(scene.moves);
                assert!(scene.sprites().iter().all(|s| glints.contains(&s.id)));
                !scene.sprites().is_empty()
            });
            assert!(lit, "a star of galaxy {maker} twinkles");
            let held = cosmos.scene(&still(Level::Galaxy(maker)), 1.0);
            assert!(!held.moves);
            assert!(held.sprites().is_empty());
        }
    });
}

#[test]
fn a_turning_galaxys_disc_stays_one_picture_so_the_screen_sends_it_once() {
    with('R', |_, cosmos| {
        let turn = cosmos.universe().makers[0].turn;
        assert!(turn > 0.0, "R's biggest maker worked this week");
        // A quarter of its turn later, the disc in the universe and inside the galaxy is the
        // same picture under the same key; the hover line tells the turn instead.
        let quarter = std::f64::consts::FRAC_PI_2 / turn;
        for level in [Level::Universe, Level::Galaxy(0)] {
            let now = cosmos.scene(&view(level), 0.0);
            let later = cosmos.scene(&view(level), quarter);
            assert_eq!(now.layers().len(), later.layers().len(), "{level:?}");
            for (a, b) in now.layers().iter().zip(later.layers()) {
                assert_eq!(a.picture.key, b.picture.key, "{level:?}");
                assert_eq!(
                    a.picture.image.pixels(),
                    b.picture.image.pixels(),
                    "{level:?}"
                );
            }
        }
    });
}

#[test]
fn a_binarys_smaller_star_rides_round_the_bigger_one() {
    with('R', |crew, cosmos| {
        let astra = crew
            .facts
            .models
            .iter()
            .position(|m| m.model == "gpt-6-astra")
            .unwrap();
        let star = cosmos.universe().star_of_model(astra).unwrap() as u32;
        let at = |scene: &crate::world::Scene| {
            marked(scene, |p| p == Pointed::Star(star))
                .first()
                .map(|(_, at)| *at)
                .unwrap()
        };
        let one = at(&cosmos.scene(&view(Level::Galaxy(1)), 0.0));
        let two = at(&cosmos.scene(&view(Level::Galaxy(1)), 4.0));
        assert!(
            (one.0 - two.0).hypot(one.1 - two.1) > 3.0,
            "{one:?} {two:?}"
        );
    });
}

/// The galaxy of the model `id` in a crew changed by `change`, drawn still, and that model's
/// star.
fn drawn(mut crew: Crew, id: &str, change: impl FnOnce(&mut Crew)) -> (Cosmos, usize, usize) {
    change(&mut crew);
    let model = crew
        .facts
        .models
        .iter()
        .position(|m| m.model == id)
        .unwrap();
    let cosmos = cosmos(&crew);
    let star = cosmos.universe().star_of_model(model).unwrap();
    let maker = cosmos.universe().stars[star].maker;
    (cosmos, star, maker)
}

#[test]
fn each_rare_body_is_drawn_by_its_rule() {
    // A protostar: a red knot in a dark cloud that turns.
    let (mut cosmos, star, maker) = drawn(crews::r(), "opencode/glm-5", |crew| {
        crew.model("opencode/glm-5").first_used = 13;
    });
    assert_eq!(cosmos.universe().stars[star].kind, StarKind::Protostar);
    let first = cosmos.scene(&view(Level::Galaxy(maker)), 0.0);
    assert!(first.moves);
    let later = cosmos.scene(&view(Level::Galaxy(maker)), 7.5);
    assert_ne!(
        first.flatten().pixels(),
        later.flatten().pixels(),
        "its cloud turns"
    );

    // A black hole: dark in its middle, ringed by its bright bent disc.
    let (mut cosmos, star, maker) = drawn(crews::t(), "opencode/glm-5", |crew| {
        crew.model("opencode/glm-5").merged = 0;
    });
    assert_eq!(cosmos.universe().stars[star].kind, StarKind::BlackHole);
    let scene = cosmos.scene(&still(Level::Galaxy(maker)), 0.0);
    let at = marked(&scene, |p| p == Pointed::Star(star as u32))[0].1;
    let image = scene.flatten();
    assert!(brightness(pixel(&image, at.0 as usize, at.1 as usize)) < 40);
    assert!(brightest(&image, at, 14.0) > 300);

    // A supernova remnant: torn filaments round a star whose CLI failed three times in a day.
    let plain = {
        let (mut cosmos, _, maker) = drawn(crews::t(), "gemini-3.8-flash", |_| {});
        cosmos.scene(&still(Level::Galaxy(maker)), 0.0).flatten()
    };
    let (mut cosmos, star, maker) = drawn(crews::t(), "gemini-3.8-flash", |crew| {
        let failure = crew.model("gemini-3.8-flash").cli_failures[0];
        crew.model("gemini-3.8-flash").cli_failures.push(failure);
    });
    assert!(cosmos.universe().stars[star].supernova.is_some());
    let torn = cosmos.scene(&still(Level::Galaxy(maker)), 0.0).flatten();
    assert_ne!(plain.pixels(), torn.pixels());

    // A white dwarf: a hard white point in the ring it shed.
    let crew = crews::r_with_older_haiku();
    let model = crew
        .facts
        .models
        .iter()
        .position(|m| m.model == "claude-haiku-4-5")
        .unwrap();
    let mut cosmos = super::pictures::cosmos(&crew);
    let star = cosmos.universe().star_of_model(model).unwrap();
    assert_eq!(cosmos.universe().stars[star].kind, StarKind::WhiteDwarf);
    let scene = cosmos.scene(&still(Level::Galaxy(0)), 0.0);
    let at = marked(&scene, |p| p == Pointed::Star(star as u32))[0].1;
    let image = scene.flatten();
    assert!(brightness(pixel(&image, at.0 as usize, at.1 as usize)) > 700);
}

#[test]
fn the_ring_marks_the_star_picked() {
    with('R', |_, cosmos| {
        let star = cosmos.universe().makers[0].stars[0];
        let ringed = crate::world::cosmos::View {
            ringed: Some(Pointed::Star(star as u32)),
            ..still(Level::Galaxy(0))
        };
        let scene = cosmos.scene(&ringed, 0.0);
        let ring = scene
            .sprites()
            .iter()
            .find(|s| s.depth == Depth::Pointer)
            .expect("a ring");
        let at = marked(&scene, |p| p == Pointed::Star(star as u32))[0].1;
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
