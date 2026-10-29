//! The universe as drawn: what lies where on its picture, what moves and what holds still, and
//! what the pointer finds. Set `ARGONAUTS_COSMOS_SHOTS` to a folder and run the ignored `shots`
//! test to write each crew's levels there as PNGs to look at.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::crews::{self, Crew};
use crate::world::cosmos::{Cosmos, Level, StarKind, Times, View, home_radius};
use crate::world::{Depth, Image, PartId, Picture, Pointed, Scene, Shape};

/// The picture's size for most tests: 64 by 13 cells of 9 by 21, half the terminal the world is
/// drawn for, which keeps the tests quick where the drawing isn't optimized.
pub(super) const WIDTH: usize = 576;
pub(super) const HEIGHT: usize = 273;
/// The picture's size on the terminal the world is drawn for: 64 by 13 cells of 18 by 42.
const FULL: (usize, usize) = (1152, 546);

pub(super) fn cosmos(crew: &Crew) -> Cosmos {
    Cosmos::new(&crew.facts, &crew.colours, crew.actions.clone())
}

type Shared = Arc<Mutex<(Crew, Cosmos)>>;

/// The crew by its letter and its cosmos, made once for all the tests, so the pictures one
/// test draws are there for the next.
pub(super) fn with<T>(letter: char, test: impl FnOnce(&Crew, &mut Cosmos) -> T) -> T {
    static ALL: OnceLock<Mutex<HashMap<char, Shared>>> = OnceLock::new();
    let shared = {
        let mut all = ALL
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        all.entry(letter)
            .or_insert_with(|| {
                let crew = match letter {
                    'R' => crews::r(),
                    'S' => crews::s(),
                    'T' => crews::t(),
                    _ => crews::u(),
                };
                let cosmos = cosmos(&crew);
                Arc::new(Mutex::new((crew, cosmos)))
            })
            .clone()
    };
    let mut held = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let (crew, cosmos) = &mut *held;
    test(crew, cosmos)
}

pub(super) fn view(level: Level) -> View {
    View::new(level, WIDTH, HEIGHT)
}

pub(super) fn still(level: Level) -> View {
    View {
        still: true,
        ..view(level)
    }
}

/// Red, green and blue at a pixel.
pub(super) fn pixel(image: &Image, x: usize, y: usize) -> [u8; 3] {
    let at = (y * image.width() + x) * 4;
    let p = image.pixels();
    [p[at], p[at + 1], p[at + 2]]
}

pub(super) fn brightness(rgb: [u8; 3]) -> u32 {
    rgb.iter().map(|c| u32::from(*c)).sum()
}

/// The brightest pixel within `radius` of a point.
pub(super) fn brightest(image: &Image, (x, y): (f64, f64), radius: f64) -> u32 {
    let mut best = 0;
    let r = radius.ceil() as i64;
    for dy in -r..=r {
        for dx in -r..=r {
            let (px, py) = (x as i64 + dx, y as i64 + dy);
            if px < 0 || py < 0 || px >= image.width() as i64 || py >= image.height() as i64 {
                continue;
            }
            best = best.max(brightness(pixel(image, px as usize, py as usize)));
        }
    }
    best
}

pub(super) fn find(scene: &Scene, x: f64, y: f64) -> Option<Pointed> {
    scene.hits.find(x as f32, y as f32)
}

/// Where the marks naming something sit, as their outlines' middles.
pub(super) fn marked(scene: &Scene, pick: impl Fn(Pointed) -> bool) -> Vec<(Pointed, (f64, f64))> {
    scene
        .hits
        .marks()
        .iter()
        .filter(|m| pick(m.pointed))
        .filter_map(|m| {
            let at = match &m.shape {
                Shape::Point { x, y } | Shape::Disc { x, y, .. } | Shape::Ellipse { x, y, .. } => {
                    (f64::from(*x), f64::from(*y))
                }
                _ => return None,
            };
            Some((m.pointed, at))
        })
        .collect()
}

/// A plain disc standing in for the planet level's picture of your planet.
fn home_picture(width: usize, height: usize) -> Picture {
    let radius = home_radius(width as f64, height as f64);
    let side = (radius * 2.0).ceil() as usize + 2;
    let mut pixels = vec![0u8; side * side * 4];
    for y in 0..side {
        for x in 0..side {
            let half = side as f64 / 2.0;
            let (dx, dy) = (x as f64 + 0.5 - half, y as f64 + 0.5 - half);
            if dx.hypot(dy) <= radius {
                let at = (y * side + x) * 4;
                pixels[at..at + 4].copy_from_slice(&[40, 90, 160, 255]);
            }
        }
    }
    Picture::new(7, Image::new(side, side, pixels).unwrap())
}

