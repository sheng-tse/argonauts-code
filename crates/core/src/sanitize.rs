//! Untrusted text can carry escapes that set the window title, write the clipboard, or reorder
//! what you read, so everything Argonauts shows goes through [`sanitize_for_terminal`] first.

/// Drops control characters except tab and newline, whole CSI, OSC, DCS, SOS, PM, and APC
/// sequences in 7-bit and 8-bit forms, other escapes, and bidi overrides.
pub fn sanitize_for_terminal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' | '\n' => out.push(c),
            '\u{1b}' => match chars.next() {
                Some('[') => skip_csi(&mut chars),
                Some(']') => skip_string(&mut chars, true),
                Some('P' | 'X' | '^' | '_') => skip_string(&mut chars, false),
                Some(next) => skip_escape(next, &mut chars),
                None => {}
            },
            '\u{9b}' => skip_csi(&mut chars),
            '\u{9d}' => skip_string(&mut chars, true),
            '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => skip_string(&mut chars, false),
            c if c.is_control() => {}
            c if is_bidi_control(c) => {}
            c => out.push(c),
        }
    }
    out
}

/// A CSI sequence: parameter and intermediate bytes, then one final byte.
fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for c in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&c) {
            return;
        }
        // Anything outside the sequence's byte range ends it early; drop it with the rest.
        if !('\u{20}'..='\u{3f}').contains(&c) {
            return;
        }
    }
}

/// An OSC, DCS, SOS, PM, or APC string, up to its terminator. OSC may also end with BEL.
fn skip_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, bell_ends: bool) {
    while let Some(c) = chars.next() {
        match c {
            '\u{7}' if bell_ends => return,
            '\u{9c}' => return,
            '\u{1b}' => {
                if chars.peek() == Some(&'\\') {
                    chars.next();
                }
                return;
            }
            _ => {}
        }
    }
}

fn skip_escape(first: char, chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let mut c = first;
    while ('\u{20}'..='\u{2f}').contains(&c) {
        match chars.next() {
            Some(next) => c = next,
            None => return,
        }
    }
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_adversarial_corpus() {
        let cases: &[(&str, &str)] = &[
            ("plain text stays", "plain text stays"),
            ("tabs\tand\nnewlines stay", "tabs\tand\nnewlines stay"),
            ("emoji 🚀 and 漢字 stay", "emoji 🚀 and 漢字 stay"),
            // OSC 52 writes the clipboard.
            ("a\u{1b}]52;c;ZXZpbA==\u{7}b", "ab"),
            ("\u{1b}]0;pwned\u{7}title", "title"),
            ("\u{1b}]2;pwned\u{1b}\\title", "title"),
            // OSC 8 plants a hyperlink; its text stays, the link goes.
            (
                "\u{1b}]8;;https://evil.example\u{1b}\\click\u{1b}]8;;\u{1b}\\",
                "click",
            ),
            ("\u{1b}[2Jcleared", "cleared"),
            ("\u{1b}[31mred\u{1b}[0m", "red"),
            ("\u{1b}[?1049hscreen", "screen"),
            // The 8-bit CSI and OSC.
            ("\u{9b}2Jcleared", "cleared"),
            ("\u{9d}0;pwned\u{9c}ok", "ok"),
            ("\u{1b}Pq#0;2;0;0;0\u{1b}\\after", "after"),
            ("\u{1b}_payload\u{1b}\\after", "after"),
            ("\u{1b}^privacy\u{1b}\\after", "after"),
            ("\u{1b}Xstring\u{1b}\\after", "after"),
            ("\u{90}dcs\u{9c}after", "after"),
            ("\u{1b}(Bascii", "ascii"),
            ("\u{1b}7saved\u{1b}8", "saved"),
            ("\u{1b}cafter reset", "after reset"),
            ("over\rwrite", "overwrite"),
            ("back\u{8}space", "backspace"),
            ("bell\u{7}", "bell"),
            ("del\u{7f}ete", "delete"),
            // Bidi overrides that reorder what you read.
            ("admin\u{202e}txt.exe", "admintxt.exe"),
            ("\u{2066}isolated\u{2069}", "isolated"),
            // Unterminated sequences drop everything after them rather than leak.
            ("ok\u{1b}]0;never ends", "ok"),
            ("ok\u{1b}[", "ok"),
            ("ok\u{1b}", "ok"),
        ];
        for (input, want) in cases {
            assert_eq!(&sanitize_for_terminal(input), want, "{input:?}");
        }
    }

    #[test]
    fn sanitizing_twice_changes_nothing() {
        let once = sanitize_for_terminal("\u{1b}]0;x\u{7}a\u{1b}[1mb\u{202e}c");
        assert_eq!(sanitize_for_terminal(&once), once);
    }
}
