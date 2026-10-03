use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::{CardNote, CardOption, CardPrompt, OptionKind};
use crate::render::RenderCtx;
use crate::text::marks::decorative;
use crate::text::{
    KEEP, Part, fit_lines, fit_parts, sanitize, sanitize_line, spans_width, str_width,
    truncate_line, wrap_spans, wrap_text,
};
use crate::theme::Role;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CardView {
    pub lines: Vec<Line<'static>>,
    /// The note's cursor, as (column, row) within `lines`.
    pub cursor: Option<(u16, u16)>,
    /// Rows that can go when the screen is short, before the options ever do.
    pub extra: Range<usize>,
    /// Rows at the bottom that can go when the screen is short.
    pub tail: usize,
}

impl CardPrompt {
    pub fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        self.layout(ctx).lines
    }

    pub fn layout(&self, ctx: &RenderCtx<'_>) -> CardView {
        let theme = ctx.theme;
        let glyphs = &theme.glyphs;
        let width = usize::from(ctx.width);
        let warning = theme.style(Role::Warning);
        let dim = theme.style(Role::Dim);
        let card = theme.style(Role::Card);

        if self.pinned {
            let line = Line::from(vec![
                Span::styled(format!("{} ", glyphs.card), warning),
                Span::raw(sanitize_line(&self.short_title).into_owned()),
                Span::styled(" · needs you · ↑ to answer", dim),
            ]);
            return CardView {
                lines: vec![truncate_line(&line, width)],
                ..CardView::default()
            };
        }

        let indent = [Span::raw("  ")];
        let mut lines = vec![Line::from(decorative(glyphs.rule.repeat(width), card))];
        lines.extend(wrap_text(
            &sanitize_line(&self.title),
            card.add_modifier(Modifier::BOLD),
            width,
            &[Span::styled(format!("{} ", glyphs.card), warning)],
            &indent,
        ));

        let extra_start = lines.len();
        if let Some(chips) = self.chip_row(ctx) {
            lines.push(chips);
        }
        if let Some(agent) = &self.agent {
            let mut spans = vec![
                Span::raw("  "),
                Span::styled("Agent: ", dim),
                Span::styled(
                    sanitize_line(&agent.name).trim().to_owned(),
                    theme.agent_style(agent.color),
                ),
            ];
            let detail = sanitize_line(&agent.detail).trim().to_owned();
            if !detail.is_empty() {
                spans.push(Span::styled(format!(" · {detail}"), dim));
            }
            lines.push(truncate_line(&Line::from(spans), width));
        }
        for detail in &self.detail {
            for part in whole_parts(&sanitize(detail), width.saturating_sub(2)) {
                lines.extend(wrap_text(&part, dim, width, &indent, &indent));
            }
        }
        if self.body.is_empty() {
            lines.push(Line::default());
        } else {
            // Dashed rules, as Claude Code frames the diff it asks about.
            let dashed = Line::from(Span::styled(glyphs.dashed.repeat(width), dim));
            lines.push(dashed.clone());
            for row in &self.body {
                lines.extend(wrap_text(
                    &sanitize(row),
                    theme.style(Role::Text),
                    width,
                    &indent,
                    &indent,
                ));
            }
            lines.push(dashed);
        }
        let extra = extra_start..lines.len();

        if let Some(question) = &self.question {
            // A card that asks several questions shows them bold, as Claude Code's question
            // dialog does. A single question reads like its permission prompt, in plain text.
            let style = if self.chips.is_empty() {
                theme.style(Role::Text)
            } else {
                theme.style(Role::Text).add_modifier(Modifier::BOLD)
            };
            lines.extend(wrap_text(
                &sanitize(question),
                style,
                width,
                &indent,
                &indent,
            ));
        }
        let cursor = self.push_options(&mut lines, ctx);

        let tail_start = lines.len();
        lines.push(Line::default());
        lines.push(self.hint_line(ctx));
        if let Some(footer) = self.footer.as_deref().map(sanitize_line) {
            let footer = Line::from(vec![
                Span::raw("  "),
                Span::styled(footer.trim().to_owned(), dim),
            ]);
            lines.push(truncate_line(&footer, width));
        }
        let tail = lines.len() - tail_start;
        // Wrapping keeps an indent whole, which a terminal narrower than it can't show.
        fit_lines(&mut lines, width);
        CardView {
            lines,
            cursor,
            extra,
            tail,
        }
    }

    /// The current chip is dark text on the card color, as Claude Code draws it.
    fn chip_row(&self, ctx: &RenderCtx<'_>) -> Option<Line<'static>> {
        if self.chips.is_empty() {
            return None;
        }
        let theme = ctx.theme;
        let glyphs = &theme.glyphs;
        let dim = theme.style(Role::Dim);
        let current = self.chips.iter().position(|chip| !chip.answered);
        // The current chip pads its box with a cell each side, which starts one cell early
        // when it's first, so every chip's box lines up with the card's text.
        let lead = if current == Some(0) { " " } else { "  " };
        let mut spans = vec![Span::raw(lead)];
        for (index, chip) in self.chips.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("  "));
            }
            let label = sanitize_line(&chip.label).trim().to_owned();
            if Some(index) == current {
                let style = theme.style(Role::Card).add_modifier(Modifier::REVERSED);
                spans.push(Span::styled(
                    format!(" {} {label} ", glyphs.checkbox_off),
                    style,
                ));
            } else {
                let box_glyph = if chip.answered {
                    glyphs.checkbox_on
                } else {
                    glyphs.checkbox_off
                };
                spans.push(Span::styled(format!("{box_glyph} {label}"), dim));
            }
        }
        Some(truncate_line(&Line::from(spans), usize::from(ctx.width)))
    }

    /// Returns the note's cursor.
    fn push_options(
        &self,
        lines: &mut Vec<Line<'static>>,
        ctx: &RenderCtx<'_>,
    ) -> Option<(u16, u16)> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);
        let card = theme.style(Role::Card);
        let numbers: Vec<String> = (1..=self.options.len()).map(|n| format!("{n}. ")).collect();
        let number_width = numbers.iter().map(|n| str_width(n)).max().unwrap_or(0);
        // Two spaces, the pointer and its space, then the number.
        let prefix_width = 4 + number_width;
        let labels: Vec<Vec<Span<'static>>> = self
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| self.label_spans(index, option, ctx))
            .collect();
        let pointed = self.selected.min(self.options.len().saturating_sub(1));
        let mut cursor = None;
        for (index, option) in self.options.iter().enumerate() {
            let pointer = if self.focused && index == pointed {
                format!("{} ", theme.glyphs.prompt)
            } else {
                "  ".to_owned()
            };
            let number = format!("{:<number_width$}", numbers[index]);
            let first = [
                Span::raw("  "),
                Span::styled(pointer, card),
                Span::styled(number, dim),
            ];
            let rest = [Span::raw(" ".repeat(prefix_width))];

            if let Some(note) = self.note.as_ref().filter(|note| note.option == index) {
                let (rows, at) = self.note_rows(note, option, &first, &rest, ctx);
                cursor = at.map(|(column, row)| (column, row + lines.len() as u16));
                lines.extend(rows);
            } else {
                lines.extend(wrap_spans(&labels[index], width, &first, &rest));
            }
            if let Some(description) = option.description.as_deref() {
                lines.extend(wrap_text(&sanitize(description), dim, width, &rest, &rest));
            }
            if let Some(list) = self.checklist.as_ref().filter(|list| list.option == index) {
                for (item, text) in option.items.iter().enumerate() {
                    let on = list.checked.get(item).copied().unwrap_or(false);
                    let here = item == list.cursor;
                    let glyph = if on {
                        theme.glyphs.checkbox_on
                    } else {
                        theme.glyphs.checkbox_off
                    };
                    let pointer = if here {
                        format!("{} ", theme.glyphs.prompt)
                    } else {
                        "  ".to_owned()
                    };
                    let text_style = if here { card } else { theme.style(Role::Text) };
                    let lead = [
                        Span::raw(" ".repeat(prefix_width)),
                        Span::styled(pointer, card),
                        Span::styled(format!("{glyph} "), if on { card } else { dim }),
                    ];
                    let hang = [Span::raw(
                        " ".repeat(prefix_width + 2 + str_width(glyph) + 1),
                    )];
                    lines.extend(wrap_text(
                        &sanitize_line(text),
                        text_style,
                        width,
                        &lead,
                        &hang,
                    ));
                }
            }
        }
        cursor
    }

    /// A free-text option is dim, like the placeholder it is.
    fn label_spans(
        &self,
        index: usize,
        option: &CardOption,
        ctx: &RenderCtx<'_>,
    ) -> Vec<Span<'static>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let selected = self.focused && index == self.selected.min(self.options.len() - 1);
        let style = if selected {
            theme.style(Role::Card)
        } else if option.kind == OptionKind::FreeText {
            dim
        } else {
            theme.style(Role::Text)
        };
        let mut spans = Vec::new();
        if option.kind == OptionKind::Toggle {
            let glyphs = &theme.glyphs;
            let (glyph, glyph_style) = if option.on {
                (glyphs.checkbox_on, theme.style(Role::Card))
            } else {
                (glyphs.checkbox_off, dim)
            };
            spans.push(Span::styled(format!("{glyph} "), glyph_style));
        }
        spans.push(Span::styled(
            sanitize_line(&option.label).trim().to_owned(),
            style,
        ));
        if option.recommended {
            spans.push(Span::styled(" (recommended)", dim));
        }
        spans
    }

    /// An empty note shows its placeholder with the cursor on its first letter, as Claude
    /// Code's amend field does. The cursor is (column, row) within the returned rows.
    fn note_rows(
        &self,
        note: &CardNote,
        option: &CardOption,
        first: &[Span<'static>],
        rest: &[Span<'static>],
        ctx: &RenderCtx<'_>,
    ) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let block = Style::new().add_modifier(Modifier::REVERSED);
        let text = sanitize_line(&note.text).into_owned();
        let label = sanitize_line(&option.label).trim().to_owned();
        let free_text = option.kind == OptionKind::FreeText;
        let mut spans = Vec::new();
        if !free_text {
            spans.push(Span::styled(format!("{label}, "), theme.style(Role::Card)));
        }

        if text.trim().is_empty() {
            let placeholder = if free_text {
                label
            } else {
                sanitize_line(&self.note_placeholder).trim().to_owned()
            };
            let first_letter = placeholder.graphemes(true).next().unwrap_or(" ").to_owned();
            spans.push(Span::styled(first_letter.clone(), block));
            spans.push(Span::styled(
                placeholder[first_letter.len().min(placeholder.len())..].to_owned(),
                theme.style(Role::Dim),
            ));
            let rows = wrap_spans(&spans, width, first, rest);
            let at = find_block(&rows);
            return (rows, at);
        }

        spans.push(Span::raw(text.clone()));
        let mut rows = wrap_spans(&spans, width, first, rest);
        // The wrap drops trailing spaces, so the cursor goes after them by hand, onto a row of
        // its own when the last row is full.
        let trailing = str_width(&text) - str_width(text.trim_end());
        let used = rows.last().map_or(0, |row| spans_width(&row.spans));
        if used + trailing + 1 > width {
            rows.push(Line::from(rest.to_vec()));
        }
        let last = rows.len() - 1;
        let row = &mut rows[last];
        let column = spans_width(&row.spans) + if used + trailing < width { trailing } else { 0 };
        if column > spans_width(&row.spans) {
            row.spans
                .push(Span::raw(" ".repeat(column - spans_width(&row.spans))));
        }
        row.spans.push(Span::styled(" ", block));
        let at = (
            u16::try_from(column).unwrap_or(u16::MAX),
            u16::try_from(last).unwrap_or(u16::MAX),
        );
        (rows, Some(at))
    }

    fn hint_line(&self, ctx: &RenderCtx<'_>) -> Line<'static> {
        let dim = ctx.theme.style(Role::Dim);
        let dot = || Span::styled(" · ", dim);
        let part = |text: &str| vec![Span::styled(text.to_owned(), dim)];
        let selected = self.options.get(self.selected);
        let mut parts = Vec::new();
        let mut hint = |first: &str, second: &str| {
            parts.push(Part::new(part(first), KEEP));
            parts.push(Part::joined(dot(), part(second), 0));
        };
        if !self.focused {
            parts.push(Part::new(part("↑ to answer"), KEEP));
        } else if let Some(list) = &self.checklist {
            if self.note.is_some() {
                hint("Enter to send", "Esc to go back");
            } else if self.wants_note(list.option) {
                hint("Space to tick · Enter to add a note", "Esc to go back");
            } else {
                hint("Space to tick · Enter to send", "Esc to go back");
            }
        } else if let Some(note) = &self.note {
            let free_text = self
                .options
                .get(note.option)
                .is_some_and(|option| option.kind == OptionKind::FreeText);
            if free_text {
                hint("Enter to answer", "Esc to close");
            } else {
                hint("Enter to choose with the note", "Esc to close the note");
            }
        } else if let Some(armed) = self.armed.and_then(|index| self.options.get(index)) {
            let verb = lowercase_first(sanitize_line(&armed.label).trim());
            // "Enter to cancel the card · Esc to cancel" would offer one word for both ways.
            let back = if verb.starts_with("cancel") {
                "Esc to go back"
            } else {
                "Esc to cancel"
            };
            hint(&format!("Enter to {verb}"), back);
        } else {
            let enter = match selected.map(|option| option.kind) {
                Some(OptionKind::View) => "Enter to open",
                Some(OptionKind::FreeText) => "Enter to type",
                Some(OptionKind::Checklist) => "Enter to pick",
                Some(OptionKind::Toggle) => "Enter to switch",
                _ => "Enter to choose",
            };
            parts.push(Part::new(part(enter), KEEP));
            if self.can_note(self.selected)
                && selected.is_some_and(|option| option.kind != OptionKind::FreeText)
            {
                parts.push(Part::joined(dot(), part("Tab to add a note"), 0));
            }
            let esc = sanitize_line(&self.esc_hint).trim().to_owned();
            if !esc.is_empty() {
                parts.push(Part::joined(dot(), part(&esc), 1));
            }
        }
        fit_parts(vec![Span::raw("  ")], &parts, usize::from(ctx.width))
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn find_block(rows: &[Line<'static>]) -> Option<(u16, u16)> {
    for (row, line) in rows.iter().enumerate() {
        let mut column = 0;
        for span in &line.spans {
            if span.style.add_modifier.contains(Modifier::REVERSED) {
                return Some((
                    u16::try_from(column).unwrap_or(u16::MAX),
                    u16::try_from(row).unwrap_or(u16::MAX),
                ));
            }
            column += span.width();
        }
    }
    None
}

/// A `·`-joined line broken between its parts where it must wrap, so `$0.00 spent` never
/// leaves `spent` on a row of its own. A part wider than `room` still wraps by words.
fn whole_parts(text: &str, room: usize) -> Vec<String> {
    const JOIN: &str = " · ";
    if text.contains('\n') || str_width(text) <= room {
        return vec![text.to_owned()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for part in text.split(JOIN) {
        if line.is_empty() {
            line = part.to_owned();
        } else if str_width(&line) + str_width(JOIN) + str_width(part) <= room {
            line.push_str(JOIN);
            line.push_str(part);
        } else {
            lines.push(std::mem::replace(&mut line, part.to_owned()));
        }
    }
    lines.push(line);
    lines
}
