//! The universe level: a galaxy for each maker round your planet, each galaxy's actions as points
//! along its arms and its models' stars among them, the faint ring of your whole mix, tidal
//! streams between makers' models, comets and meteors, and each galaxy's badge. A galaxy's disc
//! holds still in the terminal, to save sending its picture again every few seconds; its glow
//! breathes over it and its stars twinkle all the time, and its stars, binaries and newborn
//! clouds move as your recent work has them.

use std::f64::consts::TAU;

use super::glow::light_only;
use super::model::{StarKind, maker_steps, rng_for, step_colour};
use super::motion::{self, Bounds, Group};
use super::paint::{self, AT_UNIVERSE, GIANT, Rgb, WHITE, core_radius, ink, mixc, rgb};
use super::rules;
use super::scenes::{Cosmos, Level, View};
use super::shapes::{self, DRAWN, Frame};
use super::sky;
use super::stars::CLOUD_SECONDS;
use crate::world::raster::noise::smooth;
use crate::world::raster::{Blend, Brush, Canvas, Cap, Gradient, Path, Pen};
use crate::world::{
    Depth, HitMap, Image, Layer, Mark, PartId, Picture, Pointed, Scene, Shape, Sprite, Under,
};

/// Where everything of the universe sits on one size of picture.
pub(super) struct Plan {
    width: f64,
    height: f64,
    galaxies: std::sync::Arc<[Frame]>,
    clusters: Vec<sky::Cluster>,
    /// Each star's place, a binary's smaller star at its still pose.
    stars: Vec<(f64, f64)>,
    badges: Vec<(f64, f64)>,
    home: f64,
    /// The stars that move, and the groups they are drawn in.
    moving: Vec<usize>,
    groups: Vec<Group>,
}

impl Cosmos {
    pub(super) fn plan(&self, width: usize, height: usize) -> Plan {
        let (w, h) = (width as f64, height as f64);
        let galaxies = self.layout(w, h);
        let clusters = sky::clusters(&self.universe, &galaxies, w, h);
        let stars: Vec<(f64, f64)> = (0..self.universe.stars.len())
            .map(|s| self.place_star(galaxies[self.universe.stars[s].maker], s, AT_UNIVERSE, None))
            .collect();
        let rooms: Vec<(f64, f64)> = galaxies
            .iter()
            .zip(&self.shapes.galaxies)
            .map(|(frame, shape)| badge_room(frame, shape))
            .collect();
        let badges = badges(&galaxies, &rooms, w, h);
        let home = shapes::home_radius(w, h);
        let moving: Vec<usize> = (0..self.universe.stars.len())
            .filter(|s| self.star_moves(*s))
            .collect();
        let things: Vec<(Bounds, u32)> = moving
            .iter()
            .map(|s| (self.star_bounds(&galaxies, *s), self.star_steps(*s)))
            .collect();
        let groups = motion::groups(&things)
            .into_iter()
            .map(|mut group| {
                group.members = group.members.iter().map(|m| moving[*m]).collect();
                group
            })
            .collect();
        Plan {
            width: w,
            height: h,
            galaxies,
            clusters,
            stars,
            badges,
            home,
            moving,
            groups,
        }
    }

    /// The box a moving star stays inside, its glow and its whole way round included.
    fn star_bounds(&self, galaxies: &[Frame], star: usize) -> Bounds {
        let s = &self.universe.stars[star];
        let rc = core_radius(s.actions) * AT_UNIVERSE;
        let reach = match s.kind {
            StarKind::Star => rc * 4.6 * 1.16,
            StarKind::RedGiant => rc * GIANT * 3.2 * 1.16,
            StarKind::Protostar => (rc * 4.2 * std::f64::consts::SQRT_2).max(rc * 2.2),
            StarKind::WhiteDwarf => rc * 3.2 * 1.15,
            StarKind::BlackHole => rc * 3.6,
        } + 2.0;
        let frame = galaxies[s.maker];
        match self.shapes.stars[star].companion_of {
            Some(big) => {
                let tilt = self.shapes.galaxies[s.maker].tilt;
                let base = self.shapes.stars[big];
                let centre = frame.point(tilt, base.rad, base.th);
                let distance = self.orbit_distance(big, AT_UNIVERSE);
                Bounds::around(centre, distance + reach, distance * tilt + reach)
            }
            None => Bounds::around(
                self.place_star(frame, star, AT_UNIVERSE, None),
                reach,
                reach,
            ),
        }
    }

