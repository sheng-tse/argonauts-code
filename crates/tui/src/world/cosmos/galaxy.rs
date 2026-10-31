//! A maker's galaxy at its own level: the same galaxy laid nearly flat across the picture, its
//! actions along the arms, and each of its models one body by how it is used, drawn over the
//! galaxy's light rather than added to it, so each kind keeps its own look on the white core. A
//! star is a disc in its model's colour, limb-darkened, its glow breathing when it worked in the
//! last day; a red giant is swollen, deep red and mottled; a white dwarf is a hard white point in
//! the ring it shed, the ring as wide as the days since its newer model came; a protostar is a
//! red knot in a turning cloud with a dark disc across it; a black hole is dark, ringed by its
//! bent disc. A thin dark rim parts every body from the bright light behind it.

use std::f64::consts::{SQRT_2, TAU};

use super::model::{StarKind, rng_for};
use super::motion::{self, Bounds, Group};
use super::paint::{self, GIANT, WHITE, core_radius, ink, mixc, rgb};
use super::scenes::{Cosmos, Level, View};
use super::shapes::{self, Frame};
use super::stars::CLOUD_SECONDS;
use super::universe::add_ring;
use crate::world::raster::{
    Blend, Brush, Canvas, Colour, Gradient, Path, Pen, Smoothing, Transform,
};
use crate::world::{Depth, HitMap, Image, Layer, Mark, PartId, Picture, Pointed, Scene, Shape};

/// A body's size at the galaxy level, against its star's at the universe's.
pub(super) const AT_GALAXY: f64 = 2.0;

impl Cosmos {
    /// Where one maker's galaxy lies at its own level.
    pub(super) fn galaxy_frame(&self, maker: usize, width: f64, height: f64) -> Frame {
        let dwarf = self.universe.makers[maker].dwarf;
        shapes::galaxy_frame(&self.shapes.galaxies[maker], dwarf, width, height)
    }

    /// The galaxy level's scene at `t`.
    pub(super) fn galaxy_scene(&mut self, view: &View, maker: usize, t: f64, sky: bool) -> Scene {
        let (width, height) = (view.width.max(1), view.height.max(1));
        let mut scene = Scene::new(width, height);
        if maker >= self.universe.makers.len() {
            return scene;
        }
        let frame = self.galaxy_frame(maker, width as f64, height as f64);
        let (moving, groups) = self.galaxy_groups(maker, frame);
        let key = (Level::Galaxy(maker), width, height);
        let mut kept = self.layers.remove(&key).unwrap_or_default();
        let mut image = match kept.slot(view.still, sky).clone() {
            Some(image) => image,
            None => {
                let apart = if view.still { &[][..] } else { &moving[..] };
                self.galaxy_picture(maker, frame, width, height, apart, !sky)
            }
        };
        *kept.slot(view.still, sky) = Some(image.clone());
        self.layers.insert(key, kept);
        let mut layer_key = self.key([
            10,
            maker as u64,
            u64::from(view.still),
            u64::from(sky),
            width as u64,
            height as u64,
        ]);
        let places = self.galaxy_places(maker, frame, &groups, (!view.still).then_some(t));
        if !view.still && !groups.is_empty() {
            let draw = |cosmos: &mut Cosmos,
                        canvas: &mut Canvas,
                        star: usize,
                        (left, top): (f64, f64),
                        looped: f64| {
                let (x, y) = cosmos.place_star(frame, star, AT_GALAXY, Some(looped));
                cosmos.draw_body(canvas, star, (x - left, y - top), Some(looped));
            };
            if sky {
                self.add_groups(&mut scene, &image, &groups, t, 11, draw);
            } else {
                image = self.draw_groups(&image, &groups, t, draw);
                layer_key = self.key([layer_key, motion::step_of(t)]);
            }
        }
        if !view.still {
            self.add_twinkles(&mut scene, maker, frame, t);
            scene.moves = true;
        }
        scene.add_layer(Layer {
            id: PartId::new("cosmos-galaxy", 0),
            depth: Depth::Ground,
            picture: Picture::new(layer_key, image),
        });
        scene.hits = self.galaxy_hits(maker, frame, &places);
        if let Some(ringed) = view.ringed {
            let ring = match ringed {
                Pointed::Star(star) => places
                    .iter()
                    .find(|(s, _)| *s as u32 == star)
                    .map(|(s, at)| (*at, self.body_reach(*s) + 4.0)),
                Pointed::Action(action) => self.shapes.galaxies[maker]
                    .points
                    .iter()
                    .find(|p| self.universe.actions[p.action].index == action)
                    .map(|p| {
                        (
                            frame.point(self.shapes.galaxies[maker].tilt, p.rad, p.th),
                            5.0,
                        )
                    }),
                _ => None,
            };
            if let Some((at, radius)) = ring {
                add_ring(&mut scene, at, radius);
            }
        }
        scene
    }

