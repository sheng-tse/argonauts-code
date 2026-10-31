//! One model's own system: its star in the middle, its busiest behaviors as planets, nearest the
//! most used, each with the map's ground for when it was last used, rings for loops, moons for
//! its finer behaviors and lava for a behavior that mostly failed; the rest of its actions an
//! asteroid belt, one rock per action, round the belt the days of the period with today at the
//! front. Flares rise on the star for the sessions its CLI failed in, and a binary's partner goes
//! round outside everything.
//!
//! In the terminal the system is drawn in depth: what lies behind the star, the planets behind
//! it, the star, what lies in front, and the planets in front, so a planet going round passes
//! behind the star and in front of it again by moving its small picture.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use super::galaxy::remnant;
use super::model::{Planet, StarKind, flares_shown, rng_for};
use super::motion::{self, Bounds};
use super::paint::{self, Rgb, WHITE, core_radius, ink, mixc, rgb};
use super::scenes::{Cosmos, Level, View};
use super::shapes::SystemFrame;
use super::stars::CLOUD_SECONDS;
use super::universe::add_ring;
use crate::world::deliver::clock::Rate;
use crate::world::raster::noise::{fbm3_shifted, hash3, mix, smooth, vnoise3_shifted};
use crate::world::raster::{Blend, Brush, Canvas, Cap, Colour, Path, Pen};
use crate::world::{
    Depth, HitMap, Image, Layer, Mark, PartId, Picture, Pointed, Scene, Shape, Sprite,
};

/// A binary's partner goes round the system once in this many seconds, divided by how many
/// times more often than chance the two share runs.
const PARTNER_SECONDS: f64 = 30.0;
/// A flare rises and falls once in this many seconds.
const FLARE_SECONDS: f64 = 4.0;
/// A planet's light is worked out for this many directions round its orbit.
const LIGHTS: f64 = 48.0;

/// Where a planet is at one moment.
#[derive(Debug, Clone, Copy)]
struct Placed {
    planet: usize,
    at: (f64, f64),
    th: f64,
    radius: f64,
}

impl Placed {
    /// In front of the star: on the near half of its orbit.
    fn front(&self) -> bool {
        self.th.sin() >= 0.0
    }
}

/// A rock of the belt where it lies.
#[derive(Debug, Clone, Copy)]
struct Rock {
    /// Its index among the universe's actions.
    action: usize,
    at: (f64, f64),
    /// How near: -1 at the back of the belt, 1 at the front.
    near: f64,
    size: f64,
    failed: bool,
}

impl Cosmos {
    pub(super) fn system_frame(&self, star: usize, width: f64, height: f64) -> SystemFrame {
        let s = &self.universe.stars[star];
        SystemFrame::new(
            s.actions,
            s.system.planets.len(),
            s.system.belt > 0,
            width,
            height,
        )
    }

    /// The planet's radius, from its actions.
    fn planet_radius(planet: &Planet) -> f64 {
        (3.0 + 2.2 * f64::from(planet.count).sqrt()).clamp(4.5, 24.0)
    }

    /// Where each planet is at `t`, or at its still pose.
    fn planets_at(&self, star: usize, frame: &SystemFrame, t: Option<f64>) -> Vec<Placed> {
        let s = &self.universe.stars[star];
        s.system
            .planets
            .iter()
            .enumerate()
            .map(|(k, planet)| {
                let phase = hash3(s.model as f64, planet.behavior as f64, 7.0, 0.0) * TAU;
                let turned = match t {
                    Some(t) if planet.period > 0.0 => TAU * t / planet.period,
                    _ => 0.0,
                };
                let th = phase + turned;
                Placed {
                    planet: k,
                    at: frame.at(frame.orbit(k), th),
                    th,
                    radius: Self::planet_radius(planet),
                }
            })
            .collect()
    }

    /// The rocks of the belt: one for each of the period's actions of the model's other
    /// behaviors, round the belt by the day it came, today at the front.
    fn rocks(&self, star: usize, frame: &SystemFrame) -> Vec<Rock> {
        let s = &self.universe.stars[star];
        let mut rng = rng_for("rocks", s.model as u32 * 97 + 3);
        let days = f64::from(self.universe.days);
        s.system
            .rocks
            .iter()
            .map(|&action| {
                let a = &self.universe.actions[action];
                let along = rng.next_f64();
                let size = 1.1 + 1.7 * rng.next_f64() * rng.next_f64();
                let scatter = rng.next_f64() - 0.5;
                let th = FRAC_PI_2 + f64::from(a.day) / days * TAU + scatter * TAU / days;
                let radius = mix(frame.belt.0, frame.belt.1, along);
                Rock {
                    action,
                    at: frame.at(radius, th),
                    near: th.sin(),
                    size,
                    failed: a.failed,
                }
            })
            .collect()
    }

    /// The partner of a binary, where it is at `t` or still.
    fn partner_at(
        &self,
        star: usize,
        frame: &SystemFrame,
        t: Option<f64>,
    ) -> Option<(usize, (f64, f64), f64)> {
        let binary = self.universe.stars[star].binary?;
        let th = match t {
            Some(t) => TAU * t / motion::stepped(PARTNER_SECONDS / f64::from(binary.lift)) + 2.4,
            None => 2.4,
        };
        Some((binary.with, frame.at(frame.belt.1 - 6.0, th), th))
    }

