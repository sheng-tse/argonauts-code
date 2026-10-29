//! Each kind's picture for the line under the map: two terminal cells, 72 × 84 pixels, on a
//! rounded tile of the ground or sea it belongs to, drawn with the map's own drawings so the
//! picture says what to look for. A southern storm's picture is mirrored, its arms winding as
//! the storm on the map does.

use super::Pad;
use super::flat::{tornado_from_above, volcano_flat};
use super::quake::ring_out;
use super::{hurricane, thunder, tsunami};
use crate::world::map::climate::Climate;
use crate::world::map::climate::{ROCK, SNOW};
use crate::world::map::fields::{Rgb, byte};
use crate::world::map::land::{LandKind, LandMark, blob, meander};
use crate::world::map::nature::ground::{atoll, current_runs_band, fault_through, lake, river};
use crate::world::map::nature::{Shoal, Stream, runs_at_sea, shoal_at};
use crate::world::map::relief::{FORESHORTEN, top_row};
use crate::world::raster::noise::hash3;
use crate::world::raster::{Brush, Canvas, Cap, Colour, Corners, Gradient, Path, Pen, Transform};

/// An icon's size in pixels.
pub const WIDTH: u32 = 72;
pub const HEIGHT: u32 = 84;

const LAND: Rgb = [70.0, 100.0, 56.0];
const SEA: Rgb = [20.0, 56.0, 90.0];

/// The tile an icon stands on.
pub fn tile() -> Path {
    let mut path = Path::new();
    path.round_rect(4.0, 10.0, 64.0, 64.0, Corners([12.0; 4]));
    path
}

fn colour(rgb: Rgb, alpha: f64) -> Colour {
    Colour::rgba(
        byte(rgb[0].trunc()),
        byte(rgb[1].trunc()),
        byte(rgb[2].trunc()),
        alpha,
    )
}

/// A land kind standing alone at (x, y), for its picture.
fn land_mark(kind: LandKind, behavior: usize, (x, y): (f64, f64), radius: f64) -> LandMark {
    LandMark {
        kind,
        behavior,
        x,
        y,
        radius,
        reach: radius,
    }
}

