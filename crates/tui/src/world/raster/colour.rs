/// A colour as a stylesheet writes one: red, green and blue from 0 to 255 and an alpha from 0
/// to 1, not premultiplied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colour {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
}

impl Colour {
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0.0,
    };
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// `alpha` is held to 0..=1.
    pub fn rgba(r: u8, g: u8, b: u8, alpha: f64) -> Self {
        Self::rgb(r, g, b).with_alpha(alpha)
    }

    /// `0x1f202f` is red 0x1f, green 0x20 and blue 0x2f.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// A colour worked out in fractions, each channel cut to a whole number toward zero and
    /// held to 0..=255, the way a stylesheet reads `rgba(12.7, -3, 300, 0.5)`.
    pub fn of(rgb: [f64; 3], alpha: f64) -> Self {
        let channel = |value: f64| (value as i64).clamp(0, 255) as u8;
        Self::rgb(channel(rgb[0]), channel(rgb[1]), channel(rgb[2])).with_alpha(alpha)
    }

    /// The same colour at `alpha`, held to 0..=1. Not a number counts as clear.
    pub fn with_alpha(self, alpha: f64) -> Self {
        let alpha = if alpha.is_nan() {
            0.0
        } else {
            alpha.clamp(0.0, 1.0)
        };
        Self {
            a: alpha as f32,
            ..self
        }
    }

    /// The colour faded by `alpha`, which multiplies the alpha it has.
    pub(super) fn faded(self, alpha: f32) -> Self {
        Self {
            a: (self.a * alpha).clamp(0.0, 1.0),
            ..self
        }
    }

    pub(super) fn skia(self) -> tiny_skia::Color {
        let mut colour = tiny_skia::Color::from_rgba8(self.r, self.g, self.b, 255);
        colour.set_alpha(self.a.clamp(0.0, 1.0));
        colour
    }
}
