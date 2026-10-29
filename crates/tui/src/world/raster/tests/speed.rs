//! How long a whole map takes to draw, with as much on it as a busy month puts there: a
//! thousand dots, forty-odd places with their land and water, storms and the shade they cast,
//! and the fields the ground grows from. Run it in a release build:
//! `cargo test --release -p argonauts-tui --lib world::raster::tests::speed -- --ignored --nocapture`

use std::f64::consts::TAU;
use std::time::{Duration, Instant};

use super::noise::{Rng, fbm3, mix, smooth, vnoise3};
use super::*;

const WIDTH: u32 = 1152;
const HEIGHT: u32 = 546;

#[test]
#[ignore = "a speed check, meaningful only in a release build"]
fn a_busy_map_draws_in_well_under_a_third_of_a_second() {
    let started = Instant::now();
    let mut parts: Vec<(&str, Duration)> = Vec::new();
    let mut lap = {
        let mut last = Instant::now();
        move |name: &'static str, parts: &mut Vec<(&str, Duration)>| {
            parts.push((name, last.elapsed()));
            last = Instant::now();
        }
    };

    let fields = coarse_fields();
    let relief = Grid::from_fn(577, 274, |x, y| {
        fbm3(x as f64 * 0.02, y as f64 * 0.02, 0.5, 9.0, 3)
    });
    lap("fields and relief", &mut parts);

    let mut map = ground(&fields, &relief);
    lap("ground, pixel by pixel", &mut parts);

    let mut rng = Rng::new(7);
    let places: Vec<(f64, f64)> = (0..44)
        .map(|_| {
            (
                rng.next_f64() * 1000.0 + 76.0,
                rng.next_f64() * 420.0 + 63.0,
            )
        })
        .collect();
    land_and_water(&mut map, &places, &mut rng);
    lap("land, lakes and rivers", &mut parts);

    weather(&mut map, &mut rng);
    lap("storms, shade and clouds", &mut parts);

    dots(&mut map, &mut rng);
    lap("a thousand dots", &mut parts);

    let png = map.to_png();
    lap("the PNG", &mut parts);

    let total = started.elapsed();
    for (name, took) in &parts {
        eprintln!("{name:>28}: {:6.1} ms", took.as_secs_f64() * 1000.0);
    }
    eprintln!("{:>28}: {:6.1} ms", "all", total.as_secs_f64() * 1000.0);
    assert!(png.len() > 1000);
    assert!(total < Duration::from_millis(300), "{total:?}");
}

/// Four fields on the coarse grid, each fractal noise blurred, as the ground's climate is.
fn coarse_fields() -> Vec<Grid> {
    (0..4)
        .map(|seed| {
            Grid::from_fn(289, 138, |x, y| {
                fbm3(x as f64 * 0.05, y as f64 * 0.05, 0.1, f64::from(seed), 4)
            })
            .blurred(3)
        })
        .collect()
}

/// The ground's colour at every pixel, from the fields and a touch of fine noise.
fn ground(fields: &[Grid], relief: &Grid) -> Canvas {
    let sand = [210.0, 190.0, 140.0];
    let forest = [60.0, 110.0, 70.0];
    let sea = [30.0, 60.0, 110.0];
    let mut rgba = vec![0_u8; (WIDTH * HEIGHT * 4) as usize];
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((index as u32 % WIDTH) as f64, (index as u32 / WIDTH) as f64);
        let (gx, gy) = (x / 4.0, y / 4.0);
        let land = smooth(-0.1, 0.1, fields[0].sample(gx, gy));
        let wet = smooth(-0.3, 0.3, fields[1].sample(gx, gy));
        let height = relief.sample(x / 2.0, y / 2.0);
        let grain = vnoise3(x * 0.3, y * 0.3, 0.0, 3.0) * 6.0;
        let shade = 1.0 + height * 0.3 + fields[2].sample(gx, gy) * 0.1;
        for channel in 0..3 {
            let ground = mix(sand[channel], forest[channel], wet);
            let colour = mix(sea[channel], ground * shade, land) + grain;
            pixel[channel] = colour.clamp(0.0, 255.0) as u8;
        }
        pixel[3] = 255;
    }
    Canvas::from_rgba(WIDTH, HEIGHT, &rgba).expect("the map's pixels fill it")
}

