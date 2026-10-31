//! The panel, laid out as Claude Code's: an edge rule, `Search prompts · everywhere`, the list
//! with a boxed preview beside it, the filter box, and the key hints.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{HistorySearch, Preview, age};
use crate::base::draw::render::RenderCtx;
use crate::base::text::{
    KEEP, Part, fit_parts, sanitize, spans_width, str_width, truncate_spans, wrap_text,
};
use crate::base::theme::{Role, Theme};

/// Left of the list and both boxes, and right of them.
const MARGIN: usize = 2;
/// The rule, the heading, the filter box's three rows, and the hints.
const FIXED_ROWS: usize = 6;
/// Narrower than this, the list takes the whole width and the preview goes.
const PREVIEW_FROM: usize = 64;
/// Between the list and the preview.
const GAP: usize = 2;
/// `37m ago`.
const AGE_MIN: usize = 7;
/// After the age, before the prompt.
const AGE_GAP: &str = "  ";

/// The marks beyond the theme's glyph set, with ASCII forms.
struct Marks {
    above: &'static str,
    below: &'static str,
    search: &'static str,
    corners: [&'static str; 4],
    across: &'static str,
    down: &'static str,
    navigate: &'static str,
}

impl Marks {
    fn for_theme(theme: &Theme) -> Self {
        if theme.glyphs.prompt.is_ascii() {
            Self {
                above: "^",
                below: "v",
                search: "/",
                corners: ["+", "+", "+", "+"],
                across: "-",
                down: "|",
                navigate: "up/down",
            }
        } else {
            Self {
                above: "↑",
                below: "↓",
                search: "⌕",
                corners: ["╭", "╮", "╰", "╯"],
                across: "─",
                down: "│",
                navigate: "↑/↓",
            }
        }
    }

    fn top(&self, width: usize) -> String {
        let [left, right, ..] = self.corners;
        format!(
            "{left}{}{right}",
            self.across.repeat(width.saturating_sub(2))
        )
    }

    fn bottom(&self, width: usize) -> String {
        let [.., left, right] = self.corners;
        format!(
            "{left}{}{right}",
            self.across.repeat(width.saturating_sub(2))
        )
    }
}

impl HistorySearch {
    /// The panel in at most `max_rows` rows, and the cursor's (column, row) in the filter box.
    pub fn lines(&self, ctx: &RenderCtx<'_>, max_rows: usize) -> (Vec<Line<'static>>, (u16, u16)) {
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let width = usize::from(ctx.width);
        let question = theme.style(Role::Question);
        let height = self.rows.min(max_rows.saturating_sub(FIXED_ROWS)).max(1);

        let mut lines = vec![
            Line::from(Span::styled(theme.glyphs.rule.repeat(width), question)),
            fit_line(
                vec![
                    margin(),
                    Span::styled("Search prompts", question),
                    Span::styled(format!(" · {}", self.scope.label()), question),
                ],
                width,
            ),
        ];

        let inner = width.saturating_sub(2 * MARGIN);
        let preview = (inner >= PREVIEW_FROM && height >= 3).then(|| inner / 2 - 2);
        let list_width = match preview {
            Some(boxed) => inner - boxed - GAP,
            None => inner,
        };
        let list = self.list_rows(theme, &marks, list_width, height);
        let preview = preview.map(|boxed| self.preview_rows(theme, &marks, boxed, height));
        for (row, list_row) in list.into_iter().enumerate() {
            let mut spans = vec![margin()];
            spans.extend(list_row);
            if let Some(preview) = &preview {
                pad_to(&mut spans, MARGIN + list_width + GAP);
                spans.extend(preview[row].iter().cloned());
            }
            lines.push(Line::from(spans));
        }

        let (filter_rows, column) = self.filter_box(theme, &marks, inner);
        let cursor_row = lines.len() + 1;
        lines.extend(filter_rows);
        lines.push(self.hints(theme, &marks, width));
        (lines, (column as u16, cursor_row as u16))
    }

