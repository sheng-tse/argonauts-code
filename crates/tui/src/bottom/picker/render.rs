use argonauts_core::model_picker::{AllHandsToggle, EffortLine, EffortSlider};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Picker, VISIBLE_ROWS};
use crate::render::RenderCtx;
use crate::text::{KEEP, Part, fit_lines, fit_parts, sanitize_line, str_width, wrap_text};
use crate::theme::{Role, Theme};
use crate::views::layout::cut;

/// Labels past this are cut, so the descriptions keep their room.
const MAX_LABEL: usize = 36;
/// Between the label, description, and letter columns.
const GAP: usize = 3;
/// A narrower description column says too little to be worth drawing.
const MIN_DESCRIPTION: usize = 8;
/// The effort slider's left edge.
const SLIDER_INDENT: usize = 4;
/// Between the slider's levels, where there's room.
const SLIDER_GAP: usize = 6;
/// Between the slider's right end and All hands beside it.
const TOGGLE_GAP: usize = 4;
const TOGGLE_HINT: &str = "Tab to toggle";
/// Follows the level of a model that takes one only, as agy's `gemini-3.8-flash-high` does.
const FIXED: &str = "fixed by the model";

/// The marks beyond the theme's glyph set, with ASCII stand-ins.
struct Marks {
    current: &'static str,
    below: &'static str,
    above: &'static str,
    /// Starts the effort line under `/model`'s models.
    effort: &'static str,
    /// Points at the chosen level on the effort slider.
    chosen: &'static str,
}

impl Marks {
    fn for_theme(theme: &Theme) -> Self {
        if theme.glyphs.prompt.is_ascii() {
            Self {
                current: "*",
                below: "v",
                above: "^",
                effort: "*",
                chosen: "^",
            }
        } else {
            Self {
                current: "✓",
                below: "↓",
                above: "↑",
                effort: "◉",
                chosen: "▲",
            }
        }
    }
}

impl Picker {
    pub fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let card = theme.style(Role::Card);
        let dim = theme.style(Role::Dim);
        let indent = [Span::raw("  ")];

        let mut lines = vec![Line::from(Span::styled(
            theme.glyphs.rule.repeat(width),
            card,
        ))];
        lines.extend(wrap_text(
            sanitize_line(&self.title).trim(),
            card.add_modifier(Modifier::BOLD),
            width,
            &indent,
            &indent,
        ));
        let about = sanitize_line(&self.about);
        if !about.trim().is_empty() {
            lines.extend(wrap_text(about.trim(), dim, width, &indent, &indent));
        }
        lines.push(Line::default());

