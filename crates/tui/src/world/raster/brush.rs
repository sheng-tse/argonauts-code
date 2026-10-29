use super::blend::Blend;
use super::blit::Smoothing;
use super::colour::Colour;
use super::filter;
use super::ink::Ink;
use super::transform::Transform;

/// How one draw lands on a canvas, as a canvas's drawing state holds it: the ink it lays down,
/// where it goes, how strongly, how it mixes with what is there, the filter and glow run
/// over it first, and how a picture's pixels are read. Pictures ignore the ink.
///
/// A brush is a value: make one for a draw, or keep one and change a copy.
#[derive(Debug, Clone, PartialEq)]
pub struct Brush {
    ink: Ink,
    transform: Transform,
    alpha: f32,
    blend: Blend,
    brightness: Option<f64>,
    blur: f64,
    glow: Option<Glow>,
    smoothing: Smoothing,
}

/// A copy of a draw's shape in one colour, blurred and laid straight under it: the halo
/// round a bright line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Glow {
    pub colour: Colour,
    /// Twice the Gaussian's sigma, the way a canvas measures the blur of what it casts
    /// under a draw.
    pub blur: f64,
}

impl Default for Brush {
    /// Black ink, placed as drawn, at full strength, on top.
    fn default() -> Self {
        Self::new(Colour::BLACK)
    }
}

impl Brush {
    pub fn new(ink: impl Into<Ink>) -> Self {
        Self {
            ink: ink.into(),
            transform: Transform::IDENTITY,
            alpha: 1.0,
            blend: Blend::SourceOver,
            brightness: None,
            blur: 0.0,
            glow: None,
            smoothing: Smoothing::Bilinear,
        }
    }

    /// The same brush with other ink.
    pub fn ink(self, ink: impl Into<Ink>) -> Self {
        Self {
            ink: ink.into(),
            ..self
        }
    }

    /// Draws through `transform`, path, ink and picture alike.
    pub fn at(self, transform: Transform) -> Self {
        Self { transform, ..self }
    }

    /// How strongly the draw lands, from 0 to 1, as a canvas's `globalAlpha`.
    pub fn alpha(self, alpha: f64) -> Self {
        let alpha = if alpha.is_nan() {
            0.0
        } else {
            alpha.clamp(0.0, 1.0)
        };
        Self {
            alpha: alpha as f32,
            ..self
        }
    }

    pub fn blend(self, blend: Blend) -> Self {
        Self { blend, ..self }
    }

    /// Scales the draw's colours before it lands, as a canvas's `brightness()` filter: 0 makes
    /// a black silhouette of the same shape, 1 changes nothing, above 1 brightens.
    pub fn brightness(self, factor: f64) -> Self {
        Self {
            brightness: Some(factor),
            ..self
        }
    }

    /// Blurs the draw before it lands, as a canvas's `blur()` filter: a Gaussian of `sigma`
    /// pixels on the canvas, whatever the transform. It runs after any brightness.
    pub fn blur(self, sigma: f64) -> Self {
        Self {
            blur: sigma,
            ..self
        }
    }

    /// Lays a halo of `colour` under the draw: a copy of its shape blurred by `blur`, which
    /// is twice the Gaussian's sigma as a canvas measures what it casts under a draw, on the
    /// canvas whatever the transform. With no blur there is no halo, as on a canvas.
    pub fn glow(self, colour: Colour, blur: f64) -> Self {
        Self {
            glow: Some(Glow { colour, blur }),
            ..self
        }
    }

    /// How pictures drawn with this brush are read between their pixels.
    pub fn smoothing(self, smoothing: Smoothing) -> Self {
        Self { smoothing, ..self }
    }

    pub(super) fn ink_of(&self) -> &Ink {
        &self.ink
    }

    pub(super) fn transform(&self) -> Transform {
        self.transform
    }

    pub(super) fn strength(&self) -> f32 {
        self.alpha
    }

    pub(super) fn mix(&self) -> Blend {
        self.blend
    }

    pub(super) fn smoothing_of(&self) -> Smoothing {
        self.smoothing
    }

    pub(super) fn brightness_of(&self) -> Option<f64> {
        self.brightness
    }

    pub(super) fn blur_of(&self) -> f64 {
        self.blur
    }

    pub(super) fn glow_of(&self) -> Option<Glow> {
        self.glow
            .filter(|glow| glow.colour.a > 0.0 && glow.blur > 0.0)
    }

    /// Whether the draw has to be made on its own first and then laid on the canvas whole.
    pub(super) fn needs_layer(&self) -> bool {
        self.blend.reaches_everywhere()
            || self.brightness.is_some_and(|factor| factor != 1.0)
            || filter::spread(self.blur) > 0
            || self.glow_of().is_some()
    }
}
