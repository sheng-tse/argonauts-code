use std::sync::OnceLock;

use super::*;
use crate::world::geo::{LonLat, Plate};
use crate::world::map::fields::crews::{Crew, Key};
use crate::world::map::land::Marks;
use crate::world::map::relief::cells::{compare_means, mean_colours};

/// The made-up crews' pace, and the actions of theirs no label fits.
fn pace(key: Key) -> (f64, u32) {
    match key {
        Key::R | Key::U => (30.0, 38),
        Key::S => (33.0, 12),
        Key::T => (4.0, 90),
    }
}

fn ground(key: Key) -> &'static Ground {
    static GROUNDS: OnceLock<Vec<Ground>> = OnceLock::new();
    let grounds = GROUNDS.get_or_init(|| {
        Key::ALL
            .iter()
            .map(|key| Ground::grow(Crew::get(*key).facts()))
            .collect()
    });
    &grounds[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
}

pub(super) fn facts(key: Key) -> PlanetFacts {
    let crew = Crew::get(key);
    let (per_day, unlabelled) = pace(key);
    let night = crew
        .detail
        .minute
        .iter()
        .filter(|minute| {
            let hour = f64::from(**minute) / 60.0;
            !(5.0..22.0).contains(&hour)
        })
        .count();
    PlanetFacts {
        per_day,
        unlabelled,
        night: night as u32,
        fresh: crew.detail.day.iter().map(|day| *day < 7).collect(),
        bands: Vec::new(),
    }
}

pub(super) fn colours(key: Key) -> DotColours {
    DotColours {
        groups: Crew::get(key)
            .groups
            .iter()
            .map(|group| group.rgb.map(f64::from))
            .collect(),
        compared: None,
    }
}

pub(super) fn planet(key: Key) -> &'static Planet {
    static PLANETS: OnceLock<Vec<Planet>> = OnceLock::new();
    let planets = PLANETS.get_or_init(|| {
        Key::ALL
            .iter()
            .map(|key| {
                let ground = ground(*key);
                let marks = Marks {
                    icebergs: true,
                    ..Marks::default()
                };
                let texture = Texture::new(ground, None, marks);
                let nature = Nature {
                    shown: true,
                    ..Nature::default()
                };
                Planet::new(0, ground, texture, &facts(*key), nature, &colours(*key))
            })
            .collect()
    });
    &planets[Key::ALL.iter().position(|k| *k == key).unwrap_or(0)]
}

const SIZE: (usize, usize) = (1152, 546);

fn view(lon: f64, tilt: f64) -> View {
    View { lon, tilt }
}

fn light(pixel: [u8; 4]) -> f64 {
    0.3 * f64::from(pixel[0]) + 0.59 * f64::from(pixel[1]) + 0.11 * f64::from(pixel[2])
}

/// Compares a planet picture's terminal cells with its golden.
fn compare(name: &str, picture: &Canvas, golden: &str) {
    let means = mean_colours(SIZE, (18, 42), |x, y| {
        let [r, g, b, _] = picture.pixel(x as u32, y as u32);
        [r, g, b]
    });
    compare_means(name, &means, golden, || picture.to_png());
}

#[test]
fn a_busier_crew_turns_faster_and_an_idle_one_stands_still() {
    let r = planet(Key::R).turn_seconds().expect("R turns");
    let t = planet(Key::T).turn_seconds().expect("T turns");
    assert!((r - 24.0).abs() < 0.01, "{r}");
    assert!((t - 51.0).abs() < 0.5, "{t}");
    assert_eq!(turn_seconds(0.3), None);
}

#[test]
fn the_air_thickens_with_last_weeks_actions() {
    let busy = Air::of(Look::Rock, 30.0);
    let quiet = Air::of(Look::Rock, 0.3);
    assert!(busy.thickness > 0.9 && quiet.thickness == 0.08);
    assert!(busy.reach(240.0) > quiet.reach(240.0));
    assert_eq!(Air::of(Look::Ice, 4.0).colour, [196.0, 220.0, 255.0]);
}

#[test]
fn clouds_cover_as_much_sky_as_the_share_of_actions_no_label_fits() {
    assert!(
        planet(Key::R).skin.clouds.is_none(),
        "R's 4% is under the floor"
    );
    let t = planet(Key::T);
    let clouds = t.skin.clouds.as_ref().expect("T has clouds");
    let texture = &t.skin.texture;
    let (mut sum, mut weight) = (0.0, 0.0);
    for row in 0..texture.height {
        let w = (std::f64::consts::FRAC_PI_2
            - (row as f64 + 0.5) / texture.height as f64 * std::f64::consts::PI)
            .cos();
        for column in (0..texture.width).step_by(3) {
            sum += f64::from(clouds[row * texture.width + column]) * w;
            weight += w;
        }
    }
    let actions = ground(Key::T).facts.dots.len() as f64;
    let share = 90.0 / (actions + 90.0);
    let covered = sum / weight / 0.86;
    assert!((covered - share).abs() < 0.03, "{covered} for {share}");
}

#[test]
fn a_night_working_crew_has_an_aurora_and_only_the_night_shows_it() {
    let s = planet(Key::S);
    let glow = s.skin.glow.as_ref().expect("S works nights");
    let texture = &s.skin.texture;
    let row_at = |lat: f64| {
        ((std::f64::consts::FRAC_PI_2 - lat) / std::f64::consts::PI * texture.height as f64)
            as usize
    };
    let ring: f64 = (0..texture.width)
        .map(|column| f64::from(glow[row_at(1.16) * texture.width + column][1]))
        .sum();
    assert!(ring > 0.0);
    assert!(
        planet(Key::T).skin.glow.is_none(),
        "T's 1% at night is too few"
    );

    // From high above the north, the curtain lights the night side and not the day.
    let above = view(0.0, 1.1);
    let ring_light = |side: Side| {
        let picture = s.ground(&s.globe(above, SIZE), SIZE, side, CloudsIn::Ground, 0.0);
        let globe = s.globe(above, SIZE);
        let mut green = 0.0;
        for step in 0..72 {
            let lon = f64::from(step) * 5.0_f64.to_radians();
            let at = globe.project(LonLat::new(lon, 1.15));
            if at.z <= 0.3 {
                continue;
            }
            let x = (at.x as usize).saturating_sub(picture.left) as u32;
            let y = (at.y as usize).saturating_sub(picture.top) as u32;
            let [r, g, b, _] = picture.canvas.pixel(x, y);
            green += f64::from(g) - (f64::from(r) + f64::from(b)) / 2.0;
        }
        green
    };
    assert!(ring_light(Side::Night) > ring_light(Side::Day) + 200.0);
}