/// The picture of a kind of nature, land or ice, by the word nature names it with, or `None`
/// for a word that names no kind.
pub fn icon(word: &str, south: bool) -> Option<Canvas> {
    let mut c = Canvas::new(WIDTH, HEIGHT);
    let on = |c: &mut Canvas, rgb: Rgb| c.fill(&tile(), &Brush::new(colour(rgb, 1.0)));
    match word {
        "rainforest" | "forest" | "grassland" | "grass" | "savanna" | "desert" => {
            let climate = match word {
                "rainforest" => Climate::Rainforest,
                "forest" => Climate::Forest,
                "savanna" => Climate::Savanna,
                "desert" => Climate::Desert,
                _ => Climate::Grassland,
            };
            let rgb = climate.rgb();
            on(&mut c, rgb);
            if climate == Climate::Savanna {
                for k in 0..7 {
                    let k = f64::from(k);
                    let (px, py) = (
                        12.0 + hash3(k, 1.0, 2.0, 3.0) * 48.0,
                        18.0 + hash3(k, 4.0, 5.0, 6.0) * 48.0,
                    );
                    let mut bush = Path::new();
                    bush.ellipse(px, py, 6.0, 3.2, 0.0, 0.0, 7.0, false);
                    c.fill(&bush, &Brush::new(Colour::rgba(52, 58, 30, 0.9)));
                }
            } else {
                let small = matches!(climate, Climate::Desert | Climate::Grassland);
                for k in 0..40 {
                    let k = f64::from(k);
                    let (px, py) = (
                        8.0 + hash3(k, 1.0, 2.0, 3.0) * 56.0,
                        14.0 + hash3(k, 4.0, 5.0, 6.0) * 56.0,
                    );
                    let shade = 0.85 + 0.3 * hash3(k, 7.0, 8.0, 9.0);
                    let mut crown = Path::new();
                    crown.circle(px, py, if small { 3.0 } else { 4.5 });
                    let rgb = [rgb[0] * shade, rgb[1] * shade, rgb[2] * shade];
                    c.fill(&crown, &Brush::new(colour(rgb, 0.8)));
                }
            }
        }
        "rock" => {
            on(&mut c, ROCK);
            let mut peak = Path::new();
            peak.move_to(20.0, 40.0);
            peak.line_to(36.0, 18.0);
            peak.line_to(52.0, 40.0);
            peak.close();
            c.fill(&peak, &Brush::new(colour(SNOW, 1.0)));
        }
        "river" => {
            on(&mut c, LAND);
            let stream = Stream {
                to: 0,
                moves: 0,
                path: meander(10.0, 66.0, 62.0, 18.0, 5),
                width: 9.0,
            };
            river(&mut c, &stream);
        }
        "current" => {
            on(&mut c, SEA);
            let stream = Stream {
                to: 0,
                moves: 16,
                path: meander(6.0, 60.0, 66.0, 26.0, 3),
                width: 0.0,
            };
            let runs = runs_at_sea(&stream, |_, _| true);
            current_runs_band(&mut c, &runs, 0);
            // Its chevrons, placed as the map's flat view places them.
            let pen = Pen::new(1.4).cap(Cap::Round);
            let brush = Brush::new(Colour::rgba(206, 238, 248, 0.75));
            let on_screen = |x: f64, y: f64| (x, top_row() + FORESHORTEN * y);
            for run in &runs {
                let mut s = 22.0;
                while s < run.total - 14.0 {
                    let mut i = 1;
                    while i < run.lengths.len() - 1 && run.lengths[i] < s {
                        i += 1;
                    }
                    let (a, b) = (run.points[i - 1], run.points[i]);
                    let span = run.lengths[i] - run.lengths[i - 1];
                    let span = if span == 0.0 { 1.0 } else { span };
                    let u = ((s - run.lengths[i - 1]) / span).clamp(0.0, 1.0);
                    let taper = run.taper[i - 1] + (run.taper[i] - run.taper[i - 1]) * u;
                    let (x, y) = (a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u);
                    let length = (b.0 - a.0).hypot(b.1 - a.1);
                    let length = if length == 0.0 { 1.0 } else { length };
                    let (ux, uy) = ((b.0 - a.0) / length, (b.1 - a.1) / length);
                    let k = run.band * 0.3 * taper;
                    let mut chevron = Path::new();
                    for (n, (px, py)) in [
                        (x - ux * k - uy * k, y - uy * k + ux * k),
                        (x, y),
                        (x - ux * k + uy * k, y - uy * k - ux * k),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let (sx, sy) = on_screen(px, py);
                        if n == 0 {
                            chevron.move_to(sx, sy);
                        } else {
                            chevron.line_to(sx, sy);
                        }
                    }
                    c.stroke(&chevron, &pen, &brush);
                    s += 34.0;
                }
            }
        }
        "lake" => {
            on(&mut c, LAND);
            lake(&mut c, 36.0, 42.0, 17.0, 4);
        }
        "hurricane" | "typhoon" | "cyclone" => {
            on(&mut c, SEA);
            let cloud = hurricane::sprite(30.0, 7.0);
            c.draw(&cloud, 36.0 - 37.5, 42.0 - 37.5, &Brush::new(Colour::WHITE));
        }
        "tsunami" => {
            on(&mut c, SEA);
            c.fill_rect(58.0, 10.0, 10.0, 64.0, &Brush::new(colour(LAND, 1.0)));
            let wave = tsunami::wave_picture(44.0, -1.0);
            let (x, y) = (44.0 - wave.anchor.0, 62.0 - wave.anchor.1);
            c.draw(&wave.canvas, x, y, &Brush::new(Colour::WHITE));
        }
        "tornado" => {
            on(&mut c, LAND);
            let mut sky = Canvas::new(WIDTH, HEIGHT);
            tornado_from_above(&mut c, &mut sky, (40.0, 34.0), 10.0, 3, (10.0, 70.0));
            c.draw(&sky, 0.0, 0.0, &Brush::new(Colour::WHITE));
        }
        "storm" | "thunderstorm" => {
            on(&mut c, LAND);
            let cloud = thunder::cloud(15.0, 5.0);
            c.draw(
                &cloud.canvas,
                34.0 - cloud.base.0,
                58.0 - cloud.base.1,
                &Brush::new(Colour::WHITE),
            );
            let mut bolt = Path::new();
            bolt.move_to(33.0, 58.0);
            bolt.line_to(38.0, 63.0);
            bolt.line_to(32.0, 67.0);
            bolt.line_to(36.0, 72.0);
            c.stroke(
                &bolt,
                &Pen::new(2.2),
                &Brush::new(Colour::rgba(255, 250, 214, 1.0)),
            );
        }
        "volcano" => {
            on(&mut c, LAND);
            let (mut glow, mut sky) = (Canvas::new(WIDTH, HEIGHT), Canvas::new(WIDTH, HEIGHT));
            volcano_flat(&mut c, &mut glow, &mut sky, (30.0, 50.0), 18.0, true, 2);
            c.draw(&sky, 0.0, 0.0, &Brush::new(Colour::WHITE));
        }
        "fault" | "quake" => {
            on(&mut c, LAND);
            let length = if word == "fault" { 26.0 } else { 22.0 };
            let twice = Transform::IDENTITY
                .translate(36.0, 42.0)
                .scale(2.0, 2.0)
                .translate(-36.0, -42.0);
            fault_through(&mut c, (36.0, 42.0), length, 0.6, 3, twice);
            if word == "quake" {
                let mut pad = Pad::new(0.0, 0.0, WIDTH, HEIGHT);
                ring_out(&mut pad, (36.0, 42.0), 30.0, 0.35, 3, 1.0);
                c.draw(pad.canvas(), 0.0, 0.0, &Brush::new(Colour::WHITE));
            }
        }
        "swamp" => {
            on(&mut c, LAND);
            land_mark(LandKind::Swamp { waited: 0 }, 6, (36.0, 42.0), 24.0)
                .picture(&mut c, Transform::IDENTITY);
        }
        "plateau" => {
            on(&mut c, LAND);
            let top = || {
                let mut path = Path::new();
                path.ellipse(36.0, 30.0, 22.0, 8.0, 0.0, 0.0, 7.0, false);
                path
            };
            c.fill(&top(), &Brush::new(Colour::rgb(164, 136, 104)));
            for k in 0..5 {
                let band = if k % 2 == 1 {
                    Colour::rgb(214, 166, 116)
                } else {
                    Colour::rgb(146, 80, 56)
                };
                c.fill_rect(
                    14.0,
                    30.0 + f64::from(k) * 6.0,
                    44.0,
                    6.0,
                    &Brush::new(band),
                );
            }
            c.fill(&top(), &Brush::new(Colour::rgb(164, 136, 104)));
        }
        "basin" => {
            on(&mut c, LAND);
            let floor = LandKind::Basin {
                from: Vec::new(),
                total: 0,
            };
            land_mark(floor, 2, (36.0, 46.0), 15.0).picture(&mut c, Transform::IDENTITY);
            // Three streams running down to the pool in the middle, where they meet.
            let pen = Pen::new(3.0).round();
            let brush = Brush::new(Colour::rgb(96, 158, 196));
            for a in [-2.4_f64, -1.2, 0.2] {
                let mut stream = Path::new();
                stream.move_to(36.0 + a.cos() * 30.0, 46.0 + a.sin() * 26.0);
                stream.quad_to(
                    36.0 + (a + 0.4).cos() * 14.0,
                    46.0 + (a + 0.4).sin() * 12.0,
                    36.0,
                    46.0,
                );
                c.stroke(&stream, &pen, &brush);
            }
        }
        "oasis" => {
            on(&mut c, Climate::Desert.rgb());
            let larger = Transform::IDENTITY
                .translate(36.0, 42.0)
                .scale(1.9, 1.9)
                .translate(-36.0, -42.0);
            land_mark(LandKind::Oasis, 3, (36.0, 42.0), 16.0).picture(&mut c, larger);
        }
        "flood" => {
            on(&mut c, LAND);
            let water = blob(36.0, 44.0, 22.0, 3.0, 0.35, 28, 1.3);
            c.fill(&water, &Brush::new(Colour::rgba(52, 84, 104, 0.9)));
        }
        "fire" | "wildfire" => {
            on(&mut c, LAND);
            let scar = blob(36.0, 44.0, 22.0, 8.0, 0.35, 28, 1.3);
            c.fill(&scar, &Brush::new(Colour::rgba(46, 38, 34, 0.95)));
            let mut front = Path::new();
            front.arc(36.0, 44.0, 21.0, -0.9, 0.9, false);
            c.stroke(
                &front,
                &Pen::new(3.0),
                &Brush::new(Colour::rgb(255, 128, 40)),
            );
        }
        "atoll" => {
            on(&mut c, SEA);
            atoll(&mut c, 36.0, 42.0, 18.0, 2);
        }
        "cap" | "shelf" | "ice" => {
            on(&mut c, SEA);
            let mut ice = Path::new();
            ice.round_rect(4.0, 10.0, 64.0, 26.0, Corners([12.0, 12.0, 0.0, 0.0]));
            c.fill(&ice, &Brush::new(Colour::rgb(236, 242, 250)));
        }
        "iceberg" => {
            on(&mut c, SEA);
            for (px, py, s) in [(24.0, 36.0, 9.0), (48.0, 52.0, 6.0), (40.0, 26.0, 4.0)] {
                let mut melt = Path::new();
                melt.circle(px, py, s * 1.7);
                c.fill(&melt, &Brush::new(Colour::rgba(160, 210, 235, 0.5)));
                c.fill(
                    &blob(px, py, s, px, 0.3, 7, 1.3),
                    &Brush::new(Colour::rgb(246, 250, 254)),
                );
            }
        }
        "aurora" => {
            c.fill(&tile(), &Brush::new(Colour::rgb(10, 14, 30)));
            for k in 0..5 {
                let curtain = Gradient::linear(0.0, 30.0, 0.0, 70.0)
                    .stop(0.0, Colour::rgba(170, 90, 255, 0.0))
                    .stop(0.5, Colour::rgba(80, 255, 160, 0.8))
                    .stop(1.0, Colour::rgba(80, 255, 160, 0.0));
                let k = f64::from(k);
                c.fill_rect(
                    12.0 + k * 10.0,
                    26.0 + k.sin() * 6.0,
                    6.0,
                    40.0,
                    &Brush::new(curtain),
                );
            }
        }
        "clouds" => {
            on(&mut c, SEA);
            for (px, py, s) in [
                (26.0, 40.0, 14.0),
                (42.0, 36.0, 16.0),
                (50.0, 50.0, 11.0),
                (30.0, 54.0, 10.0),
            ] {
                let mut puff = Path::new();
                puff.circle(px, py, s);
                c.fill(&puff, &Brush::new(Colour::rgba(240, 244, 250, 0.85)));
            }
        }
        "sea" => on(&mut c, SEA),
        "shoal" => {
            on(&mut c, SEA);
            let shoal = Shoal {
                name: String::new(),
                x: 36.0,
                y: 42.0,
                angle: 0.5,
            };
            let mut rgba = c.to_rgba();
            for j in 10..74 {
                for i in 4..68 {
                    let at = (j * WIDTH as usize + i) * 4;
                    let sea = [rgba[at], rgba[at + 1], rgba[at + 2]].map(f64::from);
                    let x = 36.0 + (i as f64 - 36.0) * 0.62;
                    let y = 42.0 + (j as f64 - 42.0) * 0.62;
                    let shown = shoal_at(&shoal, x, y, sea);
                    for ch in 0..3 {
                        rgba[at + ch] = byte(shown[ch]);
                    }
                }
            }
            c.put_rgba(&rgba);
        }
        _ => return None,
    }
    if south {
        let mut mirrored = Canvas::new(WIDTH, HEIGHT);
        let flip = Transform::IDENTITY
            .translate(0.0, f64::from(HEIGHT))
            .scale(1.0, -1.0);
        mirrored.draw(&c, 0.0, 0.0, &Brush::new(Colour::WHITE).at(flip));
        return Some(mirrored);
    }
    Some(c)
}
