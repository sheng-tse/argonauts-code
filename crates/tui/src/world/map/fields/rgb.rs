//! Colours as the ground mixes them: red, green and blue from 0 to 255, unrounded, so a colour
//! mixed many times over keeps its fractions until it is written as a pixel.

use crate::world::raster::noise::mix;

pub type Rgb = [f64; 3];

/// The colour `t` of the way from `a` to `b`.
pub fn mix_rgb(a: Rgb, b: Rgb, t: f64) -> Rgb {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

pub fn scale_rgb(colour: Rgb, by: f64) -> Rgb {
    [colour[0] * by, colour[1] * by, colour[2] * by]
}

/// The colour's own lightness as a cool grey, for the land a comparison leaves calm.
pub fn grey(colour: Rgb) -> Rgb {
    let light = 0.3 * colour[0] + 0.59 * colour[1] + 0.11 * colour[2];
    [light, light, light * 1.04]
}

/// A mixed colour as a pixel's bytes, held to 0..=255 and rounded half to even, as a canvas's
/// pixels take it.
pub fn to_bytes(colour: Rgb) -> [u8; 3] {
    colour.map(byte)
}

/// One channel as a pixel's byte; not a number is 0.
pub fn byte(channel: f64) -> u8 {
    if channel.is_nan() {
        0
    } else {
        channel.clamp(0.0, 255.0).round_ties_even() as u8
    }
}