    /// `height` rows of `width` columns, padded when a preview follows.
    fn list_rows(
        &self,
        theme: &Theme,
        marks: &Marks,
        width: usize,
        height: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let dim = theme.style(Role::Dim);
        let question = theme.style(Role::Question);
        let mut rows: Vec<Vec<Span<'static>>> = Vec::with_capacity(height);
        if self.shown.is_empty() {
            let empty = if self.prompts().is_empty() {
                "No prompts yet"
            } else {
                "No matching prompts"
            };
            rows.push(truncate_spans(&[Span::styled(empty, dim)], width));
        }
        let window = self.window(height);
        let prompts = self.prompts();
        let ages: Vec<String> = self.shown[window.clone()]
            .iter()
            .map(|shown| {
                prompts[shown.prompt]
                    .at
                    .map_or_else(String::new, |at| age(at, self.now))
            })
            .collect();
        let age_width = ages
            .iter()
            .map(|age| str_width(age))
            .max()
            .unwrap_or(0)
            .max(AGE_MIN);
        for (n, index) in window.clone().enumerate() {
            let shown = &self.shown[index];
            let prompt = &prompts[shown.prompt];
            let selected = index == self.selected;
            let mark = if selected {
                Span::styled(theme.glyphs.prompt, question)
            } else if n == 0 && window.start > 0 {
                Span::styled(marks.above, dim)
            } else if index + 1 == window.end && window.end < self.shown.len() {
                Span::styled(marks.below, dim)
            } else {
                Span::raw(" ")
            };
            let text_style = if selected { question } else { Style::new() };
            let letter_style = if selected {
                question.add_modifier(Modifier::BOLD)
            } else {
                theme.style(Role::Bright).add_modifier(Modifier::BOLD)
            };
            let age = &ages[n];
            let mut row = vec![
                mark,
                Span::raw(" "),
                Span::styled(format!("{age:<age_width$}"), dim),
                Span::raw(AGE_GAP),
            ];
            let room = width.saturating_sub(spans_width(&row));
            row.extend(highlighted(
                prompt.first_line(),
                &shown.letters,
                text_style,
                letter_style,
                room,
            ));
            rows.push(truncate_spans(&row, width));
        }
        rows.resize(height, Vec::new());
        rows
    }

    /// The highlighted prompt, whole and wrapped, in a box `width` wide and `height` tall.
    /// Claude Code draws it all dim: the prompt in the dim color, the border in the text color
    /// dimmed.
    fn preview_rows(
        &self,
        theme: &Theme,
        marks: &Marks,
        width: usize,
        height: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let border = theme.style(Role::Text).add_modifier(Modifier::DIM);
        let dim = theme.style(Role::Dim);
        let text_width = width.saturating_sub(4);
        let room = height - 2;
        let mut text: Vec<Vec<Span<'static>>> = self
            .preview_text(text_width, room)
            .into_iter()
            .map(|row| {
                if row.is_empty() {
                    Vec::new()
                } else {
                    vec![Span::styled(row, dim)]
                }
            })
            .collect();
        if text.len() > room {
            text.truncate(room);
            if let Some(last) = text.last_mut() {
                let mut spans = std::mem::take(last);
                spans.push(Span::styled(format!(" {}", theme.glyphs.ellipsis), dim));
                *last = truncate_spans(&spans, text_width);
            }
        }
        text.resize(room, Vec::new());

        let mut rows = vec![vec![Span::styled(marks.top(width), border)]];
        for content in text {
            let mut row = vec![Span::styled(marks.down, border), Span::raw(" ")];
            row.extend(content);
            pad_to(&mut row, width - 1);
            row.push(Span::styled(marks.down, border));
            rows.push(row);
        }
        rows.push(vec![Span::styled(marks.bottom(width), border)]);
        rows
    }

    /// The selected prompt's rows, `width` wide, one more than `room` when they don't all fit.
    /// Kept until the selection or the box changes.
    fn preview_text(&self, width: usize, room: usize) -> Vec<String> {
        let Some(shown) = self.shown.get(self.selected) else {
            return Vec::new();
        };
        let fits = |preview: &Preview| {
            (preview.scope, preview.prompt, preview.width, preview.room)
                == (self.scope, shown.prompt, width, room)
        };
        if let Some(preview) = self
            .preview
            .borrow()
            .as_ref()
            .filter(|preview| fits(preview))
        {
            return preview.rows.clone();
        }
        let rows = wrap_preview(&self.prompts()[shown.prompt].text, width, room);
        *self.preview.borrow_mut() = Some(Preview {
            scope: self.scope,
            prompt: shown.prompt,
            width,
            room,
            rows: rows.clone(),
        });
        rows
    }

