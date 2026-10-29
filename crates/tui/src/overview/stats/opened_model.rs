//! A model opened in full with Enter. At the left its work in each role: how its tasks and
//! its reviews fared, the time, the cost and the size of each, and why its tasks were sent
//! back. At the right its tokens, its days or weeks, and how it stands beside the other models
//! that filled the same roles.

use argonauts_core::Spend;
use argonauts_core::api::UsagePeriod;
use argonauts_core::crew_stats::StatsRole;
use argonauts_core::logos::logo_for;
use argonauts_core::settings::stats::StatsMeasure;
use ratatui::text::Span;

use super::by_model::each_took;
use super::cli::{SQUARES, Stats, cost_each, count, model_fill, model_style, took};
use super::data::{Cli, DayPart, Model, cache_hit, written_a_second};
use super::draw::{self, Fared, Fill};
use crate::format;
use crate::kit::bars::fit_keeping_bars;
use crate::kit::legend::BETWEEN_WORDS;
use crate::kit::logo::{ModelLabel, NoPicture};
use crate::kit::{Drawn, Frame, Kit, fit, spread};
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;

/// Cells between the card's two columns.
const COLUMN_GAP: usize = BETWEEN_WORDS;
/// The fewest cells the use at the right keeps: its widest sentence beside the other models,
/// `as a worker: 31s each vs 1m 34s for the other 4`, and a blank.
const USE_LEAST: usize = 48;
/// A reason work was sent back for, and its bar.
const REASON: usize = 22;
const REASON_BAR: usize = 14;
/// The widest the bar of what its tokens were is drawn, and the cells it leaves of its column.
const TOKENS_BAR: usize = 50;
const TOKENS_SPARE: usize = 4;

type Row = Vec<Span<'static>>;