    /// The system level's scene at `t`.
    pub(super) fn system_scene(&mut self, view: &View, star: usize, t: f64, sky: bool) -> Scene {
        let (width, height) = (view.width.max(1), view.height.max(1));
        let mut scene = Scene::new(width, height);
        if star >= self.universe.stars.len() {
            return scene;
        }
        let frame = self.system_frame(star, width as f64, height as f64);
        let rocks = self.rocks(star, &frame);
        let when = (!view.still).then_some(t);
        let planets = self.planets_at(star, &frame, when);
        let partner = self.partner_at(star, &frame, when);
        let key = (Level::System(star), width, height);
        let mut kept = self.layers.remove(&key).unwrap_or_default();
        if view.still {
            let image = match kept.slot(true, sky).clone() {
                Some(image) => image,
                None => self.system_still(star, &frame, &rocks, width, height, !sky),
            };
            *kept.slot(true, sky) = Some(image.clone());
            let layer_key =
                self.key([20, star as u64, u64::from(sky), width as u64, height as u64]);
            scene.add_layer(Layer {
                id: PartId::new("cosmos-system", 0),
                depth: Depth::Ground,
                picture: Picture::new(layer_key, image),
            });
        } else {
            let (back, front) = match (kept.slot(false, sky).clone(), kept.front.clone()) {
                (Some(back), Some(front)) => (back, front),
                _ => self.system_layers(star, &frame, &rocks, width, height, !sky),
            };
            *kept.slot(false, sky) = Some(back.clone());
            kept.front = Some(front.clone());
            let back_key = self.key([21, star as u64, u64::from(sky), width as u64, height as u64]);
            let front_key = self.key([22, star as u64, width as u64, height as u64]);
            scene.add_layer(Layer {
                id: PartId::new("cosmos-system", 0),
                depth: Depth::Sky,
                picture: Picture::new(back_key, back),
            });
            scene.add_layer(Layer {
                id: PartId::new("cosmos-system", 1),
                depth: Depth::Weather,
                picture: Picture::new(front_key, front),
            });
            self.add_star_sprite(&mut scene, star, &frame, t, view.rate);
            for placed in &planets {
                self.add_planet_sprite(&mut scene, star, placed);
            }
            if let Some((partner, at, th)) = partner {
                self.add_partner_sprite(&mut scene, star, partner, at, th, t);
            }
            scene.moves = self.universe.system_moves(star);
        }
        self.layers.insert(key, kept);
        let flares = self.flare_marks(star, &frame, when);
        scene.hits = self.system_hits(star, &frame, &planets, &rocks, &flares, partner);
        if let Some(ringed) = view.ringed
            && let Some((at, radius)) =
                self.system_ring(star, &frame, &planets, &rocks, &flares, partner, ringed)
        {
            add_ring(&mut scene, at, radius);
        }
        scene
    }

    /// The whole system at its still pose, in one picture, drawn back to front.
    fn system_still(
        &mut self,
        star: usize,
        frame: &SystemFrame,
        rocks: &[Rock],
        width: usize,
        height: usize,
        bare: bool,
    ) -> Image {
        let mut canvas = self.system_back(star, frame, rocks, width, height, bare);
        let planets = self.planets_at(star, frame, None);
        for placed in planets.iter().filter(|p| !p.front()) {
            self.draw_planet(&mut canvas, star, placed, placed.at);
        }
        let partner = self.partner_at(star, frame, None);
        if let Some((with, at, th)) = partner
            && th.sin() < 0.0
        {
            self.draw_partner(&mut canvas, with, at, None);
        }
        self.draw_sun(&mut canvas, star, frame, None);
        self.draw_front(&mut canvas, star, frame, rocks);
        for placed in planets.iter().filter(|p| p.front()) {
            self.draw_planet(&mut canvas, star, placed, placed.at);
        }
        if let Some((with, at, th)) = partner
            && th.sin() >= 0.0
        {
            self.draw_partner(&mut canvas, with, at, None);
        }
        self.system_badge(&mut canvas, star);
        canvas.to_image()
    }

    /// The pictures that hold still while the system moves: everything behind the star, over
    /// the sky or `bare`, and a clear picture of everything in front of it but the planets.
    fn system_layers(
        &mut self,
        star: usize,
        frame: &SystemFrame,
        rocks: &[Rock],
        width: usize,
        height: usize,
        bare: bool,
    ) -> (Image, Image) {
        let back = self
            .system_back(star, frame, rocks, width, height, bare)
            .to_image();
        let mut front = Canvas::new(width as u32, height as u32);
        self.draw_front(&mut front, star, frame, rocks);
        self.system_badge(&mut front, star);
        (back, front.to_image())
    }

    /// The far halves of the orbits, the belt's dust, its far rocks, and a supernova's
    /// remnant, over the sky with its galaxy's band or, `bare`, over nothing.
    fn system_back(
        &mut self,
        star: usize,
        frame: &SystemFrame,
        rocks: &[Rock],
        width: usize,
        height: usize,
        bare: bool,
    ) -> Canvas {
        let mut canvas = if bare {
            Canvas::new(width as u32, height as u32)
        } else {
            let maker = self.universe.stars[star].maker;
            let band = self.universe.makers[maker].rgb;
            self.sky_canvas(width, height, Some(band), true).clone()
        };
        self.system_behind(&mut canvas, star, frame, rocks);
        canvas
    }