        let shown = self.shown();
        let warning = self.warning.clone().or_else(|| {
            shown
                .get(self.selected)
                .and_then(|&index| self.rows[index].warning.clone())
        });
        if let Some(warning) = warning {
            let warning = sanitize_line(&warning);
            let style = theme.style(Role::Warning);
            lines.extend(wrap_text(warning.trim(), style, width, &indent, &indent));
            lines.push(Line::default());
        }
        if !self.filter.is_empty() || self.filtering {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("Filter: ", dim),
                Span::raw(sanitize_line(&self.filter).into_owned()),
            ]));
        }

        if let Some(slider) = &self.flow.slider {
            lines.extend(slider_lines(slider, ctx));
        } else if let Some(text) = &self.input {
            lines.push(self.input_row(text, ctx));
        } else if shown.is_empty() {
            let text = if self.rows.is_empty() {
                sanitize_line(&self.placeholder).trim().to_owned()
            } else {
                "Nothing matches".to_owned()
            };
            lines.push(Line::from(vec![Span::raw("    "), Span::styled(text, dim)]));
        } else {
            self.push_rows(&mut lines, &shown, ctx);
        }
        if let Some(line) = self.flow.effort.as_ref().filter(|_| self.input.is_none()) {
            lines.push(Line::default());
            lines.push(self.effort_row(line, ctx));
        }

        lines.push(Line::default());
        lines.push(self.hint_line(ctx));
        fit_lines(&mut lines, width);
        lines
    }

    /// The typed line, its end kept in view when it's longer than the row.
    fn input_row(&self, text: &str, ctx: &RenderCtx<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let lead = format!("  {} ", theme.glyphs.prompt);
        let room = usize::from(ctx.width).saturating_sub(str_width(&lead) + 1);
        let text = sanitize_line(text).into_owned();
        let shown = if str_width(&text) <= room {
            text
        } else {
            let mut tail: Vec<char> = Vec::new();
            let mut used = str_width(theme.glyphs.ellipsis);
            for ch in text.chars().rev() {
                let cells = str_width(ch.encode_utf8(&mut [0; 4]));
                if used + cells > room {
                    break;
                }
                used += cells;
                tail.push(ch);
            }
            let tail: String = tail.into_iter().rev().collect();
            format!("{}{tail}", theme.glyphs.ellipsis)
        };
        Line::from(vec![
            Span::styled(lead, theme.style(Role::Card)),
            Span::raw(shown),
            Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        ])
    }

    fn push_rows(&self, lines: &mut Vec<Line<'static>>, shown: &[usize], ctx: &RenderCtx<'_>) {
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);
        let success = theme.style(Role::Success);

        let start = self.offset.min(shown.len().saturating_sub(1));
        let end = (start + VISIBLE_ROWS).min(shown.len());
        let below = shown.len() - end;
        let numbers: Vec<String> = (start..end).map(|at| format!("{}. ", at + 1)).collect();
        let number_width = numbers.iter().map(|n| str_width(n)).max().unwrap_or(0);
        // Two spaces, the pointer and its space, then the number.
        let prefix = 4 + number_width;
        let window = &shown[start..end];

        let label_text = |index: usize| {
            let row = &self.rows[index];
            let label = sanitize_line(&row.label).trim().to_owned();
            if row.current {
                format!("{label} {}", marks.current)
            } else {
                label
            }
        };
        let keyed = self.rows.iter().any(|row| row.key.is_some());
        let key_room = if keyed { GAP + 1 } else { 0 };
        let longest = window
            .iter()
            .map(|&index| str_width(&label_text(index)))
            .max()
            .unwrap_or(0);
        let label_width = longest
            .min(MAX_LABEL)
            .min(width.saturating_sub(prefix + key_room));
        let described = window
            .iter()
            .map(|&index| str_width(sanitize_line(&self.rows[index].description).trim()))
            .max()
            .unwrap_or(0);
        let description_at = prefix + label_width + GAP;
        let room = width.saturating_sub(description_at + key_room);
        let description_width = if described > 0 && room >= MIN_DESCRIPTION {
            described.min(room)
        } else {
            0
        };
        let key_at = if description_width > 0 {
            description_at + description_width + GAP
        } else {
            prefix + label_width + GAP
        };

        for (at, &index) in window.iter().enumerate() {
            let row = &self.rows[index];
            let position = start + at;
            if let Some(heading) = row.heading.as_deref().filter(|_| self.filter.is_empty()) {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(prefix)),
                    Span::styled(sanitize_line(heading).trim().to_owned(), dim),
                ]));
            }
            let (pointer, pointer_style) = if position == self.selected {
                (theme.glyphs.prompt, theme.style(Role::Card))
            } else if at + 1 == window.len() && below > 0 {
                (marks.below, dim)
            } else if at == 0 && start > 0 {
                (marks.above, dim)
            } else {
                (" ", dim)
            };
            let label_style = if row.current {
                success
            } else if position == self.selected {
                theme.style(Role::Card)
            } else {
                theme.style(Role::Text)
            };
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(format!("{pointer} "), pointer_style),
                Span::styled(format!("{:<number_width$}", numbers[at]), dim),
            ];
            let label = sanitize_line(&row.label).trim().to_owned();
            let mut used = prefix;
            if row.current {
                let mark = format!(" {}", marks.current);
                let label = cut(&label, label_width.saturating_sub(str_width(&mark)));
                used += str_width(&label) + str_width(&mark);
                spans.push(Span::styled(label, label_style));
                spans.push(Span::styled(mark, success));
            } else {
                let label = cut(&label, label_width);
                used += str_width(&label);
                spans.push(Span::styled(label, label_style));
            }
            let description = sanitize_line(&row.description).trim().to_owned();
            if description_width > 0 && !description.is_empty() {
                spans.push(Span::raw(" ".repeat(description_at.saturating_sub(used))));
                let description = cut(&description, description_width);
                used = description_at + str_width(&description);
                spans.push(Span::styled(description, dim));
            }
            if let Some(key) = row.key {
                spans.push(Span::raw(" ".repeat(key_at.saturating_sub(used).max(1))));
                spans.push(Span::styled(key.to_string(), dim));
            }
            lines.push(Line::from(spans));
            if let Some(under) = row.under.as_deref() {
                let under = cut(sanitize_line(under).trim(), width.saturating_sub(prefix));
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(prefix)),
                    Span::styled(under, dim),
                ]));
            }
        }
        if below > 0 {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{} +{below} more", theme.glyphs.ellipsis), dim),
            ]));
        }
    }

    /// `◉ xhigh effort (default)  ←/→ to adjust`, for the highlighted model, and no hint for
    /// one fixed at its only level.
    fn effort_row(&self, line: &EffortLine, ctx: &RenderCtx<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let marks = Marks::for_theme(theme);
        let default = line.default.and_then(|at| line.levels.get(at));
        let (level, note) = match line.level() {
            Some(level) if line.is_fixed() => (format!("{level} effort"), format!(" ({FIXED})")),
            Some(level) if line.is_default() => (format!("{level} effort"), " (default)".into()),
            Some(level) => (
                format!("{level} effort"),
                default.map_or_else(String::new, |default| format!(" (default {default})")),
            ),
            None => ("the CLI's own effort".to_owned(), String::new()),
        };
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{} ", marks.effort), theme.style(Role::Card)),
            Span::styled(sanitize_line(&level).into_owned(), theme.style(Role::Text)),
            Span::styled(sanitize_line(&note).into_owned(), dim),
        ];
        if !line.is_fixed() {
            spans.push(Span::styled(format!("  {}", self.adjust_hint()), dim));
        }
        Line::from(spans)
    }

    fn adjust_hint(&self) -> &'static str {
        if self.vim {
            "h/l to adjust"
        } else {
            "←/→ to adjust"
        }
    }

    fn hint_line(&self, ctx: &RenderCtx<'_>) -> Line<'static> {
        let dim = ctx.theme.style(Role::Dim);
        let part = |text: &str| vec![Span::styled(text.to_owned(), dim)];
        let dot = || Span::styled(" · ", dim);
        let enter = if self.enter_hint.is_empty() {
            "Enter to choose"
        } else {
            self.enter_hint.as_str()
        };
        let keyed = self.rows.iter().any(|row| row.key.is_some());
        let esc = |usual: &'static str| {
            if self.esc_hint.is_empty() {
                usual
            } else {
                self.esc_hint.as_str()
            }
        };
        let mut parts = Vec::new();
        let unmoving = self.flow.slider.as_ref().filter(|slider| !slider.moves());
        if let Some(slider) = unmoving {
            // Tab is named beside All hands, where the view offers it.
            if slider.all_hands.is_some() {
                parts.push(Part::new(part(enter), KEEP));
                parts.push(Part::joined(dot(), part(esc("Esc to cancel")), 1));
            } else {
                parts.push(Part::new(part(esc("Esc to close")), KEEP));
            }
        } else if self.flow.slider.is_some() {
            parts.push(Part::new(part(self.adjust_hint()), KEEP));
            parts.push(Part::joined(dot(), part(enter), KEEP));
            parts.push(Part::joined(dot(), part(esc("Esc to cancel")), 1));
        } else if self.input.is_some() {
            parts.push(Part::new(part(enter), KEEP));
            parts.push(Part::joined(dot(), part(esc("Esc to cancel")), 1));
        } else if self.rows.is_empty() {
            parts.push(Part::new(part(esc("Esc to close")), KEEP));
        } else if keyed {
            parts.push(Part::new(part(enter), KEEP));
            parts.push(Part::joined(dot(), part(esc("Esc to cancel")), 1));
        } else if self.filtering {
            parts.push(Part::new(part(enter), KEEP));
            parts.push(Part::joined(dot(), part("↑↓ to move"), 0));
            parts.push(Part::joined(dot(), part("Esc to clear the filter"), 1));
        } else {
            let (moving, filtering) = if self.vim {
                ("j/k to move", "/ to filter")
            } else {
                ("↑↓ to move", "type to filter")
            };
            parts.push(Part::new(part(enter), KEEP));
            parts.push(Part::joined(dot(), part(moving), 0));
            if self.rows.len() > 1 {
                parts.push(Part::joined(dot(), part(filtering), 0));
            }
            parts.push(Part::joined(dot(), part(esc("Esc to close")), 1));
        }
        fit_parts(vec![Span::raw("  ")], &parts, usize::from(ctx.width))
    }
}

