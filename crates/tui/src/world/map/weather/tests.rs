use crate::world::deliver::clock::Rate;
use crate::world::map::fields::crews::Key;
use crate::world::map::motion::Moment;
use crate::world::map::nature::crews::{drawn, map_at};
use crate::world::map::relief::cells::compare;

#[test]
fn the_map_moves_and_two_frames_half_a_second_apart_are_different() {
    let weather = &drawn(Key::R).weather;
    assert!(weather.moves());
    let early = weather.under_dots(Moment::at(0.0)).to_rgba();
    let later = weather.under_dots(Moment::at(0.5)).to_rgba();
    assert_ne!(early, later);
    assert_eq!(
        weather.under_dots(Moment::at(0.5)).to_rgba(),
        weather.under_dots(Moment::at(0.51)).to_rgba(),
        "a frame holds until the next is due"
    );
}

#[test]
fn held_still_the_map_keeps_one_pose() {
    let weather = &drawn(Key::R).weather;
    let still = weather.under_dots(Moment::Still).to_rgba();
    assert_eq!(still, weather.under_dots(Moment::Still).to_rgba());
    assert!(
        still.chunks(4).any(|pixel| pixel[3] > 0),
        "a current's chevrons show"
    );
}

#[test]
fn crew_rs_map_held_still() {
    compare(
        "weather-r-still",
        &map_at(Key::R, Moment::Still),
        include_str!("golden/weather-r-still.txt"),
    );
}

#[test]
fn crew_rs_map_a_moment_in() {
    compare(
        "weather-r-moving",
        &map_at(Key::R, Moment::at(0.25)),
        include_str!("golden/weather-r-moving.txt"),
    );
}

#[test]
fn the_weather_lands_where_the_relief_stands() {
    use crate::world::map::relief::STANDING;
    let drawn = drawn(Key::R);
    for (x, y) in [(500.5, 200.25), (831.7, 299.9), (10.0, 540.0)] {
        assert_eq!(
            drawn.weather.screen(x, y, 12.0),
            drawn.relief.screen(x, y, STANDING, 12.0)
        );
    }
}

/// Each flying storm's picture and its top left on the screen at a moment, by its place in
/// nature's list.
fn flying_boxes(key: Key) -> Vec<(usize, crate::world::raster::Canvas, (f64, f64))> {
    use super::{hurricane, thunder};
    use crate::world::map::nature::Kind;
    use crate::world::map::nature::crews::world;
    let world = world(key);
    let weather = &drawn(key).weather;
    world
        .nature
        .features
        .iter()
        .enumerate()
        .filter_map(|(index, feature)| {
            let over = weather.flight(index)?;
            match &feature.kind {
                Kind::Hurricane(storm) => {
                    let cloud =
                        hurricane::sprite(feature.radius, hurricane::seed(feature.behavior, storm));
                    let (x, y) = weather.screen(over.0, over.1, hurricane::ALTITUDE);
                    let corner = (
                        x - f64::from(cloud.width()) / 2.0,
                        y - f64::from(cloud.height()) / 2.0,
                    );
                    Some((index, cloud, corner))
                }
                Kind::Thunderstorm { .. } => {
                    let cloud =
                        thunder::cloud(thunder::size(feature.radius), feature.behavior as f64);
                    let (x, y) = weather.screen(over.0, over.1, thunder::ALTITUDE);
                    let corner = (x - cloud.base.0, y - cloud.base.1);
                    Some((index, cloud.canvas, corner))
                }
                _ => None,
            }
        })
        .collect()
}

#[test]
fn no_storm_cloud_or_hurricane_covers_a_dot() {
    for key in Key::ALL {
        let dots = &drawn(key).dots.at;
        let mut covered = 0;
        for (_, cloud, (ox, oy)) in flying_boxes(key) {
            let rgba = cloud.to_rgba();
            let (w, h) = (cloud.width() as i64, cloud.height() as i64);
            for &(x, y) in dots {
                let u = (f64::from(x) - ox + 0.5).floor() as i64;
                let v = (f64::from(y) - oy + 0.5).floor() as i64;
                if u >= 0 && v >= 0 && u < w && v < h && rgba[((v * w + u) * 4 + 3) as usize] > 110
                {
                    covered += 1;
                }
            }
        }
        assert_eq!(covered, 0, "{key:?}: dots under the clouds");
    }
}