#[test]
fn n_turns_the_planet_to_its_night_side() {
    let r = planet(Key::R);
    let facing = view(-0.35, 0.32);
    let disc_light = |side: Side| {
        let picture = r.picture(facing, side, SIZE, None, 0.0);
        let mut sum = 0.0;
        for y in (200..346).step_by(4) {
            for x in (500..652).step_by(4) {
                sum += light(picture.pixel(x, y));
            }
        }
        sum
    };
    assert!(disc_light(Side::Night) < disc_light(Side::Day) * 0.3);
    assert!(Sun::of(Side::Night).to[2] < 0.0 && Sun::of(Side::Day).to[2] > 0.0);
}

#[test]
fn the_globes_middle_shows_the_ground_it_faces() {
    let r = planet(Key::R);
    let ground = ground(Key::R);
    let busiest = ground.busiest.expect("R has a busiest behavior");
    let (x, y) = ground.centre[busiest];
    let place = Plate::MAP.place(x, y);
    let facing = view(place.lon, place.lat);
    let picture = r.ground(
        &r.globe(facing, SIZE),
        SIZE,
        Side::Day,
        CloudsIn::Ground,
        0.0,
    );
    let middle = picture
        .canvas
        .pixel((576 - picture.left) as u32, (273 - picture.top) as u32);
    let (column, row) = r.skin.texel(place.lon, place.lat);
    let texel = r.skin.texture.colour(column, row);
    // Lit straight on, the ground shows a little brighter than its own colour.
    assert_eq!(middle[3], 255);
    for ch in 0..3 {
        let (shown, own) = (f64::from(middle[ch]), f64::from(texel[ch]));
        assert!(
            shown > own * 0.8 && shown < own * 1.4 + 8.0,
            "{middle:?} for {texel:?}"
        );
    }
}

#[test]
fn the_air_glows_past_the_rim_and_the_sky_shows_beyond() {
    let r = planet(Key::R);
    let picture = r.ground(
        &r.globe(view(0.0, 0.32), SIZE),
        SIZE,
        Side::Day,
        CloudsIn::Ground,
        0.0,
    );
    let globe = r.globe(view(0.0, 0.32), SIZE);
    let pixel = |x: f64, y: f64| {
        picture.canvas.pixel(
            (x as usize - picture.left) as u32,
            (y as usize - picture.top) as u32,
        )
    };
    // Toward the sun, up and to the left, the air glows just past the rim.
    let toward = (-0.8_f64, -0.6_f64);
    let past = |by: f64| {
        let r = globe.radius + by;
        pixel(globe.centre.0 + toward.0 * r, globe.centre.1 + toward.1 * r)
    };
    assert!(past(2.0)[3] > 60, "{:?}", past(2.0));
    assert!(past(r.air.reach(globe.radius) + 1.0)[3] == 0);
}

#[test]
fn crew_rs_planet_is_drawn_as_before() {
    let r = planet(Key::R);
    let picture = r.picture(view(-0.35, 0.32), Side::Day, SIZE, None, 0.0);
    compare("planet-r", &picture, include_str!("golden/planet-r.txt"));
}

#[test]
fn crew_rs_planet_turned_east_and_less_tilted_is_drawn_as_before() {
    let r = planet(Key::R);
    let picture = r.picture(view(1.0, 0.18), Side::Day, SIZE, None, 0.0);
    compare(
        "planet-r-east",
        &picture,
        include_str!("golden/planet-r-east.txt"),
    );
}

#[test]
fn crew_us_planet_is_drawn_as_before() {
    let u = planet(Key::U);
    let picture = u.picture(view(1.55, 0.42), Side::Day, SIZE, None, 0.0);
    compare("planet-u", &picture, include_str!("golden/planet-u.txt"));
}

#[test]
fn crew_ts_clouded_planet_is_drawn_as_before() {
    let t = planet(Key::T);
    let picture = t.picture(view(-0.35, 0.32), Side::Day, SIZE, None, 0.0);
    compare("planet-t", &picture, include_str!("golden/planet-t.txt"));
}

#[test]
fn crew_ss_night_side_is_drawn_as_before() {
    let s = planet(Key::S);
    let picture = s.picture(view(0.4, 0.75), Side::Night, SIZE, None, 0.0);
    compare(
        "planet-s-night",
        &picture,
        include_str!("golden/planet-s-night.txt"),
    );
}

#[test]
fn a_crew_under_40_actions_is_a_gas_giant_banded_by_its_clis() {
    let mut few = Crew::get(Key::R).facts();
    few.dots.truncate(30);
    let ground = Ground::grow(few);
    assert_eq!(ground.look, Look::Gas);
    let warm = [224.0, 138.0, 102.0];
    let cool = [106.0, 166.0, 255.0];
    let facts = PlanetFacts {
        per_day: 2.0,
        bands: vec![
            Band {
                colour: warm,
                share: 0.6,
            },
            Band {
                colour: cool,
                share: 0.4,
            },
        ],
        ..PlanetFacts::default()
    };
    let texture = Texture::new(&ground, None, Marks::default());
    let gas = Planet::new(
        0,
        &ground,
        texture,
        &facts,
        Nature::default(),
        &DotColours::default(),
    );
    assert!(gas.skin.clouds.is_none() && gas.skin.glow.is_none());
    let texture = &gas.skin.texture;
    let (mut warmer, mut cooler) = (0, 0);
    for row in (0..texture.height).step_by(8) {
        let [r, _, b] = texture.colour(100, row);
        if r > b {
            warmer += 1;
        } else {
            cooler += 1;
        }
    }
    assert!(
        warmer > cooler && cooler > 0,
        "{warmer} warm rows, {cooler} cool"
    );
    assert!(texture.wet.iter().all(|wet| !wet));
}

