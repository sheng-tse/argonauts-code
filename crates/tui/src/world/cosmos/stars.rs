//! What a star does at every level: where it sits on its galaxy, how it breathes, how a
//! binary's smaller star rides round the bigger one, and whether it moves at all.

use std::f64::consts::TAU;

use super::model::StarKind;
use super::motion;
use super::paint::core_radius;
use super::scenes::Cosmos;
use super::shapes::Frame;

/// A binary's smaller star rides round the bigger one once in this many seconds, divided by how
/// many times more often than chance they share runs.
pub(super) const BINARY_SECONDS: f64 = 24.0;
/// A protostar's cloud turns once in this many seconds.
pub(super) const CLOUD_SECONDS: f64 = 30.0;

impl Cosmos {
    /// A star's place at `t` seconds, or at its still pose, on its galaxy placed by `frame` and
    /// drawn `k` times its universe size: on the galaxy's plane, or a binary's smaller star on
    /// its way round the bigger one.
    pub(super) fn place_star(
        &self,
        frame: Frame,
        star: usize,
        k: f64,
        t: Option<f64>,
    ) -> (f64, f64) {
        let s = &self.universe.stars[star];
        let tilt = self.shapes.galaxies[s.maker].tilt;
        let place = self.shapes.stars[star];
        match (place.companion_of, s.binary) {
            (Some(big), Some(binary)) => {
                let base = self.shapes.stars[big];
                let (px, py) = frame.point(tilt, base.rad, base.th);
                let angle = match t {
                    Some(t) => {
                        let seconds = motion::stepped(BINARY_SECONDS / f64::from(binary.lift));
                        TAU * t / seconds + s.model as f64
                    }
                    None => 0.6,
                };
                let distance = self.orbit_distance(big, k);
                (
                    px + angle.cos() * distance,
                    py + angle.sin() * distance * tilt,
                )
            }
            _ => frame.point(tilt, place.rad, place.th),
        }
    }

    /// How far a binary's smaller star rides from the bigger one at `big`.
    pub(super) fn orbit_distance(&self, big: usize, k: f64) -> f64 {
        (core_radius(self.universe.stars[big].actions) * k * 2.6).max(9.0)
    }

    /// How far through its breath a star is at `t`, from 0 to 1, or 0 when it keeps a steady
    /// glow or holds still.
    pub(super) fn breath(&self, star: usize, t: Option<f64>) -> f64 {
        let s = &self.universe.stars[star];
        match (s.pulse, t) {
            (Some(pulse), Some(t)) => {
                0.5 + 0.5 * (TAU * t / motion::stepped(pulse) + s.model as f64).sin()
            }
            _ => 0.0,
        }
    }

    /// Whether a star moves: it breathes, rides round its partner, or its cloud turns.
    pub(super) fn star_moves(&self, star: usize) -> bool {
        let s = &self.universe.stars[star];
        s.pulse.is_some()
            || self.shapes.stars[star].companion_of.is_some()
            || s.kind == StarKind::Protostar
    }

    /// The steps after which a moving star looks the same again: every loop it has, each
    /// dividing the cycle, comes round together.
    pub(super) fn star_steps(&self, star: usize) -> u32 {
        let s = &self.universe.stars[star];
        let mut loops = Vec::new();
        if let Some(pulse) = s.pulse {
            loops.push(pulse);
        }
        if let Some(binary) = s.binary
            && binary.companion
        {
            loops.push(BINARY_SECONDS / f64::from(binary.lift));
        }
        if s.kind == StarKind::Protostar {
            loops.push(CLOUD_SECONDS);
        }
        loops
            .into_iter()
            .map(motion::steps_for)
            .fold(1, motion::lcm)
    }
}