#[test]
fn a_hurricane_flies_in_its_own_hemisphere_off_the_equator() {
    use crate::world::map::fields::HEIGHT;
    use crate::world::map::nature::crews::world;
    use crate::world::map::nature::{EQUATOR_GAP, Kind};
    let mut seen = 0;
    for key in Key::ALL {
        let world = world(key);
        let weather = &drawn(key).weather;
        for (index, feature) in world.nature.features.iter().enumerate() {
            let Kind::Hurricane(storm) = feature.kind else {
                continue;
            };
            seen += 1;
            let (_, y) = weather.flight(index).expect("a hurricane flies");
            let middle = HEIGHT as f64 / 2.0;
            assert!((y - middle).abs() >= EQUATOR_GAP);
            assert_eq!(y > middle, storm.south);
        }
    }
    assert!(seen >= 4);
}

#[test]
fn a_hurricane_turns_three_times_as_fast_at_category_3_as_at_1() {
    use super::hurricane::angle;
    use crate::world::map::nature::Hurricane;
    use time::OffsetDateTime;
    let storm = |category, south| Hurricane {
        actions: 25,
        category,
        south,
        at: OffsetDateTime::UNIX_EPOCH,
    };
    let at = Moment::at(2.5);
    let (one, three) = (angle(&storm(1, false), at), angle(&storm(3, false), at));
    assert!((three - 3.0 * one).abs() < 1e-9);
    assert!(
        (one + 30_f64.to_radians()).abs() < 1e-9,
        "anticlockwise in the north"
    );
    assert!((angle(&storm(1, true), at) - 30_f64.to_radians()).abs() < 1e-9);
    assert_eq!(angle(&storm(5, true), Moment::Still), 0.0);
}

/// Which way a picture turned between two drawings, read from its pixels: where a picture turns
/// by d clockwise on the screen, what changes is -d times its slope round the centre.
fn sense(early: &[u8], later: &[u8], width: usize, (cx, cy): (f64, f64)) -> &'static str {
    let value = |d: &[u8], i: usize, j: usize| {
        let at = (j * width + i) * 4;
        f64::from(d[at]) * f64::from(d[at + 3]) / 255.0
    };
    let mut sum = 0.0;
    let (x0, y0) = ((cx - 90.0).max(1.0) as usize, (cy - 90.0).max(1.0) as usize);
    for j in y0..(cy + 90.0) as usize {
        for i in x0..(cx + 90.0) as usize {
            let gx = (value(early, i + 1, j) - value(early, i - 1, j)) / 2.0;
            let gy = (value(early, i, j + 1) - value(early, i, j - 1)) / 2.0;
            let turn = -(j as f64 - cy) * gx + (i as f64 - cx) * gy;
            sum += (value(later, i, j) - value(early, i, j)) * turn;
        }
    }
    if sum > 0.0 {
        "anticlockwise"
    } else {
        "clockwise"
    }
}

#[test]
fn a_storm_turns_anticlockwise_north_of_the_equator_and_clockwise_south_of_it() {
    use super::{Pad, hurricane};
    use crate::world::map::fields::WIDTH;
    use crate::world::map::nature::Hurricane;
    use time::OffsetDateTime;
    let cloud = hurricane::sprite(48.0, 7.0);
    for south in [false, true] {
        let storm = Hurricane {
            actions: 30,
            category: 1,
            south,
            at: OffsetDateTime::UNIX_EPOCH,
        };
        let centre = (400.0, 260.0);
        let drawn_at = |moment| {
            let mut pad = Pad::whole();
            let angle = hurricane::angle(&storm, moment);
            hurricane::draw(&mut pad, &cloud, centre, angle, south, false);
            pad.into_canvas().to_rgba()
        };
        let turned = sense(
            &drawn_at(Moment::at(0.0)),
            &drawn_at(Moment::at(0.7)),
            WIDTH,
            centre,
        );
        let want = if south { "clockwise" } else { "anticlockwise" };
        assert_eq!(turned, want, "south: {south}");
    }
}

#[test]
fn a_wave_travelling_right_is_the_left_one_mirrored() {
    use super::tsunami::wave_picture;
    let left = wave_picture(46.0, 1.0);
    let right = wave_picture(46.0, -1.0);
    let (w, h) = (left.canvas.width(), left.canvas.height());
    assert_eq!((w, h), (right.canvas.width(), right.canvas.height()));
    assert!((left.anchor.0 - (f64::from(w) - right.anchor.0)).abs() < 1e-9);
    let mut off = 0;
    for y in 0..h {
        for x in 0..w {
            let a = left.canvas.pixel(x, y);
            let b = right.canvas.pixel(w - 1 - x, y);
            if (0..4).any(|ch| a[ch].abs_diff(b[ch]) > 24) {
                off += 1;
            }
        }
    }
    assert!(off * 50 < (w * h) as usize, "{off} pixels are off");
}

#[test]
fn a_still_wave_stands_most_of_the_way_in_with_the_sea_drawn_back() {
    use super::tsunami::Tsunami;
    assert_eq!(Tsunami::progress(Moment::Still), 0.72);
    assert_eq!(Tsunami::progress(Moment::at(1.5)), 0.5);
    assert_eq!(Tsunami::progress(Moment::at(3.0)), 0.0);
}

