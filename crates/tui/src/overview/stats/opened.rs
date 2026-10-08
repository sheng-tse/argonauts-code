//! A model, a CLI or a role opened one level deeper with Enter: by role (or by model), by
//! kind of work, its own tokens, its days, the tasks it took over as a fallback, and its
//! plan.

use std::collections::BTreeMap;

use argonauts_core::Spend;
use argonauts_core::crew_stats::{CrewRole, Fallback};
use ratatui::text::Span;

use super::cli::{SQUARES, Stats, count, model_style, share, took};
use super::data::{Cli, Model, short_cli};
use super::draw::{self, Entry, Fared};
use crate::format;
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Frame, Kit, fit, side_by_side};

/// What Enter opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Cli(String),
    Model(String, Option<String>),
    Role(CrewRole),
}

/// Cells for a row's name in the left column: the longest role's word, `orchestrator`, and
/// a space.
const NAME: usize = 13;
/// Cells for what it did, such as `21 tasks`.
const DID: usize = 13;

impl Stats<'_> {
    /// The models `opened` covers, each with its CLI.
    fn covered(&self, opened: &Opened) -> Vec<(&Cli, &Model)> {
        self.crew
            .models()
            .filter(|(cli, model)| match opened {
                Opened::Cli(agent) => cli.agent == *agent,
                Opened::Model(agent, name) => model.is(agent, name.as_deref()),
                Opened::Role(role) => model.roles().contains(role),
            })
            .collect()
    }

    /// The opened card, or nothing when what was opened has no work in this period.
    pub fn opened(&self, kit: &Kit<'_>, frame: Frame, opened: &Opened) -> Option<Drawn> {
        let models = self.covered(opened);
        let (first_cli, first_model) = *models.first()?;
        let role = match opened {
            Opened::Role(role) => Some(*role),
            _ => None,
        };
        let inner = frame.inner();
        let spent_of = |model: &Model| match role {
            Some(role) => model.spent_as.get(&role).copied().unwrap_or_default(),
            None => model.spent,
        };
        let mut spent = Spend::default();
        for (_, model) in &models {
            spent = spent.plus(&spent_of(model));
        }

        let title: Vec<Span<'static>> = match opened {
            Opened::Model(..) => vec![
                Span::styled(kit.glyphs.swatch, model_style(kit, first_cli, first_model)),
                Span::raw(" "),
                kit.strong(first_model.name.clone()),
                kit.dim(format!(" on {} · opened with enter", first_cli.name)),
            ],
            Opened::Cli(_) => vec![
                kit.span("●", first_cli.role()),
                Span::raw(" "),
                kit.strong(first_cli.name.clone()),
                kit.dim(" · opened with enter"),
            ],
            Opened::Role(role) => vec![
                kit.span(kit.glyphs.widget, Role::Card),
                Span::raw(" "),
                kit.strong(role.plural()),
                kit.dim(" · opened with enter"),
            ],
        };
        let mut pieces: Vec<(String, &str)> = Vec::new();
        for each in [
            CrewRole::Worker,
            CrewRole::Reviewer,
            CrewRole::Judge,
            CrewRole::Orchestrator,
        ] {
            if role.is_some_and(|role| role != each) {
                continue;
            }
            let n: u32 = models.iter().map(|(_, model)| model.did(each)).sum();
            if n > 0 {
                let unit = match (each, n) {
                    (CrewRole::Worker, 1) => "task",
                    (CrewRole::Worker, _) => "tasks",
                    (CrewRole::Reviewer, 1) => "review",
                    (CrewRole::Reviewer, _) => "reviews",
                    (CrewRole::Judge, 1) => "sign-off",
                    (CrewRole::Judge, _) => "sign-offs",
                    (_, 1) => "workflow",
                    _ => "workflows",
                };
                pieces.push((n.to_string(), unit));
            }
        }
        pieces.push((self.tokens(spent.total_tokens()), "tokens"));
        pieces.push((format::spend(&spent), ""));
        let mut drawn = Drawn::new();
        drawn.push(super::cli::header_row(kit, title, &pieces, inner));

        let half = (inner.saturating_sub(4)) / 2;
        let right_at = half + 4;
        let left = self.opened_left(kit, &models, role, half);
        let right = self.opened_right(kit, &models, opened, &spent, inner - right_at);
        let body = side_by_side(left, right, right_at);
        drawn.append(body);
        Some(frame.draw(kit, drawn))
    }

    /// By role for one model or a CLI's, by model for a role, then by kind of work.
    fn opened_left(
        &self,
        kit: &Kit<'_>,
        models: &[(&Cli, &Model)],
        role: Option<CrewRole>,
        width: usize,
    ) -> Drawn {
        let mut drawn = Drawn::new();
        match role {
            Some(role) => {
                drawn.push(vec![kit.dim("by model")]);
                let names = models
                    .iter()
                    .map(|(_, m)| str_width(&m.name))
                    .max()
                    .unwrap_or(0);
                for (cli, model) in models {
                    let mut row = vec![
                        Span::styled(kit.glyphs.swatch, model_style(kit, cli, model)),
                        Span::raw(" "),
                    ];
                    row.extend(fit(vec![Span::raw(model.name.clone())], names.min(18) + 2));
                    row.extend(fit(
                        vec![Span::raw(count(model.did(role), role.unit()))],
                        DID,
                    ));
                    row.extend(review_words(kit, role, model));
                    drawn.push(truncate_spans(&row, width));
                }
            }
            None => {
                drawn.push(vec![kit.dim("by role")]);
                for each in CrewRole::ALL {
                    let n: u32 = models.iter().map(|(_, model)| model.did(each)).sum();
                    if n == 0 {
                        continue;
                    }
                    let mut row = fit(vec![Span::raw(each.word())], NAME);
                    row.extend(fit(vec![Span::raw(count(n, each.unit()))], DID));
                    let merged = Model {
                        fared: fared_of(models.iter().map(|(_, model)| model.fared)),
                        sent_back: [(
                            each,
                            models
                                .iter()
                                .map(|(_, m)| m.sent_back.get(&each).copied().unwrap_or(0))
                                .sum(),
                        )]
                        .into(),
                        took: [(
                            each,
                            models
                                .iter()
                                .map(|(_, m)| m.took.get(&each).copied().unwrap_or(0))
                                .sum(),
                        )]
                        .into(),
                        done: [(each, n)].into(),
                        ..Model::default()
                    };
                    row.extend(review_words(kit, each, &merged));
                    drawn.push(truncate_spans(&row, width));
                }
            }
        }
        // What it built, by kind.
        let mut kinds: BTreeMap<String, Fared> = BTreeMap::new();
        for (_, model) in models {
            for (kind, fared) in &model.kinds {
                let into = kinds.entry(kind.clone()).or_default();
                into.first += fared.first;
                into.once += fared.once;
                into.more += fared.more;
            }
        }
        if !kinds.is_empty() && role.is_none_or(|role| role == CrewRole::Worker) {
            drawn.push(vec![kit.dim("by kind of work, as a worker")]);
            let mut kinds: Vec<(String, Fared)> = kinds.into_iter().collect();
            kinds.sort_by_key(|(_, fared)| std::cmp::Reverse(fared.total()));
            let top = kinds.iter().map(|(_, f)| f.total()).max().unwrap_or(1);
            let style = models.first().map_or(kit.fg(Role::Card), |(cli, model)| {
                model_style(kit, cli, model)
            });
            for (kind, fared) in kinds {
                let mut row = fit(vec![Span::raw(kind)], NAME);
                row.extend(draw::meter(
                    kit,
                    u64::from(fared.total()),
                    u64::from(top),
                    12,
                    style,
                ));
                row.push(Span::raw(format!(" {:>3}", fared.total())));
                row.push(kit.dim(" · "));
                row.extend(draw::squares(kit, fared, SQUARES + 6));
                drawn.push(truncate_spans(&row, width));
            }
        }
        drawn
    }

    /// Its tokens and what they were, its days, the tasks it took over, and its plan.
    fn opened_right(
        &self,
        kit: &Kit<'_>,
        models: &[(&Cli, &Model)],
        opened: &Opened,
        spent: &Spend,
        width: usize,
    ) -> Drawn {
        let mut drawn = Drawn::new();
        let tokens = spent.total_tokens();
        let cached = spent.cached_tokens;
        let new = spent.input_tokens.saturating_sub(cached);
        let output = spent.output_tokens;
        drawn.push(vec![
            kit.dim(format!("its tokens · {}", self.tokens(tokens))),
        ]);
        let kinds = [
            (cached, Role::TokensCached, "from cache"),
            (new, Role::TokensInput, "new input"),
            (output, Role::TokensOutput, "output"),
        ];
        let parts: Vec<_> = kinds
            .iter()
            .map(|(n, role, _)| (*n, kit.fg(*role)))
            .collect();
        drawn.push(draw::bar(kit, &parts, tokens, width));
        let entries: Vec<Entry> = kinds
            .iter()
            .map(|(n, role, name)| Entry {
                style: kit.fg(*role),
                name: (*name).to_owned(),
                amount: self.tokens(*n),
                note: share(*n, tokens),
            })
            .collect();
        for line in draw::legend(kit, &entries, None, width) {
            drawn.push(line);
        }

        // Days are kept per model, so a role opened has none of its own.
        if !matches!(opened, Opened::Role(_)) {
            let days = self.crew.daily(self.measure, |part| {
                models
                    .iter()
                    .any(|(_, model)| model.is(&part.agent, part.model.as_deref()))
            });
            let style = models.first().map_or(kit.fg(Role::Card), |(cli, model)| {
                model_style(kit, cli, model)
            });
            let mut line = vec![draw::sparkline(kit, &days, style)];
            let mut words = format!(" {} a day", self.measure.word());
            let busiest = days
                .iter()
                .enumerate()
                .filter(|(_, value)| **value > 0)
                .max_by_key(|(_, value)| **value)
                .and_then(|(at, _)| self.crew.days.get(at))
                .map(|(day, _)| day.weekday().to_string());
            if let Some(day) = busiest {
                words.push_str(&format!(" · busiest {day}"));
            }
            line.push(kit.dim(words));
            drawn.push(truncate_spans(&line, width));
        }

        for line in self.fallback_lines(kit, models, width) {
            drawn.push(line);
        }

        let clis: Vec<&Cli> = {
            let mut clis: Vec<&Cli> = models.iter().map(|(cli, _)| *cli).collect();
            clis.dedup_by(|a, b| a.agent == b.agent);
            clis
        };
        if let [cli] = clis.as_slice()
            && let Some(plan) = self.usage.plans.iter().find(|plan| plan.agent == cli.agent)
            && !plan.windows.is_empty()
        {
            let mut line = vec![kit.dim(format!("{}'s plan ", cli.name))];
            let room = width.saturating_sub(spans_width(&line));
            line.extend(self.plan_line(kit, cli, room));
            drawn.push(line);
        }
        drawn
    }

    /// `fallbacks took over 2 tasks from Opus 5.5 when Claude's 5-hour window ran out`, one
    /// line per model it took over from, wrapped to `width`.
    fn fallback_lines(
        &self,
        kit: &Kit<'_>,
        models: &[(&Cli, &Model)],
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        // By the CLI and model it took over from: how many tasks, and why the first stopped.
        let mut from: BTreeMap<(String, Option<String>), (u32, &Fallback)> = BTreeMap::new();
        for fallback in &self.crew.fallbacks {
            let ours = models
                .iter()
                .any(|(_, model)| model.is(&fallback.agent, fallback.model.as_deref()));
            if ours {
                let entry = from
                    .entry((fallback.from_agent.clone(), fallback.from_model.clone()))
                    .or_insert((0, fallback));
                entry.0 += 1;
            }
        }
        let mut lines = Vec::new();
        for ((agent, model), (n, first)) in from {
            let name = self
                .crew
                .models()
                .find(|(_, m)| m.is(&agent, model.as_deref()))
                .map_or_else(|| agent.clone(), |(_, m)| m.name.clone());
            let mut text = format!("took over {} from {name}", count(n, "task"));
            match &first.window {
                Some(window) => {
                    let cli = self.crew.clis.iter().find(|cli| cli.agent == agent);
                    let cli = cli.map_or_else(|| short_cli(&agent, &agent), Cli::short);
                    let window = window_words(window);
                    text.push_str(&format!(" when {cli}'s {window} window ran out"));
                }
                None if !first.why.is_empty() => text.push_str(&format!(" · {}", first.why)),
                None => {}
            }
            for (k, row) in wrap(&format!("fallbacks {text}"), width)
                .into_iter()
                .enumerate()
            {
                lines.push(match row.strip_prefix("fallbacks ") {
                    Some(rest) if k == 0 => vec![kit.dim("fallbacks "), Span::raw(rest.to_owned())],
                    _ => vec![Span::raw(row)],
                });
            }
        }
        lines
    }
}

