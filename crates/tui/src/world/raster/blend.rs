use tiny_skia::BlendMode;

/// How a draw mixes with what the canvas already holds, as a canvas's
/// `globalCompositeOperation` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Blend {
    /// On top: the usual way.
    #[default]
    SourceOver,
    /// Light added to light, so overlapping glows brighten.
    Lighter,
    /// Lightens as two slides projected on one screen do.
    Screen,
    /// On top, but only where the canvas already holds something.
    SourceAtop,
    /// Keeps what the canvas holds only where the draw covers it, and clears the rest of the
    /// canvas, even far from the draw.
    DestinationIn,
    /// Rubs out what the canvas holds where the draw covers it.
    DestinationOut,
}

impl Blend {
    /// Whether the mix changes the canvas outside the draw's own shape too.
    pub(super) fn reaches_everywhere(self) -> bool {
        self == Self::DestinationIn
    }

    pub(super) fn skia(self) -> BlendMode {
        match self {
            Self::SourceOver => BlendMode::SourceOver,
            Self::Lighter => BlendMode::Plus,
            Self::Screen => BlendMode::Screen,
            Self::SourceAtop => BlendMode::SourceAtop,
            Self::DestinationIn => BlendMode::DestinationIn,
            Self::DestinationOut => BlendMode::DestinationOut,
        }
    }
}