#[test]
fn a_tornados_bands_climb_as_it_turns_and_rest_when_still() {
    use super::{Pad, tornado};
    use crate::world::map::nature::Kind;
    use crate::world::map::nature::crews::world;
    let world = world(Key::R);
    let weather = &drawn(Key::R).weather;
    let funnel = world
        .nature
        .features
        .iter()
        .find(|feature| matches!(feature.kind, Kind::Tornado { .. }))
        .expect("crew R has a tornado");
    let at = |moment| {
        let mut pad = Pad::whole();
        tornado::draw(&mut pad, weather, funnel, moment);
        pad.into_canvas().to_rgba()
    };
    assert_ne!(at(Moment::at(0.0)), at(Moment::at(0.5)));
    assert_eq!(
        at(Moment::at(0.0)),
        at(Moment::at(1.0)),
        "one turn a second"
    );
    assert_eq!(at(Moment::Still), at(Moment::at(0.0)));
}

#[test]
fn a_thunderstorm_is_an_anvil_cloud_with_a_dark_base_and_a_bolt_when_it_flashes() {
    use super::{Pad, thunder};
    use crate::world::map::nature::Kind;
    use crate::world::map::nature::crews::world;
    let cloud = thunder::cloud(20.0, 3.0);
    let (w, h) = (cloud.canvas.width(), cloud.canvas.height());
    let rgba = cloud.canvas.to_rgba();
    let span = |j: u32| {
        let solid: Vec<u32> = (0..w)
            .filter(|i| rgba[((j * w + i) * 4 + 3) as usize] > 128)
            .collect();
        solid.last().map_or(0, |last| last - solid[0] + 1)
    };
    let light = |from: u32, to: u32| {
        let (mut sum, mut n) = (0.0, 0.0);
        for j in from..to {
            for i in 0..w {
                let at = ((j * w + i) * 4) as usize;
                if rgba[at + 3] > 128 {
                    sum += 0.3 * f64::from(rgba[at])
                        + 0.59 * f64::from(rgba[at + 1])
                        + 0.11 * f64::from(rgba[at + 2]);
                    n += 1.0;
                }
            }
        }
        if n > 0.0 { sum / n } else { 0.0 }
    };
    let top = (0..h).find(|j| span(*j) > 0).expect("a cloud");
    let anvil = (0..6).map(|k| span(top + 2 + k)).max().unwrap_or(0);
    let tower = span((cloud.base.1 - 20.0 * 1.3).round() as u32);
    let flat_top = f64::from(span(top + 1)) / f64::from(anvil);
    let base = light(
        (cloud.base.1 - 20.0 * 0.35).round() as u32,
        cloud.base.1.round() as u32,
    );
    let upper = light(top, (f64::from(top) + 20.0 * 0.4).round() as u32);
    assert!(
        anvil >= tower * 18 / 10,
        "anvil {anvil} over a {tower} tower"
    );
    assert!(flat_top >= 0.6, "flat top {flat_top}");
    assert!(base < 0.6 * upper, "base {base} under {upper}");

    let world = world(Key::R);
    let weather = &drawn(Key::R).weather;
    let (index, storm) = world
        .nature
        .features
        .iter()
        .enumerate()
        .find(|(_, feature)| matches!(feature.kind, Kind::Thunderstorm { .. }))
        .expect("crew R has a thunderstorm");
    let over = weather.flight(index).expect("it flies");
    // The first storm nature lists flashes first in the screen's schedule.
    let flashes =
        crate::world::map::motion::Flashes::for_storms(&super::thunderstorms(&world.nature))[0];
    // The first stored frame at or after its first flash.
    let flashing = (0..64)
        .map(|frame| Moment::at(f64::from(frame) / 16.0 + 0.01))
        .find(|moment| thunder::flash(&flashes, *moment) > 0.5)
        .expect("it flashes");
    assert_eq!(thunder::flash(&flashes, Moment::at(0.01)), 0.0);
    let cloud = thunder::cloud(thunder::size(storm.radius), storm.behavior as f64);
    let mut pad = Pad::whole();
    thunder::draw(
        &mut pad,
        weather,
        (&cloud.canvas, cloud.base),
        over,
        storm.radius,
        flashes,
        storm.behavior,
        flashing,
    );
    let drawn_storm = pad.into_canvas();
    let (gx, gy) = weather.screen(over.0, over.1, 0.0);
    let mut bolt = 0;
    for y in (gy - 14.0) as u32..(gy + 4.0) as u32 {
        for x in (gx - 14.0) as u32..(gx + 14.0) as u32 {
            let [r, g, b, a] = drawn_storm.pixel(x, y);
            if a > 200 && r > 235 && g > 230 && b > 170 {
                bolt += 1;
            }
        }
    }
    assert!(bolt >= 6, "{bolt} bolt pixels");
}