    /// Draws one star as the universe shows it, its thin rim over it, at `t` or still.
    fn draw_star(&mut self, canvas: &mut Canvas, star: usize, at: (f64, f64), t: Option<f64>) {
        let pulse = self.breath(star, t);
        let spin = t.map_or(0.0, |t| t * TAU / CLOUD_SECONDS);
        let s = &self.universe.stars[star];
        let days = self.universe.days;
        paint::universe_star(
            &mut self.paint,
            canvas,
            s,
            s.model,
            at,
            AT_UNIVERSE,
            pulse,
            spin,
            days,
        );
        if matches!(s.kind, StarKind::Star | StarKind::RedGiant) {
            let swell = if s.kind == StarKind::RedGiant {
                GIANT
            } else {
                1.0
            };
            let rr = core_radius(s.actions) * AT_UNIVERSE * swell;
            let mut rim = Path::new();
            rim.circle(at.0, at.1, rr * 0.5 + 1.2);
            canvas.stroke(&rim, &Pen::new(1.1), &Brush::new(ink(rgb(s.rgb), 0.85)));
        }
    }

    /// Everything of the universe that holds still, or with `still` everything at its still
    /// pose, over the sky.
    /// `bare` leaves the sky out, for the zoom to lay over the sky it fades.
    pub(super) fn universe_picture(&mut self, plan: &Plan, still: bool, bare: bool) -> Image {
        let (width, height) = (plan.width as usize, plan.height as usize);
        let mut canvas = if bare {
            Canvas::new(width as u32, height as u32)
        } else {
            self.sky_canvas(width, height, None, false).clone()
        };
        let (cx, cy) = (plan.width / 2.0, plan.height / 2.0);
        // The ring of your whole mix: a galaxy on it does what you do overall.
        let mut ring = Path::new();
        ring.ellipse(
            cx,
            cy,
            430.0 * 0.55 * plan.width / DRAWN.0,
            190.0 * 0.55 * plan.height / DRAWN.1,
            0.0,
            0.0,
            TAU,
            false,
        );
        let faint = Brush::new(crate::world::raster::Colour::rgba(200, 211, 245, 0.16));
        canvas.stroke(&ring, &Pen::new(1.0).dash(&[2.0, 7.0]), &faint);
        let add = Blend::Lighter;
        for (maker, frame) in plan.galaxies.iter().enumerate() {
            let tilt = self.shapes.galaxies[maker].tilt;
            let disc = self.disc_picture(maker, frame.r, 1);
            let size = f64::from(disc.width());
            let placed = crate::world::raster::Transform::IDENTITY
                .translate(frame.x, frame.y)
                .rotate(frame.turn)
                .scale(1.0, tilt);
            let brush = Brush::default().blend(add).at(placed);
            canvas.draw_sized(&disc, -size / 2.0, -size / 2.0, size, size, &brush);
            let steps = maker_steps(&self.universe.makers[maker].key);
            for point in &self.shapes.galaxies[maker].points {
                let (x, y) = frame.point(tilt, point.rad, point.th);
                let star = &self.universe.stars[point.star];
                let colour = match steps {
                    Some(steps) => rgb(step_colour(steps, point.rad)),
                    None => mixc(rgb(star.rgb), WHITE, 0.25 * (1.0 - point.rad)),
                };
                let alpha = 0.95 * point.bright * if point.fresh { 1.0 } else { 0.5 };
                let side = if point.fresh { 2.3 } else { 1.6 };
                canvas.fill_rect(
                    x - side / 2.0,
                    y - side / 2.0,
                    side,
                    side,
                    &Brush::new(ink(colour, alpha)).blend(add),
                );
            }
        }
        self.draw_streams(&mut canvas, plan);
        let still_stars: Vec<usize> = (0..self.universe.stars.len())
            .filter(|s| still || !plan.moving.contains(s))
            .collect();
        for &star in &still_stars {
            self.draw_star(&mut canvas, star, plan.stars[star], None);
        }
        if still {
            for comet in 0..self.universe.comets.len() {
                self.draw_comet(&mut canvas, plan, comet, None, (0.0, 0.0));
            }
            for meteor in 0..self.universe.meteors.len() {
                self.draw_meteor(&mut canvas, plan, meteor, None, (0.0, 0.0));
            }
        }
        for (maker, at) in plan.badges.iter().enumerate() {
            let m = &self.universe.makers[maker];
            let (logo, colour) = (m.logo.clone(), rgb(m.rgb));
            self.paint.badge(&mut canvas, &logo, colour, *at, 1.0);
        }
        canvas.to_image()
    }

