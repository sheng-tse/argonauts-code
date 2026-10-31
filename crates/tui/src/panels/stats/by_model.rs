//! By model, built as By CLI is: every model together, a card for each model with the roles
//! it filled as bars, and the models that did the same job side by side.

use std::collections::BTreeMap;

use argonauts_core::behaviors::crew_stats::StatsRole;
use argonauts_core::look::display_names::{on_cli, run_by_several};
use argonauts_core::look::logos::model_logo;
use argonauts_core::settings::stats::StatsMeasure;
use ratatui::text::Span;

use super::cli::{
    SQUARES, Stats, cost_each, count, model_fill, model_style, outcome_label, outcome_row, took,
};
use super::data::{Cli, DayPart, Model, cache_hit, most, written_a_second};
use super::draw::{self, Fared, Fill};
use super::grid::{self, Body, Layout};
use super::table::{Counted, Table};
use super::together::{Part, Together};
use crate::base::format;
use crate::base::kit::bars::fit_keeping_bars;
use crate::base::kit::logo::{ModelLabel, NoPicture};
use crate::base::kit::{Drawn, Frame, Header, Kit, fit, ljust, rjust, spread};
use crate::base::text::{spans_width, truncate_spans};
use crate::base::theme::Role;
use crate::base::theme::shades::OWN_TEXTURES;

/// Cells a card gives a role's swatch and name: the square and the blank the terminal shows
/// after it, the longest name, `orchestrator`, and a blank.
const ROLE_NAME: usize = 3 + "orchestrator".len() + 1;
/// Cells of the bar beside each role or model on a card.
pub(super) const PART_BAR: usize = 4;
/// The side-by-side table's columns before what came back most: the role, the model, how its
/// work fared, and the time, cost and tokens of each piece.
const COLUMNS: [usize; 6] = [14, 19, 18, 12, 12, 13];
/// Cards for at most this many models, the ones with the most tokens. Every other model gets
/// a line in the box under them.
pub(super) const CARDS: usize = 6;
/// Cells before a line's name: the pointer, the model's logo, a blank, its swatch two cells
/// wide and two blanks, as in the table of all models.
const LINE_BEFORE_NAME: usize = 2 + 7;
/// A line's columns after its name, each with the blanks before it: what it did, its ten
/// squares and its share, the time a piece took, its sparkline, and its tokens, spend and
/// cache hit. A narrow box leaves out the time, the sparkline and the cache hit.
const LINE_DID: usize = 2 + 12;
const LINE_PASSED: usize = 2 + SQUARES + 1 + 4;
const LINE_EACH: usize = 2 + 7;
const LINE_WEEKS: usize = 2 + 10;
const LINE_TOKENS: usize = 2 + 6;
const LINE_SPEND: usize = 2 + 7;
const LINE_CACHE: usize = 2 + 5;
/// The narrowest a line's name is before the box leaves out its time, sparkline and cache hit.
const LINE_NAME: usize = 24;