impl Stats<'_> {
    /// `model` of `cli` in full.
    pub(super) fn opened_model(
        &self,
        kit: &Kit<'_>,
        frame: Frame,
        cli: &Cli,
        model: &Model,
    ) -> Drawn {
        let inner = frame.inner();
        let mut drawn = Drawn::new();
        drawn.push(self.model_title(kit, cli, model, inner));
        drawn.push(Vec::new());

        // The work takes what its widest row needs, past half the card, out of the room the
        // use at the right leaves it.
        let left = self.model_work(kit, model);
        let half = (inner + COLUMN_GAP) / 2;
        let most = inner.saturating_sub(COLUMN_GAP + USE_LEAST).max(half);
        let widest = left.iter().map(|row| spans_width(row)).max().unwrap_or(0);
        let left_width = widest.clamp(half, most);
        let right_width = inner.saturating_sub(left_width + COLUMN_GAP);
        let right = self.model_use(kit, cli, model, right_width);
        let rows = left.len().max(right.len());
        let mut lefts = left.into_iter();
        let mut rights = right.into_iter();
        for _ in 0..rows {
            let left = fit_keeping_bars(lefts.next().unwrap_or_default(), left_width);
            let mut row = fit(left, left_width + COLUMN_GAP);
            row.extend(fit_keeping_bars(
                rights.next().unwrap_or_default(),
                right_width,
            ));
            drawn.push(row);
        }
        frame.draw(kit, drawn)
    }

    /// Its logo and `██  Haiku 5.5 on Claude Code · as worker, reviewer, judge`, as By model's
    /// rows name it, and at the right its tokens, its spend and its sessions.
    fn model_title(&self, kit: &Kit<'_>, cli: &Cli, model: &Model, inner: usize) -> Row {
        let roles: Vec<&str> = model.filled().into_iter().map(StatsRole::as_str).collect();
        let logo = logo_for(&cli.agent, model.model.as_deref());
        let label = ModelLabel::named(&logo, &model.name, kit.theme, NoPicture::Dot);
        let mut left = label.logo;
        let swatch = draw::swatch(kit, model_fill(kit, cli, model));
        if let Some(cell) = swatch.first() {
            left.push(Span::styled(cell.content.repeat(2), cell.style));
        }
        left.push(Span::raw("  "));
        left.push(kit.strong(label.name));
        left.push(kit.dim(format!(" on {} · as {}", cli.name, roles.join(", "))));
        let mut right = vec![
            kit.strong(self.tokens_short(model.tokens())),
            kit.dim(" tokens · "),
            kit.strong(format::spend(&model.spent)),
        ];
        let sessions: u32 = model.sessions.values().sum();
        if sessions > 0 {
            right.push(kit.dim(format!(" · {}", count(sessions, "session"))));
        }
        let room = inner.saturating_sub(spans_width(&right) + 2);
        spread(truncate_spans(&left, room), right, inner)
    }

    /// Its work: a row each for how its tasks and its reviews fared, for the time, the cost
    /// and the size of a piece, a row for each other role it filled, and why its tasks were
    /// sent back.
    fn model_work(&self, kit: &Kit<'_>, model: &Model) -> Vec<Row> {
        let tasks = model.did(StatsRole::Worker);
        let reviews = Reviews::of(model);
        let mut labelled: Vec<(String, Row)> = Vec::new();
        if model.fared.total() > 0 {
            let back = sent_back_words(model.fared);
            labelled.push((
                "tasks".to_owned(),
                fared_words(kit, model.fared, &format!(" first pass{back}")),
            ));
        }
        if reviews.fared.total() > 0 {
            let back = format!(" accepted · {} sent back", reviews.fared.once);
            labelled.push((
                "reviews given".to_owned(),
                fared_words(kit, reviews.fared, &back),
            ));
        }
        let work = model.spent_in(StatsRole::Worker);
        let time = [
            (each_took(model, StatsRole::Worker), " a task"),
            (reviews.each_took(), " a review"),
        ];
        let cost = [
            (
                (tasks > 0).then(|| cost_each(work.cost_usd, tasks)),
                " a task",
            ),
            (reviews.each_cost(), " a review"),
        ];
        let size = [
            (
                (tasks > 0).then(|| self.tokens(work.total_tokens() / u64::from(tasks))),
                " tokens a task",
            ),
            (
                tool_calls_a_task(model).map(|calls| calls.to_string()),
                " tool calls a task",
            ),
        ];
        for (label, amounts) in [("time", time), ("cost", cost), ("size", size)] {
            let words = amounts_words(kit, amounts);
            if !words.is_empty() {
                labelled.push((label.to_owned(), words));
            }
        }
        for role in [StatsRole::Main, StatsRole::Orchestrator, StatsRole::Merger] {
            let done = model.did(role);
            if done == 0 {
                continue;
            }
            let spent = model.spent_in(role);
            let mut words = vec![kit.strong(done.to_string())];
            if let Some(each) = each_took(model, role) {
                words.extend([kit.dim(" · "), kit.strong(each), kit.dim(" each")]);
            }
            words.extend([
                kit.dim(" · "),
                kit.strong(cost_each(spent.cost_usd, done)),
                kit.dim(" each"),
            ]);
            labelled.push((format!("{}s", role.unit()), words));
        }

        // The words start a column of their own after the longest label.
        let label_w = labelled.iter().map(|(label, _)| str_width(label)).max();
        let label_w = label_w.unwrap_or(0) + BETWEEN_WORDS;
        let mut rows = vec![vec![kit.strong("work")]];
        rows.extend(labelled.into_iter().map(|(label, words)| {
            let mut row = fit(vec![Span::raw(label)], label_w);
            row.extend(words);
            row
        }));

        if !model.returned_for.is_empty() {
            rows.push(Vec::new());
            rows.push(vec![kit.strong("why its tasks were sent back")]);
            let mut reasons: Vec<(&String, &u32)> = model.returned_for.iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let whole: u32 = reasons.iter().map(|(_, n)| **n).sum();
            for (why, n) in reasons {
                let why = truncate_spans(&[Span::raw(why.clone())], REASON - 1);
                let mut row = fit(why, REASON);
                row.extend(draw::thin(
                    kit,
                    u64::from(*n),
                    u64::from(whole),
                    REASON_BAR,
                    kit.fg(Role::Warn),
                ));
                row.push(kit.dim(format!("  {n}")));
                rows.push(row);
            }
        }
        rows
    }

    /// What it used: its tokens by what they were, its days or weeks, how it stands beside the
    /// other models in its roles, and the tasks that fell back to it.
    fn model_use(&self, kit: &Kit<'_>, cli: &Cli, model: &Model, width: usize) -> Vec<Row> {
        let style = model_style(kit, cli, model);
        let mut rows = vec![vec![kit.strong("tokens")]];
        rows.extend(self.tokens_were(kit, &model.spent, model.took_secs(), width));

        rows.push(Vec::new());
        rows.push(vec![kit.strong(self.each_slot_words())]);
        let ours = |part: &DayPart| model.is(&part.agent, part.model.as_deref());
        let by_model = |part: &DayPart| Some((part.agent.clone(), part.model.clone()));
        let apart = self.crew.period == UsagePeriod::Week;
        let mut sparks = Vec::new();
        for (k, (measure, label)) in [
            (StatsMeasure::Tokens, "tokens "),
            (StatsMeasure::Tasks, "tasks "),
        ]
        .into_iter()
        .enumerate()
        {
            if k > 0 {
                sparks.push(Span::raw("   "));
            }
            sparks.push(kit.dim(label));
            let series = self.crew.series(measure, ours);
            let top = self.crew.fullest(measure, by_model);
            sparks.extend(if apart {
                draw::spark_apart(kit, &series, top, style)
            } else {
                draw::spark(kit, &series, top, style)
            });
        }
        rows.push(sparks);
        if apart {
            let mut letters = vec![Span::raw(" ".repeat("tokens ".len()))];
            letters.push(kit.dim(self.weekday_letters()));
            rows.push(letters);
        }

        let beside = self.beside_the_others(kit, model);
        if !beside.is_empty() {
            rows.push(Vec::new());
            rows.push(vec![kit.strong("beside the other models")]);
            rows.extend(beside);
        }
        let fallbacks = self.fallback_rows(kit, &[(cli, model)], width);
        if !fallbacks.is_empty() {
            rows.push(Vec::new());
            rows.extend(fallbacks);
        }
        rows
    }

    /// The bar of what its tokens were over a table of them, then the share of its input it
    /// read from cache and the tokens it wrote in each second its sessions ran.
    fn tokens_were(&self, kit: &Kit<'_>, spent: &Spend, secs: u64, width: usize) -> Vec<Row> {
        let kinds = self.kinds(kit, spent, ["cache", "new", "output"]);
        let parts: Vec<(u64, Fill)> = (kinds.iter())
            .map(|row| (row.part.value, row.part.fill))
            .collect();
        let bar = width.saturating_sub(TOKENS_SPARE).min(TOKENS_BAR);
        let mut rows = vec![draw::stacked(kit, &parts, bar)];
        let columns = draw::table_columns(kinds.iter(), width, false);
        rows.extend(draw::table(kit, &kinds, columns, true));
        let mut speed = Vec::new();
        if let Some(hit) = cache_hit(spent) {
            speed.extend([kit.dim("cache hit "), kit.strong(format::percent(hit))]);
        }
        if let Some(rate) = written_a_second(spent, secs) {
            let before = if speed.is_empty() {
                "output "
            } else {
                " · output "
            };
            speed.extend([
                kit.dim(before),
                kit.strong(rate.to_string()),
                kit.dim(" tokens/s"),
            ]);
        }
        rows.push(speed);
        rows
    }

    /// `a day, this week`: what each cell of the sparklines is, and over what.
    fn each_slot_words(&self) -> &'static str {
        match self.crew.period {
            UsagePeriod::Today => "today",
            UsagePeriod::Week => "a day, this week",
            UsagePeriod::Month => "a day, this month",
            UsagePeriod::All if self.crew.slots_are_days() => "a day, so far",
            UsagePeriod::All => "a week, so far",
        }
    }

    /// The reviewers and the workers it stood among, two rows a role: the time of a piece
    /// of its work against the others', then how often it sent work back, or had its own
    /// pass first time, against how often they did.
    fn beside_the_others(&self, kit: &Kit<'_>, model: &Model) -> Vec<Row> {
        let mut rows = Vec::new();
        for role in [StatsRole::Worker, StatsRole::Reviewer, StatsRole::Judge] {
            let others: Vec<&Model> = self
                .crew
                .in_role(role)
                .into_iter()
                .map(|(_, other)| other)
                .filter(|other| !other.is(&model.agent, model.model.as_deref()))
                .filter(|other| other.did(role) > 0)
                .collect();
            if others.is_empty() || model.did(role) == 0 {
                continue;
            }
            let who = match others.as_slice() {
                [one] => one.name.clone(),
                many => format!("the other {}", many.len()),
            };
            let done: u32 = others.iter().map(|other| other.did(role)).sum();
            let secs: u64 = others.iter().map(|other| other.took_in(role)).sum();
            if let Some(each) = each_took(model, role)
                && secs > 0
            {
                rows.push(vec![
                    kit.dim(format!("as a {}: ", role.as_str())),
                    kit.strong(each),
                    kit.dim(" each vs "),
                    kit.strong(took(secs / u64::from(done))),
                    kit.dim(format!(" for {who}")),
                ]);
            }
            let share = |part: u32, whole: u32| format::percent(f64::from(part) / f64::from(whole));
            let (before, ours, then, theirs) = match role {
                StatsRole::Worker => {
                    let fared = others.iter().fold(Fared::default(), |sum, other| Fared {
                        first: sum.first + other.fared.first,
                        once: sum.once + other.fared.once,
                        more: sum.more + other.fared.more,
                    });
                    if model.fared.total() == 0 || fared.total() == 0 {
                        continue;
                    }
                    (
                        "",
                        format::percent(model.fared.first_pass()),
                        " of its tasks pass first time vs ",
                        format::percent(fared.first_pass()),
                    )
                }
                _ => {
                    let back: u32 = others.iter().map(|other| other.back_in(role)).sum();
                    (
                        "sends back ",
                        share(model.back_in(role), model.did(role)),
                        " of what it reviews vs ",
                        share(back, done),
                    )
                }
            };
            rows.push(vec![
                kit.dim(before),
                kit.strong(ours),
                kit.dim(then),
                kit.strong(theirs),
            ]);
        }
        rows
    }
}