    /// The far halves of the orbits, the belt's dust, its far rocks, and a supernova's
    /// remnant.
    fn system_behind(
        &mut self,
        canvas: &mut Canvas,
        star: usize,
        frame: &SystemFrame,
        rocks: &[Rock],
    ) {
        let s = &self.universe.stars[star];
        let colour = rgb(s.rgb);
        let has_belt = s.system.belt > 0;
        let planets = s.system.planets.len();
        let nova = s.supernova.is_some();
        let model = s.model;
        orbits(canvas, frame, planets, false);
        if has_belt {
            // The belt's dust, faint, so the rocks read as one ring.
            let middle = (frame.belt.0 + frame.belt.1) / 2.0;
            let mut ring = Path::new();
            ring.ellipse(
                frame.cx,
                frame.cy,
                middle,
                middle * frame.tilt,
                0.0,
                0.0,
                TAU,
                false,
            );
            let dust = ink(mixc([150.0, 140.0, 130.0], colour, 0.2), 0.05);
            let pen = Pen::new((frame.belt.1 - frame.belt.0) * frame.tilt * 1.1);
            canvas.stroke(&ring, &pen, &Brush::new(dust));
        }
        self.draw_rocks(canvas, colour, rocks, false);
        if nova {
            remnant(canvas, model, (frame.cx, frame.cy), frame.star * 1.7, 0.75);
        }
    }

    /// The near halves of the orbits and the near rocks.
    fn draw_front(
        &mut self,
        canvas: &mut Canvas,
        star: usize,
        frame: &SystemFrame,
        rocks: &[Rock],
    ) {
        let s = &self.universe.stars[star];
        let colour = rgb(s.rgb);
        orbits(canvas, frame, s.system.planets.len(), true);
        self.draw_rocks(canvas, colour, rocks, true);
    }

    fn system_badge(&mut self, canvas: &mut Canvas, star: usize) {
        let s = &self.universe.stars[star];
        let (logo, colour) = (s.logo.clone(), rgb(s.rgb));
        self.paint.badge(canvas, &logo, colour, (30.0, 30.0), 0.95);
    }

    fn draw_rocks(&mut self, canvas: &mut Canvas, colour: Rgb, rocks: &[Rock], near: bool) {
        for rock in rocks.iter().filter(|r| (r.near >= 0.0) == near) {
            let tint = if rock.failed {
                [255.0, 120.0, 80.0]
            } else {
                mixc([178.0, 168.0, 156.0], colour, 0.2)
            };
            if rock.failed {
                self.paint.draw_glow(
                    canvas,
                    [255.0, 90.0, 50.0],
                    rock.at,
                    5.0,
                    0.55,
                    0.4,
                    Blend::SourceOver,
                );
            }
            let mut dot = Path::new();
            dot.circle(rock.at.0, rock.at.1, rock.size / 2.0);
            canvas.fill(
                &dot,
                &Brush::new(ink(tint, 0.6 + 0.35 * (rock.near + 1.0) / 2.0)),
            );
        }
    }

    /// The star itself, its glow and its face, and its flares, at `t` or still.
    fn draw_sun(&mut self, canvas: &mut Canvas, star: usize, frame: &SystemFrame, t: Option<f64>) {
        let s = self.universe.stars[star].clone();
        let (cx, cy, rs) = (frame.cx, frame.cy, frame.star);
        let pulse = self.breath(star, t);
        let add = Blend::Lighter;
        match s.kind {
            StarKind::BlackHole => {
                let k = rs / core_radius(s.actions) * 0.62;
                let days = self.universe.days;
                paint::universe_star(
                    &mut self.paint,
                    canvas,
                    &s,
                    s.model,
                    (cx, cy),
                    k,
                    pulse,
                    0.0,
                    days,
                );
            }
            StarKind::WhiteDwarf => {
                let k = rs / core_radius(s.actions) * 0.5;
                let days = self.universe.days;
                paint::universe_star(
                    &mut self.paint,
                    canvas,
                    &s,
                    s.model,
                    (cx, cy),
                    k,
                    pulse,
                    0.0,
                    days,
                );
            }
            StarKind::Protostar => {
                let spin = t.map_or(0.0, |t| t * TAU / CLOUD_SECONDS);
                self.paint
                    .nebula(canvas, &s, s.model, (cx, cy), rs * 3.4, spin, 1.0);
                self.paint.draw_glow(
                    canvas,
                    [255.0, 130.0, 90.0],
                    (cx, cy),
                    rs * 1.8,
                    0.9,
                    0.4,
                    add,
                );
            }
            StarKind::Star | StarKind::RedGiant => {
                let glow = if s.kind == StarKind::RedGiant {
                    mixc(rgb(s.rgb), [255.0, 112.0, 60.0], 0.6)
                } else {
                    rgb(s.rgb)
                };
                let active = s.last_7_days > 0;
                let wide = if active { 0.55 } else { 0.32 } * (0.8 + 0.2 * pulse);
                self.paint.draw_glow(
                    canvas,
                    glow,
                    (cx, cy),
                    rs * (3.4 + 0.5 * pulse),
                    wide,
                    0.25,
                    add,
                );
                let near = if active { 0.8 } else { 0.5 };
                self.paint.draw_glow(
                    canvas,
                    mixc(glow, WHITE, 0.4),
                    (cx, cy),
                    rs * 1.55,
                    near,
                    0.6,
                    add,
                );
            }
        }
        if matches!(
            s.kind,
            StarKind::Star | StarKind::RedGiant | StarKind::Protostar
        ) {
            let face_radius = if s.kind == StarKind::Protostar {
                rs * 0.42
            } else {
                rs
            };
            let face = self.star_face(star, face_radius);
            let side = face_radius * 2.0;
            canvas.draw_sized(
                &face,
                cx - face_radius,
                cy - face_radius,
                side,
                side,
                &Brush::default(),
            );
        }
        if flares_shown(s.kind) {
            for (k, at) in self.flare_loops(star, t).into_iter().enumerate() {
                draw_flare(canvas, (cx, cy), rs, at, k);
            }
        }
    }

