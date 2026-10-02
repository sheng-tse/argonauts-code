//! The modifier diff follows Codex's `ModifierDiff` (Apache-2.0), with one fix: bold and dim
//! share one reset code, so removing either one sets the other again when it stays.

use std::io::{self, Write};

use crossterm::queue;
use crossterm::style::{
    Attribute, Colors, SetAttribute, SetBackgroundColor, SetColors, SetForegroundColor,
};
use ratatui::backend::IntoCrossterm;
use ratatui::style::{Color, Modifier};

/// The colors and attributes the terminal draws with right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pen {
    fg: Color,
    bg: Color,
    modifier: Modifier,
}

impl Default for Pen {
    /// The state after `SGR 0`.
    fn default() -> Self {
        Self {
            fg: Color::Reset,
            bg: Color::Reset,
            modifier: Modifier::empty(),
        }
    }
}

impl Pen {
    pub(crate) fn set<W: Write>(
        &mut self,
        writer: &mut W,
        fg: Color,
        bg: Color,
        modifier: Modifier,
    ) -> io::Result<()> {
        if modifier != self.modifier {
            queue_modifier_change(writer, self.modifier, modifier)?;
            self.modifier = modifier;
        }
        match (fg != self.fg, bg != self.bg) {
            (true, true) => queue!(
                writer,
                SetColors(Colors::new(fg.into_crossterm(), bg.into_crossterm()))
            )?,
            (true, false) => queue!(writer, SetForegroundColor(fg.into_crossterm()))?,
            (false, true) => queue!(writer, SetBackgroundColor(bg.into_crossterm()))?,
            (false, false) => {}
        }
        self.fg = fg;
        self.bg = bg;
        Ok(())
    }

    pub(crate) fn reset<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        queue!(writer, SetAttribute(Attribute::Reset))?;
        *self = Self::default();
        Ok(())
    }

    /// Gets ready to erase in `bg`: attributes such as reverse would show on erased cells, so
    /// they go first, and only the background matters after that.
    pub(crate) fn prepare_erase<W: Write>(&mut self, writer: &mut W, bg: Color) -> io::Result<()> {
        if !self.modifier.is_empty() {
            self.reset(writer)?;
        }
        if self.bg != bg {
            queue!(writer, SetBackgroundColor(bg.into_crossterm()))?;
            self.bg = bg;
        }
        Ok(())
    }

    pub(crate) fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if *self != Self::default() {
            self.reset(writer)?;
        }
        Ok(())
    }
}

fn queue_modifier_change<W: Write>(writer: &mut W, from: Modifier, to: Modifier) -> io::Result<()> {
    let removed = from - to;
    let mut added = to - from;
    let intensity = Modifier::BOLD | Modifier::DIM;
    if removed.intersects(intensity) {
        queue!(writer, SetAttribute(Attribute::NormalIntensity))?;
        added |= to & intensity;
    }
    let blink = Modifier::SLOW_BLINK | Modifier::RAPID_BLINK;
    if removed.intersects(blink) {
        queue!(writer, SetAttribute(Attribute::NoBlink))?;
        added |= to & blink;
    }
    let offs = [
        (Modifier::ITALIC, Attribute::NoItalic),
        (Modifier::UNDERLINED, Attribute::NoUnderline),
        (Modifier::REVERSED, Attribute::NoReverse),
        (Modifier::CROSSED_OUT, Attribute::NotCrossedOut),
        (Modifier::HIDDEN, Attribute::NoHidden),
    ];
    for (modifier, attribute) in offs {
        if removed.contains(modifier) {
            queue!(writer, SetAttribute(attribute))?;
        }
    }
    let ons = [
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::DIM, Attribute::Dim),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::REVERSED, Attribute::Reverse),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
        (Modifier::SLOW_BLINK, Attribute::SlowBlink),
        (Modifier::RAPID_BLINK, Attribute::RapidBlink),
        (Modifier::HIDDEN, Attribute::Hidden),
    ];
    for (modifier, attribute) in ons {
        if added.contains(modifier) {
            queue!(writer, SetAttribute(attribute))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(from: Modifier, to: Modifier) -> String {
        let mut out = Vec::new();
        queue_modifier_change(&mut out, from, to).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn dropping_dim_keeps_bold() {
        assert_eq!(
            codes(Modifier::BOLD | Modifier::DIM, Modifier::BOLD),
            "\x1b[22m\x1b[1m"
        );
        assert_eq!(codes(Modifier::BOLD, Modifier::DIM), "\x1b[22m\x1b[2m");
        assert_eq!(codes(Modifier::empty(), Modifier::ITALIC), "\x1b[3m");
        assert_eq!(codes(Modifier::UNDERLINED, Modifier::empty()), "\x1b[24m");
    }

    #[test]
    fn pen_writes_only_changes() {
        crossterm::style::force_color_output(true);
        let mut pen = Pen::default();
        let mut out = Vec::new();
        pen.set(&mut out, Color::Reset, Color::Reset, Modifier::empty())
            .unwrap();
        assert!(out.is_empty());
        pen.set(&mut out, Color::Red, Color::Reset, Modifier::BOLD)
            .unwrap();
        pen.set(&mut out, Color::Red, Color::Reset, Modifier::BOLD)
            .unwrap();
        pen.set(&mut out, Color::Green, Color::Blue, Modifier::BOLD)
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b[1m\x1b[38;5;1m\x1b[38;5;2;48;5;4m"
        );
    }
}