/// A model's reviews and sign-offs together, a judge's sign-off being a review too.
struct Reviews {
    fared: Fared,
    took_secs: u64,
    cost_usd: f64,
}

impl Reviews {
    /// The time and the cost are those of the roles it finished something in, so a session
    /// that ended with no review doesn't weigh on each one that was given.
    fn of(model: &Model) -> Self {
        let roles = [StatsRole::Reviewer, StatsRole::Judge]
            .into_iter()
            .filter(|role| model.did(*role) > 0);
        Self {
            fared: model.reviewed(),
            took_secs: roles.clone().map(|role| model.took_in(role)).sum(),
            cost_usd: roles.map(|role| model.spent_in(role).cost_usd).sum(),
        }
    }

    fn each_took(&self) -> Option<String> {
        let done = self.fared.total();
        (done > 0 && self.took_secs > 0).then(|| took(self.took_secs / u64::from(done)))
    }

    fn each_cost(&self) -> Option<String> {
        let done = self.fared.total();
        (done > 0).then(|| cost_each(self.cost_usd, done))
    }
}

/// `▪▪▪▪▪▪▪▪▪▪ 54% accepted · 6 sent back`: ten squares, the share that went the first way, and
/// `words`.
fn fared_words(kit: &Kit<'_>, fared: Fared, words: &str) -> Row {
    let mut row = draw::tenths(kit, fared, SQUARES);
    row.push(Span::raw(" "));
    row.push(kit.strong(format::percent(fared.first_pass())));
    row.push(kit.dim(words.to_owned()));
    row
}

