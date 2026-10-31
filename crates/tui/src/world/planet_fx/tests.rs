use std::sync::Arc;

use super::*;
use crate::base::draw::geo::{Globe, Plate, View};
use crate::world::map::motion::Flashes;
use crate::world::planet::Side;
use crate::world::raster::{Brush, Colour, Path};

const SIZE: (u32, u32) = (1152, 546);

/// A storm's picture for the tests: a pale disc with one dark arm off its middle, so a turn
/// shows.
fn storm_picture(radius: f64) -> Arc<Canvas> {
    let across = (radius * 2.5).ceil();
    let mut canvas = Canvas::new(across as u32, across as u32);
    let half = across / 2.0;
    let mut disc = Path::new();
    disc.circle(half, half, radius);
    canvas.fill(&disc, &Brush::new(Colour::rgb(230, 234, 240)));
    let mut arm = Path::new();
    arm.circle(half + radius * 0.5, half, radius * 0.3);
    canvas.fill(&arm, &Brush::new(Colour::rgb(40, 44, 60)));
    Arc::new(canvas)
}

/// A globe facing the map's pixel (x, y), at `radius`, in the middle of the scene.
fn facing(x: f64, y: f64, radius: f64, side: Side) -> GlobeGround {
    let place = Plate::MAP.place(x, y);
    let view = View {
        lon: place.lon,
        tilt: 0.2,
    };
    let globe = Globe::new(view, (576.0, 273.0), radius);
    GlobeGround::new(globe, side)
}

fn drawn(thing: &dyn Weather, ground: &GlobeGround, t: f64, still: bool) -> Canvas {
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    thing.draw(&mut canvas, ground, t, still);
    canvas
}

fn inked(canvas: &Canvas) -> usize {
    canvas
        .to_rgba()
        .chunks(4)
        .filter(|pixel| pixel[3] > 0)
        .count()
}

fn hurricane(south: bool) -> Hurricane {
    Hurricane {
        at: (700.0, if south { 380.0 } else { 160.0 }),
        radius: 30.0,
        category: 3,
        south,
        picture: storm_picture(30.0),
    }
}

fn volcano() -> Volcano {
    Volcano {
        at: (500.0, 250.0),
        radius: 14.0,
        smoking: true,
        wind: wind(),
    }
}

#[test]
fn the_planets_weather_moves_and_holds_still_with_motion_reduced() {
    let storm = hurricane(false);
    let ground = facing(700.0, 160.0, 240.0, Side::Day);
    let (a, b) = (
        drawn(&storm, &ground, 0.0, false),
        drawn(&storm, &ground, 2.5, false),
    );
    assert_ne!(a.to_rgba(), b.to_rgba(), "it turns");
    let (c, d) = (
        drawn(&storm, &ground, 0.0, true),
        drawn(&storm, &ground, 2.5, true),
    );
    assert_eq!(c.to_rgba(), d.to_rgba(), "held still");
}

#[test]
fn a_hurricane_draws_on_the_side_facing_you_and_nothing_behind() {
    let storm = hurricane(false);
    let front = inked(&drawn(
        &storm,
        &facing(700.0, 160.0, 240.0, Side::Day),
        1.0,
        false,
    ));
    let behind = inked(&drawn(
        &storm,
        &facing(124.0, 160.0, 240.0, Side::Day),
        1.0,
        false,
    ));
    assert!(
        front > 500 && behind == 0,
        "{front} facing, {behind} behind"
    );
}

#[test]
fn the_small_planet_draws_only_what_reads_at_its_size() {
    let storm = hurricane(false);
    let smoke = volcano();
    let small_storm = inked(&drawn(
        &storm,
        &facing(700.0, 160.0, 46.0, Side::Day),
        1.0,
        false,
    ));
    let small_smoke = inked(&drawn(
        &smoke,
        &facing(500.0, 250.0, 46.0, Side::Day),
        1.0,
        false,
    ));
    let big_smoke = inked(&drawn(
        &smoke,
        &facing(500.0, 250.0, 240.0, Side::Day),
        1.0,
        false,
    ));
    assert!(small_storm > 20, "{small_storm} px of storm");
    assert_eq!(small_smoke, 0);
    assert!(big_smoke > 50, "{big_smoke} px of smoke");
    assert!(storm.moves_on(46.0) && !smoke.moves_on(46.0) && smoke.moves_on(240.0));
}

#[test]
fn a_storm_turns_anticlockwise_north_of_the_equator_and_clockwise_south() {
    // On a screen whose y runs down, a growing angle turns clockwise.
    assert!(hurricane(false).angle(1.0) < 0.0);
    assert!(hurricane(true).angle(1.0) > 0.0);
    // Half a turn looks the same as a whole, so the angle counts round 180°.
    let storm = hurricane(false);
    let half_turn = 180.0 / (12.0 * 3.0);
    assert!(storm.angle(half_turn + 0.5).abs() < std::f64::consts::PI);
    // The south's arms are mirrored: drawn unturned, the two pictures are each other's flip.
    let north = drawn(
        &hurricane(false),
        &facing(700.0, 270.0, 240.0, Side::Day),
        0.0,
        true,
    );
    let south_storm = Hurricane {
        at: (700.0, 270.0),
        ..hurricane(true)
    };
    let south = drawn(
        &south_storm,
        &facing(700.0, 270.0, 240.0, Side::Day),
        0.0,
        true,
    );
    assert_ne!(north.to_rgba(), south.to_rgba());
}

