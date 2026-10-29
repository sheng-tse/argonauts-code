use tiny_skia::{BlendMode, FillRule, FilterQuality, Mask, Paint, Pixmap, PixmapPaint};

use super::brush::Brush;
use super::canvas::Canvas;
use super::filter;
use super::ink::Ink;
use super::path::Path;
use super::pen::Pen;
use super::transform::Transform;

/// Where one draw's pixels go and how: straight onto the canvas, or into a layer made for it.
pub(super) struct Landing<'a> {
    pub transform: tiny_skia::Transform,
    pub strength: f32,
    pub blend: BlendMode,
    pub clip: Option<&'a Mask>,
}

impl Landing<'_> {
    fn paint(&self, ink: &Ink) -> Paint<'static> {
        Paint {
            shader: ink.shader(self.strength),
            blend_mode: self.blend,
            anti_alias: true,
            ..Paint::default()
        }
    }
}

impl Canvas {
    pub fn fill(&mut self, path: &Path, brush: &Brush) {
        let Some(path) = path.skia().filter(has_area) else {
            return;
        };
        let reach = Reach::placed(path.bounds(), 0.0, brush.transform());
        self.lay(brush, reach, |pixmap, landing| {
            pixmap.fill_path(
                &path,
                &landing.paint(brush.ink_of()),
                FillRule::Winding,
                landing.transform,
                landing.clip,
            );
        });
    }

    pub fn stroke(&mut self, path: &Path, pen: &Pen, brush: &Brush) {
        let Some(path) = path.skia() else {
            return;
        };
        let stroke = pen.skia();
        // Half a pixel more for a line thinner than a pixel, which is drawn a pixel wide.
        let reach = Reach::placed(path.bounds(), pen.reach() + 0.5, brush.transform());
        self.lay(brush, reach, |pixmap, landing| {
            pixmap.stroke_path(
                &path,
                &landing.paint(brush.ink_of()),
                &stroke,
                landing.transform,
                landing.clip,
            );
        });
    }

    /// Fills the box from (x, y) across `width` and down `height`.
    pub fn fill_rect(&mut self, x: f64, y: f64, width: f64, height: f64, brush: &Brush) {
        let mut path = Path::new();
        path.rect(x, y, width, height);
        self.fill(&path, brush);
    }

    /// Makes one draw that stays within `reach`: straight onto the canvas when the brush
    /// allows, or else into a clear layer of its own, where the brush's filter runs and its
    /// glow is cast, which is then laid on the canvas whole.
    pub(super) fn lay(
        &mut self,
        brush: &Brush,
        reach: Reach,
        draw: impl FnOnce(&mut Pixmap, &Landing),
    ) {
        if !brush.needs_layer() {
            let landing = Landing {
                transform: brush.transform().skia(),
                strength: brush.strength(),
                blend: brush.mix().skia(),
                clip: self.clip.as_ref(),
            };
            draw(&mut self.pixmap, &landing);
            return;
        }

        // The layer holds every pixel the blurs can spread colour into, and the canvas's
        // pixels themselves can take colour from up to that far outside it.
        let blurred = filter::spread(brush.blur_of());
        let cast = brush
            .glow_of()
            .map_or(0, |glow| filter::spread(glow.blur / 2.0));
        let spread = (blurred + cast) as f64;
        let canvas = Reach {
            left: 0.0,
            top: 0.0,
            right: f64::from(self.width()),
            bottom: f64::from(self.height()),
        }
        .widened(spread);
        let Some((left, top, mut layer)) = reach.widened(spread + 1.0).within(canvas).layer()
        else {
            if brush.mix().reaches_everywhere() {
                self.keep_inside(None, brush.strength());
            }
            return;
        };

        let on_its_own = Landing {
            transform: tiny_skia::Transform::from_translate(-(left as f32), -(top as f32))
                .pre_concat(brush.transform().skia()),
            strength: 1.0,
            blend: BlendMode::SourceOver,
            clip: None,
        };
        draw(&mut layer, &on_its_own);
        if let Some(factor) = brush.brightness_of() {
            filter::brighten(layer.data_mut(), factor);
        }
        let width = layer.width() as usize;
        filter::blur(layer.data_mut(), width, brush.blur_of());
        if let Some(glow) = brush.glow_of() {
            let mut cast = layer.clone();
            filter::tint(cast.data_mut(), glow.colour);
            filter::blur(cast.data_mut(), width, glow.blur / 2.0);
            self.put_layer(&cast, (left, top), brush);
        }
        self.put_layer(&layer, (left, top), brush);
    }

