//! The dots on the land, one an action, each standing on the ground under it with soft shade
//! at its foot so it reads as resting on the slope; far dots first, so a nearer one is on top.
//! A dot a nearer slope stands in front of is drawn through it as a dashed ring, so none is ever
//! lost, and the pointer still finds it.

use super::{FORESHORTEN, Relief, top_row};
use crate::base::draw::scene::{Mark, Pointed, Shape};
use crate::world::map::fields::{Ground, HEIGHT, Rgb, WIDTH};
use crate::world::raster::{Brush, Canvas, Colour, Path, Pen};

/// A dot's radius on the map at its usual size, in pixels: 0.82 of what it was when the land
/// filled the frame, as the land is now 0.74 of it.
const DOT: f64 = 0.82 * 1.6;
/// The rim round every dot, a compared dot's white middle, and the others' grey.
const EDGE: Rgb = [20.0, 20.0, 30.0];
const WHITE: Rgb = [246.0, 243.0, 236.0];
const GREY: Rgb = [150.0, 152.0, 170.0];

/// How the dots are coloured: each group's colour, and the group compared, if one is.
#[derive(Debug, Clone, Copy)]
pub struct DotStyle<'a> {
    pub groups: &'a [Rgb],
    pub compared: Option<usize>,
}

/// One dot's look: its colour, radius and strength, and a compared dot's ring in its group's
/// colour.
struct Look {
    colour: Rgb,
    radius: f64,
    alpha: f64,
    ring: Option<Rgb>,
}

impl DotStyle<'_> {
    fn look(&self, group: usize) -> Look {
        let own = self.groups.get(group).copied().unwrap_or(GREY);
        match self.compared {
            None => Look {
                colour: own,
                radius: 2.7 * DOT,
                alpha: 1.0,
                ring: None,
            },
            Some(compared) if compared == group => Look {
                colour: WHITE,
                radius: 2.4 * DOT,
                alpha: 1.0,
                ring: Some(own),
            },
            Some(_) => Look {
                colour: GREY,
                radius: 2.2 * DOT,
                alpha: 0.45,
                ring: None,
            },
        }
    }
}

/// The dots drawn for one height of the land, where each is drawn, and which a slope hides.
#[derive(Debug, Clone)]
pub struct Dots {
    pub stand: f64,
    pub picture: Canvas,
    /// Each dot's pixel on the screen, by its action's index.
    pub at: Vec<(f32, f32)>,
    pub hidden: Vec<bool>,
}

impl Dots {
    pub fn new(ground: &Ground, relief: &Relief, style: DotStyle<'_>, stand: f64) -> Self {
        let at: Vec<(f32, f32)> = ground
            .dots
            .iter()
            .map(|&(x, y)| {
                let (sx, sy) = relief.screen(f64::from(x), f64::from(y), stand, 0.0);
                (sx as f32, sy as f32)
            })
            .collect();
        let hidden: Vec<bool> = ground
            .dots
            .iter()
            .zip(&at)
            .map(|(&(x, y), &(_, sy))| stand > 0.0 && behind_a_slope(relief, x, y, sy, stand))
            .collect();
        let groups: Vec<usize> = ground.facts.dots.iter().map(|dot| dot.group).collect();
        let mut order: Vec<usize> = (0..at.len()).collect();
        let lifted = |i: usize| style.compared == Some(groups[i]);
        order.sort_by(|&p, &q| {
            lifted(p).cmp(&lifted(q)).then(
                at[p]
                    .1
                    .partial_cmp(&at[q].1)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });

        let mut picture = Canvas::new(WIDTH as u32, HEIGHT as u32);
        let disc = |x: f64, y: f64, radius: f64| {
            let mut path = Path::new();
            path.circle(x, y, radius);
            path
        };
        for i in order {
            let look = style.look(groups[i]);
            let (x, y) = (f64::from(at[i].0), f64::from(at[i].1));
            let (r, a) = (look.radius, look.alpha);
            if hidden[i] {
                let ring = disc(x, y, r + 0.6);
                picture.fill(&ring, &Brush::new(Colour::of(look.colour, 0.22 * a)));
                picture.stroke(
                    &ring,
                    &Pen::new(1.4).dash(&[2.2, 1.6]),
                    &Brush::new(Colour::of(look.colour, 0.9 * a)),
                );
                continue;
            }
            let mut shade = Path::new();
            shade.ellipse(
                x + 1.6,
                y + 1.3,
                r + 1.6,
                (r + 1.6) * 0.7,
                0.0,
                0.0,
                7.0,
                false,
            );
            picture.fill(&shade, &Brush::new(Colour::rgba(6, 8, 14, 0.32 * a)));
            let rim = if look.ring.is_some() { 2.6 } else { 1.2 };
            picture.fill(&disc(x, y, r + rim), &Brush::new(Colour::of(EDGE, a)));
            if let Some(ring) = look.ring {
                picture.fill(&disc(x, y, r + 1.6), &Brush::new(Colour::of(ring, a)));
            }
            picture.fill(&disc(x, y, r), &Brush::new(Colour::of(look.colour, a)));
        }
        Self {
            stand,
            picture,
            at,
            hidden,
        }
    }

    /// How many dots are drawn through a slope as rings.
    pub fn hidden_count(&self) -> usize {
        self.hidden.iter().filter(|hidden| **hidden).count()
    }

    /// What the pointer finds of the dots: one right under it, within 6 px, at `near_tier`, and
    /// the nearest a little further off, within 14 px, at `far_tier`, which should still beat
    /// the bare ground but not what nature draws.
    pub fn marks(&self, near_tier: u8, far_tier: u8) -> Vec<Mark> {
        self.at
            .iter()
            .enumerate()
            .flat_map(|(index, &(x, y))| {
                let point = Shape::Point { x, y };
                let pointed = Pointed::Action(index as u32);
                [
                    Mark::new(point.clone(), pointed).reach(6.0).tier(near_tier),
                    Mark::new(point, pointed).reach(14.0).tier(far_tier),
                ]
            })
            .collect()
    }
}

/// Whether some place nearer you, down the dot's column, stands higher on the screen than it.
fn behind_a_slope(relief: &Relief, x: f32, y: f32, screen_y: f32, stand: f64) -> bool {
    let (x, from) = (f64::from(x), f64::from(y) + 2.0);
    let mut y = from;
    while y < HEIGHT as f64 {
        let nearer = top_row() + FORESHORTEN * y - stand * relief.standing_at(x, y);
        if nearer < f64::from(screen_y) - 1.0 {
            return true;
        }
        y += 1.0;
    }
    false
}
