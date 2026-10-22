//! Glyphs some fonts (Iosevka in Ghostty) draw two cells wide although they take one: `■` and
//! the arrows. Drawn edge to edge with text they cover the next cell, so a screen never places
//! `■` against anything but a blank (`▪` is the small one that fits), and an arrow before text
//! is followed by two blanks, or not used. A glyph followed straight by another isn't widened,
//! so the pairs key hints write, `↑↓`, `↑/↓` and `←/→`, are one mark, and so is an arrow before
//! punctuation, as in `ctrl+↑,`. An arrow before a frame's rule, as a scroll mark on a box's edge,
//! has the room it needs, and so has a list's scroll mark in the pointer's column, which every
//! row keeps one cell wide for `❯`.

/// The glyphs that need room after them.
const SQUARE: char = '■';
const ARROWS: [char; 4] = ['→', '←', '↑', '↓'];

/// Where a row of a picture crowds one of those glyphs, by the glyph's column.
pub fn crowded(row: &str) -> Vec<usize> {
    let cells: Vec<char> = row.chars().collect();
    let blank = |at: usize| cells.get(at).is_none_or(|c| c.is_whitespace());
    let rule = |at: usize| {
        cells
            .get(at)
            .is_some_and(|c| ('\u{2500}'..='\u{257f}').contains(c))
    };
    cells
        .iter()
        .enumerate()
        .filter(|&(at, &c)| {
            if c == SQUARE {
                return !blank(at + 1);
            }
            if !ARROWS.contains(&c) {
                return false;
            }
            let next = cells.get(at + 1).copied();
            let arrow = |at: usize| cells.get(at).is_some_and(|c| ARROWS.contains(c));
            let pair = (c == '↑' && next == Some('↓')) || (next == Some('/') && arrow(at + 2));
            let mark = next.is_some_and(|c| matches!(c, ',' | '.' | ')' | ';' | ':'));
            let digit = |at: usize| cells.get(at).is_some_and(char::is_ascii_digit);
            let pointer = at == 2 && blank(0) && blank(1) && blank(3) && digit(4);
            // At the row's end, before a gap of two, or before a rule, the arrow has its room.
            let room = at + 1 == cells.len() || (blank(at + 1) && (blank(at + 2) || rule(at + 2)));
            !(pair || mark || pointer || room)
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
        assert_eq!(crowded("↑↓ select · ↓ 201k tokens"), [1, 12]);
        assert!(crowded("↑↓  select · ↓  201k tokens").is_empty());
        assert!(crowded("──── ↑  2–6 of 6 ↓ ─╯").is_empty());
        assert!(crowded("  ↑/↓  to navigate · ctrl+↑, to jump").is_empty());
        assert!(crowded("  ↓ 8. Gemma3 12B").is_empty());
        assert_eq!(crowded("  ↑/↓ to navigate"), [4]);
    }
}
