//! What the pointer finds on the map's nature and weather, in the screen's pixels. What stands on
//! the land or flies over it answers first: storms, hurricanes, volcanoes, tornadoes, the
//! tsunami and the wonders. Then what lies on the ground: water, lakes, faults, atolls, the land's
//! kinds, the skirts of floods and burn scars, and the shoals. Nature names what it found by
//! its place in its list, the shoals after its features.

use super::tsunami::wave_bounds;
use super::{Weather, hurricane, thunder};
use crate::base::draw::scene::{Mark, Pointed, Shape};
use crate::world::map::fields::{COARSE, Ground, HEIGHT, WIDTH};
use crate::world::map::motion::Moment;
use crate::world::map::nature::{Kind, Nature, Wave, scars};
use crate::world::map::relief::FORESHORTEN;
use crate::world::wonders::Placed;

/// The tier of what stands or flies, which answers just after a dot right under the pointer.
pub const STANDING: u8 = 1;
/// The tier of what lies on the ground, which answers before a dot a little further off.
pub const LYING: u8 = 2;

/// Pixels a side of the patches a flood's or a scar's skirt is found in.
const SKIRT_PATCH: usize = 8;

/// A mark the pointer finds: one that keeps still, or the tsunami's, which rolls in with the
/// moment, by its place in nature's list.
#[derive(Debug, Clone)]
pub(super) enum Slot {
    Still(Mark),
    Rolling { index: usize, wave: Wave },
}

fn rect(left: f64, top: f64, right: f64, bottom: f64) -> Shape {
    Shape::Rect {
        left: left as f32,
        top: top as f32,
        right: right as f32,
        bottom: bottom as f32,
    }
}

impl Weather {
    /// What the pointer finds of nature, the weather and the wonders at a moment.
    pub fn marks(&self, moment: Moment) -> Vec<Mark> {
        self.marks
            .iter()
            .map(|slot| match slot {
                Slot::Still(mark) => mark.clone(),
                Slot::Rolling { index, wave } => {
                    let ([left, top, right, bottom], _) = wave_bounds(wave, self, moment);
                    let shape = rect(left, top, right, bottom);
                    Mark::new(shape, Pointed::Feature(*index as u32)).tier(STANDING)
                }
            })
            .collect()
    }

