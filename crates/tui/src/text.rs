//! Display-width helpers that truncate and wrap text by terminal columns.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn width(text: &str) -> usize {
    text.width()
}

/// Cuts `text` to at most `max` columns, ending with `…` when something was cut.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Word-wraps `text` to `max` columns, keeping a paragraph's leading indent on its first line
/// and splitting words longer than a line.
pub(crate) fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let indent_len = paragraph.len() - paragraph.trim_start_matches(' ').len();
        let mut line = " ".repeat(indent_len.min(max.saturating_sub(1)));
        let mut used = line.len();
        let mut line_has_word = false;
        for word in paragraph.split_whitespace() {
            let w = word.width();
            if line_has_word && used + 1 + w > max {
                out.push(std::mem::take(&mut line));
                used = 0;
                line_has_word = false;
            }
            if line_has_word {
                line.push(' ');
                used += 1;
            }
            if w <= max.saturating_sub(used) {
                line.push_str(word);
                used += w;
            } else {
                for c in word.chars() {
                    let cw = c.width().unwrap_or(0);
                    if used + cw > max && used > 0 {
                        out.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    line.push(c);
                    used += cw;
                }
            }
            line_has_word = true;
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_counts_columns_not_bytes() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(truncate("你好世界", 5), "你好…");
        assert_eq!(width(&truncate("你好世界", 5)), 5);
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abc", 1), "…");
    }

    #[test]
    fn wrap_breaks_at_words() {
        assert_eq!(
            wrap("the quick brown fox jumps", 10),
            vec!["the quick", "brown fox", "jumps"]
        );
    }

    #[test]
    fn wrap_keeps_newlines_and_indent() {
        assert_eq!(wrap("a\n  b c", 10), vec!["a", "  b c"]);
        assert_eq!(wrap("x\n", 10), vec!["x", ""]);
    }

    #[test]
    fn wrap_splits_long_words() {
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab 你好世界", 4), vec!["ab", "你好", "世界"]);
    }

    #[test]
    fn wrap_never_exceeds_width() {
        let text = "Adding the Retry-After header now; the limiter already returns the reset time";
        for max in 1..30 {
            assert!(
                wrap(text, max).iter().all(|l| width(l) <= max),
                "width {max}"
            );
        }
    }
}
