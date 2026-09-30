//! Removes terminal control sequences from untrusted text before anything draws it.

/// Newlines stay, tabs become four spaces, and bidirectional overrides are dropped, so agent
/// text can't restyle the terminal, move the cursor, set the title, or disguise what it shows.
pub fn clean(text: &str) -> String {
    if !text.chars().any(needs_work) {
        return text.to_owned();
    }
    let normalized = text.replace("\r\n", "\n").replace('\t', "    ");
    strip_ansi_escapes::strip_str(&normalized)
        .chars()
        .filter(|&c| c == '\n' || !is_unsafe(c))
        .collect()
}

/// Like [`clean`], with each run of line breaks folded into one space, for one-line labels.
pub fn clean_line(text: &str) -> String {
    let cleaned = clean(text);
    if !cleaned.contains('\n') {
        return cleaned;
    }
    cleaned
        .split('\n')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn needs_work(c: char) -> bool {
    c == '\t' || c == '\r' || is_unsafe(c)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(clean("hello, wörld ✓ 你好"), "hello, wörld ✓ 你好");
        assert_eq!(clean("two\nlines"), "two\nlines");
    }

    #[test]
    fn strips_csi_colors_and_cursor_moves() {
        assert_eq!(
            clean("\x1b[31mred\x1b[0m and \x1b[2J\x1b[Hclear"),
            "red and clear"
        );
    }

    #[test]
    fn strips_osc_title_and_hyperlinks() {
        assert_eq!(clean("\x1b]0;pwned\x07after"), "after");
        assert_eq!(
            clean("\x1b]8;;https://evil.example\x1b\\click\x1b]8;;\x1b\\"),
            "click"
        );
    }

    #[test]
    fn drops_other_controls_and_c1() {
        assert_eq!(clean("a\x07b\x08c\x00d\u{9b}31me"), "abcd31me");
        assert_eq!(clean("carriage\rreturn"), "carriagereturn");
    }

    #[test]
    fn keeps_newlines_and_expands_tabs() {
        assert_eq!(clean("a\tb\r\nc"), "a    b\nc");
    }

    #[test]
    fn drops_bidi_overrides() {
        assert_eq!(clean("admin\u{202E}txt.exe"), "admintxt.exe");
        assert_eq!(clean("\u{2067}x\u{2069}"), "x");
    }

    #[test]
    fn lone_escape_is_removed() {
        let out = clean("tail\x1b");
        assert!(!out.contains('\x1b'));
        assert!(out.starts_with("tail"));
    }

    #[test]
    fn clean_line_folds_newlines() {
        assert_eq!(
            clean_line("first\n\n  second \nthird"),
            "first second third"
        );
        assert_eq!(clean_line("\x1b[1mbold\x1b[0m"), "bold");
    }
}
