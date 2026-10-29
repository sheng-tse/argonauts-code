//! By role, built as By CLI is: every role together, a card for each role with the models
//! that filled it as bars, and what each role caught or changed.

use std::collections::BTreeMap;

use argonauts_core::Spend;
use argonauts_core::crew_stats::StatsRole;
use ratatui::style::Style;
use ratatui::text::Span;

use super::by_model::{PART_BAR, a_piece};
use super::cli::{
    Stats, cost_each, count, model_fill, model_style, outcome_label, outcome_row, took,
};
use super::data::{Cli, DayPart, Model, most};
use super::draw::{self, Fared, Fill, Legend};
use super::grid::{self, Body, Layout};
use super::table::{Counted, Table};
use super::together::{Part, Together};
use crate::format;
use crate::kit::bars::fit_keeping_bars;
use crate::kit::{Drawn, Frame, Header, Kit, fit, spread};
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;

/// The fewest cells a card gives a model's swatch and name, and the most, with one blank
/// before its bar: a longer name is cut with `…`.
const LEAST_NAME: usize = 16;
const MOST_NAME: usize = draw::MODEL_NAME + 3;
/// The caught-or-changed card's column of role names.
const ROLE_COLUMN: usize = 16;
/// The colours of the reasons work was sent back for, the most common first.
const REASONS: [Role; 4] = [Role::Warn, Role::Danger, Role::AgentBlue, Role::Dim];

/// A role with the models that filled it, and what they did in it together.
struct InRole<'a> {
    role: StatsRole,
    /// The models that did its work. What it spent and took counts every model in it.
    models: Vec<(&'a Cli, &'a Model)>,
    spent: Spend,
    done: u32,
    took_secs: u64,
}

