//! The zoom between levels: it starts as the outer level, ends as the inner one, grows what you
//! went into on the way, and holds back when motion is reduced.

use super::pictures::{HEIGHT, WIDTH, brightest, still, view, with};
use crate::world::cosmos::{HomeShot, Level, Times, home_radius};
use crate::world::raster::{Brush, Canvas};
use crate::world::{Depth, PartId, Scene};

fn picture(scene: &Scene) -> crate::world::Image {
    scene.flatten()
}

/// How alike two pictures are: the mean difference of their channels, 0 for the same.
fn difference(a: &crate::world::Image, b: &crate::world::Image) -> f64 {
    let total: u64 = a
        .pixels()
        .iter()
        .zip(b.pixels())
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    total as f64 / a.pixels().len() as f64
}

#[test]
fn a_zoom_begins_at_the_outer_level_and_ends_at_the_inner_one() {
    with('R', |_, cosmos| {
        let (outer, inner) = (view(Level::Universe), view(Level::Galaxy(0)));
        let start = picture(&cosmos.zoom(&outer, &inner, 0.0, Times::at(0.0)));
        let end = picture(&cosmos.zoom(&outer, &inner, 0.999, Times::at(0.0)));
        let universe = picture(&cosmos.scene(&still(Level::Universe), 0.0));
        let galaxy = picture(&cosmos.scene(&still(Level::Galaxy(0)), 0.0));
        assert!(difference(&start, &universe) < difference(&start, &galaxy));
        assert!(difference(&end, &galaxy) < difference(&end, &universe));
        // Halfway it is neither: the inner level's sky, with both levels stretched over it.
        let half = cosmos.zoom(&outer, &inner, 0.5, Times::at(0.0));
        assert!(half.moves);
        assert_eq!(half.layers().len(), 1);
        assert_eq!(half.layers()[0].depth, Depth::Sky);
        assert_eq!(half.scaled().len(), 2);
        let half = picture(&half);
        assert!(difference(&half, &universe) > 1.0 && difference(&half, &galaxy) > 1.0);
        // Its end is the level's own scene.
        let done = cosmos.zoom(&outer, &inner, 1.0, Times::at(0.0));
        assert_eq!(
            done.layers()[0].picture.key,
            cosmos.scene(&view(Level::Galaxy(0)), 0.0).layers()[0]
                .picture
                .key
        );
    });
}

#[test]
fn zooming_out_plays_the_same_frames_backward() {
    with('R', |_, cosmos| {
        let (galaxy, system) = (view(Level::Galaxy(0)), view(Level::System(0)));
        let inward = picture(&cosmos.zoom(&galaxy, &system, 0.3, Times::at(0.0)));
        let outward = picture(&cosmos.zoom(&system, &galaxy, 0.7, Times::at(0.0)));
        assert!(difference(&inward, &outward) < 0.01);
    });
}

/// Reduced motion still zooms, its levels held still: the pictures it stretches are the same
/// at any time.
#[test]
fn with_motion_held_still_the_zoom_stretches_levels_held_still() {
    with('R', |_, cosmos| {
        let (from, to) = (still(Level::Universe), still(Level::Galaxy(0)));
        let keys = |scene: &Scene| -> Vec<u64> {
            scene.scaled().iter().map(|part| part.picture.key).collect()
        };
        let early = cosmos.zoom(&from, &to, 0.4, Times::at(0.0));
        let later = cosmos.zoom(&from, &to, 0.4, Times::at(9.0));
        assert!(early.moves);
        assert!(!keys(&early).is_empty());
        assert_eq!(keys(&early), keys(&later));
    });
}

/// Your planet as one opaque picture `radius` px round, for the zoom to shrink.
fn planet_shot(radius: f64) -> HomeShot {
    let side = (radius * 2.0).round() as usize;
    let image = crate::world::Image::new(side, side, [200, 210, 220, 255].repeat(side * side));
    HomeShot {
        picture: crate::world::Picture::new(7, image.expect("whole pixels")),
        centre: (radius, radius),
        radius,
    }
}

