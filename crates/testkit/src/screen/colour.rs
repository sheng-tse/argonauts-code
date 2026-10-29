//! A colour as a screen's cell holds it, and the true colours a terminal paints.

use std::fmt;

/// A colour as the program set it: the terminal's own, one of its 256, or spelled out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Colour {
    /// The terminal's own text colour or background.
    #[default]
    Own,
    /// One of the 256 colours a terminal numbers, the first 16 its named ones.
    Indexed(u8),
    Rgb(Rgb),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#rrggbb`.
    pub fn parse(hex: &str) -> Option<Rgb> {
        let digits = hex.strip_prefix('#').filter(|digits| digits.len() == 6)?;
        let [_, red, green, blue] = u32::from_str_radix(digits, 16).ok()?.to_be_bytes();
        Some(Rgb(red, green, blue))
    }

    /// The true colour a kitty placeholder names picture `id` by, its low 24 bits.
    pub fn from_id(id: u32) -> Rgb {
        let [_, red, green, blue] = id.to_be_bytes();
        Rgb(red, green, blue)
    }

    /// This colour laid over `under` at `strength`, as a terminal blends a fill or faint text.
    pub(crate) fn over(self, under: Rgb, strength: f64) -> Rgb {
        let mix = |top: u8, bottom: u8| {
            let value = f64::from(top) * strength + f64::from(bottom) * (1.0 - strength);
            value.round().clamp(0.0, 255.0) as u8
        };
        Rgb(
            mix(self.0, under.0),
            mix(self.1, under.1),
            mix(self.2, under.2),
        )
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl Colour {
    /// Whether the colour is a grey: one of the terminal's two named greys or its white, the
    /// 256-colour ramp, or a true colour whose channels stand within a few steps of each other
    /// or within a quarter of the brightest.
    pub fn is_grey(self) -> bool {
        match self {
            Colour::Own => false,
            Colour::Indexed(at) => matches!(at, 7 | 8 | 15 | 232..=255),
            Colour::Rgb(Rgb(r, g, b)) => {
                let high = r.max(g).max(b);
                let spread = high - r.min(g).min(b);
                spread <= GREY_SPREAD || u16::from(spread) * GREY_TINT <= u16::from(high)
            }
        }
    }

    /// The picture a kitty placeholder in this text colour names: a true colour holds an id of
    /// up to 24 bits, an indexed one of up to 8. The terminal's own colour names none, which is
    /// id 0, and no picture is kept under 0. A third diacritic for a higher byte isn't read.
    pub fn picture_id(self) -> u32 {
        match self {
            Colour::Own => 0,
            Colour::Indexed(index) => u32::from(index),
            Colour::Rgb(Rgb(red, green, blue)) => u32::from_be_bytes([0, red, green, blue]),
        }
    }
}

/// How far apart a grey's channels may stand. The theme's greys are mixed from a tinted
/// background, so they lean toward blue: a dark one a few steps, a light one up to a quarter
/// of its brightest channel, as the grey the models past a CLI's first few share does.
const GREY_SPREAD: u8 = 16;
const GREY_TINT: u16 = 4;

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Colour::Own => f.write_str("default"),
            Colour::Indexed(at) => write!(f, "colour {at}"),
            Colour::Rgb(rgb) => rgb.fmt(f),
        }
    }
}

impl From<vt100::Color> for Colour {
    fn from(colour: vt100::Color) -> Colour {
        match colour {
            vt100::Color::Default => Colour::Own,
            vt100::Color::Idx(index) => Colour::Indexed(index),
            vt100::Color::Rgb(red, green, blue) => Colour::Rgb(Rgb(red, green, blue)),
        }
    }
}
