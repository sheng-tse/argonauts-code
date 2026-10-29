/// Where a draw lands: a 2D affine map from the drawing's own space to the canvas's
/// pixels.
///
/// Each step applies to what is drawn inside it, as a canvas's `translate`, `rotate` and
/// `scale` do: `Transform::IDENTITY.translate(40.0, 20.0).rotate(a)` turns a sprite round its
/// own origin and then puts that origin at (40, 20).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Self = Self::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    /// The matrix in a canvas's `setTransform` order: a point (x, y) lands at
    /// (a·x + c·y + e, b·x + d·y + f).
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub fn translate(self, x: f64, y: f64) -> Self {
        self.then(Self::new(1.0, 0.0, 0.0, 1.0, x, y))
    }

    /// Turns by `radians`, clockwise on the screen.
    pub fn rotate(self, radians: f64) -> Self {
        let (sin, cos) = radians.sin_cos();
        self.then(Self::new(cos, sin, -sin, cos, 0.0, 0.0))
    }

    pub fn scale(self, x: f64, y: f64) -> Self {
        self.then(Self::new(x, 0.0, 0.0, y, 0.0, 0.0))
    }

    /// `inner` applied first, then this transform, as a canvas's `transform` call composes.
    pub fn then(self, inner: Self) -> Self {
        Self {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    /// Where the point (x, y) lands.
    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    pub(super) fn skia(self) -> tiny_skia::Transform {
        tiny_skia::Transform::from_row(
            self.a as f32,
            self.b as f32,
            self.c as f32,
            self.d as f32,
            self.e as f32,
            self.f as f32,
        )
    }
}
