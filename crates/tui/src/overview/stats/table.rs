//! The by-model and by-role views: one row per model across every CLI, or one per role,
//! each with what it did, how it fared in review, what it used, and how long each piece of
//! work took.

use argonauts_core::crew_stats::CrewRole;
use ratatui::style::Style;
use ratatui::text::Span;

use super::cli::{SQUARES, Stats, count, model_style, took};
use super::data::{Cli, Model, measured, short_cli};
use super::draw::{self, Fared};
use crate::format;
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Frame, Kit, fit};

/// The order `s` puts the models in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// What the bars measure, the most first.
    #[default]
    Measure,
    /// Pieces of work, the most first.
    Work,
    Name,
}

impl Sort {
    pub fn next(self) -> Self {
        match self {
            Sort::Measure => Sort::Work,
            Sort::Work => Sort::Name,
            Sort::Name => Sort::Measure,
        }
    }
}

/// A table's column: its heading, and whether it gives way on a narrow screen.
struct Column {
    title: &'static str,
    /// Columns go from the highest number down until the row fits. Zero never does.
    gives_way: u8,
}

/// Cells for the row's mark, `❯ `.
const MARK: usize = 2;
/// The bars in the measure column.
const METER: usize = 6;

/// One row's cells, column by column.
struct Row {
    picked: bool,
    cells: Vec<Vec<Span<'static>>>,
}

/// Each column as wide as its widest cell, two cells apart, or one apart when that's what
/// fits. Past that, columns give way in their order.
fn table(kit: &Kit<'_>, columns: &[Column], rows: Vec<Row>, inner: usize) -> Drawn {
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(k, column)| {
            let cells = rows.iter().map(|row| spans_width(&row.cells[k]));
            cells.chain([str_width(column.title)]).max().unwrap_or(0)
        })
        .collect();
    let mut keep: Vec<bool> = vec![true; columns.len()];
    let used = |keep: &[bool], gap: usize| -> usize {
        let kept: Vec<usize> = (0..columns.len()).filter(|&k| keep[k]).collect();
        let cells: usize = kept.iter().map(|&k| widths[k]).sum();
        MARK + cells + gap * kept.len().saturating_sub(1)
    };
    let mut order: Vec<usize> = (0..columns.len())
        .filter(|&k| columns[k].gives_way > 0)
        .collect();
    order.sort_by_key(|&k| std::cmp::Reverse(columns[k].gives_way));
    let mut order = order.into_iter();
    let gap = loop {
        if used(&keep, 2) <= inner {
            break 2;
        }
        if used(&keep, 1) <= inner {
            break 1;
        }
        match order.next() {
            Some(k) => keep[k] = false,
            None => break 1,
        }
    };
    let kept: Vec<usize> = (0..columns.len()).filter(|&k| keep[k]).collect();
    let line = |mark: Span<'static>, cells: Vec<Vec<Span<'static>>>| {
        let mut line = vec![mark];
        let mut cells: Vec<Option<Vec<Span<'static>>>> = cells.into_iter().map(Some).collect();
        for (n, &k) in kept.iter().enumerate() {
            let width = if n + 1 == kept.len() {
                widths[k]
            } else {
                widths[k] + gap
            };
            line.extend(fit(cells[k].take().unwrap_or_default(), width));
        }
        truncate_spans(&line, inner)
    };
    let mut drawn = Drawn::new();
    let titles = columns
        .iter()
        .map(|column| vec![kit.dim(column.title)])
        .collect();
    drawn.push(line(Span::raw(" ".repeat(MARK)), titles));
    for row in rows {
        let mark = if row.picked {
            Span::styled("❯ ", kit.bold(Role::Card))
        } else {
            Span::raw("  ")
        };
        drawn.push(line(mark, row.cells));
    }
    drawn
}

