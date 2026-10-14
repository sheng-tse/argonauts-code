//! By model, built as By CLI is: every model together, a card for each model with the roles
//! it filled as bars, and the models that did the same job side by side.

use std::collections::BTreeMap;

use argonauts_core::crew_stats::StatsRole;
use argonauts_core::settings::stats::StatsMeasure;
use ratatui::text::Span;

use super::cli::{SQUARES, Stats, cost_each, model_style, outcome_row, took};
use super::data::{Cli, DayPart, Model, cache_hit, most, written_a_second};
use super::draw::{self, Fared};
use super::grid::{self, Body, Layout};
use super::together::Part;
use crate::format;
use crate::kit::bars::fit_keeping_bars;
use crate::kit::{Drawn, Frame, Header, Kit, fit, spread};
use crate::text::{spans_width, truncate_spans};
use crate::theme::Role;

/// Cells a card gives a role's swatch and name: the longest, `orchestrator`, and a blank.
const ROLE_NAME: usize = 15;
/// Cells of the bar beside each role or model on a card.
pub(super) const PART_BAR: usize = 4;
/// The side-by-side table's columns before what came back most: the role, the model, how its
/// work fared, and the time, cost and tokens of each piece.
const COLUMNS: [usize; 6] = [14, 19, 18, 12, 12, 13];

impl Stats<'_> {
    /// Every model together: the period's chart stacked by model, beside what each used.
    pub fn all_models(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        let parts: Vec<Part<'_>> = self
            .crew
            .models()
            .map(|(cli, model)| Part {
                name: draw::model_name(&model.name),
                style: model_style(kit, cli, model),
                spent: model.spent,
                took_secs: model.took_secs(),
                holds: Box::new(move |part: &DayPart| model.is(&part.agent, part.model.as_deref())),
            })
            .collect();
        self.together(kit, frame, "all models", "model", &parts)
    }

    /// A card for each model, `picked` the one the keys are on, and how they're laid out.
    pub fn model_cards(
        &self,
        kit: &Kit<'_>,
        width: usize,
        picked: Option<usize>,
    ) -> (Vec<Drawn>, Layout) {
        let layout = Layout::of(width, self.crew.models().count());
        let bodies = self
            .crew
            .models()
            .enumerate()
            .map(|(at, (cli, model))| self.model_body(kit, cli, model, layout.inner(at)))
            .collect();
        (grid::draw(kit, layout, bodies, picked), layout)
    }

    /// One model: its tokens, its CLI and spend, a bar for each role it filled, how its tasks
    /// and its reviews fared, its pace over the period, and how much it read from cache and
    /// how fast it wrote.
    fn model_body(&self, kit: &Kit<'_>, cli: &Cli, model: &Model, inner: usize) -> Body {
        let style = model_style(kit, cli, model);
        let tokens = vec![kit.strong(self.tokens(model.tokens())), kit.dim(" tok")];
        let mut title = draw::swatch(kit, style);
        title.push(kit.strong(model.name.clone()));
        let room = inner.saturating_sub(spans_width(&tokens) + 1);
        let spend = vec![kit.dim(format::spend(&model.spent))];
        let room_on = inner.saturating_sub(spans_width(&spend) + 1);
        let on = truncate_spans(&[kit.dim(cli.name.clone())], room_on);
        let head = vec![
            spread(truncate_spans(&title, room), tokens, inner),
            spread(on, spend, inner),
            Vec::new(),
        ];

        let bars = model
            .filled()
            .into_iter()
            .map(|role| {
                let color = draw::role_style(kit, role);
                let used = model.spent_in(role).total_tokens();
                let mut row = draw::swatch(kit, color);
                row.push(Span::raw(role.plural()));
                let mut row = fit(row, ROLE_NAME);
                row.extend(draw::thin(kit, used, model.tokens(), PART_BAR, color));
                row.push(kit.dim(format!(" {} · {}", self.tokens(used), model.did(role))));
                fit_keeping_bars(row, inner)
            })
            .collect();

        let work = self.crew.series(StatsMeasure::Tasks, |part| {
            model.is(&part.agent, part.model.as_deref())
        });
        let top = self.crew.fullest(StatsMeasure::Tasks, |part| {
            Some((part.agent.clone(), part.model.clone()))
        });
        let mut pace = draw::spark(kit, &work, top, style);
        let mut words = format!(" {}", self.span_words());
        if let Some(role) = model.main_role()
            && let Some(each) = each_took(model, role)
        {
            words.push_str(&format!(" · {each} {}", a_piece(role)));
        }
        pace.push(kit.dim(words));
        let rest = vec![
            Vec::new(),
            outcome_row(kit, "tasks", model.fared, "first pass"),
            outcome_row(kit, "reviews", model.reviewed(), "accepted"),
            truncate_spans(&pace, inner),
            truncate_spans(&[kit.dim(reading_and_writing(model))], inner),
        ];
        Body { head, bars, rest }
    }

    /// The models that filled the same role, side by side: how each one's work fared, the
    /// time, the cost and the tokens of a piece of it, and what came back most. Nothing when
    /// no role had two models.
    pub fn same_job(&self, kit: &Kit<'_>, frame: Frame) -> Option<Drawn> {
        let inner = frame.inner();
        let shared: Vec<(StatsRole, Vec<(&Cli, &Model)>)> = self
            .crew
            .filled()
            .into_iter()
            .map(|role| {
                // Only the models that did the job compare at it.
                let did: Vec<_> = (self.crew.in_role(role).into_iter())
                    .filter(|(_, model)| model.did(role) > 0)
                    .collect();
                (role, did)
            })
            .filter(|(_, models)| models.len() > 1)
            .collect();
        if shared.is_empty() {
            return None;
        }
        let mut drawn = Drawn::new();
        let left = Header::new("the same job, side by side", Role::Question).left(kit);
        let note = vec![kit.dim("every role two or more models filled · enter opens a model")];
        let room = inner.saturating_sub(spans_width(&left) + 2);
        drawn.push(spread(left, truncate_spans(&note, room), inner));
        for (role, models) in shared {
            let (fared_word, why_word) = compared_by(role);
            drawn.push(Vec::new());
            let titles = [
                "",
                "model",
                fared_word,
                "time each",
                "cost each",
                "tokens each",
            ]
            .map(|title| vec![kit.dim(title)]);
            drawn.push(table_row(titles, vec![kit.dim(why_word)], inner));
            for (k, (cli, model)) in models.iter().enumerate() {
                let mut name = draw::swatch(kit, model_style(kit, cli, model));
                name.push(Span::raw(model.name.clone()));
                let done = model.did(role);
                let spent = model.spent_in(role);
                let fared = fared_in(model, role);
                let mut how = Vec::new();
                if fared.total() > 0 {
                    how = draw::tenths(kit, fared, SQUARES);
                    how.push(Span::raw(" "));
                    how.push(kit.strong(format::percent(fared.first_pass())));
                }
                let each = |words: Option<String>| vec![Span::raw(words.unwrap_or_default())];
                let cells = [
                    vec![Span::raw(if k == 0 { role.plural() } else { "" })],
                    truncate_spans(&name, COLUMNS[1] - 1),
                    how,
                    each(each_took(model, role)),
                    each((done > 0).then(|| cost_each(spent.cost_usd, done))),
                    each(
                        (done > 0).then(|| format::tokens(spent.total_tokens() / u64::from(done))),
                    ),
                ];
                let why = most(came_back(model, role)).unwrap_or_default();
                drawn.push(table_row(cells, vec![kit.dim(why.to_owned())], inner));
            }
        }
        Some(frame.draw(kit, drawn))
    }
}