#[test]
fn a_thunderstorm_flashes_once_for_each_stopped_session_up_to_four() {
    let storm = Thunderstorm {
        at: (600.0, 240.0),
        radius: 16.0,
        seed: 3,
        stopped: 6,
        flashes: Flashes::for_storms(&[6])[0],
        wind: wind(),
        picture: storm_picture(16.0 * 1.76),
    };
    let flashes = (0..32)
        .map(|k| storm.flash(Motion::LIGHTNING.moment(k), false).1)
        .filter(|which| *which > 0)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(flashes.len(), 4);
    let lit = (0..32)
        .filter(|k| storm.flash(Motion::LIGHTNING.moment(*k), false).0 > 0.0)
        .count();
    // A quarter second each, 8 pictures a second: two pictures a flash.
    assert_eq!(lit, 8);
    assert_eq!(storm.flash(0.1, true), (0.75, 0));
    let ground = facing(600.0, 240.0, 240.0, Side::Night);
    let flash_at = (0.3 / 4.0) * 4.0 + 0.05;
    let light = |canvas: &Canvas| -> u64 {
        canvas
            .to_rgba()
            .chunks(4)
            .map(|pixel| u64::from(pixel[0]) + u64::from(pixel[2]))
            .sum()
    };
    let dark = drawn(&storm, &ground, 0.0, false);
    let flashing = drawn(&storm, &ground, flash_at, false);
    assert!(light(&flashing) > light(&dark) + 1000);
}

#[test]
fn light_runs_down_a_river_only_by_day_and_never_held_still() {
    let path: Vec<(f64, f64)> = (0..40)
        .map(|i| (520.0 + f64::from(i) * 3.0, 220.0 + f64::from(i)))
        .collect();
    let river = Flow {
        river: true,
        path,
        width: 4.0,
        at_sea: None,
    };
    let day = facing(580.0, 240.0, 240.0, Side::Day);
    let night = facing(580.0, 240.0, 240.0, Side::Night);
    assert!(inked(&drawn(&river, &day, 0.4, false)) > 0);
    assert_eq!(inked(&drawn(&river, &day, 0.4, true)), 0);
    let at_night = drawn(&river, &night, 0.4, false);
    let bright = at_night
        .to_rgba()
        .chunks(4)
        .filter(|pixel| pixel[3] > 40)
        .count();
    assert_eq!(bright, 0);
}

#[test]
fn a_current_held_still_shows_its_way_in_chevrons() {
    let points: Vec<(f64, f64)> = (0..60)
        .map(|i| (420.0 + f64::from(i) * 4.0, 300.0))
        .collect();
    let current = Current {
        stretches: vec![Stretch::along(points, 14.0)],
        seed: 2,
        to: 5,
    };
    let ground = facing(540.0, 300.0, 240.0, Side::Day);
    assert!(inked(&drawn(&current, &ground, 0.0, true)) > 0);
    let (a, b) = (
        drawn(&current, &ground, 0.0, false),
        drawn(&current, &ground, 0.5, false),
    );
    assert_ne!(a.to_rgba(), b.to_rgba());
}

#[test]
fn quake_rings_run_out_and_hold_at_their_spacing_when_still() {
    let quake = Quake {
        at: (600.0, 260.0),
        crashes: 2,
    };
    let ground = facing(600.0, 260.0, 240.0, Side::Day);
    let (a, b) = (
        drawn(&quake, &ground, 0.0, false),
        drawn(&quake, &ground, 0.75, false),
    );
    assert_ne!(a.to_rgba(), b.to_rgba());
    assert_eq!(
        drawn(&quake, &ground, 0.0, true).to_rgba(),
        drawn(&quake, &ground, 1.0, true).to_rgba()
    );
    assert!(!quake.moves_on(46.0));
}

#[test]
fn a_tornado_and_a_wildfire_move_and_a_quiet_volcano_does_not() {
    let tornado = Tornado {
        at: (610.0, 250.0),
        radius: 18.0,
        seed: 4,
    };
    let ground = facing(610.0, 250.0, 240.0, Side::Day);
    assert_ne!(
        drawn(&tornado, &ground, 0.0, false).to_rgba(),
        drawn(&tornado, &ground, 0.5, false).to_rgba()
    );
    let front: Vec<(f64, f64)> = (0..12)
        .map(|i| (590.0 + f64::from(i) * 4.0, 262.0))
        .collect();
    let fire = Wildfire {
        front,
        burning: true,
        wind: wind(),
    };
    assert_ne!(
        drawn(&fire, &ground, 0.0, false).to_rgba(),
        drawn(&fire, &ground, 0.4, false).to_rgba()
    );
    let quiet = Volcano {
        smoking: false,
        ..volcano()
    };
    assert_eq!(inked(&drawn(&quiet, &ground, 1.0, false)), 0);
    assert!(!quiet.moves_on(240.0));
}

#[test]
fn the_clouds_drift_east_at_the_maps_pace_stepped_twice_a_second() {
    assert!((DRIFT_DEGREES_A_SECOND - 2.083).abs() < 0.001);
    assert_eq!(drift(0.4), 0.0);
    assert_eq!(drift(0.6), drift(0.9));
    assert!((drift(10.0) - (10.0 * DRIFT_DEGREES_A_SECOND).to_radians()).abs() < 1e-12);
}

#[test]
fn weather_is_drawn_water_first_and_hurricanes_last() {
    let mut kinds = [Kind::Hurricane, Kind::Flow, Kind::Volcano, Kind::Tsunami];
    kinds.sort();
    assert_eq!(
        kinds,
        [Kind::Flow, Kind::Tsunami, Kind::Volcano, Kind::Hurricane]
    );
    assert_eq!(Motion::HURRICANE.picture_at(15.0 / 24.0 * 5.0 + 0.01), 5);
    assert_eq!(Motion::LIGHTNING.moment(33), Motion::LIGHTNING.moment(1));
}