    /// Tidal streams: two makers' models that share runs more than chance, a faint stream of
    /// light between their stars.
    fn draw_streams(&mut self, canvas: &mut Canvas, plan: &Plan) {
        for stream in &self.universe.streams {
            let [a, b] = stream.stars;
            let (from, to) = (plan.stars[a], plan.stars[b]);
            let control = stream_bend(from, to);
            let (ca, cb) = (
                rgb(self.universe.stars[a].rgb),
                rgb(self.universe.stars[b].rgb),
            );
            let gradient = Gradient::linear(from.0, from.1, to.0, to.1)
                .stop(0.0, ink(ca, 0.05))
                .stop(1.0, ink(cb, 0.05));
            let mut path = Path::new();
            path.move_to(from.0, from.1);
            path.quad_to(control.0, control.1, to.0, to.1);
            canvas.stroke(
                &path,
                &Pen::new(6.0).cap(Cap::Round),
                &Brush::new(gradient).blend(Blend::Lighter),
            );
            let models = (self.universe.stars[a].model, self.universe.stars[b].model);
            let mut rng = rng_for("stream", (models.0 * 13 + models.1) as u32);
            for _ in 0..60 {
                let u = rng.next_f64();
                let (x, y) = bezier([from, control, to], u);
                let (x, y) = (x + rng.normal() * 2.4, y + rng.normal() * 2.4);
                let brush = Brush::new(ink(mixc(ca, cb, u), 0.16)).blend(Blend::Lighter);
                canvas.fill_rect(x, y, 1.2, 1.2, &brush);
            }
        }
    }

    /// A comet's way: from the galaxy of the maker that started the run to the one that joined,
    /// bowed away from your planet so it never crosses it.
    fn comet_path(&self, plan: &Plan, comet: usize) -> [(f64, f64); 3] {
        let c = self.universe.comets[comet];
        let (a, b) = (plan.galaxies[c.from], plan.galaxies[c.to]);
        let (mx, my) = ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let (ox, oy) = (mx - plan.width / 2.0, my - plan.height / 2.0);
        let length = ox.hypot(oy);
        let length = if length > 0.0 { length } else { 1.0 };
        let bow = 70.0 + 0.15 * (b.x - a.x).hypot(b.y - a.y);
        [
            (a.x, a.y),
            (mx + ox / length * bow, my + oy / length * bow),
            (b.x, b.y),
        ]
    }

    /// How far along its way a comet's head is at `t`, or `None` while it rests: 5 s across,
    /// then 3 s at rest. Still, it stays part way.
    fn comet_travel(&self, comet: usize, t: Option<f64>) -> Option<f64> {
        let cycle = rules::COMET_TRAVEL + rules::COMET_REST;
        let phase = match t {
            Some(t) => ((t / cycle + self.universe.comets[comet].phase).fract()) * cycle,
            None => 0.55 * rules::COMET_TRAVEL,
        };
        (phase <= rules::COMET_TRAVEL).then(|| phase / rules::COMET_TRAVEL)
    }

    /// Draws a comet at `t`, or still, moved by `offset`; gives where its head is.
    fn draw_comet(
        &mut self,
        canvas: &mut Canvas,
        plan: &Plan,
        comet: usize,
        t: Option<f64>,
        offset: (f64, f64),
    ) -> Option<(f64, f64)> {
        let u = self.comet_travel(comet, t)?;
        let path = self
            .comet_path(plan, comet)
            .map(|(x, y)| (x - offset.0, y - offset.1));
        let eased = smooth(0.0, 1.0, u);
        let fade = smooth(0.0, 0.12, u) * smooth(1.0, 0.85, u);
        let head = bezier(path, 0.08 + 0.84 * eased);
        let from = rgb(self.universe.makers[self.universe.comets[comet].from].rgb);
        let tail = mixc(from, [200.0, 230.0, 255.0], 0.45);
        // The tail trails back along its way, fading.
        for k in (1..=22).rev() {
            let k = f64::from(k);
            let v = (0.08 + 0.84 * eased - k * 0.011).max(0.0);
            let (end, start) = (bezier(path, v), bezier(path, (v - 0.011).max(0.0)));
            let mut segment = Path::new();
            segment.move_to(start.0, start.1);
            segment.line_to(end.0, end.1);
            let pen = Pen::new(4.2 * (1.0 - k / 24.0)).cap(Cap::Round);
            canvas.stroke(
                &segment,
                &pen,
                &Brush::new(ink(tail, 0.42 * fade * (1.0 - k / 23.0))).blend(Blend::Lighter),
            );
        }
        self.paint.draw_glow(
            canvas,
            [215.0, 235.0, 255.0],
            head,
            13.0,
            0.95 * fade,
            0.4,
            Blend::Lighter,
        );
        let mut dot = Path::new();
        dot.circle(head.0, head.1, 1.6);
        canvas.fill(&dot, &Brush::new(ink(WHITE, fade)).blend(Blend::Lighter));
        Some((head.0 + offset.0, head.1 + offset.1))
    }

