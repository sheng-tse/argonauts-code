//! The water's motion: light running down each river the way the agents moved, and streaks
//! running along each current's lanes; held still, a current's chevrons point its way and a
//! river rests.

use super::{Pad, Weather};
use crate::world::map::motion::{
    CURRENT, CURRENT_GAP, CURRENT_SPEED, Moment, RIVER, RIVER_GAP, RIVER_SPEED,
};
use crate::world::map::nature::{CurrentRun, Stream};
use crate::world::raster::noise::{hash3, mix, smooth};
use crate::world::raster::{Cap, Colour, Path, Pen};

/// A river's bed as the screen shows it.
#[derive(Debug, Clone)]
pub(super) struct OnScreen {
    points: Vec<(f64, f64)>,
    lengths: Vec<f64>,
    width: f64,
}

impl OnScreen {
    pub fn new(weather: &Weather, stream: &Stream) -> Self {
        let points: Vec<(f64, f64)> = stream
            .path
            .iter()
            .map(|&(x, y)| weather.screen(x, y, 0.0))
            .collect();
        let mut lengths = vec![0.0];
        for pair in points.windows(2) {
            let last = lengths[lengths.len() - 1];
            lengths.push(last + (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1));
        }
        Self {
            points,
            lengths,
            width: stream.width,
        }
    }

    /// The box its flecks fall in on the screen, as left, top, right and bottom.
    pub fn bounds(&self) -> [f64; 4] {
        spread(self.points.iter().copied(), 6.0 + self.width)
    }

    /// Short bright flecks running down the bed 12 px a second, 18 px apart, as wide as the
    /// water there and faded at both ends. A river held still shows none.
    pub fn flecks(&self, pad: &mut Pad, moment: Moment) {
        if moment.is_still() {
            return;
        }
        let total = self.lengths[self.lengths.len() - 1];
        if total < 12.0 {
            return;
        }
        let dash = 2.6;
        let mut s = (moment.time(RIVER) * RIVER_SPEED) % RIVER_GAP;
        let mut i = 1;
        while s < total {
            while i < self.lengths.len() - 1 && self.lengths[i] < s {
                i += 1;
            }
            let (a, b) = (self.points[i - 1], self.points[i]);
            let span = self.lengths[i] - self.lengths[i - 1];
            let span = if span == 0.0 { 1.0 } else { span };
            let u = (s - self.lengths[i - 1]) / span;
            let (x, y) = (mix(a.0, b.0, u), mix(a.1, b.1, u));
            let (dx, dy) = ((b.0 - a.0) / span, (b.1 - a.1) / span);
            let t = s / total;
            let alpha = 0.75 * smooth(0.0, 0.07, t) * smooth(1.0, 0.93, t);
            let width = (mix(0.7, self.width, t.powf(0.8)) * 0.6).max(0.9);
            let mut fleck = Path::new();
            fleck.move_to(x - dx * dash, y - dy * dash);
            fleck.line_to(x + dx * dash, y + dy * dash);
            let brush = pad.brush(Colour::rgba(214, 238, 252, 0.85)).alpha(alpha);
            pad.stroke(&fleck, &Pen::new(width).cap(Cap::Round), &brush);
            s += RIVER_GAP;
        }
    }
}

/// A current's stretches over the sea, by the move it stands for.
#[derive(Debug, Clone)]
pub(super) struct Lanes {
    pub from: usize,
    pub to: usize,
    pub runs: Vec<CurrentRun>,
}

impl Lanes {
    /// The box its streaks or chevrons fall in on the screen.
    pub fn bounds(&self, weather: &Weather) -> [f64; 4] {
        let band = self.runs.iter().map(|run| run.band).fold(0.0, f64::max);
        let points = self
            .runs
            .iter()
            .flat_map(|run| run.points.iter())
            .map(|&(x, y)| weather.screen(x, y, 0.0));
        spread(points, band + 8.0)
    }