    /// A maker's models whose bodies move, and the groups they are drawn apart in.
    fn galaxy_groups(&self, maker: usize, frame: Frame) -> (Vec<usize>, Vec<Group>) {
        let moving: Vec<usize> = self.universe.makers[maker]
            .stars
            .iter()
            .copied()
            .filter(|s| self.star_moves(*s))
            .collect();
        let things: Vec<(Bounds, u32)> = moving
            .iter()
            .map(|s| (self.body_bounds(frame, *s), self.star_steps(*s)))
            .collect();
        let groups = motion::groups(&things)
            .into_iter()
            .map(|mut group| {
                group.members = group.members.iter().map(|m| moving[*m]).collect();
                group
            })
            .collect();
        (moving, groups)
    }

    /// Where each of a maker's bodies is at `t`, each moving one where its group's loop has
    /// it, or all at their still poses.
    pub(super) fn galaxy_places(
        &self,
        maker: usize,
        frame: Frame,
        groups: &[Group],
        t: Option<f64>,
    ) -> Vec<(usize, (f64, f64))> {
        self.universe.makers[maker]
            .stars
            .iter()
            .map(|&star| {
                let looped = t.and_then(|t| {
                    let group = groups.iter().find(|g| g.members.contains(&star))?;
                    Some(motion::looped(group, t))
                });
                (star, self.place_star(frame, star, AT_GALAXY, looped))
            })
            .collect()
    }

    /// Where a body of a maker's galaxy is at `t`, or at its still pose, for a zoom to grow it.
    pub(super) fn galaxy_place(
        &self,
        maker: usize,
        frame: Frame,
        star: usize,
        t: Option<f64>,
    ) -> Option<(f64, f64)> {
        let (_, groups) = self.galaxy_groups(maker, frame);
        self.galaxy_places(maker, frame, &groups, t)
            .into_iter()
            .find(|(s, _)| *s == star)
            .map(|(_, at)| at)
    }

    /// Everything of the galaxy level but the bodies in `moving`, at their still poses.
    pub(super) fn galaxy_picture(
        &mut self,
        maker: usize,
        frame: Frame,
        width: usize,
        height: usize,
        moving: &[usize],
        bare: bool,
    ) -> Image {
        let mut canvas = if bare {
            Canvas::new(width as u32, height as u32)
        } else {
            self.sky_canvas(width, height, None, true).clone()
        };
        let tilt = self.shapes.galaxies[maker].tilt;
        let (disc, full) = self.disc_small(maker, frame.r, 2);
        let placed = Transform::IDENTITY
            .translate(frame.x, frame.y)
            .rotate(frame.turn)
            .scale(1.0, tilt);
        let brush = Brush::default()
            .blend(Blend::Lighter)
            .at(placed)
            .smoothing(Smoothing::Bicubic);
        canvas.draw_sized(&disc, -full / 2.0, -full / 2.0, full, full, &brush);
        let points = self.shapes.galaxies[maker].points.clone();
        for point in &points {
            let (x, y) = frame.point(tilt, point.rad, point.th);
            let colour = rgb(self.universe.stars[point.star].rgb);
            let tint = mixc(colour, WHITE, if point.fresh { 0.35 } else { 0.1 });
            if point.fresh {
                self.paint.draw_glow(
                    &mut canvas,
                    colour,
                    (x, y),
                    5.0,
                    0.35 * point.bright,
                    0.4,
                    Blend::Lighter,
                );
            }
            let alpha = if point.fresh { 0.95 } else { 0.6 } * point.bright;
            let side = if point.fresh { 2.5 } else { 1.8 };
            let brush = Brush::new(ink(tint, alpha)).blend(Blend::Lighter);
            canvas.fill_rect(x - side / 2.0, y - side / 2.0, side, side, &brush);
        }
        let stars = self.universe.makers[maker].stars.clone();
        for &star in &stars {
            let at = self.place_star(frame, star, AT_GALAXY, None);
            if self.universe.stars[star].supernova.is_some() {
                // Its filaments drawn strong enough to find.
                let radius = core_radius(self.universe.stars[star].actions) * AT_GALAXY * 3.6;
                remnant(
                    &mut canvas,
                    self.universe.stars[star].model,
                    at,
                    radius,
                    2.6,
                );
            }
            if let Some(big) = self.shapes.stars[star].companion_of {
                // The binary's orbit, faint, so the pair reads as a pair.
                let centre = self.place_star(frame, big, AT_GALAXY, None);
                let distance = self.orbit_distance(big, AT_GALAXY);
                let mut orbit = Path::new();
                orbit.ellipse(
                    centre.0,
                    centre.1,
                    distance,
                    distance * tilt,
                    0.0,
                    0.0,
                    TAU,
                    false,
                );
                let colour = rgb(self.universe.stars[star].rgb);
                canvas.stroke(
                    &orbit,
                    &Pen::new(1.0).dash(&[2.0, 4.0]),
                    &Brush::new(ink(colour, 0.45)),
                );
            }
            if !moving.contains(&star) {
                self.draw_body(&mut canvas, star, at, None);
            }
        }
        let m = &self.universe.makers[maker];
        let (logo, colour) = (m.logo.clone(), rgb(m.rgb));
        self.paint
            .badge(&mut canvas, &logo, colour, (30.0, 30.0), 0.95);
        canvas.to_image()
    }

