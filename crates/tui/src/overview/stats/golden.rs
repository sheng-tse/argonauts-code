//! Each Stats screen drawn from example numbers at the width of its drawing in `screens/`,
//! and compared with it cell for cell: each character, and each cell's color and weight.
//! A drawing is a grid of terminal cells, one character per cell; its `.html` gives each
//! cell's color by name. The panel draws a column in from each side of the screen, so a
//! drawing `w` cells wide is the panel on a screen `w + 2` wide.

use std::sync::Arc;
use std::time::Instant;

use argonauts_core::Spend;
use argonauts_core::api::{PlanUsage, UsagePeriod, UsageSummary};
use argonauts_core::crew_stats::{CrewRole, CrewStats, Fallback, ModelDay, TaskRecord, WorkRow};
use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::VendorId;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier};
use time::{Date, Duration, OffsetDateTime};

use super::draw::shade;
use crate::overview::{OverviewView, Tab, UsageWatch, cli_role};
use crate::state::AppState;
use crate::theme::{ColorLevel, Glyphs, Role, Theme};
use crate::views::{View, ViewCtx};
use crate::widgets::Kit;

/// One cell of a drawing: its character, the name of its color, and whether it's bold.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Cell {
    pub ch: char,
    pub color: Option<String>,
    pub bold: bool,
}

/// A drawing's rows of cells, from its `.html`.
pub(super) fn drawing(html: &str) -> Vec<Vec<Cell>> {
    let body = html
        .trim_end()
        .trim_start_matches(r#"<div class="scroll"><pre class="term grid">"#)
        .trim_end_matches("</pre></div>");
    body.split('\n').map(cells_of).collect()
}

fn cells_of(line: &str) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let (text, color, bold, after) = match rest.strip_prefix(r#"<span style=""#) {
            Some(styled) => {
                let (style, after) = styled.split_once(r#"">"#).expect("a span's style ends");
                let (text, after) = after.split_once("</span>").expect("a span ends");
                let color = style
                    .split(';')
                    .find_map(|part| part.strip_prefix("color:var(--"))
                    .map(|name| name.trim_end_matches(')').to_owned());
                (text, color, style.contains("font-weight:700"), after)
            }
            None => {
                let end = rest.find("<span").unwrap_or(rest.len());
                (&rest[..end], None, false, &rest[end..])
            }
        };
        for ch in unescape(text).chars() {
            cells.push(Cell {
                ch,
                color: color.clone(),
                bold,
            });
        }
        rest = after;
    }
    cells
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

/// The theme the drawings are drawn in: full color, Unicode glyphs.
pub(super) fn theme() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::TrueColor);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The colors a cell named `name` in a drawing may be drawn in. A drawing names fewer
/// colors than the theme has roles, so some names stand for more than one.
fn colors_for(kit: &Kit<'_>, name: Option<&str>, bold: bool) -> Vec<Option<Color>> {
    let fg = |role: Role| kit.fg(role).fg;
    let model = |agent: &str, nth: usize| shade(kit, cli_role(agent), nth).fg;
    match name {
        None | Some("term-fg") if bold => vec![None, Some(Color::Reset), fg(Role::Bright)],
        None | Some("term-fg") => vec![None, Some(Color::Reset), fg(Role::Text)],
        Some("term-acc") => vec![fg(Role::Card)],
        Some("term-dim") => vec![fg(Role::Dim)],
        Some("term-faint") => vec![
            fg(Role::Faint),
            fg(Role::TokensCached),
            fg(Role::WidgetBorder),
        ],
        Some("term-grn") => vec![fg(Role::Success)],
        Some("term-amb") => vec![fg(Role::Warning)],
        Some("term-red") => vec![fg(Role::Error)],
        Some("term-org") => vec![fg(Role::Accent), fg(Role::TokensOutput)],
        Some("term-blu") => vec![fg(Role::TokensInput)],
        Some("v-claude") => vec![fg(cli_role("claude-code"))],
        Some("v-codex") => vec![fg(cli_role("codex"))],
        Some("v-agy") => vec![fg(cli_role("antigravity-cli"))],
        Some("m-opus") => vec![model("claude-code", 0)],
        Some("m-sonnet") => vec![model("claude-code", 1)],
        Some("m-gpt") => vec![model("codex", 0)],
        Some("m-luna") => vec![model("codex", 1)],
        Some("m-pro") => vec![model("antigravity-cli", 0)],
        Some("m-flash") => vec![model("antigravity-cli", 1)],
        Some(other) => panic!("a color the drawings don't use: {other}"),
    }
}