/// ` · 3 sent back once`, ` · 3 sent back once, 1 twice`, or nothing when none was.
fn sent_back_words(fared: Fared) -> String {
    match (fared.once, fared.more) {
        (0, 0) => String::new(),
        (once, 0) => format!(" · {once} sent back once"),
        (0, more) => format!(" · {more} sent back twice"),
        (once, more) => format!(" · {once} sent back once, {more} twice"),
    }
}

/// `2m 55s a task · 8s a review`: each amount there is, bold, before its words.
fn amounts_words(kit: &Kit<'_>, amounts: [(Option<String>, &str); 2]) -> Row {
    let mut row = Vec::new();
    for (amount, words) in amounts {
        let Some(amount) = amount else {
            continue;
        };
        if !row.is_empty() {
            row.push(kit.dim(" · "));
        }
        row.push(kit.strong(amount));
        row.push(kit.dim(words));
    }
    row
}

/// The tools its worker sessions called for each task it built.
fn tool_calls_a_task(model: &Model) -> Option<u32> {
    let tasks = model.did(StatsRole::Worker);
    let calls = model
        .tool_calls
        .get(&StatsRole::Worker)
        .copied()
        .unwrap_or(0);
    (tasks > 0 && calls > 0).then(|| (f64::from(calls) / f64::from(tasks)).round() as u32)
}