fn land_and_water(map: &mut Canvas, places: &[(f64, f64)], rng: &mut Rng) {
    for &(x, y) in places {
        let glow = Gradient::radial(x, y, 0.0, x, y, 40.0)
            .stop(0.0, Colour::rgba(255, 230, 160, 0.25))
            .stop(1.0, Colour::rgba(255, 230, 160, 0.0));
        map.fill_rect(
            x - 40.0,
            y - 40.0,
            80.0,
            80.0,
            &Brush::new(glow).blend(Blend::Lighter),
        );

        let mut blob = Path::new();
        let radius = 10.0 + rng.next_f64() * 14.0;
        for step in 0..28 {
            let angle = f64::from(step) / 28.0 * TAU;
            let wobble = radius * (1.0 + 0.28 * vnoise3(angle.cos(), angle.sin(), x, 5.0));
            blob.line_to(x + wobble * angle.cos(), y + wobble * angle.sin());
        }
        blob.close();
        map.fill(&blob, &Brush::new(Colour::rgba(40, 90, 150, 0.8)));
        map.stroke(
            &blob,
            &Pen::new(1.0).dash(&[2.0, 4.0]),
            &Brush::new(Colour::rgba(220, 240, 255, 0.5)),
        );
    }
    for river in 0..30 {
        let (mut x, mut y) = places[river % places.len()];
        let mut path = Path::new();
        path.move_to(x, y);
        for _ in 0..20 {
            x += rng.normal() * 9.0;
            y += rng.normal() * 9.0;
            path.line_to(x, y);
        }
        let water = Colour::rgb(90, 160, 220);
        let brush = Brush::new(water);
        let brush = if river % 3 == 0 {
            brush.glow(water, 4.0)
        } else {
            brush
        };
        map.stroke(&path, &Pen::new(2.0).round(), &brush);
    }
}

fn weather(map: &mut Canvas, rng: &mut Rng) {
    let mut storm = Canvas::new(64, 64);
    for ring in 0..6 {
        let radius = 30.0 - f64::from(ring) * 4.0;
        let cloud = Gradient::radial(32.0, 32.0, 0.0, 32.0, 32.0, radius)
            .stop(0.0, Colour::rgba(240, 240, 250, 0.6))
            .stop(1.0, Colour::rgba(240, 240, 250, 0.0));
        let mut disc = Path::new();
        disc.circle(32.0, 32.0, radius);
        storm.fill(&disc, &Brush::new(cloud));
    }
    for _ in 0..12 {
        let (x, y) = (
            rng.next_f64() * WIDTH as f64,
            rng.next_f64() * HEIGHT as f64,
        );
        let turned = Transform::IDENTITY
            .translate(x, y)
            .rotate(rng.next_f64() * TAU);
        let shade = Brush::default()
            .at(Transform::IDENTITY.translate(x + 5.0, y + 7.0))
            .brightness(0.0)
            .blur(1.5)
            .alpha(0.3);
        map.draw(&storm, -32.0, -32.0, &shade);
        map.draw(&storm, -32.0, -32.0, &Brush::default().at(turned));
    }

    let mut clouds = Canvas::new(WIDTH, HEIGHT);
    for _ in 0..40 {
        let mut puff = Path::new();
        let (x, y) = (
            rng.next_f64() * WIDTH as f64,
            rng.next_f64() * HEIGHT as f64,
        );
        puff.ellipse(x, y, 40.0, 14.0, 0.0, 0.0, TAU, false);
        clouds.fill(&puff, &Brush::new(Colour::rgba(0, 0, 0, 0.5)));
    }
    clouds.blur(1.5);
    let mut land = Path::new();
    land.rect(
        40.0,
        30.0,
        f64::from(WIDTH) - 80.0,
        f64::from(HEIGHT) - 60.0,
    );
    clouds.fill(&land, &Brush::default().blend(Blend::DestinationIn));
    map.draw(&clouds, 0.0, 0.0, &Brush::default().alpha(0.6));

    let dusk = Gradient::linear(0.0, 0.0, f64::from(WIDTH), 0.0)
        .stop(0.0, Colour::rgba(255, 140, 60, 0.15))
        .stop(1.0, Colour::rgba(40, 60, 160, 0.15));
    map.fill_rect(
        0.0,
        0.0,
        f64::from(WIDTH),
        f64::from(HEIGHT),
        &Brush::new(dusk).blend(Blend::Screen),
    );
}

fn dots(map: &mut Canvas, rng: &mut Rng) {
    for _ in 0..961 {
        let (x, y) = (
            rng.next_f64() * WIDTH as f64,
            rng.next_f64() * HEIGHT as f64,
        );
        let mut dot = Path::new();
        dot.circle(x, y, 2.5);
        map.fill(&dot, &Brush::new(Colour::rgb(250, 200, 90)));
        map.stroke(
            &dot,
            &Pen::new(1.0),
            &Brush::new(Colour::rgba(20, 20, 30, 0.6)),
        );
    }
}
