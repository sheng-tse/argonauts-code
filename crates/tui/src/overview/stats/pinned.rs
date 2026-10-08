//! The stats mod pinned above the input: the period's totals, a bar of the crew's tokens by
//! CLI, and one row per CLI with its busiest model, its share, and the columns
//! `mods/stats.toml` names.

use argonauts_core::api::{UsagePeriod, UsageSummary};
use argonauts_core::crew_stats::CrewRole;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::settings::stats::{StatsColumn, StatsSettings};
use ratatui::style::Style;
use ratatui::text::Span;
use time::OffsetDateTime;

use super::cli::{SQUARES, Stats, count, numbers, took, window_meter};
use super::data::{Cli, Crew};
use super::draw;
use crate::format;
use crate::overview::cards::{used_of, window_label};
use crate::text::{spans_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Form, Frame, Header, Kit, fit, spread};

/// Cells for a CLI's dot and short name.
const NAME: usize = 10;
/// Cells for its busiest model.
const MODEL: usize = 18;
/// Cells of its share's bar.
const SHARE: usize = 10;
/// Cells for its squares, or `12 sent back`.
const REVIEW: usize = SQUARES + 2;

/// What the card draws from.
pub struct StatsCard<'a> {
    pub usage: &'a UsageSummary,
    pub names: &'a DisplayNames,
    pub now: OffsetDateTime,
}

