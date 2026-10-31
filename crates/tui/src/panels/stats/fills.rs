//! What a part of Stats draws in: its colour and, for a model, the texture that tells it from
//! its CLI's other models; and the swatch that stands for a part in a legend or a table.

use ratatui::style::Style;
use ratatui::text::Span;

use super::data::{Cli, Model};
use super::draw::{self, THIN_PARTS, fill_style};
use crate::base::kit::Kit;
use crate::base::theme::shades::texture;

/// What a part draws in: its colour and, for a model, the texture that tells it from the
/// other models of its CLI, whose hue it shares. A CLI or a role draws solid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::panels) struct Fill {
    pub style: Style,
    pub texture: Option<&'static str>,
}

impl Fill {
    /// The fill of the model at `place` among its CLI's, in `style`.
    pub fn model(kit: &Kit<'_>, style: Style, place: usize) -> Self {
        Self {
            style,
            texture: Some(texture(kit, place)),
        }
    }
}

impl From<Style> for Fill {
    fn from(style: Style) -> Self {
        Self {
            style,
            texture: None,
        }
    }
}

/// The model's color: its CLI's hue, or the grey of the rest after its CLI's third model.
pub(super) fn model_style(kit: &Kit<'_>, cli: &Cli, model: &Model) -> Style {
    draw::shade(kit, cli.role(), model.place)
}

/// The model's color with the texture that tells it from its CLI's other models.
pub(super) fn model_fill(kit: &Kit<'_>, cli: &Cli, model: &Model) -> Fill {
    Fill::model(kit, model_style(kit, cli, model), model.place)
}

/// A legend's swatch, then a blank you can see before the name: a square, or a cell of a
/// model's texture.
pub(in crate::panels) fn swatch(kit: &Kit<'_>, fill: impl Into<Fill>) -> Vec<Span<'static>> {
    let fill = fill.into();
    let (square, blank) = match fill.texture {
        Some(texture) => (texture, " "),
        // Iosevka paints the square two cells wide, over the first blank after it.
        None if kit.glyphs.halves.is_some() => ("■", "  "),
        None => ("#", " "),
    };
    vec![
        Span::styled(square, fill_style(kit, fill.style)),
        Span::raw(blank),
    ]
}

/// A table row's swatch, two cells of the part's texture or color. Without color it is the
/// line the part's bar draws in, the `k`th of a thin bar's, so the row still names its part.
pub(super) fn table_swatch(kit: &Kit<'_>, fill: Fill, k: usize) -> Span<'static> {
    let glyph = fill.texture.unwrap_or(if kit.has_color() {
        "█"
    } else {
        THIN_PARTS[k % THIN_PARTS.len()]
    });
    Span::styled(glyph.repeat(2), fill_style(kit, fill.style))
}