    /// Each flare's angle on the star's edge and how far it has risen.
    fn flare_loops(&self, star: usize, t: Option<f64>) -> Vec<(f64, f64)> {
        let s = &self.universe.stars[star];
        (0..s.system.flares.len())
            .map(|k| {
                let angle = -PI * 0.9 + hash3(s.model as f64, k as f64, 11.0, 0.0) * PI * 1.8;
                let lift = match t {
                    Some(t) => {
                        let seconds = motion::stepped(FLARE_SECONDS);
                        0.35 + 0.65 * (0.5 + 0.5 * (TAU * t / seconds + k as f64 * 1.9).sin())
                    }
                    None => 0.8,
                };
                (angle, lift)
            })
            .collect()
    }

    /// Where each flare's mark is, at `t` or still.
    fn flare_marks(&self, star: usize, frame: &SystemFrame, t: Option<f64>) -> Vec<(f64, f64)> {
        if !flares_shown(self.universe.stars[star].kind) {
            return Vec::new();
        }
        let rs = frame.star;
        self.flare_loops(star, t)
            .into_iter()
            .map(|(angle, lift)| {
                let height = rs * (0.07 + 0.11 * lift);
                (
                    frame.cx + angle.cos() * (rs + height * 0.8),
                    frame.cy + angle.sin() * (rs + height * 0.8),
                )
            })
            .collect()
    }