#[test]
fn a_volcano_smokes_while_its_failures_are_fresh_and_an_older_one_is_quiet() {
    use super::{Pad, volcano};
    let drawn = |smoking: bool, moment: Moment| {
        let weather = &drawn(Key::R).weather;
        let cone = volcano::cone(18.0, smoking, 9.0);
        let mut pad = Pad::whole();
        volcano::draw(
            &mut pad,
            weather,
            &cone,
            (592.0, 183.0),
            18.0,
            smoking,
            moment,
        );
        pad.into_canvas().to_rgba()
    };
    assert!(
        drawn(true, Moment::at(0.0)) != drawn(true, Moment::at(0.25)),
        "smoke rises"
    );
    assert!(
        drawn(false, Moment::at(0.0)) == drawn(false, Moment::at(0.25)),
        "a quiet one holds"
    );
    let lit = |rgba: &[u8]| {
        rgba.chunks(4)
            .filter(|p| p[3] > 200 && p[0] > 230 && p[1] > 90 && p[2] < 120)
            .count()
    };
    assert!(
        lit(&drawn(true, Moment::Still)) > 5,
        "a smoking crater glows"
    );
    assert_eq!(
        lit(&drawn(false, Moment::Still)),
        0,
        "a quiet crater is dark"
    );
}

#[test]
fn an_earthquakes_rings_run_out_and_rest_when_still() {
    use super::{Pad, quake};
    let weather = &drawn(Key::S).weather;
    let at = |moment| {
        let mut pad = Pad::whole();
        quake::draw(&mut pad, weather, (815.0, 350.0), 1, moment);
        pad.into_canvas()
    };
    assert!(
        at(Moment::at(0.0)).to_rgba() != at(Moment::at(1.0)).to_rgba(),
        "the ring spreads"
    );
    assert!(
        at(Moment::at(0.0)).to_rgba() == at(Moment::at(2.0)).to_rgba(),
        "every 2 s"
    );
    let still = at(Moment::Still);
    let (x, y) = weather.screen(815.0, 350.0, 0.0);
    let amber = (0..360)
        .filter(|degree| {
            let a = f64::from(*degree).to_radians();
            let r = 6.0 + 0.35 * 34.0;
            let [red, green, blue, alpha] = still.pixel(
                (x + a.cos() * r) as u32,
                (y + a.sin() * r * 0.94 * 0.8) as u32,
            );
            alpha > 60 && red > 200 && green > 140 && blue < 160
        })
        .count();
    assert!(
        amber > 60 && amber < 330,
        "a broken amber ring: {amber} of 360"
    );
}

#[test]
fn crew_ss_map_held_still() {
    compare(
        "weather-s-still",
        &map_at(Key::S, Moment::Still),
        include_str!("golden/weather-s-still.txt"),
    );
}

#[test]
fn a_burning_front_flickers_and_its_smoke_drifts() {
    use super::{Pad, fire};
    use crate::world::map::nature::crews::world;
    use crate::world::map::nature::{Kind, fire_front};
    let world = world(Key::R);
    let weather = &drawn(Key::R).weather;
    let burning = world
        .nature
        .features
        .iter()
        .find(|feature| matches!(feature.kind, Kind::Wildfire { burning: true, .. }))
        .expect("crew R has a fire burning");
    let front = fire_front(&world.ground, burning);
    let at = |moment| {
        let mut pad = Pad::whole();
        fire::draw(&mut pad, weather, &front, burning.behavior, moment);
        pad.into_canvas().to_rgba()
    };
    assert!(
        at(Moment::at(0.0)) != at(Moment::at(0.25)),
        "the flames flicker"
    );
    assert!(at(Moment::Still) == at(Moment::Still));
    let flames = at(Moment::Still)
        .chunks(4)
        .filter(|p| p[3] > 180 && p[0] > 240 && p[1] > 120)
        .count();
    assert!(flames > 20, "{flames} flame pixels");
}

#[test]
fn clouds_cast_shadows_on_as_much_of_the_map_as_actions_no_label_fits() {
    use crate::world::map::nature::crews::world;
    let t = world(Key::T);
    let share = 90.0 / (775.0 + 90.0);
    assert!((t.nature.clouds.expect("crew T has clouds") - share).abs() < 1e-9);
    let shadows = drawn(Key::T)
        .weather
        .cloud_shadows()
        .expect("they cast shadows");
    assert!(
        (shadows.cover() - share).abs() < 0.02,
        "{}",
        shadows.cover()
    );
    assert!(
        world(Key::R).nature.clouds.is_none(),
        "under a tenth, no clouds"
    );
    assert!(drawn(Key::R).weather.cloud_shadows().is_none());
}

