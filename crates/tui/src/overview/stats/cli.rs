//! The view Stats opens in, by CLI: every CLI together, one card for each CLI with its
//! models as bars, and who did what in each role.

use argonauts_core::api::UsageSummary;
use argonauts_core::crew_stats::CrewRole;
use argonauts_core::limits::{QuotaReading, QuotaStatus};
use argonauts_core::settings::stats::StatsMeasure;
use ratatui::style::Style;
use ratatui::text::Span;

use super::data::{Cli, Crew, Model};
use super::draw::{self, Entry, Fared};
use crate::format;
use crate::overview::cards::{used_of, window_label};
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Frame, Header, Kit, Level, fit, spread};

/// The days chart's width: seven columns two cells wide, a cell apart.
const CHART: usize = 20;
/// Cells between the chart and the bars beside it.
const CHART_GAP: usize = 3;
/// From this inner width the all-together card draws its days chart.
const CHART_FROM: usize = 64;
/// The narrowest a CLI's card is drawn before the cards take a row each.
const LEAST_CLI_CARD: usize = 34;
/// The who-did-what card's column of role names.
const ROLE_COLUMN: usize = 13;
/// Squares before they stand for more than one task each.
pub(super) const SQUARES: usize = 10;

/// What the cards draw from.
pub(super) struct Stats<'a> {
    pub crew: &'a Crew,
    pub usage: &'a UsageSummary,
    pub measure: StatsMeasure,
}

/// Tokens in millions with two decimals, the second dropped when it's a zero: `5.0M`,
/// `1.55M`, `0.8M`. Under ten thousand a count would read as nothing, so it keeps its own
/// unit.
pub(super) fn millions(count: u64) -> String {
    if count < 10_000 {
        return format::tokens(count);
    }
    let text = format!("{:.2}", count as f64 / 1_000_000.0);
    let text = text.strip_suffix('0').unwrap_or(&text);
    format!("{text}M")
}

/// `1 task`, `24 tasks`, `14 sign-offs`.
pub(super) fn count(n: u32, unit: &str) -> String {
    match n {
        1 => format!("1 {unit}"),
        n => format!("{n} {unit}s"),
    }
}

/// A share, as `58%`.
pub(super) fn share(part: u64, whole: u64) -> String {
    format::percent(part as f64 / whole.max(1) as f64)
}

/// `4m 12s`, `3m 05s`, `41s`, `12h 40m`: the seconds take two digits beside minutes, so a
/// column of them lines up.
pub(super) fn took(secs: u64) -> String {
    match secs {
        3600.. => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        60.. => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{secs}s"),
    }
}

/// Bold numbers and dim words, as a header's right side writes them, giving up pieces from
/// the end until they fit `room`.
pub(super) fn numbers(kit: &Kit<'_>, pieces: &[(String, &str)], room: usize) -> Vec<Span<'static>> {
    for keep in (1..=pieces.len()).rev() {
        let mut spans = Vec::new();
        for (k, (number, words)) in pieces[..keep].iter().enumerate() {
            if k > 0 {
                spans.push(kit.dim(" · "));
            }
            spans.push(kit.strong(number.clone()));
            if !words.is_empty() {
                spans.push(kit.dim(format!(" {words}")));
            }
        }
        if spans_width(&spans) <= room {
            return spans;
        }
    }
    Vec::new()
}

/// A card's first row: its mark and title at the left, the numbers at the right.
pub(super) fn header_row(
    kit: &Kit<'_>,
    left: Vec<Span<'static>>,
    pieces: &[(String, &str)],
    width: usize,
) -> Vec<Span<'static>> {
    let room = width.saturating_sub(spans_width(&left) + 2);
    spread(left, numbers(kit, pieces, room), width)
}

/// The model's color: its shade of its CLI's.
pub(super) fn model_style(kit: &Kit<'_>, cli: &Cli, model: &Model) -> Style {
    draw::shade(kit, cli.role(), model.shade)
}