    /// A star's face: limb-darkened, its surface grained, sunspots in the two belts either side
    /// of its equator for every 2% of its actions that failed.
    fn star_face(&mut self, star: usize, radius: f64) -> Canvas {
        let side = (radius * 2.0).ceil().max(2.0) as usize;
        let key = self.key([23, star as u64, side as u64]);
        if let Some(image) = self.frames.get(&key) {
            return Canvas::from_image(image).unwrap_or_else(|| Canvas::new(1, 1));
        }
        let s = &self.universe.stars[star];
        let giant = s.kind == StarKind::RedGiant;
        let colour = if giant {
            mixc(rgb(s.rgb), [255.0, 112.0, 60.0], 0.6)
        } else {
            rgb(s.rgb)
        };
        let hot = mixc(
            colour,
            [255.0, 250.0, 240.0],
            if giant { 0.4 } else { 0.72 },
        );
        let limb = mixc(colour, [120.0, 40.0, 20.0], if giant { 0.45 } else { 0.25 });
        let seed = s.model as f64;
        let mut rng = rng_for("sunspots", s.model as u32 * 41 + 1);
        let spots: Vec<(f64, f64, f64)> = (0..s.system.sunspots)
            .map(|_| {
                let side = if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };
                let lat = side * (0.18 + 0.27 * rng.next_f64());
                let lon = rng.next_f64() * 1.6 - 0.8;
                let size = 0.035 + 0.05 * rng.next_f64() * rng.next_f64();
                (lon, lat, size)
            })
            .collect();
        // Granules a few pixels across; a giant's few and huge.
        let cell = if giant {
            7.0
        } else {
            (radius * 0.42).max(14.0)
        };
        let mut bytes = vec![0u8; side * side * 4];
        for j in 0..side {
            for i in 0..side {
                let u = (i as f64 + 0.5) / side as f64 * 2.0 - 1.0;
                let v = (j as f64 + 0.5) / side as f64 * 2.0 - 1.0;
                let rr = u * u + v * v;
                if rr > 1.0 {
                    continue;
                }
                let mu = (1.0 - rr).sqrt();
                let darkening = 0.42 + 0.58 * mu.powf(0.6);
                let px = u / (0.35 + 0.65 * mu);
                let grain = 1.0 - vnoise3_shifted(px * cell, v * cell, 0.5, seed).abs();
                let texture = if giant {
                    0.9 + 0.12 * (0.5 + 0.5 * fbm3_shifted(px * 3.0, v * 3.0, 0.5, seed, 3))
                } else {
                    0.9 + 0.13 * grain * grain
                        + 0.05 * fbm3_shifted(u * 3.0, v * 3.0, 2.0, seed + 5.0, 2)
                };
                let mut dark: f64 = 1.0;
                for (sx, sy, size) in &spots {
                    let d = ((u - sx * mu) / size).hypot((v - sy) / (size * 0.8));
                    if d < 1.9 {
                        let shade = if d < 0.7 {
                            0.12
                        } else if d < 1.25 {
                            0.5
                        } else {
                            mix(0.5, 1.0, smooth(1.25, 1.9, d))
                        };
                        dark = dark.min(shade);
                    }
                }
                let k = darkening * texture * dark;
                let base = mixc(limb, hot, mu.powf(0.8));
                let at = (j * side + i) * 4;
                for (c, (channel, warm)) in base.iter().zip([1.06, 1.02, 0.98]).enumerate() {
                    bytes[at + c] = (channel * k * warm).clamp(0.0, 255.0) as u8;
                }
                bytes[at + 3] = (255.0 * smooth(1.0, 0.97, rr.sqrt())) as u8;
            }
        }
        let canvas = Canvas::from_rgba(side as u32, side as u32, &bytes)
            .unwrap_or_else(|| Canvas::new(1, 1));
        self.frames.insert(key, canvas.to_image());
        canvas
    }

    /// The star, its glow and its flares as one picture for the moving system: its glow as
    /// light that lets what is behind show, its face solid over it. Its picture changes every
    /// second step: as big as the glow reaches, a busy star's loop at the full rate would take
    /// more than the terminal keeps for the world (162 MB at 16 a second), so each step would
    /// send a picture again. Its pulse and flares are slow enough to read the same.
    fn add_star_sprite(
        &mut self,
        scene: &mut Scene,
        star: usize,
        frame: &SystemFrame,
        t: f64,
        rate: Rate,
    ) {
        let rs = frame.star;
        let reach = rs * 3.9 + 4.0;
        let bounds = Bounds::around((frame.cx, frame.cy), reach, reach);
        let Some((left, top, w, h)) = bounds.pixels(scene.width(), scene.height()) else {
            return;
        };
        let s = &self.universe.stars[star];
        let mut loops = Vec::new();
        if let Some(pulse) = s.pulse {
            loops.push(pulse);
        }
        if s.kind == StarKind::Protostar {
            loops.push(CLOUD_SECONDS);
        }
        if flares_shown(s.kind) && !s.system.flares.is_empty() {
            loops.push(FLARE_SECONDS);
        }
        let steps = loops
            .into_iter()
            .map(motion::steps_for)
            .fold(1, motion::lcm);
        let stride = 2 * motion::ticks_a_step(rate);
        let step = motion::step_of(t) % u64::from(steps) / stride * stride;
        let looped = step as f64 / motion::TICKS;
        let key = self.key([
            24,
            star as u64,
            step,
            scene.width() as u64,
            scene.height() as u64,
        ]);
        let shifted = frame.moved(-f64::from(left), -f64::from(top));
        let picture = self.frame(key, |cosmos| {
            let mut canvas = Canvas::new(w, h);
            cosmos.draw_sun(&mut canvas, star, &shifted, Some(looped));
            light_and_face(canvas)
        });
        scene.add_sprite(Sprite {
            id: PartId::new("cosmos-sun", 0),
            depth: Depth::Nature,
            at: (left as i32, top as i32),
            picture,
        });
    }

    /// One planet with its rings and moons as a small picture, lit for where it is on its
    /// orbit, behind the star or in front of it.
    fn add_planet_sprite(&mut self, scene: &mut Scene, star: usize, placed: &Placed) {
        let light = ((placed.th.rem_euclid(TAU)) / TAU * LIGHTS).round() as u64 % LIGHTS as u64;
        let moons = self.universe.stars[star].system.planets[placed.planet]
            .moons
            .len();
        let moon_reach = placed.radius + 6.0 + 5.0 * moons.saturating_sub(1) as f64 + 4.0;
        let reach = (placed.radius * 1.9 + 4.0).max(moon_reach) + 2.0;
        let side = (reach * 2.0).ceil() as u32;
        let key = self.key([
            25,
            star as u64,
            placed.planet as u64,
            light,
            u64::from(side),
        ]);
        let lit = Placed {
            th: light as f64 / LIGHTS * TAU,
            ..*placed
        };
        let centre = f64::from(side) / 2.0;
        let picture = self.frame(key, |cosmos| {
            let mut canvas = Canvas::new(side, side);
            cosmos.draw_planet(&mut canvas, star, &lit, (centre, centre));
            canvas.to_image()
        });
        scene.add_sprite(Sprite {
            id: PartId::new("cosmos-planet", placed.planet as u32),
            depth: if placed.front() {
                Depth::Dots
            } else {
                Depth::Ground
            },
            at: (
                (placed.at.0 - centre).round() as i32,
                (placed.at.1 - centre).round() as i32,
            ),
            picture,
        });
    }

    /// A binary's partner going round outside everything.
    fn add_partner_sprite(
        &mut self,
        scene: &mut Scene,
        star: usize,
        partner: usize,
        at: (f64, f64),
        th: f64,
        t: f64,
    ) {
        let p = &self.universe.stars[partner];
        let rc = core_radius(p.actions) * 1.1;
        let reach = (rc * 4.6 * 1.16).max(rc * 4.2 * std::f64::consts::SQRT_2) + 3.0;
        let side = (reach * 2.0).ceil() as u32;
        let mut loops = Vec::new();
        if let Some(pulse) = p.pulse {
            loops.push(pulse);
        }
        if p.kind == StarKind::Protostar {
            loops.push(CLOUD_SECONDS);
        }
        let steps = loops
            .into_iter()
            .map(motion::steps_for)
            .fold(1, motion::lcm);
        let step = motion::step_of(t) % u64::from(steps);
        let looped = step as f64 / motion::TICKS;
        let key = self.key([26, star as u64, partner as u64, step, u64::from(side)]);
        let centre = f64::from(side) / 2.0;
        let picture = self.frame(key, |cosmos| {
            let mut canvas = Canvas::new(side, side);
            cosmos.draw_partner(&mut canvas, partner, (centre, centre), Some(looped));
            light_and_face(canvas)
        });
        scene.add_sprite(Sprite {
            id: PartId::new("cosmos-partner", 0),
            depth: if th.sin() >= 0.0 {
                Depth::Dots
            } else {
                Depth::Ground
            },
            at: (
                (at.0 - centre).round() as i32,
                (at.1 - centre).round() as i32,
            ),
            picture,
        });
    }

    fn draw_partner(
        &mut self,
        canvas: &mut Canvas,
        partner: usize,
        at: (f64, f64),
        t: Option<f64>,
    ) {
        let pulse = self.breath(partner, t);
        let spin = t.map_or(0.0, |t| t * TAU / CLOUD_SECONDS);
        let s = self.universe.stars[partner].clone();
        let days = self.universe.days;
        paint::universe_star(
            &mut self.paint,
            canvas,
            &s,
            s.model,
            at,
            1.1,
            pulse,
            spin,
            days,
        );
    }

    /// One planet at `at`, lit by its star from where it is on its orbit: the side toward the
    /// star bright, so it shows phases as it goes round. Its rings' far half goes behind it and
    /// its near half in front; its moons sit round it.
    fn draw_planet(&mut self, canvas: &mut Canvas, star: usize, placed: &Placed, at: (f64, f64)) {
        let s = &self.universe.stars[star];
        let planet = s.system.planets[placed.planet].clone();
        let model = s.model;
        let tilt = SystemFrame::TILT;
        let (x, y) = at;
        let r = placed.radius;
        // Toward the star, in the camera's frame.
        let light = [
            -placed.th.cos(),
            placed.th.sin() * tilt,
            -placed.th.sin() * (1.0 - tilt * tilt).sqrt(),
        ];
        let loops = planet.loops.min(4) as f64;
        let ring = (planet.loops > 0).then(|| {
            let colour = ink(
                mixc(planet.climate.rgb(), [230.0, 220.0, 200.0], 0.6),
                0.35 + 0.12 * loops,
            );
            (colour, 1.0 + (planet.loops.min(3) as f64))
        });
        let half = |canvas: &mut Canvas, upper: bool| {
            if let Some((colour, width)) = ring {
                let mut arc = Path::new();
                let (from, to) = if upper { (PI, TAU) } else { (0.0, PI) };
                arc.ellipse(x, y, r * 1.9, r * 0.5, -0.22, from, to, false);
                canvas.stroke(&arc, &Pen::new(width), &Brush::new(colour));
            }
        };
        half(canvas, true);
        let face = planet_face(&planet, model, r, light);
        canvas.draw_sized(&face, x - r, y - r, r * 2.0, r * 2.0, &Brush::default());
        half(canvas, false);
        for k in 0..planet.moons.len() {
            let (moon_at, moon_radius) = moon_place(&planet, k, at, r);
            let mut dot = Path::new();
            dot.circle(moon_at.0, moon_at.1, moon_radius);
            canvas.fill(&dot, &Brush::new(Colour::rgba(200, 200, 210, 0.95)));
        }
    }

    /// What the pointer finds in a system: a moon, then a planet, a flare or the partner, then
    /// the star, then the nearest rock, then the belt.
    #[allow(clippy::too_many_arguments)]
    fn system_hits(
        &self,
        star: usize,
        frame: &SystemFrame,
        planets: &[Placed],
        rocks: &[Rock],
        flares: &[(f64, f64)],
        partner: Option<(usize, (f64, f64), f64)>,
    ) -> HitMap {
        let mut hits = HitMap::new();
        let s = &self.universe.stars[star];
        let body = |b: super::model::Body| self.universe.body_index(b).map(Pointed::Body);
        let disc = |(x, y): (f64, f64), radius: f64| Shape::Disc {
            x: x as f32,
            y: y as f32,
            radius: radius as f32,
        };
        for placed in planets {
            let planet = &s.system.planets[placed.planet];
            for k in 0..planet.moons.len() {
                let (at, radius) = moon_place(planet, k, placed.at, placed.radius);
                let found = super::model::Body::Moon {
                    star,
                    planet: placed.planet,
                    moon: k,
                };
                if let Some(pointed) = body(found) {
                    hits.add(Mark::new(disc(at, (radius + 2.0).max(5.0)), pointed));
                }
            }
            let found = super::model::Body::Planet {
                star,
                planet: placed.planet,
            };
            if let Some(pointed) = body(found) {
                hits.add(Mark::new(disc(placed.at, placed.radius + 3.0), pointed).tier(1));
            }
        }
        for (flare, at) in flares.iter().enumerate() {
            if let Some(pointed) = body(super::model::Body::Flare { star, flare }) {
                hits.add(Mark::new(disc(*at, 9.0), pointed).tier(1));
            }
        }
        if let Some((with, at, _)) = partner {
            let radius = (core_radius(self.universe.stars[with].actions) * 1.1).max(8.0);
            hits.add(Mark::new(disc(at, radius), Pointed::Star(with as u32)).tier(1));
        }
        hits.add(
            Mark::new(
                disc((frame.cx, frame.cy), frame.star),
                Pointed::Star(star as u32),
            )
            .tier(2),
        );
        for rock in rocks {
            let action = self.universe.actions[rock.action].index;
            let point = Shape::Point {
                x: rock.at.0 as f32,
                y: rock.at.1 as f32,
            };
            hits.add(Mark::new(point, Pointed::Action(action)).reach(5.0).tier(3));
        }
        if s.system.belt > 0
            && let Some(pointed) = body(super::model::Body::Belt { star })
        {
            // The belt as a chain of discs, each as wide as the belt is there.
            let middle = (frame.belt.0 + frame.belt.1) / 2.0;
            let half = (frame.belt.1 - frame.belt.0) / 2.0 + 0.03 * frame.belt.1;
            for k in 0..96 {
                let th = f64::from(k) / 96.0 * TAU;
                let across = half * th.cos().hypot(frame.tilt * th.sin());
                hits.add(Mark::new(disc(frame.at(middle, th), across), pointed).tier(4));
            }
        }
        hits
    }

    #[allow(clippy::too_many_arguments)]
    fn system_ring(
        &self,
        star: usize,
        frame: &SystemFrame,
        planets: &[Placed],
        rocks: &[Rock],
        flares: &[(f64, f64)],
        partner: Option<(usize, (f64, f64), f64)>,
        ringed: Pointed,
    ) -> Option<((f64, f64), f64)> {
        let s = &self.universe.stars[star];
        match ringed {
            Pointed::Star(found) if found as usize == star => {
                Some(((frame.cx, frame.cy), frame.star + 4.0))
            }
            Pointed::Star(found) => {
                let (with, at, _) = partner?;
                let radius = (core_radius(self.universe.stars[with].actions) * 1.1).max(8.0);
                (found as usize == with).then_some((at, radius + 4.0))
            }
            Pointed::Action(action) => rocks
                .iter()
                .find(|r| self.universe.actions[r.action].index == action)
                .map(|r| (r.at, 5.0)),
            Pointed::Body(found) => match self.universe.body(found)? {
                super::model::Body::Planet { star: sun, planet } if sun == star => {
                    let placed = planets.iter().find(|p| p.planet == planet)?;
                    Some((placed.at, placed.radius + 7.0))
                }
                super::model::Body::Moon {
                    star: sun,
                    planet,
                    moon,
                } if sun == star => {
                    let placed = planets.iter().find(|p| p.planet == planet)?;
                    let p = &s.system.planets[planet];
                    let (at, radius) = moon_place(p, moon, placed.at, placed.radius);
                    Some((at, (radius + 2.0).max(5.0) + 4.0))
                }
                super::model::Body::Flare { star: sun, flare } if sun == star => {
                    flares.get(flare).map(|at| (*at, 13.0))
                }
                _ => None,
            },
            _ => None,
        }
    }
}

