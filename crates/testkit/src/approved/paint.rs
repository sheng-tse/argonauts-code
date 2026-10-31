//! What a terminal paints for a cell: its colours resolved through the terminal's palette,
//! inverse and faint text as drawn, and the block glyphs Ghostty draws as flat fills.

use argonauts_core::look::logos::LogoMark;

use crate::screen::{Cell, Colour, PLACEHOLDER, Placeholder, Rgb};

/// The colours a terminal paints with: its own text colour and background, its sixteen named
/// colours, and how much of its text colour faint text keeps over the background.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub text: Rgb,
    pub background: Rgb,
    pub ansi: [Rgb; 16],
    pub faint: f64,
}

/// A terminal's own text colour and background, which every unstyled cell is painted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal {
    pub text: Rgb,
    pub background: Rgb,
}

impl Terminal {
    /// Whether its background is light, where a one-colour logo takes its ink for light screens.
    pub fn is_light(&self) -> bool {
        let Rgb(red, green, blue) = self.background;
        let luma = 299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue);
        luma > 127_500
    }
}

impl Palette {
    /// Ghostty with its TokyoNight Moon theme, which the drawings are drawn for. Ghostty draws
    /// faint text at half strength and bold text in its own colour.
    pub const TOKYONIGHT_MOON: Palette = Palette {
        text: Rgb(0xc8, 0xd3, 0xf5),
        background: Rgb(0x22, 0x24, 0x36),
        ansi: [
            Rgb(0x1b, 0x1d, 0x2b),
            Rgb(0xff, 0x75, 0x7f),
            Rgb(0xc3, 0xe8, 0x8d),
            Rgb(0xff, 0xc7, 0x77),
            Rgb(0x82, 0xaa, 0xff),
            Rgb(0xc0, 0x99, 0xff),
            Rgb(0x86, 0xe1, 0xfc),
            Rgb(0x82, 0x8b, 0xb8),
            Rgb(0x44, 0x4a, 0x73),
            Rgb(0xff, 0x75, 0x7f),
            Rgb(0xc3, 0xe8, 0x8d),
            Rgb(0xff, 0xc7, 0x77),
            Rgb(0x82, 0xaa, 0xff),
            Rgb(0xc0, 0x99, 0xff),
            Rgb(0x86, 0xe1, 0xfc),
            Rgb(0xc8, 0xd3, 0xf5),
        ],
        faint: 0.5,
    };

    /// Ghostty with its TokyoNight Day theme, the light twin of TokyoNight Moon, for drawings
    /// made on a light terminal.
    pub const TOKYONIGHT_DAY: Palette = Palette {
        text: Rgb(0x37, 0x60, 0xbf),
        background: Rgb(0xe1, 0xe2, 0xe7),
        ansi: [
            Rgb(0xe9, 0xe9, 0xed),
            Rgb(0xf5, 0x2a, 0x65),
            Rgb(0x58, 0x75, 0x39),
            Rgb(0x8c, 0x6c, 0x3e),
            Rgb(0x2e, 0x7d, 0xe9),
            Rgb(0x98, 0x54, 0xf1),
            Rgb(0x00, 0x71, 0x97),
            Rgb(0x61, 0x72, 0xb0),
            Rgb(0xa1, 0xa6, 0xc5),
            Rgb(0xf5, 0x2a, 0x65),
            Rgb(0x58, 0x75, 0x39),
            Rgb(0x8c, 0x6c, 0x3e),
            Rgb(0x2e, 0x7d, 0xe9),
            Rgb(0x98, 0x54, 0xf1),
            Rgb(0x00, 0x71, 0x97),
            Rgb(0x37, 0x60, 0xbf),
        ],
        faint: 0.5,
    };

    /// What a drawing made on `terminal` is painted in: Ghostty's TokyoNight Day on a light
    /// terminal and Moon on a dark one, with the terminal's own colours.
    pub fn for_terminal(terminal: Terminal) -> Palette {
        let theme = if terminal.is_light() {
            Palette::TOKYONIGHT_DAY
        } else {
            Palette::TOKYONIGHT_MOON
        };
        theme.on(terminal)
    }

    /// The same terminal with `terminal`'s own colours, as a drawing names them.
    pub fn on(&self, terminal: Terminal) -> Palette {
        Palette {
            text: terminal.text,
            background: terminal.background,
            ..*self
        }
    }

    pub fn terminal(&self) -> Terminal {
        Terminal {
            text: self.text,
            background: self.background,
        }
    }

    /// One of the 256 colours: the theme's sixteen, then xterm's 6 × 6 × 6 cube and 24 greys.
    pub fn indexed(&self, index: u8) -> Rgb {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let cube = index - 16;
                Rgb(
                    LEVELS[usize::from(cube / 36)],
                    LEVELS[usize::from(cube / 6 % 6)],
                    LEVELS[usize::from(cube % 6)],
                )
            }
            232..=255 => {
                let grey = 8 + 10 * (index - 232);
                Rgb(grey, grey, grey)
            }
        }
    }

    fn resolve(&self, colour: Colour, own: Rgb) -> Rgb {
        match colour {
            Colour::Own => own,
            Colour::Indexed(index) => self.indexed(index),
            Colour::Rgb(rgb) => rgb,
        }
    }
}

