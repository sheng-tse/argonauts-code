//! The map as a picture, for a terminal that draws one: 64 by 13 cells at the terminal's cell
//! size, the comparison as soft red and blue blocks behind the dots, and each dot in its
//! group's colour. A lit behavior's dots glow and the rest fade; a ringed dot, the example in
//! hand or the one nearest the mouse, gets a warm glow, a white edge and a bold ring. Nothing is
//! written on it.

use argonauts_core::behavior_map::{BehaviorMap, GRID};

use super::{GroupColours, MAP_COLUMNS, MAP_ROWS};
use crate::mascot::Graphics;
use crate::mascot::png::{self, Rgba};

type Rgb = (u8, u8, u8);

const PAPER: Rgb = (31, 32, 47);

/// The map's paper as a terminal colour, for words that read as cut out of it, such as a new
/// row's badge. A terminal of fewer colours takes the badges' own text colour.
pub fn paper_colour(kit: &crate::kit::Kit<'_>) -> Option<ratatui::style::Color> {
    use crate::theme::{ColorLevel, Role, nearest_ansi256};
    use ratatui::style::Color;
    let (r, g, b) = PAPER;
    match kit.theme.level {
        ColorLevel::TrueColor => Some(Color::Rgb(r, g, b)),
        ColorLevel::Ansi256 => Some(Color::Indexed(nearest_ansi256(PAPER))),
        _ => kit.theme.color(Role::BadgeText),
    }
}
const EDGE: Rgb = (20, 20, 30);
const WHITE: Rgb = (255, 255, 255);
const MORE: Rgb = (226, 85, 98);
const LESS: Rgb = (86, 132, 214);
/// The difference shows where it is more than this share of its largest.
const SHOWN: f32 = 0.08;
/// The sizes below are drawn for a map this wide, cells of 18 by 42 pixels on a Retina screen, and scale with it.
const DRAWN_WIDTH: f32 = 1152.0;

/// What one picture of the map shows.
pub struct Scene<'a> {
    pub map: &'a BehaviorMap,
    pub colours: &'a GroupColours,
    pub compared: Option<usize>,
    pub lit: &'a [usize],
    /// The example in hand, ringed.
    pub ringed: Option<usize>,
    /// The dot nearest the mouse, ringed with a warm glow.
    pub hovered: Option<usize>,
}

/// A picture's pixels, filled with one colour.
struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 3]>,
}

impl Canvas {
    fn new(width: usize, height: usize, paper: Rgb) -> Self {
        let colour = [f32::from(paper.0), f32::from(paper.1), f32::from(paper.2)];
        Self {
            width,
            height,
            pixels: vec![colour; width * height],
        }
    }

    fn blend(&mut self, x: usize, y: usize, colour: Rgb, alpha: f32) {
        if x >= self.width || y >= self.height || alpha <= 0.0 {
            return;
        }
        let alpha = alpha.min(1.0);
        let pixel = &mut self.pixels[y * self.width + x];
        for (channel, value) in pixel.iter_mut().zip([colour.0, colour.1, colour.2]) {
            *channel += (f32::from(value) - *channel) * alpha;
        }
    }

    /// Paints `alpha` of `colour` over every pixel `coverage` reaches near `(cx, cy)`.
    fn around(&mut self, cx: f32, cy: f32, reach: f32, colour: Rgb, coverage: impl Fn(f32) -> f32) {
        let (left, right) = ((cx - reach).floor().max(0.0), (cx + reach).ceil());
        let (top, bottom) = ((cy - reach).floor().max(0.0), (cy + reach).ceil());
        for y in top as usize..=(bottom as usize).min(self.height.saturating_sub(1)) {
            for x in left as usize..=(right as usize).min(self.width.saturating_sub(1)) {
                let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                self.blend(x, y, colour, coverage(d));
            }
        }
    }

    fn disc(&mut self, cx: f32, cy: f32, radius: f32, colour: Rgb, alpha: f32) {
        self.around(cx, cy, radius + 1.0, colour, |d| {
            (radius + 0.5 - d).clamp(0.0, 1.0) * alpha
        });
    }

    fn ring(&mut self, cx: f32, cy: f32, radius: f32, width: f32, colour: Rgb, alpha: f32) {
        self.around(cx, cy, radius + width, colour, |d| {
            (width / 2.0 + 0.5 - (d - radius).abs()).clamp(0.0, 1.0) * alpha
        });
    }

    /// A soft glow: full to `radius`, fading over `soft` either side of it.
    fn glow(&mut self, cx: f32, cy: f32, radius: f32, soft: f32, colour: Rgb, alpha: f32) {
        self.around(cx, cy, radius + soft, colour, |d| {
            let t = ((radius + soft - d) / (2.0 * soft)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t) * alpha
        });
    }

    fn into_rgba(self) -> Rgba {
        let mut image = Rgba::transparent(self.width, self.height);
        for (at, pixel) in self.pixels.iter().enumerate() {
            let out = &mut image.pixels[at * 4..at * 4 + 4];
            for (channel, value) in out.iter_mut().zip(pixel) {
                *channel = value.round().clamp(0.0, 255.0) as u8;
            }
            out[3] = 255;
        }
        image
    }
}

/// The picture's size in pixels for cells of `cell` pixels.
pub fn size(cell: (u16, u16)) -> (usize, usize) {
    (
        MAP_COLUMNS * usize::from(cell.0),
        MAP_ROWS * usize::from(cell.1),
    )
}

