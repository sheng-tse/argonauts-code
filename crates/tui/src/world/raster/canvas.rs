use tiny_skia::{BlendMode, FillRule, Mask, Paint, Pixmap, PremultipliedColorU8};

use super::filter;
use super::path::Path;
use super::transform::Transform;
use crate::base::draw::scene::Image;

/// A picture being drawn: RGBA pixels, kept premultiplied by their alpha while drawing and
/// handed out straight.
#[derive(Clone)]
pub struct Canvas {
    pub(super) pixmap: Pixmap,
    /// How much of each pixel draws may reach, while a clip holds.
    pub(super) clip: Option<Mask>,
}

impl std::fmt::Debug for Canvas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Canvas({} × {})", self.width(), self.height())
    }
}

impl Canvas {
    /// A clear canvas. Each side is at least one pixel.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pixmap: blank(width, height),
            clip: None,
        }
    }

    /// A canvas holding `rgba`, rows of straight red, green, blue and alpha from the top left,
    /// or `None` when there aren't `width` × `height` pixels of them.
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Self> {
        let mut canvas = Self::new(width, height);
        canvas.put_rgba(rgba).then_some(canvas)
    }

    pub fn from_image(image: &Image) -> Option<Self> {
        let width = u32::try_from(image.width()).ok()?;
        let height = u32::try_from(image.height()).ok()?;
        Self::from_rgba(width, height, image.pixels())
    }

    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    /// Makes every pixel clear, or only those inside the clip while one holds.
    pub fn clear(&mut self) {
        let Some(clip) = &self.clip else {
            self.pixmap.fill(tiny_skia::Color::TRANSPARENT);
            return;
        };
        let whole = tiny_skia::Rect::from_xywh(0.0, 0.0, self.width() as f32, self.height() as f32);
        // Rubbing out with solid ink, which the clip thins at its edges, clears exactly what
        // the clip lets through.
        let rubbing_out = Paint {
            blend_mode: BlendMode::DestinationOut,
            ..Paint::default()
        };
        if let Some(whole) = whole {
            self.pixmap.fill_rect(
                whole,
                &rubbing_out,
                tiny_skia::Transform::identity(),
                Some(clip),
            );
        }
    }

    /// Runs `draw` with every draw it makes kept inside `path`, placed by `transform`, and
    /// inside any clip that already holds, as a canvas's `clip` keeps them until `restore`.
    /// Clip edges are anti-aliased. Pixels put, blurred or brightened whole ignore the clip.
    pub fn with_clip<T>(
        &mut self,
        path: &Path,
        transform: Transform,
        draw: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let narrowed = self.narrowed(path, transform);
        let outer = self.clip.replace(narrowed);
        let drawn = draw(self);
        self.clip = outer;
        drawn
    }

    /// The clip that holds now, cut down to `path`.
    fn narrowed(&self, path: &Path, transform: Transform) -> Mask {
        let (width, height) = (self.width(), self.height());
        let transform = transform.skia();
        match (&self.clip, path.skia()) {
            (Some(clip), Some(path)) => {
                let mut clip = clip.clone();
                clip.intersect_path(&path, FillRule::Winding, true, transform);
                clip
            }
            (None, Some(path)) => {
                let mut clip = no_pixels(width, height);
                clip.fill_path(&path, FillRule::Winding, true, transform);
                clip
            }
            (_, None) => no_pixels(width, height),
        }
    }

    /// Blurs the whole canvas by a Gaussian of `sigma` pixels, as if clear pixels lay all
    /// round it.
    pub fn blur(&mut self, sigma: f64) {
        let width = self.width() as usize;
        filter::blur(self.pixmap.data_mut(), width, sigma);
    }

    /// Scales every colour by `factor`, as a canvas's `brightness()` filter: 0 leaves a black
    /// silhouette, 1 changes nothing, above 1 brightens up to white. Alpha stays.
    pub fn brighten(&mut self, factor: f64) {
        filter::brighten(self.pixmap.data_mut(), factor);
    }

    /// The straight red, green, blue and alpha at (x, y), clear outside the canvas.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixmap.pixel(x, y).map_or([0; 4], straight)
    }

    /// Every pixel as straight red, green, blue and alpha, row by row from the top left.
    pub fn to_rgba(&self) -> Vec<u8> {
        // A clear pixel and a solid one read the same premultiplied or not, and most of a
        // world's pictures are one or the other, so only the pixels between are worked out.
        let mut rgba = self.pixmap.data().to_vec();
        for (pixel, out) in self.pixmap.pixels().iter().zip(rgba.chunks_exact_mut(4)) {
            if !matches!(pixel.alpha(), 0 | u8::MAX) {
                out.copy_from_slice(&straight(*pixel));
            }
        }
        rgba
    }

    /// Replaces every pixel with `rgba`, straight red, green, blue and alpha row by row, or
    /// changes nothing and says `false` when its length doesn't fit the canvas.
    pub fn put_rgba(&mut self, rgba: &[u8]) -> bool {
        let pixels = self.pixmap.pixels_mut();
        if rgba.len() != pixels.len() * 4 {
            return false;
        }
        for (pixel, from) in pixels.iter_mut().zip(rgba.chunks_exact(4)) {
            *pixel =
                tiny_skia::ColorU8::from_rgba(from[0], from[1], from[2], from[3]).premultiply();
        }
        true
    }

    /// The pixels as drawn, red, green, blue and alpha premultiplied by alpha: the fast way
    /// to read many of them.
    pub fn premultiplied(&self) -> &[u8] {
        self.pixmap.data()
    }

    /// The pixels as drawn, premultiplied by alpha, to write in place. Each colour channel
    /// must stay at or under its pixel's alpha.
    pub fn premultiplied_mut(&mut self) -> &mut [u8] {
        self.pixmap.data_mut()
    }

    pub fn to_image(&self) -> Image {
        let (width, height) = (self.width() as usize, self.height() as usize);
        Image::new(width, height, self.to_rgba())
            .unwrap_or_else(|| Image::transparent(width, height))
    }

    /// The canvas as a PNG, through the crate's own encoder.
    pub fn to_png(&self) -> Vec<u8> {
        self.to_image().to_png()
    }
}

/// A clip that lets no draw through, the size of a canvas.
fn no_pixels(width: u32, height: u32) -> Mask {
    Mask::new(width, height).expect("a mask fits wherever its canvas's pixels do")
}

fn straight(pixel: PremultipliedColorU8) -> [u8; 4] {
    let colour = pixel.demultiply();
    [colour.red(), colour.green(), colour.blue(), colour.alpha()]
}

/// A clear pixmap of at least one pixel each way, and of one pixel when the size asked for is
/// more than memory can hold.
pub(super) fn blank(width: u32, height: u32) -> Pixmap {
    Pixmap::new(width.max(1), height.max(1))
        .or_else(|| Pixmap::new(1, 1))
        .expect("a one-pixel pixmap always fits")
}
