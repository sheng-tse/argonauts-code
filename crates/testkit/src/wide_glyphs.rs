//! Glyphs some fonts (Iosevka in Ghostty) draw two cells wide although they take one: `■` and
//! the side arrows. Drawn edge to edge with text they cover the next cell, so a screen never
//! places `■` against anything but a blank (`▪` is the small one that fits), and an arrow before
//! text is followed by two spaces, or not used. `↑` and `↓` are left as Claude Code draws them in
//! its own key hints and counts, which read the same in that font.

/// The glyphs that need room after them.
const SQUARE: char = '■';
const ARROWS: [char; 2] = ['→', '←'];

/// Where a row of a picture crowds one of those glyphs, by the glyph's column.
pub fn crowded(row: &str) -> Vec<usize> {
    let cells: Vec<char> = row.chars().collect();
    let blank = |at: usize| cells.get(at).is_none_or(|c| c.is_whitespace());
    cells
        .iter()
        .enumerate()
        .filter(|&(at, &c)| {
            if c == SQUARE {
                return !blank(at + 1);
            }
            // At the row's end, or before a gap of two, the arrow has its room.
            ARROWS.contains(&c) && !(at + 1 == cells.len() || (blank(at + 1) && blank(at + 2)))
        })
        .map(|(at, _)| at)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::crowded;

    #[test]
    fn a_square_needs_a_blank_and_an_arrow_two() {
        assert!(crowded("▪ Haiku 5.5  ■ Codex").is_empty());
        assert!(crowded("save ~48.1k   75% →").is_empty());
        assert!(crowded("Status →  runs").is_empty());
        assert_eq!(crowded("■Codex"), [0]);
        assert_eq!(crowded("Status → runs"), [7]);
        assert_eq!(crowded("tab · ←→ tabs"), [6, 7]);
        assert!(crowded("↑↓ select · ↓ 201k tokens").is_empty());
    }
}