mod spin {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use super::*;
    use crate::world::planet_fx::{Hurricane, Weather};
    use crate::world::raster::{Brush, Colour, Path};
    use crate::world::scene::{Depth, Part, Scene};

    /// A quarter of the map's size, so a whole turn draws quickly.
    const SMALL: (usize, usize) = (288, 137);

    fn shot<'a>(step: u32, weather: &'a [&'a dyn Weather], moment: Moment) -> Shot<'a> {
        Shot {
            view: View::stepped(f64::from(step) * std::f64::consts::TAU / 36.0),
            side: Side::Day,
            size: SMALL,
            clouds: CloudsIn::Ground,
            weather,
            standing: &[],
            moment,
            sky: None,
        }
    }

    fn part(scene: &Scene, depth: Depth) -> Vec<u64> {
        scene
            .stack()
            .iter()
            .filter(|part| part.depth() == depth)
            .map(|part| part.picture().key)
            .collect()
    }

    fn weather_parts(scene: &Scene) -> usize {
        scene
            .stack()
            .iter()
            .filter(|part| matches!(part, Part::Sprite(_)) && part.depth() == Depth::Weather)
            .count()
    }

    /// A hurricane on R's busiest land, its picture a pale disc with one dark arm.
    fn storm() -> Hurricane {
        let ground = super::ground(Key::R);
        let busiest = ground.busiest.expect("R has a busiest behavior");
        let (x, y) = ground.centre[busiest];
        let mut picture = Canvas::new(150, 150);
        let mut disc = Path::new();
        disc.circle(75.0, 75.0, 60.0);
        picture.fill(&disc, &Brush::new(Colour::rgb(230, 234, 240)));
        let mut arm = Path::new();
        arm.circle(105.0, 75.0, 18.0);
        picture.fill(&arm, &Brush::new(Colour::rgb(40, 44, 60)));
        Hurricane {
            at: (x, y),
            radius: 60.0,
            category: 2,
            south: false,
            picture: Arc::new(picture),
        }
    }

    #[test]
    fn each_step_is_drawn_once_and_a_second_turn_draws_nothing() {
        let r = planet(Key::R);
        let mut spin = Spin::new();
        let mut grounds = BTreeSet::new();
        for step in 0..36 {
            let scene = spin.scene(r, &shot(step, &[], Moment::default()));
            grounds.extend(part(&scene, Depth::Ground));
            assert!(scene.moves, "R's planet turns");
        }
        assert_eq!(grounds.len(), 36);
        let kept = spin.kept();
        for step in 0..36 {
            let scene = spin.scene(r, &shot(step, &[], Moment::default()));
            assert!(grounds.contains(&part(&scene, Depth::Ground)[0]));
        }
        assert_eq!(spin.kept(), kept, "nothing new drawn on the second turn");
    }

    /// The planet as the tab shows it, at `cell` pixels a cell: the map's 64 × 13 cells, the
    /// clouds apart where they drift.
    fn shown_at<'a>(step: u32, cell: (u16, u16), weather: &'a [&'a dyn Weather]) -> Shot<'a> {
        Shot {
            size: (64 * usize::from(cell.0), 13 * usize::from(cell.1)),
            clouds: CloudsIn::Apart,
            ..shot(step, weather, Moment::default())
        }
    }

    /// One turn of `planet` through `spin` and `stage`: how many pictures it drew and how many
    /// it sent.
    fn one_turn(
        planet: &Planet,
        spin: &mut Spin,
        stage: &mut crate::world::deliver::Stage,
        cell: (u16, u16),
    ) -> (u64, usize) {
        let spot = crate::world::deliver::Spot {
            cells: ratatui::layout::Rect::new(0, 0, 64, 13),
            cell,
        };
        let drawn = spin.drawn();
        let mut sent = 0;
        for step in 0..36 {
            let scene = spin.scene(planet, &shown_at(step, cell, &[]));
            let out = String::from_utf8_lossy(&stage.show(&scene, spot)).into_owned();
            sent += out.matches("a=t,").count();
        }
        (spin.drawn() - drawn, sent)
    }

    /// A whole turn of R's planet and of T's, whose clouds drift apart from its ground and
    /// dots, stays kept here and in the terminal at the usual cell, and R's at cells twice as
    /// big each way: the second turn draws and sends nothing.
    #[test]
    fn a_second_turn_draws_and_sends_nothing_at_the_cells_in_use() {
        for (key, cell) in [(Key::R, (18, 42)), (Key::T, (18, 42)), (Key::R, (36, 84))] {
            let mut spin = Spin::new();
            let mut stage = crate::world::deliver::Stage::new(1).packing_in_line();
            let (drawn, sent) = one_turn(planet(key), &mut spin, &mut stage, cell);
            assert!(
                drawn >= 36 && sent >= 36,
                "{key:?} at {cell:?}: {drawn} drawn, {sent} sent"
            );
            assert_eq!(
                one_turn(planet(key), &mut spin, &mut stage, cell),
                (0, 0),
                "{key:?} at {cell:?} after {drawn} drawn"
            );
        }
    }

    /// A turn bigger than the spin keeps holds on to the same steps pass after pass, rather
    /// than letting each go just before it comes round again.
    #[test]
    fn a_turn_bigger_than_the_limit_keeps_the_same_share_of_its_steps() {
        let r = planet(Key::R);
        let mut first = Spin::new();
        for step in 0..36 {
            first.scene(r, &shot(step, &[], Moment::default()));
        }
        let mut spin = Spin::with_limit(first.bytes() / 2);
        let turn = |spin: &mut Spin| {
            let drawn = spin.drawn();
            for step in 0..36 {
                spin.scene(r, &shot(step, &[], Moment::default()));
            }
            spin.drawn() - drawn
        };
        assert_eq!(turn(&mut spin), first.drawn());
        let second = turn(&mut spin);
        assert!(
            second < first.drawn() * 2 / 3,
            "half the turn stays kept: {second} of {} drawn again",
            first.drawn()
        );
        assert_eq!(turn(&mut spin), second, "the same share every pass");
    }

    #[test]
    fn a_drag_draws_only_the_steps_it_reaches() {
        let r = planet(Key::R);
        let mut spin = Spin::new();
        spin.scene(r, &shot(0, &[], Moment::default()));
        let first = spin.kept();
        for step in [1, 2, 3, 2, 1] {
            spin.scene(r, &shot(step, &[], Moment::default()));
        }
        assert_eq!(spin.kept(), first + 3);
    }

    #[test]
    fn the_weather_is_drawn_into_each_picture_at_its_steps_moment() {
        let r = planet(Key::R);
        let storm = storm();
        let weather: [&dyn Weather; 1] = [&storm];
        let mut spin = Spin::new();
        let busiest = super::ground(Key::R).busiest.expect("a busiest behavior");
        let place = Plate::MAP.place(
            super::ground(Key::R).centre[busiest].0,
            super::ground(Key::R).centre[busiest].1,
        );
        let step = View::stepped(place.lon).step();
        let with = spin.scene(r, &shot(step, &weather, Moment::default()));
        let without = spin.scene(r, &shot(step, &[], Moment::default()));
        assert_ne!(part(&with, Depth::Ground), part(&without, Depth::Ground));
        assert_eq!(weather_parts(&with), 0, "no loops while it turns");
        // The same step shows the same pose, whatever the clock says.
        let later = Moment {
            t: 3.7,
            ..Moment::default()
        };
        let again = spin.scene(r, &shot(step, &weather, later));
        assert_eq!(part(&with, Depth::Ground), part(&again, Depth::Ground));
        // A turn holds whole loops: the step after the last is the first again.
        let turn = r.turn_seconds().expect("R turns");
        let loops = (turn / storm.motion().seconds).round();
        assert_eq!(loops, 2.0);
        assert_eq!(turning_moment(r, &storm, 0), turning_moment(r, &storm, 36));
        assert_ne!(turning_moment(r, &storm, 0), turning_moment(r, &storm, 9));
    }

    #[test]
    fn small_loops_play_only_while_the_turn_holds() {
        let r = planet(Key::R);
        let storm = storm();
        let weather: [&dyn Weather; 1] = [&storm];
        let ground = super::ground(Key::R);
        let busiest = ground.busiest.expect("a busiest behavior");
        let place = Plate::MAP.place(ground.centre[busiest].0, ground.centre[busiest].1);
        let step = View::stepped(place.lon).step();
        let mut spin = Spin::new();
        let held = |t: f64| Moment {
            t,
            holding: true,
            ..Moment::default()
        };
        let mut loop_pictures = BTreeSet::new();
        for picture in 0..48 {
            let scene = spin.scene(r, &shot(step, &weather, held(f64::from(picture) / 8.0)));
            assert_eq!(weather_parts(&scene), 1);
            assert!(scene.moves);
            loop_pictures.extend(part(&scene, Depth::Weather));
            // The dots stay on top of the loop.
            assert_eq!(part(&scene, Depth::Dots).len(), 1);
        }
        // Six seconds of a 15 s loop of 24 pictures: 8 a second, up to one new picture each.
        assert!(
            loop_pictures.len() > 4 && loop_pictures.len() <= 24,
            "{}",
            loop_pictures.len()
        );
        let still = Moment {
            still: true,
            holding: true,
            ..Moment::default()
        };
        let scene = spin.scene(r, &shot(step, &weather, still));
        assert_eq!(weather_parts(&scene), 0);
        assert!(!scene.moves, "nothing moves with motion reduced");
    }

    #[test]
    fn an_idle_crews_planet_stands_still_and_only_its_weather_moves() {
        let ground = super::ground(Key::R);
        let facts = PlanetFacts {
            per_day: 0.3,
            ..super::facts(Key::R)
        };
        let texture = Texture::new(ground, None, Marks::default());
        let idle = Planet::new(
            9,
            ground,
            texture,
            &facts,
            Nature::default(),
            &colours(Key::R),
        );
        assert_eq!(idle.turn_seconds(), None);
        let mut spin = Spin::new();
        let scene = spin.scene(&idle, &shot(4, &[], Moment::default()));
        assert!(!scene.moves);
        assert_eq!(turned(&idle, 0.7, 30.0), 0.7);
        let storm = storm();
        let weather: [&dyn Weather; 1] = [&storm];
        let busiest = ground.busiest.expect("a busiest behavior");
        let place = Plate::MAP.place(ground.centre[busiest].0, ground.centre[busiest].1);
        let step = View::stepped(place.lon).step();
        let scene = spin.scene(&idle, &shot(step, &weather, Moment::default()));
        assert!(
            scene.moves && weather_parts(&scene) == 1,
            "its storm turns in place"
        );
        let r = planet(Key::R);
        assert!((turned(r, 0.0, 6.0) + std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    }

    #[test]
    fn clouds_apart_drift_over_the_ground_on_36_pictures_of_their_own() {
        let t = planet(Key::T);
        let mut spin = Spin::new();
        let apart = |step: u32, drift: f64, holding: bool| Shot {
            clouds: CloudsIn::Apart,
            moment: Moment {
                drift,
                holding,
                ..Moment::default()
            },
            ..shot(step, &[], Moment::default())
        };
        let scene = spin.scene(t, &apart(5, 0.0, false));
        let clouds = part(&scene, Depth::Weather);
        assert_eq!(clouds.len(), 1);
        assert_eq!(part(&scene, Depth::Dots).len(), 1);
        // 10° of drift takes about 4.8 s: the clouds then show as they would a step back.
        let drifted = spin.scene(t, &apart(5, 5.0, false));
        let back = spin.scene(t, &apart(4, 0.0, false));
        assert_eq!(part(&drifted, Depth::Weather), part(&back, Depth::Weather));
        assert_ne!(part(&drifted, Depth::Weather), clouds);
        assert_eq!(part(&drifted, Depth::Ground), part(&scene, Depth::Ground));
        assert!(scene.moves);
        // R has no clouds to keep apart.
        let r = spin.scene(planet(Key::R), &apart(5, 0.0, false));
        assert!(part(&r, Depth::Weather).is_empty() && part(&r, Depth::Dots).is_empty());
    }

    #[test]
    fn the_scene_shows_what_the_still_picture_shows() {
        let r = planet(Key::R);
        let mut spin = Spin::new();
        let view = View::stepped(-0.35);
        let scene = spin.scene(
            r,
            &Shot {
                view,
                size: SIZE,
                ..shot(0, &[], Moment::default())
            },
        );
        let flat = scene.flatten();
        let picture = r.picture(view, Side::Day, SIZE, None, 0.0);
        let means = |pixel: &dyn Fn(usize, usize) -> [u8; 3]| mean_colours(SIZE, (18, 42), pixel);
        let from_scene = means(&|x, y| {
            let at = (y * SIZE.0 + x) * 4;
            let p = flat.pixels();
            [p[at], p[at + 1], p[at + 2]]
        });
        let from_picture = means(&|x, y| {
            let [r, g, b, _] = picture.pixel(x as u32, y as u32);
            [r, g, b]
        });
        compare_means("planet-scene", &from_scene, &from_picture, || flat.to_png());
    }

    #[test]
    fn the_weather_lies_on_the_globe_where_the_map_has_it() {
        let r = planet(Key::R);
        let storm = storm();
        let weather: [&dyn Weather; 1] = [&storm];
        let place = Plate::MAP.place(storm.at.0, storm.at.1);
        let view = View::stepped(place.lon + 0.35);
        let mut spin = Spin::new();
        let held = Moment {
            t: 1.0,
            holding: true,
            ..Moment::default()
        };
        let scene = spin.scene(
            r,
            &Shot {
                view,
                size: SIZE,
                ..shot(0, &weather, held)
            },
        );
        let sprite = scene
            .sprites()
            .iter()
            .find(|sprite| sprite.id == crate::world::scene::PartId::new("planet weather", 0))
            .expect("the storm's loop");
        let globe = r.globe(view, SIZE);
        let at = globe.project(place);
        let (left, top) = (f64::from(sprite.at.0), f64::from(sprite.at.1));
        let (width, height) = (
            sprite.picture.image.width() as f64,
            sprite.picture.image.height() as f64,
        );
        assert!(at.x > left && at.x < left + width && at.y > top && at.y < top + height);
        if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURES_OUT") {
            let _ = std::fs::write(
                std::path::Path::new(&dir).join("planet-weather.png"),
                scene.flatten().to_png(),
            );
        }
    }

    #[test]
    fn a_spin_lets_the_pictures_used_longest_ago_go_first() {
        let r = planet(Key::R);
        let one = SMALL.0 * SMALL.1 * 4;
        let mut spin = Spin::with_limit(one * 4);
        for step in 0..12 {
            spin.scene(r, &shot(step, &[], Moment::default()));
        }
        assert!(
            spin.bytes() <= one * 4 && spin.kept() < 12,
            "{}",
            spin.kept()
        );
        let kept = spin.kept();
        spin.scene(r, &shot(11, &[], Moment::default()));
        assert_eq!(spin.kept(), kept, "the newest step is still kept");
    }
}

mod pointing {
    use super::*;
    use crate::world::map::land::{GroundKind, line_at};
    use crate::world::scene::{Mark, Pointed, Shape};

    fn facing() -> View {
        View::stepped(-0.35)
    }

    #[test]
    fn pointing_at_a_dot_on_the_planet_finds_that_dot() {
        let r = planet(Key::R);
        let hits = r.hits(facing(), SIZE, &[], &[]);
        let facing_dots = r.dots_facing(&r.globe(facing(), SIZE));
        let globe = r.globe(facing(), SIZE);
        let (index, (x, y)) = facing_dots
            .iter()
            .enumerate()
            .filter_map(|(index, at)| at.map(|at| (index, at)))
            .find(|(index, _)| {
                let (mx, my) = super::ground(Key::R).dots[*index];
                globe
                    .project(Plate::MAP.place(f64::from(mx), f64::from(my)))
                    .z
                    > 0.8
            })
            .expect("a dot near the middle");
        let found = hits.find(x + 1.0, y);
        let Some(Pointed::Action(at)) = found else {
            panic!("found {found:?}");
        };
        // A neighbour may lie as near; the dot found is one within reach of the pointer.
        let (fx, fy) = facing_dots[at as usize].expect("it faces you");
        assert!((fx - x - 1.0).hypot(fy - y) <= 6.0, "{index} vs {at}");
    }

    #[test]
    fn pointing_at_land_on_the_planet_says_what_the_map_says_there() {
        let r = planet(Key::R);
        let ground = super::ground(Key::R);
        let hits = r.hits(facing(), SIZE, &[], &[]);
        let globe = r.globe(facing(), SIZE);
        let mut found = None;
        'search: for y in (200..360).step_by(5) {
            for x in (420..740).step_by(5) {
                let (x, y) = (x as f32, y as f32);
                if let Some(Pointed::Ground(Some(place))) = hits.find(x, y)
                    && let OnGlobe::Ground { x: mx, y: my } = r.on_globe(place, Side::Day, 0.0)
                    && line_at(ground, mx, my).kind != GroundKind::Sea
                {
                    found = Some((x, y, place, mx, my));
                    break 'search;
                }
            }
        }
        let (x, y, place, mx, my) = found.expect("land facing you");
        let surface = globe
            .surface_at(f64::from(x), f64::from(y))
            .expect("on the disc");
        assert!((surface.place.lon - place.lon).abs() < 1e-9);
        let (px, py) = Plate::MAP.pixel(surface.place);
        assert!((px - mx).abs() < 1e-6 && (py - my).abs() < 1e-6);
    }

    #[test]
    fn a_mark_of_the_map_answers_where_it_lies_on_the_planet() {
        let r = planet(Key::R);
        let globe = r.globe(facing(), SIZE);
        // A lake near the middle of the face, and a volcano on the far side.
        let near = Plate::MAP.pixel(LonLat::new(-0.3, 0.3));
        let far = Plate::MAP.pixel(LonLat::new(2.8, 0.1));
        let marks = [
            Mark::new(
                Shape::Disc {
                    x: near.0 as f32,
                    y: near.1 as f32,
                    radius: 8.0,
                },
                Pointed::Feature(3),
            )
            .reach(4.0),
            Mark::new(
                Shape::Point {
                    x: far.0 as f32,
                    y: far.1 as f32,
                },
                Pointed::Feature(7),
            )
            .reach(10.0),
        ];
        let hits = r.hits(facing(), SIZE, &marks, &[]);
        let at = globe.project(LonLat::new(-0.3, 0.3));
        assert_eq!(
            hits.find(at.x as f32, at.y as f32),
            Some(Pointed::Feature(3))
        );
        assert!(
            hits.marks()
                .iter()
                .all(|mark| mark.pointed != Pointed::Feature(7)),
            "the far side's volcano is not found"
        );
    }

    #[test]
    fn the_planets_own_ice_answers_for_quiet_and_idle_days() {
        let t = planet(Key::T);
        let polar = Polar::of(super::ground(Key::T));
        let south = LonLat::new(0.5, -polar.edge_south() - 0.05);
        assert_eq!(t.on_globe(south, Side::Day, 0.0), OnGlobe::SouthIce);
        let north = LonLat::new(0.5, polar.edge_north() + 0.05);
        assert_eq!(t.on_globe(north, Side::Day, 0.0), OnGlobe::IceCap);
        let berg = polar.bergs.first().expect("T has quiet stretches");
        assert_eq!(
            t.on_globe(LonLat::new(berg.lon, berg.lat), Side::Day, 0.0),
            OnGlobe::Iceberg(berg.index)
        );
        // R is never idle 3 days, so it has no north cap.
        let r = planet(Key::R);
        assert_ne!(
            r.on_globe(LonLat::new(0.5, 1.5), Side::Day, 0.0),
            OnGlobe::IceCap
        );
    }

    #[test]
    fn the_aurora_answers_only_on_the_night_side() {
        let s = planet(Key::S);
        let ring = LonLat::new(0.2, 1.16);
        assert_eq!(s.on_globe(ring, Side::Night, 0.0), OnGlobe::Aurora);
        assert_ne!(s.on_globe(ring, Side::Day, 0.0), OnGlobe::Aurora);
        let t = planet(Key::T);
        assert_ne!(
            t.on_globe(LonLat::new(2.0, -1.16), Side::Night, 0.0),
            OnGlobe::Aurora
        );
    }

    #[test]
    fn the_clouds_answer_where_they_are_drawn_and_move_with_their_drift() {
        let t = planet(Key::T);
        let clouds = t.skin.clouds.as_ref().expect("T has clouds");
        let texture = &t.skin.texture;
        let (width, height) = (texture.width, texture.height);
        let row = height / 2 + 40;
        let column = (0..width)
            .find(|column| {
                clouds[row * width + column] > 0.7
                    && clouds[row * width + (column + width - 43) % width] < 0.2
            })
            .expect("a thick cloud with clear sky a step west of it");
        let lat =
            std::f64::consts::FRAC_PI_2 - (row as f64 + 0.5) / height as f64 * std::f64::consts::PI;
        let lon =
            (column as f64 + 0.5) / width as f64 * std::f64::consts::TAU - std::f64::consts::PI;
        let place = LonLat::new(lon, lat);
        assert_eq!(t.on_globe(place, Side::Day, 0.0), OnGlobe::Clouds);
        // Drifted a step east, the cloud has moved on and the clear sky behind it is here.
        let step = std::f64::consts::TAU / 36.0;
        assert_ne!(t.on_globe(place, Side::Day, step), OnGlobe::Clouds);
        let held = Moment {
            drift: 30.0,
            still: true,
            ..Moment::default()
        };
        assert_eq!(shown_drift(CloudsIn::Apart, held), 0.0);
        assert_eq!(
            shown_drift(
                CloudsIn::Ground,
                Moment {
                    drift: 30.0,
                    ..held
                }
            ),
            0.0
        );
        let moving = Moment {
            drift: 30.0,
            ..Moment::default()
        };
        assert!((shown_drift(CloudsIn::Apart, moving) - 6.0 * step).abs() < 1e-9);
    }

    #[test]
    fn off_the_globe_the_pointer_is_on_the_sky() {
        let r = planet(Key::R);
        let hits = r.hits(facing(), SIZE, &[], &[]);
        for (x, y) in [(92.0, 362.0), (40.0, 40.0), (1100.0, 500.0)] {
            assert_eq!(hits.find(x, y), Some(Pointed::Sky(None)));
        }
        assert!(matches!(
            hits.find(576.0, 273.0),
            Some(Pointed::Ground(Some(_)) | Pointed::Action(_))
        ));
    }
}

mod home {
    use std::sync::Arc;

    use super::*;
    use crate::world::planet_fx::{Hurricane, Volcano, Weather, wind};
    use crate::world::raster::{Brush, Colour, Path};

    /// The universe's scene, and your planet's radius in it.
    const UNIVERSE: (usize, usize) = (1152, 546);
    const RADIUS: f64 = 46.0;

    fn home<'a>(view: View, weather: &'a [&'a dyn Weather], moment: Moment) -> Home<'a> {
        Home {
            view,
            side: Side::Day,
            centre: (576.0, 273.0),
            radius: RADIUS,
            size: UNIVERSE,
            clouds: CloudsIn::Ground,
            weather,
            moment,
        }
    }

    fn storm_at(x: f64, y: f64) -> Hurricane {
        let mut picture = Canvas::new(80, 80);
        let mut disc = Path::new();
        disc.circle(40.0, 40.0, 32.0);
        picture.fill(&disc, &Brush::new(Colour::rgb(230, 234, 240)));
        let mut arm = Path::new();
        arm.circle(56.0, 40.0, 10.0);
        picture.fill(&arm, &Brush::new(Colour::rgb(40, 44, 60)));
        Hurricane {
            at: (x, y),
            radius: 32.0,
            category: 4,
            south: false,
            picture: Arc::new(picture),
        }
    }

    #[test]
    fn your_planet_in_the_universe_is_the_worlds_own_drawn_over_the_universe() {
        let r = planet(Key::R);
        let mut universe = Canvas::new(UNIVERSE.0 as u32, UNIVERSE.1 as u32);
        let globe = Globe::new(View::stepped(-0.35), (576.0, 273.0), RADIUS);
        r.draw_on(&mut universe, &globe, Side::Day, 0.0, 0.0);
        if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURES_OUT") {
            let mut shown = plain_sky(UNIVERSE);
            shown.draw(&universe, 0.0, 0.0, &Brush::new(Colour::WHITE));
            let _ = std::fs::write(
                std::path::Path::new(&dir).join("planet-home.png"),
                shown.to_png(),
            );
        }
        let middle = universe.pixel(576, 273);
        assert_eq!(middle[3], 255, "solid where the universe was clear");
        assert!(light(middle) > 20.0, "{middle:?}");
        assert_eq!(universe.pixel(576 + 60, 273)[3], 0, "nothing past its air");
    }

    #[test]
    fn your_small_planet_turns_with_its_hurricanes_and_not_its_smoke() {
        let r = planet(Key::R);
        let ground = super::ground(Key::R);
        let busiest = ground.busiest.expect("a busiest behavior");
        let (x, y) = ground.centre[busiest];
        let place = Plate::MAP.place(x, y);
        let storm = storm_at(x, y);
        let smoke = Volcano {
            at: (x, y),
            radius: 14.0,
            smoking: true,
            wind: wind(),
        };
        let weather: [&dyn Weather; 2] = [&storm, &smoke];
        let view = View::stepped(place.lon);
        let mut spin = Spin::new();
        let (sprites, moves) = spin.home(r, &home(view, &weather, Moment::default()));
        assert!(moves, "your planet turns in the universe");
        assert_eq!(sprites.len(), 1, "its ground, the storm drawn in");
        let picture = &sprites[0].picture;
        assert!(picture.image.width() < 120 && picture.image.height() < 120);
        // The storm reads at this size: it changes the picture; the smoke doesn't.
        let (bare, _) = spin.home(r, &home(view, &[], Moment::default()));
        let (smoked, _) = spin.home(r, &home(view, &[&smoke], Moment::default()));
        assert_ne!(sprites[0].picture.image, bare[0].picture.image);
        assert_eq!(smoked[0].picture.image, bare[0].picture.image);
        // A whole turn is 36 small pictures, drawn once each.
        let kept = spin.kept();
        for step in 0..36 {
            let view = View::stepped(f64::from(step) * std::f64::consts::TAU / 36.0);
            spin.home(r, &home(view, &weather, Moment::default()));
        }
        let turned = spin.kept();
        for step in 0..36 {
            let view = View::stepped(f64::from(step) * std::f64::consts::TAU / 36.0);
            spin.home(r, &home(view, &weather, Moment::default()));
        }
        assert_eq!(spin.kept(), turned);
        assert!(turned - kept <= 36);
        // With motion reduced it holds still.
        let still = Moment {
            still: true,
            ..Moment::default()
        };
        assert!(!spin.home(r, &home(view, &weather, still)).1);
    }

    #[test]
    fn your_small_planets_clouds_drift_where_they_lie_apart() {
        let t = planet(Key::T);
        let mut spin = Spin::new();
        let apart = |drift: f64| Home {
            clouds: CloudsIn::Apart,
            ..home(
                View::stepped(0.0),
                &[],
                Moment {
                    drift,
                    ..Moment::default()
                },
            )
        };
        let (now, moves) = spin.home(t, &apart(0.0));
        let (later, _) = spin.home(t, &apart(10.0));
        assert!(moves && now.len() == 2 && later.len() == 2);
        assert_eq!(now[0].picture.key, later[0].picture.key, "the same ground");
        assert_ne!(now[1].picture.key, later[1].picture.key, "the clouds moved");
    }
}

mod unroll {
    use super::*;
    use crate::world::map::relief::{Light, LitGround, Projected, Relief};

    fn flat_map(key: Key) -> Canvas {
        let ground = super::ground(key);
        let relief = Relief::new(ground);
        let polar = Polar::of(ground);
        let light = Light::new(ground, &relief, 0.0);
        let lit = LitGround::new(
            ground,
            &relief,
            &light,
            None,
            Marks::default(),
            Some(&polar),
        );
        Projected::new(&relief, &lit, 0.0, 1).picture
    }

    #[test]
    fn the_planet_unrolls_into_the_map_and_nothing_jumps_at_either_end() {
        let r = planet(Key::R);
        let map = flat_map(Key::R);
        let flat = flat_ground(super::ground(Key::R), None, Marks::default());
        let from = View::stepped(-0.35);
        let start = r
            .unrolled(from, 0.0, Side::Day, &map, &flat, &[])
            .expect("map-sized pictures");
        let end = r
            .unrolled(from, 1.0, Side::Day, &map, &flat, &[])
            .expect("map-sized pictures");
        // At the start the globe stands as the planet shows it.
        let globe = r.globe(from, SIZE);
        let middle = start.pixel(576, 273);
        let planet_middle = r.picture(from, Side::Day, SIZE, None, 0.0).pixel(576, 273);
        for ch in 0..3 {
            assert!(
                (i32::from(middle[ch]) - i32::from(planet_middle[ch])).abs() < 24,
                "{middle:?} against {planet_middle:?}"
            );
        }
        assert!(globe.radius > 200.0);
        let half = r
            .unrolled(from, 0.5, Side::Day, &map, &flat, &[])
            .expect("map-sized pictures");
        if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURES_OUT") {
            for (name, picture) in [
                ("unroll-0", &start),
                ("unroll-5", &half),
                ("unroll-10", &end),
            ] {
                let _ = std::fs::write(
                    std::path::Path::new(&dir).join(format!("{name}.png")),
                    picture.to_png(),
                );
            }
        }
        // At the end each pixel shows the flat map's own colour from a point at most 2 px up
        // and to the left, where its block of 3 × 3 began, away from the dots.
        let ground = super::ground(Key::R);
        let near_a_dot = |x: usize, y: usize| {
            ground
                .dots
                .iter()
                .any(|&(dx, dy)| (f64::from(dx) - x as f64).hypot(f64::from(dy) - y as f64) < 6.0)
        };
        let mut same = 0;
        let mut checked = 0;
        for y in (20..520).step_by(37) {
            for x in (20..1130).step_by(41) {
                if near_a_dot(x, y) {
                    continue;
                }
                checked += 1;
                let shown = end.pixel(x as u32, y as u32);
                let found = (0..3).any(|dy| {
                    (0..3).any(|dx| map.pixel((x - dx) as u32, (y - dy) as u32) == shown)
                });
                if found {
                    same += 1;
                }
            }
        }
        assert_eq!(same, checked);
        // Halfway, it is neither.
        assert_ne!(half.to_rgba(), end.to_rgba());
        let small = Canvas::new(10, 10);
        assert!(
            r.unrolled(from, 0.5, Side::Day, &small, &flat, &[])
                .is_none()
        );
    }
}

mod standing {
    use super::*;
    use crate::world::raster::{Brush, Colour, Path};
    use crate::world::scene::{Depth, Pointed};

