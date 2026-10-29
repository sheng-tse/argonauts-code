//! What runs over a draw's pixels before they land: blur, brightness, and the tint a glow
//! takes. Each works on premultiplied pixels, so a colour channel never passes its alpha.

use std::f64::consts::PI;

use super::colour::Colour;

/// One box blur's run: how many pixels it averages, and how many of them lie before the one
/// it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    size: usize,
    before: usize,
}

impl Run {
    fn after(self) -> usize {
        self.size - 1 - self.before
    }
}

/// The three box blurs that approximate a Gaussian of `sigma`, as SVG's blur sizes them, or
/// none when the blur is too slight to change a pixel.
fn runs(sigma: f64) -> Option<[Run; 3]> {
    if !sigma.is_finite() || sigma <= 0.0 {
        return None;
    }
    let size = (sigma * 3.0 * (2.0 * PI).sqrt() / 4.0 + 0.5).floor() as usize;
    if size <= 1 {
        return None;
    }
    let half = size / 2;
    Some(if size % 2 == 1 {
        [Run { size, before: half }; 3]
    } else {
        // An even box can't centre on a pixel: one leans left, one right, and a third one
        // pixel wider centres the result.
        [
            Run { size, before: half },
            Run {
                size,
                before: half - 1,
            },
            Run {
                size: size + 1,
                before: half,
            },
        ]
    })
}

/// How many pixels a blur of `sigma` spreads a pixel's colour, each way.
pub(super) fn spread(sigma: f64) -> usize {
    runs(sigma.min(WIDEST)).map_or(0, |runs| {
        let before: usize = runs.iter().map(|run| run.before).sum();
        let after: usize = runs.iter().map(|run| run.after()).sum();
        before.max(after)
    })
}

/// Blurs premultiplied RGBA `pixels`, `width` across, by a Gaussian of `sigma`, as if clear
/// pixels lay all round them. A sigma past [`WIDEST`] blurs as much as that.
pub(super) fn blur(pixels: &mut [u8], width: usize, sigma: f64) {
    let Some(runs) = runs(sigma.min(WIDEST)) else {
        return;
    };
    if width == 0 || pixels.is_empty() {
        return;
    }
    let stride = width * 4;
    let height = pixels.len() / stride;

    // Each row through all three runs while it is in cache. A clear row stays clear.
    let (mut line, mut spare_line) = (vec![0; stride], vec![0; stride]);
    for row in pixels.chunks_exact_mut(stride) {
        if row.iter().all(|&value| value == 0) {
            continue;
        }
        line.copy_from_slice(row);
        for run in runs {
            across(&line, &mut spare_line, run);
            std::mem::swap(&mut line, &mut spare_line);
        }
        row.copy_from_slice(&line);
    }

    let mut spare = vec![0; pixels.len()];
    down(pixels, &mut spare, stride, height, runs[0]);
    down(&spare, pixels, stride, height, runs[1]);
    down(pixels, &mut spare, stride, height, runs[2]);
    pixels.copy_from_slice(&spare);
}

/// The widest blur, in pixels of sigma, which keeps every sum within 32 bits.
const WIDEST: f64 = 500.0;

/// Divides a sum of `size` channels back to one, rounded, in 32-bit fixed point: a box of
/// under a thousand pixels keeps `sum × scale` under 2^32.
#[derive(Debug, Clone, Copy)]
struct Divider {
    scale: u32,
}

impl Divider {
    fn new(size: usize) -> Self {
        Self {
            scale: ((1_u32 << 24) as f64 / size as f64).round() as u32,
        }
    }

    fn apply(self, sum: u32) -> u8 {
        ((sum * self.scale + (1 << 23)) >> 24).min(255) as u8
    }
}