impl Stats<'_> {
    /// Every model as the table reads it, in the bars' order.
    pub(super) fn model_counted(&self) -> Vec<Counted> {
        (self.crew.models())
            .map(|(cli, model)| Counted::model(cli, model))
            .collect()
    }

    /// Every model together: three bars of what each used, each CLI's models side by side,
    /// over a table of them, beside the period's chart stacked by CLI.
    pub fn all_models(&self, kit: &Kit<'_>, frame: Frame, table: &Table, in_table: bool) -> Drawn {
        let clis = &self.crew.clis;
        let parts: Vec<Part<'_>> = self
            .crew
            .models()
            .map(|(cli, model)| {
                let group = (clis.iter())
                    .position(|other| other.agent == cli.agent)
                    .unwrap_or(0);
                Part {
                    counted: Counted::model(cli, model),
                    fill: model_fill(kit, cli, model),
                    logo: Some(model_logo(&cli.agent, model.model.as_deref())),
                    group,
                    folds: model.place >= OWN_TEXTURES,
                    rank: model.place,
                    stack: group,
                    holds: Box::new(move |part: &DayPart| {
                        model.is(&part.agent, part.model.as_deref())
                    }),
                }
            })
            .collect();
        let stacks: Vec<Fill> = clis.iter().map(|cli| kit.fg(cli.role()).into()).collect();
        let look = Together {
            title: "all models",
            by: "model",
            parts: &parts,
            stacks: &stacks,
            table,
            in_table,
        };
        self.together(kit, frame, look)
    }

    /// A card for each of `shown`, models by their place in the bars' order, `picked` the one
    /// the keys are on, and how they're laid out.
    pub fn model_cards(
        &self,
        kit: &Kit<'_>,
        width: usize,
        shown: &[usize],
        picked: Option<usize>,
    ) -> (Vec<Drawn>, Layout) {
        let models: Vec<(&Cli, &Model)> = self.crew.models().collect();
        let layout = Layout::of(width, shown.len());
        let bodies = (shown.iter().enumerate())
            .map(|(at, part)| {
                let (cli, model) = models[*part];
                let labels = outcome_label(layout.least_inner());
                self.model_body(kit, cli, model, (labels, layout.inner(at)))
            })
            .collect();
        let picked = picked.and_then(|part| shown.iter().position(|at| *at == part));
        (grid::draw(kit, layout, bodies, picked), layout)
    }

    /// The models with no card, a line each, holding what a card holds: what the model did in
    /// its main role and how that work passed, the time a piece took, its weeks, and its
    /// tokens, spend and cache hit. `pointed` is the model the table's keys are on. Nothing
    /// when every model has a card.
    pub fn every_other_model(
        &self,
        kit: &Kit<'_>,
        frame: Frame,
        lines: &[usize],
        pointed: Option<usize>,
    ) -> Option<Drawn> {
        if lines.is_empty() {
            return None;
        }
        let inner = frame.inner();
        let full = LINE_DID + LINE_PASSED + LINE_EACH + LINE_WEEKS + LINE_TOKENS;
        let full = full + LINE_SPEND + LINE_CACHE;
        let roomy = inner >= LINE_BEFORE_NAME + LINE_NAME + full;
        let right = if roomy {
            full
        } else {
            LINE_DID + LINE_PASSED + LINE_TOKENS + LINE_SPEND
        };
        let name = inner.saturating_sub(LINE_BEFORE_NAME + right);
        let mut drawn = Drawn::new();
        let left = Header::new("every other model, one line each", Role::Question).left(kit);
        let note = vec![kit.dim(format!(
            "the top {CARDS} have cards above · enter opens one"
        ))];
        let room = inner.saturating_sub(spans_width(&left) + 2);
        drawn.push(spread(left, truncate_spans(&note, room), inner));
        drawn.push(Vec::new());

        let mut titles = " ".repeat(LINE_BEFORE_NAME) + &ljust("model", name);
        titles += &rjust("did", LINE_DID);
        titles += &ljust("  passed", LINE_PASSED);
        if roomy {
            titles += &rjust("each", LINE_EACH);
            titles += &ljust("  weeks", LINE_WEEKS);
        }
        titles += &rjust("tokens", LINE_TOKENS);
        titles += &rjust("spend", LINE_SPEND);
        if roomy {
            titles += &rjust("cache", LINE_CACHE);
        }
        drawn.push(vec![kit.dim(titles)]);

        let models: Vec<(&Cli, &Model)> = self.crew.models().collect();
        let shared = run_by_several(
            (models.iter()).map(|(cli, model)| (cli.agent.as_str(), model.name.as_str())),
        );
        let top = self.crew.fullest(StatsMeasure::Tasks, |part| {
            Some((part.agent.clone(), part.model.clone()))
        });
        for part in lines {
            let (cli, model) = models[*part];
            let mut row = vec![if pointed == Some(*part) {
                kit.span(format!("{} ", kit.theme.glyphs.prompt), Role::Question)
            } else {
                Span::raw("  ")
            }];
            let logo = model_logo(&cli.agent, model.model.as_deref());
            let mut label = ModelLabel::named(&logo, &model.name, kit.theme, NoPicture::Dot);
            if shared.contains(model.name.as_str()) {
                label.name = on_cli(&label.name, &cli.name);
            }
            row.extend(label.logo);
            let fill = model_fill(kit, cli, model);
            let swatch = draw::swatch(kit, fill);
            let glyph = swatch.first().map(|cell| cell.content.to_string());
            row.push(Span::styled(
                glyph.unwrap_or_default().repeat(2),
                swatch.first().map(|cell| cell.style).unwrap_or_default(),
            ));
            row.push(Span::raw("  "));
            row.extend(fit(vec![Span::raw(label.name)], name));
            let role = model.main_role();
            let did = role.map(|role| count(model.did(role), role.unit()));
            row.push(Span::raw(rjust(&did.unwrap_or_default(), LINE_DID)));
            row.push(Span::raw("  "));
            let fared = role.map(|role| fared_in(model, role)).unwrap_or_default();
            if fared.total() > 0 {
                row.extend(draw::tenths(kit, fared, SQUARES));
                row.push(Span::raw(rjust(&format::percent(fared.first_pass()), 5)));
            } else {
                row.push(Span::raw(rjust("–", SQUARES + 5)));
            }
            if roomy {
                let each = role.and_then(|role| each_took(model, role));
                row.push(Span::raw(rjust(&each.unwrap_or_default(), LINE_EACH)));
                row.push(Span::raw("  "));
                let work = self.crew.series(StatsMeasure::Tasks, |part| {
                    model.is(&part.agent, part.model.as_deref())
                });
                let style = model_style(kit, cli, model);
                row.extend(fit(draw::spark(kit, &work, top, style), LINE_WEEKS - 2));
            }
            row.push(Span::raw(rjust(
                &self.table_tokens(model.tokens()),
                LINE_TOKENS,
            )));
            row.push(Span::raw(rjust(&format::spend(&model.spent), LINE_SPEND)));
            if roomy {
                let hit = cache_hit(&model.spent).map(format::percent);
                row.push(Span::raw(rjust(&hit.unwrap_or_default(), LINE_CACHE)));
            }
            drawn.push(fit_keeping_bars(row, inner));
        }
        Some(frame.draw(kit, drawn))
    }

    /// One model: its tokens, its CLI and spend, a bar for each role it filled, how its tasks
    /// and its reviews fared, its pace over the period, and how much it read from cache and
    /// how fast it wrote.
    fn model_body(
        &self,
        kit: &Kit<'_>,
        cli: &Cli,
        model: &Model,
        (labels, inner): (usize, usize),
    ) -> Body {
        let style = model_style(kit, cli, model);
        let tokens = vec![kit.strong(self.tokens(model.tokens())), kit.dim(" tok")];
        // The logo of whoever serves the model, where the terminal draws it, before its swatch.
        let logo = model_logo(&cli.agent, model.model.as_deref());
        let label = ModelLabel::named(&logo, &model.name, kit.theme, NoPicture::Nothing);
        let mut title = label.logo;
        title.extend(draw::swatch(kit, model_fill(kit, cli, model)));
        title.push(kit.strong(label.name));
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
            outcome_row(kit, ("tasks", labels), model.fared, "first pass"),
            outcome_row(kit, ("reviews", labels), model.reviewed(), "accepted"),
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
                let mut name = draw::swatch(kit, model_fill(kit, cli, model));
                let room = (COLUMNS[1] - 1).saturating_sub(spans_width(&name));
                name.push(Span::raw(draw::cut_name(&model.name, room)));
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
                    name,
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
