//! The by-model and by-role views: one row per model across every CLI, or one per role with
//! every model that filled it, each with what it did, how it fared in review, what it used,
//! and how long each piece of work took.

use argonauts_core::Spend;
use argonauts_core::crew_stats::CrewRole;
use argonauts_core::settings::stats::StatsMeasure;
use ratatui::style::Style;
use ratatui::text::Span;

use super::cli::{SQUARES, Stats, count, model_style, took};
use super::data::{Cli, Model, short_cli};
use super::draw::{self, Entry, Fared};
use crate::format;
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Frame, Kit, fit};

/// The order `s` puts the rows in.
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

/// The order `s` puts the roles in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoleSort {
    /// As the who-did-what card lists them, from planning to signing off.
    #[default]
    Listed,
    /// What the bars measure, the most first.
    Measure,
    /// Pieces of work, the most first.
    Work,
}

impl RoleSort {
    pub fn next(self) -> Self {
        match self {
            RoleSort::Listed => RoleSort::Measure,
            RoleSort::Measure => RoleSort::Work,
            RoleSort::Work => RoleSort::Listed,
        }
    }
}

/// A table's column: its heading, the cells it takes at least so the tables read alike
/// whatever is in them, and when it gives way on a narrow screen.
struct Column {
    title: &'static str,
    least: usize,
    /// Columns go from the highest number down until the row fits. Zero never does.
    gives_way: u8,
}

const fn column(title: &'static str, least: usize, gives_way: u8) -> Column {
    Column {
        title,
        least,
        gives_way,
    }
}

/// The by-model columns.
const MODEL_COLUMNS: [Column; 8] = [
    column("model", 21, 0),
    column("on", 12, 2),
    column("as", 21, 4),
    column("did", 14, 0),
    column("in review", 16, 1),
    column("tokens", 15, 0),
    column("spend", 8, 3),
    column("each", 7, 5),
];

/// The by-role columns. The second holds the bar of who filled the role.
const ROLE_COLUMNS: [Column; 7] = [
    column("role", 16, 0),
    column("filled by", 30, 0),
    column("did", 15, 0),
    column("in review", 17, 1),
    column("tokens", 15, 0),
    column("spend", 9, 3),
    column("each", 7, 4),
];

/// The bar in the measure column.
const METER: usize = 8;
/// The widest and the narrowest bar of who filled a role.
const FILLED_BY: usize = 28;
const LEAST_FILLED_BY: usize = 8;

/// Which columns show and how wide each is: each at least its least and a cell wider than
/// its widest cell, then only that, then the columns that give way go until the row fits.
fn widths(columns: &[Column], cells: &[Vec<usize>], inner: usize) -> Vec<Option<usize>> {
    let content: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(k, column)| {
            let widest = cells.iter().map(|row| row[k]).max().unwrap_or(0);
            widest.max(str_width(column.title))
        })
        .collect();
    let mut keep = vec![true; columns.len()];
    let mut order: Vec<usize> = (0..columns.len())
        .filter(|&k| columns[k].gives_way > 0)
        .collect();
    order.sort_by_key(|&k| std::cmp::Reverse(columns[k].gives_way));
    let mut order = order.into_iter();
    loop {
        let kept: Vec<usize> = (0..columns.len()).filter(|&k| keep[k]).collect();
        let last = kept.last().copied();
        let gap = |k: usize| usize::from(Some(k) != last);
        let roomy: Vec<usize> = kept
            .iter()
            .map(|&k| columns[k].least.max(content[k] + gap(k)))
            .collect();
        let tight: Vec<usize> = kept.iter().map(|&k| content[k] + gap(k)).collect();
        let fits = [roomy, tight]
            .into_iter()
            .find(|widths| widths.iter().sum::<usize>() <= inner);
        let chosen = match (fits, order.next()) {
            (Some(fits), _) => fits,
            (None, Some(k)) => {
                keep[k] = false;
                continue;
            }
            (None, None) => kept.iter().map(|&k| content[k] + gap(k)).collect(),
        };
        let mut out = vec![None; columns.len()];
        for (k, width) in kept.into_iter().zip(chosen) {
            out[k] = Some(width);
        }
        return out;
    }
}

/// The row's cells in their columns, cut at `inner`.
fn line(
    cells: Vec<Vec<Span<'static>>>,
    widths: &[Option<usize>],
    inner: usize,
) -> Vec<Span<'static>> {
    let mut line = Vec::new();
    for (cell, width) in cells.into_iter().zip(widths) {
        if let Some(width) = width {
            line.extend(fit(cell, *width));
        }
    }
    truncate_spans(&line, inner)
}