#[test]
fn cloud_shadows_fall_full_on_land_and_at_a_quarter_over_the_sea_soft_edged() {
    use super::Pad;
    use crate::world::map::fields::{HEIGHT, WIDTH};
    let drawn = drawn(Key::T);
    let shadows = drawn.weather.cloud_shadows().expect("crew T casts shadows");
    let mut pad = Pad::whole();
    shadows.draw(&mut pad, Moment::at(0.0));
    let layer = pad.into_canvas();
    let (mut land, mut sea) = (0, 0);
    for y in 80..HEIGHT - 80 {
        for x in 0..WIDTH {
            let shown = drawn.land.rows[y * WIDTH + x];
            if shown < 0.0 {
                continue;
            }
            let row = ((f64::from(shown) - 0.5).round() as usize).min(HEIGHT - 1);
            let inside = drawn.relief.coast.get(x, row);
            let alpha = layer.pixel(x as u32, y as u32)[3];
            if inside > 6.0 {
                land = land.max(alpha);
            } else if inside < -6.0 {
                sea = sea.max(alpha);
            }
        }
    }
    assert!(land > 70, "strongest {land} on land");
    assert!(
        sea > 0 && f64::from(sea) <= f64::from(land) * 0.3,
        "{sea} at sea"
    );
    let mut still = Pad::whole();
    shadows.draw(&mut still, Moment::Still);
    assert!(
        still.into_canvas().to_rgba().chunks(4).all(|p| p[3] == 0),
        "none held still"
    );
}

#[test]
fn the_drifting_cloud_shadows_keep_off_the_polar_ice() {
    use super::Pad;
    use crate::world::map::fields::{HEIGHT, WIDTH};
    use crate::world::map::land::Polar;
    use crate::world::map::nature::crews::world;
    use crate::world::map::relief::{FORESHORTEN, top_row};
    let polar = Polar::of(&world(Key::T).ground);
    assert!(
        polar.cap_north.is_some(),
        "crew T, quiet 9 days, has a north cap"
    );
    let scale = WIDTH as f64 / std::f64::consts::TAU;
    let ice_ends = (top_row()
        + FORESHORTEN * (HEIGHT as f64 / 2.0 - (polar.edge_north() - 0.1) * scale))
        as u32;
    assert!(ice_ends > 20);
    let shadows = drawn(Key::T).weather.cloud_shadows().expect("shadows");
    for t in [0.9, 3.9, 11.0] {
        let mut pad = Pad::whole();
        shadows.draw(&mut pad, Moment::at(t));
        let layer = pad.into_canvas();
        for y in 0..ice_ends {
            for x in 0..WIDTH as u32 {
                assert_eq!(layer.pixel(x, y)[3], 0, "a shade on the ice at {x}, {y}");
            }
        }
    }
}

#[test]
fn crew_ts_map_with_its_clouds_drifting() {
    compare(
        "weather-t-moving",
        &map_at(Key::T, Moment::at(6.0)),
        include_str!("golden/weather-t-moving.txt"),
    );
}

/// Crew `key`'s map laid flat, seen from straight above, with nature's flat layers on it.
fn flat_with_layers(key: Key, keep: super::Keep) -> crate::world::raster::Canvas {
    use super::FlatLayers;
    use crate::world::map::land::{Marks, Polar};
    use crate::world::map::nature::crews::world;
    use crate::world::map::relief::{Light, LitGround, Projected, Relief};
    use crate::world::raster::{Brush, Colour};
    let world = world(key);
    let ground = &world.ground;
    let layers = FlatLayers::new(ground, &world.nature, keep, None);
    let laid = layers.ground.to_rgba();
    let relief = Relief::new(ground);
    let polar = Polar::of(ground);
    let light = Light::new(ground, &relief, 0.0);
    let marks = Marks {
        ground: Some(&laid),
        icebergs: true,
        ..Marks::default()
    };
    let lit = LitGround::new(ground, &relief, &light, None, marks, Some(&polar));
    let mut picture = Projected::new(&relief, &lit, 0.0, 1).picture;
    picture.draw(&layers.sky, 0.0, 0.0, &Brush::new(Colour::WHITE));
    picture
}

#[test]
fn crew_rs_nature_from_straight_above() {
    compare(
        "flat-r",
        &flat_with_layers(Key::R, super::Keep::All),
        include_str!("golden/flat-r.txt"),
    );
}

