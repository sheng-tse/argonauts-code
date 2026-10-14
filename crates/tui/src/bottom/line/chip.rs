//! One CLI on the bottom line: its dot, its name, and its plan windows as bars, cells or words.

use argonauts_core::settings::bottom_line::LineForm;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::fit::{Fine, Second, Shape};
use super::{CliState, CliUse, WindowUse};
use crate::kit::{Kit, bars, merge};
use crate::theme::Role;

/// A window's bar.
const BAR_CELLS: usize = 6;

/// A bar on the line: the lower half of each cell, so a row of them reads as marks beside
/// their numbers and not as blocks, with its edge to half a cell. Without color or block
/// glyphs it is the kit's meter, which has a form for both.
pub(super) fn small_bar(kit: &Kit<'_>, share: f64, fill: Role) -> Vec<Span<'static>> {
    if !kit.has_color() || kit.theme.glyphs.is_ascii() {
        return bars::block_meter(kit, share, BAR_CELLS, fill, Role::BarTrack);
    }
    let halves = (share.clamp(0.0, 1.0) * (BAR_CELLS * 2) as f64).round() as usize;
    let cell = |at: usize| match halves.saturating_sub(at * 2) {
        0 => Span::styled("▄", kit.fg(Role::BarTrack)),
        1 => Span::styled("▖", kit.fg(fill)),
        _ => Span::styled("▄", kit.fg(fill)),
    };
    merge((0..BAR_CELLS).map(cell).collect())
}

pub(super) struct Look<'a> {
    pub kit: &'a Kit<'a>,
    pub form: LineForm,
    pub as_left: bool,
}

pub(super) fn draw(cli: &CliUse, shape: &Shape, look: &Look<'_>) -> Vec<Span<'static>> {
    Chip { cli, shape, look }.draw()
}

struct Chip<'a> {
    cli: &'a CliUse,
    shape: &'a Shape,
    look: &'a Look<'a>,
}