/// `/effort`'s slider: "Faster" and "Smarter" over the ends of a line, a marker on it over the
/// chosen level, the levels under it, and what the chosen one means.
fn slider_lines(slider: &EffortSlider, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    if !slider.moves() {
        return fixed_level_lines(slider, ctx);
    }
    let theme = ctx.theme;
    let marks = Marks::for_theme(theme);
    let dim = theme.style(Role::Dim);
    let card = theme.style(Role::Card);
    let width = usize::from(ctx.width);
    let labels: Vec<String> = slider
        .stops
        .iter()
        .map(|stop| sanitize_line(&stop.level).trim().to_owned())
        .collect();
    let Some(last) = labels.len().checked_sub(1) else {
        return Vec::new();
    };
    let chosen = slider.chosen.min(last);
    let widths: Vec<usize> = labels.iter().map(|label| str_width(label)).collect();
    let total: usize = widths.iter().sum();
    let gaps = labels.len().saturating_sub(1);
    let (faster, smarter) = (EffortSlider::FASTER, EffortSlider::SMARTER);
    let ends = str_width(faster) + 1 + str_width(smarter);
    let room = width.saturating_sub(SLIDER_INDENT + 2);
    let scale = (total + gaps * SLIDER_GAP)
        .max(ends)
        .min(room)
        .max(total + gaps);
    // The levels spread evenly from one end of the line to the other.
    let spare = scale - total;
    let mut starts = Vec::with_capacity(labels.len());
    let share = |gap: usize| (spare * gap).checked_div(gaps);
    let mut at = if gaps == 0 { spare / 2 } else { 0 };
    for (index, cells) in widths.iter().enumerate() {
        starts.push(at);
        if let (Some(next), Some(this)) = (share(index + 1), share(index)) {
            at += cells + next - this;
        }
    }
    let indent = || Span::raw(" ".repeat(SLIDER_INDENT));
    let marker_at = starts[chosen] + widths[chosen].saturating_sub(1) / 2;

    let mut lines = vec![
        Line::from(vec![
            indent(),
            Span::styled(faster, dim),
            Span::raw(" ".repeat(scale.saturating_sub(ends) + 1)),
            Span::styled(smarter, dim),
        ]),
        Line::from(vec![
            indent(),
            Span::styled(theme.glyphs.rule.repeat(marker_at), dim),
            Span::styled(marks.chosen, if slider.cli_default { dim } else { card }),
            Span::styled(
                theme
                    .glyphs
                    .rule
                    .repeat(scale.saturating_sub(marker_at + 1)),
                dim,
            ),
        ]),
    ];
    let mut levels = vec![indent()];
    let mut used = 0;
    for (index, label) in labels.iter().enumerate() {
        levels.push(Span::raw(" ".repeat(starts[index].saturating_sub(used))));
        let style = if index == chosen && !slider.cli_default {
            card.add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        levels.push(Span::styled(label.clone(), style));
        used = starts[index] + widths[index];
    }
    lines.push(Line::from(levels));
    if let Some(toggle) = slider.all_hands {
        add_all_hands(&mut lines, toggle, scale, ctx);
    }
    lines.push(Line::default());
    let description = sanitize_line(&slider.stops[chosen].description)
        .trim()
        .to_owned();
    let description = match (description.is_empty(), slider.default == Some(chosen)) {
        // As `/model` says it: nothing is set, so the marker only shows where ← and → start.
        _ if slider.cli_default => "The CLI's own effort, until you pick a level".to_owned(),
        (true, true) => "The model's default".to_owned(),
        (true, false) => String::new(),
        (false, true) => format!("{}. The model's default", description.trim_end_matches('.')),
        (false, false) => description,
    };
    if !description.is_empty() {
        let lead = [indent()];
        lines.extend(wrap_text(
            &description,
            theme.style(Role::Text),
            width,
            &lead,
            &lead,
        ));
    }
    lines
}

/// All hands beside the slider, where Claude Code puts Ultracode: its name and state level with
/// the line and `Tab to toggle` under them, else one row under the levels where there's no room
/// beside it. While it's on, a line under the levels says what it does.
fn add_all_hands(
    lines: &mut Vec<Line<'static>>,
    toggle: AllHandsToggle,
    scale: usize,
    ctx: &RenderCtx<'_>,
) {
    let column = str_width(AllHandsToggle::NAME) + 2 + str_width("off");
    let end = SLIDER_INDENT + scale;
    // The rule row and the levels row are the last two, each `end` cells wide.
    let rows = lines.len();
    if end + TOGGLE_GAP + column.max(str_width(TOGGLE_HINT)) <= usize::from(ctx.width) {
        let pad =
            |line: &Line<'static>| " ".repeat((end + TOGGLE_GAP).saturating_sub(line.width()));
        let gap = pad(&lines[rows - 2]);
        lines[rows - 2].spans.push(Span::raw(gap));
        lines[rows - 2].spans.extend(all_hands_named(toggle, ctx));
        let gap = pad(&lines[rows - 1]);
        lines[rows - 1].spans.push(Span::raw(gap));
        lines[rows - 1]
            .spans
            .push(Span::styled(TOGGLE_HINT, ctx.theme.style(Role::Dim)));
    } else {
        lines.push(all_hands_row(toggle, ctx));
    }
    lines.extend(all_hands_means(toggle, ctx));
}

/// `All hands  on`, brighter while it's on.
fn all_hands_named(toggle: AllHandsToggle, ctx: &RenderCtx<'_>) -> Vec<Span<'static>> {
    let theme = ctx.theme;
    let (name, state) = if toggle.on {
        let label = theme.style(Role::Label);
        (label, label.add_modifier(Modifier::BOLD))
    } else {
        (theme.style(Role::Text), theme.style(Role::Dim))
    };
    let word = if toggle.on { "on" } else { "off" };
    vec![
        Span::styled(AllHandsToggle::NAME, name),
        Span::raw("  "),
        Span::styled(word, state),
    ]
}

/// All hands on a row of its own, with `Tab to toggle` after it.
fn all_hands_row(toggle: AllHandsToggle, ctx: &RenderCtx<'_>) -> Line<'static> {
    let mut row = vec![Span::raw(" ".repeat(SLIDER_INDENT))];
    row.extend(all_hands_named(toggle, ctx));
    row.push(Span::styled(
        format!(" · {TOGGLE_HINT}"),
        ctx.theme.style(Role::Dim),
    ));
    Line::from(row)
}

/// What All hands does, only while it's on.
fn all_hands_means(toggle: AllHandsToggle, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    if !toggle.on {
        return Vec::new();
    }
    let lead = [Span::raw(" ".repeat(SLIDER_INDENT))];
    let means = format!("{}: {}", AllHandsToggle::NAME, AllHandsToggle::MEANS);
    wrap_text(
        &means,
        ctx.theme.style(Role::Dim),
        usize::from(ctx.width),
        &lead,
        &lead,
    )
}

/// A model that takes one level has no slider: the level, as `/model` shows it, and how to get
/// another. One that takes none says so. Either way, the default agent's view still offers All
/// hands.
fn fixed_level_lines(slider: &EffortSlider, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let text = theme.style(Role::Text);
    let width = usize::from(ctx.width);
    let indent = || Span::raw(" ".repeat(SLIDER_INDENT));
    let lead = [indent()];
    let (mut lines, then) = match slider.level() {
        Some(level) => {
            let marks = Marks::for_theme(theme);
            let level = sanitize_line(level).trim().to_owned();
            let line = Line::from(vec![
                indent(),
                Span::styled(format!("{} ", marks.effort), theme.style(Role::Card)),
                Span::styled(format!("{level} effort"), text),
                Span::styled(format!(" ({FIXED})"), theme.style(Role::Dim)),
            ]);
            (vec![line], "To change it, pick another model with /model.")
        }
        None => {
            let model = sanitize_line(&slider.model).trim().to_owned();
            let says = format!("{model} takes no effort setting.");
            (
                wrap_text(&says, text, width, &lead, &lead),
                "To set an effort, pick another model with /model.",
            )
        }
    };
    lines.push(Line::default());
    if let Some(toggle) = slider.all_hands {
        lines.push(all_hands_row(toggle, ctx));
        lines.extend(all_hands_means(toggle, ctx));
        lines.push(Line::default());
    }
    lines.extend(wrap_text(then, text, width, &lead, &lead));
    lines
}