    /// How far a meteor has fallen at `t`, or `None` between its falls: the last 10 minutes
    /// replayed in 10 s, each action 1.1 s from its maker's galaxy to your world. Still, it is a
    /// streak near your world.
    fn meteor_fall(&self, meteor: usize, t: Option<f64>) -> Option<f64> {
        let Some(t) = t else {
            return Some(0.86);
        };
        let age_minutes = self.universe.meteors[meteor].age / 60.0;
        let start =
            (rules::METEOR_MINUTES - age_minutes) / rules::METEOR_MINUTES * rules::METEOR_LOOP;
        let u = (t - start).rem_euclid(rules::METEOR_LOOP) / rules::METEOR_TRAVEL;
        (u <= 1.0).then_some(u)
    }

    fn meteor_line(&self, plan: &Plan, meteor: usize, u: f64) -> ((f64, f64), (f64, f64), Rgb) {
        let star = &self.universe.stars[self.universe.meteors[meteor].star];
        let from = plan.galaxies[star.maker];
        let (cx, cy) = (plan.width / 2.0, plan.height / 2.0);
        let (dx, dy) = (cx - from.x, cy - from.y);
        let length = dx.hypot(dy).max(1e-9);
        let (ux, uy) = (dx / length, dy / length);
        let end = (cx - ux * (plan.home + 2.0), cy - uy * (plan.home + 2.0));
        let eased = u * u;
        let head = (
            from.x + (end.0 - from.x) * eased,
            from.y + (end.1 - from.y) * eased,
        );
        let tail = 26.0 + 30.0 * eased;
        (
            (head.0 - ux * tail, head.1 - uy * tail),
            head,
            rgb(star.rgb),
        )
    }

    fn draw_meteor(
        &mut self,
        canvas: &mut Canvas,
        plan: &Plan,
        meteor: usize,
        t: Option<f64>,
        offset: (f64, f64),
    ) {
        let Some(u) = self.meteor_fall(meteor, t) else {
            return;
        };
        let (tail, head, colour) = self.meteor_line(plan, meteor, u);
        let (tail, head) = (
            (tail.0 - offset.0, tail.1 - offset.1),
            (head.0 - offset.0, head.1 - offset.1),
        );
        let gradient = Gradient::linear(tail.0, tail.1, head.0, head.1)
            .stop(0.0, ink(colour, 0.0))
            .stop(1.0, ink(mixc(colour, WHITE, 0.6), 0.9));
        let mut streak = Path::new();
        streak.move_to(tail.0, tail.1);
        streak.line_to(head.0, head.1);
        canvas.stroke(
            &streak,
            &Pen::new(1.6).cap(Cap::Round),
            &Brush::new(gradient).blend(Blend::Lighter),
        );
        if u > 0.9 && t.is_some() {
            let (_, end, _) = self.meteor_line(plan, meteor, 1.0);
            let end = (end.0 - offset.0, end.1 - offset.1);
            self.paint.draw_glow(
                canvas,
                mixc(colour, WHITE, 0.5),
                end,
                7.0,
                (u - 0.9) * 8.0,
                0.4,
                Blend::Lighter,
            );
        }
    }

    /// The box round a meteor's streak at `u`, its landing glow included.
    fn meteor_bounds(&self, plan: &Plan, meteor: usize, u: f64) -> Bounds {
        let (tail, head, _) = self.meteor_line(plan, meteor, u);
        let (_, end, _) = self.meteor_line(plan, meteor, 1.0);
        let mut bounds = Bounds::around(tail, 2.0, 2.0).union(Bounds::around(head, 2.0, 2.0));
        if u > 0.9 {
            bounds = bounds.union(Bounds::around(end, 8.0, 8.0));
        }
        bounds
    }

