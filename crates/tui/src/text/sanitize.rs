//! Agent output and tool results can carry escape sequences that would restyle the terminal,
//! move the cursor, set the title, or hide text. Every string from outside passes through here
//! before it is drawn.

use std::borrow::Cow;
use std::iter::Peekable;
use std::str::Chars;

const ESC: char = '\u{1b}';
const BEL: char = '\u{07}';
const C1_CSI: char = '\u{9b}';
const C1_ST: char = '\u{9c}';

/// Newlines stay, tabs become four spaces, carriage returns go, and bidirectional overrides are
/// dropped. Joined emoji come apart: a zero-width joiner between pictographs and skin-tone
/// modifiers go, since terminals disagree on how wide a joined emoji is, and one that draws it
/// wider than the kit counted would wrap a row on its own and break the screen. Each emoji
/// left is two columns everywhere. The emoji variation selector goes too: a font may then draw
/// a one-cell symbol such as `✔` two cells wide. Text with nothing to remove comes back
/// borrowed.
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(needs_work) {
        return Cow::Borrowed(text);
    }
    // Core's sanitizer is the one every Argonauts Code screen shares; the rest of this loop only
    // fixes layout, such as tabs and joined emoji. Line by line, so an unterminated OSC takes
    // only the rest of its own line with it.
    let safe = text
        .split('\n')
        .map(argonauts_core::sanitize::sanitize_for_terminal)
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = String::with_capacity(safe.len());
    let mut chars = safe.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => out.push('\n'),
            '\u{2028}' | '\u{2029}' => out.push('\n'),
            '\t' => out.push_str("    "),
            ESC => skip_escape(&mut chars),
            C1_CSI => skip_csi(&mut chars),
            // OSC, DCS, SOS, PM, and APC all run to a string terminator.
            '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => skip_string(&mut chars),
            c if SKIN_TONES.contains(&c) => {}
            EMOJI_PRESENTATION => {}
            // A joiner in other scripts shapes letters and takes no room, so it stays.
            ZWJ if out.chars().next_back().is_some_and(is_pictograph)
                || chars.peek().copied().is_some_and(is_pictograph) => {}
            c if is_unsafe(c) => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

const ZWJ: char = '\u{200D}';
const EMOJI_PRESENTATION: char = '\u{FE0F}';
const SKIN_TONES: std::ops::RangeInclusive<char> = '\u{1F3FB}'..='\u{1F3FF}';

/// Emoji and pictographic symbols, roughly Unicode's Extended_Pictographic blocks.
fn is_pictograph(c: char) -> bool {
    matches!(
        c,
        '\u{1F000}'..='\u{1FAFF}' | '\u{2600}'..='\u{27BF}' | '\u{2300}'..='\u{23FF}' | '\u{2B00}'..='\u{2BFF}'
    )
}

/// Like [`sanitize`], with each run of line breaks folded into one space, for one-line labels.
pub fn sanitize_line(text: &str) -> Cow<'_, str> {
    let cleaned = sanitize(text);
    if !cleaned.contains('\n') {
        return cleaned;
    }
    let folded = cleaned
        .split('\n')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Cow::Owned(folded)
}

fn needs_work(c: char) -> bool {
    matches!(c, '\t' | '\u{2028}' | '\u{2029}' | ZWJ | EMOJI_PRESENTATION)
        || SKIN_TONES.contains(&c)
        || is_unsafe(c)
}

fn is_unsafe(c: char) -> bool {
    (c.is_control() && c != '\n') || is_bidi_control(c)
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Skips what follows an ESC: a CSI, a string sequence, or a short two or three byte sequence.
fn skip_escape(chars: &mut Peekable<Chars<'_>>) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            skip_csi(chars);
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            skip_string(chars);
        }
        // Intermediate bytes, then one final byte, as in `ESC ( B`.
        Some(' '..='/') => {
            while chars.next_if(|c| matches!(c, ' '..='/')).is_some() {}
            chars.next_if(|c| matches!(c, '0'..='~'));
        }
        Some('0'..='~') => {
            chars.next();
        }
        _ => {}
    }
}

