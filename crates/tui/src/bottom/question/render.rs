use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::{ANSWER_PLACEHOLDER, OptionKind, QuestionNote, QuestionOption, QuestionPrompt};
use crate::base::draw::panes::{scroll_mark, window};
use crate::base::draw::render::RenderCtx;
use crate::base::text::marks::decorative;
use crate::base::text::{
    KEEP, Part, fit_lines, fit_parts, sanitize, sanitize_line, spans_width, str_width,
    truncate_line, wrap_spans, wrap_text,
};
use crate::base::theme::Role;
use crate::bottom::composer::{Composer, ComposerState};

/// How many options a question lists at once, as Claude Code's longer pickers do. The pointer
/// brings the rest into view.
const OPTIONS_SHOWN: usize = 10;

/// The fewest cells a description beside its option may wrap in on a framed question. Narrower,
/// it goes under its option.
const DESCRIPTION_ROOM: usize = 24;

/// A description as it reads on one row.
fn one_row(description: &str) -> String {
    sanitize_line(description).trim().to_owned()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuestionView {
    pub lines: Vec<Line<'static>>,
    /// The note's cursor, as (column, row) within `lines`.
    pub cursor: Option<(u16, u16)>,
    /// Rows that can go when the screen is short, before the options ever do.
    pub extra: Range<usize>,
    /// Rows at the bottom that can go when the screen is short.
    pub tail: usize,
}

/// Where a question draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// By itself, where the input was, or under a docked view that stays up over it.
    Alone,
    /// Between two faint rules, one cell in, its title alone on its row, as the guided setup's
    /// steps and the start's own questions draw.
    Framed,
}

impl Place {
    /// The blank cells every row starts with.
    fn margin(self) -> usize {
        match self {
            Place::Alone => 2,
            Place::Framed => 1,
        }
    }

    fn indent(self) -> Span<'static> {
        Span::raw(" ".repeat(self.margin()))
    }
}

