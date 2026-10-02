//! Shared helpers every view uses to keep its shape: cut text to a width in display cells
//! with `…`, cut paths in the middle, and right-align numbers in fixed slots.

use unicode_width::UnicodeWidthStr;

use crate::text::str_width;

/// Cuts `text` to at most `width` display cells, ending in `…` when it had to cut.
pub fn cut(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Cuts a path in the middle, keeping its first part and its end: `/…/ui-kit/src/composer.rs`.
pub fn cut_middle(path: &str, width: usize) -> String {
    if str_width(path) <= width {
        return path.to_owned();
    }
    let parts: Vec<&str> = path.split('/').collect();
    // Keep as many trailing parts as fit after the first part and `/…/`.
    let head = parts.first().copied().unwrap_or("");
    let mut tail: Vec<&str> = Vec::new();
    for part in parts.iter().skip(1).rev() {
        let candidate = format!("{head}/…/{}", {
            let mut t = tail.clone();
            t.insert(0, part);
            t.join("/")
        });
        if candidate.width() > width {
            break;
        }
        tail.insert(0, part);
    }
    if tail.is_empty() {
        return cut(path, width);
    }
    format!("{head}/…/{}", tail.join("/"))
}

/// `text` right-aligned in a slot `width` cells wide, cut when it's longer.
pub fn right(text: &str, width: usize) -> String {
    let text = cut(text, width);
    let pad = width.saturating_sub(str_width(&text));
    format!("{}{text}", " ".repeat(pad))
}

/// `text` left-aligned in a slot `width` cells wide, cut when it's longer.
pub fn left(text: &str, width: usize) -> String {
    let text = cut(text, width);
    let pad = width.saturating_sub(str_width(&text));
    format!("{text}{}", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_counts_display_cells() {
        assert_eq!(cut("argonauts", 5), "argo…");
        assert_eq!(cut("中文字", 4), "中…");
        assert_eq!(cut("short", 10), "short");
    }

    #[test]
    fn long_paths_cut_in_the_middle() {
        let path = "/Users/someone/github/argonauts/crates/ui-kit/src/composer.rs";
        let cut = cut_middle(path, 30);
        assert!(cut.starts_with("/…/"), "{cut}");
        assert!(cut.ends_with("composer.rs"));
        assert!(str_width(&cut) <= 30);
    }

    #[test]
    fn slots_keep_their_width() {
        assert_eq!(right("8m12s", 7), "  8m12s");
        assert_eq!(left("中", 4), "中  ");
    }
}
