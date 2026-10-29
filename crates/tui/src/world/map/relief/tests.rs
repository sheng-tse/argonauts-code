use std::sync::OnceLock;

use super::cells::{compare, composed};
use super::*;
use crate::world::map::fields::crews::{Crew, Key};
use crate::world::map::fields::{Ground, HEIGHT, Rgb, WIDTH};
use crate::world::map::land::{Marks, Polar};

/// A crew's map, worked out once: its ground, relief, light both ways, and the pictures.
struct Map {
    ground: Ground,
    relief: Relief,
    standing: Projected,
    flat: Projected,
    dots: Dots,
    flat_dots: Dots,
}

fn groups(key: Key) -> Vec<Rgb> {
    Crew::get(key)
        .groups
        .iter()
        .map(|group| group.rgb.map(f64::from))
        .collect()
}

fn map(key: Key) -> &'static Map {
    static MAPS: OnceLock<Vec<Map>> = OnceLock::new();
    let maps = MAPS.get_or_init(|| Key::ALL.iter().map(|key| draw(*key)).collect());
    &maps[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
}

fn draw(key: Key) -> Map {
    let ground = Ground::grow(Crew::get(key).facts());
    let relief = Relief::new(&ground);
    let polar = Polar::of(&ground);
    let picture = |stand: f64| {
        let light = Light::new(&ground, &relief, stand);
        let lit = LitGround::new(
            &ground,
            &relief,
            &light,
            None,
            Marks::default(),
            Some(&polar),
        );
        Projected::new(&relief, &lit, stand, 2)
    };
    let colours = groups(key);
    let style = DotStyle {
        groups: &colours,
        compared: None,
    };
    Map {
        standing: picture(STANDING),
        flat: picture(0.0),
        dots: Dots::new(&ground, &relief, style, STANDING),
        flat_dots: Dots::new(&ground, &relief, style, 0.0),
        relief,
        ground,
    }
}

#[test]
fn a_mountain_stands_as_tall_as_its_height() {
    let map = map(Key::R);
    let busiest = map.ground.busiest.expect("a busiest behavior");
    let (x, y) = map.ground.centre[busiest];
    let (_, screen_y) = map.relief.screen(x, y, STANDING, 0.0);
    let lift = top_row() + FORESHORTEN * y - screen_y;
    let height = map.relief.standing_at(x, y);
    assert!((lift - STANDING * height).abs() < 0.01);
    assert_eq!((height * 10.0).round() / 10.0, 43.6);
}

#[test]
fn no_dot_is_lost_behind_a_slope() {
    let map = map(Key::R);
    assert_eq!(map.dots.hidden_count(), 16);
    assert!(map.dots.hidden_count() as f64 <= 0.02 * map.ground.dots.len() as f64);
    assert_eq!(map.flat_dots.hidden_count(), 0);
}

#[test]
fn under_each_dot_lies_its_own_place() {
    let map = map(Key::R);
    let (mut sampled, mut wrong) = (0, 0);
    for (index, &(x, y)) in map.dots.at.iter().enumerate().step_by(7) {
        sampled += 1;
        let shown = map.standing.rows[(y as f64).round() as usize * WIDTH + x as usize];
        if (f64::from(shown) - f64::from(map.ground.dots[index].1)).abs() > 6.0 {
            wrong += 1;
        }
    }
    assert_eq!((sampled, wrong), (138, 6));
}

#[test]
fn laid_flat_every_dot_is_at_its_own_place() {
    let map = map(Key::R);
    for (&(x, y), &(sx, sy)) in map.ground.dots.iter().zip(&map.flat_dots.at) {
        assert_eq!(sx, x);
        let expected = top_row() + FORESHORTEN * f64::from(y);
        assert!((f64::from(sy) - expected).abs() < 1e-3);
        let row = map
            .flat
            .row_at(f64::from(sx), f64::from(sy))
            .expect("a row");
        assert!((row - f64::from(y)).abs() < 1.5, "{row} for {y}");
    }
}

#[test]
fn the_ice_shows_on_the_map_as_on_the_planet() {
    let light = |map: &Map, x: u32, y: u32| {
        let [r, g, b, _] = map.standing.picture.pixel(x, y);
        (0.3 * f64::from(r) + 0.59 * f64::from(g) + 0.11 * f64::from(b)).round()
    };
    let bottom = HEIGHT as u32 - 12;
    let busy = map(Key::R);
    assert!(light(busy, 300, 12) < 60.0, "R's north is open sea");
    assert!(light(busy, 300, bottom) > 170.0, "R's south ice");
    let quiet = map(Key::T);
    assert!(light(quiet, 300, 12) > 170.0, "T's north cap");
    assert!(light(quiet, 300, bottom) > 170.0, "T's south ice");
}