impl Stats<'_> {
    fn in_role(&self, role: StatsRole) -> InRole<'_> {
        let spent_in = self.crew.in_role(role);
        InRole {
            role,
            spent: spent_in.iter().fold(Spend::default(), |sum, (_, model)| {
                sum.plus(&model.spent_in(role))
            }),
            done: spent_in.iter().map(|(_, model)| model.did(role)).sum(),
            took_secs: spent_in.iter().map(|(_, model)| model.took_in(role)).sum(),
            models: (spent_in.into_iter())
                .filter(|(_, model)| model.did(role) > 0)
                .collect(),
        }
    }

    /// Every role as the table reads it, in the order By role lists them.
    pub(super) fn role_counted(&self) -> Vec<Counted> {
        (self.crew.filled().into_iter())
            .map(|role| {
                let all = self.in_role(role);
                Counted {
                    name: role.plural().to_owned(),
                    search: role.plural().to_owned(),
                    spent: all.spent,
                    took_secs: all.took_secs,
                }
            })
            .collect()
    }

    /// Every role together: three bars of what each used over a table of them, beside the
    /// period's chart stacked by role.
    pub fn all_roles(&self, kit: &Kit<'_>, frame: Frame, table: &Table, in_table: bool) -> Drawn {
        let roles = self.crew.filled();
        let counted = self.role_counted();
        let parts: Vec<Part<'_>> = (roles.iter().zip(counted))
            .enumerate()
            .map(|(at, (role, counted))| {
                let role = *role;
                Part {
                    counted,
                    fill: draw::role_style(kit, role).into(),
                    logo: None,
                    group: 0,
                    folds: false,
                    rank: at,
                    stack: at,
                    holds: Box::new(move |part: &DayPart| part.role == Some(role)),
                }
            })
            .collect();
        let stacks: Vec<Fill> = parts.iter().map(|part| part.fill).collect();
        let look = Together {
            title: "all roles",
            by: "role",
            parts: &parts,
            stacks: &stacks,
            table,
            in_table,
        };
        self.together(kit, frame, look)
    }

    /// A card for each role, `picked` the one the keys are on, and how they're laid out.
    pub fn role_cards(
        &self,
        kit: &Kit<'_>,
        width: usize,
        picked: Option<usize>,
    ) -> (Vec<Drawn>, Layout) {
        let roles = self.crew.filled();
        let layout = Layout::of(width, roles.len());
        let names = self
            .crew
            .models()
            .map(|(_, model)| str_width(&model.name) + 3)
            .max()
            .unwrap_or(0)
            .clamp(LEAST_NAME, MOST_NAME);
        let bodies = roles
            .into_iter()
            .enumerate()
            .map(|(at, role)| {
                let columns = (names, outcome_label(layout.least_inner()));
                self.role_body(kit, &self.in_role(role), columns, layout.inner(at))
            })
            .collect();
        (grid::draw(kit, layout, bodies, picked), layout)
    }

    /// One role: its tokens, what it did and its spend, a bar for each model that filled it,
    /// how its work fared and what it caught, its pace over the period, and who was best at
    /// it or what a piece of it cost.
    fn role_body(
        &self,
        kit: &Kit<'_>,
        all: &InRole<'_>,
        (names, labels): (usize, usize),
        inner: usize,
    ) -> Body {
        let role = all.role;
        let color = draw::role_style(kit, role);
        let used = all.spent.total_tokens();
        let tokens = vec![kit.strong(self.tokens(used)), kit.dim(" tok")];
        let mut title = draw::swatch(kit, color);
        title.push(kit.strong(role.plural()));
        let spend = vec![kit.dim(format::spend(&all.spent))];
        let did = format!(
            "{} · {}",
            count(all.done, role.unit()),
            count(all.models.len() as u32, "model")
        );
        let room = inner.saturating_sub(spans_width(&spend) + 1);
        let head = vec![
            spread(title, tokens, inner),
            spread(truncate_spans(&[kit.dim(did)], room), spend, inner),
            Vec::new(),
        ];

        let bars = all
            .models
            .iter()
            .map(|(cli, model)| {
                let style = model_style(kit, cli, model);
                let tokens = model.spent_in(role).total_tokens();
                let mut row = draw::swatch(kit, model_fill(kit, cli, model));
                row.push(Span::raw(model.name.clone()));
                // A name cut to its column keeps a blank before the bar.
                let mut row = fit(truncate_spans(&row, names - 1), names);
                row.extend(draw::thin(kit, tokens, used, PART_BAR, style));
                row.push(kit.dim(format!(" {} · {}", self.tokens(tokens), model.did(role))));
                fit_keeping_bars(row, inner)
            })
            .collect();

        let work = self.crew.done_series(|part| part.role == Some(role));
        let top = self.crew.fullest_done(|part| part.role);
        let mut pace = draw::spark(kit, &work, top, color);
        let mut words = format!(" {}", self.span_words());
        if all.done > 0 && all.took_secs > 0 {
            let each = took(all.took_secs / u64::from(all.done));
            words.push_str(&format!(" · {each} {}", a_piece(role)));
        }
        pace.push(kit.dim(words));

        let [fared, caught] = self.role_facts(kit, all, (labels, inner));
        let rest = vec![
            Vec::new(),
            fared,
            caught,
            truncate_spans(&pace, inner),
            truncate_spans(&[kit.dim(self.role_note(all))], inner),
        ];
        Body { head, bars, rest }
    }

    /// The two rows under a role's bars, on a card `inner` cells across: how its work fared,
    /// and what it caught or passed on.
    fn role_facts(
        &self,
        kit: &Kit<'_>,
        all: &InRole<'_>,
        (labels, inner): (usize, usize),
    ) -> [Vec<Span<'static>>; 2] {
        let crew = self.crew;
        let outcomes = &crew.outcomes;
        let back: u32 = all
            .models
            .iter()
            .map(|(_, model)| model.back_in(all.role))
            .sum();
        let accepted = Fared {
            first: all.done.saturating_sub(back),
            once: back.min(all.done),
            more: 0,
        };
        match all.role {
            StatsRole::Main => {
                let turns: u32 = all.models.iter().map(|(_, model)| model.turns).sum();
                // `61 in 9 · 7 a conversation`, and on a card too narrow for that
                // `61 in 9 · 7 each`: the count of conversations stays on the row.
                let in_conversations = |each: &str| match all.done {
                    0 => String::new(),
                    done => format!(
                        " in {done} · {:.0} {each}",
                        f64::from(turns) / f64::from(done)
                    ),
                };
                let in_full = fact(
                    kit,
                    ("turns", labels),
                    turns,
                    &in_conversations("a conversation"),
                );
                let turns = if spans_width(&in_full) <= inner {
                    in_full
                } else {
                    fact(kit, ("turns", labels), turns, &in_conversations("each"))
                };
                let runs = if crew.workflows == 1 { "run" } else { "runs" };
                [
                    turns,
                    fact(
                        kit,
                        ("handed", labels),
                        crew.workflows,
                        &format!(" {runs} to the crew"),
                    ),
                ]
            }
            StatsRole::Orchestrator => {
                // Of the runs whose ending is known.
                let merged = outcomes.runs_merged.min(outcomes.runs);
                let runs = Fared {
                    first: merged,
                    once: outcomes.runs - merged,
                    more: 0,
                };
                let tasks = match all.done {
                    0 => String::new(),
                    plans => format!(
                        " · {:.1} tasks a plan",
                        f64::from(crew.did(StatsRole::Worker)) / f64::from(plans)
                    ),
                };
                [
                    outcome_row(kit, ("runs", labels), runs, "merged"),
                    fact(kit, ("plans", labels), all.done, &tasks),
                ]
            }
            StatsRole::Worker => {
                let returns: u32 = all.models.iter().map(|(_, model)| model.returns).sum();
                let why = most(Some(&crew.returned_for()))
                    .map(|why| format!(" · most: {why}"))
                    .unwrap_or_default();
                [
                    outcome_row(kit, ("tasks", labels), crew.fared, "first pass"),
                    fact(kit, ("returns", labels), returns, &why),
                ]
            }
            StatsRole::Reviewer => {
                let tasks = if back == 1 { "task" } else { "tasks" };
                [
                    outcome_row(kit, ("reviews", labels), accepted, "accepted"),
                    fact(
                        kit,
                        ("caught", labels),
                        back,
                        &format!(" {tasks} to send back"),
                    ),
                ]
            }
            StatsRole::Judge => {
                let (worker, reviewer) = (outcomes.sided_with_worker, outcomes.sided_with_reviewer);
                let sided = match (worker, reviewer) {
                    (0, 0) => String::new(),
                    (1, 1) => " · one each way".to_owned(),
                    (worker, _) => format!(" · {worker} for the worker"),
                };
                [
                    outcome_row(kit, ("signed", labels), accepted, "passed"),
                    fact(kit, ("rulings", labels), outcomes.rulings(), &sided),
                ]
            }
            StatsRole::Merger => {
                let merges = Fared {
                    first: outcomes.merges_clean,
                    once: outcomes.merges_settled,
                    more: outcomes.merges_failed,
                };
                let conflicts = if outcomes.merges_settled == 1 {
                    "conflict"
                } else {
                    "conflicts"
                };
                let words = format!(" {conflicts} · {} failed", outcomes.merges_failed);
                [
                    outcome_row(kit, ("merges", labels), merges, "clean"),
                    fact(kit, ("settled", labels), outcomes.merges_settled, &words),
                ]
            }
        }
    }

    /// A role's last row: who was best at it when several models filled it, else its model
    /// and what a piece of its work cost. The orchestrator's says how often it planned again.
    fn role_note(&self, all: &InRole<'_>) -> String {
        let role = all.role;
        let each = (all.done > 0).then(|| {
            format!(
                "{} {}",
                cost_each(all.spent.cost_usd, all.done),
                cost_word(role)
            )
        });
        let who = match (role, all.models.as_slice()) {
            (StatsRole::Orchestrator, _) if self.crew.outcomes.runs == 0 => {
                count(all.models.len() as u32, "model")
            }
            (StatsRole::Orchestrator, _) => match self.crew.outcomes.runs_replanned {
                0 => "never replanned".to_owned(),
                1 => "replanned once".to_owned(),
                2 => "replanned twice".to_owned(),
                n => format!("replanned {n} times"),
            },
            (_, [(_, model)]) => model.name.clone(),
            (StatsRole::Worker, models) => {
                return best(models, |model| model.fared)
                    .map(|(model, share)| format!("best first pass: {}, {share}", model.name))
                    .unwrap_or_default();
            }
            (StatsRole::Reviewer, models) => {
                return best(models, |model| model.accepted_as(role))
                    .map(|(model, _)| format!("most accepted: {}", model.name))
                    .unwrap_or_default();
            }
            (_, models) => count(models.len() as u32, "model"),
        };
        match each {
            Some(each) => format!("{who} · {each}"),
            None => who,
        }
    }

    /// What each role caught or changed, a bar each and a table of its parts under it, every
    /// role's in the same columns, then what they are parts of: the runs the orchestrator
    /// planned again, why the reviewers sent work back, whom the judge sided with, and the
    /// conflicts the merger settled. Nothing when no role has any of it.
    pub fn caught_or_changed(&self, kit: &Kit<'_>, frame: Frame) -> Option<Drawn> {
        let inner = frame.inner();
        let groups: Vec<Caught> = self
            .crew
            .filled()
            .into_iter()
            .filter_map(|role| self.caught(kit, &self.in_role(role)))
            .collect();
        if groups.is_empty() {
            return None;
        }
        let mut drawn = Drawn::new();
        let left = Header::new("what each role caught or changed", Role::Question).left(kit);
        let note = vec![kit.dim("every send-back, ruling and conflict · enter opens a role")];
        let room = inner.saturating_sub(spans_width(&left) + 2);
        drawn.push(spread(left, truncate_spans(&note, room), inner));
        let bar = inner.saturating_sub(ROLE_COLUMN);
        let tables: Vec<Vec<Legend>> = (groups.iter())
            .map(|group| {
                let whole: u32 = group.parts.iter().map(|(n, _, _)| n).sum();
                let row = |(n, style, name): &(u32, Style, String)| {
                    let n = u64::from(*n);
                    Legend::of(*style, name, (n, n, u64::from(whole)), n.to_string())
                };
                group.parts.iter().map(row).collect()
            })
            .collect();
        let columns = draw::table_columns(tables.iter().flatten(), bar, false);
        let indent = || Span::raw(" ".repeat(ROLE_COLUMN));
        for (group, table) in groups.iter().zip(&tables) {
            drawn.push(Vec::new());
            let parts: Vec<(u64, Fill)> = (table.iter())
                .map(|row| (row.part.value, row.part.fill))
                .collect();
            let mut row = fit(vec![Span::raw(group.role.plural())], ROLE_COLUMN);
            row.extend(draw::stacked(kit, &parts, bar));
            drawn.push(row);
            for line in draw::table(kit, table, columns, false) {
                let mut row = vec![indent()];
                row.extend(line);
                drawn.push(row);
            }
            let mut row = vec![indent()];
            row.extend(draw::table_note(kit, columns, group.of.clone(), bar));
            drawn.push(row);
        }
        Some(frame.draw(kit, drawn))
    }

    /// What `all`'s role caught or changed, when it's a role that does and it did anything.
    fn caught(&self, kit: &Kit<'_>, all: &InRole<'_>) -> Option<Caught> {
        let crew = self.crew;
        let outcomes = &crew.outcomes;
        let part = |n: u32, role: Role, name: &str| (n, kit.fg(role), name.to_owned());
        let (parts, of) = match all.role {
            StatsRole::Orchestrator if outcomes.runs > 0 => {
                let again = outcomes.runs_replanned.min(outcomes.runs);
                (
                    vec![
                        part(outcomes.runs - again, Role::Success, "ran as planned"),
                        part(again, Role::Warn, "replanned"),
                    ],
                    format!("of {}", count(outcomes.runs, "run")),
                )
            }
            StatsRole::Reviewer if all.done > 0 => {
                let mut reasons: BTreeMap<&str, u32> = BTreeMap::new();
                for (_, model) in &all.models {
                    let tally = model.reviews.get(&all.role);
                    for (why, n) in tally.iter().flat_map(|tally| &tally.reasons) {
                        *reasons.entry(why).or_default() += n;
                    }
                }
                let mut reasons: Vec<(&str, u32)> = reasons.into_iter().collect();
                reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                let named: u32 = reasons.iter().map(|(_, n)| n).sum();
                let back: u32 = all
                    .models
                    .iter()
                    .map(|(_, model)| model.back_in(all.role))
                    .sum::<u32>()
                    .max(named);
                let mut parts: Vec<(u32, Style, String)> = reasons
                    .into_iter()
                    .enumerate()
                    .map(|(k, (why, n))| part(n, REASONS[k % REASONS.len()], why))
                    .collect();
                // A review filed without its reason still sent work back.
                parts.push(part(back - named, Role::Dim, "no reason kept"));
                let of = match back {
                    0 => "nothing sent back".to_owned(),
                    back => format!("of {back} sent back"),
                };
                (parts, of)
            }
            StatsRole::Judge if all.done > 0 || outcomes.rulings() > 0 => {
                let refused: u32 = all
                    .models
                    .iter()
                    .map(|(_, model)| model.back_in(all.role))
                    .sum();
                let refused = match refused {
                    0 => "none refused".to_owned(),
                    refused => format!("{refused} refused"),
                };
                (
                    vec![
                        (
                            outcomes.sided_with_worker,
                            draw::role_style(kit, StatsRole::Worker),
                            "sided with the worker".to_owned(),
                        ),
                        (
                            outcomes.sided_with_reviewer,
                            draw::role_style(kit, StatsRole::Reviewer),
                            "with the reviewer".to_owned(),
                        ),
                    ],
                    format!(
                        "of {} · {}, {refused}",
                        count(outcomes.rulings(), "ruling"),
                        count(all.done, all.role.unit())
                    ),
                )
            }
            StatsRole::Merger if outcomes.merges() > 0 => {
                let checks = match outcomes.checks_failed {
                    0 => "checks passed after every one".to_owned(),
                    failed => format!("checks failed after {failed}"),
                };
                (
                    vec![
                        part(outcomes.merges_clean, Role::Success, "no conflict"),
                        part(outcomes.merges_settled, Role::Warn, "settled a conflict"),
                        part(outcomes.merges_failed, Role::Danger, "couldn't settle"),
                    ],
                    format!("of {} · {checks}", count(outcomes.merges(), "merge")),
                )
            }
            _ => return None,
        };
        Some(Caught {
            role: all.role,
            parts,
            of,
        })
    }
}

