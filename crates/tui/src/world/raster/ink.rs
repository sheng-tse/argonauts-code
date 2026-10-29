use tiny_skia::{
    GradientStop, LinearGradient, Point, RadialGradient, Shader, SpreadMode, SweepGradient,
};

use super::colour::Colour;

/// What a fill or a line is painted with.
#[derive(Debug, Clone, PartialEq)]
pub enum Ink {
    Colour(Colour),
    Gradient(Gradient),
}

impl From<Colour> for Ink {
    fn from(colour: Colour) -> Self {
        Self::Colour(colour)
    }
}

impl From<Gradient> for Ink {
    fn from(gradient: Gradient) -> Self {
        Self::Gradient(gradient)
    }
}

/// Colours that change across a draw, laid out in the drawing's own space as a
/// canvas's gradients are. Past the first and last stops the end colours carry on.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    shape: Shape,
    stops: Vec<(f64, Colour)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Linear {
        from: (f64, f64),
        to: (f64, f64),
    },
    Radial {
        from: (f64, f64, f64),
        to: (f64, f64, f64),
    },
    Conic {
        centre: (f64, f64),
        angle: f64,
    },
}

impl Gradient {
    /// Colours along the line from (x0, y0) to (x1, y1), the same across it.
    pub fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self::of(Shape::Linear {
            from: (x0, y0),
            to: (x1, y1),
        })
    }

    /// Colours from the circle round (x0, y0) of radius `r0` out to the circle round (x1, y1)
    /// of radius `r1`. Where the circles don't nest, only the cone they sweep out is painted.
    pub fn radial(x0: f64, y0: f64, r0: f64, x1: f64, y1: f64, r1: f64) -> Self {
        Self::of(Shape::Radial {
            from: (x0, y0, r0),
            to: (x1, y1, r1),
        })
    }

    /// Colours round (x, y), starting at `angle` in radians and turning clockwise once.
    pub fn conic(angle: f64, x: f64, y: f64) -> Self {
        Self::of(Shape::Conic {
            centre: (x, y),
            angle,
        })
    }

    /// The colour at `offset`, from 0 at the start to 1 at the end. Stops at the same offset
    /// keep their order, so two of them make a hard edge.
    pub fn stop(mut self, offset: f64, colour: Colour) -> Self {
        if offset.is_finite() {
            self.stops.push((offset.clamp(0.0, 1.0), colour));
        }
        self
    }

    fn of(shape: Shape) -> Self {
        Self {
            shape,
            stops: Vec::new(),
        }
    }

    fn shader(&self, alpha: f32) -> Shader<'static> {
        let mut stops = self.stops.clone();
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        let stops: Vec<GradientStop> = stops
            .iter()
            .map(|&(offset, colour)| GradientStop::new(offset as f32, colour.faded(alpha).skia()))
            .collect();
        if stops.is_empty() || self.paints_nothing() {
            return Shader::SolidColor(tiny_skia::Color::TRANSPARENT);
        }
        let point = |(x, y): (f64, f64)| Point::from_xy(x as f32, y as f32);
        let identity = tiny_skia::Transform::identity();
        let shader = match self.shape {
            Shape::Linear { from, to } => {
                LinearGradient::new(point(from), point(to), stops, SpreadMode::Pad, identity)
            }
            Shape::Radial {
                from: (x0, y0, r0),
                to: (x1, y1, r1),
            } => RadialGradient::new(
                point((x0, y0)),
                r0 as f32,
                point((x1, y1)),
                r1 as f32,
                stops,
                SpreadMode::Pad,
                identity,
            ),
            Shape::Conic { centre, angle } => SweepGradient::new(
                point(centre),
                0.0,
                360.0,
                stops,
                SpreadMode::Pad,
                tiny_skia::Transform::from_rotate_at(
                    angle.to_degrees() as f32,
                    centre.0 as f32,
                    centre.1 as f32,
                ),
            ),
        };
        shader.unwrap_or(Shader::SolidColor(tiny_skia::Color::TRANSPARENT))
    }

    /// A line with no length, or two equal circles in one place, give a gradient no
    /// direction to run in, and a canvas paints nothing with it.
    fn paints_nothing(&self) -> bool {
        match self.shape {
            Shape::Linear { from, to } => from == to,
            Shape::Radial { from, to } => from == to || from.2 < 0.0 || to.2 < 0.0,
            Shape::Conic { .. } => false,
        }
    }
}

impl Ink {
    /// The shader that lays this ink down at `alpha` of its own strength.
    pub(super) fn shader(&self, alpha: f32) -> Shader<'static> {
        match self {
            Self::Colour(colour) => Shader::SolidColor(colour.faded(alpha).skia()),
            Self::Gradient(gradient) => gradient.shader(alpha),
        }
    }
}