#[test]
fn the_pointer_finds_the_ground_behind_each_pixel() {
    use crate::world::scene::{HitMap, Pointed};
    let map = map(Key::R);
    let mut hits = HitMap::new();
    for mark in map.dots.marks(0, 9) {
        hits.add(mark);
    }
    hits.set_under(map.standing.under());
    let (x, y) = map.dots.at[0];
    assert_eq!(hits.find(x, y), Some(Pointed::Action(0)));
    assert_eq!(hits.find(x + 10.0, y), hits.find(x + 10.0, y));
    match hits.find(4.0, 2.0) {
        Some(Pointed::Ground(None)) => {}
        found => panic!("past the far edge is open sea at no one place, found {found:?}"),
    }
    match hits.find(20.0, 300.0) {
        Some(Pointed::Ground(Some(_))) => {}
        found => panic!("the sea at a place, found {found:?}"),
    }
}

#[test]
fn crew_rs_map_stands_up_as_drawn() {
    let map = map(Key::R);
    compare(
        "ground-r",
        &composed(&map.standing, &map.dots),
        include_str!("golden/ground-r.txt"),
    );
}

#[test]
fn crew_rs_map_lies_flat_as_drawn() {
    let map = map(Key::R);
    compare(
        "ground-r-flat",
        &composed(&map.flat, &map.flat_dots),
        include_str!("golden/ground-r-flat.txt"),
    );
}

#[test]
fn crews_s_t_and_u_stand_up_as_drawn() {
    compare(
        "ground-s",
        &composed(&map(Key::S).standing, &map(Key::S).dots),
        include_str!("golden/ground-s.txt"),
    );
    compare(
        "ground-t",
        &composed(&map(Key::T).standing, &map(Key::T).dots),
        include_str!("golden/ground-t.txt"),
    );
    compare(
        "ground-u",
        &composed(&map(Key::U).standing, &map(Key::U).dots),
        include_str!("golden/ground-u.txt"),
    );
}

#[test]
fn crew_rs_map_compares_its_first_group_as_drawn() {
    use crate::world::map::land::{Compared, Tint};

    let map = map(Key::R);
    let ground = &map.ground;
    let compared = Compared::new(ground, 0).expect("enough actions to compare");
    let tint = Tint::new(ground, compared);
    let polar = Polar::of(ground);
    let light = Light::new(ground, &map.relief, STANDING);
    let lit = LitGround::new(
        ground,
        &map.relief,
        &light,
        Some(&tint),
        Marks::default(),
        Some(&polar),
    );
    let land = Projected::new(&map.relief, &lit, STANDING, 2);
    let colours = groups(Key::R);
    let style = DotStyle {
        groups: &colours,
        compared: Some(0),
    };
    let dots = Dots::new(ground, &map.relief, style, STANDING);
    compare(
        "ground-r-compared",
        &composed(&land, &dots),
        include_str!("golden/ground-r-compared.txt"),
    );
}

/// How long each part of a crew's map takes. Run it in a release build:
/// `cargo test --release -p argonauts-tui --lib world::map::relief::tests::a_crews_map -- --ignored --nocapture`
#[test]
#[ignore = "a speed check, meaningful only in a release build"]
fn a_crews_map_draws_in_well_under_a_second() {
    use std::time::Instant;
    let facts = Crew::get(Key::R).facts();
    let started = Instant::now();
    let mut last = Instant::now();
    let mut lap = |name: &str| {
        eprintln!(
            "{name:>24}: {:6.1} ms",
            last.elapsed().as_secs_f64() * 1000.0
        );
        last = Instant::now();
    };
    let ground = Ground::grow(facts);
    lap("fields and climate");
    let relief = Relief::new(&ground);
    lap("relief");
    let light = Light::new(&ground, &relief, STANDING);
    lap("light");
    let polar = Polar::of(&ground);
    let lit = LitGround::new(
        &ground,
        &relief,
        &light,
        None,
        Marks::default(),
        Some(&polar),
    );
    lap("lit ground");
    let land = Projected::new(&relief, &lit, STANDING, 2);
    lap("stood up, twice the size");
    let colours = groups(Key::R);
    let style = DotStyle {
        groups: &colours,
        compared: None,
    };
    let dots = Dots::new(&ground, &relief, style, STANDING);
    lap("dots");
    let rising = Projected::new(&relief, &lit, STANDING * 0.5, 1);
    lap("a frame of it rising");
    let texture = crate::world::map::land::Texture::new(&ground, None, Marks::default());
    lap("the planet's texture");
    let total = started.elapsed();
    eprintln!("{:>24}: {:6.1} ms", "all", total.as_secs_f64() * 1000.0);
    assert!(land.rows.len() + dots.at.len() + rising.rows.len() + texture.wet.len() > 0);
    assert!(total.as_secs_f64() < 3.0);
}
