//! The sky behind the planet when the universe lends none: a dark field a little lighter in the
//! middle, with 700 faint stars.

use crate::world::map::fields::byte;
use crate::world::raster::Canvas;
use crate::world::raster::noise::{Rng, mix};

/// The plain sky, `width` × `height` px, the same in every run.
pub fn plain(width: usize, height: usize) -> Canvas {
    let (w, h) = (width as f64, height as f64);
    let mut rgba = vec![0_u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let away = ((x as f64 - w / 2.0) / w).hypot((y as f64 - h / 2.0) / h);
            let k = 1.0 - 0.38 * away;
            let at = (y * width + x) * 4;
            rgba[at..at + 4].copy_from_slice(&[
                byte(23.0 * k),
                byte(24.0 * k),
                byte(37.0 * k),
                255,
            ]);
        }
    }
    let mut rng = Rng::new(99);
    for _ in 0..700 {
        let x = (rng.next_f64() * w) as usize;
        let y = (rng.next_f64() * h) as usize;
        let strength = 0.06 + 0.2 * rng.next_f64() * rng.next_f64() * rng.next_f64();
        let warmth = rng.next_f64();
        let colour = if warmth < 0.2 {
            [255.0, 214.0, 170.0]
        } else if warmth < 0.75 {
            [214.0, 222.0, 245.0]
        } else {
            [178.0, 198.0, 255.0]
        };
        let at = (y.min(height - 1) * width + x.min(width - 1)) * 4;
        for ch in 0..3 {
            rgba[at + ch] = byte(mix(f64::from(rgba[at + ch]), colour[ch], strength));
        }
    }
    Canvas::from_rgba(width as u32, height as u32, &rgba)
        .unwrap_or_else(|| Canvas::new(width as u32, height as u32))
}