impl StatsCard<'_> {
    fn settings(&self) -> StatsSettings {
        self.usage
            .stats
            .as_ref()
            .map(|stats| stats.settings.clone())
            .unwrap_or_default()
    }

    fn crew(&self) -> Option<Crew> {
        let stats = self.usage.stats.as_ref()?;
        Some(Crew::of(self.usage, stats, self.names, self.now.date()))
    }

    /// The card in its frame. Folded, it's one row.
    pub fn draw(&self, kit: &Kit<'_>, form: Form, frame: Frame) -> Drawn {
        self.draw_as(kit, form, frame, self.settings())
    }

    /// The card as `settings` would draw it, for the stats mod's page to preview.
    pub fn draw_as(
        &self,
        kit: &Kit<'_>,
        form: Form,
        frame: Frame,
        settings: StatsSettings,
    ) -> Drawn {
        if matches!(form, Form::Line | Form::Chip) {
            let mut drawn = Drawn::new();
            let width = frame.left + frame.width;
            let mut row = vec![Span::raw(" ".repeat(frame.left))];
            row.extend(self.line(kit, width.saturating_sub(frame.left)));
            drawn.push(row);
            return drawn;
        }
        let inner = frame.inner();
        let mut drawn = Drawn::new();
        let left = header_left(kit, self.usage.period);
        let Some(crew) = self.crew() else {
            drawn.push(spread(left, vec![kit.dim("reading…")], inner));
            return frame.draw(kit, drawn);
        };
        let look = Stats {
            crew: &crew,
            usage: self.usage,
            measure: settings.measure,
        };
        if settings.total {
            drawn.push(super::cli::header_row(
                kit,
                left,
                &self.totals(&crew),
                inner,
            ));
            let whole = crew.measured(settings.measure);
            let parts: Vec<(u64, Style)> = crew
                .clis
                .iter()
                .map(|cli| (cli.measured(settings.measure), kit.fg(cli.role())))
                .collect();
            drawn.push(draw::bar(kit, &parts, whole, inner));
        } else {
            drawn.push(left);
        }
        if crew.is_empty() {
            drawn.push(vec![kit.dim("Nothing recorded in this period yet.")]);
        }
        let whole = crew.measured(settings.measure).max(1);
        let rows: Vec<Row> = crew
            .clis
            .iter()
            .map(|cli| self.cli_row(kit, &look, cli, whole, &settings.columns))
            .collect();
        for line in lay_out(rows, inner) {
            drawn.push(line);
        }
        frame.draw(kit, drawn)
    }

    /// `11 tasks merged · 10 first pass · 1.4M tokens · ~$1.86 · 2h 14m`.
    fn totals(&self, crew: &Crew) -> Vec<(String, &'static str)> {
        let merged = if crew.merged == 1 {
            "task merged"
        } else {
            "tasks merged"
        };
        let mut pieces = vec![
            (crew.merged.to_string(), merged),
            (crew.fared.first.to_string(), "first pass"),
            (format::tokens(crew.total.total_tokens()), "tokens"),
            (format::spend(&crew.total), ""),
        ];
        if crew.took_secs > 0 {
            pieces.push((took(crew.took_secs), ""));
        }
        pieces
    }

    /// `● Claude  Opus 5.5  ▄▄▄▄▄▄  7 tasks ■■■■■■■      840k tok  7d ▄▄ 56%`.
    fn cli_row(
        &self,
        kit: &Kit<'_>,
        look: &Stats<'_>,
        cli: &Cli,
        whole: u64,
        columns: &[StatsColumn],
    ) -> Row {
        let dot = if cli.main { "●" } else { "○" };
        let name = if cli.main {
            kit.strong(cli.short())
        } else {
            Span::raw(cli.short())
        };
        let mut left = fit(vec![kit.span(dot, cli.role()), Span::raw(" "), name], NAME);
        let model = cli
            .models
            .first()
            .map(|model| model.name.clone())
            .unwrap_or_default();
        left.extend(fit(vec![Span::raw(model)], MODEL));
        let measured = cli.measured(look.measure);
        left.extend(draw::meter(kit, measured, whole, SHARE, kit.fg(cli.role())));
        left.push(Span::raw("  "));
        let mut right: Vec<Vec<Span<'static>>> = Vec::new();
        for column in columns {
            // Work and review read on from the name while they come first. Put after another
            // column, they take their place among the cells.
            let reads_on = right.is_empty();
            let cell = match column {
                StatsColumn::Work if reads_on => {
                    left.extend(fit(vec![Span::raw(work_words(cli))], 12));
                    continue;
                }
                StatsColumn::Review if reads_on => {
                    left.extend(review_cell(kit, cli));
                    left.push(Span::raw("  "));
                    continue;
                }
                StatsColumn::Work => vec![Span::raw(work_words(cli))],
                StatsColumn::Review => review_cell(kit, cli),
                StatsColumn::Tokens => {
                    vec![kit.strong(format::tokens(cli.tokens())), kit.dim(" tok")]
                }
                StatsColumn::Plan => plan_cell(kit, look, cli),
                StatsColumn::Spend => vec![Span::raw(format::spend(&cli.spent()))],
                StatsColumn::Pace => pace_cell(kit, cli),
                StatsColumn::Fallbacks => {
                    let taken = look
                        .crew
                        .fallbacks
                        .iter()
                        .filter(|fallback| fallback.agent == cli.agent)
                        .count();
                    if taken == 0 {
                        Vec::new()
                    } else {
                        vec![kit.dim(format!("took over {taken}"))]
                    }
                }
                // Nothing records lines per task yet.
                StatsColumn::Lines => continue,
            };
            right.push(cell);
        }
        Row { left, right }
    }

    /// The folded card: its mark and name, the period's tokens and tasks.
    pub fn line(&self, kit: &Kit<'_>, width: usize) -> Vec<Span<'static>> {
        let mut row = header_left(kit, self.usage.period);
        row.push(Span::raw("  "));
        if let Some(crew) = self.crew() {
            row.extend(numbers(kit, &self.totals(&crew)[..3], width));
        }
        truncate_spans(&row, width)
    }

    /// What a hidden band still says of it: the period's tokens.
    pub fn key_number(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        vec![kit.dim(self.key_words())]
    }

    /// `stats 1.2M tok`, which the bottom line's `stats` part shows too.
    pub fn key_words(&self) -> String {
        let tokens = self.crew().map_or(0, |crew| crew.total.total_tokens());
        format!("stats {} tok", format::tokens(tokens))
    }
}

/// A CLI's row: what reads from the left, and the cells at the right, which line up with
/// the other rows' in columns.
struct Row {
    left: Vec<Span<'static>>,
    right: Vec<Vec<Span<'static>>>,
}