/// Where the `k`th moon of a planet at (x, y) of radius `r` sits, and how big it is.
fn moon_place(planet: &Planet, k: usize, (x, y): (f64, f64), r: f64) -> ((f64, f64), f64) {
    let moon = &planet.moons[k];
    let angle = hash3(planet.behavior as f64, moon.behavior as f64, 3.0, 0.0) * TAU;
    let distance = r + 6.0 + 5.0 * k as f64;
    let size = (1.4 + 0.5 * f64::from(moon.count).sqrt()).clamp(1.6, 3.6);
    (
        (x + angle.cos() * distance, y + angle.sin() * distance * 0.6),
        size,
    )
}

/// The far or near halves of the planets' orbits, faint: they tell which planets are nearer
/// the star, and so more used.
fn orbits(canvas: &mut Canvas, frame: &SystemFrame, planets: usize, near: bool) {
    let brush = Brush::new(Colour::rgba(200, 211, 245, 0.14));
    for k in 0..planets {
        let radius = frame.orbit(k);
        let (from, to) = if near { (0.0, PI) } else { (PI, TAU) };
        let mut arc = Path::new();
        arc.ellipse(
            frame.cx,
            frame.cy,
            radius,
            radius * frame.tilt,
            0.0,
            from,
            to,
            false,
        );
        canvas.stroke(&arc, &Pen::new(1.0), &brush);
    }
}