/// One box blur along a row of pixels.
fn across(from: &[u8], to: &mut [u8], run: Run) {
    let width = from.len() / 4;
    let divide = Divider::new(run.size);
    let pixel = |x: usize| -> [u32; 4] {
        let at = x * 4;
        [from[at], from[at + 1], from[at + 2], from[at + 3]].map(u32::from)
    };
    let mut sum = [0_u32; 4];
    for x in 0..run.after().min(width) {
        let entering = pixel(x);
        for channel in 0..4 {
            sum[channel] += entering[channel];
        }
    }
    for (x, out) in to.chunks_exact_mut(4).enumerate() {
        if x + run.after() < width {
            let entering = pixel(x + run.after());
            for channel in 0..4 {
                sum[channel] += entering[channel];
            }
        }
        for channel in 0..4 {
            out[channel] = divide.apply(sum[channel]);
        }
        if x >= run.before {
            let leaving = pixel(x - run.before);
            for channel in 0..4 {
                sum[channel] -= leaving[channel];
            }
        }
    }
}

/// One box blur down each column, a row at a time so memory is read in order.
fn down(from: &[u8], to: &mut [u8], stride: usize, height: usize, run: Run) {
    let divide = Divider::new(run.size);
    let row = |y: usize| &from[y * stride..(y + 1) * stride];
    let mut sums = vec![0_u32; stride];
    for y in 0..run.after().min(height) {
        add(&mut sums, row(y));
    }
    for (y, out) in to.chunks_exact_mut(stride).enumerate() {
        if y + run.after() < height {
            add(&mut sums, row(y + run.after()));
        }
        for (value, &sum) in out.iter_mut().zip(&sums) {
            *value = divide.apply(sum);
        }
        if y >= run.before {
            take(&mut sums, row(y - run.before));
        }
    }
}

fn add(sums: &mut [u32], pixels: &[u8]) {
    for (sum, &value) in sums.iter_mut().zip(pixels) {
        *sum += u32::from(value);
    }
}

fn take(sums: &mut [u32], pixels: &[u8]) {
    for (sum, &value) in sums.iter_mut().zip(pixels) {
        *sum -= u32::from(value);
    }
}

/// Scales premultiplied colours by `factor`, as a canvas's `brightness()` filter does: 0
/// leaves a black silhouette, 1 changes nothing, and above 1 colours brighten up to white.
pub(super) fn brighten(pixels: &mut [u8], factor: f64) {
    let factor = if factor.is_finite() {
        factor.max(0.0)
    } else {
        1.0
    };
    if factor == 1.0 {
        return;
    }
    let scale = (factor * 65536.0).round() as u64;
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u64::from(pixel[3]);
        for channel in &mut pixel[..3] {
            let scaled = (u64::from(*channel) * scale + 32768) >> 16;
            *channel = scaled.min(alpha) as u8;
        }
    }
}

/// Repaints premultiplied `pixels` in `colour`, each keeping its own alpha times the
/// colour's: the shape a glow takes before it softens.
pub(super) fn tint(pixels: &mut [u8], colour: Colour) {
    let strength = (f64::from(colour.a) * 255.0).round() as u32;
    let channels = [colour.r, colour.g, colour.b].map(u32::from);
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = (u32::from(pixel[3]) * strength + 127) / 255;
        for (out, channel) in pixel.iter_mut().zip(channels) {
            *out = ((channel * alpha + 127) / 255) as u8;
        }
        pixel[3] = alpha as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blur_is_sized_as_svg_sizes_it() {
        assert_eq!(runs(0.0), None);
        assert_eq!(runs(0.5), None);
        // 1.5 pixels: three boxes of three.
        assert_eq!(runs(1.5), Some([Run { size: 3, before: 1 }; 3]));
        // 2 pixels: an even box of four, leaning one way, then the other, then centred.
        assert_eq!(
            runs(2.0),
            Some([
                Run { size: 4, before: 2 },
                Run { size: 4, before: 1 },
                Run { size: 5, before: 2 },
            ])
        );
        assert_eq!(spread(1.5), 3);
        assert_eq!(spread(2.0), 5);
    }
}
