use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::marks::{self, Join};
use super::width::{ambiguous_width, spans_width, str_width};

/// Wraps `spans` to `width` columns. The first row starts with `first` and later rows with
/// `rest`, so wrapped text lines up under the text instead of under a bullet. Leading spaces of
/// a paragraph are kept and words wider than a row split between graphemes. Text without spaces
/// between words, such as Chinese or Japanese, breaks where UAX #14 allows, so a row fills up
/// and never starts with a closing comma. Empty input still gives one row holding `first`.
pub fn wrap_spans(
    spans: &[Span<'_>],
    width: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> Vec<Line<'static>> {
    if let Some(row) = one_ascii_row(spans, width.saturating_sub(spans_width(first)).max(1)) {
        let mut line = first.to_vec();
        line.extend(row);
        return vec![Line::from(line)];
    }
    wrap_tokens(spans, width, first, rest)
}

fn wrap_tokens(
    spans: &[Span<'_>],
    width: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> Vec<Line<'static>> {
    let mut rows = Rows::new(width, first, rest);
    let mut pending_space: Option<&Token> = None;
    let mut paragraph_start = true;
    let tokens = tokenize(spans);

    for token in &tokens {
        match token.kind {
            Kind::Newline => {
                rows.end_row();
                pending_space = None;
                paragraph_start = true;
            }
            // Indentation at the start of a paragraph means something, so it stays.
            Kind::Space if paragraph_start && rows.used == 0 => rows.push_clipped(token),
            Kind::Space => pending_space = Some(token),
            Kind::Word => {
                let space = pending_space.map_or(0, |t| t.width);
                if rows.used + space + token.width <= rows.avail() {
                    if let Some(space) = pending_space.take() {
                        rows.push(space);
                    }
                    rows.push(token);
                } else if rows.used == 0 {
                    rows.push_split(token);
                } else {
                    // A break at a space drops the space, which a copy puts back.
                    let join = if pending_space.take().is_some() {
                        Join::Space
                    } else {
                        Join::Direct
                    };
                    rows.break_row(join);
                    if token.width <= rows.avail() {
                        rows.push(token);
                    } else {
                        rows.push_split(token);
                    }
                }
                paragraph_start = false;
            }
        }
    }
    if rows.used > 0 || rows.lines.is_empty() {
        rows.end_row();
    }
    rows.lines
}

pub fn wrap_text(
    text: &str,
    style: Style,
    width: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> Vec<Line<'static>> {
    wrap_spans(&[Span::styled(text, style)], width, first, rest)
}

/// Cuts `spans` into rows of at most `width` columns, breaking between graphemes, for code and
/// diffs where nothing may be dropped. A grapheme wider than the whole row still gets a row of
/// its own, so the loop always moves on.
pub fn hard_wrap_spans(spans: &[Span<'_>], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    // Most code and diff rows are plain ASCII and fit, and need no walk over their graphemes.
    let plain_len = spans.iter().try_fold(0, |len, span| {
        is_plain_ascii(&span.content).then(|| len + span.content.len())
    });
    if plain_len.is_some_and(|len| len <= width) {
        let row = spans
            .iter()
            .filter(|span| !span.content.is_empty())
            .map(to_static_span)
            .collect();
        return vec![row];
    }
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for span in spans {
        let mut piece = String::new();
        for grapheme in span.content.graphemes(true) {
            let w = str_width(grapheme);
            if used + w > width && used > 0 {
                if !piece.is_empty() {
                    row.push(Span::styled(std::mem::take(&mut piece), span.style));
                }
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            piece.push_str(grapheme);
            used += w;
        }
        if !piece.is_empty() {
            row.push(Span::styled(piece, span.style));
        }
    }
    rows.push(row);
    for row in rows.iter_mut().skip(1) {
        marks::continues(row, Join::Direct);
    }
    rows
}

/// Like [`hard_wrap_spans`], but a row ends where a token does when it can, after a space or
/// after punctuation that a word follows, so `-> Result<()>` never splits inside `Result`.
/// A token longer than a row still breaks between graphemes.
pub fn wrap_code_spans(spans: &[Span<'_>], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    if spans_width(spans) <= width {
        return hard_wrap_spans(spans, width);
    }
    let cells: Vec<(&str, Style, usize)> = spans
        .iter()
        .flat_map(|span| {
            span.content
                .graphemes(true)
                .map(move |grapheme| (grapheme, span.style, str_width(grapheme)))
        })
        .collect();

    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    // Where the current row may end, as an index into `cells`.
    let mut last_break = None;
    let mut i = 0;
    while i < cells.len() {
        let (grapheme, _, w) = cells[i];
        if used + w > width && used > 0 {
            let end = last_break.filter(|&end| end > start).unwrap_or(i);
            rows.push(styled_row(&cells[start..end]));
            start = end;
            used = cells[start..i].iter().map(|cell| cell.2).sum();
            // What moved down may hold a break of its own.
            last_break = (start..i)
                .rev()
                .filter(|&j| breaks_after(cells[j].0, cells.get(j + 1).map(|cell| cell.0)))
                .map(|j| j + 1)
                .find(|&end| end < i);
            continue;
        }
        used += w;
        let next = cells.get(i + 1).map(|cell| cell.0);
        if breaks_after(grapheme, next) {
            last_break = Some(i + 1);
        }
        i += 1;
    }
    rows.push(styled_row(&cells[start..]));
    for row in rows.iter_mut().skip(1) {
        marks::continues(row, Join::Direct);
    }
    rows
}

fn breaks_after(grapheme: &str, next: Option<&str>) -> bool {
    let Some(next) = next else {
        return false;
    };
    let is = |g: &str, test: fn(char) -> bool| g.chars().next().is_some_and(test);
    let punct = |c: char| c.is_ascii_punctuation() && c != '_';
    let word = |c: char| c.is_alphanumeric() || c == '_';
    is(grapheme, char::is_whitespace) && !is(next, char::is_whitespace)
        || is(grapheme, punct) && (is(next, word) || is(next, char::is_whitespace))
}

fn styled_row(cells: &[(&str, Style, usize)]) -> Vec<Span<'static>> {
    let mut row: Vec<Span<'static>> = Vec::new();
    for (grapheme, style, _) in cells {
        match row.last_mut() {
            Some(last) if last.style == *style => last.content.to_mut().push_str(grapheme),
            _ => row.push(Span::styled((*grapheme).to_owned(), *style)),
        }
    }
    row
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

fn to_static_span(span: &Span<'_>) -> Span<'static> {
    Span::styled(span.content.to_string(), span.style)
}

/// Printable ASCII, where every byte is one column and one grapheme.
fn is_plain_ascii(text: &str) -> bool {
    text.bytes().all(|b| (b' '..=b'~').contains(&b))
}

/// `spans` as one row when they are plain ASCII and fit in `avail` columns, which is most rows.
/// It must give exactly what the full wrap gives such text: empty spans dropped, neighbors of
/// the same style joined, and trailing spaces cut unless the row holds nothing else.
fn one_ascii_row(spans: &[Span<'_>], avail: usize) -> Option<Vec<Span<'static>>> {
    if !spans.iter().all(|span| is_plain_ascii(&span.content)) {
        return None;
    }
    let mut row: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    for span in spans.iter().filter(|span| !span.content.is_empty()) {
        match row.last_mut() {
            Some(last) if last.style == span.style => last.content.to_mut().push_str(&span.content),
            _ => row.push(to_static_span(span)),
        }
    }
    if row.iter().any(|span| !span.content.trim().is_empty()) {
        while let Some(last) = row.last_mut() {
            let kept = last.content.trim_end().len();
            if kept == 0 {
                row.pop();
            } else {
                last.content.to_mut().truncate(kept);
                break;
            }
        }
    }
    let used: usize = row.iter().map(|span| span.content.len()).sum();
    (used <= avail).then_some(row)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Space,
    Newline,
}

/// A run of graphemes of one kind, which may cross span boundaries and so carry several styles.
#[derive(Debug)]
struct Token {
    kind: Kind,
    parts: Vec<(String, Style)>,
    width: usize,
}

impl Token {
    fn add(&mut self, grapheme: &str, style: Style, width: usize) {
        match self.parts.last_mut() {
            Some((text, last)) if *last == style => text.push_str(grapheme),
            _ => self.parts.push((grapheme.to_owned(), style)),
        }
        self.width += width;
    }
}

fn tokenize(spans: &[Span<'_>]) -> Vec<Token> {
    let breaks = word_breaks(spans);
    let mut tokens: Vec<Token> = Vec::new();
    let mut offset = 0;
    let mut previous_ascii = true;
    for span in spans {
        for grapheme in span.content.graphemes(true) {
            let at = offset;
            offset += grapheme.len();
            let kind = if grapheme == "\n" || grapheme == "\r\n" {
                Kind::Newline
            } else if grapheme.chars().all(char::is_whitespace) {
                Kind::Space
            } else {
                Kind::Word
            };
            let width = if kind == Kind::Newline {
                0
            } else {
                str_width(grapheme)
            };
            // A word may break inside where Unicode allows it next to wide or other non-ASCII
            // text. ASCII words keep breaking only at spaces.
            let ascii = grapheme.is_ascii();
            let splits = kind == Kind::Word
                && !(ascii && previous_ascii)
                && breaks.binary_search(&at).is_ok();
            previous_ascii = ascii;
            match tokens.last_mut() {
                Some(last) if last.kind == kind && kind != Kind::Newline && !splits => {
                    last.add(grapheme, span.style, width);
                }
                _ => {
                    let mut token = Token {
                        kind,
                        parts: Vec::new(),
                        width: 0,
                    };
                    token.add(grapheme, span.style, width);
                    tokens.push(token);
                }
            }
        }
    }
    tokens
}

/// Byte offsets, into the spans' text run together, where a line may break inside a word under
/// Unicode line breaking. Empty for ASCII text, which breaks at spaces alone.
fn word_breaks(spans: &[Span<'_>]) -> Vec<usize> {
    if spans.iter().all(|span| span.content.is_ascii()) {
        return Vec::new();
    }
    let joined: String = spans.iter().map(|span| span.content.as_ref()).collect();
    unicode_linebreak::linebreaks(&joined)
        .filter(|(_, kind)| *kind == unicode_linebreak::BreakOpportunity::Allowed)
        .map(|(at, _)| at)
        .collect()
}

struct Rows<'p> {
    width: usize,
    first: &'p [Span<'static>],
    rest: &'p [Span<'static>],
    lines: Vec<Line<'static>>,
    row: Vec<Span<'static>>,
    used: usize,
    /// How the row being filled goes on from the one above.
    join: Join,
}

impl<'p> Rows<'p> {
    fn new(width: usize, first: &'p [Span<'static>], rest: &'p [Span<'static>]) -> Self {
        Self {
            width,
            first,
            rest,
            lines: Vec::new(),
            row: Vec::new(),
            used: 0,
            join: Join::NewLine,
        }
    }

    fn prefix(&self) -> &'p [Span<'static>] {
        if self.lines.is_empty() {
            self.first
        } else {
            self.rest
        }
    }

    /// Columns left for content after the prefix. Always at least one, so wrapping progresses.
    fn avail(&self) -> usize {
        self.width.saturating_sub(spans_width(self.prefix())).max(1)
    }

    fn end_row(&mut self) {
        marks::continues(&mut self.row, self.join);
        let mut spans = self.prefix().to_vec();
        spans.append(&mut self.row);
        self.lines.push(Line::from(spans));
        self.used = 0;
        self.join = Join::NewLine;
    }

    /// Ends the row where the line goes on in the next one.
    fn break_row(&mut self, join: Join) {
        self.end_row();
        self.join = join;
    }

    fn push_text(&mut self, text: &str, style: Style) {
        match self.row.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push_str(text),
            _ => self.row.push(Span::styled(text.to_owned(), style)),
        }
    }

    fn push(&mut self, token: &Token) {
        for (text, style) in &token.parts {
            self.push_text(text, *style);
        }
        self.used += token.width;
    }

    /// Pushes as much of `token` as fits on this row and drops the rest.
    fn push_clipped(&mut self, token: &Token) {
        for (text, style) in &token.parts {
            for grapheme in text.graphemes(true) {
                let w = str_width(grapheme);
                if self.used + w > self.avail() {
                    return;
                }
                self.push_text(grapheme, *style);
                self.used += w;
            }
        }
    }

    fn push_split(&mut self, token: &Token) {
        for (text, style) in &token.parts {
            for grapheme in text.graphemes(true) {
                let w = str_width(grapheme);
                if self.used > 0 && self.used + w > self.avail() {
                    self.break_row(Join::Direct);
                }
                self.push_text(grapheme, *style);
                self.used += w;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{plain_text, str_width};
    use ratatui::style::Stylize;

    fn wrap(text: &str, width: usize, first: &str, rest: &str) -> String {
        plain_text(&wrap_text(
            text,
            Style::default(),
            width,
            &[Span::raw(first.to_owned())],
            &[Span::raw(rest.to_owned())],
        ))
    }

    #[test]
    fn chinese_fills_its_rows_and_never_starts_one_with_a_comma() {
        let text = "我们需要给导出命令加一个参数，这样用户就可以选择输出格式。";
        let rows = wrap(text, 24, "● ", "  ");
        let rows: Vec<&str> = rows.lines().collect();
        for row in &rows[..rows.len() - 1] {
            assert!(str_width(row) >= 22, "{rows:?}");
        }
        for row in &rows[1..] {
            assert!(!row.trim_start().starts_with(['，', '。']), "{rows:?}");
        }
        assert_eq!(rows.concat().replace(['●', ' '], ""), text);
        assert_eq!(
            wrap("see src/app.rs and self-test", 12, "", ""),
            "see\nsrc/app.rs\nand\nself-test"
        );
    }

    #[test]
    fn hanging_indent_lines_up_under_the_text() {
        assert_eq!(
            wrap("one two three four five", 12, "● ", "  "),
            "● one two\n  three four\n  five"
        );
    }

    #[test]
    fn newlines_start_rows_and_keep_blank_lines() {
        assert_eq!(wrap("a\n\nb", 10, "- ", "  "), "- a\n\n  b");
        assert_eq!(wrap("a\n", 10, "", ""), "a");
    }

    #[test]
    fn long_words_split_between_graphemes() {
        assert_eq!(wrap("abcdefghij", 6, "> ", "  "), "> abcd\n  efgh\n  ij");
    }

    #[test]
    fn wide_characters_count_two_columns() {
        assert_eq!(wrap("你好世界", 6, "", ""), "你好世\n界");
    }

    #[test]
    fn leading_indentation_survives_but_wrap_spaces_do_not() {
        assert_eq!(
            wrap("    code line here", 12, "", ""),
            "    code\nline here"
        );
    }

    #[test]
    fn empty_text_keeps_the_prefix() {
        assert_eq!(wrap("", 10, "● ", "  "), "●");
    }

    #[test]
    fn styles_survive_wrapping() {
        let spans = [Span::raw("plain "), "bold".bold(), Span::raw(" tail")];
        let lines = wrap_spans(&spans, 8, &[], &[]);
        assert_eq!(plain_text(&lines), "plain\nbold\ntail");
        assert_eq!(
            marks::unmarked(lines[1].spans[0].style),
            Style::new().bold()
        );
    }

    #[test]
    fn hard_wrap_breaks_anywhere_and_keeps_styles() {
        let spans = [Span::raw("abc"), "defgh".bold()];
        let rows = hard_wrap_spans(&spans, 4);
        let text: Vec<String> = rows
            .iter()
            .map(|row| row.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text, ["abcd", "efgh"]);
        assert_eq!(rows[0][1].style, Style::new().bold());
        assert_eq!(hard_wrap_spans(&[], 4), vec![Vec::<Span<'static>>::new()]);
        let rows = hard_wrap_spans(&[Span::raw("  x  y")], 3);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0].content, "  y");
    }

    #[test]
    fn hard_wrap_never_splits_a_wide_character() {
        let rows = hard_wrap_spans(&[Span::raw("a你好")], 2);
        let text: Vec<String> = rows
            .iter()
            .map(|row| row.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text, ["a", "你", "好"]);
    }

    #[test]
    fn fit_lines_cuts_only_what_is_too_wide() {
        let mut lines = vec![Line::from("short"), Line::from("much too long")];
        fit_lines(&mut lines, 6);
        assert_eq!(plain_text(&lines), "short\nmuch …");
    }

    #[test]
    fn the_one_row_shortcut_matches_the_full_wrap() {
        let bold = Style::new().bold();
        let cases: Vec<Vec<Span<'static>>> = vec![
            vec![],
            vec![Span::raw("")],
            vec![Span::raw("   ")],
            vec![Span::raw("  head and tail  ")],
            vec![Span::raw("a "), Span::raw(" "), Span::styled("b", bold)],
            vec![
                Span::raw("same "),
                Span::raw("style "),
                Span::styled("  ", bold),
            ],
            vec![
                Span::styled("x", bold),
                Span::raw(""),
                Span::styled("y", bold),
            ],
            vec![Span::raw("exactly ten")],
            vec![Span::raw("a much longer line that has to wrap somewhere")],
            vec![Span::raw("caf\u{e9} is not ascii")],
            vec![Span::raw("two\nlines")],
            vec![Span::raw("tab\there")],
        ];
        for width in [1, 3, 10, 11, 12, 13, 40] {
            for spans in &cases {
                let first = [Span::raw("\u{25cf} ")];
                let rest = [Span::raw("  ")];
                assert_eq!(
                    wrap_spans(spans, width, &first, &rest),
                    wrap_tokens(spans, width, &first, &rest),
                    "{spans:?} at {width}"
                );
            }
        }
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
    fn two_cell_ambiguous_glyphs_cut_with_dots() {
        assert_eq!(cut_mark_for(40, 1), "…");
        assert_eq!(cut_mark_for(40, 2), "...");
        assert_eq!(cut_mark_for(3, 2), "");
        assert_eq!(crate::text::width_with("...", 2), 3);
    }
}