    /// Every mark the pointer finds of the nature, the weather and the wonders this weather
    /// was made from, worked out once, in the order they are found.
    pub(super) fn slots(&self, ground: &Ground, nature: &Nature, wonders: &[Placed]) -> Vec<Slot> {
        let mut marks = Vec::new();
        let feature_mark = |shape: Shape, index: usize, tier: u8| {
            Slot::Still(Mark::new(shape, Pointed::Feature(index as u32)).tier(tier))
        };
        for (index, feature) in nature.features.iter().enumerate() {
            let (x, y) = self.screen(feature.x, feature.y, 0.0);
            let lifted = match &feature.kind {
                Kind::Thunderstorm { .. } => {
                    let (fx, fy) = self.flight(index).unwrap_or((feature.x, feature.y));
                    let (sx, sy) = self.screen(fx, fy, thunder::ALTITUDE);
                    let middle = Shape::Point {
                        x: sx as f32,
                        y: (sy - feature.radius * 0.75) as f32,
                    };
                    Some(
                        Mark::new(middle, Pointed::Feature(index as u32))
                            .reach(feature.reach as f32),
                    )
                }
                Kind::Hurricane(_) => {
                    let (fx, fy) = self.flight(index).unwrap_or((feature.x, feature.y));
                    let (sx, sy) = self.screen(fx, fy, hurricane::ALTITUDE);
                    let eye = Shape::Point {
                        x: sx as f32,
                        y: sy as f32,
                    };
                    Some(Mark::new(eye, Pointed::Feature(index as u32)).reach(feature.reach as f32))
                }
                Kind::Volcano { smoking, .. } => {
                    let r = feature.radius;
                    let rise = if *smoking { 2.6 } else { 1.1 };
                    Some(Mark::new(
                        rect(x - r, y - r * rise, x + r, y + r * 0.4),
                        Pointed::Feature(index as u32),
                    ))
                }
                Kind::Tornado { .. } => {
                    let r = feature.radius;
                    let shape = rect(x - r, y - (r - 4.0) * 2.8, x + r, y + r * 0.5);
                    Some(Mark::new(shape, Pointed::Feature(index as u32)))
                }
                Kind::Tsunami(wave) => {
                    marks.push(Slot::Rolling { index, wave: *wave });
                    continue;
                }
                _ => None,
            };
            if let Some(mark) = lifted {
                marks.push(Slot::Still(mark.tier(STANDING)));
                continue;
            }
            if let Some(stream) = feature.stream() {
                let points = stream
                    .path
                    .iter()
                    .map(|&(px, py)| {
                        let (sx, sy) = self.screen(px, py, 0.0);
                        (sx as f32, sy as f32)
                    })
                    .collect();
                let width = (2.0 * (stream.width + 3.0)) as f32;
                marks.push(feature_mark(Shape::Path { points, width }, index, LYING));
            } else if feature.reach > 0.0 {
                let shape = Shape::Ellipse {
                    x: x as f32,
                    y: y as f32,
                    rx: feature.reach as f32,
                    ry: (feature.reach * FORESHORTEN) as f32,
                    angle: 0.0,
                };
                marks.push(feature_mark(shape, index, LYING));
            }
        }
        for (index, wonder) in wonders.iter().enumerate() {
            let b = self.wonder_box(wonder);
            let shape = rect(b.x0 - 2.0, b.y0 - 2.0, b.x1 + 2.0, b.y1 + 2.0);
            let mark = Mark::new(shape, Pointed::Wonder(index as u32)).tier(STANDING);
            marks.push(Slot::Still(mark));
        }
        marks.extend(self.skirts(ground, nature).into_iter().map(Slot::Still));
        let first_shoal = nature.features.len();
        for (k, shoal) in nature.shoals.iter().enumerate() {
            let (sx, sy) = self.screen(shoal.x, shoal.y, 0.0);
            let point = Shape::Point {
                x: sx as f32,
                y: sy as f32,
            };
            let pointed = Pointed::Feature((first_shoal + k) as u32);
            let mark = Mark::new(point, pointed).reach(14.0).tier(LYING);
            marks.push(Slot::Still(mark));
        }
        marks
    }

    /// The patches of the map where a flood or a burn scar lies on its mountain's skirt, each
    /// answering for it.
    fn skirts(&self, ground: &Ground, nature: &Nature) -> Vec<Mark> {
        let skirted: Vec<usize> = nature
            .features
            .iter()
            .enumerate()
            .filter(|(_, f)| matches!(f.kind, Kind::Flood { .. } | Kind::Wildfire { .. }))
            .map(|(index, _)| index)
            .collect();
        if skirted.is_empty() {
            return Vec::new();
        }
        let laid = scars(ground, nature);
        let mut marks = Vec::new();
        let half = SKIRT_PATCH as f64 / 2.0;
        for j in (0..HEIGHT).step_by(SKIRT_PATCH) {
            for i in (0..WIDTH).step_by(SKIRT_PATCH) {
                let (x, y) = (i as f64 + half, j as f64 + half);
                let at = ((y as usize).min(HEIGHT - 1) * WIDTH + (x as usize).min(WIDTH - 1)) * 4;
                if laid[at + 3] <= 60 {
                    continue;
                }
                let found = skirted.iter().copied().find(|&index| {
                    let feature = &nature.features[index];
                    if (feature.x - x).hypot(feature.y - y) >= 110.0 {
                        return false;
                    }
                    ground.mountains[feature.behavior]
                        .as_ref()
                        .is_some_and(|own| {
                            let top =
                                f64::from(own.values().iter().copied().fold(0.0_f32, f32::max));
                            let share = own.sample(x / COARSE as f64, y / COARSE as f64) / top;
                            share > 0.04 && share < 0.6
                        })
                });
                if let Some(index) = found {
                    let (left, top) = self.screen(x - half, y - half, 0.0);
                    let (right, bottom) = self.screen(x + half, y + half, 0.0);
                    let shape = Shape::Rect {
                        left: left as f32,
                        top: top.min(bottom) as f32,
                        right: right as f32,
                        bottom: top.max(bottom) as f32,
                    };
                    marks.push(Mark::new(shape, Pointed::Feature(index as u32)).tier(LYING));
                }
            }
        }
        marks
    }
}