#[test]
fn from_your_planet_the_universe_grows_in_round_it() {
    with('R', |_, cosmos| {
        let planet = 240.0 * WIDTH as f64 / 1152.0;
        let home = home_radius(WIDTH as f64, HEIGHT as f64);
        let shot = planet_shot(planet);
        // How big the planet shows, from the box it is stretched over.
        let radius = |scene: &Scene| {
            let part = scene.scaled().last().expect("the planet");
            f64::from(part.size.0) / 2.0
        };
        let at_planet = cosmos.zoom_from_planet(WIDTH, HEIGHT, &shot, 1.0, 0.0);
        assert!((radius(&at_planet) - planet).abs() <= 1.0);
        let halfway = cosmos.zoom_from_planet(WIDTH, HEIGHT, &shot, 0.5, 0.0);
        assert!(radius(&halfway) > home + 1.0 && radius(&halfway) < planet - 1.0);
        let at_universe = cosmos.zoom_from_planet(WIDTH, HEIGHT, &shot, 0.0, 0.0);
        assert!((radius(&at_universe) - home).abs() <= 1.0);
        assert!(!at_universe.moves);
        // Round the planet, at the planet's end only the sky shows; at the universe's, its
        // galaxies.
        let unseen = HomeShot {
            picture: crate::world::Picture::new(8, crate::world::Image::transparent(2, 2)),
            ..shot
        };
        let sky = cosmos.sky(WIDTH, HEIGHT);
        let start = picture(&cosmos.zoom_from_planet(WIDTH, HEIGHT, &unseen, 1.0, 0.0));
        let end = picture(&cosmos.zoom_from_planet(WIDTH, HEIGHT, &unseen, 0.0, 0.0));
        assert!(difference(&start, &sky) < 0.01);
        assert!(difference(&end, &sky) > 1.0);
        assert!(brightest(&end, (WIDTH as f64 / 2.0, HEIGHT as f64 / 2.0), 2.0) < 400);
    });
}

/// The level a zoom arrives at is drawn as it will be when the zoom ends, so the zoom ends as
/// that level then shows it.
#[test]
fn the_zoom_ends_as_the_level_it_arrives_at_will_show() {
    with('R', |_, cosmos| {
        let (galaxy, system) = (view(Level::Galaxy(0)), view(Level::System(0)));
        // Its planets have gone round since the world opened.
        let t = 37.0;
        let times = Times { from: 0.0, to: t };
        let near_end = picture(&cosmos.zoom(&galaxy, &system, 0.999, times));
        let now = picture(&cosmos.scene(&system, t));
        let at_open = picture(&cosmos.scene(&system, 0.0));
        assert!(difference(&near_end, &now) < 0.5);
        assert!(difference(&near_end, &now) < difference(&near_end, &at_open));
    });
}

/// Every step of a zoom stretches the pictures its first steps drew, so a terminal keeps them
/// and each later step is placements: another galaxy or another moment draws others.
#[test]
fn a_zoom_stretches_the_same_pictures_at_every_step() {
    with('R', |_, cosmos| {
        let universe = view(Level::Universe);
        let parts = |scene: Scene| -> Vec<(u64, (u32, u32))> {
            scene
                .scaled()
                .iter()
                .map(|part| (part.picture.key, part.size))
                .collect()
        };
        let galaxy = view(Level::Galaxy(0));
        let early = parts(cosmos.zoom(&universe, &galaxy, 0.4, Times::at(0.0)));
        let later = parts(cosmos.zoom(&universe, &galaxy, 0.45, Times::at(0.0)));
        assert_eq!(early.len(), 2);
        assert_eq!(
            early.iter().map(|part| part.0).collect::<Vec<_>>(),
            later.iter().map(|part| part.0).collect::<Vec<_>>()
        );
        assert!(early.iter().zip(&later).all(|(a, b)| a.1 != b.1));
        let other = parts(cosmos.zoom(&universe, &view(Level::Galaxy(1)), 0.4, Times::at(0.0)));
        assert_ne!(early[1].0, other[1].0);
        let moved_on = parts(cosmos.zoom(&universe, &galaxy, 0.4, Times::at(1.0)));
        assert_ne!(early, moved_on);
    });
}