/// A cell as the terminal paints it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Painted<'a> {
    pub text: &'a str,
    pub ink: Rgb,
    pub fill: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// The picture whose placeholder the cell holds, and whether its screen holds it.
    pub picture: Option<Placeholder>,
}

impl Cell {
    pub(crate) fn painted(&self, palette: &Palette) -> Painted<'_> {
        let mut ink = palette.resolve(self.ink, palette.text);
        let mut fill = palette.resolve(self.background, palette.background);
        if self.inverse {
            std::mem::swap(&mut ink, &mut fill);
        }
        if self.dim {
            ink = ink.over(fill, palette.faint);
        }
        Painted {
            text: &self.text,
            ink,
            fill,
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            picture: self.picture,
        }
    }
}

/// The glyphs Ghostty draws as one flat colour over the whole cell: this much of the text
/// colour over the background.
const FLAT_FILLS: [(char, f64); 4] = [('█', 1.0), ('▓', 0.75), ('▒', 0.5), ('░', 0.25)];

/// The one colour a cell is painted in over the whole of it, a blank's background or a flat
/// fill's blend, or `None` for a glyph, whose colour and background both show.
pub(crate) fn solid(text: &str, ink: Rgb, fill: Rgb) -> Option<Rgb> {
    if blank(text) {
        return Some(fill);
    }
    flat_fill(text).map(|strength| ink.over(fill, strength))
}

/// Whether a cell shows one colour over the whole of it, whatever its colours.
pub(crate) fn solid_text(text: &str) -> bool {
    blank(text) || flat_fill(text).is_some()
}

/// A cell that shows only its background: never written, a blank, or the right half of a wide
/// glyph.
pub(crate) fn blank(text: &str) -> bool {
    text.chars().all(|c| c == ' ')
}

/// A cell whose glyph's weight and slant show: text, not a blank or a flat fill.
pub(crate) fn shows_glyph(text: &str) -> bool {
    !solid_text(text)
}

/// Whether the background shows anywhere in the cell, around its glyph or through it.
pub(crate) fn shows_fill(text: &str) -> bool {
    flat_fill(text) != Some(1.0)
}

fn flat_fill(text: &str) -> Option<f64> {
    let mut chars = text.chars();
    let glyph = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    FLAT_FILLS
        .iter()
        .find(|(block, _)| *block == glyph)
        .map(|&(_, strength)| strength)
}

/// The protocol's first two diacritics, which number a placeholder's row or column 0 and 1.
const DIACRITICS: [char; 2] = ['\u{0305}', '\u{030D}'];

/// The text of a logo's left (`half` 0) or right (1) cell: the top row of its picture.
pub(crate) fn logo_half(half: usize) -> String {
    [PLACEHOLDER, DIACRITICS[0], DIACRITICS[half % 2]]
        .iter()
        .collect()
}

/// The id of a logo's picture on a dark screen, plain or numbered `number`, as the app numbers
/// them (`LogoMark::picture_id`).
pub(crate) fn logo_id(stem: &str, number: Option<u8>) -> Option<u32> {
    let mark = LogoMark::ALL.into_iter().find(|mark| mark.stem() == stem)?;
    match number {
        Some(number) if !(1..=LogoMark::MOST_NUMBERED).contains(&number) => None,
        _ => Some(mark.picture_id(number, false)),
    }
}

/// The logo a picture id shows in either ink, by its picture's name, and its number for a
/// numbered one.
pub(crate) fn logo_of(id: u32) -> Option<(&'static str, Option<u8>)> {
    LogoMark::of_picture(id).map(|(mark, number)| (mark.stem(), number))
}

/// Whether a logo's picture id is its ink for light screens.
pub(crate) fn light_ink(id: u32) -> bool {
    id % 2 == 1
}

/// A placeholder's picture in words, for a report, saying so when its screen doesn't hold it.
pub(crate) fn picture_words(picture: Placeholder, text: &str) -> String {
    let words = named_picture(picture.id, text);
    if picture.shows {
        words
    } else {
        format!("{words}, blank since its screen doesn't hold the picture")
    }
}

fn named_picture(id: u32, text: &str) -> String {
    let half = if text.ends_with(DIACRITICS[1]) {
        "right"
    } else {
        "left"
    };
    let ink = if light_ink(id) {
        ", in its ink for light screens"
    } else {
        ""
    };
    match logo_of(id) {
        Some((stem, None)) => format!("the {stem} logo's {half} half{ink}"),
        Some((stem, Some(number))) => {
            format!("the {stem} logo numbered {number}, {half} half{ink}")
        }
        None => format!("a picture, id {id:#08x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_light_terminal_is_painted_in_the_light_theme_and_its_own_colours() {
        let white = Terminal {
            text: Rgb(0, 0, 0),
            background: Rgb(0xff, 0xff, 0xff),
        };
        let light = Palette::for_terminal(white);
        assert_eq!(light.terminal(), white);
        assert_eq!(light.indexed(4), Palette::TOKYONIGHT_DAY.indexed(4));
        let moon = Palette::TOKYONIGHT_MOON;
        assert_eq!(Palette::for_terminal(moon.terminal()), moon);
    }
}