#[test]
fn the_universe_is_one_still_picture_over_the_sky_with_its_moving_stars_apart() {
    with('R', |_, cosmos| {
        let scene = cosmos.scene(&view(Level::Universe), 0.0);
        assert_eq!((scene.width(), scene.height()), (WIDTH, HEIGHT));
        assert_eq!(scene.layers().len(), 1);
        let layer = &scene.layers()[0].picture.image;
        assert!(
            layer.pixels().chunks_exact(4).all(|p| p[3] == 255),
            "the universe is opaque"
        );
        assert!(!scene.sprites().is_empty());
        assert!(scene.moves);
        let still = cosmos.scene(&still(Level::Universe), 0.0);
        assert_eq!(still.layers().len(), 1);
        assert!(still.sprites().is_empty(), "nothing moves apart when still");
        assert!(!still.moves);
    });
}

#[test]
fn every_galaxy_carries_its_makers_badge_and_the_pointer_finds_it() {
    with('R', |_, cosmos| {
        let scene = cosmos.scene(&still(Level::Universe), 0.0);
        let badges: Vec<(Pointed, (f64, f64))> = scene
            .hits
            .marks()
            .iter()
            .filter(|m| m.tier == 2)
            .filter_map(|m| match &m.shape {
                Shape::Disc { x, y, .. } => Some((m.pointed, (f64::from(*x), f64::from(*y)))),
                _ => None,
            })
            .collect();
        assert_eq!(badges.len(), cosmos.universe().makers.len());
        let image = scene.flatten();
        for (at, (pointed, (x, y))) in badges.into_iter().enumerate() {
            assert_eq!(pointed, Pointed::Galaxy(at as u32));
            assert_eq!(find(&scene, x, y), Some(pointed));
            // The tile is dark, and its logo or dot lights its middle.
            assert!(brightest(&image, (x, y), 8.0) > 300, "badge {at}");
        }
    });
}

#[test]
fn your_planet_sits_in_the_middle_and_the_pointer_finds_it() {
    with('R', |_, cosmos| {
        let view = View {
            home: Some(home_picture(WIDTH, HEIGHT)),
            ..still(Level::Universe)
        };
        let scene = cosmos.scene(&view, 0.0);
        let home = scene
            .sprites()
            .iter()
            .find(|s| s.picture.key == 7)
            .expect("your planet");
        let half = home.picture.image.width() as f64 / 2.0;
        let centred = |across: usize| (across as f64 / 2.0 - half).round() as i32;
        assert_eq!(home.at, (centred(WIDTH), centred(HEIGHT)));
        assert_eq!(
            find(&scene, WIDTH as f64 / 2.0, HEIGHT as f64 / 2.0),
            Some(Pointed::Home)
        );
        let flat = scene.flatten();
        assert_eq!(pixel(&flat, WIDTH / 2, HEIGHT / 2), [40, 90, 160]);
    });
}

#[test]
fn galaxies_keep_clear_of_your_planet_and_inside_the_picture() {
    for letter in ['R', 'S', 'T', 'U'] {
        with(letter, |_, cosmos| {
            let scene = cosmos.scene(&still(Level::Universe), 0.0);
            let home = home_radius(WIDTH as f64, HEIGHT as f64);
            for (_, (x, y)) in marked(&scene, |p| matches!(p, Pointed::Galaxy(_))) {
                let (dx, dy) = (x - WIDTH as f64 / 2.0, y - HEIGHT as f64 / 2.0);
                assert!(
                    dx.hypot(dy) > home + 20.0,
                    "{letter}: a galaxy sits on your planet"
                );
                assert!((0.0..WIDTH as f64).contains(&x) && (0.0..HEIGHT as f64).contains(&y));
            }
        });
    }
}

#[test]
fn each_star_shines_where_the_pointer_finds_it() {
    with('R', |_, cosmos| {
        let scene = cosmos.scene(&still(Level::Universe), 0.0);
        let image = scene.flatten();
        let sky = cosmos.sky(WIDTH, HEIGHT);
        let stars = marked(&scene, |p| matches!(p, Pointed::Star(_)));
        assert_eq!(stars.len(), cosmos.universe().stars.len());
        for (pointed, at) in stars {
            assert_eq!(find(&scene, at.0, at.1), Some(pointed));
            let (lit, dark) = (brightest(&image, at, 2.0), brightest(&sky, at, 2.0));
            assert!(lit > dark + 120, "{pointed:?} is lit: {lit} over {dark}");
        }
    });
}