    /// The box's three rows and the cursor's column.
    fn filter_box(
        &self,
        theme: &Theme,
        marks: &Marks,
        width: usize,
    ) -> (Vec<Line<'static>>, usize) {
        let border = theme.style(Role::Question);
        // The magnifier takes the text color, as in Claude Code, not the border's.
        let opening = format!(" {} ", marks.search);
        let room = width.saturating_sub(str_width(&opening) + 3);
        let mut row = vec![
            margin(),
            Span::styled(marks.down, border),
            Span::styled(opening, theme.style(Role::Text)),
        ];
        let column;
        if self.filter.is_empty() {
            let placeholder = format!("Filter history{}", theme.glyphs.ellipsis);
            column = spans_width(&row);
            row.extend(truncate_spans(
                &[Span::styled(placeholder, theme.style(Role::Dim))],
                room,
            ));
        } else {
            // The end of a long filter stays in view, where typing happens.
            let shown = tail(&sanitize(&self.filter), room.saturating_sub(1));
            row.push(Span::raw(shown));
            column = spans_width(&row);
        }
        pad_to(&mut row, MARGIN + width - 1);
        row.push(Span::styled(marks.down, border));
        let rows = vec![
            Line::from(vec![margin(), Span::styled(marks.top(width), border)]),
            Line::from(row),
            Line::from(vec![margin(), Span::styled(marks.bottom(width), border)]),
        ];
        (rows, column)
    }

    fn hints(&self, theme: &Theme, marks: &Marks, width: usize) -> Line<'static> {
        let dim = theme.style(Role::Dim);
        let hint = |text: String, rank: u16| {
            Part::joined(
                Span::styled(" · ", dim),
                vec![Span::styled(text, dim)],
                rank,
            )
        };
        let mut parts = vec![
            hint(
                format!(
                    "{}{}to navigate",
                    marks.navigate,
                    crate::base::theme::glyphs::gap_after(marks.navigate)
                ),
                2,
            ),
            hint("Enter to use".to_owned(), KEEP),
            hint("Esc to cancel".to_owned(), KEEP),
            hint("ctrl+s to scope".to_owned(), 1),
        ];
        if self.vim {
            parts.push(hint("Esc i / for slash commands".to_owned(), 0));
        }
        fit_parts(vec![margin()], &parts, width)
    }
}

fn margin() -> Span<'static> {
    Span::raw(" ".repeat(MARGIN))
}

fn fit_line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    Line::from(truncate_spans(&spans, width))
}

fn pad_to(spans: &mut Vec<Span<'static>>, width: usize) {
    let used = spans_width(spans);
    if used < width {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
}

/// Up to `room + 1` rows of `prompt` wrapped `width` wide, line breaks kept and blank lines at
/// either end dropped. Wrapping stops once the rows are more than `room`, so a long paste
/// costs no more than a short one.
fn wrap_preview(prompt: &str, width: usize, room: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    // Columns enough to fill every row and see that more follow.
    let enough = (room + 2) * width.max(1);
    // Blank lines go in only once a line with text follows them.
    let mut blanks = 0;
    for raw in prompt.split('\n') {
        for line in sanitize(raw).split('\n') {
            let line = line.trim_end();
            if line.trim().is_empty() {
                blanks += usize::from(!rows.is_empty());
                continue;
            }
            if rows.len() > room {
                rows.truncate(room + 1);
                return rows;
            }
            rows.extend(std::iter::repeat_n(String::new(), blanks));
            blanks = 0;
            rows.extend(
                wrap_text(head(line, enough), Style::new(), width, &[], &[])
                    .into_iter()
                    .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect()),
            );
        }
    }
    rows.truncate(room + 1);
    rows
}

/// The first `width` columns of `text`, or all of it.
fn head(text: &str, width: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices() {
        used += str_width(c.encode_utf8(&mut [0; 4]));
        if used > width {
            return &text[..at];
        }
    }
    text
}

/// The line with the chars at `letters` in `letter`, the rest in `style`, and no more of it
/// than `room` columns and one to show it goes on.
fn highlighted(
    line: &str,
    letters: &[usize],
    style: Style,
    letter: Style,
    room: usize,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_lit = false;
    let mut next = letters.iter().peekable();
    for (i, c) in head(line, room + 1).chars().enumerate() {
        let lit = next.next_if_eq(&&i).is_some();
        if lit != run_lit && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_lit { letter } else { style },
            ));
        }
        run_lit = lit;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_lit { letter } else { style }));
    }
    spans
}

/// The last `width` columns of `text`.
fn tail(text: &str, width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let mut kept = Vec::new();
    let mut used = 0;
    for grapheme in text.graphemes(true).rev() {
        let w = str_width(grapheme);
        if used + w > width {
            break;
        }
        used += w;
        kept.push(grapheme);
    }
    kept.reverse();
    kept.concat()
}