/// A role's row of what it caught or changed: each part's count, colour and name, and what
/// they are parts of.
struct Caught {
    role: StatsRole,
    parts: Vec<(u32, Style, String)>,
    of: String,
}

/// `returns  6 · most: a test missing`: a label, a count, and what the count is of. A count
/// of nothing says `none`.
fn fact(kit: &Kit<'_>, (label, width): (&str, usize), n: u32, words: &str) -> Vec<Span<'static>> {
    let mut row = vec![kit.dim(format!("{label:<width$}"))];
    if n == 0 {
        row.push(kit.dim("none"));
        return row;
    }
    row.push(kit.strong(n.to_string()));
    if !words.is_empty() {
        row.push(kit.dim(words.to_owned()));
    }
    row
}

/// The model whose work fared best, with its share, among those with any work to judge.
fn best<'a>(
    models: &[(&'a Cli, &'a Model)],
    fared: impl Fn(&Model) -> Fared,
) -> Option<(&'a Model, String)> {
    models
        .iter()
        .map(|(_, model)| (*model, fared(model)))
        .filter(|(_, fared)| fared.total() > 0)
        .max_by(|a, b| {
            a.1.first_pass()
                .total_cmp(&b.1.first_pass())
                .then(a.1.total().cmp(&b.1.total()))
        })
        .map(|(model, fared)| (model, format::percent(fared.first_pass())))
}

/// What a piece of a role's work is called after its cost. The judge's is `each`, which
/// leaves its model's name room.
fn cost_word(role: StatsRole) -> &'static str {
    match role {
        StatsRole::Main => "a conversation",
        StatsRole::Judge => "each",
        role => a_piece(role),
    }
}