/// The pixel a place on the map falls on, leaving a margin so edge dots stay whole.
fn to_pixel(x: f32, y: f32, (width, height): (usize, usize)) -> (f32, f32) {
    let (width, height) = (width as f32, height as f32);
    let margin = 12.0 * width / 720.0;
    (
        margin + x * (width - 2.0 * margin),
        height - margin - y * (height - 2.0 * margin),
    )
}

/// The place on the map a pixel stands for, the other way from [`to_pixel`], on a picture
/// `width` by `height` pixels.
pub fn to_place(px: f32, py: f32, (width, height): (f32, f32)) -> (f32, f32) {
    let margin = 12.0 * width / 720.0;
    (
        (px - margin) / (width - 2.0 * margin),
        (height - margin - py) / (height - 2.0 * margin),
    )
}

/// The scene drawn for cells of `cell` pixels.
pub fn draw(scene: &Scene<'_>, cell: (u16, u16)) -> Rgba {
    let size = size(cell);
    let (width, height) = size;
    let mut canvas = Canvas::new(width, height, PAPER);
    let scale = width as f32 / DRAWN_WIDTH;
    let dot = width as f32 / 720.0;
    let quiet = !scene.lit.is_empty() || scene.ringed.is_some();
    let overlay = scene
        .compared
        .and_then(|group| scene.map.comparisons.get(group))
        .and_then(|c| c.overlay.as_deref());
    if let Some(grid) = overlay {
        let strength = if quiet { 0.45 } else { 1.0 };
        for y in 0..height {
            let row = GRID - 1 - (y * GRID / height).min(GRID - 1);
            for x in 0..width {
                let column = (x * GRID / width).min(GRID - 1);
                let value = grid.get(row * GRID + column).copied().unwrap_or(0.0);
                if value.abs() > SHOWN {
                    let colour = if value > 0.0 { MORE } else { LESS };
                    let alpha = value.abs().min(1.0) * 150.0 / 255.0 * strength;
                    canvas.blend(x, y, colour, alpha);
                }
            }
        }
    }
    let at = |index: usize| {
        let action = &scene.map.actions[index];
        to_pixel(action.x, action.y, size)
    };
    for &index in scene.lit {
        let (px, py) = at(index);
        canvas.glow(
            px,
            py,
            26.0 * scale,
            12.0 * scale,
            (255, 236, 200),
            95.0 / 255.0,
        );
    }
    for (index, action) in scene.map.actions.iter().enumerate() {
        if scene.lit.contains(&index) {
            continue;
        }
        let (px, py) = at(index);
        let colour = scene.colours.rgb(action.group);
        let (radius, alpha, edged) = match scene.compared {
            _ if quiet => (3.0 * dot, 0.28, false),
            None => (3.5 * dot, 1.0, true),
            Some(group) if group == action.group => (4.0 * dot, 1.0, true),
            Some(_) => (3.0 * dot, 110.0 / 255.0, true),
        };
        if edged {
            canvas.disc(px, py, radius + 1.0, EDGE, alpha);
        }
        canvas.disc(px, py, radius, colour, alpha);
    }
    for &index in scene.lit {
        let (px, py) = at(index);
        let colour = scene.colours.rgb(scene.map.actions[index].group);
        canvas.disc(px, py, 5.0 * dot + 2.0 * scale, (250, 250, 255), 1.0);
        canvas.disc(px, py, 5.0 * dot, colour, 1.0);
    }
    if let Some(index) = scene.ringed {
        let (px, py) = at(index);
        let colour = scene.colours.rgb(scene.map.actions[index].group);
        canvas.ring(px, py, 23.0 * scale, 3.0 * scale, WHITE, 110.0 / 255.0);
        canvas.disc(px, py, 6.0 * dot, colour, 1.0);
        canvas.ring(px, py, 17.0 * scale, 3.0 * scale, WHITE, 1.0);
    }
    if let Some(index) = scene.hovered {
        let (px, py) = at(index);
        let colour = scene.colours.rgb(scene.map.actions[index].group);
        canvas.glow(
            px,
            py,
            34.0 * scale,
            14.0 * scale,
            (255, 240, 210),
            150.0 / 255.0,
        );
        canvas.ring(px, py, 31.0 * scale, 3.0 * scale, WHITE, 120.0 / 255.0);
        canvas.ring(px, py, 23.0 * scale, 4.0 * scale, WHITE, 1.0);
        canvas.disc(px, py, 10.0 * scale + 3.0 * scale, WHITE, 1.0);
        canvas.disc(px, py, 10.0 * scale, colour, 1.0);
    }
    canvas.into_rgba()
}

/// The scene as a PNG for cells of `cell` pixels. It goes out again each time the mouse
/// moves to another dot, so it is packed fast.
pub fn png(scene: &Scene<'_>, cell: (u16, u16)) -> Vec<u8> {
    png::encode_quickly(&draw(scene, cell))
}

/// The escapes that draw the scene over the map's cells from the cursor, as the mascots' go
/// out, named `id` for the kitty protocol.
pub fn escapes(graphics: Graphics, scene: &Scene<'_>, cell: (u16, u16), id: u32) -> Vec<u8> {
    crate::mascot::graphics::draw_over(graphics, &png(scene, cell), id, (MAP_COLUMNS, MAP_ROWS))
}