/// One flare: a loop on the star's edge rising and falling, in three strokes from a wide red
/// glow to a thin hot core.
fn draw_flare(
    canvas: &mut Canvas,
    (cx, cy): (f64, f64),
    rs: f64,
    (angle, lift): (f64, f64),
    k: usize,
) {
    let half = 0.2;
    let height = rs * (0.07 + 0.11 * lift);
    let mut path = Path::new();
    for q in 0..=16 {
        let f = f64::from(q) / 16.0;
        let a = angle - half + 2.0 * half * f;
        let rad = rs * 0.99 + height * (PI * f).sin() * (1.0 + 0.15 * (f * 9.0 + k as f64).sin());
        let (px, py) = (cx + a.cos() * rad, cy + a.sin() * rad);
        if q == 0 {
            path.move_to(px, py);
        } else {
            path.line_to(px, py);
        }
    }
    let strokes = [
        (12.0, 0.12, [255.0, 90.0, 60.0]),
        (6.0, 0.32, [255.0, 120.0, 70.0]),
        (2.4, 0.75, [255.0, 200.0, 150.0]),
    ];
    for (width, alpha, colour) in strokes {
        let pen = Pen::new(width * rs / 70.0).cap(Cap::Round);
        canvas.stroke(
            &path,
            &pen,
            &Brush::new(ink(colour, alpha)).blend(Blend::Lighter),
        );
    }
}

