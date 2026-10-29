//! Every model, or every role, together: the box By model and By role open with. Three bars of
//! what the parts used, tokens, spend and agent time, then a table with a row for each part
//! (see [`super::table`]), beside the period's chart stacked by CLI or by role. In a bar each
//! CLI's models sit together, a blank cell between CLIs: its first three models in their own
//! textures and every other one in a run of the fourth, so a CLI's width is its exact share
//! however many models it ran.

mod bars;

use argonauts_core::Spend;
use argonauts_core::logos::Logo;
use ratatui::text::Span;

use super::cli::{Stack, Stats, header_row_keeping_last, took_whole};
use super::data::DayPart;
use super::draw::{self, Fill};
use super::table::{Counted, Mark, Sort, Table, fine};
use crate::format;
use crate::kit::legend::{self, Columns, Figure, Sorted, TableRow};
use crate::kit::logo::{ModelLabel, NoPicture};
use crate::kit::{Drawn, Frame, Kit};
use crate::text::str_width;
use crate::theme::Role;

/// The box's measures, as its bars and its table's columns name them.
const MEASURES: [&str; 3] = ["tokens", "spend", "time"];
/// Cells of a bar's label: the longest, `tokens`, and a blank.
const LABEL: usize = 7;
/// Cells before a part's name in the table, as many as a bar's label, so the names start
/// where the bars do.
const EMBLEM: usize = draw::EMBLEM;
const _: () = assert!(EMBLEM == LABEL);

/// Which of the period's parts something holds, for the chart.
type Holds<'a> = Box<dyn Fn(&DayPart) -> bool + 'a>;

/// A model or a role in the box.
pub(super) struct Part<'a> {
    /// Its name and what it used, as the table reads them.
    pub counted: Counted,
    pub fill: Fill,
    /// The logo before a model's name. A role has none.
    pub logo: Option<Logo>,
    /// The bars draw a group's parts side by side, a blank cell between groups: a model's
    /// group is its CLI. Every role is in one group.
    pub group: usize,
    /// The part shares its group's last run in a bar with the others that fold, as a CLI's
    /// models after its third do.
    pub folds: bool,
    /// Its order in its group's bar, before it folds: a model's place among its CLI's.
    pub rank: usize,
    /// The chart's stack it is drawn in: its CLI's for a model, its own for a role.
    pub stack: usize,
    /// Which of the period's parts are its own, for the chart.
    pub holds: Holds<'a>,
}

impl Part<'_> {
    /// What it used of a measure, in the bars' units: tokens, hundredths of a cent, seconds.
    fn used(&self, measure: usize) -> u64 {
        let counted = &self.counted;
        match measure {
            0 => counted.tokens(),
            1 => counted.spend(),
            _ => counted.took_secs,
        }
    }
}

/// What the box draws: its title, what one row of its table is, its parts in the bars' order
/// with the fill of each chart stack, and the table's keys.
pub(super) struct Together<'a> {
    pub title: &'a str,
    pub by: &'a str,
    pub parts: &'a [Part<'a>],
    pub stacks: &'a [Fill],
    pub table: &'a Table,
    /// The keys are on the table, so its pointer and the pointed part's light show.
    pub in_table: bool,
}