/// Stops early at anything a CSI can't contain, so a broken sequence never eats ordinary text.
fn skip_csi(chars: &mut Peekable<Chars<'_>>) {
    while chars.next_if(|c| matches!(c, '0'..='?')).is_some() {}
    while chars.next_if(|c| matches!(c, ' '..='/')).is_some() {}
    chars.next_if(|c| matches!(c, '@'..='~'));
}

/// Skips an OSC or similar string up to BEL or ST. A newline also ends it, so an unterminated
/// sequence can't swallow the rest of a message.
fn skip_string(chars: &mut Peekable<Chars<'_>>) {
    while let Some(&c) = chars.peek() {
        match c {
            BEL | C1_ST => {
                chars.next();
                return;
            }
            ESC => {
                chars.next();
                chars.next_if_eq(&'\\');
                return;
            }
            '\n' => return,
            _ => {
                chars.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_borrowed() {
        assert!(matches!(sanitize("hello, wörld ✓ 你好"), Cow::Borrowed(_)));
        assert_eq!(sanitize("two\nlines"), "two\nlines");
    }

    #[test]
    fn joined_emoji_come_apart_so_every_terminal_counts_them_alike() {
        // People joined by zero-width joiners, and a thumbs up with a skin tone.
        let joined = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(sanitize(joined), "\u{1F468}\u{1F469}\u{1F467}");
        assert_eq!(sanitize("ok \u{1F44D}\u{1F3FD}!"), "ok \u{1F44D}!");
        assert_eq!(crate::text::str_width(&sanitize(joined)), 6);
        // A joiner that shapes letters, as in Devanagari, stays.
        let conjunct = "\u{0915}\u{094D}\u{200D}\u{0937}";
        assert_eq!(sanitize(conjunct), conjunct);
    }

    #[test]
    fn the_emoji_variation_selector_goes() {
        assert_eq!(sanitize("done \u{2714}\u{FE0F}"), "done \u{2714}");
        assert_eq!(crate::text::str_width(&sanitize("\u{2764}\u{FE0F}")), 1);
        // Text presentation asks for one cell, which is what the kit counts anyway.
        assert_eq!(sanitize("\u{2714}\u{FE0E}"), "\u{2714}\u{FE0E}");
    }

    #[test]
    fn strips_csi_colors_and_cursor_moves() {
        assert_eq!(
            sanitize("\x1b[31mred\x1b[0m and \x1b[2J\x1b[Hclear"),
            "red and clear"
        );
        assert_eq!(sanitize("\x1b[?1049hafter"), "after");
    }

    #[test]
    fn strips_osc_titles_and_hyperlinks() {
        assert_eq!(sanitize("\x1b]0;pwned\x07after"), "after");
        assert_eq!(
            sanitize("\x1b]8;;https://evil.example\x1b\\click\x1b]8;;\x1b\\"),
            "click"
        );
        assert_eq!(sanitize("\u{9d}0;title\u{9c}after"), "after");
    }

    #[test]
    fn unterminated_osc_stops_at_newline() {
        assert_eq!(sanitize("\x1b]0;oops\nnext line"), "\nnext line");
    }

    #[test]
    fn drops_other_controls_and_c1() {
        assert_eq!(sanitize("a\x07b\x08c\x00d\u{9b}31me"), "abcde");
        assert_eq!(sanitize("carriage\rreturn"), "carriagereturn");
        assert_eq!(sanitize("\x1b(Bcharset"), "charset");
        assert_eq!(sanitize("\x1b7saved\x1b8"), "saved");
    }

    #[test]
    fn keeps_newlines_and_expands_tabs() {
        assert_eq!(sanitize("a\tb\r\nc"), "a    b\nc");
    }

    #[test]
    fn drops_bidi_overrides() {
        assert_eq!(sanitize("admin\u{202E}txt.exe"), "admintxt.exe");
        assert_eq!(sanitize("\u{2067}x\u{2069}"), "x");
    }

    #[test]
    fn lone_escape_is_removed() {
        assert_eq!(sanitize("tail\x1b"), "tail");
    }

    #[test]
    fn sanitize_line_folds_newlines() {
        assert_eq!(
            sanitize_line("first\n\n  second \nthird"),
            "first second third"
        );
        assert_eq!(sanitize_line("\x1b[1mbold\x1b[0m"), "bold");
    }
}