    /// The box a moving body stays inside at the galaxy level, its glow, its cloud and its
    /// whole way round included.
    fn body_bounds(&self, frame: Frame, star: usize) -> Bounds {
        let s = &self.universe.stars[star];
        let r0 = core_radius(s.actions) * AT_GALAXY * 0.62;
        let reach = match s.kind {
            StarKind::Star => r0 * 2.7 * 1.14,
            StarKind::RedGiant => r0 * GIANT * 2.0 * 1.14,
            StarKind::Protostar => (r0 * 6.0).max(54.0) * 0.9 * SQRT_2,
            StarKind::WhiteDwarf => (r0 * 3.7 + 6.0) * 1.2,
            StarKind::BlackHole => core_radius(s.actions) * AT_GALAXY * 3.6,
        } + 3.0;
        match self.shapes.stars[star].companion_of {
            Some(big) => {
                let tilt = self.shapes.galaxies[s.maker].tilt;
                let centre = self.place_star(frame, big, AT_GALAXY, None);
                let distance = self.orbit_distance(big, AT_GALAXY);
                Bounds::around(centre, distance + reach, distance * tilt + reach)
            }
            None => Bounds::around(self.place_star(frame, star, AT_GALAXY, None), reach, reach),
        }
    }

    /// The radius the pointer finds a body within at the galaxy level.
    fn body_reach(&self, star: usize) -> f64 {
        let s = &self.universe.stars[star];
        let r0 = core_radius(s.actions) * AT_GALAXY * 0.62;
        let drawn = match s.kind {
            StarKind::Protostar => (r0 * 0.5).max(3.0) * 3.0,
            StarKind::WhiteDwarf => r0 * (1.5 + 2.2 * self.dwarf_age(star)) + 6.0,
            StarKind::BlackHole => core_radius(s.actions) * AT_GALAXY * 1.6,
            StarKind::Star => r0,
            StarKind::RedGiant => r0 * GIANT,
        };
        drawn.max(match s.kind {
            StarKind::Protostar => 12.0,
            _ => 7.0,
        })
    }

    /// How long ago a white dwarf's newer model came, as a share of the period.
    fn dwarf_age(&self, star: usize) -> f64 {
        let came = self.universe.stars[star].newer.map_or(0, |(_, came)| came);
        (f64::from(came) / f64::from(self.universe.days.max(1))).clamp(0.0, 1.0)
    }

