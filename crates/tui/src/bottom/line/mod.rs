//! The line under the input, drawn as a mod.
//!
//! Each CLI in the crew gets a chip in its own color: a small bar per plan window, the CLI
//! the main session is on with a filled dot, bold and both windows, the others with a hollow
//! dot and their fuller window. A window turns amber from 90% with its reset time, and a CLI
//! that is out is red. The line reads the terminal's width and lays itself out again on every
//! draw, giving things up in the fixed order of [`fit`], so no CLI ever leaves it.

mod chip;
mod fit;

use argonauts_core::settings::bottom_line::LineForm;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::cell::Tone;
use crate::render::RenderCtx;
use crate::text::{sanitize_line, spans_width, truncate_line, truncate_spans};
use crate::theme::Role;
use crate::widgets::Kit;

pub(super) use fit::Give;
use fit::Shape;

/// Columns kept free between the left side and what sits against the right edge. One is
/// enough, since the right side opens with a dot or a word in another color, and it is the
/// column that lets a 100-column terminal keep `auto mode on`.
const SIDE_GAP: usize = 1;

/// Columns between two things on the right: its parts, the chips, and a notice.
const PART_GAP: usize = 2;

/// What the bottom line shows and how, as the bridge reads it from the settings and the state.
#[derive(Debug, Clone, PartialEq)]
pub struct BottomLine {
    pub form: LineForm,
    /// From the left edge on, joined by ` · `.
    pub left: Vec<LinePart>,
    /// Against the right edge, two columns apart.
    pub right: Vec<LinePart>,
    /// The added parts that go first on a narrow terminal, first to go first. The rest of
    /// what was added follows, from the last on the line back.
    pub drop_first: Vec<String>,
    /// Numbers say what is left of a plan window rather than what is used.
    pub as_left: bool,
}

impl Default for BottomLine {
    /// The mode alone, as before anything is known about the crew.
    fn default() -> Self {
        Self {
            form: LineForm::Fit,
            left: vec![LinePart::Mode],
            right: Vec::new(),
            drop_first: Vec::new(),
            as_left: false,
        }
    }
}

impl BottomLine {
    /// Every CLI the line has a chip for.
    pub fn clis(&self) -> impl Iterator<Item = &CliUse> {
        self.left
            .iter()
            .chain(&self.right)
            .filter_map(|part| match part {
                LinePart::PlanUsage(clis) => Some(clis),
                _ => None,
            })
            .flatten()
    }

