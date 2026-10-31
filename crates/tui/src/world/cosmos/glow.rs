//! The motion the universe keeps whatever you did lately: each galaxy's glow breathes slowly and
//! a few of its stars twinkle in turn, there and in the galaxy's own level, under the motion your
//! recent work adds. Both come round
//! within the 30-second cycle on a handful of pictures, a galaxy's glow at a few strengths and a
//! glint at a few brightnesses, so once each is drawn the screen only moves their placements. The
//! disc itself holds still: a turning one would be a new picture of the whole galaxy each step.

use std::f64::consts::{PI, TAU};

use super::model::rng_for;
use super::motion::{self, Bounds};
use super::paint::{WHITE, ink, mixc, rgb};
use super::scenes::Cosmos;
use super::shapes::Frame;
use crate::world::raster::{Blend, Brush, Canvas, Path, Pen, Transform};
use crate::world::{Depth, Image, PartId, Picture, Scene, Sprite};

/// A galaxy's glow breathes in and out once in this many seconds.
pub(super) const BREATH_SECONDS: f64 = 10.0;
/// The strengths a breath steps through above the still glow. Each is one picture, shown on the
/// way in and again on the way out.
pub(super) const BREATH_LEVELS: u32 = 12;
/// How much of its own light the glow adds at the top of a breath.
const BREATH_GAIN: f64 = 0.45;
/// Stars of one galaxy that twinkle at once.
pub(super) const TWINKLES: u32 = 2;
/// Each twinkle's turn: a star lights for the first part of it, then another takes the next.
const TWINKLE_SECONDS: f64 = 2.5;
const TWINKLE_LIT: f64 = 0.5;
/// The brightnesses a twinkle steps through, each one picture for every star of its galaxy.
pub(super) const GLINT_LEVELS: u32 = 4;
/// A glint's picture is this many pixels square, its middle on a pixel's middle.
const GLINT_SIDE: u32 = 13;

/// One star of a galaxy twinkling at one step.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Twinkle {
    /// Its turn among the galaxy's twinkles at once.
    slot: u32,
    /// Its index among the galaxy's points.
    point: usize,
    /// From 1 to [`GLINT_LEVELS`].
    level: u32,
}

impl Cosmos {
    /// How far into its breath a galaxy's glow is at `t`, from 0 for the still glow to
    /// [`BREATH_LEVELS`].
    fn breath_level(&self, maker: usize, t: f64) -> u32 {
        let ticks = u64::from(motion::steps_for(BREATH_SECONDS));
        let start = self.motion_start(maker, 1, ticks);
        let at = (motion::step_of(t) + start) % ticks;
        let depth = 0.5 - 0.5 * (TAU * at as f64 / ticks as f64).cos();
        (depth * f64::from(BREATH_LEVELS)).round() as u32
    }

    /// The galaxy's stars that twinkle at `t`, each a bright one of its points chosen afresh
    /// for every turn of the cycle.
    fn twinkles(&self, maker: usize, t: f64) -> Vec<Twinkle> {
        let points = &self.shapes.galaxies[maker].points;
        if points.is_empty() {
            return Vec::new();
        }
        let turn = u64::from(motion::steps_for(TWINKLE_SECONDS));
        let cycle = u64::from(motion::CYCLE);
        let start = self.motion_start(maker, 2, cycle);
        let key = &self.universe.makers[maker].key;
        (0..TWINKLES)
            .filter_map(|slot| {
                let at = (motion::step_of(t) + start + u64::from(slot) * turn / 2) % cycle;
                let lit = (at % turn) as f64 / turn as f64 / TWINKLE_LIT;
                if lit >= 1.0 {
                    return None;
                }
                let level = ((PI * lit).sin() * f64::from(GLINT_LEVELS)).ceil() as u32;
                if level == 0 {
                    return None;
                }
                let round = u32::try_from(at / turn).unwrap_or(0);
                let mut rng = rng_for(key, 1_000 + slot * 100 + round);
                let mut pick =
                    || ((rng.next_f64() * points.len() as f64) as usize).min(points.len() - 1);
                // The brightest of three, so a twinkle seldom lands on a faint old action.
                let point = [pick(), pick(), pick()]
                    .into_iter()
                    .max_by(|a, b| points[*a].bright.total_cmp(&points[*b].bright))
                    .unwrap_or(0);
                Some(Twinkle {
                    slot,
                    point,
                    level: level.min(GLINT_LEVELS),
                })
            })
            .collect()
    }

    /// Where in a loop of `ticks` a galaxy's motion `what` starts, so galaxies breathe and
    /// twinkle out of step with one another.
    fn motion_start(&self, maker: usize, what: u32, ticks: u64) -> u64 {
        let key = &self.universe.makers[maker].key;
        (rng_for(key, 0x6c6f_7700 + what).next_f64() * ticks as f64) as u64
    }