#[test]
fn the_planets_copy_leaves_out_what_it_draws_moving() {
    use super::{FlatLayers, Keep};
    use crate::world::map::nature::crews::world;
    let world = world(Key::R);
    let all = FlatLayers::new(&world.ground, &world.nature, Keep::All, None);
    let planet = FlatLayers::new(&world.ground, &world.nature, Keep::Planet, None);
    let alpha = |canvas: &crate::world::raster::Canvas| {
        canvas
            .to_rgba()
            .chunks(4)
            .map(|p| u64::from(p[3]))
            .sum::<u64>()
    };
    assert!(alpha(&all.sky) > 0, "the map's storms and smoke");
    assert_eq!(alpha(&planet.sky), 0, "the planet draws its weather moving");
    assert!(
        alpha(&planet.glow) > 0,
        "lava and embers still glow at night"
    );
    assert!(alpha(&planet.ground) > 0);
}

#[test]
fn storm_clouds_fly_clear_of_the_wonders() {
    for key in Key::ALL {
        let drawn = drawn(key);
        for (_, cloud, (x0, y0)) in flying_boxes(key) {
            let (x1, y1) = (
                x0 + f64::from(cloud.width()),
                y0 + f64::from(cloud.height()),
            );
            for wonder in &drawn.wonders {
                let b = drawn.weather.wonder_box(wonder);
                let overlaps = x0 < b.x1 && x1 > b.x0 && y0 < b.y1 && y1 > b.y0;
                assert!(!overlaps, "{key:?}: a cloud over {:?}", wonder.wonder);
            }
        }
    }
}

#[test]
fn crew_us_map_with_its_drowned_city_held_still() {
    compare(
        "weather-u-still",
        &map_at(Key::U, Moment::Still),
        include_str!("golden/weather-u-still.txt"),
    );
}

/// The pointer's finds on crew `key`'s map at a moment, the dots included as the screen adds
/// them.
fn hits(key: Key, moment: Moment) -> crate::base::draw::scene::HitMap {
    let drawn = drawn(key);
    let mut hits = crate::base::draw::scene::HitMap::new();
    for mark in drawn.dots.marks(0, 3) {
        hits.add(mark);
    }
    for mark in drawn.weather.marks(moment) {
        hits.add(mark);
    }
    hits
}

