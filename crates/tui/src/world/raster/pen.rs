/// How a path is turned into a line: its width, its ends, its corners and its dashes.
#[derive(Debug, Clone, PartialEq)]
pub struct Pen {
    width: f64,
    cap: Cap,
    join: Join,
    dash: Vec<f64>,
    dash_offset: f64,
}

/// How a line ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cap {
    /// Square, at the end itself.
    #[default]
    Butt,
    /// A half circle past the end.
    Round,
    /// A half square past the end.
    Square,
}

/// How a line turns a corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Join {
    /// A sharp point, cut short where it would reach past ten times the half width.
    #[default]
    Miter,
    Round,
    Bevel,
}

impl Pen {
    /// A pen `width` pixels wide with square ends and sharp corners, as a canvas starts.
    pub fn new(width: f64) -> Self {
        Self {
            width,
            cap: Cap::Butt,
            join: Join::Miter,
            dash: Vec::new(),
            dash_offset: 0.0,
        }
    }

    /// Round ends and round corners, the soft line most of the world is drawn with.
    pub fn round(self) -> Self {
        self.cap(Cap::Round).join(Join::Round)
    }

    pub fn cap(self, cap: Cap) -> Self {
        Self { cap, ..self }
    }

    pub fn join(self, join: Join) -> Self {
        Self { join, ..self }
    }

    /// Dashes and gaps in turn, in pixels along the line. A list of odd length runs twice
    /// over, and one with a negative or endless length draws a solid line, as a canvas's
    /// `setLineDash` does.
    pub fn dash(self, pattern: &[f64]) -> Self {
        let usable = pattern.iter().all(|v| v.is_finite() && *v >= 0.0);
        let mut dash = if usable { pattern.to_vec() } else { Vec::new() };
        if dash.len() % 2 == 1 {
            dash.extend_from_within(..);
        }
        Self { dash, ..self }
    }

    /// How far into the pattern the line starts, as a canvas's `lineDashOffset`.
    pub fn dash_offset(self, offset: f64) -> Self {
        Self {
            dash_offset: offset,
            ..self
        }
    }

    pub fn width(&self) -> f64 {
        self.width
    }

    /// How far past the path the line can reach, in the path's own units.
    pub(super) fn reach(&self) -> f64 {
        let half = self.width.max(0.0) / 2.0;
        let corner = match self.join {
            Join::Miter => MITER_LIMIT,
            Join::Round | Join::Bevel => 1.0,
        };
        let end = match self.cap {
            Cap::Square => std::f64::consts::SQRT_2,
            Cap::Butt | Cap::Round => 1.0,
        };
        half * corner.max(end)
    }

    pub(super) fn skia(&self) -> tiny_skia::Stroke {
        let dash = (self.dash.iter().sum::<f64>() > 0.0)
            .then(|| {
                tiny_skia::StrokeDash::new(
                    self.dash.iter().map(|&v| v as f32).collect(),
                    self.dash_offset as f32,
                )
            })
            .flatten();
        tiny_skia::Stroke {
            width: self.width.max(0.0) as f32,
            miter_limit: MITER_LIMIT as f32,
            line_cap: match self.cap {
                Cap::Butt => tiny_skia::LineCap::Butt,
                Cap::Round => tiny_skia::LineCap::Round,
                Cap::Square => tiny_skia::LineCap::Square,
            },
            line_join: match self.join {
                Join::Miter => tiny_skia::LineJoin::Miter,
                Join::Round => tiny_skia::LineJoin::Round,
                Join::Bevel => tiny_skia::LineJoin::Bevel,
            },
            dash,
        }
    }
}

/// A canvas's default miter limit.
const MITER_LIMIT: f64 = 10.0;