    /// Adds each galaxy's glow as it breathes at `t` and the stars that twinkle then, over
    /// everything else that moves, galaxies placed by `galaxies` on a picture the scene's size.
    pub(super) fn add_galaxy_motion(&mut self, scene: &mut Scene, galaxies: &[Frame], t: f64) {
        let (width, height) = (scene.width(), scene.height());
        for (maker, frame) in galaxies.iter().enumerate() {
            let level = self.breath_level(maker, t);
            if level > 0
                && let Some(sprite) = self.glow_sprite(maker, *frame, level, width, height)
            {
                scene.add_sprite(sprite);
            }
            self.add_twinkles(scene, maker, *frame, t);
        }
    }

    /// Adds a glint over each of the galaxy's stars that twinkle at `t`, the galaxy placed by
    /// `frame`.
    pub(super) fn add_twinkles(&mut self, scene: &mut Scene, maker: usize, frame: Frame, t: f64) {
        let tilt = self.shapes.galaxies[maker].tilt;
        let half = f64::from(GLINT_SIDE) / 2.0;
        for twinkle in self.twinkles(maker, t) {
            let point = self.shapes.galaxies[maker].points[twinkle.point];
            let (x, y) = frame.point(tilt, point.rad, point.th);
            scene.add_sprite(Sprite {
                id: PartId::new("cosmos-twinkle", maker as u32 * TWINKLES + twinkle.slot),
                depth: Depth::Dots,
                at: ((x - half).round() as i32, (y - half).round() as i32),
                picture: self.glint(maker, twinkle.level),
            });
        }
    }

    /// A galaxy's glow at `level` of its breath, as light to lay over the still picture: the
    /// disc's own light, as much of it as the level adds.
    fn glow_sprite(
        &mut self,
        maker: usize,
        frame: Frame,
        level: u32,
        width: usize,
        height: usize,
    ) -> Option<Sprite> {
        let tilt = self.shapes.galaxies[maker].tilt;
        let side = super::disc::side(frame.r.round(), 1);
        let half = f64::from(side) / 2.0;
        let (sin, cos) = frame.turn.sin_cos();
        let reach = (
            half * (cos.abs() + tilt * sin.abs()),
            half * (sin.abs() + tilt * cos.abs()),
        );
        let (left, top, w, h) =
            Bounds::around((frame.x, frame.y), reach.0, reach.1).pixels(width, height)?;
        let key = self.key([
            40,
            maker as u64,
            u64::from(level),
            width as u64,
            height as u64,
        ]);
        let picture = self.frame(key, |cosmos| {
            let disc = cosmos.disc_picture(maker, frame.r, 1);
            let placed = Transform::IDENTITY
                .translate(frame.x - f64::from(left), frame.y - f64::from(top))
                .rotate(frame.turn)
                .scale(1.0, tilt);
            let strength = BREATH_GAIN * f64::from(level) / f64::from(BREATH_LEVELS);
            let brush = Brush::default()
                .blend(Blend::Lighter)
                .alpha(strength)
                .at(placed);
            let mut canvas = Canvas::new(w, h);
            canvas.draw_sized(&disc, -half, -half, 2.0 * half, 2.0 * half, &brush);
            light_only(canvas)
        });
        Some(Sprite {
            id: PartId::new("cosmos-glow", maker as u32),
            depth: Depth::Dots,
            at: (left as i32, top as i32),
            picture,
        })
    }

    /// A glint in the galaxy's own light at `level` of a twinkle: a soft point with four thin
    /// rays that reach further as it brightens.
    fn glint(&mut self, maker: usize, level: u32) -> Picture {
        let key = self.key([41, maker as u64, u64::from(level)]);
        self.frame(key, |cosmos| {
            let colour = mixc(rgb(cosmos.universe.makers[maker].rgb), WHITE, 0.55);
            let strength = f64::from(level) / f64::from(GLINT_LEVELS);
            let middle = f64::from(GLINT_SIDE) / 2.0;
            let mut canvas = Canvas::new(GLINT_SIDE, GLINT_SIDE);
            cosmos.paint.draw_glow(
                &mut canvas,
                colour,
                (middle, middle),
                1.6 + 2.4 * strength,
                0.95 * strength,
                0.6,
                Blend::Lighter,
            );
            let reach = 1.5 + (middle - 2.0) * strength;
            let mut rays = Path::new();
            rays.move_to(middle - reach, middle);
            rays.line_to(middle + reach, middle);
            rays.move_to(middle, middle - reach);
            rays.line_to(middle, middle + reach);
            let ink = Brush::new(ink(colour, 0.8 * strength)).blend(Blend::Lighter);
            canvas.stroke(&rays, &Pen::new(0.9), &ink);
            light_only(canvas)
        })
    }
}

/// A moving picture drawn by adding light onto a clear canvas, made to lie over what is behind
/// it as light does: each pixel as opaque as its brightest channel.
pub(super) fn light_only(mut canvas: Canvas) -> Image {
    for pixel in canvas.premultiplied_mut().chunks_exact_mut(4) {
        pixel[3] = pixel[0].max(pixel[1]).max(pixel[2]);
    }
    canvas.to_image()
}
