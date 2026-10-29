//! Pictures that behave as text, through the kitty graphics protocol's Unicode placeholders. A
//! picture goes to a screen with a virtual placement, and from then on any cell on that screen
//! that prints [`PLACEHOLDER`] in the picture's id as its foreground colour shows that cell's part
//! of it. kitty and Ghostty keep a store for each screen and empty the alternate one on every
//! entry, so a picture goes out again there (`terminal::Kept`). Such cells scroll, clear and redraw like any other, so nothing is left behind in
//! scrollback. Ghostty and kitty draw them. WezTerm takes kitty pictures but not placeholders
//! (wezterm#986), and tmux and screen pass neither through.

use ratatui::style::Color;

use super::graphics::{self, Graphics};

/// The character a placeholder cell prints, with its row and column as diacritics after it.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// The diacritics that number a placeholder's row and column: the nth one says n. The start of
/// kitty's `rowcolumn-diacritics.txt`, which Ghostty reads the same way.
const DIACRITICS: [char; 64] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
];

/// The most rows or columns a placeholder picture spans here.
pub const MOST_CELLS: u16 = DIACRITICS.len() as u16;

/// The id the graphics query asks under, apart from every picture's.
const QUERY_ID: u32 = 0x4c51;

/// Whether the terminal the environment names draws placeholders, before it is asked: Ghostty or
/// kitty, straight, not through a multiplexer. A program named in `TERM_PROGRAM` is the one the
/// app runs in, so a terminal started from Ghostty, which keeps Ghostty's other variables, isn't
/// taken for it.
pub fn terminal_draws_them(var: impl Fn(&str) -> Option<String>) -> bool {
    if Graphics::detect(&var) != Some(Graphics::Kitty) {
        return false;
    }
    let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
    match var("TERM_PROGRAM").unwrap_or_default().as_str() {
        "ghostty" | "kitty" => true,
        "" => {
            let term = var("TERM").unwrap_or_default();
            !set("WEZTERM_PANE")
                && (matches!(term.as_str(), "xterm-kitty" | "xterm-ghostty")
                    || set("KITTY_WINDOW_ID")
                    || set("GHOSTTY_RESOURCES_DIR"))
        }
        _ => false,
    }
}

/// The graphics query, a one-pixel picture the terminal checks and doesn't keep, then DA1, which
/// every terminal answers after it, so the wait ends there.
pub fn query() -> Vec<u8> {
    format!("\x1b_Gi={QUERY_ID},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c").into_bytes()
}

/// Whether `answer` holds the terminal's OK to [`query`] before its answer to DA1. A terminal
/// that knows no kitty pictures answers DA1 alone, and one that can't take this picture answers
/// with an error.
pub fn query_answered(answer: &[u8]) -> bool {
    let ok = format!("\x1b_Gi={QUERY_ID};OK\x1b\\");
    let Some(at) = find(answer, ok.as_bytes()) else {
        return false;
    };
    find(answer, b"\x1b[?").is_some_and(|attributes| attributes > at)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The escapes that send `png` under `id` with a virtual placement `cells` wide and tall,
/// columns by rows: nothing shows until cells print its placeholders. The terminal says nothing
/// back.
pub fn send(png: &[u8], id: u32, (columns, rows): (u16, u16)) -> Vec<u8> {
    let mut out = graphics::store(png, id);
    out.extend_from_slice(
        format!("\x1b_Ga=p,U=1,i={id},c={columns},r={rows},q=2\x1b\\").as_bytes(),
    );
    out
}

/// The text of one placeholder cell: the picture's cell at `row` and `column`, counted from 0.
/// `None` past [`MOST_CELLS`].
pub fn cell(row: u16, column: u16) -> Option<String> {
    let row = DIACRITICS.get(usize::from(row))?;
    let column = DIACRITICS.get(usize::from(column))?;
    Some([PLACEHOLDER, *row, *column].iter().collect())
}

/// The foreground colour that names picture `id` to the terminal, in true colour. Ids fit in its
/// 24 bits, so no cell needs a third diacritic for the top byte.
pub fn id_colour(id: u32) -> Color {
    let [_, red, green, blue] = id.to_be_bytes();
    Color::Rgb(red, green, blue)
}

#[cfg(test)]
#[path = "placeholder_tests.rs"]
mod tests;