    /// A made-up wonder: a pale tower as tall as asked, standing on its foot.
    #[derive(Debug)]
    struct Tower {
        at: (f64, f64),
    }

    impl Standing for Tower {
        fn place(&self) -> (f64, f64) {
            self.at
        }

        fn size(&self) -> f64 {
            ON_MAP
        }

        fn picture(&self, size: f64, _light: [f64; 3]) -> (Canvas, (f64, f64)) {
            let width = (size * 0.5).ceil().max(2.0);
            let height = size.ceil().max(2.0);
            let mut canvas = Canvas::new(width as u32, height as u32);
            let mut body = Path::new();
            body.rect(0.0, 0.0, width, height);
            canvas.fill(&body, &Brush::new(Colour::rgb(236, 226, 200)));
            (canvas, (width / 2.0, height))
        }

        fn pointed(&self) -> Pointed {
            Pointed::Wonder(2)
        }
    }

    fn near_middle() -> (f64, f64) {
        Plate::MAP.pixel(LonLat::new(-0.3, 0.25))
    }

    fn shot<'a>(view: View, standing: &'a [&'a dyn Standing]) -> Shot<'a> {
        Shot {
            view,
            side: Side::Day,
            size: SIZE,
            clouds: CloudsIn::Ground,
            weather: &[],
            standing,
            moment: Moment::default(),
            sky: None,
        }
    }

    fn ground_picture(scene: &crate::world::scene::Scene) -> crate::world::scene::Image {
        scene
            .stack()
            .iter()
            .find(|part| part.depth() == Depth::Ground)
            .map(|part| part.picture().image.clone())
            .expect("a ground")
    }

    #[test]
    fn a_wonder_stands_on_the_planet_and_the_pointer_finds_it_there() {
        let r = planet(Key::R);
        let tower = Tower { at: near_middle() };
        let standing: [&dyn Standing; 1] = [&tower];
        let view = View::stepped(-0.35);
        let hits = r.hits(view, SIZE, &[], &standing);
        let at = r
            .globe(view, SIZE)
            .project(Plate::MAP.place(tower.at.0, tower.at.1));
        // Up its body, above the dots at its foot.
        assert_eq!(
            hits.find(at.x as f32, (at.y - 12.0) as f32),
            Some(Pointed::Wonder(2))
        );
        // Turned away, it is neither found nor drawn.
        let away = View::stepped(-0.35 + std::f64::consts::PI);
        let behind = r.hits(away, SIZE, &[], &standing);
        assert!(
            behind
                .marks()
                .iter()
                .all(|mark| mark.pointed != Pointed::Wonder(2))
        );
        let mut spin = Spin::new();
        let with = spin.scene(r, &shot(view, &standing));
        let without = spin.scene(r, &shot(view, &[]));
        assert_ne!(ground_picture(&with), ground_picture(&without));
        let with_away = spin.scene(r, &shot(away, &standing));
        let without_away = spin.scene(r, &shot(away, &[]));
        assert_eq!(
            ground_picture(&with_away).pixels(),
            ground_picture(&without_away).pixels()
        );
    }
}

/// How long the planet takes to draw, in a release build: `cargo test --release -p
/// argonauts-tui --lib -- --ignored a_turning_picture`.
#[test]
#[ignore = "a speed check, for a release build"]
fn a_turning_picture_draws_well_within_a_step() {
    use std::time::Instant;

    let ground = ground(Key::T);
    let started = Instant::now();
    let texture = Texture::new(ground, None, Marks::default());
    let made = Planet::new(
        1,
        ground,
        texture,
        &facts(Key::T),
        Nature {
            shown: true,
            ..Nature::default()
        },
        &colours(Key::T),
    );
    let planet_made = started.elapsed();
    let mut spin = Spin::new();
    let started = Instant::now();
    for step in 0..36 {
        let shot = Shot {
            view: View::stepped(f64::from(step) * std::f64::consts::TAU / 36.0),
            side: Side::Day,
            size: SIZE,
            clouds: CloudsIn::Apart,
            weather: &[],
            standing: &[],
            moment: Moment::default(),
            sky: None,
        };
        spin.scene(&made, &shot);
    }
    let turn = started.elapsed();
    let each = turn / 36;
    eprintln!("planet made in {planet_made:?}; a turn of 36 steps in {turn:?}, {each:?} a step");
    assert!(each.as_millis() < 150, "{each:?} a step");
}