impl Stats<'_> {
    /// `◆ all models  every day so far` and the period's totals, then a bar each for the
    /// tokens, the spend and the agent time, a blank row between them, and the table under
    /// them, beside the period's chart where the table keeps room for its names.
    pub fn together(&self, kit: &Kit<'_>, frame: Frame, look: Together<'_>) -> Drawn {
        let inner = frame.inner();
        let parts = look.parts;
        let counted: Vec<Counted> = parts.iter().map(|part| part.counted.clone()).collect();
        let lit = look.table.lit(&counted, look.in_table);

        let mut drawn = Drawn::new();
        let left = vec![
            kit.span(kit.glyphs.the_mod, Role::Question),
            Span::raw(" "),
            kit.strong(look.title.to_owned()),
            kit.dim(format!("  {}", self.period_words())),
        ];
        // The shares are of the tokens, the spend and the time, so on a narrow screen the
        // title keeps those.
        // The time is the table's column added up, the time each part's sessions took, so
        // the title and the rows under it agree.
        let column_secs = counted.iter().map(|counted| counted.took_secs).sum();
        let totals = self.totals_taking(column_secs, took_whole);
        drawn.push(header_row_keeping_last(kit, left, &totals, inner));
        drawn.push(Vec::new());

        let chart = self.chart_fits(inner, Columns::least(EMBLEM, MEASURES.len()));
        let width = if chart {
            self.beside_chart(inner)
        } else {
            inner
        };
        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        let cells = width.saturating_sub(legend::POINTER + LABEL);
        for (measure, label) in MEASURES.iter().enumerate() {
            if measure > 0 {
                rows.push(Vec::new());
            }
            let mut row = vec![Span::raw(" ".repeat(legend::POINTER))];
            row.push(Span::raw(crate::kit::ljust(label, LABEL)));
            row.extend(bars::bar(kit, parts, measure, lit.as_deref(), cells));
            rows.push(row);
        }
        rows.extend(self.table_rows(kit, &look, &counted, width));

        if chart {
            let lit = lit.as_deref();
            let grey = Fill::from(kit.fg(Role::Faint));
            let holds: Vec<Holds<'_>> = (0..look.stacks.len())
                .map(|stack| {
                    Box::new(move |day: &DayPart| {
                        parts.iter().enumerate().any(|(at, part)| {
                            part.stack == stack
                                && lit.is_none_or(|lit| lit[at])
                                && (part.holds)(day)
                        })
                    }) as Holds<'_>
                })
                .chain(lit.map(|lit| {
                    Box::new(move |day: &DayPart| {
                        parts
                            .iter()
                            .enumerate()
                            .any(|(at, part)| !lit[at] && (part.holds)(day))
                    }) as Holds<'_>
                }))
                .collect();
            let fills = look.stacks.iter().copied().chain(lit.map(|_| grey));
            let stacks: Vec<Stack<'_>> = fills
                .zip(&holds)
                .map(|(fill, holds)| Stack {
                    fill,
                    holds: holds.as_ref(),
                })
                .collect();
            rows = self.chart_beside(kit, rows, &stacks);
        }
        for row in rows {
            drawn.push(row);
        }
        frame.draw(kit, drawn)
    }

    /// The table: the filter's row or a blank, the titles, then the window's rows, each part's
    /// numbers in its columns and a row of sums for the rows out of view at either end.
    fn table_rows(
        &self,
        kit: &Kit<'_>,
        look: &Together<'_>,
        counted: &[Counted],
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let table = look.table;
        let order = table.order(counted);
        let every: Vec<usize> = (0..counted.len()).collect();
        let whole = Totals::of(counted, &every);
        // Every part's amounts and the sums of all of them set the columns' width, so
        // scrolling, sorting or filtering never moves a column.
        let widest = (every.iter())
            .map(|at| self.figures(&Totals::of(counted, &[*at]), &whole))
            .chain([self.figures(&whole, &whole)])
            .flatten()
            .map(|figure| str_width(&figure.amount))
            .max()
            .unwrap_or(0);
        let columns = Columns::fitting(width, EMBLEM, MEASURES.len(), widest);
        let mut rows = Vec::new();
        match &table.query {
            Some(query) => {
                let mut words = format!("/ {query}");
                if table.typing {
                    words.push('▏');
                }
                words.push_str(&format!("   {} of {} match", order.len(), counted.len()));
                let row = TableRow {
                    name: vec![Span::raw(words)],
                    figures: self.figures(&Totals::of(counted, &order), &whole),
                    ..TableRow::default()
                };
                rows.push(legend::table_row(kit, columns, &row));
            }
            None => rows.push(Vec::new()),
        }
        let sorted = match table.sort_for(counted.len()) {
            Sort::Bars => None,
            Sort::Tokens => Some(Sorted::Measure(0)),
            Sort::Spend => Some(Sorted::Measure(1)),
            Sort::Time => Some(Sorted::Measure(2)),
            Sort::Name => Some(Sorted::Name),
        };
        rows.push(legend::table_head(columns, look.by, &MEASURES, sorted));
        if order.is_empty() {
            let query = table.query.as_deref().unwrap_or_default();
            let row = TableRow {
                name: vec![kit.dim(format!("nothing matches {query}"))],
                ..TableRow::default()
            };
            rows.push(legend::table_row(kit, columns, &row));
            return rows;
        }
        let window = table.window(order.len());
        let pointed = |mark: Option<Mark>| table.pointing(look.in_table) && table.on_mark == mark;
        let mark = |mark: Mark, hidden: &[usize]| {
            let (arrow, place) = match mark {
                Mark::Above => ("↑", "above"),
                Mark::Below => ("↓", "below"),
            };
            // Rows out of view that hold nothing leave their sums blank, so the line never
            // reads as a row of its own.
            let sums = Totals::of(counted, hidden);
            TableRow {
                name: vec![kit.dim(format!("{arrow}  {} more {place}", hidden.len()))],
                figures: if sums.is_nothing() {
                    Vec::new()
                } else {
                    self.figures(&sums, &whole)
                },
                pointed: pointed(Some(mark)),
                ..TableRow::default()
            }
        };
        if window.above > 0 {
            let row = mark(Mark::Above, &order[..window.rows.start]);
            rows.push(legend::table_row(kit, columns, &row));
        }
        for row_at in window.rows.clone() {
            let at = order[row_at];
            let part = &look.parts[at];
            let row = TableRow {
                emblem: emblem(kit, part),
                name: vec![Span::raw(match &part.logo {
                    Some(logo) => {
                        ModelLabel::named(logo, &part.counted.name, kit.theme, NoPicture::Dot).name
                    }
                    None => part.counted.name.clone(),
                })],
                figures: self.figures(&Totals::of(counted, &[at]), &whole),
                pointed: pointed(None) && row_at == table.at.min(order.len() - 1),
            };
            rows.push(legend::table_row(kit, columns, &row));
        }
        if window.below > 0 {
            let row = mark(Mark::Below, &order[window.rows.end..]);
            rows.push(legend::table_row(kit, columns, &row));
        }
        rows
    }

    /// `2.60M 26%`, `~$7.10 42%` and `7h 40m 39%`: what `some` used, and their share of
    /// `whole`.
    fn figures(&self, some: &Totals, whole: &Totals) -> Vec<Figure> {
        vec![
            Figure {
                amount: self.table_tokens(some.spent.total_tokens()),
                share: legend::share(some.spent.total_tokens(), whole.spent.total_tokens()),
            },
            Figure {
                amount: format::spend(&some.spent),
                share: legend::share(fine(&some.spent), fine(&whole.spent)),
            },
            Figure {
                amount: took_whole(some.took_secs),
                share: legend::share(some.took_secs, whole.took_secs),
            },
        ]
    }
}

/// What some of the parts used together.
struct Totals {
    spent: Spend,
    took_secs: u64,
}

impl Totals {
    fn of(counted: &[Counted], some: &[usize]) -> Self {
        let mut spent = Spend::default();
        let mut took_secs = 0;
        for at in some {
            spent = spent.plus(&counted[*at].spent);
            took_secs += counted[*at].took_secs;
        }
        Self { spent, took_secs }
    }

    fn is_nothing(&self) -> bool {
        self.spent.total_tokens() == 0 && fine(&self.spent) == 0 && self.took_secs == 0
    }
}

/// What stands before a part's name: a model's logo and its swatch in its texture. A role
/// keeps the logo's cells blank.
fn emblem(kit: &Kit<'_>, part: &Part<'_>) -> Vec<Span<'static>> {
    let swatch = bars::swatch(kit, part);
    draw::emblem(kit, part.logo.as_ref(), swatch)
}
