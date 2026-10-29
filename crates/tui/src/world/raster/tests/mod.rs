use super::*;

mod blends;
mod blits;
mod clips;
mod filters;
mod gradients;
mod grids;
mod noises;
mod paths;
mod pixels;
mod speed;
mod strokes;

const RED: Colour = Colour::rgb(255, 0, 0);

fn alpha(canvas: &Canvas, x: u32, y: u32) -> u8 {
    canvas.pixel(x, y)[3]
}

/// Whether the pixel is almost wholly covered.
fn inked(canvas: &Canvas, x: u32, y: u32) -> bool {
    alpha(canvas, x, y) > 200
}

/// Whether the pixel is almost wholly clear.
fn clear(canvas: &Canvas, x: u32, y: u32) -> bool {
    alpha(canvas, x, y) < 30
}

/// How many whole pixels the drawing covers, adding up partly covered ones. Edges come out
/// a little light, as a canvas's do, so a round shape's area reads about half a percent short.
fn coverage(canvas: &Canvas) -> f64 {
    canvas
        .to_rgba()
        .chunks_exact(4)
        .map(|pixel| f64::from(pixel[3]) / 255.0)
        .sum()
}

fn near(value: f64, expected: f64, within: f64) -> bool {
    (value - expected).abs() <= within
}