    /// The universe level's scene at `t`.
    pub(super) fn universe_scene(&mut self, view: &View, t: f64, sky: bool) -> Scene {
        let (width, height) = (view.width.max(1), view.height.max(1));
        let plan = self.plan(width, height);
        let mut scene = Scene::new(width, height);
        let key = (Level::Universe, width, height);
        let mut kept = self.layers.remove(&key).unwrap_or_default();
        let mut image = match kept.slot(view.still, sky).clone() {
            Some(image) => image,
            None => self.universe_picture(&plan, view.still, !sky),
        };
        *kept.slot(view.still, sky) = Some(image.clone());
        self.layers.insert(key, kept);
        let mut layer_key = self.key([
            1,
            u64::from(view.still),
            u64::from(sky),
            width as u64,
            height as u64,
        ]);
        if let Some(home) = &view.home {
            let at = (
                (plan.width / 2.0 - home.image.width() as f64 / 2.0).round() as i32,
                (plan.height / 2.0 - home.image.height() as f64 / 2.0).round() as i32,
            );
            scene.add_sprite(Sprite {
                id: PartId::new("cosmos-home", 0),
                depth: Depth::Ground,
                at,
                picture: home.clone(),
            });
        }
        let step = motion::step_of(t);
        let mut star_places = plan.stars.clone();
        if !view.still {
            for group in &plan.groups {
                let looped = motion::looped(group, t);
                for &star in &group.members {
                    let frame = plan.galaxies[self.universe.stars[star].maker];
                    star_places[star] = self.place_star(frame, star, AT_UNIVERSE, Some(looped));
                }
            }
            let galaxies = plan.galaxies.clone();
            let draw = |cosmos: &mut Cosmos,
                        canvas: &mut Canvas,
                        star: usize,
                        (left, top): (f64, f64),
                        looped: f64| {
                let frame = galaxies[cosmos.universe.stars[star].maker];
                let (x, y) = cosmos.place_star(frame, star, AT_UNIVERSE, Some(looped));
                cosmos.draw_star(canvas, star, (x - left, y - top), Some(looped));
            };
            if sky {
                self.add_groups(&mut scene, &image, &plan.groups, t, 2, draw);
            } else {
                image = self.draw_groups(&image, &plan.groups, t, draw);
                layer_key = self.key([layer_key, step]);
            }
        }
        if !view.still {
            self.add_galaxy_motion(&mut scene, &plan.galaxies, t);
        }
        scene.add_layer(Layer {
            id: PartId::new("cosmos-universe", 0),
            depth: Depth::Ground,
            picture: Picture::new(layer_key, image),
        });
        let mut comet_heads: Vec<(usize, (f64, f64))> = Vec::new();
        if !view.still {
            let cycle = ((rules::COMET_TRAVEL + rules::COMET_REST) * motion::TICKS).round() as u64;
            let frame = step % cycle;
            let looped = frame as f64 / motion::TICKS;
            for comet in 0..self.universe.comets.len() {
                let Some(u) = self.comet_travel(comet, Some(looped)) else {
                    continue;
                };
                let path = self.comet_path(&plan, comet);
                let eased = smooth(0.0, 1.0, u);
                let reach = 0.08 + 0.84 * eased;
                let mut bounds = Bounds::around(bezier(path, reach), 14.0, 14.0);
                for k in 1..=23 {
                    bounds = bounds.union(Bounds::around(
                        bezier(path, (reach - f64::from(k) * 0.011).max(0.0)),
                        3.0,
                        3.0,
                    ));
                }
                let Some((left, top, w, h)) = bounds.pixels(width, height) else {
                    continue;
                };
                let key = self.key([3, comet as u64, frame, width as u64, height as u64]);
                let offset = (f64::from(left), f64::from(top));
                let picture = self.frame(key, |cosmos| {
                    let mut canvas = Canvas::new(w, h);
                    cosmos.draw_comet(&mut canvas, &plan, comet, Some(looped), offset);
                    light_only(canvas)
                });
                if let Some(head) = self.comet_head(&plan, comet, Some(looped)) {
                    comet_heads.push((comet, head));
                }
                scene.add_sprite(Sprite {
                    id: PartId::new("cosmos-comet", comet as u32),
                    depth: Depth::Dots,
                    at: (left as i32, top as i32),
                    picture,
                });
            }
            let cycle = (rules::METEOR_LOOP * motion::TICKS).round() as u64;
            let frame = step % cycle;
            let looped = frame as f64 / motion::TICKS;
            for meteor in 0..self.universe.meteors.len() {
                let Some(u) = self.meteor_fall(meteor, Some(looped)) else {
                    continue;
                };
                let Some((left, top, w, h)) =
                    self.meteor_bounds(&plan, meteor, u).pixels(width, height)
                else {
                    continue;
                };
                let key = self.key([4, meteor as u64, frame, width as u64, height as u64]);
                let offset = (f64::from(left), f64::from(top));
                let picture = self.frame(key, |cosmos| {
                    let mut canvas = Canvas::new(w, h);
                    cosmos.draw_meteor(&mut canvas, &plan, meteor, Some(looped), offset);
                    light_only(canvas)
                });
                scene.add_sprite(Sprite {
                    id: PartId::new("cosmos-meteor", meteor as u32),
                    depth: Depth::Dots,
                    at: (left as i32, top as i32),
                    picture,
                });
            }
        } else {
            for comet in 0..self.universe.comets.len() {
                if let Some(head) = self.comet_head(&plan, comet, None) {
                    comet_heads.push((comet, head));
                }
            }
        }
        scene.hits = self.universe_hits(&plan, &star_places, &comet_heads);
        if let Some(ringed) = view.ringed
            && let Some((at, radius)) =
                self.universe_ring(&plan, &star_places, &comet_heads, ringed)
        {
            add_ring(&mut scene, at, radius);
        }
        scene.moves = !view.still && self.universe.moves();
        scene
    }