/// A whole zoom into a galaxy, step by step through a terminal's stage: the levels and their
/// skies go out once each, four pictures in all, and every other step only places them.
#[test]
fn a_zoom_sends_each_level_and_sky_once_and_then_only_places_them() {
    use crate::world::deliver::{Spot, Stage};
    let spot = Spot {
        cells: ratatui::layout::Rect::new(0, 0, 32, 6),
        cell: (18, 42),
    };
    with('R', |_, cosmos| {
        let (universe, galaxy) = (view(Level::Universe), view(Level::Galaxy(0)));
        let mut stage = Stage::new(900).packing_in_line();
        let sent: Vec<usize> = (0..6)
            .map(|frame| {
                let k = f64::from(frame) / 6.0;
                let scene = cosmos.zoom(&universe, &galaxy, k, Times::at(0.0));
                let escapes = stage.show(&scene, spot);
                crate::base::mascot::graphics::pictures_in(&escapes).len()
            })
            .collect();
        assert_eq!(sent.iter().sum::<usize>(), 4, "{sent:?}");
        assert!(sent.iter().filter(|&&n| n == 0).count() >= 2, "{sent:?}");
    });
}

/// The mean difference of the channels of two pictures along the edge of a box `inset` pixels
/// in from it, or out from it where `inset` is negative.
fn difference_round(
    a: &crate::world::Image,
    b: &crate::world::Image,
    (left, top, w, h): (i64, i64, i64, i64),
    inset: i64,
) -> f64 {
    let (x0, y0, x1, y1) = (
        left + inset,
        top + inset,
        left + w - 1 - inset,
        top + h - 1 - inset,
    );
    let (pa, pb) = (a.pixels(), b.pixels());
    let (mut total, mut count) = (0u64, 0u64);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let edge = x == x0 || x == x1 || y == y0 || y == y1;
            if !edge || x < 0 || y < 0 || x >= a.width() as i64 || y >= a.height() as i64 {
                continue;
            }
            let at = (y as usize * a.width() + x as usize) * 4;
            for c in 0..3 {
                total += u64::from(pa[at + c].abs_diff(pb[at + c]));
            }
            count += 3;
        }
    }
    total as f64 / count.max(1) as f64
}

#[test]
fn a_level_without_its_sky_lays_its_moving_bodies_over_what_holds_still_once() {
    for level in [Level::Universe, Level::Galaxy(0)] {
        with('R', |_, cosmos| {
            let t = 1.0;
            let own = cosmos.scene(&view(level), t);
            let bare = cosmos.level_scene(&view(level), t, false);
            assert_eq!(bare.layers().len(), 1);
            let sky = match level {
                Level::Universe => cosmos.sky_canvas(WIDTH, HEIGHT, None, false),
                _ => cosmos.sky_canvas(WIDTH, HEIGHT, None, true),
            };
            let mut over = sky.clone();
            let laid = Canvas::from_image(&picture(&bare)).unwrap();
            over.draw(&laid, 0.0, 0.0, &Brush::default());
            let (over, own_picture) = (over.to_image(), picture(&own));
            let moving: Vec<_> = own
                .sprites()
                .iter()
                .filter(|s| (0..64).any(|i| s.id == PartId::new("cosmos-moving", i)))
                .collect();
            assert!(!moving.is_empty());
            for sprite in moving {
                let image = &sprite.picture.image;
                let area = (
                    i64::from(sprite.at.0),
                    i64::from(sprite.at.1),
                    image.width() as i64,
                    image.height() as i64,
                );
                // No box shows round a moving body: just inside its picture's edge the level
                // looks as it does just outside.
                let inside = difference_round(&over, &own_picture, area, 1);
                let outside = difference_round(&over, &own_picture, area, -2);
                assert!(
                    inside < outside * 1.5 + 0.3,
                    "{level:?}: {inside:.2} inside, {outside:.2} outside"
                );
            }
        });
    }
}