/// A planet's ground: the map's colour for when its behavior was last used, seas where the
/// land is wet, white poles; or, for a behavior that mostly failed, dark crust split by glowing
/// cracks, as the map's volcanoes are.
fn planet_ground(planet: &Planet, model: usize, side: usize) -> Vec<[f64; 3]> {
    let base = planet.climate.ground();
    let sea_level = planet.climate.sea_level();
    let seed = (planet.behavior * 7 + model) as f64;
    let mut ground = vec![[0.0; 3]; side * side];
    for j in 0..side {
        for i in 0..side {
            let u = (i as f64 + 0.5) / side as f64 * 2.0 - 1.0;
            let v = (j as f64 + 0.5) / side as f64 * 2.0 - 1.0;
            let rr = u * u + v * v;
            if rr > 1.0 {
                continue;
            }
            let z = (1.0 - rr).sqrt();
            let n = fbm3_shifted(u * 2.2, v * 2.2, z * 2.2, seed, 4);
            let colour = if planet.lava {
                let crack = 1.0 - vnoise3_shifted(u * 5.0, v * 5.0, z * 5.0, seed + 3.0).abs();
                mixc(
                    [38.0, 30.0, 30.0],
                    [255.0, 120.0, 40.0],
                    smooth(0.86, 0.97, crack),
                )
            } else {
                let band = 0.5 + 0.5 * (v * 7.0 + n * 3.0).sin();
                let mut c = if n < sea_level {
                    mixc([34.0, 74.0, 122.0], [52.0, 104.0, 150.0], band)
                } else {
                    let land = mixc(base, mixc(base, WHITE, 0.22), smooth(0.1, 0.6, n));
                    land.map(|channel| channel * (0.86 + 0.14 * band))
                };
                if v.abs() > 0.82 - 0.06 * n {
                    c = mixc(c, [236.0, 240.0, 245.0], 0.8);
                }
                c
            };
            ground[j * side + i] = colour;
        }
    }
    ground
}

/// A planet lit from `light`. The night side keeps a third of its light, so its ground, which
/// is the data, still reads in front of the star.
fn planet_face(planet: &Planet, model: usize, radius: f64, light: [f64; 3]) -> Canvas {
    let side = (radius * 2.0).ceil().max(8.0) as usize;
    let ground = planet_ground(planet, model, side);
    let mut bytes = vec![0u8; side * side * 4];
    for j in 0..side {
        for i in 0..side {
            let u = (i as f64 + 0.5) / side as f64 * 2.0 - 1.0;
            let v = (j as f64 + 0.5) / side as f64 * 2.0 - 1.0;
            let rr = u * u + v * v;
            if rr > 1.0 {
                continue;
            }
            let nz = (1.0 - rr).sqrt();
            let mu = u * light[0] - v * light[1] + nz * light[2];
            let lit = 0.32 + 0.9 * smooth(-0.1, 0.3, mu) * (0.6 + 0.4 * mu.max(0.0));
            let edge = smooth(1.0, 0.86, rr.sqrt());
            let c = ground[j * side + i];
            let glows = if planet.lava && c[0] > 200.0 {
                0.8
            } else {
                0.0
            };
            let at = (j * side + i) * 4;
            for k in 0..3 {
                bytes[at + k] = (c[k] * lit.max(glows)).clamp(0.0, 255.0) as u8;
            }
            bytes[at + 3] = (255.0 * (edge * 1.6).min(1.0)) as u8;
        }
    }
    Canvas::from_rgba(side as u32, side as u32, &bytes).unwrap_or_else(|| Canvas::new(1, 1))
}

/// A picture of a star drawn by adding light onto a clear canvas, made to lie over what is
/// behind it: its glow as light, as opaque as its brightest channel, and its solid face kept
/// solid.
fn light_and_face(mut canvas: Canvas) -> Image {
    for pixel in canvas.premultiplied_mut().chunks_exact_mut(4) {
        let brightest = pixel[0].max(pixel[1]).max(pixel[2]);
        if pixel[3] < 250 {
            pixel[3] = brightest;
        }
    }
    canvas.to_image()
}