impl QuestionPrompt {
    pub fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        self.layout(ctx).lines
    }

    pub fn layout(&self, ctx: &RenderCtx<'_>) -> QuestionView {
        self.layout_within(ctx, usize::MAX)
    }

    /// The question where the screen has `rows` rows for it. A long list of options, which
    /// shows as a window around the pointer, takes fewer of them on a short screen, so the
    /// rule, the title and the key line stay.
    pub fn layout_within(&self, ctx: &RenderCtx<'_>, rows: usize) -> QuestionView {
        self.layout_in(ctx, Place::Alone, rows)
    }

    /// The question under a docked view that stays up while it holds the keys: drawn as it is
    /// alone, across the screen, under the view's box.
    pub fn layout_under_view(&self, ctx: &RenderCtx<'_>) -> QuestionView {
        self.layout_in(ctx, Place::Alone, usize::MAX)
    }

    fn layout_in(&self, ctx: &RenderCtx<'_>, place: Place, rows: usize) -> QuestionView {
        let theme = ctx.theme;
        let glyphs = &theme.glyphs;
        let width = usize::from(ctx.width);
        let warning = theme.style(Role::Warn);
        let dim = theme.style(Role::Dim);
        let question = theme.style(Role::Question);

        if self.dismissed {
            return QuestionView::default();
        }
        if self.keys_only {
            return self.letters_layout(ctx);
        }
        // An option's sheet stands where the question was, under the question's rule.
        if let Some(sheet) = self.sheet.as_ref().filter(|sheet| sheet.open) {
            let mut lines = vec![Line::from(decorative(glyphs.rule.repeat(width), question))];
            lines.extend(sheet.lines(width, theme));
            return QuestionView {
                lines,
                ..QuestionView::default()
            };
        }
        let place = if self.framed && place == Place::Alone {
            Place::Framed
        } else {
            place
        };

        let indent = [place.indent()];
        let details = self.detail.as_slice();
        let mut lines = Vec::new();
        match place {
            Place::Alone => {
                lines.push(Line::from(decorative(glyphs.rule.repeat(width), question)));
                // A warning reads in the warning colour from its mark to its last word.
                let title = if self.warns {
                    warning
                } else {
                    question.add_modifier(Modifier::BOLD)
                };
                lines.extend(wrap_text(
                    &sanitize_line(&self.title),
                    title,
                    width,
                    &[Span::styled(format!("{} ", glyphs.question), warning)],
                    &indent,
                ));
            }
            Place::Framed => {
                let faint = theme.style(Role::Faint);
                lines.push(Line::from(decorative(glyphs.rule.repeat(width), faint)));
                let title = if self.warns {
                    warning
                } else {
                    theme.style(Role::Text)
                };
                lines.extend(wrap_text(
                    sanitize_line(&self.title).trim(),
                    title.add_modifier(Modifier::BOLD),
                    width,
                    &indent,
                    &indent,
                ));
            }
        }

        let extra_start = lines.len();
        if let Some(chips) = self.chip_row(ctx, place) {
            lines.push(chips);
        }
        if let Some(agent) = &self.agent {
            let mut spans = vec![
                place.indent(),
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
        for detail in details {
            for part in whole_parts(&sanitize(detail), width.saturating_sub(place.margin())) {
                lines.extend(wrap_text(&part, dim, width, &indent, &indent));
            }
        }
        let plain = theme.style(Role::Text);
        if self.two_columns {
            lines.extend(two_columns(
                &self.noted_rows,
                plain,
                dim,
                width,
                place.margin(),
            ));
        } else {
            for row in &self.noted_rows {
                lines.extend(noted_row(row, plain, dim, width, place.margin()));
            }
        }
        if self.body.is_empty() {
            lines.push(Line::default());
        } else {
            // Dashed rules, as Claude Code frames the diff it asks about. Around a warning's
            // words they step back further, so the words stand out.
            let rule = if self.warns {
                theme.style(Role::Faint)
            } else {
                dim
            };
            let dashed = Line::from(Span::styled(glyphs.dashed.repeat(width), rule));
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
            // A question that asks several questions shows them bold, as Claude Code's question
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
        let (cursor, options) = self.push_options(&mut lines, ctx, place);

        let tail_start = lines.len();
        lines.push(Line::default());
        lines.push(self.hint_line(ctx, place, ""));
        if let Some(footer) = self.footer.as_deref().map(sanitize_line) {
            let footer = Line::from(vec![
                place.indent(),
                Span::styled(footer.trim().to_owned(), dim),
            ]);
            lines.push(truncate_line(&footer, width));
        }
        if place == Place::Framed {
            let faint = theme.style(Role::Faint);
            lines.push(Line::from(decorative(glyphs.rule.repeat(width), faint)));
        }
        let tail = lines.len() - tail_start;
        // Wrapping keeps an indent whole, which a terminal narrower than it can't show.
        fit_lines(&mut lines, width);
        let view = QuestionView {
            lines,
            cursor,
            extra,
            tail,
        };
        self.scrolled(view, &options, rows, ctx, place)
    }

    /// `view` with a window of its options, the ones around the pointer, where they are too
    /// many to list at once. The key line then says which of them show. In fewer than the
    /// rows it needs, what the question says about itself goes first, then the window
    /// shrinks, and the rule, the title and the key line always stay. `options` are each
    /// option's rows in `view`.
    fn scrolled(
        &self,
        view: QuestionView,
        options: &[Range<usize>],
        room: usize,
        ctx: &RenderCtx<'_>,
        place: Place,
    ) -> QuestionView {
        let (Some(first), Some(last)) = (options.first(), options.last()) else {
            return view;
        };
        let total = options.len();
        if total <= OPTIONS_SHOWN {
            return view;
        }
        let all = first.start..last.end;
        // The blank row and the footer around the key line, which give way before it does.
        let around_keys = view.tail.saturating_sub(1);
        // The head, the question's own words and the key line.
        let stays = view.lines.len() - all.len() - view.extra.len() - around_keys;
        let pointed = self.selected.min(total - 1);
        let rows_of =
            |shown: &Range<usize>| options[shown.end - 1].end - options[shown.start].start;
        let mut count = total.min(OPTIONS_SHOWN);
        let shown = loop {
            let (from, to) = window(pointed, total, count);
            let shown = from..to;
            if count == 1 || stays + rows_of(&shown) <= room {
                break shown;
            }
            count -= 1;
        };
        // Rows left over go to the rows around the key line, then to the details, the last
        // of them first.
        let mut left = room.saturating_sub(stays + rows_of(&shown));
        let whole_tail = left >= around_keys;
        if whole_tail {
            left -= around_keys;
        }
        let details = view.extra.len().min(left);

        let keys_at = view.lines.len() - around_keys;
        let (top, bottom) = (options[shown.start].start, options[shown.end - 1].end);
        let mut lines = view.lines[..view.extra.start].to_vec();
        let extra = lines.len()..lines.len() + details;
        lines.extend_from_slice(&view.lines[view.extra.end - details..all.start]);
        let moved_to = lines.len();
        lines.extend_from_slice(&view.lines[top..bottom]);
        let tail_start = lines.len();
        if whole_tail {
            lines.extend_from_slice(&view.lines[all.end..keys_at]);
        }
        let mark = scroll_mark(shown.start, shown.end, total, ctx.theme);
        lines.push(self.hint_line(ctx, place, &mark));
        if whole_tail {
            lines.extend_from_slice(&view.lines[keys_at + 1..]);
        }
        let cursor = view.cursor.and_then(|(column, row)| {
            let row = usize::from(row);
            (top..bottom)
                .contains(&row)
                .then(|| (column, (row - top + moved_to) as u16))
        });
        QuestionView {
            tail: lines.len() - tail_start,
            lines,
            cursor,
            extra,
        }
    }

    /// A question its letters answer, on one line under what it asks about, as
    /// `Run it now? y / N`: each option's letter, the default's capital and bold.
    fn letters_layout(&self, ctx: &RenderCtx<'_>) -> QuestionView {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(
                sanitize_line(&self.title).trim().to_owned(),
                theme.style(Role::Text),
            ),
        ];
        let count = self.options.len();
        for (index, option) in self.options.iter().enumerate() {
            let Some(letter) = option.label.chars().next() else {
                continue;
            };
            spans.push(Span::raw(" "));
            if option.recommended {
                spans.push(Span::styled(
                    letter.to_uppercase().to_string(),
                    theme.style(Role::Text).add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(letter.to_lowercase().to_string(), dim));
            }
            if index + 1 < count {
                spans.push(Span::styled(" /", dim));
            }
        }
        let mut lines = vec![Line::from(spans)];
        fit_lines(&mut lines, usize::from(ctx.width));
        QuestionView {
            lines,
            ..QuestionView::default()
        }
    }

    /// The current chip is dark text on the question color, as Claude Code draws it.
    fn chip_row(&self, ctx: &RenderCtx<'_>, place: Place) -> Option<Line<'static>> {
        if self.chips.is_empty() {
            return None;
        }
        let theme = ctx.theme;
        let glyphs = &theme.glyphs;
        let dim = theme.style(Role::Dim);
        let current = self.chips.iter().position(|chip| !chip.answered);
        // The current chip pads its box with a cell each side, which starts one cell early
        // when it's first, so every chip's box lines up with the question's text.
        let opening = place.margin() - usize::from(current == Some(0));
        let mut spans = vec![Span::raw(" ".repeat(opening))];
        for (index, chip) in self.chips.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("  "));
            }
            let label = sanitize_line(&chip.label).trim().to_owned();
            if Some(index) == current {
                let style = theme.style(Role::Question).add_modifier(Modifier::REVERSED);
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

    /// Returns the note's cursor, and the rows each option took in `lines`.
    fn push_options(
        &self,
        lines: &mut Vec<Line<'static>>,
        ctx: &RenderCtx<'_>,
        place: Place,
    ) -> (Option<(u16, u16)>, Vec<Range<usize>>) {
        if self.is_answer_field() {
            return (self.push_field(lines, ctx, place), Vec::new());
        }
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);
        let question = theme.style(Role::Question);
        let numbers: Vec<String> = (1..=self.options.len()).map(|n| format!("{n}. ")).collect();
        let number_width = numbers.iter().map(|n| str_width(n)).max().unwrap_or(0);
        // The margin, the pointer and its space, then the number.
        let prefix_width = place.margin() + 2 + number_width;
        let labels: Vec<Vec<Span<'static>>> = self
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| self.label_spans(index, option, ctx))
            .collect();
        let explained: Vec<Vec<Span<'static>>> = self
            .options
            .iter()
            .map(|option| explanation(option, ctx))
            .collect();
        let pointed = self.selected.min(self.options.len().saturating_sub(1));
        // A question that asks for it has each description beside its option wherever it draws.
        let described = self.description_column(&labels, width.saturating_sub(prefix_width));
        // The seats that back an option line up after the longest label of those that have some.
        let seated = self
            .options
            .iter()
            .zip(&labels)
            .filter(|(option, _)| !option.seats.is_empty())
            .map(|(_, label)| spans_width(label) + SEATS_GAP)
            .max();
        let mut cursor = None;
        let mut starts = Vec::with_capacity(self.options.len());
        for (index, option) in self.options.iter().enumerate() {
            starts.push(lines.len());
            let here = self.focused && index == pointed;
            let pointer = if here {
                format!("{} ", theme.glyphs.prompt)
            } else {
                "  ".to_owned()
            };
            let number = format!("{:<number_width$}", numbers[index]);
            // Under a view or framed, the number reads with its label, as the view's own rows do.
            let number_style = match place {
                Place::Alone if !self.warns => dim,
                _ if here => question,
                _ => theme.style(Role::Text),
            };
            let first = [
                place.indent(),
                Span::styled(pointer, question),
                Span::styled(number, number_style),
            ];
            let rest = [Span::raw(" ".repeat(prefix_width))];
            let noted = self.note.as_ref().filter(|note| note.option == index);

            if let Some(note) = noted {
                let label = sanitize_line(&option.label).trim().to_owned();
                let (rows, at) = if option.kind == OptionKind::FreeText {
                    editor_rows(&note.editor, &label, self.focused, &first, &rest, ctx)
                } else {
                    let placeholder = sanitize_line(&self.note_placeholder).trim().to_owned();
                    note_rows(&note.text(), Some(&label), &placeholder, &first, &rest, ctx)
                };
                cursor = at.map(|(column, row)| (column, row + lines.len() as u16));
                lines.extend(rows);
            } else if let Some(column) = described
                && let Some(description) = option.description.as_deref()
            {
                let mut row = first.to_vec();
                row.extend(labels[index].iter().cloned());
                let gap = column - spans_width(&labels[index]);
                row.push(Span::raw(" ".repeat(gap)));
                let hang = [Span::raw(" ".repeat(prefix_width + column))];
                lines.extend(wrap_text(&one_row(description), dim, width, &row, &hang));
                continue;
            } else if let Some(column) = seated.filter(|_| !option.seats.is_empty()) {
                let mut row = first.to_vec();
                row.extend(labels[index].iter().cloned());
                row.push(Span::raw(" ".repeat(column - spans_width(&labels[index]))));
                for (n, face) in option.seats.iter().enumerate() {
                    if n > 0 {
                        row.push(Span::raw(" ".repeat(SEATS_GAP - 1)));
                    }
                    row.extend(face.badge(theme));
                    row.push(Span::raw(" "));
                    row.push(Span::styled(face.beside(theme), dim));
                }
                lines.push(truncate_line(&Line::from(row), width));
            } else {
                lines.extend(wrap_spans(&labels[index], width, &first, &rest));
            }
            if !explained[index].is_empty() {
                lines.extend(wrap_spans(&explained[index], width, &rest, &rest));
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
                    let text_style = if here {
                        question
                    } else {
                        theme.style(Role::Text)
                    };
                    let opening = [
                        Span::raw(" ".repeat(prefix_width)),
                        Span::styled(pointer, question),
                        Span::styled(format!("{glyph} "), if on { question } else { dim }),
                    ];
                    let hang = [Span::raw(
                        " ".repeat(prefix_width + 2 + str_width(glyph) + 1),
                    )];
                    lines.extend(wrap_text(
                        &sanitize_line(text),
                        text_style,
                        width,
                        &opening,
                        &hang,
                    ));
                }
            }
        }
        let ends = starts.iter().skip(1).copied().chain([lines.len()]);
        let rows = starts
            .iter()
            .copied()
            .zip(ends)
            .map(|(a, b)| a..b)
            .collect();
        (cursor, rows)
    }

    /// The column, counted from where the labels start, at which every description is drawn
    /// beside its option: [`Self::descriptions_beside`] cells after the longest label. `None`
    /// when the question doesn't ask for it, a note is open, or a row wouldn't fit in `room`
    /// cells.
    fn description_column(&self, labels: &[Vec<Span<'static>>], room: usize) -> Option<usize> {
        let gap = self.descriptions_beside?;
        if self.note.is_some() || self.checklist.is_some() {
            return None;
        }
        let column = labels.iter().map(|label| spans_width(label)).max()? + gap;
        let longest = self
            .options
            .iter()
            .filter_map(|option| option.description.as_deref())
            .map(|description| str_width(&one_row(description)))
            .max()?;
        // A framed question wraps a long one beside its option, as the setup's steps do.
        let needs = if self.framed {
            longest.min(DESCRIPTION_ROOM)
        } else {
            longest
        };
        (column + needs <= room).then_some(column)
    }

    /// A free-text option is dim, like the placeholder it is.
    fn label_spans(
        &self,
        index: usize,
        option: &QuestionOption,
        ctx: &RenderCtx<'_>,
    ) -> Vec<Span<'static>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let selected = self.focused && index == self.selected.min(self.options.len() - 1);
        let style = if selected {
            theme.style(Role::Question)
        } else if option.kind == OptionKind::FreeText {
            dim
        } else {
            theme.style(Role::Text)
        };
        let mut spans = Vec::new();
        if option.kind == OptionKind::Toggle {
            let glyphs = &theme.glyphs;
            let (glyph, glyph_style) = if option.on {
                (glyphs.checkbox_on, theme.style(Role::Question))
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

    /// A free-text question's answer field, open from the start, with the `❯` Claude Code's
    /// input has, and the input's own editor. Until the question has the keys it shows no cursor,
    /// and the input under it is where typing goes.
    fn push_field(
        &self,
        lines: &mut Vec<Line<'static>>,
        ctx: &RenderCtx<'_>,
        place: Place,
    ) -> Option<(u16, u16)> {
        let theme = ctx.theme;
        let pointer = if self.focused {
            format!("{} ", theme.glyphs.prompt)
        } else {
            "  ".to_owned()
        };
        let first = [
            place.indent(),
            Span::styled(pointer, theme.style(Role::Question)),
        ];
        let rest = [Span::raw(" ".repeat(place.margin() + 2))];
        let editor = self.note.as_ref().map(|note| &note.editor);
        let empty = Composer::field();
        let (rows, at) = editor_rows(
            editor.unwrap_or(&empty),
            ANSWER_PLACEHOLDER,
            self.focused,
            &first,
            &rest,
            ctx,
        );
        let top = lines.len() as u16;
        lines.extend(rows);
        at.map(|(column, row)| (column, row + top))
    }

    /// The keys, and at the row's far end `mark`: which of a long list of options show.
    fn hint_line(&self, ctx: &RenderCtx<'_>, place: Place, mark: &str) -> Line<'static> {
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
            parts.push(Part::new(part("↑  to answer"), KEEP));
        } else if let Some(list) = &self.checklist {
            if self.note.is_some() {
                hint("Enter to send", "Esc to go back");
            } else if self.wants_note(list.option) {
                hint("Space to tick · Enter to add a note", "Esc to go back");
            } else {
                hint("Space to tick · Enter to send", "Esc to go back");
            }
        } else if self.is_answer_field() {
            parts.push(Part::new(part("Enter to send"), KEEP));
            let esc = sanitize_line(&self.esc_hint).trim().to_owned();
            if !esc.is_empty() {
                parts.push(Part::joined(dot(), part(&esc), 1));
            }
            // Once you've typed, Shift+Tab stays with the field, so a draft isn't lost to the
            // main agent answering in autopilot.
            let empty = self.note.as_ref().is_none_or(QuestionNote::is_empty);
            if self.needs_you && self.offers_shift_tab && empty {
                parts.push(Part::joined(dot(), part("shift+tab to cycle modes"), 0));
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
            // "Enter to cancel the question · Esc to cancel" would offer one word for both ways.
            let back = if verb.starts_with("cancel") {
                "Esc to go back"
            } else {
                "Esc to cancel"
            };
            let costs = armed
                .warning
                .as_deref()
                .map(|warning| format!(", {}", sanitize_line(warning).trim()))
                .unwrap_or_default();
            hint(&format!("Enter to {verb}{costs}"), back);
        } else {
            let enter = match selected.map(|option| option.kind) {
                Some(OptionKind::View) => "Enter to open",
                Some(OptionKind::FreeText) => "Enter to type",
                Some(OptionKind::Checklist) => "Enter to pick",
                Some(OptionKind::Toggle) => "Enter to switch",
                _ => self.enter_hint.as_deref().unwrap_or("Enter to choose"),
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
            // As Claude Code's prompts offer it: autopilot, a press or two away, settles a run's
            // question that waits on you.
            if self.needs_you && self.offers_shift_tab {
                parts.push(Part::joined(dot(), part("shift+tab to cycle modes"), 0));
            }
        }
        let width = usize::from(ctx.width);
        if mark.is_empty() {
            return fit_parts(vec![place.indent()], &parts, width);
        }
        // The mark ends as far from the right edge as the rows start from the left one.
        let end = width.saturating_sub(place.margin());
        let mark_width = str_width(mark);
        let mut line = fit_parts(
            vec![place.indent()],
            &parts,
            end.saturating_sub(mark_width + 2),
        );
        let gap = end.saturating_sub(spans_width(&line.spans) + mark_width);
        line.spans.push(Span::raw(" ".repeat(gap)));
        line.spans.push(Span::styled(mark.to_owned(), dim));
        truncate_line(&line, width)
    }
}

/// What an option says after its label: the model it names, in its CLI's color, then why to
/// pick it.
fn explanation(option: &QuestionOption, ctx: &RenderCtx<'_>) -> Vec<Span<'static>> {
    let theme = ctx.theme;
    let dim = theme.style(Role::Dim);
    let mut spans = Vec::new();
    if let Some(model) = &option.model {
        let style = match Role::for_cli(&model.cli) {
            Role::CatOther => dim,
            role => theme.style(role),
        };
        spans.push(Span::styled(
            sanitize_line(&model.name).trim().to_owned(),
            style,
        ));
    }
    if let Some(description) = option.description.as_deref() {
        let gap = if spans.is_empty() { "" } else { " " };
        spans.push(Span::styled(format!("{gap}{}", sanitize(description)), dim));
    }
    if let Some(countdown) = &option.counts_to {
        let join = if spans.is_empty() { "" } else { " · " };
        let text = sanitize_line(&countdown.text(ctx.clock.now)).into_owned();
        spans.push(Span::styled(format!("{join}{text}"), dim));
    }
    spans
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Columns the editor draws its own prompt in, which the question's prefix takes the place of.
const EDITOR_PROMPT: usize = 2;

/// Cells between an option's label and the seats that back it.
const SEATS_GAP: usize = 3;

/// An answer in the input's own editor, drawn as the input draws it without its rules, after
/// `first` on its first row and `rest` on the others. Empty, it shows `placeholder`. The cursor
/// is (column, row) within the returned rows, while the question has the keys.
fn editor_rows(
    editor: &Composer,
    placeholder: &str,
    focused: bool,
    first: &[Span<'static>],
    rest: &[Span<'static>],
    ctx: &RenderCtx<'_>,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let margin = spans_width(first).max(spans_width(rest));
    let room = usize::from(ctx.width).saturating_sub(margin).max(1) + EDITOR_PROMPT;
    let inner = RenderCtx {
        width: u16::try_from(room).unwrap_or(u16::MAX),
        ..*ctx
    };
    let view = editor.render_with(
        &inner,
        ComposerState {
            focused,
            busy: false,
            offer_example: false,
            placeholder: Some(placeholder),
        },
    );
    // The first and last lines are the input's rules.
    let rows = view.lines.len().saturating_sub(2);
    let lines = view
        .lines
        .iter()
        .skip(1)
        .take(rows)
        .enumerate()
        .map(|(index, row)| {
            let mut spans = if index == 0 { first } else { rest }.to_vec();
            spans.extend(row.spans.iter().skip(1).cloned());
            Line::from(spans)
        })
        .collect();
    let (column, row) = view.cursor;
    let cursor = focused.then(|| {
        let column = usize::from(column).saturating_sub(EDITOR_PROMPT) + margin;
        (
            u16::try_from(column).unwrap_or(u16::MAX),
            row.saturating_sub(1),
        )
    });
    (lines, cursor)
}

/// A note or an answer being typed, after `label` when it notes an option. Empty, it shows
/// its placeholder with the cursor on its first letter, as Claude Code's amend field does. The
/// cursor is (column, row) within the returned rows.
fn note_rows(
    text: &str,
    label: Option<&str>,
    placeholder: &str,
    first: &[Span<'static>],
    rest: &[Span<'static>],
    ctx: &RenderCtx<'_>,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let theme = ctx.theme;
    let width = usize::from(ctx.width);
    let block = Style::new().add_modifier(Modifier::REVERSED);
    let text = sanitize_line(text).into_owned();
    let mut spans = Vec::new();
    if let Some(label) = label {
        spans.push(Span::styled(
            format!("{label}, "),
            theme.style(Role::Question),
        ));
    }

    if text.trim().is_empty() {
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
    // The wrap drops trailing spaces, so the cursor goes after them by hand, onto a row of its
    // own when the last row is full.
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

/// One row of a noted table: its parts plain and dim in turn. A row too wide for the question wraps
/// under where its second column starts, so the section names stay alone on the left.
fn noted_row(
    row: &str,
    plain: Style,
    dim: Style,
    width: usize,
    margin: usize,
) -> Vec<Line<'static>> {
    let spans: Vec<Span<'static>> = row
        .split('\t')
        .enumerate()
        .map(|(at, part)| {
            let style = if at % 2 == 1 { dim } else { plain };
            Span::styled(sanitize_line(part).into_owned(), style)
        })
        .collect();
    let text: String = row.split('\t').collect();
    let section = text.chars().take_while(|c| *c != ' ').count();
    let gap = text.chars().skip(section).take_while(|c| *c == ' ').count();
    let under = (margin + section + gap).min(width / 2);
    wrap_spans(
        &spans,
        width,
        &[Span::raw(" ".repeat(margin))],
        &[Span::raw(" ".repeat(under))],
    )
}

/// Cells between two columns.
const COLUMN_GAP: usize = 6;
/// The room a right column needs, short of its longest cell, before the two columns stand side
/// by side: less, and each row's cells go one under the other.
const RIGHT_COLUMN: usize = 40;

/// One row of a two-column table: its left cell's plain and dim words, then its right cell's.
struct Cells<'a> {
    left: (&'a str, &'a str),
    right: (&'a str, &'a str),
}

impl<'a> Cells<'a> {
    fn of(row: &'a str) -> Option<Self> {
        let mut parts = row.split('\t');
        let cells = Self {
            left: (parts.next()?, parts.next()?),
            right: (parts.next()?, parts.next()?),
        };
        parts.next().is_none().then_some(cells)
    }

    /// The row that names the columns: dim words alone in both.
    fn names(&self) -> Option<(&'a str, &'a str)> {
        let named = self.left.0.is_empty()
            && self.right.0.is_empty()
            && !self.left.1.is_empty()
            && !self.right.1.is_empty();
        named.then_some((self.left.1.trim(), self.right.1.trim()))
    }

    fn left_width(&self) -> usize {
        str_width(self.left.0) + str_width(self.left.1)
    }

    fn right_width(&self) -> usize {
        str_width(self.right.0) + str_width(self.right.1)
    }
}

fn cell(words: (&str, &str), plain: Style, dim: Style) -> Vec<Span<'static>> {
    [(words.0, plain), (words.1, dim)]
        .into_iter()
        .filter(|(text, _)| !text.is_empty())
        .map(|(text, style)| Span::styled(sanitize_line(text).into_owned(), style))
        .collect()
}

/// A two-column table two cells in: side by side where the right column has room for its
/// cells, or 40 cells of it, each cell wrapping in its own column, and else each row's left
/// cell then its right one, on lines that start with the columns' names.
fn two_columns(
    rows: &[String],
    plain: Style,
    dim: Style,
    width: usize,
    margin: usize,
) -> Vec<Line<'static>> {
    let indent = margin + 2;
    let table: Vec<Option<Cells<'_>>> = rows.iter().map(|row| Cells::of(row)).collect();
    let names = table.iter().flatten().find_map(Cells::names);
    let left = table
        .iter()
        .flatten()
        .map(Cells::left_width)
        .max()
        .unwrap_or(0);
    let right_at = indent + left + COLUMN_GAP;
    let longest = table
        .iter()
        .flatten()
        .filter(|cells| !cells.right.0.is_empty() || cells.names().is_some())
        .map(Cells::right_width)
        .max()
        .unwrap_or(0);
    let side_by_side =
        names.is_none() || width.saturating_sub(right_at) >= longest.min(RIGHT_COLUMN);
    let pad = |cells: usize| Span::raw(" ".repeat(cells));
    let mut lines = Vec::new();
    for (row, cells) in rows.iter().zip(&table) {
        let Some(cells) = cells else {
            lines.extend(noted_row(row, plain, dim, width, margin));
            continue;
        };
        if side_by_side {
            let mut first = vec![pad(indent)];
            first.extend(cell(cells.left, plain, dim));
            first.push(pad(right_at - indent - cells.left_width()));
            lines.extend(wrap_spans(
                &cell(cells.right, plain, dim),
                width,
                &first,
                &[pad(right_at)],
            ));
            continue;
        }
        let Some((now, after)) = names else {
            continue;
        };
        if cells.names().is_some() {
            continue;
        }
        let labelled = |name: &str, words: (&str, &str)| {
            let label = format!("{name}: ");
            let under = indent + str_width(&label);
            let first = [pad(indent), Span::styled(label, dim)];
            wrap_spans(&cell(words, plain, dim), width, &first, &[pad(under)])
        };
        if !cells.left.0.is_empty() || !cells.left.1.is_empty() {
            lines.extend(labelled(now, cells.left));
        }
        if cells.right.0.is_empty() {
            // A note on the row above sits under that row's value.
            let under = indent + str_width(after) + 2;
            lines.extend(wrap_spans(
                &cell(cells.right, plain, dim),
                width,
                &[pad(under)],
                &[pad(under)],
            ));
        } else {
            lines.extend(labelled(after, cells.right));
        }
    }
    lines
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