/// The rows, each right cell padded at its left to its column's width, three cells between
/// columns that hold anything. When the cells don't fit, a row keeps what reads from the
/// left.
fn lay_out(rows: Vec<Row>, inner: usize) -> Vec<Vec<Span<'static>>> {
    let columns = rows.iter().map(|row| row.right.len()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|k| {
            let cells = rows.iter().filter_map(|row| row.right.get(k));
            cells.map(|cell| spans_width(cell)).max().unwrap_or(0)
        })
        .collect();
    rows.into_iter()
        .map(|row| {
            let mut right = Vec::new();
            for (k, cell) in row.right.into_iter().enumerate() {
                if widths[k] == 0 {
                    continue;
                }
                if !right.is_empty() {
                    right.push(Span::raw("   "));
                }
                right.push(Span::raw(" ".repeat(widths[k] - spans_width(&cell))));
                right.extend(cell);
            }
            if spans_width(&row.left) + spans_width(&right) + 1 > inner {
                return truncate_spans(&row.left, inner);
            }
            spread(row.left, right, inner)
        })
        .collect()
}

/// `◆ stats today`.
fn header_left(kit: &Kit<'_>, period: UsagePeriod) -> Vec<Span<'static>> {
    let mut left = Header::new("stats", Role::Card).left(kit);
    left.push(kit.dim(format!(" {}", crate::overview::period_words(period))));
    left
}

/// `7 tasks`, `11 reviews`, a judge's sign-offs counted as reviews as on the tab.
fn work_words(cli: &Cli) -> String {
    let reviews = cli.did(CrewRole::Reviewer) + cli.did(CrewRole::Judge);
    let workflows = cli.did(CrewRole::Orchestrator);
    match (cli.did(CrewRole::Worker), reviews, workflows) {
        (0, 0, 0) => String::new(),
        (0, 0, n) => count(n, "workflow"),
        (0, n, _) => count(n, "review"),
        (n, _, _) => count(n, "task"),
    }
}

/// Squares for its tasks, or for a CLI that only reviewed, how many it sent back.
fn review_cell(kit: &Kit<'_>, cli: &Cli) -> Vec<Span<'static>> {
    let fared = cli.fared();
    if fared.total() > 0 {
        return fit(draw::squares(kit, fared, SQUARES), REVIEW);
    }
    let reviews = cli.did(CrewRole::Reviewer) + cli.did(CrewRole::Judge);
    if reviews == 0 {
        return fit(Vec::new(), REVIEW);
    }
    let back = cli.sent_back(CrewRole::Reviewer) + cli.sent_back(CrewRole::Judge);
    fit(vec![kit.dim(format!("{back} sent back"))], REVIEW)
}

/// `7d ▄▄▄ 56%`: its fuller window.
fn plan_cell(kit: &Kit<'_>, look: &Stats<'_>, cli: &Cli) -> Vec<Span<'static>> {
    let Some(plan) = look.usage.plans.iter().find(|plan| plan.agent == cli.agent) else {
        return Vec::new();
    };
    let Some(reading) = plan
        .windows
        .iter()
        .max_by(|a, b| used_of(a).total_cmp(&used_of(b)))
    else {
        return Vec::new();
    };
    let mut cell = vec![kit.dim(format!("{} ", window_label(reading)))];
    cell.extend(window_meter(kit, cli, reading, 6));
    cell
}

/// `4m 12s a task`.
fn pace_cell(kit: &Kit<'_>, cli: &Cli) -> Vec<Span<'static>> {
    let reviews = cli.did(CrewRole::Reviewer) + cli.did(CrewRole::Judge);
    let (n, secs, unit) = match cli.did(CrewRole::Worker) {
        0 => (
            reviews,
            cli.took(CrewRole::Reviewer) + cli.took(CrewRole::Judge),
            "a review",
        ),
        tasks => (tasks, cli.took(CrewRole::Worker), "a task"),
    };
    if n == 0 || secs == 0 {
        return Vec::new();
    }
    vec![kit.dim(format!("{} {unit}", took(secs / u64::from(n))))]
}