    /// Lays a finished layer on the canvas with its top left at `at`, as the brush mixes.
    fn put_layer(&mut self, layer: &Pixmap, at: (i32, i32), brush: &Brush) {
        if brush.mix().reaches_everywhere() {
            self.keep_inside(Some((layer, at)), brush.strength());
            return;
        }
        self.pixmap.draw_pixmap(
            at.0,
            at.1,
            layer.as_ref(),
            &PixmapPaint {
                opacity: brush.strength(),
                blend_mode: brush.mix().skia(),
                quality: FilterQuality::Nearest,
            },
            tiny_skia::Transform::identity(),
            self.clip.as_ref(),
        );
    }
}

impl Canvas {
    /// Keeps the canvas only where the layer at its spot covers it, at `strength`, and
    /// clears the rest of what the clip lets through, as a canvas's keep inside does.
    ///
    /// tiny-skia's masks thin the ink rather than the result, which can't keep what lies
    /// outside the clip, so this mix is worked out here.
    fn keep_inside(&mut self, layer: Option<(&Pixmap, (i32, i32))>, strength: f32) {
        let width = self.width() as usize;
        let strength = (strength.clamp(0.0, 1.0) * 255.0).round() as u32;
        let clip = self.clip.as_ref().map(Mask::data);
        let pixels = self.pixmap.data_mut();
        for (y, row) in pixels.chunks_exact_mut(width * 4).enumerate() {
            // The layer's row over this one, and where it starts across the canvas.
            let over = layer.and_then(|(layer, (left, top))| {
                let at = usize::try_from(y as i64 - i64::from(top)).ok()?;
                let stride = layer.width() as usize * 4;
                let row = layer.data().get(at * stride..(at + 1) * stride)?;
                Some((row, i64::from(left)))
            });
            for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                let covered = over.map_or(0, |(over, left)| {
                    usize::try_from(x as i64 - left)
                        .ok()
                        .and_then(|column| over.get(column * 4 + 3))
                        .map_or(0, |&alpha| u32::from(alpha))
                });
                let kept = (covered * strength + 127) / 255;
                let reach = clip.map_or(255, |clip| u32::from(clip[y * width + x]));
                // Out of 255 × 255: all of what the clip holds back, and of the rest, the
                // part the layer covers.
                let keep = (255 - reach) * 255 + reach * kept;
                if keep == 255 * 255 {
                    continue;
                }
                for channel in pixel {
                    *channel = ((u32::from(*channel) * keep + 32512) / 65025) as u8;
                }
            }
        }
    }
}

/// A box on the canvas, in pixels, that a draw stays within.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Reach {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Reach {
    /// The box round `bounds`, in the drawing's own space and widened by `widen` there,
    /// once `transform` places it on the canvas.
    pub(super) fn placed(bounds: tiny_skia::Rect, widen: f64, transform: Transform) -> Self {
        let (left, top) = (
            f64::from(bounds.left()) - widen,
            f64::from(bounds.top()) - widen,
        );
        let (right, bottom) = (
            f64::from(bounds.right()) + widen,
            f64::from(bounds.bottom()) + widen,
        );
        let corners = [(left, top), (right, top), (right, bottom), (left, bottom)]
            .map(|(x, y)| transform.apply(x, y));
        let xs = corners.map(|(x, _)| x);
        let ys = corners.map(|(_, y)| y);
        Self {
            left: xs.into_iter().fold(f64::INFINITY, f64::min),
            top: ys.into_iter().fold(f64::INFINITY, f64::min),
            right: xs.into_iter().fold(f64::NEG_INFINITY, f64::max),
            bottom: ys.into_iter().fold(f64::NEG_INFINITY, f64::max),
        }
    }

    fn widened(self, by: f64) -> Self {
        Self {
            left: self.left - by,
            top: self.top - by,
            right: self.right + by,
            bottom: self.bottom + by,
        }
    }

    fn within(self, other: Self) -> Self {
        Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }

    /// A clear layer covering the box in whole pixels, with its top left on the canvas, or
    /// `None` when the box holds no pixel.
    fn layer(self) -> Option<(i32, i32, Pixmap)> {
        let (left, top) = (self.left.floor(), self.top.floor());
        let (right, bottom) = (self.right.ceil(), self.bottom.ceil());
        if !(right > left && bottom > top) {
            return None;
        }
        let layer = Pixmap::new((right - left) as u32, (bottom - top) as u32)?;
        Some((left as i32, top as i32, layer))
    }
}

/// Whether a path encloses anything to fill; a flat one would only make tiny-skia complain.
fn has_area(path: &tiny_skia::Path) -> bool {
    let bounds = path.bounds();
    bounds.width() > 0.0 && bounds.height() > 0.0
}