#[test]
fn the_universe_moves_only_where_its_facts_say_and_holds_still_when_asked() {
    // R's stars breathe and a binary goes round: two steps draw two other pictures.
    with('R', |_, cosmos| {
        let first = cosmos.scene(&view(Level::Universe), 0.0).flatten();
        let later = cosmos.scene(&view(Level::Universe), 1.25).flatten();
        assert_ne!(first.pixels(), later.pixels());
        let still_first = cosmos.scene(&still(Level::Universe), 0.0).flatten();
        let still_later = cosmos.scene(&still(Level::Universe), 1.25).flatten();
        assert_eq!(still_first.pixels(), still_later.pixels());
    });
    // T has been quiet for nine days: nothing in its universe moves.
    with('T', |_, cosmos| {
        let scene = cosmos.scene(&view(Level::Universe), 3.0);
        assert!(!scene.moves);
        assert!(scene.sprites().is_empty());
    });
}

#[test]
fn a_loop_replays_the_pictures_the_screen_already_holds() {
    with('R', |_, cosmos| {
        let keys = |scene: &Scene| -> Vec<u64> {
            let mut keys: Vec<u64> = scene
                .sprites()
                .iter()
                .filter(|s| s.depth == Depth::Dots && s.id == PartId::new("cosmos-moving", 0))
                .map(|s| s.picture.key)
                .collect();
            keys.sort_unstable();
            keys
        };
        let at_start = keys(&cosmos.scene(&view(Level::Universe), 0.0));
        let a_cycle_on = keys(&cosmos.scene(&view(Level::Universe), 30.0));
        assert!(!at_start.is_empty());
        assert_eq!(at_start, a_cycle_on);
        let a_step_on = keys(&cosmos.scene(&view(Level::Universe), 0.125));
        assert_ne!(at_start, a_step_on);
    });
}

#[test]
fn comets_fly_part_way_when_still_and_meteors_fall_only_while_they_fly() {
    with('R', |_, cosmos| {
        let scene = cosmos.scene(&still(Level::Universe), 0.0);
        let comets = scene
            .hits
            .marks()
            .iter()
            .filter(|m| matches!(m.pointed, Pointed::Body(_)) && m.tier == 0)
            .count();
        assert_eq!(comets, cosmos.universe().comets.len());
        let meteor = |at: u32| PartId::new("cosmos-meteor", at);
        let count = cosmos.universe().meteors.len() as u32;
        let falling = |scene: &Scene| {
            scene
                .sprites()
                .iter()
                .filter(|s| (0..count).any(|at| s.id == meteor(at)))
                .count()
        };
        // Each falls for 1.1 s of the 10 s loop, so over a loop each shows in some steps.
        let mut seen = vec![false; count as usize];
        for step in 0..80 {
            let scene = cosmos.scene(&view(Level::Universe), f64::from(step) / 8.0);
            assert!(falling(&scene) <= count as usize);
            for (at, seen) in seen.iter_mut().enumerate() {
                *seen |= scene.sprites().iter().any(|s| s.id == meteor(at as u32));
            }
        }
        assert!(seen.iter().all(|s| *s));
    });
}

#[test]
fn the_pointer_finds_a_galaxys_actions_its_disc_and_the_sky_beyond() {
    with('R', |_, cosmos| {
        let scene = cosmos.scene(&still(Level::Universe), 0.0);
        let actions = marked(&scene, |p| matches!(p, Pointed::Action(_)));
        assert_eq!(actions.len(), cosmos.universe().actions.len());
        let outlying = actions
            .iter()
            .find(|(_, at)| matches!(find(&scene, at.0, at.1), Some(Pointed::Action(_))));
        assert!(outlying.is_some(), "some action is found by itself");
        let found = find(&scene, 3.0, 3.0);
        assert!(matches!(found, Some(Pointed::Sky(_))), "{found:?}");
    });
}

#[test]
fn the_sky_names_each_earlier_months_cluster() {
    with('T', |_, cosmos| {
        let scene = cosmos.scene(&still(Level::Universe), 0.0);
        let clusters = marked(&scene, |p| matches!(p, Pointed::Sky(Some(_))));
        assert_eq!(clusters.len(), 12);
        let sky = cosmos.sky(WIDTH, HEIGHT);
        for (pointed, (x, y)) in clusters {
            assert_eq!(cosmos.sky_at(WIDTH, HEIGHT, x as f32, y as f32), pointed);
            assert!(brightest(&sky, (x, y), 18.0) > 300, "{pointed:?} shows");
        }
        assert_eq!(cosmos.sky_at(WIDTH, HEIGHT, 1.0, 1.0), Pointed::Sky(None));
    });
}

#[test]
fn the_far_field_is_fainter_than_any_earlier_action() {
    // With no earlier months the sky is the far field alone. Each far star lands at 0.2 of its
    // colour at most, fainter than any earlier action's at 0.22; only where two far stars fall
    // on one pixel can it be brighter.
    let mut crew = crews::r();
    crew.facts.crew.earlier_months.clear();
    let sky = cosmos(&crew).sky(WIDTH, HEIGHT);
    let single = 23 + 24 + 37 + (0.2 * 255.0 * 3.0) as u32;
    let brighter = sky
        .pixels()
        .chunks_exact(4)
        .filter(|p| brightness([p[0], p[1], p[2]]) > single)
        .count();
    assert!(brighter <= 30, "{brighter} pixels");
}