/// `■■■■■■■■■■96%` for a builder's tasks, `sent back 4` for a reviewer or judge.
fn in_review(kit: &Kit<'_>, fared: Fared, back: Option<u32>) -> Vec<Span<'static>> {
    if fared.total() > 0 {
        let mut cell = draw::squares(kit, fared, SQUARES);
        cell.push(kit.strong(format::percent(fared.first_pass())));
        return cell;
    }
    match back {
        Some(back) => vec![kit.dim("sent back "), kit.strong(back.to_string())],
        None => Vec::new(),
    }
}

impl Stats<'_> {
    /// A measured amount behind a short bar, scaled to the largest row, each part in its
    /// model's shade.
    fn measured_cell(&self, kit: &Kit<'_>, parts: &[(u64, Style)], top: u64) -> Vec<Span<'static>> {
        let value = parts.iter().map(|(value, _)| value).sum();
        let mut cell = draw::bar(kit, parts, top.max(value), METER);
        cell.push(Span::raw(" "));
        cell.push(Span::raw(self.amount(value)));
        cell
    }

    /// The models in the order `sort` asks, each with its CLI.
    pub fn sorted_models(&self, sort: Sort) -> Vec<(&Cli, &Model)> {
        let mut models: Vec<(&Cli, &Model)> = self.crew.models().collect();
        match sort {
            Sort::Measure => {
                models.sort_by_key(|(_, model)| std::cmp::Reverse(model.measured(self.measure)))
            }
            Sort::Work => models.sort_by_key(|(_, model)| std::cmp::Reverse(model.work())),
            Sort::Name => models.sort_by(|(_, a), (_, b)| a.name.cmp(&b.name)),
        }
        models
    }

    /// One row per model, whichever CLI runs it.
    pub fn by_model(&self, kit: &Kit<'_>, frame: Frame, sort: Sort, picked: usize) -> Drawn {
        let models = self.sorted_models(sort);
        let columns = [
            Column {
                title: "model",
                gives_way: 0,
            },
            Column {
                title: "on",
                gives_way: 2,
            },
            Column {
                title: "as",
                gives_way: 4,
            },
            Column {
                title: "did",
                gives_way: 0,
            },
            Column {
                title: "in review",
                gives_way: 1,
            },
            Column {
                title: self.measure.word(),
                gives_way: 0,
            },
            Column {
                title: "spend",
                gives_way: 3,
            },
            Column {
                title: "each",
                gives_way: 5,
            },
        ];
        let top = models
            .iter()
            .map(|(_, model)| model.measured(self.measure))
            .max()
            .unwrap_or(0);
        let rows = models
            .iter()
            .enumerate()
            .map(|(at, (cli, model))| {
                let style = model_style(kit, cli, model);
                let role = model.main_role();
                let did = role.map_or(String::new(), |role| count(model.did(role), role.unit()));
                let back = role
                    .filter(|role| matches!(role, CrewRole::Reviewer | CrewRole::Judge))
                    .map(|role| model.sent_back.get(&role).copied().unwrap_or(0));
                let each = role
                    .filter(|role| model.did(*role) > 0)
                    .map(|role| {
                        let took_secs = model.took.get(&role).copied().unwrap_or(0);
                        took(took_secs / u64::from(model.did(role)))
                    })
                    .unwrap_or_default();
                Row {
                    picked: at == picked,
                    cells: vec![
                        vec![
                            Span::styled(kit.glyphs.swatch, style),
                            Span::raw(" "),
                            kit.strong(model.name.clone()),
                        ],
                        vec![kit.dim(on(cli))],
                        vec![kit.dim(roles_of(model))],
                        vec![Span::raw(did)],
                        in_review(kit, model.fared, back),
                        self.measured_cell(kit, &[(model.measured(self.measure), style)], top),
                        vec![Span::raw(format::spend(&model.spent))],
                        vec![kit.dim(each)],
                    ],
                }
            })
            .collect();
        frame.draw(kit, table(kit, &columns, rows, frame.inner()))
    }

    /// One row per role, with every model that filled it.
    pub fn by_role(&self, kit: &Kit<'_>, frame: Frame, picked: usize) -> Drawn {
        let roles = self.crew.roles();
        let columns = [
            Column {
                title: "role",
                gives_way: 0,
            },
            Column {
                title: "filled by",
                gives_way: 2,
            },
            Column {
                title: "did",
                gives_way: 0,
            },
            Column {
                title: "in review",
                gives_way: 1,
            },
            Column {
                title: self.measure.word(),
                gives_way: 0,
            },
            Column {
                title: "spend",
                gives_way: 3,
            },
            Column {
                title: "each",
                gives_way: 4,
            },
        ];
        let filled_by = |role: CrewRole| -> Vec<(&Cli, &Model)> {
            let mut filled: Vec<(&Cli, &Model)> = self
                .crew
                .models()
                .filter(|(_, model)| model.roles().contains(&role))
                .collect();
            filled.sort_by_key(|(cli, _)| std::cmp::Reverse(cli.did(role)));
            filled
        };
        let role_value = |role: CrewRole| -> u64 {
            filled_by(role)
                .iter()
                .map(|(_, model)| {
                    let spent = model.spent_as.get(&role).copied().unwrap_or_default();
                    measured(&spent, model.did(role), self.measure)
                })
                .sum()
        };
        let top = roles
            .iter()
            .map(|role| role_value(*role))
            .max()
            .unwrap_or(0);
        let rows = roles
            .iter()
            .enumerate()
            .map(|(at, &role)| {
                let filled = filled_by(role);
                let parts: Vec<(u64, Style)> = filled
                    .iter()
                    .map(|(cli, model)| {
                        let spent = model.spent_as.get(&role).copied().unwrap_or_default();
                        let value = measured(&spent, model.did(role), self.measure);
                        (value, model_style(kit, cli, model))
                    })
                    .collect();
                let mut swatches = Vec::new();
                for (k, (cli, model)) in filled.iter().enumerate() {
                    if k > 0 {
                        swatches.push(Span::raw(" "));
                    }
                    swatches.push(Span::styled(
                        kit.glyphs.swatch,
                        model_style(kit, cli, model),
                    ));
                    swatches.push(Span::raw(format!(" {}", model.name)));
                }
                let did = self.crew.did(role);
                let mut fared = Fared::default();
                let mut spent = argonauts_core::Spend::default();
                let mut took_secs = 0;
                let mut back = 0;
                for (_, model) in &filled {
                    if role == CrewRole::Worker {
                        fared.first += model.fared.first;
                        fared.once += model.fared.once;
                        fared.more += model.fared.more;
                    }
                    spent = spent.plus(&model.spent_as.get(&role).copied().unwrap_or_default());
                    took_secs += model.took.get(&role).copied().unwrap_or(0);
                    back += model.sent_back.get(&role).copied().unwrap_or(0);
                }
                let back = matches!(role, CrewRole::Reviewer | CrewRole::Judge).then_some(back);
                let each = if did > 0 && took_secs > 0 {
                    took(took_secs / u64::from(did))
                } else {
                    String::new()
                };
                Row {
                    picked: at == picked,
                    cells: vec![
                        vec![kit.strong(role.plural())],
                        swatches,
                        vec![Span::raw(if did > 0 {
                            count(did, role.unit())
                        } else {
                            String::new()
                        })],
                        in_review(kit, fared, back),
                        self.measured_cell(kit, &parts, top),
                        vec![Span::raw(format::spend(&spent))],
                        vec![kit.dim(each)],
                    ],
                }
            })
            .collect();
        frame.draw(kit, table(kit, &columns, rows, frame.inner()))
    }
}

/// The CLI as the by-model rows name it: `Claude Code`, `Codex`, `agy`.
fn on(cli: &Cli) -> String {
    match cli.agent.as_str() {
        "antigravity-cli" => short_cli(&cli.agent, &cli.name),
        _ => cli.name.clone(),
    }
}

/// `orchestrator, worker`.
fn roles_of(model: &Model) -> String {
    model
        .roles()
        .iter()
        .map(|role| role.word())
        .collect::<Vec<_>>()
        .join(", ")
}