/// The glyphs a card's frame is drawn with.
const FRAME: &str = "╭╮╰╯─│";

/// Where a drawing's rows are on the screen: drawing row `at` is screen row `at + rows`, and
/// its columns start at screen column `columns`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Offset {
    pub rows: isize,
    pub columns: isize,
}

/// Every cell of `drawing` in `region` that differs from the screen, as one line
/// each: where, what the drawing has, what was drawn.
pub(super) fn differences(
    theme: &Theme,
    drawing: &[Vec<Cell>],
    screen: &Buffer,
    at: Offset,
    region: Region,
) -> Vec<String> {
    let kit = Kit::new(theme);
    let mut out = Vec::new();
    for row in region.rows {
        let Some(cells) = drawing.get(row) else {
            out.push(format!("row {row}: the drawing has no such row"));
            continue;
        };
        let columns = region.columns.start..region.columns.end.min(cells.len());
        let y = (row as isize + at.rows) as u16;
        let drawn: String = columns
            .clone()
            .map(|col| symbol(screen, (col as isize + at.columns) as u16, y))
            .collect();
        let wanted: String = cells[columns.clone()].iter().map(|cell| cell.ch).collect();
        if drawn != wanted {
            out.push(format!(
                "row {row}:\n  drawing |{wanted}|\n  drawn   |{drawn}|"
            ));
            continue;
        }
        for (col, cell) in cells
            .iter()
            .enumerate()
            .filter(|(col, _)| columns.contains(col))
        {
            // A frame's lines are the shared frame's to color; a blank has none.
            if cell.ch == ' ' || FRAME.contains(cell.ch) {
                continue;
            }
            let x = (col as isize + at.columns) as u16;
            let drawn = &screen[(x, y)];
            let colors = colors_for(&kit, cell.color.as_deref(), cell.bold);
            let color_matches = colors.contains(&Some(drawn.fg))
                || (drawn.fg == Color::Reset && colors.contains(&None));
            if !color_matches {
                out.push(format!(
                    "row {row} col {col} {:?}: drawing {:?}, drawn {:?}",
                    cell.ch, cell.color, drawn.fg
                ));
            }
            let bold = drawn.modifier.contains(Modifier::BOLD);
            if bold != cell.bold {
                out.push(format!(
                    "row {row} col {col} {:?}: drawing {} bold, drawn {} bold",
                    cell.ch,
                    if cell.bold { "is" } else { "isn't" },
                    if bold { "is" } else { "isn't" },
                ));
            }
        }
    }
    out
}

fn symbol(screen: &Buffer, x: u16, y: u16) -> String {
    let area = screen.area;
    if x >= area.right() || y >= area.bottom() {
        return "∅".to_owned();
    }
    let symbol = screen[(x, y)].symbol();
    if symbol.is_empty() {
        " ".to_owned()
    } else {
        symbol.to_owned()
    }
}