/// How a plan window reads in a sentence: `five_hour` is the `5-hour` window.
fn window_words(window: &str) -> String {
    match window {
        "five_hour" => "5-hour".to_owned(),
        "seven_day" | "weekly" => "weekly".to_owned(),
        "seven_day_opus" => "weekly Opus".to_owned(),
        "seven_day_sonnet" => "weekly Sonnet".to_owned(),
        "daily" | "one_day" => "daily".to_owned(),
        other => other.replace('_', " "),
    }
}

/// `90% first pass` with squares for a builder, `sent back 2 · 52s each` for a reviewer.
fn review_words(kit: &Kit<'_>, role: CrewRole, model: &Model) -> Vec<Span<'static>> {
    let n = model.did(role);
    match role {
        CrewRole::Worker if model.fared.total() > 0 => {
            let mut words = draw::squares(kit, model.fared, SQUARES);
            words.push(kit.strong(format::percent(model.fared.first_pass())));
            words.push(kit.dim(" first pass"));
            words
        }
        CrewRole::Reviewer | CrewRole::Judge => {
            let back = model.sent_back.get(&role).copied().unwrap_or(0);
            let mut words = vec![kit.dim("sent back "), kit.strong(back.to_string())];
            let took_secs = model.took.get(&role).copied().unwrap_or(0);
            if n > 0 && took_secs > 0 {
                words.push(kit.dim(format!(" · {} each", took(took_secs / u64::from(n)))));
            }
            words
        }
        _ => {
            let took_secs = model.took.get(&role).copied().unwrap_or(0);
            if n > 0 && took_secs > 0 {
                vec![kit.dim(format!("{} each", took(took_secs / u64::from(n))))]
            } else {
                Vec::new()
            }
        }
    }
}

/// Words onto rows of at most `width` cells, a word too long for a row cut.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split(' ') {
        let needs = str_width(word) + usize::from(!row.is_empty());
        if !row.is_empty() && str_width(&row) + needs > width {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn fared_of(each: impl Iterator<Item = Fared>) -> Fared {
    let mut fared = Fared::default();
    for one in each {
        fared.first += one.first;
        fared.once += one.once;
        fared.more += one.more;
    }
    fared
}
