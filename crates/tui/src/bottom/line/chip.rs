//! One CLI on the bottom line: its logo, or its dot and name, and its plan windows as bars,
//! cells or words.

use argonauts_core::look::display_names;
use argonauts_core::settings::bottom_line::LineForm;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::fit::{Fine, Second, Shape};
use super::{CliState, CliUse, WindowUse};
use crate::base::kit::logo::logo_picture;
use crate::base::kit::{Kit, bars};
use crate::base::theme::Role;

/// A window's bar.
const BAR_CELLS: usize = 6;

/// The cells a percentage takes after a bar, right-aligned, so `5%` and `100%` end in one
/// column.
const PERCENT_CELLS: usize = 4;

/// A bar on the line: the kit's window meter in whole cells, six cells wide.
pub(super) fn small_bar(kit: &Kit<'_>, share: f64, fill: Role) -> Vec<Span<'static>> {
    bars::whole_window_meter(kit, share, BAR_CELLS, fill)
}

pub(super) struct Look<'a> {
    pub kit: &'a Kit<'a>,
    pub form: LineForm,
    pub as_left: bool,
}

pub(super) fn draw(cli: &CliUse, shape: &Shape, look: &Look<'_>) -> Vec<Span<'static>> {
    Chip { cli, shape, look }.draw()
}

/// What follows a chip's head with all the room it wants: its windows' labels, bars and
/// numbers, or the words of a CLI that needs you.
pub(super) fn meters(cli: &CliUse, look: &Look<'_>) -> Vec<Span<'static>> {
    let shape = Shape::default();
    Chip {
        cli,
        shape: &shape,
        look,
    }
    .body(Fine::Bar)
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
        join(&mut spans, self.body(fine));
        spans
    }

    fn body(&self, fine: Fine) -> Vec<Span<'static>> {
        match &self.cli.state {
            CliState::Down => vec![self.kit().span("down", Role::Danger)],
            CliState::WrappingUp => vec![self.kit().span("wrapping up", Role::Warn)],
            CliState::NearLimit => vec![self.kit().span("near limit", Role::Warn)],
            // When it's back stands in for a bar, which would only say it's full.
            CliState::Out { until } => vec![
                self.kit()
                    .span(self.out_words(until.as_deref()), Role::Danger),
            ],
            CliState::Fine if fine >= Fine::Name => Vec::new(),
            CliState::Fine => match self.look.form {
                LineForm::Gauges => self.gauges(),
                LineForm::Compact => self.compact(fine),
                _ => self.bars(fine),
            },
        }
    }

    fn kit(&self) -> &Kit<'_> {
        self.look.kit
    }

    /// The CLI's logo where the terminal draws logos, which names it by itself. Elsewhere the
    /// dot in its color, filled for the one in use, and its name, or two letters where a narrow
    /// terminal took the name: never the dot alone.
    fn head(&self, fine: Fine) -> Vec<Span<'static>> {
        let cli = self.cli;
        let kit = self.kit();
        if let Some(logo) = &cli.logo {
            let picture = logo_picture(logo, kit.theme);
            if !picture.is_empty() {
                return picture;
            }
        }
        let mut spans = vec![self.dot()];
        let name = if fine < Fine::Dot && !self.shape.names_gone {
            Some(cli.name.clone())
        } else {
            self.letters()
        };
        if let Some(name) = name {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(name, self.name_style()));
        }
        spans
    }

    /// Two capitals for the CLI where its name has no room, `CX` beside `○`; `None` only past
    /// every other step, a CLI that is fine first, so its dot stands alone.
    fn letters(&self) -> Option<String> {
        let cli = self.cli;
        let fine = cli.state == CliState::Fine && !cli.main;
        if self.shape.letters_gone || (self.shape.fine_letters_gone && fine) {
            return None;
        }
        let id = cli.logo.as_ref().map_or("", |logo| logo.agent.0.as_str());
        Some(display_names::letters(id, &cli.name))
    }

    fn dot(&self) -> Span<'static> {
        let kit = self.kit();
        let dot = match (self.cli.main, kit.theme.glyphs.is_ascii()) {
            (true, false) => "●",
            (false, false) => "○",
            (true, true) => "*",
            (false, true) => "o",
        };
        Span::styled(dot, kit.fg(self.cli.color))
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

    /// [`Self::percent`] after a bar or a cell, right-aligned in four cells: ` 56%`.
    fn percent_after_bar(&self, window: &WindowUse) -> Vec<Span<'static>> {
        let percent = self.percent(window);
        let digits = percent.content.split(' ').next().unwrap_or_default();
        let pad = PERCENT_CELLS.saturating_sub(digits.chars().count());
        if pad == 0 {
            return vec![percent];
        }
        vec![Span::raw(" ".repeat(pad)), percent]
    }

    fn bar(&self, window: &WindowUse) -> Vec<Span<'static>> {
        small_bar(self.kit(), self.shown(window), self.fill(window))
    }

    /// A window's bar as one cell, drawn as the bar's own cells are: filled for any use.
    fn cell(&self, window: &WindowUse) -> Vec<Span<'static>> {
        bars::whole_window_meter(self.kit(), self.shown(window), 1, self.fill(window))
    }

    /// A gauge's cell for a window, an eighth of a cell per eighth used and never empty.
    fn gauge(&self, window: &WindowUse) -> Span<'static> {
        let kit = self.kit();
        Span::styled(level(kit, self.shown(window)), kit.fg(self.fill(window)))
    }

    /// Whether the bars have gone, leaving the numbers: every CLI's last of all, and before
    /// that those of a CLI at 90% that isn't the one in use.
    fn bar_gone(&self) -> bool {
        let near = !self.cli.main && self.cli.needs_you();
        self.shape.main_bar_gone || (near && self.shape.near_bar_gone)
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

    /// `5h ▄▄▄▄▄▁  93% resets 4:40pm  7d ▄▄▄▁▁▁  56%` for the CLI in use or one near its
    /// limit, `7d ▄▁▁▁▁▁  11%` for another. The fuller window is never given up.
    fn bars(&self, fine: Fine) -> Vec<Span<'static>> {
        let cli = self.cli;
        let kit = self.kit();
        let Some(fuller) = cli.fuller() else {
            return self.note();
        };
        if fine == Fine::Cell {
            let window = &cli.windows[fuller];
            let mut spans = self.cell(window);
            join(&mut spans, self.percent_after_bar(window));
            return spans;
        }
        let mut spans = Vec::new();
        for (at, window) in cli.windows.iter().enumerate() {
            let mut one = Vec::new();
            let second = at != fuller;
            match (second, self.shape.second) {
                (true, Second::Gone) => continue,
                (true, Second::Cell) => {
                    one.push(kit.dim(window.label.clone()));
                    join(&mut one, self.cell(window));
                    join(&mut one, self.percent_after_bar(window));
                }
                _ if self.bar_gone() => one.push(self.percent(window)),
                _ => {
                    one.push(kit.dim(window.label.clone()));
                    join(&mut one, self.bar(window));
                    join(&mut one, self.percent_after_bar(window));
                }
            }
            join(&mut one, self.after_number(window));
            self.join_window(&mut spans, one);
        }
        spans
    }

    /// Adds a window after the one before it, two columns on, as far apart as the parts on the
    /// right, and one where the chips have closed up.
    fn join_window(&self, spans: &mut Vec<Span<'static>>, window: Vec<Span<'static>>) {
        if !spans.is_empty() && !self.shape.tight {
            spans.push(Span::raw(" "));
        }
        join(spans, window);
    }

    /// One bar, the fuller window, with no label: `██████ 56%`.
    fn compact(&self, fine: Fine) -> Vec<Span<'static>> {
        let Some(window) = self.cli.fuller().map(|at| &self.cli.windows[at]) else {
            return self.note();
        };
        let mut spans = match fine {
            _ if self.bar_gone() => vec![self.percent(window)],
            Fine::Cell => self.cell(window),
            _ => self.bar(window),
        };
        if !self.bar_gone() {
            join(&mut spans, self.percent_after_bar(window));
        }
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
            windows.iter().map(|window| self.gauge(window)).collect();
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

    /// Today's line: `● Claude Code 5h 1% · 7d 56% used`, bold for the CLI in use and dim for the
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
        } else if let Some(letters) = self.letters() {
            // Where its name gave way, two letters stand beside the dot.
            text.push_str(&letters);
        }
        if !words.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&words);
        }
        let mut spans = Vec::new();
        if cli.main || !named {
            let dot = self.dot();
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
