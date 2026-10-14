//! What the kit draws when color or Unicode can't carry the meaning, and the words a screen
//! reader hears in place of a card.
//!
//! Without color a bar is used `█` against free `░` with a `│` tick, and a badge is `[21%]`,
//! the same width as its colored form so nothing moves. With `ui.glyphs = "ascii"` frames are
//! `+-|`, the mod glyph `*`, swatches `#`, and pills `#-`.

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KitGlyphs {
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub across: &'static str,
    pub down: &'static str,
    /// Before a mod's title.
    pub the_mod: &'static str,
    /// A legend's color swatch.
    pub swatch: &'static str,
    /// Before a folded mod's line.
    pub folded: &'static str,
    /// A bar's cells without color.
    pub used: &'static str,
    pub free: &'static str,
    pub tick: &'static str,
    /// A cell's left part, in eighths, for bars drawn in color: index 1 is `▏`, 7 is `▉`.
    pub eighths: Option<[&'static str; 8]>,
    pub line_fill: &'static str,
    pub line_half: &'static str,
    pub line_track: &'static str,
    pub line_tick: &'static str,
    /// The lower half of a cell, for meters in rows back to back where color tells fill
    /// from track: half a cell wide, then a whole one.
    pub halves: Option<[&'static str; 2]>,
    pub pill_on: &'static str,
    pub pill_off: &'static str,
    /// The column chart's levels, a cell in eighths, `▁` to `█`.
    pub columns: [&'static str; 8],
}

impl KitGlyphs {
    pub const UNICODE: KitGlyphs = KitGlyphs {
        top_left: "╭",
        top_right: "╮",
        bottom_left: "╰",
        bottom_right: "╯",
        across: "─",
        down: "│",
        the_mod: "◆",
        swatch: "▌",
        folded: "▸",
        used: "█",
        free: "░",
        tick: "│",
        eighths: Some([" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"]),
        line_fill: "━",
        line_half: "╸",
        line_track: "─",
        line_tick: "┃",
        halves: Some(["▖", "▄"]),
        pill_on: "▰",
        pill_off: "▱",
        columns: ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"],
    };

    pub const ASCII: KitGlyphs = KitGlyphs {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        across: "-",
        down: "|",
        the_mod: "*",
        swatch: "#",
        folded: ">",
        used: "#",
        free: ".",
        tick: "|",
        eighths: None,
        line_fill: "=",
        line_half: "=",
        line_track: "-",
        line_tick: "|",
        halves: None,
        pill_on: "#",
        pill_off: "-",
        columns: ["_", "_", ".", ".", ":", ":", "#", "#"],
    };

    pub fn for_theme(theme: &Theme) -> Self {
        if theme.glyphs.is_ascii() {
            Self::ASCII
        } else {
            Self::UNICODE
        }
    }
}

/// A mod as a screen reader hears it: `name: value` lines of at most `per_line` items,
/// as `context: tools 12k 1%, skills 2.2k 0.2%`.
pub fn spoken(name: &str, items: &[String], per_line: usize) -> Vec<String> {
    items
        .chunks(per_line.max(1))
        .map(|chunk| format!("{name}: {}", chunk.join(", ")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::str_width;

    #[test]
    fn every_glyph_is_one_cell() {
        for set in [KitGlyphs::UNICODE, KitGlyphs::ASCII] {
            let singles = [
                set.top_left,
                set.top_right,
                set.bottom_left,
                set.bottom_right,
                set.across,
                set.down,
                set.the_mod,
                set.swatch,
                set.folded,
                set.used,
                set.free,
                set.tick,
                set.line_fill,
                set.line_half,
                set.line_track,
                set.line_tick,
                set.pill_on,
                set.pill_off,
            ];
            let eighths: Vec<&str> = set.eighths.map(|e| e.to_vec()).unwrap_or_default();
            let halves: Vec<&str> = set.halves.map(|h| h.to_vec()).unwrap_or_default();
            let parts = eighths.iter().chain(&halves);
            for glyph in singles.iter().chain(parts).chain(&set.columns) {
                assert_eq!(str_width(glyph), 1, "{glyph:?}");
            }
        }
        for glyph in [KitGlyphs::ASCII.the_mod, KitGlyphs::ASCII.pill_on] {
            assert!(glyph.is_ascii());
        }
    }

    #[test]
    fn spoken_lines_hold_a_few_items_each() {
        let items: Vec<String> = ["a 1", "b 2", "c 3"].map(String::from).to_vec();
        assert_eq!(
            spoken("context", &items, 2),
            ["context: a 1, b 2", "context: c 3"]
        );
    }
}