#[test]
fn pointing_at_what_stands_or_flies_names_it() {
    use crate::base::draw::scene::Pointed;
    use crate::world::map::nature::Kind;
    use crate::world::map::nature::crews::world;
    let world = world(Key::R);
    let weather = &drawn(Key::R).weather;
    let hits = hits(Key::R, Moment::Still);
    let found = |x: f64, y: f64| hits.find(x as f32, y as f32);
    let line = |pointed: Option<Pointed>| match pointed {
        Some(Pointed::Feature(index)) => world
            .nature
            .line(index as usize, &world.ground, &world.facts, &mut |cli| {
                cli.to_owned()
            })
            .unwrap_or_default(),
        other => format!("{other:?}"),
    };
    let (index, volcano) = world
        .nature
        .features
        .iter()
        .enumerate()
        .find(|(_, f)| matches!(f.kind, Kind::Volcano { .. }))
        .expect("a volcano");
    let (x, y) = weather.screen(volcano.x, volcano.y, 0.0);
    let pointed = found(x, y - volcano.radius * 0.5);
    assert_eq!(pointed, Some(Pointed::Feature(index as u32)));
    assert!(
        line(pointed).starts_with(
            "Volcano · 27 of its 27 ‘ran the test suite, which failed’ actions failed"
        )
    );

    let (index, _) = world
        .nature
        .features
        .iter()
        .enumerate()
        .find(|(_, f)| matches!(f.kind, Kind::Hurricane(_)))
        .expect("a hurricane");
    let (fx, fy) = weather.flight(index).expect("it flies");
    let (x, y) = weather.screen(fx, fy, super::hurricane::ALTITUDE);
    assert!(line(found(x, y)).starts_with("Hurricane · "));

    let (index, wave) = world
        .nature
        .features
        .iter()
        .enumerate()
        .find_map(|(i, f)| match f.kind {
            Kind::Tsunami(wave) => Some((i, wave)),
            _ => None,
        })
        .expect("a tsunami");
    let (_, (cx, cy)) = super::tsunami::wave_bounds(&wave, weather, Moment::Still);
    assert_eq!(found(cx, cy), Some(Pointed::Feature(index as u32)));

    let wonder = &drawn(Key::R).wonders[0];
    let b = weather.wonder_box(wonder);
    let pointed = found((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
    assert_eq!(pointed, Some(Pointed::Wonder(0)));
}

#[test]
fn pointing_at_what_lies_on_the_ground_names_it_and_a_dot_under_the_pointer_wins() {
    use crate::base::draw::scene::Pointed;
    use crate::world::map::nature::Kind;
    use crate::world::map::nature::crews::world;
    let world = world(Key::R);
    let weather = &drawn(Key::R).weather;
    let hits = hits(Key::R, Moment::Still);
    let found = |x: f64, y: f64| hits.find(x as f32, y as f32);
    let lake = world
        .nature
        .features
        .iter()
        .position(|f| matches!(f.kind, Kind::Lake { .. }))
        .expect("a lake");
    let feature = &world.nature.features[lake];
    let (x, y) = weather.screen(feature.x, feature.y, 0.0);
    assert_eq!(found(x, y), Some(Pointed::Feature(lake as u32)));

    let shoal = &world.nature.shoals[0];
    let (x, y) = weather.screen(shoal.x, shoal.y, 0.0);
    let Some(Pointed::Feature(index)) = found(x + 6.0, y) else {
        panic!("a shoal answers");
    };
    let line = world
        .nature
        .line(index as usize, &world.ground, &world.facts, &mut |cli| {
            cli.to_owned()
        });
    assert_eq!(
        line.as_deref(),
        Some("Shoal · ‘failed to read a file’ is a label no action has used")
    );

    let dot = drawn(Key::R).dots.at[0];
    assert_eq!(
        found(f64::from(dot.0), f64::from(dot.1)),
        Some(Pointed::Action(0))
    );
}

#[test]
fn every_kind_and_wonder_has_a_picture_for_its_line() {
    use super::icon::{HEIGHT, WIDTH, icon};
    use crate::world::map::relief::cells::{compare_means, mean_colours};
    use crate::world::raster::{Brush, Canvas, Colour};
    use argonauts_core::behaviors::behavior_world::Wonder;
    let words = [
        "rainforest",
        "forest",
        "grassland",
        "savanna",
        "desert",
        "rock",
        "river",
        "current",
        "lake",
        "hurricane",
        "tsunami",
        "tornado",
        "storm",
        "volcano",
        "fault",
        "quake",
        "swamp",
        "plateau",
        "basin",
        "oasis",
        "flood",
        "fire",
        "atoll",
        "cap",
        "iceberg",
        "aurora",
        "clouds",
        "sea",
        "shoal",
    ];
    let mut pictures: Vec<Canvas> = words
        .iter()
        .map(|word| icon(word, false).unwrap_or_else(|| panic!("{word} has a picture")))
        .collect();
    pictures.push(icon("hurricane", true).expect("a southern storm's picture"));
    pictures.extend(
        Wonder::ALL
            .iter()
            .map(|wonder| crate::world::wonders::icon(*wonder)),
    );
    assert!(icon("a kind no one drew", false).is_none());
    for key in Key::ALL {
        let nature = &crate::world::map::nature::crews::world(key).nature;
        for feature in &nature.features {
            let word = feature.kind.word();
            assert!(
                icon(word, false).is_some(),
                "{key:?}'s {word} has a picture"
            );
        }
    }
    let across = 9;
    let down = pictures.len().div_ceil(across) as u32;
    let mut sheet = Canvas::new(WIDTH * across as u32, HEIGHT * down);
    for (i, picture) in pictures.iter().enumerate() {
        let (x, y) = ((i % across) as u32 * WIDTH, (i / across) as u32 * HEIGHT);
        sheet.draw(
            picture,
            f64::from(x),
            f64::from(y),
            &Brush::new(Colour::WHITE),
        );
    }
    let size = (sheet.width() as usize, sheet.height() as usize);
    let means = mean_colours(size, (18, 42), |x, y| {
        let [r, g, b, _] = sheet.pixel(x as u32, y as u32);
        [r, g, b]
    });
    compare_means("icons", &means, include_str!("golden/icons.txt"), || {
        sheet.to_png()
    });
}

#[test]
fn every_part_fits_in_its_own_box() {
    use crate::base::draw::scene::PartId;
    for key in Key::ALL {
        let weather = &drawn(key).weather;
        for moment in [
            Moment::Still,
            Moment::at(0.3),
            Moment::at(1.7),
            Moment::at(9.2),
        ] {
            for part in weather.parts(moment) {
                if part.id == PartId::new("cloud shadows", 0) {
                    continue;
                }
                let canvas = part.pad.canvas();
                let (w, h) = (canvas.width(), canvas.height());
                let edge = (0..w)
                    .flat_map(|x| [(x, 0), (x, h - 1)])
                    .chain((0..h).flat_map(|y| [(0, y), (w - 1, y)]))
                    .map(|(x, y)| canvas.pixel(x, y)[3])
                    .max()
                    .unwrap_or(0);
                assert!(
                    edge <= 2,
                    "{key:?} {:?} at {moment:?} runs past its box: {edge}",
                    part.id
                );
            }
        }
    }
}

#[test]
fn a_part_keeps_its_name_and_changes_its_picture_only_when_its_frame_does() {
    let weather = &drawn(Key::R).weather;
    let ids = |moment| -> Vec<_> { weather.sprites(moment, 7).iter().map(|s| s.id).collect() };
    assert_eq!(ids(Moment::at(0.0)), ids(Moment::at(2.3)));
    let keys = |moment| -> Vec<u64> {
        weather
            .sprites(moment, 7)
            .iter()
            .map(|s| s.picture.key)
            .collect()
    };
    assert_eq!(
        keys(Moment::at(0.5)),
        keys(Moment::at(0.51)),
        "nothing steps within a frame"
    );
    let (early, later) = (keys(Moment::at(0.0)), keys(Moment::at(0.125)));
    let changed = early.iter().zip(&later).filter(|(a, b)| a != b).count();
    assert!(
        changed > 0 && changed < early.len(),
        "{changed} of {} parts stepped",
        early.len()
    );
    let (now, later) = (
        weather.sprites(Moment::at(0.0), 7),
        weather.sprites(Moment::at(3.0), 7),
    );
    let unchanged = now
        .iter()
        .zip(&later)
        .filter(|(a, b)| a.picture.key == b.picture.key)
        .count();
    assert!(
        unchanged >= 6,
        "the wonders never change: {unchanged} parts held"
    );
    assert_ne!(keys(Moment::at(0.0)), keys(Moment::Still));
}

#[test]
fn crew_rs_map_laid_flat() {
    use crate::world::map::nature::crews::flat_map;
    compare(
        "weather-r-flat",
        &flat_map(Key::R),
        include_str!("golden/weather-r-flat.txt"),
    );
}

/// Once every loop on the map has come round, a step of the weather draws nothing new: each
/// frame is found by its key among those drawn before, the same picture as drawing it again,
/// and what the loops keep stays within its room.
#[test]
fn a_loop_that_comes_round_again_is_not_drawn_again() {
    use crate::world::map::nature::crews::world;
    use crate::world::map::weather::Weather;
    use crate::world::map::weather::parts::{LOOP_FRAME_AT_MOST, LOOPS_KEPT};

    let (world, shown) = (world(Key::R), drawn(Key::R));
    let fresh = || {
        Weather::new(
            &world.ground,
            &shown.relief,
            &world.nature,
            &shown.land,
            &shown.wonders,
        )
    };
    let weather = fresh();
    let kept = || weather.frames.lock().unwrap().bytes();
    // 15 s at the quicker rate, the longest loop but the clouds', whose frames are each the
    // whole map.
    let at = |tick| Moment::of_tick(tick, Rate::QUICK);
    for tick in 0..240 {
        weather.sprites(at(tick), 7);
    }
    // With room left for any frame, a frame drawn the second time round would be kept too.
    let first = kept();
    assert!(
        first > 0 && first + LOOP_FRAME_AT_MOST <= LOOPS_KEPT,
        "{first} bytes kept"
    );
    for tick in 240..480 {
        weather.sprites(at(tick), 7);
    }
    assert_eq!(kept(), first, "a second time round draws nothing new");
    let again = fresh().sprites(at(400), 7);
    let found = weather.sprites(at(400), 7);
    assert_eq!(found.len(), again.len());
    for (found, again) in found.iter().zip(&again) {
        assert_eq!((found.id, found.at), (again.id, again.at));
        assert_eq!(found.picture.key, again.picture.key);
        assert_eq!(found.picture.image.pixels(), again.picture.image.pixels());
    }
}

/// Every river and current flows on the one loop they share, so they go to the screen as one
/// moving thing: whatever the map holds of them, a step of its water is a single picture, the
/// same one again a loop later.
#[test]
fn the_rivers_and_currents_move_as_one_thing() {
    use crate::world::PartId;
    use crate::world::map::motion::RIVER;

    let weather = &drawn(Key::R).weather;
    let water = PartId::new("water", 0);
    let named = |t: f64| -> Vec<(PartId, u64)> {
        weather
            .parts(Moment::at(t))
            .iter()
            .map(|part| (part.id, part.frame))
            .collect()
    };
    let now = named(1.0);
    assert_eq!(now.iter().filter(|(id, _)| *id == water).count(), 1);
    let apart = |name: &str| (0..40).any(|n| now.iter().any(|(id, _)| *id == PartId::new(name, n)));
    assert!(
        !apart("river") && !apart("current"),
        "no river or current on its own"
    );
    let frame =
        |parts: &[(PartId, u64)]| parts.iter().find(|(id, _)| *id == water).map(|(_, f)| *f);
    assert_eq!(frame(&now), frame(&named(1.0 + RIVER.seconds)));
    assert_ne!(frame(&now), frame(&named(1.0 + RIVER.seconds / 2.0)));
}