impl Chip<'_> {
    fn draw(&self) -> Vec<Span<'static>> {
        if self.look.form == LineForm::Text {
            return self.as_text();
        }
        let cli = self.cli;
        // A CLI that is fine and not the one in use steps down to a name, then to a dot.
        let fine = if cli.main || cli.needs_you() {
            Fine::Bar
        } else {
            self.shape.fine
        };
        let mut spans = self.head(fine);
        let more = match &cli.state {
            CliState::Down => vec![self.kit().span("down", Role::Danger)],
            CliState::WrappingUp => vec![self.kit().span("wrapping up", Role::Warn)],
            CliState::NearLimit => vec![self.kit().span("near limit", Role::Warn)],
            CliState::Out { until } => self.out(until.as_deref()),
            CliState::Fine if fine >= Fine::Name => Vec::new(),
            CliState::Fine => match self.look.form {
                LineForm::Gauges => self.gauges(),
                LineForm::Compact => self.compact(fine),
                _ => self.bars(fine),
            },
        };
        join(&mut spans, more);
        spans
    }

    fn kit(&self) -> &Kit<'_> {
        self.look.kit
    }

    /// The dot in the CLI's color, filled for the one in use, and its name unless a narrow
    /// terminal took it.
    fn head(&self, fine: Fine) -> Vec<Span<'static>> {
        let cli = self.cli;
        let kit = self.kit();
        let ascii = kit.theme.glyphs.is_ascii();
        let dot = match (cli.main, ascii) {
            (true, false) => "●",
            (false, false) => "○",
            (true, true) => "*",
            (false, true) => "o",
        };
        let mut spans = vec![Span::styled(dot, kit.fg(cli.color))];
        if fine < Fine::Dot && !self.shape.names_gone {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(cli.name.clone(), self.name_style()));
        }
        spans
    }

    fn name_style(&self) -> Style {
        if self.cli.main {
            self.kit().bold(Role::Bright)
        } else {
            self.kit().style(Role::Dim)
        }
    }

    /// A number beside a bar: bold for the CLI in use, dim for the others, amber near a limit.
    fn number_style(&self, near: bool) -> Style {
        let kit = self.kit();
        match (near, self.cli.main) {
            (true, true) => kit.bold(Role::Warn),
            (true, false) => kit.style(Role::Warn),
            (false, true) => kit.bold(Role::Bright),
            (false, false) => kit.style(Role::Dim),
        }
    }

    fn fill(&self, window: &WindowUse) -> Role {
        if window.near {
            Role::Warn
        } else {
            self.cli.color
        }
    }

    /// The share a window's bar and number show: what is used, or what is left.
    fn shown(&self, window: &WindowUse) -> f64 {
        if self.look.as_left {
            1.0 - window.used
        } else {
            window.used
        }
    }

    /// `56%`, or `44% left` where the numbers say what is left, so none reads as used.
    fn percent(&self, window: &WindowUse) -> Span<'static> {
        let percent = (self.shown(window) * 100.0).round() as u8;
        let unit = if self.look.as_left { " left" } else { "" };
        Span::styled(format!("{percent}%{unit}"), self.number_style(window.near))
    }

    fn bar(&self, window: &WindowUse) -> Vec<Span<'static>> {
        small_bar(self.kit(), self.shown(window), self.fill(window))
    }

    /// One cell for a window, an eighth of a cell per eighth used and never empty.
    fn cell(&self, window: &WindowUse) -> Span<'static> {
        let kit = self.kit();
        Span::styled(level(kit, self.shown(window)), kit.fg(self.fill(window)))
    }

    /// `resets 4:40pm`, then `4:40pm`, then nothing, and how old the reading is while there's
    /// room for the long form.
    fn after_number(&self, window: &WindowUse) -> Vec<Span<'static>> {
        let kit = self.kit();
        let mut spans = Vec::new();
        if let Some(at) = window.resets.as_deref().filter(|_| !self.shape.reset_gone) {
            let words = if self.shape.short_words {
                at.to_owned()
            } else {
                format!("resets {at}")
            };
            spans.push(kit.span(words, Role::Warn));
        }
        if let Some(age) = window.age.as_deref().filter(|_| !self.shape.short_words) {
            join(&mut spans, vec![kit.dim(format!("{age} ago"))]);
        }
        spans
    }

    /// `5h ██████ 93% resets 4:40pm 7d ███▍   56%` for the CLI in use or one near its limit,
    /// `7d ▋      11%` for another. The fuller window is never given up.
    fn bars(&self, fine: Fine) -> Vec<Span<'static>> {
        let cli = self.cli;
        let kit = self.kit();
        let Some(fuller) = cli.fuller() else {
            return self.note();
        };
        if fine == Fine::Cell {
            let window = &cli.windows[fuller];
            return vec![self.cell(window), Span::raw(" "), self.percent(window)];
        }
        let mut spans = Vec::new();
        for (at, window) in cli.windows.iter().enumerate() {
            let mut one = Vec::new();
            let second = at != fuller;
            match (second, self.shape.second) {
                (true, Second::Gone) => continue,
                (true, Second::Cell) => {
                    one.push(kit.dim(window.label.clone()));
                    join(&mut one, vec![self.cell(window)]);
                }
                _ if self.shape.main_bar_gone => {}
                _ => {
                    one.push(kit.dim(window.label.clone()));
                    join(&mut one, self.bar(window));
                }
            }
            join(&mut one, vec![self.percent(window)]);
            join(&mut one, self.after_number(window));
            join(&mut spans, one);
        }
        spans
    }

    /// One bar, the fuller window, with no label: `██████ 56%`.
    fn compact(&self, fine: Fine) -> Vec<Span<'static>> {
        let Some(window) = self.cli.fuller().map(|at| &self.cli.windows[at]) else {
            return self.note();
        };
        let mut spans = match fine {
            _ if self.shape.main_bar_gone => Vec::new(),
            Fine::Cell => vec![self.cell(window)],
            _ => self.bar(window),
        };
        join(&mut spans, vec![self.percent(window)]);
        join(&mut spans, self.after_number(window));
        spans
    }

    /// One cell per window, the shorter window first, and the number of any that is near its
    /// limit, since a cell alone can't say which. Where even the cells are too much, the
    /// fuller window's number stands for them.
    fn gauges(&self) -> Vec<Span<'static>> {
        let cli = self.cli;
        let Some(fuller) = cli.fuller() else {
            return self.note();
        };
        if self.shape.main_bar_gone {
            let window = &cli.windows[fuller];
            let mut spans = vec![self.percent(window)];
            join(&mut spans, self.after_number(window));
            return spans;
        }
        let shown = |at: &usize| *at == fuller || self.shape.second != Second::Gone;
        let windows: Vec<&WindowUse> = (0..cli.windows.len())
            .filter(shown)
            .map(|at| &cli.windows[at])
            .collect();
        let mut spans: Vec<Span<'static>> =
            windows.iter().map(|window| self.cell(window)).collect();
        for window in windows.iter().filter(|window| window.near) {
            join(&mut spans, vec![self.percent(window)]);
            join(&mut spans, self.after_number(window));
        }
        // A reading that can't be read again says how old it is, as the bars do.
        let fuller = &cli.windows[fuller];
        if let Some(age) = fuller.age.as_deref().filter(|_| !fuller.near)
            && !self.shape.short_words
        {
            join(&mut spans, vec![self.kit().dim(format!("{age} ago"))]);
        }
        spans
    }

    /// A red bar while there's room, and when it comes back: `out until Tue 3:45pm`, then
    /// `out · Tue 3:45pm`, then `out`.
    fn out(&self, until: Option<&str>) -> Vec<Span<'static>> {
        let kit = self.kit();
        let shape = self.shape;
        let mut spans = Vec::new();
        let drawn_as_bars = matches!(self.look.form, LineForm::Fit | LineForm::Bars);
        if drawn_as_bars && !shape.out_bar_gone {
            spans = small_bar(kit, 1.0, Role::Danger);
        }
        join(
            &mut spans,
            vec![kit.span(self.out_words(until), Role::Danger)],
        );
        spans
    }

    /// What a CLI without a plan shows in a window's place, such as what it spent.
    fn note(&self) -> Vec<Span<'static>> {
        match self.cli.note.as_deref().filter(|_| !self.shape.short_words) {
            Some(note) => vec![self.kit().dim(note.to_owned())],
            None => Vec::new(),
        }
    }

    /// `out until Tue 3:45pm`, then `out · Tue 3:45pm`, then `out`.
    fn out_words(&self, until: Option<&str>) -> String {
        match until.filter(|_| !self.shape.out_time_gone) {
            Some(until) if self.shape.short_words => format!("out · {until}"),
            Some(until) => format!("out until {until}"),
            None => "out".to_owned(),
        }
    }

    /// Today's line: `● Claude 5h 1% · 7d 56% used`, bold for the CLI in use and dim for the
    /// others, with no bars at all. A CLI that isn't in use has no dot until its name goes.
    fn as_text(&self) -> Vec<Span<'static>> {
        let cli = self.cli;
        let kit = self.kit();
        let shape = self.shape;
        let fine = if cli.main || cli.needs_you() {
            Fine::Bar
        } else {
            shape.fine
        };
        let (words, tone) = match &cli.state {
            CliState::Down => ("down".to_owned(), Some(Role::Danger)),
            CliState::WrappingUp => ("wrapping up".to_owned(), Some(Role::Warn)),
            CliState::NearLimit => ("near limit".to_owned(), Some(Role::Warn)),
            CliState::Out { until } => (self.out_words(until.as_deref()), Some(Role::Danger)),
            CliState::Fine if fine >= Fine::Name => (String::new(), None),
            CliState::Fine => self.windows_in_words(),
        };
        let mut style = kit.style(tone.unwrap_or(if cli.main { Role::Bright } else { Role::Dim }));
        if cli.main {
            style = style.add_modifier(Modifier::BOLD);
        }
        let named = fine < Fine::Dot && !shape.names_gone;
        let mut text = String::new();
        if named {
            text.push_str(&cli.name);
        }
        if !words.is_empty() {
            if named {
                text.push(' ');
            }
            text.push_str(&words);
        }
        let mut spans = Vec::new();
        if cli.main || !named {
            let dot = self.head(Fine::Dot).remove(0);
            spans.push(if cli.main {
                Span::styled(dot.content, style)
            } else {
                dot
            });
        }
        if !text.is_empty() {
            join(&mut spans, vec![Span::styled(text, style)]);
        }
        spans
    }

    /// `5h 1% · 7d 56% used`, with `· resets 4:40pm` after a window near its limit, and the
    /// color that goes with it.
    fn windows_in_words(&self) -> (String, Option<Role>) {
        let cli = self.cli;
        let shape = self.shape;
        let Some(fuller) = cli.fuller() else {
            return (cli.note.clone().unwrap_or_default(), None);
        };
        let percent = |window: &WindowUse| (self.shown(window) * 100.0).round() as u8;
        let unit = if self.look.as_left { "left" } else { "used" };
        let mut words = if shape.main_bar_gone {
            format!("{}%", percent(&cli.windows[fuller]))
        } else {
            let windows: Vec<String> = cli
                .windows
                .iter()
                .enumerate()
                .filter(|(at, _)| *at == fuller || shape.second != Second::Gone)
                .map(|(_, window)| format!("{} {}%", window.label, percent(window)))
                .collect();
            format!("{} {unit}", windows.join(" · "))
        };
        let near = cli.windows.iter().find(|window| window.near);
        let resets = near
            .and_then(|window| window.resets.as_deref())
            .filter(|_| !shape.reset_gone);
        match resets {
            Some(at) if shape.short_words => words.push_str(&format!(" · {at}")),
            Some(at) => words.push_str(&format!(" · resets {at}")),
            None => {}
        }
        if let Some(age) = cli.windows[fuller].age.as_deref()
            && !shape.short_words
        {
            words.push_str(&format!(" · {age} ago"));
        }
        (words, near.map(|_| Role::Warn))
    }
}

/// Adds `more` after `spans`, one column on.
fn join(spans: &mut Vec<Span<'static>>, more: Vec<Span<'static>>) {
    if more.is_empty() {
        return;
    }
    if !spans.is_empty() {
        spans.push(Span::raw(" "));
    }
    spans.extend(more);
}

/// A share as one cell, `▁` to `█`: an eighth of a cell per eighth, and never empty.
fn level(kit: &Kit<'_>, share: f64) -> &'static str {
    let eighths = (share.clamp(0.0, 1.0) * 8.0).ceil() as usize;
    kit.glyphs.columns[eighths.clamp(1, 8) - 1]
}

/// `▂▃▁▅▇▃▆`: one cell per value, against the largest.
pub(super) fn spark(kit: &Kit<'_>, values: &[f64]) -> String {
    let top = values.iter().copied().fold(0.0, f64::max);
    values
        .iter()
        .map(|value| level(kit, if top > 0.0 { value / top } else { 0.0 }))
        .collect()
}
