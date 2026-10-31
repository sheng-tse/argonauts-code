//! Cutting text to its room. [`cut`] and [`cut_spans`] are for text with something after it,
//! such as a table's column, and [`truncate_spans`] is the last guard that keeps a row from
//! wrapping on its own.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::width::{ambiguous_width, spans_width, str_width};
use super::wrap::to_static_span;

/// Where a cut may fall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// After any character, for text such as a summary or a line of counts.
    Char,
    /// After the last whole word that fits, for a name. A word ends at a space or a `-`, and
    /// a first word too long for the room is cut inside, so the name keeps at least its start.
    Word,
}

/// `text` in `cells`: whole where it fits, else cut with `…` one cell short, so a blank always
/// stays after the cut. Iosevka draws `…` two cells wide though the terminal counts one, and
/// a cut that filled its room would run into whatever comes next. Measured as the screen
/// measures, `ui.ambiguous_width` included.
pub fn cut(text: &str, cells: usize, at: At) -> String {
    if str_width(text) <= cells {
        return text.to_owned();
    }
    let (kept, mark) = cut_point(text, cells, at);
    format!("{}{mark}", &text[..kept])
}

/// [`cut`] for styled text: each part keeps its style, and the `…` takes the style of the
/// text it stands for.
pub fn cut_spans(spans: &[Span<'_>], cells: usize, at: At) -> Vec<Span<'static>> {
    if spans_width(spans) <= cells {
        return spans.iter().map(to_static_span).collect();
    }
    let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
    let (kept, mark) = cut_point(&text, cells, at);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut start = 0;
    for span in spans {
        let end = start + span.content.len();
        let take = kept.min(end).saturating_sub(start);
        if take > 0 {
            out.push(Span::styled(span.content[..take].to_owned(), span.style));
        }
        start = end;
    }
    if !mark.is_empty() {
        let dropped = text[kept..]
            .find(|c: char| !c.is_whitespace())
            .map_or(kept, |at| kept + at);
        out.push(Span::styled(mark, style_at(spans, dropped)));
    }
    out
}

/// How many bytes of `text` a cut to `cells` keeps, and the mark that follows them.
fn cut_point(text: &str, cells: usize, at: At) -> (usize, &'static str) {
    let room = cells.saturating_sub(1);
    let mark = cut_mark(room);
    let Some(left) = room.checked_sub(str_width(mark)) else {
        return (0, "");
    };
    let end = fitting(text, left);
    let chars = text[..end].trim_end();
    let kept = match at {
        At::Char => chars,
        At::Word => whole_words(text, end).unwrap_or(chars),
    };
    (kept.len(), mark)
}

/// The bytes of the graphemes at the start of `text` that fit in `cells`.
fn fitting(text: &str, cells: usize) -> usize {
    let mut used = 0;
    for (at, grapheme) in text.grapheme_indices(true) {
        used += str_width(grapheme);
        if used > cells {
            return at;
        }
    }
    text.len()
}

/// The whole words in `text[..end]`, without the spaces and marks that joined them to the
/// next, such as the ` ·` of `1 task · 14 reviews`. `None` when not even one word is whole.
fn whole_words(text: &str, end: usize) -> Option<&str> {
    let chars = &text[..end];
    let words = if text[end..].starts_with(ends_a_word) {
        chars
    } else {
        &chars[..chars.rfind(ends_a_word)?]
    };
    let words = words.trim_end_matches(joins_words);
    (!words.is_empty()).then_some(words)
}

fn ends_a_word(c: char) -> bool {
    c.is_whitespace() || c == '-'
}

fn joins_words(c: char) -> bool {
    ends_a_word(c) || matches!(c, '·' | ',' | ';' | ':')
}

/// The style of the span that holds byte `at` of the spans' text run together.
fn style_at(spans: &[Span<'_>], at: usize) -> Style {
    let mut start = 0;
    for span in spans {
        start += span.content.len();
        if at < start {
            return span.style;
        }
    }
    spans.last().map_or_else(Style::default, |span| span.style)
}

/// Cuts every line wider than `width` down to it, as a last guard so no renderer ever hands the
/// terminal a row that would wrap on its own.
pub fn fit_lines(lines: &mut [Line<'static>], width: usize) {
    for line in lines.iter_mut() {
        if spans_width(&line.spans) > width {
            *line = truncate_line(line, width);
        }
    }
}

/// Cuts `spans` to at most `width` columns, ending with `…` when anything was cut. Where
/// ambiguous glyphs take two cells, `…` is one of them, so the cut shows `...` instead, or
/// nothing on a row too narrow for it.
pub fn truncate_spans(spans: &[Span<'_>], width: usize) -> Vec<Span<'static>> {
    if spans_width(spans) <= width {
        return spans.iter().map(to_static_span).collect();
    }
    if width == 0 {
        return Vec::new();
    }
    let mark = cut_mark(width);
    let limit = width - str_width(mark);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    let mut last_style = Style::default();
    'spans: for span in spans {
        last_style = span.style;
        let mut kept = String::new();
        for grapheme in span.content.graphemes(true) {
            let w = str_width(grapheme);
            if used + w > limit {
                if !kept.is_empty() {
                    out.push(Span::styled(kept, span.style));
                }
                break 'spans;
            }
            used += w;
            kept.push_str(grapheme);
        }
        if !kept.is_empty() {
            out.push(Span::styled(kept, span.style));
        }
    }
    if !mark.is_empty() {
        out.push(Span::styled(mark, last_style));
    }
    out
}

/// [`truncate_spans`] for a name: a cut keeps the whole words that fit, a word ending at a
/// space or a `-`, so "GLM 5.3 Flash Free" in 16 cells reads "GLM 5.3 Flash…" rather than
/// "GLM 5.3 Flash F…". A first word longer than the room is cut inside, so the name keeps
/// at least its start.
pub fn truncate_spans_at_word(spans: &[Span<'_>], width: usize) -> Vec<Span<'static>> {
    let cut = truncate_spans(spans, width);
    if spans_width(spans) <= width || cut.is_empty() {
        return cut;
    }
    let mark = cut_mark(width);
    let full: String = spans.iter().map(|span| span.content.as_ref()).collect();
    let shown: String = cut.iter().map(|span| span.content.as_ref()).collect();
    let kept = &shown[..shown.len() - mark.len()];
    let ends_a_word = full[kept.len()..].starts_with([' ', '-']);
    let words = if ends_a_word {
        Some(kept)
    } else {
        kept.rfind([' ', '-']).map(|at| &kept[..at])
    };
    let Some(words) = words
        .map(|words| words.trim_end_matches([' ', '-']))
        .filter(|words| !words.is_empty())
    else {
        return cut;
    };
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut left = words.len();
    let mut last_style = Style::default();
    for span in spans {
        if left == 0 {
            break;
        }
        let take = left.min(span.content.len());
        out.push(Span::styled(span.content[..take].to_owned(), span.style));
        last_style = span.style;
        left -= take;
    }
    if !mark.is_empty() {
        out.push(Span::styled(mark, last_style));
    }
    out
}

/// What ends a row cut to `width` columns.
fn cut_mark(width: usize) -> &'static str {
    cut_mark_for(width, ambiguous_width())
}

fn cut_mark_for(width: usize, ambiguous: u8) -> &'static str {
    match ambiguous {
        2 if width > 3 => "...",
        2 => "",
        _ => "…",
    }
}

pub fn truncate_line(line: &Line<'_>, width: usize) -> Line<'static> {
    Line::from(truncate_spans(&line.spans, width)).style(line.style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::text::plain_text;
    use ratatui::style::Stylize;

    #[test]
    fn fit_lines_cuts_only_what_is_too_wide() {
        let mut lines = vec![Line::from("short"), Line::from("much too long")];
        fit_lines(&mut lines, 6);
        assert_eq!(plain_text(&lines), "short\nmuch …");
    }

    #[test]
    fn truncation_adds_an_ellipsis() {
        let spans = [Span::raw("hello "), Span::raw("world")];
        let cut = truncate_spans(&spans, 8);
        assert_eq!(Line::from(cut).to_string(), "hello w…");
        assert_eq!(
            Line::from(truncate_spans(&spans, 11)).to_string(),
            "hello world"
        );
        assert!(truncate_spans(&spans, 0).is_empty());
    }

    #[test]
    fn a_name_is_cut_after_its_last_whole_word() {
        let cut = |name: &str, width: usize| {
            Line::from(truncate_spans_at_word(&[Span::raw(name.to_owned())], width)).to_string()
        };
        assert_eq!(cut("GLM 5.3 Flash Free", 16), "GLM 5.3 Flash…");
        assert_eq!(cut("GLM-5.3-Flash Free", 12), "GLM-5.3…");
        // The room ends where a word does.
        assert_eq!(cut("Sonnet 5.5 Thinking", 11), "Sonnet 5.5…");
        assert_eq!(cut("Qwen3 Coder 30B", 15), "Qwen3 Coder 30B");
        // A first word too long for the room is cut inside it.
        assert_eq!(cut("Nemotron-Ultra", 6), "Nemot…");
        assert!(truncate_spans_at_word(&[Span::raw("Opus")], 0).is_empty());
    }

    #[test]
    fn a_name_cut_at_a_word_keeps_each_part_its_style() {
        let spans = [Span::raw("GLM 5.3 "), Span::raw("Flash Free").dim()];
        let cut = truncate_spans_at_word(&spans, 16);
        assert_eq!(Line::from(cut.clone()).to_string(), "GLM 5.3 Flash…");
        assert_eq!(cut[1].content, "Flash");
        assert_eq!(cut[1].style, cut[2].style);
        assert_eq!(cut[1].style, Style::default().dim());
    }

    #[test]
    fn two_cell_ambiguous_glyphs_cut_with_dots() {
        assert_eq!(cut_mark_for(40, 1), "…");
        assert_eq!(cut_mark_for(40, 2), "...");
        assert_eq!(cut_mark_for(3, 2), "");
        assert_eq!(crate::base::text::width_with("...", 2), 3);
    }

    fn by(at: At) -> impl Fn(&str, usize) -> String {
        move |text, cells| cut(text, cells, at)
    }

    #[test]
    fn text_that_fits_stays_whole() {
        for at in [At::Char, At::Word] {
            assert_eq!(
                cut("Gemini 3.8 Flash (Low)", 22, at),
                "Gemini 3.8 Flash (Low)"
            );
            assert_eq!(cut("Opus 5.5", 40, at), "Opus 5.5");
            assert_eq!(cut("", 0, at), "");
        }
    }

    #[test]
    fn a_cut_keeps_one_blank_after_it() {
        for at in [At::Char, At::Word] {
            for (text, cells) in [
                ("1 workflow · 1 task · 14 reviews", 26),
                ("Gemini 3.8 Flash (Low)", 21),
                ("Sample Coder 30B (64k)", 20),
                ("abcdef", 5),
                ("修复修复修复", 7),
            ] {
                let cut = cut(text, cells, at);
                assert!(cut.ends_with('…'), "{text:?} at {cells}: {cut:?}");
                assert!(str_width(&cut) < cells, "{text:?} at {cells}: {cut:?}");
            }
        }
    }

    /// The overlap on Limits: "did" cut to fill its 26 cells left its `…` against the
    /// week's bars, and Iosevka draws the `…` over the one blank between them.
    #[test]
    fn a_cut_column_never_touches_the_next() {
        let column = cut("1 workflow · 1 task · 14 reviews", 26, At::Char);
        let row = format!("{column}{}▁ ▁ ▁", " ".repeat(26 - str_width(&column)));
        let (_, after) = row.split_once('…').unwrap();
        assert!(after.starts_with(' '), "{row:?}");
    }

    #[test]
    fn a_char_cut_falls_after_any_character() {
        let cut = by(At::Char);
        assert_eq!(cut("Sample Coder 30B (64k)", 20), "Sample Coder 30B (…");
        assert_eq!(cut("abcdef", 5), "abc…");
        assert_eq!(cut("abcdef", 6), "abcdef");
        // A space before the cut goes, so the `…` follows the text.
        assert_eq!(cut("Gemini 3.8 Flash (Low)", 19), "Gemini 3.8 Flash…");
    }

    #[test]
    fn a_word_cut_keeps_whole_words() {
        let cut = by(At::Word);
        assert_eq!(cut("Sample Coder 30B (64k)", 20), "Sample Coder 30B…");
        assert_eq!(cut("GLM 5.3 Flash Free", 17), "GLM 5.3 Flash…");
        assert_eq!(cut("claude-opus-5-5-preview", 14), "claude-opus…");
        // The room ends where a word does.
        assert_eq!(cut("Sonnet 5.5 Thinking", 12), "Sonnet 5.5…");
        // What joined the last word to the next goes with it.
        assert_eq!(
            cut("1 workflow · 1 task · 14 reviews", 24),
            "1 workflow · 1 task…"
        );
        // A first word too long for the room is cut inside it.
        assert_eq!(cut("Nemotron-Ultra", 7), "Nemot…");
        assert_eq!(cut("修复修复修复", 7), "修复…");
    }

    #[test]
    fn wide_glyphs_count_their_cells() {
        let cut = by(At::Char);
        assert_eq!(cut("修复修复修复", 6), "修复…");
        assert_eq!(cut("修复修复修复", 5), "修…");
        assert_eq!(cut("a修复", 4), "a…");
    }

    #[test]
    fn tiny_rooms_hold_what_they_can() {
        for at in [At::Char, At::Word] {
            assert_eq!(cut("Opus", 0, at), "");
            assert_eq!(cut("Opus", 1, at), "");
            assert_eq!(cut("Opus", 2, at), "…");
            assert_eq!(cut("Opus", 3, at), "O…");
        }
    }

    #[test]
    fn two_cell_ambiguous_glyphs_cut_with_dots_and_keep_the_blank() {
        crate::base::text::set_ambiguous_width(2);
        let cut = by(At::Char);
        assert_eq!(cut("abcdefghij", 8), "abcd...");
        // `●` takes two cells here, so three are left for one of them and the dots.
        assert_eq!(cut("●●●●", 7), "●...");
        assert_eq!(cut("abcdefghij", 4), "abc");
        assert_eq!(by(At::Word)("Sonnet 5.5 Thinking", 15), "Sonnet 5.5...");
    }

    #[test]
    fn styled_text_keeps_its_styles() {
        let spans = [Span::raw("GLM 5.3 "), Span::raw("Flash Free").dim()];
        let cut = cut_spans(&spans, 17, At::Word);
        assert_eq!(Line::from(cut.clone()).to_string(), "GLM 5.3 Flash…");
        assert_eq!(cut[0].content, "GLM 5.3 ");
        assert_eq!(cut[1].content, "Flash");
        assert_eq!(cut[1].style, Style::default().dim());
        assert_eq!(
            cut[2].style,
            Style::default().dim(),
            "the … stands for Free"
        );
        let whole = cut_spans(&spans, 18, At::Word);
        assert_eq!(Line::from(whole).to_string(), "GLM 5.3 Flash Free");
    }

    #[test]
    fn the_mark_takes_the_style_of_what_it_stands_for() {
        let spans = [Span::raw("Opus 5.5 "), Span::raw("(1M)").bold()];
        let cut = cut_spans(&spans, 11, At::Char);
        assert_eq!(Line::from(cut.clone()).to_string(), "Opus 5.5…");
        assert_eq!(cut.last().unwrap().style, Style::default().bold());
        let cut = cut_spans(&spans, 8, At::Char);
        assert_eq!(Line::from(cut.clone()).to_string(), "Opus 5…");
        assert_eq!(cut.last().unwrap().style, Style::default());
        assert!(cut_spans(&spans, 1, At::Char).is_empty());
    }

    #[test]
    fn a_cut_matches_the_text_cut_of_its_spans() {
        let texts = [
            "Gemini 3.8 Flash (Low)",
            "1 workflow · 1 task · 14 reviews",
            "修复修复修复",
            "  leading spaces",
            "trailing spaces   ",
            "Nemotron-Ultra",
        ];
        for text in texts {
            for cells in 0..=str_width(text) + 1 {
                for at in [At::Char, At::Word] {
                    let middle = (0..=text.len() / 2)
                        .rev()
                        .find(|at| text.is_char_boundary(*at))
                        .unwrap_or(0);
                    let (head, tail) = text.split_at(middle);
                    let spans = [Span::raw(head), Span::raw(tail).italic()];
                    assert_eq!(
                        Line::from(cut_spans(&spans, cells, at)).to_string(),
                        cut(text, cells, at),
                        "{text:?} at {cells}, {at:?}"
                    );
                }
            }
        }
    }
}