    /// One body as the galaxy level shows it, at `t` or still.
    fn draw_body(&mut self, canvas: &mut Canvas, star: usize, at: (f64, f64), t: Option<f64>) {
        let pulse = self.breath(star, t);
        let spin = t.map_or(0.0, |t| t * TAU / CLOUD_SECONDS);
        let age = self.dwarf_age(star);
        let s = &self.universe.stars[star];
        let (x, y) = at;
        let c = rgb(s.rgb);
        let bright = if s.quiet() { 0.78 } else { 1.0 };
        let r0 = core_radius(s.actions) * AT_GALAXY * 0.62;
        match s.kind {
            StarKind::Protostar => {
                // A protostar sits deep in the dark cloud it gathers from, which turns while it
                // is new.
                let cloud = (r0 * 6.0).max(54.0);
                let dark = Gradient::radial(x, y, 0.0, x, y, cloud)
                    .stop(0.0, Colour::rgba(18, 10, 20, 0.78))
                    .stop(0.6, Colour::rgba(18, 10, 20, 0.5))
                    .stop(1.0, Colour::rgba(18, 10, 20, 0.0));
                let mut round = Path::new();
                round.circle(x, y, cloud);
                canvas.fill(&round, &Brush::new(dark));
                let s = s.clone();
                self.paint
                    .nebula(canvas, &s, s.model, at, cloud * 0.9, spin, 1.0);
                // The young star seen past the edge of its disc: light above and below a dark
                // band, slanted a little.
                let knot = (r0 * 0.5).max(3.0);
                let slant: f64 = -0.35;
                let (ux, uy) = (slant.cos(), slant.sin());
                for side in [1.0, -1.0] {
                    let spot = (x - uy * knot * 1.3 * side, y + ux * knot * 1.3 * side);
                    let strength = 0.9 * (0.8 + 0.2 * pulse);
                    self.paint.draw_glow(
                        canvas,
                        [255.0, 130.0, 90.0],
                        spot,
                        knot * 2.4,
                        strength,
                        0.4,
                        Blend::Lighter,
                    );
                }
                let mut core = Path::new();
                core.circle(x, y, knot * 0.75);
                canvas.fill(&core, &Brush::new(ink([255.0, 206.0, 168.0], 1.0)));
                let mut band = Path::new();
                band.ellipse(x, y, knot * 3.4, knot * 0.34, slant, 0.0, TAU, false);
                canvas.fill(&band, &Brush::new(Colour::rgba(16, 8, 12, 0.82)));
            }
            StarKind::WhiteDwarf => {
                let shell = r0 * (1.5 + 2.2 * age) + 6.0;
                let tint = mixc(c, [110.0, 222.0, 236.0], 0.85);
                let mut round = Path::new();
                round.circle(x, y, shell);
                canvas.fill(&round, &Brush::new(ink(tint, 0.08)));
                canvas.stroke(
                    &round,
                    &Pen::new((shell * 0.16).max(2.0)),
                    &Brush::new(ink(tint, 0.62)),
                );
                let mut outer = Path::new();
                outer.circle(x, y, shell * 1.13);
                let pink = Brush::new(ink([255.0, 140.0, 170.0], 0.5));
                canvas.stroke(&outer, &Pen::new((shell * 0.07).max(1.0)), &pink);
                let core = (r0 * 0.3).max(2.4);
                let strength = 0.8 * bright * (0.85 + 0.15 * pulse);
                self.paint.draw_glow(
                    canvas,
                    [220.0, 235.0, 255.0],
                    at,
                    core * 4.0,
                    strength,
                    0.4,
                    Blend::Lighter,
                );
                seat(canvas, at, core);
                let mut point = Path::new();
                point.circle(x, y, core);
                canvas.fill(&point, &Brush::new(ink([250.0, 252.0, 255.0], 1.0)));
            }
            StarKind::BlackHole => {
                // The black hole keeps the universe's drawing, at the galaxy's size.
                let rc = core_radius(s.actions) * AT_GALAXY;
                seat(canvas, at, rc * 0.7 * 1.05);
                let s = s.clone();
                paint::black_hole(&mut self.paint, canvas, &s, at, rc, 1.0, Blend::SourceOver);
            }
            StarKind::Star | StarKind::RedGiant => {
                let giant = s.kind == StarKind::RedGiant;
                let cc = if giant {
                    mixc(c, [246.0, 92.0, 44.0], 0.86)
                } else {
                    c
                };
                let r = if giant { r0 * GIANT } else { r0 };
                let halo = r * if giant { 2.0 } else { 2.7 } * (1.0 + 0.14 * pulse);
                let strength = if giant { 0.6 } else { 0.62 } * bright * (0.8 + 0.2 * pulse);
                self.paint.draw_glow(
                    canvas,
                    cc,
                    at,
                    halo,
                    strength,
                    if giant { 0.75 } else { 0.45 },
                    Blend::Lighter,
                );
                seat(canvas, at, r);
                let lit = if giant { 1.0 } else { bright };
                let heart = if giant {
                    ([255.0, 214.0, 150.0], 0.42)
                } else {
                    ([255.0, 250.0, 240.0], 0.78)
                };
                let gradient = Gradient::radial(x - r * 0.22, y - r * 0.22, 0.0, x, y, r)
                    .stop(0.0, ink(mixc(cc, heart.0, heart.1), lit))
                    .stop(
                        0.55,
                        ink(
                            mixc(cc, [255.0, 250.0, 240.0], if giant { 0.06 } else { 0.3 }),
                            lit,
                        ),
                    )
                    .stop(
                        1.0,
                        ink(
                            mixc(cc, [40.0, 8.0, 6.0], if giant { 0.32 } else { 0.22 }),
                            lit,
                        ),
                    );
                let mut body = Path::new();
                body.circle(x, y, r);
                canvas.fill(&body, &Brush::new(gradient));
                if giant {
                    let model = s.model;
                    let boil = self.paint.granules(model);
                    canvas.draw_sized(boil, x - r, y - r, 2.0 * r, 2.0 * r, &Brush::default());
                }
            }
        }
    }