    fn comet_head(&self, plan: &Plan, comet: usize, t: Option<f64>) -> Option<(f64, f64)> {
        let u = self.comet_travel(comet, t)?;
        Some(bezier(
            self.comet_path(plan, comet),
            0.08 + 0.84 * smooth(0.0, 1.0, u),
        ))
    }

    /// The radius the pointer finds a star within at the universe level.
    fn star_reach(&self, star: usize) -> f64 {
        let s = &self.universe.stars[star];
        let rc = core_radius(s.actions) * AT_UNIVERSE;
        let drawn = match s.kind {
            StarKind::Protostar => rc * 2.0,
            StarKind::WhiteDwarf => {
                let came = s.newer.map_or(0, |(_, came)| came);
                rc * (1.4
                    + 1.8
                        * (f64::from(came) / f64::from(self.universe.days.max(1))).clamp(0.0, 1.0))
            }
            StarKind::BlackHole => rc * 1.6,
            StarKind::Star => rc,
            StarKind::RedGiant => rc * GIANT,
        };
        (drawn * 0.8).max(5.0)
    }

    /// What the pointer finds in the universe: a comet first, then a star, a galaxy's badge,
    /// your planet, the nearest action, a galaxy's disc, a tidal stream, an earlier month, and
    /// the far stars behind it all.
    fn universe_hits(
        &self,
        plan: &Plan,
        stars: &[(f64, f64)],
        comets: &[(usize, (f64, f64))],
    ) -> HitMap {
        let mut hits = HitMap::new();
        let index = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        let disc = |(x, y): (f64, f64), radius: f64| Shape::Disc {
            x: x as f32,
            y: y as f32,
            radius: radius as f32,
        };
        for (comet, head) in comets {
            if let Some(body) = self
                .universe
                .body_index(super::model::Body::Comet { comet: *comet })
            {
                hits.add(Mark::new(disc(*head, 9.0), Pointed::Body(body)));
            }
        }
        for (star, at) in stars.iter().enumerate() {
            let mark = Mark::new(disc(*at, self.star_reach(star)), Pointed::Star(index(star)));
            hits.add(mark.reach(2.0).tier(1));
        }
        for (maker, at) in plan.badges.iter().enumerate() {
            hits.add(Mark::new(disc(*at, 19.0), Pointed::Galaxy(index(maker))).tier(2));
        }
        let centre = (plan.width / 2.0, plan.height / 2.0);
        hits.add(
            Mark::new(disc(centre, plan.home), Pointed::Home)
                .reach(4.0)
                .tier(3),
        );
        for (maker, frame) in plan.galaxies.iter().enumerate() {
            let shape = &self.shapes.galaxies[maker];
            let galaxy = Pointed::Galaxy(index(maker));
            // A galaxy laid along its logo is found by its shape, its core and its points.
            let along = shape.logo.map(|_| (frame.r * 0.12).max(6.0) as f32);
            for point in &shape.points {
                let (x, y) = frame.point(shape.tilt, point.rad, point.th);
                let at = Shape::Point {
                    x: x as f32,
                    y: y as f32,
                };
                let action = self.universe.actions[point.action].index;
                hits.add(
                    Mark::new(at.clone(), Pointed::Action(action))
                        .reach(4.5)
                        .tier(4),
                );
                if let Some(reach) = along {
                    hits.add(Mark::new(at, galaxy).reach(reach).tier(5));
                }
            }
            let outline = match along {
                Some(_) => disc((frame.x, frame.y), frame.r * 0.25),
                None => Shape::Ellipse {
                    x: frame.x as f32,
                    y: frame.y as f32,
                    rx: (frame.r * 0.9) as f32,
                    ry: (frame.r * 0.9 * shape.tilt) as f32,
                    angle: frame.turn as f32,
                },
            };
            hits.add(Mark::new(outline, galaxy).tier(5));
        }
        for (at, stream) in self.universe.streams.iter().enumerate() {
            let [a, b] = stream.stars;
            let (from, to) = (stars[a], stars[b]);
            let control = stream_bend(from, to);
            let points = (0..=40)
                .map(|k| {
                    let (x, y) = bezier([from, control, to], f64::from(k) / 40.0);
                    (x as f32, y as f32)
                })
                .collect();
            if let Some(body) = self
                .universe
                .body_index(super::model::Body::Stream { stream: at })
            {
                hits.add(
                    Mark::new(Shape::Path { points, width: 0.0 }, Pointed::Body(body))
                        .reach(6.0)
                        .tier(6),
                );
            }
        }
        for cluster in &plan.clusters {
            if let Some((x, y, rx, ry, angle)) = cluster.outline() {
                let shape = Shape::Ellipse {
                    x: x as f32,
                    y: y as f32,
                    rx: rx as f32,
                    ry: ry as f32,
                    angle: angle as f32,
                };
                hits.add(Mark::new(shape, Pointed::Sky(Some(index(cluster.back)))).tier(7));
            }
        }
        hits.set_under(Under::Sky);
        hits
    }