    /// Streaks running along five lanes of each stretch 16 px a second the way the agents went,
    /// each lane its own phase so they never line up in rows; held still, chevrons along its
    /// middle point the way.
    pub fn flow(&self, pad: &mut Pad, weather: &Weather, moment: Moment) {
        for run in &self.runs {
            let at = |s: f64| {
                let mut i = 1;
                while i < run.lengths.len() - 1 && run.lengths[i] < s {
                    i += 1;
                }
                let (a, b) = (run.points[i - 1], run.points[i]);
                let span = run.lengths[i] - run.lengths[i - 1];
                let span = if span == 0.0 { 1.0 } else { span };
                let u = ((s - run.lengths[i - 1]) / span).clamp(0.0, 1.0);
                let taper = mix(run.taper[i - 1], run.taper[i], u);
                let normal = (
                    mix(run.normals[i - 1].0, run.normals[i].0, u),
                    mix(run.normals[i - 1].1, run.normals[i].1, u),
                );
                let along = (b.0 - a.0, b.1 - a.1);
                ((mix(a.0, b.0, u), mix(a.1, b.1, u)), taper, normal, along)
            };
            let place = |(x, y): (f64, f64), lane: f64, taper: f64, normal: (f64, f64)| {
                let h = run.band * lane * taper;
                (x + normal.0 * h, y + normal.1 * h)
            };
            let unit = |(dx, dy): (f64, f64)| {
                let l = dx.hypot(dy);
                let l = if l == 0.0 { 1.0 } else { l };
                (dx / l, dy / l)
            };
            if moment.is_still() {
                let brush = pad.brush(Colour::rgba(206, 238, 248, 0.75));
                let pen = Pen::new(1.4).cap(Cap::Round);
                let mut s = 22.0;
                while s < run.total - 14.0 {
                    let (middle, taper, normal, along) = at(s);
                    let (x, y) = place(middle, 0.0, taper, normal);
                    let (ux, uy) = unit(along);
                    let k = run.band * 0.3 * taper;
                    let points = [
                        (x - ux * k - uy * k, y - uy * k + ux * k),
                        (x, y),
                        (x - ux * k + uy * k, y - uy * k - ux * k),
                    ];
                    let mut chevron = Path::new();
                    for (n, &(px, py)) in points.iter().enumerate() {
                        let (sx, sy) = weather.screen(px, py, 0.0);
                        if n == 0 {
                            chevron.move_to(sx, sy);
                        } else {
                            chevron.line_to(sx, sy);
                        }
                    }
                    pad.stroke(&chevron, &pen, &brush);
                    s += 34.0;
                }
                continue;
            }
            let (from, to) = (self.from as f64, self.to as f64);
            for (j, lane) in [-0.36, -0.12, 0.12, 0.36, 0.0].into_iter().enumerate() {
                let j = j as f64;
                let offset = (moment.time(CURRENT) * CURRENT_SPEED
                    + hash3(j, from, to, 7.0) * CURRENT_GAP)
                    % CURRENT_GAP;
                let mut s = offset + j * 5.0;
                while s < run.total {
                    let (middle, taper, normal, along) = at(s);
                    let (x, y) = place(middle, lane, taper, normal);
                    let (ux, uy) = unit(along);
                    let k = 2.5 + 1.5 * hash3((s / CURRENT_GAP).floor(), j, from, 9.0);
                    if taper >= 0.2 {
                        let a = weather.screen(x - ux * k, y - uy * k, 0.0);
                        let b = weather.screen(x + ux * k, y + uy * k, 0.0);
                        let mut streak = Path::new();
                        streak.move_to(a.0, a.1);
                        streak.line_to(b.0, b.1);
                        let brush = pad
                            .brush(Colour::rgb(220, 244, 252))
                            .alpha(0.7 * taper * (1.0 - lane.abs()));
                        pad.stroke(&streak, &Pen::new(1.2).cap(Cap::Round), &brush);
                    }
                    s += CURRENT_GAP;
                }
            }
        }
    }
}

/// The box round points, `margin` px wider each way, as left, top, right and bottom.
pub(super) fn spread(points: impl Iterator<Item = (f64, f64)>, margin: f64) -> [f64; 4] {
    let mut area = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (x, y) in points {
        area = [
            area[0].min(x),
            area[1].min(y),
            area[2].max(x),
            area[3].max(y),
        ];
    }
    [
        area[0] - margin,
        area[1] - margin,
        area[2] + margin,
        area[3] + margin,
    ]
}