    /// What the pointer finds in a galaxy: its stars first, then the nearest action, then the
    /// galaxy itself anywhere on its disc.
    fn galaxy_hits(&self, maker: usize, frame: Frame, places: &[(usize, (f64, f64))]) -> HitMap {
        let mut hits = HitMap::new();
        let index = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        for (star, (x, y)) in places {
            let shape = Shape::Disc {
                x: *x as f32,
                y: *y as f32,
                radius: self.body_reach(*star) as f32,
            };
            hits.add(Mark::new(shape, Pointed::Star(index(*star))).reach(3.0));
        }
        let shape = &self.shapes.galaxies[maker];
        for point in &shape.points {
            let (x, y) = frame.point(shape.tilt, point.rad, point.th);
            let action = self.universe.actions[point.action].index;
            let mark = Mark::new(
                Shape::Point {
                    x: x as f32,
                    y: y as f32,
                },
                Pointed::Action(action),
            );
            hits.add(mark.reach(5.0).tier(1));
        }
        let disc = Shape::Ellipse {
            x: frame.x as f32,
            y: frame.y as f32,
            rx: (frame.r * 1.05) as f32,
            ry: (frame.r * 1.05 * shape.tilt) as f32,
            angle: frame.turn as f32,
        };
        hits.add(Mark::new(disc, Pointed::Galaxy(index(maker))).tier(2));
        hits
    }

    /// A galaxy's disc drawn at 1/`down` of its size, and the size it is drawn out to.
    pub(super) fn disc_small(&mut self, maker: usize, radius: f64, down: u32) -> (Canvas, f64) {
        let canvas = self.disc_picture(maker, radius, down);
        let full = f64::from(canvas.width() * down);
        (canvas, full)
    }
}

/// The thin dark rim that parts a body from the light behind it.
fn seat(canvas: &mut Canvas, (x, y): (f64, f64), r: f64) {
    let mut rim = Path::new();
    rim.circle(x, y, r + 1.2);
    canvas.fill(&rim, &Brush::new(Colour::rgba(8, 9, 16, 0.55)));
}

/// A supernova remnant: a shell of torn filaments where a model's CLI failed in many sessions
/// on one day.
pub(super) fn remnant(canvas: &mut Canvas, seed: usize, (x, y): (f64, f64), r: f64, alpha: f64) {
    let mut rng = rng_for("remnant", seed as u32);
    let add = Blend::Lighter;
    // The shell's own glow, then its torn filaments.
    let band = Gradient::radial(x, y, r * 0.7, x, y, r * 1.2)
        .stop(0.0, Colour::rgba(110, 230, 220, 0.0))
        .stop(0.55, ink([150.0, 200.0, 220.0], 0.1 * alpha))
        .stop(0.8, ink([255.0, 120.0, 100.0], 0.07 * alpha))
        .stop(1.0, Colour::rgba(255, 120, 100, 0.0));
    let mut shell = Path::new();
    shell.circle(x, y, r * 1.2);
    canvas.fill(&shell, &Brush::new(band).blend(add));
    for _ in 0..34 {
        let start = rng.next_f64() * TAU;
        let length = 0.18 + rng.next_f64() * 0.4;
        let reach = r * (0.9 + 0.16 * rng.next_f64());
        let hot = rng.next_f64() < 0.5;
        let colour = if hot {
            [255.0, 110.0, 90.0]
        } else {
            [110.0, 230.0, 220.0]
        };
        let strength = (0.07 + 0.12 * rng.next_f64()) * alpha;
        let width = (0.8 + rng.next_f64() * 1.2) * (r / 90.0).max(1.0);
        let phase = rng.next_f64() * TAU;
        let mut filament = Path::new();
        for q in 0..=24 {
            let f = f64::from(q) / 24.0;
            let angle = start + length * f;
            let wobble = reach * (1.0 + 0.035 * (f * 5.0 + phase).sin());
            let (px, py) = (x + angle.cos() * wobble, y + angle.sin() * wobble);
            if q == 0 {
                filament.move_to(px, py);
            } else {
                filament.line_to(px, py);
            }
        }
        canvas.stroke(
            &filament,
            &Pen::new(width),
            &Brush::new(ink(colour, strength)).blend(add),
        );
    }
}