/// The Stats tab with `usage`, after `keys`, drawn on a screen `width` wide and as tall as
/// the panel wants.
pub(super) fn screen(
    theme: &Theme,
    usage: UsageSummary,
    now: OffsetDateTime,
    keys: &[KeyCode],
    width: u16,
) -> Buffer {
    let mut state = AppState::default();
    let period = usage.period;
    state.usage = Some(Arc::new(usage));
    state.names = names();
    let mut view = OverviewView::new(Tab::Stats, UsageWatch::default())
        .at(now)
        .with_period(period);
    let ctx = ViewCtx {
        theme,
        state: &state,
        now: Instant::now(),
    };
    for key in keys {
        view.handle_key(KeyEvent::from(*key), &ctx);
    }
    let screen = Size::new(width, 120);
    let height = view.dock_height(screen, &ctx).unwrap_or(100);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

/// `lines` drawn on a screen `width` wide.
pub(super) fn lines_screen(lines: Vec<Vec<ratatui::text::Span<'static>>>, width: u16) -> Buffer {
    let area = Rect::new(0, 0, width, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    for (y, line) in lines.into_iter().enumerate() {
        buf.set_line(0, y as u16, &ratatui::text::Line::from(line), width);
    }
    buf
}

/// The screen's rows as text, for a failure's message.
pub(super) fn text(screen: &Buffer) -> String {
    let area = screen.area;
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .map(|x| symbol(screen, x, y))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rows of a drawing, and the columns of them a test compares.
#[derive(Debug, Clone)]
pub(super) struct Region {
    pub rows: std::ops::Range<usize>,
    pub columns: std::ops::Range<usize>,
}

/// The whole of rows `rows`.
pub(super) fn rows(rows: std::ops::Range<usize>) -> Region {
    Region {
        rows,
        columns: 0..usize::MAX,
    }
}

/// Columns `columns` of row `row`.
pub(super) fn cells(row: usize, columns: std::ops::Range<usize>) -> Region {
    Region {
        rows: row..row + 1,
        columns,
    }
}

/// Asserts the drawing's `region` matches the screen.
pub(super) fn assert_matches(
    name: &str,
    html: &str,
    theme: &Theme,
    screen: &Buffer,
    at: Offset,
    region: Region,
) {
    let drawing = drawing(html);
    let differences = differences(theme, &drawing, screen, at, region);
    assert!(
        differences.is_empty(),
        "{name} differs from its drawing:\n{}\n\nthe screen:\n{}",
        differences.join("\n"),
        text(screen)
    );
}

/// Asserts the drawing's `region` differs from the screen in `known` alone, each a line
/// as [`differences`] writes it, a cell's color without what was drawn.
pub(super) fn assert_differs_only(
    name: &str,
    html: &str,
    theme: &Theme,
    screen: &Buffer,
    at: Offset,
    region: Region,
    known: &[&str],
) {
    let drawing = drawing(html);
    let differences = differences(theme, &drawing, screen, at, region);
    // A cell's color is known by its place and the drawing's color alone, so a theme that
    // draws a role another way still finds it.
    let place = |line: &String| match line.split_once(", drawn ") {
        Some((place, _)) if line.contains(" col ") => place.to_owned(),
        _ => line.clone(),
    };
    let found: Vec<String> = differences.iter().map(place).collect();
    assert_eq!(
        found,
        known,
        "{name} differs from its drawing in more than is known:\n{}\n\nthe screen:\n{}",
        differences.join("\n"),
        text(screen)
    );
}

/// What `screen` drew for the drawing `name` from `at`, written beside it when asked.
pub(super) fn dump(name: &str, screen: &Buffer, at: Offset, html: &str) {
    let drawing = drawing(html);
    let width = drawing.iter().map(Vec::len).max().unwrap_or(0);
    let at = (at.columns.max(0) as usize, at.rows.max(0) as usize);
    let cut = crate::picture_dump::cut(screen, at, (width, drawing.len()));
    crate::picture_dump::rows(name, &cut);
}

/// The first screen row holding `words`.
pub(super) fn row_of(screen: &Buffer, words: &str) -> usize {
    text(screen)
        .lines()
        .position(|row| row.contains(words))
        .unwrap_or_else(|| panic!("{words}:\n{}", text(screen)))
}

pub(super) fn names() -> argonauts_core::display_names::DisplayNames {
    let mut names = argonauts_core::display_names::DisplayNames::default();
    for (id, name) in [
        ("claude-code", "Claude Code"),
        ("codex", "Codex"),
        ("antigravity-cli", "Antigravity CLI"),
    ] {
        names.harnesses.insert(id.to_owned(), name.to_owned());
    }
    names
}

/// The drawings' CLIs and models.
pub(super) const CLAUDE: &str = "claude-code";
pub(super) const CODEX: &str = "codex";
pub(super) const AGY: &str = "antigravity-cli";
pub(super) const OPUS: (&str, &str) = (CLAUDE, "claude-opus-5-5");
pub(super) const SONNET: (&str, &str) = (CLAUDE, "claude-sonnet-5-5");
pub(super) const HAIKU: (&str, &str) = (CLAUDE, "claude-haiku-5-5");
pub(super) const GPT: (&str, &str) = (CODEX, "gpt-5.6");
pub(super) const LUNA: (&str, &str) = (CODEX, "gpt-5.6-luna");
pub(super) const PRO: (&str, &str) = (AGY, "gemini-3.6-pro");
pub(super) const FLASH: (&str, &str) = (AGY, "gemini-3.6-flash");

/// Tokens from cache, new input and output, and what they cost.
pub(super) fn spent(cached: u64, new: u64, output: u64, cost: f64, estimated: bool) -> Spend {
    Spend {
        input_tokens: cached + new,
        cached_tokens: cached,
        output_tokens: output,
        cost_usd: cost,
        cost_estimated: estimated,
        cost_partial: false,
    }
}

/// Tokens in all, as a drawing writes them, split two to one between cache and the rest.
pub(super) fn tokens(total: u64, cost: f64, estimated: bool) -> Spend {
    let cached = total / 3 * 2;
    let output = total / 20;
    spent(cached, total - cached - output, output, cost, estimated)
}

/// The example's numbers as the stats a period carries.
#[derive(Default)]
pub(super) struct Example {
    pub stats: CrewStats,
    pub plans: Vec<PlanUsage>,
}

impl Example {
    /// What `model` did as `role`: `done` pieces of work taking `each` seconds apiece, `back`
    /// of them sending work back.
    pub fn work(
        &mut self,
        model: (&str, &str),
        role: CrewRole,
        kind: Option<&str>,
        spent: Spend,
        (done, back, each): (u32, u32, u64),
    ) -> &mut WorkRow {
        self.stats.work.push(WorkRow {
            agent: model.0.to_owned(),
            model: Some(model.1.to_owned()),
            role,
            kind: kind.map(str::to_owned),
            spent,
            done,
            sent_back: back,
            took_secs: each * u64::from(done),
            review: Default::default(),
        });
        self.stats.work.last_mut().expect("just pushed")
    }

    /// Tasks `model` built of `kind`: `first` passed review first time, `once` were sent back
    /// once, `twice` twice.
    pub fn tasks(&mut self, model: (&str, &str), kind: &str, first: u32, once: u32, twice: u32) {
        for (n, back) in [(first, 0), (once, 1), (twice, 2)] {
            for _ in 0..n {
                self.stats.tasks.push(TaskRecord {
                    agent: model.0.to_owned(),
                    model: Some(model.1.to_owned()),
                    kind: Some(kind.to_owned()),
                    sent_back: back,
                    merged: true,
                    reviewed: true,
                });
            }
        }
    }

    /// What `model` did in `role` on `day`.
    pub fn day(&mut self, model: (&str, &str), role: CrewRole, day: Date, tokens: u64, done: u32) {
        self.stats.days.push(ModelDay {
            day,
            agent: model.0.to_owned(),
            model: Some(model.1.to_owned()),
            role: Some(role),
            tokens,
            cost_usd: 0.0,
            done,
        });
    }

    /// `n` tasks `model` took over from `from` when its 5-hour window ran out.
    pub fn fallbacks(&mut self, model: (&str, &str), from: (&str, &str), n: usize) {
        for _ in 0..n {
            self.stats.fallbacks.push(Fallback {
                agent: model.0.to_owned(),
                model: Some(model.1.to_owned()),
                from_agent: from.0.to_owned(),
                from_model: Some(from.1.to_owned()),
                why: String::new(),
                window: Some("five_hour".to_owned()),
            });
        }
    }

    /// The CLI's plan and its 5-hour and weekly windows, as shares used.
    pub fn plan(
        &mut self,
        agent: &str,
        plan: &str,
        main: bool,
        windows: [f64; 2],
        now: OffsetDateTime,
    ) {
        let reading = |window: &str, used: f64| QuotaReading {
            vendor: VendorId(agent.to_owned()),
            status: QuotaStatus::Allowed,
            used: Some(used),
            resets_at: Some(now + Duration::hours(3)),
            window: Some(window.to_owned()),
            source: QuotaSource::AppServer,
            observed_at: now,
            detail: None,
            wrap_up: false,
        };
        self.plans.push(PlanUsage {
            agent: agent.to_owned(),
            name: names().harness(agent),
            plan: Some(plan.to_owned()),
            in_use: main,
            windows: vec![
                reading("five_hour", windows[0]),
                reading("seven_day", windows[1]),
            ],
            ..PlanUsage::default()
        });
    }

    pub fn usage(self, period: UsagePeriod) -> UsageSummary {
        UsageSummary {
            period,
            plans: self.plans,
            stats: Some(self.stats),
            ..UsageSummary::default()
        }
    }
}