#[test]
fn inside_a_galaxy_or_a_system_the_sky_keeps_the_far_field_alone() {
    // Your earlier months' clusters would sit over a galaxy's arms and a system's orbits and read
    // as that maker's own stars, so only the universe draws them.
    let mut without = crews::r();
    without.facts.crew.earlier_months.clear();
    let mut without = cosmos(&without);
    with('R', |_, cosmos| {
        for level in [Level::Universe, Level::Galaxy(0), Level::System(0)] {
            let a = cosmos.scene(&still(level), 0.0).flatten();
            let b = without.scene(&still(level), 0.0).flatten();
            let same = a.pixels() == b.pixels();
            assert_eq!(same, level != Level::Universe, "{level:?}");
        }
    });
}

#[test]
fn a_red_giant_is_drawn_and_found_like_any_star() {
    with('T', |_, cosmos| {
        let giant = cosmos
            .universe()
            .stars
            .iter()
            .position(|s| s.kind == StarKind::RedGiant)
            .unwrap();
        let scene = cosmos.scene(&still(Level::Universe), 0.0);
        let at = marked(&scene, |p| p == Pointed::Star(giant as u32));
        assert_eq!(at.len(), 1);
        assert_eq!(
            find(&scene, at[0].1.0, at[0].1.1),
            Some(Pointed::Star(giant as u32))
        );
    });
}

#[test]
fn the_pointers_ring_sits_on_what_it_found() {
    with('R', |_, cosmos| {
        let ringed = View {
            ringed: Some(Pointed::Home),
            ..still(Level::Universe)
        };
        let scene = cosmos.scene(&ringed, 0.0);
        let ring = scene
            .sprites()
            .iter()
            .find(|s| s.depth == Depth::Pointer)
            .expect("a ring");
        let (w, h) = (
            ring.picture.image.width() as i32,
            ring.picture.image.height() as i32,
        );
        let centre = (ring.at.0 + w / 2, ring.at.1 + h / 2);
        assert!(
            (centre.0 - WIDTH as i32 / 2).abs() <= 1 && (centre.1 - HEIGHT as i32 / 2).abs() <= 1
        );
        let home = home_radius(WIDTH as f64, HEIGHT as f64);
        assert!(f64::from(w) > home * 2.0);
    });
}

/// Writes each crew's levels as PNGs into the folder `ARGONAUTS_COSMOS_SHOTS` names.
#[test]
#[ignore = "writes pictures to look at"]
fn shots() {
    let Some(folder) = std::env::var_os("ARGONAUTS_COSMOS_SHOTS").map(PathBuf::from) else {
        return;
    };
    let write = |name: String, scene: &Scene| {
        std::fs::write(folder.join(name), scene.flatten().to_png()).unwrap();
    };
    let (width, height) = FULL;
    let at = |level: Level, still: bool| View {
        still,
        home: Some(home_picture(width, height)),
        ..View::new(level, width, height)
    };
    with('R', |_, cosmos| {
        let (universe, galaxy, system) = (
            at(Level::Universe, false),
            at(Level::Galaxy(0), false),
            at(Level::System(0), false),
        );
        // Each of a zoom's steps between its two ends.
        for step in 1..6 {
            let k = f64::from(step) / 6.0;
            write(
                format!("zoom-R-galaxy-{step}.png"),
                &cosmos.zoom(&universe, &galaxy, k, Times::at(0.0)),
            );
            write(
                format!("zoom-R-system-{step}.png"),
                &cosmos.zoom(&galaxy, &system, k, Times::at(0.0)),
            );
        }
    });
    for letter in ['R', 'S', 'T', 'U'] {
        with(letter, |_, cosmos| {
            write(
                format!("universe-{letter}.png"),
                &cosmos.scene(&at(Level::Universe, true), 0.0),
            );
            let moving = cosmos.scene(&at(Level::Universe, false), 1.0);
            write(format!("universe-{letter}-moving.png"), &moving);
            for maker in 0..cosmos.universe().makers.len() {
                let key = cosmos.universe().makers[maker].key.clone();
                let scene = cosmos.scene(&at(Level::Galaxy(maker), true), 0.0);
                write(format!("galaxy-{letter}-{key}.png"), &scene);
            }
            for star in 0..cosmos.universe().stars.len() {
                let scene = cosmos.scene(&at(Level::System(star), true), 0.0);
                write(format!("system-{letter}-{star}.png"), &scene);
                let scene = cosmos.scene(&at(Level::System(star), false), 2.0);
                write(format!("system-{letter}-{star}-moving.png"), &scene);
            }
        });
    }
}