    /// The names of the parts you added, in the order they leave a narrow terminal.
    fn added_in_drop_order(&self) -> Vec<&str> {
        let added: Vec<&str> = self
            .left
            .iter()
            .chain(&self.right)
            .filter_map(|part| match part {
                LinePart::Added(part) => Some(part.name.as_str()),
                _ => None,
            })
            .collect();
        let mut order: Vec<&str> = self
            .drop_first
            .iter()
            .map(String::as_str)
            .filter(|name| added.contains(name))
            .collect();
        for name in added.iter().rev() {
            if !order.contains(name) {
                order.push(name);
            }
        }
        order
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinePart {
    /// The approval mode with its hints. It can be moved, never left out.
    Mode,
    /// One chip per CLI, in the crew's order.
    PlanUsage(Vec<CliUse>),
    /// A part you added, shown whole or not at all.
    Added(AddedPart),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddedPart {
    /// The name `left`, `right` and `[narrow] drop` know it by.
    pub name: String,
    pub body: PartBody,
}

impl AddedPart {
    pub fn words(name: &str, text: impl Into<String>, tone: Tone) -> Self {
        Self {
            name: name.to_owned(),
            body: PartBody::Words {
                text: text.into(),
                tone,
            },
        }
    }
}

/// How an added part draws.
#[derive(Debug, Clone, PartialEq)]
pub enum PartBody {
    /// `main`, or `$0.21/$5.00`.
    Words { text: String, tone: Tone },
    /// `context ▄▖     21%`, or a running workflow's `add-due-dates ▄▄▄▖   3/5`: a name in
    /// `label_role`, a small bar and its number.
    Meter {
        label: String,
        label_role: Role,
        ratio: f64,
        value: String,
        fill: Role,
    },
    /// `~$1.86 ▂▃▁▅▇▃▆`: a number and its last few days.
    Spark {
        text: String,
        values: Vec<f64>,
        role: Role,
    },
    /// `⧉ weekly-usage-chart · 1 feedback draft`, as Claude Code names a published artifact
    /// beside the mode. With `others` published before it, it is their count: `⧉ 3`.
    Artifact {
        name: String,
        others: u32,
        feedback_drafts: u32,
    },
    /// What other code drew for the part, such as a mod's own chip.
    Drawn(Vec<Span<'static>>),
    /// A mod of yours from `mods/` that the line lists, drawn as its chip with the theme.
    Mod {
        file: Box<argonauts_core::settings::mods::ModFile>,
        reading: Box<argonauts_core::settings::mods::ModReading>,
    },
}

/// The longest a name shows on the line, where Claude Code cuts an artifact's.
const NAME_CELLS: usize = 24;

/// One CLI's plan usage.
#[derive(Debug, Clone, PartialEq)]
pub struct CliUse {
    /// The short name its chip shows, such as `Claude`.
    pub name: String,
    /// Its own color, the same in every mod.
    pub color: Role,
    /// The main session runs on it.
    pub main: bool,
    pub state: CliState,
    /// The windows to show, the shorter first: both for the CLI in use, one for the others.
    pub windows: Vec<WindowUse>,
    /// What stands in for a window on a CLI without a plan, such as `$0.06` spent.
    pub note: Option<String>,
}

impl CliUse {
    pub fn new(name: impl Into<String>, color: Role) -> Self {
        Self {
            name: name.into(),
            color,
            main: false,
            state: CliState::Fine,
            windows: Vec::new(),
            note: None,
        }
    }

    /// Something about it needs you: it is out, down, or a window is at 90%. Such a CLI
    /// keeps its words however narrow the terminal gets.
    pub fn needs_you(&self) -> bool {
        self.state != CliState::Fine || self.windows.iter().any(|window| window.near)
    }

    /// The window nearest its limit, which the line never gives up for the CLI in use.
    fn fuller(&self) -> Option<usize> {
        (0..self.windows.len()).max_by(|&a, &b| {
            let used = |i: usize| self.windows[i].used;
            // Of two equally full windows the shorter one binds sooner.
            used(a).total_cmp(&used(b)).then(b.cmp(&a))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CliState {
    #[default]
    Fine,
    /// Its plan ran out. `until` is when it resets, such as `Tue 3:45pm`.
    Out { until: Option<String> },
    /// Out for new work, while the running turn finishes on the CLI's allowance.
    WrappingUp,
    /// The CLI warned that a limit is near, without saying how much is used.
    NearLimit,
    /// The CLI can't be reached.
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowUse {
    /// `5h` or `7d`.
    pub label: String,
    /// The share used, 0 to 1.
    pub used: f64,
    /// At 90% or more: amber, and never given up.
    pub near: bool,
    /// When it resets, such as `4:40pm`, where the settings show it.
    pub resets: Option<String>,
    /// How old the reading is, such as `2d`, when it couldn't be read again.
    pub age: Option<String>,
}

impl WindowUse {
    pub fn new(label: impl Into<String>, used: f64) -> Self {
        let used = used.clamp(0.0, 1.0);
        Self {
            label: label.into(),
            used,
            near: used >= NEAR_LIMIT,
            resets: None,
            age: None,
        }
    }
}

/// A window turns amber here, shows its reset time, and stays on the line at any width.
pub const NEAR_LIMIT: f64 = 0.9;

/// The mode's own part, in the order its pieces draw.
#[derive(Debug, Clone, Default)]
pub(super) struct ModePieces(pub Vec<ModePiece>);

#[derive(Debug, Clone)]
pub(super) enum ModePiece {
    /// The approval mode, which shortens to its word and then to its sign.
    Mode {
        sign: &'static str,
        /// `auto`, the mode in one word.
        word: &'static str,
        /// `auto mode on`.
        banner: String,
        style: Style,
    },
    /// A hint or a label beside it. `gives` says when a narrow terminal drops it; without
    /// one it stays, in its `short` form once the line runs out of room.
    Words {
        joiner: Joiner,
        span: Span<'static>,
        gives: Option<Give>,
        short: Option<Span<'static>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Joiner {
    Space,
    Dot,
}

/// The whole line in `ctx.width` columns: the fullest layout that fits, and past the last
/// thing it can give up, cut at the right.
pub(super) fn lay_out(
    line: &BottomLine,
    mode: &ModePieces,
    notice: Option<Span<'static>>,
    ctx: &RenderCtx<'_>,
) -> Line<'static> {
    let width = usize::from(ctx.width);
    let (mut spans, right) = fitted(line, mode, notice, ctx);
    // Past the last thing it can give up, the left side is cut, never a CLI on the right.
    if !right.is_empty() && spans_width(&spans) + SIDE_GAP + spans_width(&right) > width {
        spans = truncate_spans(&spans, width.saturating_sub(spans_width(&right) + SIDE_GAP));
    }
    if !right.is_empty() {
        let used = spans_width(&spans) + spans_width(&right);
        spans.push(Span::raw(
            " ".repeat(width.saturating_sub(used).max(SIDE_GAP)),
        ));
        spans.extend(right);
    }
    truncate_line(&Line::from(spans), width)
}

/// Both sides in the fullest layout `ctx.width` has room for: the first shape along the order
/// that fits, or the barest one where none does.
fn fitted(
    line: &BottomLine,
    mode: &ModePieces,
    notice: Option<Span<'static>>,
    ctx: &RenderCtx<'_>,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let width = usize::from(ctx.width);
    let drawer = Drawer {
        line,
        mode,
        kit: Kit::new(ctx.theme),
    };
    let order = fit::order(line);
    // A notice is cut to what the barest layout leaves, never dropped, since an error there
    // may be the only word on why Enter did nothing.
    let barest = Shape::after(&order, order.len());
    let (left, right) = drawer.sides(&barest);
    let rest = spans_width(&left) + spans_width(&right);
    let joined = if right.is_empty() { 0 } else { PART_GAP };
    let notice = notice
        .map(|notice| truncate_spans(&[notice], width.saturating_sub(rest + joined + SIDE_GAP)))
        .filter(|notice| spans_width(notice) > 0);

    let fits = |shape: &Shape| {
        let (left, right) = drawer.sides(shape);
        let right = with_notice(right, notice.as_deref());
        let gap = if right.is_empty() { 0 } else { SIDE_GAP };
        (spans_width(&left) + gap + spans_width(&right) <= width).then_some((left, right))
    };
    (0..=order.len())
        .find_map(|given| fits(&Shape::after(&order, given)))
        .unwrap_or_else(|| {
            let (left, right) = drawer.sides(&barest);
            (left, with_notice(right, notice.as_deref()))
        })
}

fn with_notice(
    mut right: Vec<Span<'static>>,
    notice: Option<&[Span<'static>]>,
) -> Vec<Span<'static>> {
    if let Some(notice) = notice {
        if !right.is_empty() {
            right.push(Span::raw(" ".repeat(PART_GAP)));
        }
        right.extend(notice.iter().cloned());
    }
    right
}

struct Drawer<'a> {
    line: &'a BottomLine,
    mode: &'a ModePieces,
    kit: Kit<'a>,
}

impl Drawer<'_> {
    /// Both sides as `shape` leaves them, the left one after the row's two-column indent.
    fn sides(&self, shape: &Shape) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let dim = self.kit.style(Role::Dim);
        let mut left = vec![Span::raw("  ")];
        let mut first = true;
        for part in &self.line.left {
            let spans = self.part(part, shape);
            if spans.is_empty() {
                continue;
            }
            if !first {
                left.push(Span::styled(" · ", dim));
            }
            first = false;
            left.extend(spans);
        }
        let mut right = Vec::new();
        for part in &self.line.right {
            let spans = self.part(part, shape);
            if spans.is_empty() {
                continue;
            }
            if !right.is_empty() {
                right.push(Span::raw(chip_gap(shape)));
            }
            right.extend(spans);
        }
        (left, right)
    }

    fn part(&self, part: &LinePart, shape: &Shape) -> Vec<Span<'static>> {
        match part {
            LinePart::Mode => self.mode_part(shape),
            LinePart::PlanUsage(clis) => self.chips(clis, shape),
            LinePart::Added(part) if shape.dropped(&part.name) => Vec::new(),
            LinePart::Added(part) => self.added(&part.body),
        }
    }

    fn mode_part(&self, shape: &Shape) -> Vec<Span<'static>> {
        let dim = self.kit.style(Role::Dim);
        let mut spans: Vec<Span<'static>> = Vec::new();
        for piece in &self.mode.0 {
            match piece {
                ModePiece::Mode {
                    sign,
                    word,
                    banner,
                    style,
                } => {
                    if !spans.is_empty() {
                        spans.push(Span::raw(" "));
                    }
                    let text = match shape.mode {
                        fit::ModeForm::Full => format!("{sign} {banner}"),
                        fit::ModeForm::Word => format!("{sign} {word}"),
                        fit::ModeForm::Sign => (*sign).to_owned(),
                    };
                    spans.push(Span::styled(text, *style));
                }
                ModePiece::Words { gives, .. } if gives.is_some_and(|give| shape.gave(give)) => {}
                ModePiece::Words {
                    joiner,
                    span,
                    short,
                    ..
                } => {
                    if !spans.is_empty() {
                        spans.push(match joiner {
                            Joiner::Space => Span::raw(" "),
                            Joiner::Dot => Span::styled(" · ", dim),
                        });
                    }
                    let shown = match short {
                        Some(short) if shape.gave(Give::ShortPieces) => short,
                        _ => span,
                    };
                    spans.push(shown.clone());
                }
            }
        }
        spans
    }

    fn chips(&self, clis: &[CliUse], shape: &Shape) -> Vec<Span<'static>> {
        let look = chip::Look {
            kit: &self.kit,
            form: self.line.form,
            as_left: self.line.as_left,
        };
        let mut spans = Vec::new();
        for cli in clis {
            if !spans.is_empty() {
                spans.push(Span::raw(chip_gap(shape)));
            }
            spans.extend(chip::draw(cli, shape, &look));
        }
        spans
    }

    fn added(&self, body: &PartBody) -> Vec<Span<'static>> {
        let kit = &self.kit;
        match body {
            PartBody::Words { text, tone } => {
                let text = sanitize_line(text).trim().to_owned();
                if text.is_empty() {
                    return Vec::new();
                }
                vec![Span::styled(text, kit.style(tone.role()))]
            }
            PartBody::Meter {
                label,
                label_role,
                ratio,
                value,
                fill,
            } => {
                let label =
                    Span::styled(sanitize_line(label).trim().to_owned(), kit.fg(*label_role));
                let mut spans = truncate_spans(&[label], NAME_CELLS);
                spans.push(Span::raw(" "));
                spans.extend(chip::small_bar(kit, *ratio, *fill));
                spans.push(kit.dim(format!(" {}", sanitize_line(value).trim())));
                spans
            }
            PartBody::Spark { text, values, role } => {
                let mut spans = vec![kit.dim(sanitize_line(text).trim().to_owned())];
                if values.iter().any(|value| *value > 0.0) {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(chip::spark(kit, values), kit.fg(*role)));
                }
                spans
            }
            PartBody::Artifact {
                name,
                others,
                feedback_drafts,
            } => self.artifact(name, *others, *feedback_drafts),
            PartBody::Drawn(spans) => spans.clone(),
            PartBody::Mod { file, reading } => crate::mods::draw::chip(&self.kit, file, reading),
        }
    }

    /// Claude Code's own words for it: one artifact by name, several by their count, and
    /// both spelled out where its mark can't be drawn.
    fn artifact(&self, name: &str, others: u32, feedback_drafts: u32) -> Vec<Span<'static>> {
        let kit = &self.kit;
        let name = sanitize_line(name).trim().to_owned();
        if name.is_empty() {
            return Vec::new();
        }
        let ascii = kit.theme.glyphs.is_ascii();
        let mut spans = if others > 0 {
            let count = others + 1;
            vec![kit.dim(if ascii {
                format!("{count} artifacts")
            } else {
                format!("⧉ {count}")
            })]
        } else {
            let mut spans = vec![kit.dim(if ascii { "artifact " } else { "⧉ " })];
            spans.extend(truncate_spans(&[kit.dim(name)], NAME_CELLS));
            spans
        };
        if feedback_drafts > 0 {
            let drafts = if feedback_drafts == 1 {
                " feedback draft"
            } else {
                " feedback drafts"
            };
            spans.push(kit.dim(" · "));
            spans.push(kit.span(feedback_drafts.to_string(), Role::Text));
            spans.push(kit.dim(drafts));
        }
        spans
    }
}

/// Chips sit two columns apart, as the parts on the right do, and one on a terminal too narrow
/// for anything else to give.
fn chip_gap(shape: &Shape) -> &'static str {
    if shape.tight { " " } else { "  " }
}

#[cfg(test)]
mod tests;