    /// Where the ring round what the pointer found goes, and how wide.
    fn universe_ring(
        &self,
        plan: &Plan,
        stars: &[(f64, f64)],
        comets: &[(usize, (f64, f64))],
        ringed: Pointed,
    ) -> Option<((f64, f64), f64)> {
        let at = |index: u32| usize::try_from(index).ok();
        match ringed {
            Pointed::Star(star) => {
                let star = at(star)?;
                Some((*stars.get(star)?, self.star_reach(star) + 4.0))
            }
            Pointed::Home => Some(((plan.width / 2.0, plan.height / 2.0), plan.home + 4.0)),
            Pointed::Body(body) => match self.universe.body(body)? {
                super::model::Body::Comet { comet } => {
                    let (_, head) = comets.iter().find(|(c, _)| *c == comet)?;
                    Some((*head, 13.0))
                }
                _ => None,
            },
            Pointed::Action(action) => {
                for (maker, frame) in plan.galaxies.iter().enumerate() {
                    let shape = &self.shapes.galaxies[maker];
                    if let Some(point) = shape
                        .points
                        .iter()
                        .find(|p| self.universe.actions[p.action].index == action)
                    {
                        return Some((frame.point(shape.tilt, point.rad, point.th), 5.0));
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// A galaxy's disc picture for a frame of radius `radius`, drawn at 1/`down` of its size,
    /// kept.
    pub(super) fn disc_picture(&mut self, maker: usize, radius: f64, down: u32) -> Canvas {
        let key = self.key([5, maker as u64, radius.round() as u64, u64::from(down)]);
        let image = self.frames.get(&key).cloned().unwrap_or_else(|| {
            let m = &self.universe.makers[maker];
            let seed = (rng_for(&m.key, 3).next_f64() * 97.0).floor();
            let strength = (0.42 + 0.03 * (m.share * shapes::SHARE_SCALE).sqrt()).min(0.95);
            let disc = super::disc::Disc {
                points: &self.shapes.galaxies[maker].points,
                colour: m.rgb,
                dwarf: m.dwarf,
                strength,
                seed,
                logo: self.shapes.galaxies[maker].logo,
                steps: maker_steps(&m.key),
            };
            let image = disc.picture(radius.round(), down).to_image();
            self.frames.insert(key, image.clone());
            image
        });
        Canvas::from_image(&image).unwrap_or_else(|| Canvas::new(1, 1))
    }
}

/// The ring round what the pointer found, as a sprite of its own.
pub(super) fn add_ring(scene: &mut Scene, (x, y): (f64, f64), radius: f64) {
    let side = (radius * 2.0 + 6.0).ceil();
    let mut canvas = Canvas::new(side as u32, side as u32);
    let left = (x - side / 2.0).floor();
    let top = (y - side / 2.0).floor();
    paint::ring(&mut canvas, (x - left, y - top), radius, 0.9);
    let key = crate::world::picture_key([
        6,
        radius.to_bits(),
        (x - left).to_bits(),
        (y - top).to_bits(),
    ]);
    scene.add_sprite(Sprite {
        id: PartId::new("cosmos-ring", 0),
        depth: Depth::Pointer,
        at: (left as i32, top as i32),
        picture: Picture::new(key, canvas.to_image()),
    });
}

/// A point `u` of the way along a quadratic curve.
pub(super) fn bezier([a, c, b]: [(f64, f64); 3], u: f64) -> (f64, f64) {
    let v = 1.0 - u;
    (
        v * v * a.0 + 2.0 * v * u * c.0 + u * u * b.0,
        v * v * a.1 + 2.0 * v * u * c.1 + u * u * b.1,
    )
}

/// The control point a tidal stream bends through, a little to one side of the straight way.
fn stream_bend(from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let (mx, my) = ((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0);
    let (nx, ny) = (-(to.1 - from.1), to.0 - from.0);
    (mx + nx * 0.18, my + ny * 0.18)
}

/// Half the width and height a galaxy's badge keeps clear of: all its points where it lies along
/// its logo, so Anthropic's goes below its longest rays, else most of its disc.
fn badge_room(frame: &Frame, shape: &shapes::GalaxyShape) -> (f64, f64) {
    if shape.logo.is_some() {
        return shape.points.iter().fold((0.0, 0.0), |(w, h), p| {
            let (x, y) = frame.point(shape.tilt, p.rad, p.th);
            (w.max((x - frame.x).abs()), h.max((y - frame.y).abs()))
        });
    }
    let (sin, cos) = frame.turn.sin_cos();
    (
        (0.72 * frame.r * cos.hypot(shape.tilt * sin)).min(64.0),
        (0.72 * frame.r * sin.hypot(shape.tilt * cos)).min(52.0),
    )
}

/// Where each galaxy's badge goes: below its galaxy where there is room, else beside it, clear of
/// the other galaxies, the other badges and your planet.
fn badges(galaxies: &[Frame], rooms: &[(f64, f64)], width: f64, height: f64) -> Vec<(f64, f64)> {
    let mut placed: Vec<(f64, f64)> = Vec::new();
    let home = shapes::HOME_RADIUS;
    for (at, (galaxy, &(half_width, half_height))) in galaxies.iter().zip(rooms).enumerate() {
        let mut best = None;
        let mut best_score = f64::NEG_INFINITY;
        let directions = [
            (0.0, 1.0),
            (0.0, -1.0),
            (1.0, 0.0),
            (-1.0, 0.0),
            (0.75, 0.75),
            (-0.75, 0.75),
            (0.75, -0.75),
            (-0.75, -0.75),
        ];
        for (dx, dy) in directions {
            let x = galaxy.x + dx * (half_width + 26.0);
            let y = galaxy.y + dy * (half_height + 26.0);
            if x < 22.0 || y < 22.0 || x > width - 22.0 || y > height - 22.0 {
                continue;
            }
            let mut score = -dy.abs() * 0.5 - dx.abs() * 2.0;
            for (other_at, other) in galaxies.iter().enumerate() {
                if other_at == at {
                    continue;
                }
                let d = ((x - other.x) / (other.r * 0.75 + 20.0))
                    .hypot((y - other.y) / (other.r * 0.5 + 20.0));
                if d < 1.3 {
                    score -= (1.3 - d) * 40.0;
                }
            }
            for p in &placed {
                if (x - p.0).hypot(y - p.1) < 44.0 {
                    score -= 60.0;
                }
            }
            if (x - width / 2.0).hypot(y - height / 2.0) < home + 30.0 {
                score -= 50.0;
            }
            if score > best_score {
                best_score = score;
                best = Some((x, y));
            }
        }
        let spot = best.unwrap_or((
            galaxy.x,
            (galaxy.y + half_height + 24.0).clamp(22.0, height - 22.0),
        ));
        placed.push(spot);
    }
    placed
}