impl Stats<'_> {
    /// Tokens as the tab writes them. From a crew total of a million on, every count is in
    /// millions, so the numbers side by side read on one scale: `5.0M`, `1.55M`, `0.55M`.
    pub fn tokens(&self, count: u64) -> String {
        if self.crew.total.total_tokens() < 1_000_000 {
            return format::tokens(count);
        }
        millions(count)
    }

    /// A measured amount as the bars' legends write it: `2.9M`, `24`, `$7.90`.
    pub fn amount(&self, value: u64) -> String {
        match self.measure {
            StatsMeasure::Tokens => self.tokens(value),
            StatsMeasure::Tasks => value.to_string(),
            StatsMeasure::Spend => format::usd(value as f64 / 100.0),
        }
    }

    /// Every CLI together: the days chart, then what the bars measure by CLI, what the
    /// tokens were, and how the tasks fared in review.
    pub fn all_together(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        let inner = frame.inner();
        let crew = self.crew;
        let mut drawn = Drawn::new();
        let left = vec![
            kit.span(kit.glyphs.widget, Role::Card),
            Span::raw(" "),
            kit.strong("all together"),
        ];
        drawn.push(header_row(kit, left, &self.totals(), inner));

        let chart = inner >= CHART_FROM;
        let right = if chart {
            inner - CHART - CHART_GAP
        } else {
            inner
        };
        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        let measured = crew.measured(self.measure);
        rows.push(vec![kit.dim(format!(
            "{} by CLI · {}",
            self.measure.word(),
            self.amount(measured)
        ))]);
        let parts: Vec<(u64, Style)> = crew
            .clis
            .iter()
            .map(|cli| (cli.measured(self.measure), kit.fg(cli.role())))
            .collect();
        rows.push(draw::bar(kit, &parts, measured, right));
        let entries: Vec<Entry> = crew
            .clis
            .iter()
            .map(|cli| {
                let value = cli.measured(self.measure);
                Entry {
                    style: kit.fg(cli.role()),
                    name: cli.name.clone(),
                    amount: self.amount(value),
                    note: share(value, measured),
                }
            })
            .collect();
        rows.extend(draw::legend(kit, &entries, None, right));

        let total = &crew.total;
        let tokens = total.total_tokens();
        let cached = total.cached_tokens;
        let new = total.input_tokens.saturating_sub(cached);
        let output = total.output_tokens;
        rows.push(vec![kit.dim("what those tokens were")]);
        let kinds = [
            (cached, Role::TokensCached, "from cache"),
            (new, Role::TokensInput, "new input"),
            (output, Role::TokensOutput, "output"),
        ];
        let parts: Vec<(u64, Style)> = kinds
            .iter()
            .map(|(n, role, _)| (*n, kit.fg(*role)))
            .collect();
        rows.push(draw::bar(kit, &parts, tokens, right));
        let entries: Vec<Entry> = kinds
            .iter()
            .map(|(n, role, name)| Entry {
                style: kit.fg(*role),
                name: (*name).to_owned(),
                amount: self.tokens(*n),
                note: share(*n, tokens),
            })
            .collect();
        rows.extend(draw::legend(kit, &entries, None, right));

        let fared = crew.fared;
        if fared.total() > 0 {
            rows.push(vec![kit.dim(format!(
                "how the {} fared in review",
                count(fared.total(), "task")
            ))]);
            rows.push(fared_bar(kit, fared, right));
            let entries = [
                (fared.first, Role::Success, "passed first time"),
                (fared.once, Role::Warning, "sent back once"),
                (fared.more, Role::Error, "sent back twice"),
            ]
            .into_iter()
            .filter(|(n, _, _)| *n > 0)
            .map(|(n, role, name)| Entry {
                style: kit.fg(role),
                name: name.to_owned(),
                amount: n.to_string(),
                note: String::new(),
            })
            .collect::<Vec<_>>();
            rows.extend(draw::legend(kit, &entries, None, right));
        } else {
            rows.push(vec![kit.dim("no task reviewed yet")]);
        }

        if chart {
            let height = rows.len().saturating_sub(1).clamp(2, 6);
            let mut columns = self.day_columns(kit, height);
            // The chart sits at the bottom, its letters on the last row.
            let pad = rows.len().saturating_sub(columns.len());
            let mut left: Vec<Vec<Span<'static>>> = vec![Vec::new(); pad];
            left.append(&mut columns);
            for (at, row) in rows.into_iter().enumerate() {
                let mut line = fit(left.get(at).cloned().unwrap_or_default(), CHART + CHART_GAP);
                line.extend(row);
                drawn.push(line);
            }
        } else {
            for row in rows {
                drawn.push(row);
            }
        }
        frame.draw(kit, drawn)
    }

    /// `14 workflows · 63 tasks merged · 5.0M tokens · ~$11.20 · 12h 40m`.
    pub fn totals(&self) -> Vec<(String, &'static str)> {
        let crew = self.crew;
        let workflows = if crew.workflows == 1 {
            "workflow"
        } else {
            "workflows"
        };
        let mut pieces = vec![(crew.workflows.to_string(), workflows)];
        let merged = if crew.merged == 1 {
            "task merged"
        } else {
            "tasks merged"
        };
        pieces.push((crew.merged.to_string(), merged));
        pieces.push((self.tokens(crew.total.total_tokens()), "tokens"));
        pieces.push((format::spend(&crew.total), ""));
        if crew.took_secs > 0 {
            pieces.push((took(crew.took_secs), ""));
        }
        pieces
    }

    /// The last seven days, stacked by CLI from the bottom, over their letters.
    fn day_columns(&self, kit: &Kit<'_>, rows: usize) -> Vec<Vec<Span<'static>>> {
        let measure = self.measure;
        let days: Vec<(char, Vec<(u64, Style)>)> = self
            .crew
            .days
            .iter()
            .map(|(day, parts)| {
                let letter = match day.weekday() {
                    time::Weekday::Monday => 'M',
                    time::Weekday::Tuesday | time::Weekday::Thursday => 'T',
                    time::Weekday::Wednesday => 'W',
                    time::Weekday::Friday => 'F',
                    time::Weekday::Saturday | time::Weekday::Sunday => 'S',
                };
                let stacked = self
                    .crew
                    .clis
                    .iter()
                    .map(|cli| {
                        let value = parts
                            .iter()
                            .filter(|part| part.agent == cli.agent)
                            .map(|part| part.measured(measure))
                            .sum();
                        (value, kit.fg(cli.role()))
                    })
                    .collect();
                (letter, stacked)
            })
            .collect();
        draw::day_columns(kit, &days, rows)
    }

    /// The CLIs' cards side by side, as many to a row as fit, all one height. `picked` is the
    /// card the keys are on.
    pub fn cli_cards(&self, kit: &Kit<'_>, width: usize, picked: Option<usize>) -> Vec<Drawn> {
        let clis = &self.crew.clis;
        if clis.is_empty() {
            return Vec::new();
        }
        let room = width.saturating_sub(2);
        let per_row = ((room + 1) / (LEAST_CLI_CARD + 1)).clamp(1, clis.len());
        let mut out = Vec::new();
        for (row_at, row) in clis.chunks(per_row).enumerate() {
            let card = (room + 1) / per_row - 1;
            let inner = card.saturating_sub(4);
            let bodies: Vec<Drawn> = row
                .iter()
                .map(|cli| self.cli_body(kit, cli, inner))
                .collect();
            let height = bodies.iter().map(Drawn::height).max().unwrap_or(0);
            let mut line = Drawn::new();
            let mut at = 1;
            for (k, mut body) in bodies.into_iter().enumerate() {
                while body.height() < height {
                    body.push(Vec::new());
                }
                let frame = Frame {
                    left: 0,
                    width: card,
                    toggle: None,
                    active: picked == Some(row_at * per_row + k),
                };
                let drawn = frame.draw(kit, body);
                line = if k == 0 {
                    let mut first = drawn;
                    first.indent(1);
                    first
                } else {
                    crate::widgets::side_by_side(line, drawn, at)
                };
                at += card + 1;
            }
            out.push(line);
        }
        out
    }

    /// One CLI: its tokens, work and plan, each model as a bar, how its work fared in
    /// review, its days, and its plan windows.
    fn cli_body(&self, kit: &Kit<'_>, cli: &Cli, inner: usize) -> Drawn {
        let mut drawn = Drawn::new();
        let left = vec![
            kit.span("●", cli.role()),
            Span::raw(" "),
            kit.strong(cli.name.clone()),
        ];
        let tokens = self.tokens(cli.tokens());
        drawn.push(spread(
            left,
            vec![kit.strong(tokens), kit.dim(" tok")],
            inner,
        ));

        // A judge's sign-off is a review too, so a CLI that reviews and judges counts both.
        let mut did = Vec::new();
        let tasks = cli.did(CrewRole::Worker);
        let reviews = cli.did(CrewRole::Reviewer) + cli.did(CrewRole::Judge);
        if tasks > 0 {
            did.push(count(tasks, "task"));
        }
        if reviews > 0 {
            did.push(count(reviews, "review"));
        }
        if did.is_empty() {
            did.push(match cli.did(CrewRole::Main) {
                0 => "no tasks yet".to_owned(),
                n => count(n, "turn"),
            });
        }
        let plan = self.plan_name(&cli.agent);
        let spend = format::spend(&cli.spent());
        let right = match plan {
            Some(plan) => format!("{plan} · {spend}"),
            None => spend,
        };
        let right = vec![kit.dim(right)];
        let room = inner.saturating_sub(spans_width(&right) + 1);
        let left = truncate_spans(&[kit.dim(did.join(" · "))], room);
        drawn.push(spread(left, right, inner));

        let whole = cli.measured(self.measure).max(1);
        let names = cli
            .models
            .iter()
            .map(|model| crate::text::str_width(&model.name));
        let name_col = names.max().unwrap_or(0).min(18) + 1;
        for model in &cli.models {
            let style = model_style(kit, cli, model);
            let value = model.measured(self.measure);
            let mut row = vec![Span::styled(kit.glyphs.swatch, style), Span::raw(" ")];
            row.extend(fit(vec![Span::raw(model.name.clone())], name_col));
            let work = model.main_role().map_or(0, |role| model.did(role));
            let right = match self.measure {
                StatsMeasure::Tasks => format!("{work} · {}", self.tokens(model.tokens())),
                _ => format!("{} · {work}", self.amount(value)),
            };
            let right_width = crate::text::str_width(&right) + 1;
            let cells = inner.saturating_sub(spans_width(&row) + right_width + 1);
            row.extend(draw::meter(kit, value, whole, cells, style));
            row.push(Span::raw(" "));
            drawn.push(spread(row, vec![kit.dim(right)], inner));
        }

        drawn.push(self.review_line(kit, cli, inner));
        let days = self
            .crew
            .daily(self.measure, |part| part.agent == cli.agent);
        let mut line = vec![draw::sparkline(kit, &days, kit.fg(cli.role()))];
        let mut words = " 7 days".to_owned();
        let (n, secs, unit) = if tasks > 0 {
            (tasks, cli.took(CrewRole::Worker), "a task")
        } else {
            let secs = cli.took(CrewRole::Reviewer) + cli.took(CrewRole::Judge);
            (reviews, secs, "a review")
        };
        if n > 0 && secs > 0 {
            words.push_str(&format!(" · {} {unit}", took(secs / u64::from(n))));
        }
        line.push(kit.dim(words));
        drawn.push(truncate_spans(&line, inner));
        drawn.push(self.plan_line(kit, cli, inner));
        drawn
    }

    /// Squares for the tasks it built and the share that passed first time, or for a CLI
    /// that only reviewed, its reviews and how many sent work back.
    fn review_line(&self, kit: &Kit<'_>, cli: &Cli, inner: usize) -> Vec<Span<'static>> {
        let fared = cli.fared();
        if fared.total() > 0 {
            let mut line = draw::squares(kit, fared, SQUARES);
            line.push(kit.strong(format::percent(fared.first_pass())));
            line.push(kit.dim(" first pass"));
            return truncate_spans(&line, inner);
        }
        let reviews = cli.did(CrewRole::Reviewer) + cli.did(CrewRole::Judge);
        if reviews > 0 {
            let back = cli.sent_back(CrewRole::Reviewer) + cli.sent_back(CrewRole::Judge);
            let as_tasks = Fared {
                first: reviews - back.min(reviews),
                once: back.min(reviews),
                more: 0,
            };
            let mut line = draw::squares(kit, as_tasks, SQUARES);
            line.push(kit.strong(back.to_string()));
            line.push(kit.dim(format!(" of {reviews} sent back")));
            return truncate_spans(&line, inner);
        }
        vec![kit.dim("nothing reviewed yet")]
    }

    /// `5h ▄▄▄ 1%  7d ▄▄▄ 56%`: each of the CLI's plan windows as a meter in its color,
    /// amber from 90%.
    pub(super) fn plan_line(&self, kit: &Kit<'_>, cli: &Cli, inner: usize) -> Vec<Span<'static>> {
        let Some(plan) = self.usage.plans.iter().find(|plan| plan.agent == cli.agent) else {
            return vec![kit.dim("no plan reading yet")];
        };
        if plan.pays_per_token {
            return vec![kit.dim("pays per token")];
        }
        if plan.windows.is_empty() {
            return vec![kit.dim("no plan reading yet")];
        }
        let shown = &plan.windows[..plan.windows.len().min(2)];
        // `5h ` and ` 56%` around each meter, two cells between windows.
        let labels: Vec<String> = shown.iter().map(window_label).collect();
        let fixed: usize = labels
            .iter()
            .map(|label| str_width(label) + 6)
            .sum::<usize>()
            + 2 * (shown.len() - 1);
        let cells = (inner.saturating_sub(fixed) / shown.len()).max(1);
        let mut line = Vec::new();
        for (k, (reading, label)) in shown.iter().zip(labels).enumerate() {
            if k > 0 {
                line.push(Span::raw("  "));
            }
            line.push(kit.dim(format!("{label} ")));
            line.extend(window_meter(kit, cli, reading, cells));
        }
        truncate_spans(&line, inner)
    }

    fn plan_name(&self, agent: &str) -> Option<String> {
        self.usage
            .plans
            .iter()
            .find(|plan| plan.agent == agent)
            .and_then(|plan| plan.plan.clone())
    }

    /// Each role with every model that filled it, a bar in their shades and a legend of what
    /// each did. `picked` is the role the keys are on.
    pub fn who_did_what(&self, kit: &Kit<'_>, frame: Frame, picked: Option<CrewRole>) -> Drawn {
        let inner = frame.inner();
        let mut drawn = Drawn::new();
        let left = Header::new("who did what", Role::Card).left(kit);
        let note = vec![kit.dim("every model that filled the role · enter opens a role")];
        let room = inner.saturating_sub(spans_width(&left) + 2);
        drawn.push(spread(left, truncate_spans(&note, room), inner));
        let bar_cells = inner.saturating_sub(ROLE_COLUMN);
        for role in self.crew.roles() {
            let mut filled: Vec<(&Cli, &Model)> = self
                .crew
                .models()
                .filter(|(_, model)| model.roles().contains(&role))
                .collect();
            // The CLI that did the most of the role leads, its models together.
            filled.sort_by_key(|(cli, _)| std::cmp::Reverse(cli.did(role)));
            let measured = |model: &Model| -> u64 {
                let spent = model.spent_as.get(&role).copied().unwrap_or_default();
                super::data::measured(&spent, model.did(role), self.measure)
            };
            let whole: u64 = filled.iter().map(|(_, model)| measured(model)).sum();
            let parts: Vec<(u64, Style)> = filled
                .iter()
                .map(|(cli, model)| (measured(model), model_style(kit, cli, model)))
                .collect();
            let label = if picked == Some(role) {
                vec![Span::styled(
                    format!("❯ {}", role.plural()),
                    kit.bold(Role::Card),
                )]
            } else {
                vec![kit.strong(role.plural())]
            };
            let mut row = fit(label, ROLE_COLUMN);
            row.extend(draw::bar(kit, &parts, whole, bar_cells));
            drawn.push(row);

            let did = self.crew.did(role);
            let single = filled.len() == 1;
            let entries: Vec<Entry> = filled
                .iter()
                .map(|(cli, model)| Entry {
                    style: model_style(kit, cli, model),
                    name: model.name.clone(),
                    amount: if single {
                        String::new()
                    } else {
                        model.did(role).to_string()
                    },
                    note: if single {
                        count(model.did(role), role.unit())
                    } else {
                        String::new()
                    },
                })
                .collect();
            let tail = (!single && did > 0).then(|| format!("of {}", count(did, role.unit())));
            for line in draw::legend(kit, &entries, tail, bar_cells) {
                let mut row = vec![Span::raw(" ".repeat(ROLE_COLUMN))];
                row.extend(line);
                drawn.push(row);
            }
        }
        if self.crew.roles().is_empty() {
            drawn.push(vec![kit.dim("No work recorded in this period yet.")]);
        }
        frame.draw(kit, drawn)
    }
}

/// A plan window as a meter in the CLI's color and its share used, amber from 90% and red
/// with `out` once it's spent.
pub(super) fn window_meter(
    kit: &Kit<'_>,
    cli: &Cli,
    reading: &QuotaReading,
    cells: usize,
) -> Vec<Span<'static>> {
    let used = used_of(reading);
    let level = if used >= 1.0 {
        Level::Danger
    } else if used >= 0.9 {
        Level::Warn
    } else {
        Level::Ok
    };
    let fill = match level {
        Level::Ok => kit.fg(cli.role()),
        level => kit.fg(level.number_role()),
    };
    let percent = if reading.status == QuotaStatus::Rejected {
        "out".to_owned()
    } else {
        format::percent(used)
    };
    let mut meter = draw::meter(kit, (used * 1000.0) as u64, 1000, cells, fill);
    meter.push(Span::styled(
        format!(" {percent:>4}"),
        kit.style(level.number_role()),
    ));
    meter
}

/// How tasks fared in review, as one bar: green, amber, red.
pub(super) fn fared_bar(kit: &Kit<'_>, fared: Fared, cells: usize) -> Vec<Span<'static>> {
    let parts = [
        (u64::from(fared.first), kit.fg(Role::Success)),
        (u64::from(fared.once), kit.fg(Role::Warning)),
        (u64::from(fared.more), kit.fg(Role::Error)),
    ];
    draw::bar(kit, &parts, u64::from(fared.total()), cells)
}
