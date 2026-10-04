//! Claude Code's keyword rainbow, from the 2.1.289 program text. Its dark, light, and color-blind
//! themes all use the same seven colors, and both ANSI themes the same named ones.

use ratatui::style::{Color, Style};

use super::Theme;
use super::color::{ColorLevel, nearest_ansi256};

/// One letter's color and the lighter one the shimmer lights it with, in true color and in the
/// ANSI themes' named colors.
struct Band {
    color: (u8, u8, u8),
    shimmer: (u8, u8, u8),
    named: Color,
    named_shimmer: Color,
}

const fn band(color: (u8, u8, u8), shimmer: (u8, u8, u8), named: [Color; 2]) -> Band {
    Band {
        color,
        shimmer,
        named: named[0],
        named_shimmer: named[1],
    }
}

/// Red, orange, yellow, green, blue, indigo, violet.
#[rustfmt::skip]
const BANDS: [Band; 7] = [
    band((235, 95, 87),   (250, 155, 147), [Color::Red, Color::LightRed]),
    band((245, 139, 87),  (255, 185, 137), [Color::LightRed, Color::Yellow]),
    band((250, 195, 95),  (255, 225, 155), [Color::Yellow, Color::LightYellow]),
    band((145, 200, 130), (185, 230, 180), [Color::Green, Color::LightGreen]),
    band((130, 170, 220), (180, 205, 240), [Color::Cyan, Color::LightCyan]),
    band((155, 130, 200), (195, 180, 230), [Color::Blue, Color::LightBlue]),
    band((200, 130, 180), (230, 180, 210), [Color::Magenta, Color::LightMagenta]),
];

impl Theme {
    /// The color of a keyword's `index`th letter, or its lighter shade while the shimmer passes.
    /// Without color the letter stays plain.
    pub fn rainbow(&self, index: usize, lit: bool) -> Style {
        let band = &BANDS[index % BANDS.len()];
        let (rgb, named) = if lit {
            (band.shimmer, band.named_shimmer)
        } else {
            (band.color, band.named)
        };
        let color = match self.level {
            ColorLevel::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
            ColorLevel::Ansi256 => Color::Indexed(nearest_ansi256(rgb)),
            ColorLevel::Ansi16 => named,
            ColorLevel::None => return Style::new(),
        };
        Style::new().fg(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_cycle_through_seven_colors() {
        let theme = Theme::dark();
        assert_eq!(theme.rainbow(0, false).fg, Some(Color::Rgb(235, 95, 87)));
        assert_eq!(theme.rainbow(6, false).fg, Some(Color::Rgb(200, 130, 180)));
        assert_eq!(theme.rainbow(7, false), theme.rainbow(0, false));
        assert_eq!(theme.rainbow(2, true).fg, Some(Color::Rgb(255, 225, 155)));
    }

    #[test]
    fn light_and_dark_share_the_colors() {
        for index in 0..7 {
            for lit in [false, true] {
                assert_eq!(
                    Theme::light().rainbow(index, lit),
                    Theme::dark().rainbow(index, lit)
                );
            }
        }
    }

    #[test]
    fn fewer_colors_use_claude_codes_named_ones_or_none() {
        let ansi = Theme::dark().with_level(ColorLevel::Ansi16);
        let named: Vec<_> = (0..7).map(|i| ansi.rainbow(i, false).fg.unwrap()).collect();
        assert_eq!(
            named,
            [
                Color::Red,
                Color::LightRed,
                Color::Yellow,
                Color::Green,
                Color::Cyan,
                Color::Blue,
                Color::Magenta
            ]
        );
        assert_eq!(ansi.rainbow(0, true).fg, Some(Color::LightRed));
        let indexed = Theme::dark().with_level(ColorLevel::Ansi256);
        assert!(matches!(
            indexed.rainbow(0, false).fg,
            Some(Color::Indexed(_))
        ));
        let plain = Theme::dark().with_level(ColorLevel::None);
        assert_eq!(plain.rainbow(0, false), Style::new());
    }
}
