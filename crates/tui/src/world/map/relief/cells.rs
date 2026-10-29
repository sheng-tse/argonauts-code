//! Goldens for the map's pictures: each terminal cell's mean colour, compared within a little.

use super::{Dots, Projected};
use crate::world::map::fields::{HEIGHT, WIDTH};
use crate::world::raster::{Brush, Canvas, Colour};

/// A terminal cell at the map's size, in pixels.
const CELL: (usize, usize) = (18, 42);

/// The picture as the screen shows it: the stood-up land with its dots over it.
pub(crate) fn composed(projected: &Projected, dots: &Dots) -> Canvas {
    let mut picture = projected.picture.clone();
    picture.draw(&dots.picture, 0.0, 0.0, &Brush::new(Colour::WHITE));
    picture
}

/// Each block's mean colour as hex, one line a row of blocks, for a picture of `size` pixels
/// read through `pixel`.
pub(crate) fn mean_colours(
    size: (usize, usize),
    block: (usize, usize),
    pixel: impl Fn(usize, usize) -> [u8; 3],
) -> String {
    let (block_w, block_h) = block;
    let mut lines = Vec::new();
    for row in 0..size.1 / block_h {
        let mut line = Vec::new();
        for column in 0..size.0 / block_w {
            let mut sum = [0_u32; 3];
            for y in row * block_h..(row + 1) * block_h {
                for x in column * block_w..(column + 1) * block_w {
                    let colour = pixel(x, y);
                    for ch in 0..3 {
                        sum[ch] += u32::from(colour[ch]);
                    }
                }
            }
            let n = (block_w * block_h) as u32;
            line.push(format!(
                "{:02x}{:02x}{:02x}",
                sum[0] / n,
                sum[1] / n,
                sum[2] / n
            ));
        }
        lines.push(line.join(" "));
    }
    lines.join("\n") + "\n"
}

/// Compares a map picture's terminal cells, 64 across and 13 down, with its golden.
pub(crate) fn compare(name: &str, picture: &Canvas, golden: &str) {
    let actual = mean_colours((WIDTH, HEIGHT), CELL, |x, y| {
        let [r, g, b, _] = picture.pixel(x as u32, y as u32);
        [r, g, b]
    });
    compare_means(name, &actual, golden, || picture.to_png());
}

/// Compares mean colours with their golden, a channel within 2 of it; with
/// `ARGONAUTS_PICTURES_OUT` set, the picture and its means are kept there too.
pub(crate) fn compare_means(name: &str, actual: &str, golden: &str, png: impl FnOnce() -> Vec<u8>) {
    if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURES_OUT") {
        let dir = std::path::Path::new(&dir);
        let _ = std::fs::write(dir.join(format!("{name}.txt")), actual);
        let _ = std::fs::write(dir.join(format!("{name}.png")), png());
    }
    let parse = |text: &str| -> Vec<[i32; 3]> {
        text.split_whitespace()
            .map(|cell| {
                let value = u32::from_str_radix(cell, 16).unwrap_or(0);
                [
                    (value >> 16) as i32,
                    ((value >> 8) & 0xff) as i32,
                    (value & 0xff) as i32,
                ]
            })
            .collect()
    };
    let across = actual
        .lines()
        .next()
        .map_or(1, |line| line.split_whitespace().count());
    let (expected, got) = (parse(golden), parse(actual));
    assert_eq!(expected.len(), got.len(), "{name}: cells");
    let off: Vec<String> = expected
        .iter()
        .zip(&got)
        .enumerate()
        .filter(|(_, (e, g))| (0..3).any(|ch| (e[ch] - g[ch]).abs() > 2))
        .map(|(at, (e, g))| format!("cell {},{}: {e:?} now {g:?}", at % across, at / across))
        .collect();
    assert!(
        off.is_empty(),
        "{name}: {} cells are off: {:?}",
        off.len(),
        &off[..off.len().min(8)]
    );
}