/// `❯ ` before the row the keys are on, two blanks before the others.
fn mark(kit: &Kit<'_>, picked: bool) -> Span<'static> {
    if picked {
        Span::styled("❯ ", kit.bold(Role::Card))
    } else {
        Span::raw("  ")
    }
}

/// A row's name, bold, in the card's color when the keys are on it.
fn name(kit: &Kit<'_>, text: &str, picked: bool) -> Span<'static> {
    if picked {
        Span::styled(text.to_owned(), kit.bold(Role::Card))
    } else {
        kit.strong(text.to_owned())
    }
}

/// `▪▪▪▪▪▪▪▪▪▪ 96%` for built tasks, `sent back 4` for reviews and rulings.
pub(super) fn in_review(kit: &Kit<'_>, fared: Fared, back: Option<u32>) -> Vec<Span<'static>> {
    if fared.total() > 0 {
        let mut cell = draw::tenths(kit, fared, SQUARES);
        cell.push(Span::raw(" "));
        cell.push(kit.strong(format::percent(fared.first_pass())));
        return cell;
    }
    match back {
        Some(back) => vec![kit.dim("sent back "), kit.strong(back.to_string())],
        None => Vec::new(),
    }
}

/// What was spent as a table writes it: whole dollars from ten, so the column stays narrow.
pub(super) fn spend_cell(spent: &Spend) -> String {
    let words = format::spend(spent);
    if spent.cost_usd < 10.0 {
        return words;
    }
    let whole = format!("${:.0}", spent.cost_usd);
    words.replace(&format::usd(spent.cost_usd), &whole)
}

/// The time each piece of work took, from the time together.
pub(super) fn each(secs: u64, done: u32) -> String {
    if done == 0 || secs == 0 {
        return String::new();
    }
    took(secs / u64::from(done))
}

impl Stats<'_> {
    /// A measured amount behind a short bar scaled to the largest row.
    fn measured_cell(
        &self,
        kit: &Kit<'_>,
        value: u64,
        top: u64,
        style: Style,
    ) -> Vec<Span<'static>> {
        let mut cell = draw::thin(kit, value, top.max(value), METER, style);
        let amount = match self.measure {
            StatsMeasure::Tokens => self.tokens_short(value),
            _ => self.amount(value),
        };
        cell.push(Span::raw(format!(" {amount}")));
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
        let inner = frame.inner();
        let models = self.sorted_models(sort);
        let top = models
            .iter()
            .map(|(_, model)| model.measured(self.measure))
            .max()
            .unwrap_or(0);
        let mut titles: Vec<Vec<Span<'static>>> = MODEL_COLUMNS
            .iter()
            .map(|column| vec![kit.dim(column.title)])
            .collect();
        titles[5] = vec![kit.dim(self.measure.word())];
        let rows: Vec<Vec<Vec<Span<'static>>>> = models
            .iter()
            .enumerate()
            .map(|(at, (cli, model))| {
                let style = model_style(kit, cli, model);
                let role = model.main_role();
                let did = role.map_or(0, |role| model.did(role));
                let back = role
                    .filter(|role| matches!(role, CrewRole::Reviewer | CrewRole::Judge))
                    .map(|role| model.sent_back.get(&role).copied().unwrap_or(0));
                let took_secs = role.map_or(0, |role| model.took.get(&role).copied().unwrap_or(0));
                let mut first = vec![mark(kit, at == picked)];
                first.extend(draw::swatch(kit, style));
                first.push(name(kit, &model.name, at == picked));
                vec![
                    first,
                    vec![kit.dim(on(cli))],
                    vec![kit.dim(roles_of(model))],
                    vec![Span::raw(
                        role.map_or(String::new(), |role| count(did, role.unit())),
                    )],
                    in_review(kit, model.fared, back),
                    self.measured_cell(kit, model.measured(self.measure), top, style),
                    vec![Span::raw(spend_cell(&model.spent))],
                    vec![kit.dim(each(took_secs, did))],
                ]
            })
            .collect();
        let sizes: Vec<Vec<usize>> = std::iter::once(&titles)
            .chain(&rows)
            .map(|row| row.iter().map(|cell| spans_width(cell)).collect())
            .collect();
        let widths = widths(&MODEL_COLUMNS, &sizes, inner);
        let mut drawn = Drawn::new();
        drawn.push(line(titles, &widths, inner));
        for row in rows {
            drawn.push(line(row, &widths, inner));
        }
        frame.draw(kit, drawn)
    }

    /// The models that filled `role`, the CLI that did the most of it first, its models
    /// together.
    pub(super) fn filled_by(&self, role: CrewRole) -> Vec<(&Cli, &Model)> {
        let mut filled: Vec<(&Cli, &Model)> = self
            .crew
            .models()
            .filter(|(_, model)| model.roles().contains(&role))
            .collect();
        filled.sort_by_key(|(cli, _)| std::cmp::Reverse(cli.did(role)));
        filled
    }

    /// What every model in `role` used in it.
    pub(super) fn spent_as(&self, role: CrewRole) -> Spend {
        self.filled_by(role)
            .iter()
            .fold(Spend::default(), |spent, (_, model)| {
                spent.plus(&model.spent_as.get(&role).copied().unwrap_or_default())
            })
    }

    /// What the bars measure for everything `role` did.
    fn role_measured(&self, role: CrewRole) -> u64 {
        super::data::measured(&self.spent_as(role), self.crew.did(role), self.measure)
    }

    /// The roles in the order `sort` asks.
    pub fn sorted_roles(&self, sort: RoleSort) -> Vec<CrewRole> {
        let mut roles = self.crew.roles();
        match sort {
            RoleSort::Listed => {}
            RoleSort::Measure => {
                roles.sort_by_key(|role| std::cmp::Reverse(self.role_measured(*role)))
            }
            RoleSort::Work => roles.sort_by_key(|role| std::cmp::Reverse(self.crew.did(*role))),
        }
        roles
    }

    /// One row per role, with every model that filled it as a bar and a legend under it.
    pub fn by_role(&self, kit: &Kit<'_>, frame: Frame, sort: RoleSort, picked: usize) -> Drawn {
        let inner = frame.inner();
        let roles = self.sorted_roles(sort);
        let top = roles
            .iter()
            .map(|role| self.role_measured(*role))
            .max()
            .unwrap_or(0);
        let mut titles: Vec<Vec<Span<'static>>> = ROLE_COLUMNS
            .iter()
            .map(|column| vec![kit.dim(column.title)])
            .collect();
        titles[4] = vec![kit.dim(self.measure.word())];
        // Each role's cells, its bar's parts and its legend; the bar is drawn once the room
        // the other columns leave it is known.
        let mut rows = Vec::new();
        for (at, &role) in roles.iter().enumerate() {
            let filled = self.filled_by(role);
            let parts: Vec<(u64, Style)> = filled
                .iter()
                .map(|(cli, model)| (u64::from(model.did(role)), model_style(kit, cli, model)))
                .collect();
            let entries: Vec<Entry> = filled
                .iter()
                .map(|(cli, model)| Entry {
                    style: model_style(kit, cli, model),
                    name: model.name.clone(),
                    amount: model.did(role).to_string(),
                    note: String::new(),
                })
                .collect();
            let did = self.crew.did(role);
            let mut fared = Fared::default();
            let mut took_secs = 0;
            let mut back = 0;
            for (_, model) in &filled {
                if role == CrewRole::Worker {
                    fared.first += model.fared.first;
                    fared.once += model.fared.once;
                    fared.more += model.fared.more;
                }
                took_secs += model.took.get(&role).copied().unwrap_or(0);
                back += model.sent_back.get(&role).copied().unwrap_or(0);
            }
            let back = matches!(role, CrewRole::Reviewer | CrewRole::Judge).then_some(back);
            let did_words = if did > 0 {
                count(did, role.unit())
            } else {
                String::new()
            };
            let value = self.role_measured(role);
            let cells = vec![
                vec![
                    mark(kit, at == picked),
                    name(kit, role.plural(), at == picked),
                ],
                Vec::new(),
                vec![Span::raw(did_words)],
                in_review(kit, fared, back),
                self.measured_cell(kit, value, top, kit.fg(Role::Accent)),
                vec![Span::raw(spend_cell(&self.spent_as(role)))],
                vec![kit.dim(each(took_secs, did))],
            ];
            rows.push((cells, parts, entries));
        }
        let mut sizes: Vec<Vec<usize>> = std::iter::once(&titles)
            .chain(rows.iter().map(|(cells, _, _)| cells))
            .map(|row| row.iter().map(|cell| spans_width(cell)).collect())
            .collect();
        for row in &mut sizes {
            row[1] = LEAST_FILLED_BY + 2;
        }
        let mut widths = widths(&ROLE_COLUMNS, &sizes, inner);
        // The bar takes the room the other columns leave, up to its widest.
        let others: usize = widths
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != 1)
            .filter_map(|(_, width)| *width)
            .sum();
        let bar = inner
            .saturating_sub(others + 2)
            .clamp(LEAST_FILLED_BY, FILLED_BY);
        widths[1] = Some(bar + 2);
        let indent = widths[0].unwrap_or(0);

        let mut drawn = Drawn::new();
        drawn.push(line(titles, &widths, inner));
        for (mut cells, parts, entries) in rows {
            drawn.push(Vec::new());
            cells[1] = draw::stacked(kit, &parts, bar);
            drawn.push(line(cells, &widths, inner));
            let room = inner.saturating_sub(indent);
            for legend in draw::legend_apart(kit, &entries, None, room, 2) {
                let mut row = vec![Span::raw(" ".repeat(indent))];
                row.extend(legend);
                drawn.push(row);
            }
        }
        frame.draw(kit, drawn)
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