/// The table's cells in their columns, then `last`, cut at `inner` with its squares whole.
fn table_row(
    cells: [Vec<Span<'static>>; 6],
    last: Vec<Span<'static>>,
    inner: usize,
) -> Vec<Span<'static>> {
    let mut row = Vec::new();
    for (cell, width) in cells.into_iter().zip(COLUMNS) {
        row.extend(fit(cell, width));
    }
    row.extend(last);
    fit_keeping_bars(row, inner)
}

/// The words over how each model's work in `role` fared, and over what came back most.
fn compared_by(role: StatsRole) -> (&'static str, &'static str) {
    match role {
        StatsRole::Worker => ("first pass", "sent back most for"),
        StatsRole::Reviewer => ("accepted", "caught most"),
        StatsRole::Judge => ("passed", "refused most for"),
        StatsRole::Main | StatsRole::Orchestrator | StatsRole::Merger => ("", ""),
    }
}

/// How the model's work in `role` fared: its tasks in review, or its reviews and sign-offs
/// that let the work through.
fn fared_in(model: &Model, role: StatsRole) -> Fared {
    match role {
        StatsRole::Worker => model.fared,
        StatsRole::Reviewer | StatsRole::Judge => model.accepted_as(role),
        StatsRole::Main | StatsRole::Orchestrator | StatsRole::Merger => Fared::default(),
    }
}

/// Why the model's tasks came back, or why it sent others' back, by how often.
fn came_back(model: &Model, role: StatsRole) -> Option<&BTreeMap<String, u32>> {
    match role {
        StatsRole::Worker => Some(&model.returned_for),
        role => model.reviews.get(&role).map(|tally| &tally.reasons),
    }
}

/// The time one piece of the model's work in `role` took, when it did any.
pub(super) fn each_took(model: &Model, role: StatsRole) -> Option<String> {
    let (done, secs) = (model.did(role), model.took_in(role));
    (done > 0 && secs > 0).then(|| took(secs / u64::from(done)))
}

/// `a task`, `a review`: one piece of a role's work, after its time or cost. A conversation
/// is too long a word beside a sparkline, so there it's `each`.
pub(super) fn a_piece(role: StatsRole) -> &'static str {
    match role {
        StatsRole::Main => "each",
        StatsRole::Orchestrator => "a plan",
        StatsRole::Worker => "a task",
        StatsRole::Reviewer => "a review",
        StatsRole::Judge => "a sign-off",
        StatsRole::Merger => "a merge",
    }
}

/// `cache hit 71% · 58 tok/s out`: the share of its input it read from cache, and the tokens
/// it wrote in each second its sessions ran.
fn reading_and_writing(model: &Model) -> String {
    let mut words = Vec::new();
    if let Some(hit) = cache_hit(&model.spent) {
        words.push(format!("cache hit {}", format::percent(hit)));
    }
    if let Some(speed) = written_a_second(&model.spent, model.took_secs()) {
        words.push(format!("{speed} tok/s out"));
    }
    words.join(" · ")
}
