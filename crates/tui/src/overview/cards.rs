//! The usage cards: plan usage, spend, tokens, history and workflows, each drawn from one
//! [`UsageSummary`]. The Usage tab stacks them in full, and the band above the input pins any
//! of them, compact or folded to a line.
//!
//! Plan usage rows sit back to back, so their meters are half meters, which never merge into
//! a slab the way block meters would. The spend and tokens bars are half stacks, so every bar
//! of the Usage tab is one height.

use argonauts_core::api::{PlanUsage, RunState, RunSummary, UsagePeriod, UsageSummary};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::limits::{QuotaReading, QuotaStatus};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::ui::WidgetName;
use ratatui::text::Span;
use time::OffsetDateTime;

use crate::format::{self, ClockStyle};
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::bars::{self, Stack};
use crate::widgets::{
    Badge, Chart, Drawn, Entry, Form, Frame, Header, Kit, Level, State, States, bodies, ljust,
    rjust, spread,
};

/// From this share of a window on, plan usage shows amber, with its reset time.
const NEAR_LIMIT: f64 = 0.9;
/// The least cells a plan window's meter takes before its reset time gives way.
const LEAST_METER: usize = 8;
/// Days the history card charts.
const HISTORY_DAYS: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageCard {
    PlanUsage,
    Spend,
    Tokens,
    History,
    Workflows,
}

impl UsageCard {
    /// In the order the Usage tab stacks them.
    pub const ALL: [UsageCard; 5] = [
        UsageCard::PlanUsage,
        UsageCard::Spend,
        UsageCard::Tokens,
        UsageCard::History,
        UsageCard::Workflows,
    ];

    /// The card a mod's name draws, for the names these cards answer to.
    pub fn of(name: WidgetName) -> Option<Self> {
        match name {
            WidgetName::Quota => Some(UsageCard::PlanUsage),
            WidgetName::Spend => Some(UsageCard::Spend),
            WidgetName::Tokens => Some(UsageCard::Tokens),
            WidgetName::History => Some(UsageCard::History),
            WidgetName::Runs => Some(UsageCard::Workflows),
            _ => None,
        }
    }

    pub fn name(self) -> WidgetName {
        match self {
            UsageCard::PlanUsage => WidgetName::Quota,
            UsageCard::Spend => WidgetName::Spend,
            UsageCard::Tokens => WidgetName::Tokens,
            UsageCard::History => WidgetName::History,
            UsageCard::Workflows => WidgetName::Runs,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            UsageCard::PlanUsage => "plan usage",
            UsageCard::Spend => "spend",
            UsageCard::Tokens => "tokens",
            UsageCard::History => "history",
            UsageCard::Workflows => "workflows",
        }
    }

    fn glyph_role(self) -> Role {
        match self {
            UsageCard::PlanUsage | UsageCard::Tokens | UsageCard::Workflows => Role::Card,
            UsageCard::Spend => Role::Success,
            UsageCard::History => Role::AgentOrange,
        }
    }
}

/// The color a CLI keeps on every bar and legend, so its share reads the same everywhere.
pub fn cli_role(agent: &str) -> Role {
    match agent {
        "claude-code" => Role::AgentOrange,
        "codex" => Role::AgentBlue,
        "antigravity-cli" => Role::AgentCyan,
        "opencode" => Role::AgentPurple,
        _ => Role::AgentPink,
    }
}

/// What the cards draw from.
#[derive(Debug, Clone, Copy)]
pub struct UsageCards<'a> {
    pub usage: &'a UsageSummary,
    pub names: &'a DisplayNames,
    /// Now, in the zone times are shown in.
    pub now: OffsetDateTime,
    /// Plan usage as what's left of a window, rather than what's used.
    pub left: bool,
}

impl UsageCards<'_> {
    /// The card in its frame: header, then its body. `Line` is the folded row, with no frame.
    pub fn draw(&self, kit: &Kit<'_>, card: UsageCard, form: Form, frame: Frame) -> Drawn {
        let mut drawn = Drawn::new();
        if matches!(form, Form::Line | Form::Chip) {
            let width = frame.left + frame.width;
            let mut row = vec![Span::raw(" ".repeat(frame.left))];
            row.extend(self.line(kit, card, width.saturating_sub(frame.left)));
            drawn.push(row);
            return drawn;
        }
        let inner = frame.inner();
        let full = form == Form::Full;
        drawn.push(self.header(kit, card, inner));
        drawn.append(match card {
            UsageCard::PlanUsage => self.plan_rows(kit, inner, full),
            UsageCard::Spend => self.spend_body(kit, inner, full),
            UsageCard::Tokens => self.tokens_body(kit, inner, full),
            UsageCard::History => self.history_body(kit, inner),
            UsageCard::Workflows => self.workflow_rows(kit, inner, full),
        });
        frame.draw(kit, drawn)
    }

    /// The folded card: its mark and name, then the one number that matters.
    pub fn line(&self, kit: &Kit<'_>, card: UsageCard, width: usize) -> Vec<Span<'static>> {
        let mut row = Header::new(card.title(), card.glyph_role()).left(kit);
        row.push(Span::raw("  "));
        row.extend(self.key_number(kit, card));
        truncate_spans(&row, width)
    }

    /// What a hidden band still says of the card, in its level's color: the fullest plan
    /// window, the spend, the tokens.
    pub fn key_number(&self, kit: &Kit<'_>, card: UsageCard) -> Vec<Span<'static>> {
        let usage = self.usage;
        match card {
            UsageCard::PlanUsage => match self.fullest() {
                Some((plan, reading)) => {
                    let level = self.level(reading);
                    vec![Span::styled(
                        format!(
                            "{} {} {}",
                            short_name(plan),
                            window_label(reading),
                            self.percent(reading)
                        ),
                        kit.style(level.number_role()),
                    )]
                }
                None => vec![kit.dim("no reading yet")],
            },
            UsageCard::Spend => vec![kit.dim(format!(
                "{} {}",
                format::spend(&usage.total),
                period_words(usage.period)
            ))],
            UsageCard::Tokens => vec![kit.dim(format!(
                "{} {}",
                format::tokens(usage.total.total_tokens()),
                period_words(usage.period)
            ))],
            UsageCard::History => vec![kit.dim(format!(
                "{} in {HISTORY_DAYS} days",
                format::spend(&self.history_total())
            ))],
            UsageCard::Workflows => vec![kit.dim(count(usage.runs.len(), "workflow"))],
        }
    }

    fn header(&self, kit: &Kit<'_>, card: UsageCard, width: usize) -> Vec<Span<'static>> {
        let left = Header::new(card.title(), card.glyph_role()).left(kit);
        let usage = self.usage;
        let period = period_words(usage.period);
        // What gives way first on a narrow card, then what stays.
        let (lead, keep): (Vec<Span<'static>>, Vec<Span<'static>>) = match card {
            UsageCard::PlanUsage => {
                let limit = match usage.plan_limit {
                    Some(limit) => vec![
                        kit.dim("your limit "),
                        kit.strong(format!("{:.0}%", limit * 100.0)),
                        kit.dim(" · highest "),
                    ],
                    None => vec![kit.dim("no limit set · highest ")],
                };
                match self.fullest() {
                    Some((plan, reading)) => {
                        let level = self.level(reading);
                        let mut keep = vec![
                            kit.strong(format!("{} {}", short_name(plan), window_label(reading))),
                            Span::raw(" "),
                        ];
                        let used = reading.used.unwrap_or(1.0);
                        keep.extend(Badge::percent(self.shown(used), level).spans(kit));
                        (limit, keep)
                    }
                    None => (Vec::new(), vec![kit.dim("no reading yet")]),
                }
            }
            UsageCard::Spend => {
                let spent = kit.strong(format::spend(&usage.total));
                match usage.spend_limit {
                    Some(limit) if limit > 0.0 => {
                        let share = usage.total.cost_usd / limit;
                        let level = if share >= 1.0 {
                            Level::Danger
                        } else if share >= NEAR_LIMIT {
                            Level::Warn
                        } else {
                            Level::Ok
                        };
                        let mut keep = vec![
                            spent,
                            kit.dim(format!(" of {} {period}", format::usd(limit))),
                            Span::raw(" "),
                        ];
                        keep.extend(Badge::percent(share, level).spans(kit));
                        (Vec::new(), keep)
                    }
                    _ => {
                        let mut tail = format!(" {period} · no limit set");
                        if let Some(who) = self.spenders() {
                            tail.push_str(&format!(" · {who}"));
                        }
                        (Vec::new(), vec![spent, kit.dim(tail)])
                    }
                }
            }
            UsageCard::Tokens => (
                Vec::new(),
                vec![
                    kit.strong(format::tokens(usage.total.total_tokens())),
                    kit.dim(format!(" {period}")),
                ],
            ),
            UsageCard::History => (
                Vec::new(),
                vec![
                    kit.strong(format::spend(&self.history_total())),
                    kit.dim(format!(" in {HISTORY_DAYS} days")),
                ],
            ),
            UsageCard::Workflows => {
                let spent = usage
                    .runs
                    .iter()
                    .fold(argonauts_core::Spend::default(), |sum, run| {
                        sum.plus(&run.spent)
                    });
                (
                    Vec::new(),
                    vec![kit.dim(format!(
                        "{period} · {} · {}",
                        usage.runs.len(),
                        format::spend(&spent)
                    ))],
                )
            }
        };
        let room = width.saturating_sub(spans_width(&left) + 2);
        let right = if spans_width(&lead) + spans_width(&keep) <= room {
            [lead, keep].concat()
        } else {
            truncate_spans(&keep, room)
        };
        spread(left, right, width)
    }

    /// `3 runs and this chat`, when both spent in the period.
    fn spenders(&self) -> Option<String> {
        let runs = self.usage.runs.len();
        let chat = self.usage.conversation.is_some();
        match (runs, chat) {
            (0, _) => None,
            (runs, true) => Some(format!("{} and this chat", count(runs, "run"))),
            (runs, false) => Some(count(runs, "run")),
        }
    }

    /// The plan window nearest its end, across every CLI that still has some left. One
    /// that's out has its own row saying so.
    fn fullest(&self) -> Option<(&PlanUsage, &QuotaReading)> {
        self.usage
            .plans
            .iter()
            .flat_map(|plan| plan.windows.iter().map(move |reading| (plan, reading)))
            .filter(|(_, reading)| reading.status != QuotaStatus::Rejected)
            .max_by(|a, b| used_of(a.1).total_cmp(&used_of(b.1)))
    }

    /// A share of a window as shown: what's used, or what's left of it.
    fn shown(&self, used: f64) -> f64 {
        if self.left {
            (1.0 - used).max(0.0)
        } else {
            used
        }
    }

    fn percent(&self, reading: &QuotaReading) -> String {
        match reading.used {
            Some(used) => format!("{:.0}%", self.shown(used).max(0.0) * 100.0),
            None if reading.status == QuotaStatus::Rejected => "out".to_owned(),
            None => format::UNKNOWN.to_owned(),
        }
    }

    /// Amber near the end of a window or at your own limit, red once it's out.
    fn level(&self, reading: &QuotaReading) -> Level {
        let used = used_of(reading);
        let limit = self.usage.plan_limit.unwrap_or(NEAR_LIMIT).min(NEAR_LIMIT);
        if reading.status == QuotaStatus::Rejected || used >= 1.0 {
            Level::Danger
        } else if used >= limit || reading.status == QuotaStatus::Warning {
            Level::Warn
        } else {
            Level::Ok
        }
    }

    /// One row per CLI, its windows side by side. In full each CLI gets a row of its own
    /// first, saying its plan, whether the main agent runs on it, and how old the reading is.
    fn plan_rows(&self, kit: &Kit<'_>, width: usize, full: bool) -> Drawn {
        let plans = &self.usage.plans;
        let mut body = Drawn::new();
        if plans.is_empty() {
            body.push(vec![kit.dim("No CLI in the crew reports a plan yet")]);
            return body;
        }
        let label = |plan: &PlanUsage| match &plan.plan {
            Some(name) => format!("{} {name}", plan.name),
            None => plan.name.clone(),
        };
        const WINDOWS: &str = "  5-hour and 7-day";
        let name_slot = if full {
            str_width(WINDOWS)
        } else {
            plans
                .iter()
                .map(|plan| str_width(&label(plan)))
                .max()
                .unwrap_or(0)
        } + 2;
        let layout = self.window_layout(width.saturating_sub(name_slot));
        for plan in plans {
            let name = |kit: &Kit<'_>| {
                let mut spans = vec![kit.strong(plan.name.clone())];
                if let Some(tier) = &plan.plan {
                    spans.push(kit.dim(format!(" {tier}")));
                }
                spans
            };
            if let Some(mut why) = self.no_windows(kit, plan) {
                // In full the CLI in use keeps its mark, reading or not.
                let in_use = full && plan.in_use;
                let mut row = Vec::new();
                if in_use {
                    row.push(kit.span("● ", Role::Bright));
                }
                row.extend(name(kit));
                if in_use {
                    row.push(kit.dim(" · in use · "));
                    if let Some(first) = why.first_mut() {
                        first.content = first.content.trim_start().to_owned().into();
                    }
                }
                row.extend(why);
                body.push(truncate_spans(&row, width));
                continue;
            }
            if full {
                let mut head = Vec::new();
                if plan.in_use {
                    head.push(kit.span("● ", Role::Bright));
                }
                head.extend(name(kit));
                let mut notes = Vec::new();
                if plan.in_use {
                    notes.push("in use".to_owned());
                }
                if let Some(age) = self.read_ago(plan) {
                    notes.push(age);
                }
                if !notes.is_empty() {
                    head.push(kit.dim(format!(" · {}", notes.join(" · "))));
                }
                body.push(truncate_spans(&head, width));
            }
            let mut row = if full {
                vec![kit.dim(ljust(WINDOWS, name_slot))]
            } else {
                let mut cell = name(kit);
                let pad = name_slot.saturating_sub(spans_width(&cell));
                cell.push(Span::raw(" ".repeat(pad)));
                cell
            };
            let (windows, reached) = self.windows(kit, plan, &layout);
            row.extend(windows);
            // The words go beside the window where they fit whole, and under it where not.
            match reached {
                Some(words) if spans_width(&row) + WINDOW_GAP + spans_width(&words) <= width => {
                    row.push(Span::raw(" ".repeat(WINDOW_GAP)));
                    row.extend(words);
                    body.push(row);
                }
                Some(words) => {
                    body.push(truncate_spans(&row, width));
                    let mut under = vec![Span::raw(" ".repeat(name_slot))];
                    under.extend(words);
                    body.push(truncate_spans(&under, width));
                }
                None => body.push(truncate_spans(&row, width)),
            }
        }
        body
    }

    /// Why a CLI has no meters to show, when it hasn't: out, down, billed per token, or not
    /// read yet. The words follow its name.
    fn no_windows(&self, kit: &Kit<'_>, plan: &PlanUsage) -> Option<Vec<Span<'static>>> {
        let moved = plan
            .moved_to
            .as_ref()
            .map(|taker| kit.dim(format!(" · its tasks moved to {taker}")));
        let out = plan
            .windows
            .iter()
            .find(|reading| reading.status == QuotaStatus::Rejected && !reading.wrap_up);
        let mut words = if plan.pays_per_token {
            vec![kit.dim(" pays per token, so there is no plan to track")]
        } else if let Some(reading) = out {
            let until = reading
                .resets_at
                .map_or_else(String::new, |at| format!(" until {}", self.reset_time(at)));
            vec![kit.span(format!("  out{until}"), Role::Error)]
        } else if plan.down {
            vec![kit.span("  can't be reached", Role::Error)]
        } else if plan.windows.is_empty() {
            return Some(vec![kit.dim("  no reading yet")]);
        } else {
            return None;
        };
        words.extend(moved);
        Some(words)
    }

    /// When a window resets, on the clock times are shown in.
    fn reset_time(&self, at: OffsetDateTime) -> String {
        let at = at.to_offset(self.now.offset());
        format::reset_time(at, self.now, ClockStyle::default())
    }

    /// `read 3m ago`, from the newest of the CLI's readings.
    fn read_ago(&self, plan: &PlanUsage) -> Option<String> {
        let newest = plan
            .windows
            .iter()
            .map(|reading| reading.observed_at)
            .max()?;
        let ago = (self.now - newest).max(time::Duration::ZERO);
        let words = if ago < time::Duration::minutes(1) {
            "just now".to_owned()
        } else {
            format!("{} ago", format::countdown(ago))
        };
        Some(format!("read {words}"))
    }

    /// How every CLI's windows share `width` cells, so their meters and numbers line up from
    /// row to row: label, meter, percent, reset time, twice over when two fit. On a narrow
    /// card the reset times go first, then every window but each CLI's fullest.
    fn window_layout(&self, width: usize) -> WindowLayout {
        let readings = || self.usage.plans.iter().flat_map(|plan| &plan.windows);
        let reset_slot = readings()
            .filter_map(|reading| reading.resets_at)
            .map(|at| str_width(&format!("resets {}", self.reset_time(at))))
            .max()
            .unwrap_or(0);
        let most = self
            .usage
            .plans
            .iter()
            .map(|plan| plan.windows.len())
            .max()
            .unwrap_or(1)
            .max(1);
        // The label in its four cells, then the meter, then the percent in four more.
        let fixed = 4 + 4;
        let meter_for = |count: usize, resets: bool| {
            let notes = if resets && reset_slot > 0 {
                2 + reset_slot
            } else {
                0
            };
            (width.saturating_sub(WINDOW_GAP * (count - 1)) / count).saturating_sub(fixed + notes)
        };
        let (count, resets) = [(most, true), (most, false), (1, true), (1, false)]
            .into_iter()
            .find(|(count, resets)| meter_for(*count, *resets) >= LEAST_METER)
            .unwrap_or((1, false));
        WindowLayout {
            all: count == most,
            meter: meter_for(count, resets),
            reset_slot: if resets { reset_slot } else { 0 },
        }
    }

    /// A CLI's windows as one row, and when one has reached your own limit, the words that
    /// say so and who takes its tasks.
    fn windows(
        &self,
        kit: &Kit<'_>,
        plan: &PlanUsage,
        layout: &WindowLayout,
    ) -> (Vec<Span<'static>>, Option<Vec<Span<'static>>>) {
        let fullest = plan
            .windows
            .iter()
            .max_by(|a, b| used_of(a).total_cmp(&used_of(b)));
        // At your own limit the window that reached it is what matters, with who takes over.
        let reached = self
            .usage
            .plan_limit
            .zip(fullest)
            .filter(|(limit, reading)| used_of(reading) >= *limit)
            .map(|(_, reading)| reading);
        let shown: Vec<&QuotaReading> = if layout.all && reached.is_none() {
            plan.windows.iter().collect()
        } else {
            fullest.into_iter().collect()
        };
        let mut row = Vec::new();
        for (k, reading) in shown.into_iter().enumerate() {
            if k > 0 {
                row.push(Span::raw(" ".repeat(WINDOW_GAP)));
            }
            let level = self.level(reading);
            row.push(kit.dim(ljust(&window_label(reading), 4)));
            row.extend(bars::half_meter(
                kit,
                self.shown(used_of(reading)),
                layout.meter,
                level.fill_role(),
                self.usage.plan_limit.filter(|_| !self.left),
            ));
            row.push(Span::styled(
                format!("{:>4}", self.percent(reading)),
                kit.bold(level.strong_role()),
            ));
            if layout.reset_slot > 0 {
                let resets = reading
                    .resets_at
                    .map_or_else(String::new, |at| format!("resets {}", self.reset_time(at)));
                row.push(kit.dim(format!("  {}", ljust(&resets, layout.reset_slot))));
            }
        }
        let words = reached.map(|_| {
            let taker = plan
                .moved_to
                .as_ref()
                .map_or_else(String::new, |taker| format!(" · {taker} takes its tasks"));
            vec![kit.span(format!("limit reached{taker}"), Role::Warning)]
        });
        (row, words)
    }

    fn spend_body(&self, kit: &Kit<'_>, width: usize, full: bool) -> Drawn {
        let shares = &self.usage.by_agent;
        let cents = |usd: f64| (usd.max(0.0) * 10_000.0).round() as u64;
        let total = cents(self.usage.total.cost_usd);
        let limit = self.usage.spend_limit.map(cents).filter(|limit| *limit > 0);
        let stack = Stack {
            parts: shares
                .iter()
                .map(|share| (cents(share.spent.cost_usd), cli_role(&share.vendor)))
                .collect(),
            // Against a limit the bar is the limit, and what's unspent stays track.
            total: limit.map_or(total, |limit| limit.max(total)).max(1),
            tick: None,
            held_from: None,
            track: Role::BarTrack,
        };
        let entries: Vec<Entry> = if full {
            shares
                .iter()
                .map(|share| Entry {
                    role: cli_role(&share.vendor),
                    name: self
                        .names
                        .worker(&WorkerRef::new(share.agent.clone(), share.model.clone())),
                    amount: format::spend(&share.spent),
                    share: share_of(share.spent.cost_usd, self.usage.total.cost_usd),
                })
                .collect()
        } else {
            Vec::new()
        };
        bodies::half_stack(kit, &stack, &entries, width, 3)
    }

    fn tokens_body(&self, kit: &Kit<'_>, width: usize, full: bool) -> Drawn {
        let total = &self.usage.total;
        let cached = total.cached_tokens.min(total.input_tokens);
        let parts = [
            ("from cache", cached, Role::Denominator),
            ("new input", total.input_tokens - cached, Role::AgentBlue),
            ("output", total.output_tokens, Role::AgentOrange),
        ];
        let all = total.total_tokens();
        let stack = Stack {
            parts: parts
                .iter()
                .map(|(_, tokens, role)| (*tokens, *role))
                .collect(),
            total: all.max(1),
            tick: None,
            held_from: None,
            track: Role::BarTrack,
        };
        let entries: Vec<Entry> = if full {
            parts
                .iter()
                .map(|(name, tokens, role)| Entry {
                    role: *role,
                    name: (*name).to_owned(),
                    amount: format::tokens(*tokens),
                    share: share_of(*tokens as f64, all as f64),
                })
                .collect()
        } else {
            Vec::new()
        };
        bodies::half_stack(kit, &stack, &entries, width, 2)
    }

    fn history_total(&self) -> argonauts_core::Spend {
        self.usage
            .history
            .iter()
            .fold(argonauts_core::Spend::default(), |sum, day| {
                sum.plus(&day.spent)
            })
    }

    fn history_body(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let days = &self.usage.history;
        let top = days
            .iter()
            .map(|day| day.spent.cost_usd)
            .fold(0.0, f64::max);
        let chart = Chart {
            values: days.iter().map(|day| day.spent.cost_usd).collect(),
            label: format::usd(top),
            letters: days
                .iter()
                .map(|day| day.day.weekday().to_string()[..1].to_owned())
                .collect(),
            role: Role::AgentOrange,
            last_role: Role::Warning,
        };
        let mut body = bodies::columns(kit, &chart);
        for line in &mut body.lines {
            line.spans = truncate_spans(&line.spans, width);
        }
        body
    }

    /// The period's workflows, newest first: state, how many of its tasks are merged with a
    /// meter of them, what it spent, and how long it ran. Rows sit back to back, so the
    /// meters are half meters. On a narrow card the meter gives way, then the name.
    fn workflow_rows(&self, kit: &Kit<'_>, width: usize, full: bool) -> Drawn {
        let runs = &self.usage.runs;
        let mut body = Drawn::new();
        if runs.is_empty() {
            body.push(vec![kit.dim(format!(
                "No workflow ran {}",
                period_words(self.usage.period)
            ))]);
            return body;
        }
        let most = if full { WORKFLOW_ROWS } else { 3 };
        let rows: Vec<WorkflowRow> = runs
            .iter()
            .take(most)
            .map(|run| self.workflow_row(run))
            .collect();
        let widest = |f: fn(&WorkflowRow) -> &str| {
            rows.iter().map(|row| str_width(f(row))).max().unwrap_or(0)
        };
        // A text column takes two cells after its widest value, and none when it's empty.
        let column = |widest: usize| if widest == 0 { 0 } else { widest + 2 };
        let (word_w, tasks_w) = (
            column(widest(|row| &row.word)),
            column(widest(|row| &row.tasks)),
        );
        let (spend_w, took_w) = (
            column(widest(|row| &row.spend)),
            column(widest(|row| &row.took)),
        );
        let rest = 2 + word_w + tasks_w + spend_w + took_w;
        let name_w = column(widest(|row| &row.name));
        let meter_w = match width.saturating_sub(rest + name_w) {
            room if room >= LEAST_METER => room,
            _ => 0,
        };
        let name_w = name_w.min(width.saturating_sub(rest + meter_w));
        let states = States::default();
        for row in &rows {
            let name = truncate_spans(&[Span::raw(row.name.clone())], name_w.saturating_sub(2));
            let name: String = name.iter().map(|span| span.content.as_ref()).collect();
            let mut line = vec![
                states.span(kit, row.state),
                Span::raw(format!(" {}", ljust(&name, name_w))),
                kit.span(ljust(&row.word, word_w), row.state.role()),
                kit.dim(ljust(&row.tasks, tasks_w)),
            ];
            match row.done {
                Some(done) if meter_w > 0 => {
                    line.extend(bars::half_meter(kit, done, meter_w, row.state.role(), None))
                }
                _ => line.push(Span::raw(" ".repeat(meter_w))),
            }
            if spend_w > 0 {
                line.push(kit.strong(rjust(&row.spend, spend_w)));
            }
            if took_w > 0 {
                line.push(kit.dim(rjust(&row.took, took_w)));
            }
            body.push(truncate_spans(&line, width));
        }
        if runs.len() > rows.len() {
            body.push(vec![kit.dim(format!(
                "  and {} more · /workflows lists them",
                runs.len() - rows.len()
            ))]);
        }
        body
    }

    fn workflow_row(&self, run: &RunSummary) -> WorkflowRow {
        let progress = self
            .usage
            .progress
            .iter()
            .find(|progress| progress.run == run.run);
        let planned = progress.filter(|progress| progress.tasks > 0);
        WorkflowRow {
            state: run_state(run),
            name: run.name.clone().unwrap_or_else(|| run.title.clone()),
            word: run_word(run).to_owned(),
            tasks: planned.map_or_else(String::new, |progress| {
                format!("{}/{} tasks", progress.tasks_done, progress.tasks)
            }),
            done: planned
                .map(|progress| f64::from(progress.tasks_done) / f64::from(progress.tasks)),
            spend: format::spend(&run.spent),
            took: progress
                .and_then(|progress| progress.started)
                .map_or_else(String::new, |started| self.took(run, started)),
        }
    }

    /// How long a run has taken: to now while it runs, counted in minutes so the number
    /// keeps still, and to its last event once it has stopped.
    fn took(&self, run: &RunSummary, started: OffsetDateTime) -> String {
        let running = run.live && matches!(run.state, RunState::Running | RunState::Pausing);
        let end = if running { self.now } else { run.updated };
        let elapsed = std::time::Duration::try_from(end - started).unwrap_or_default();
        let minutes = elapsed.as_secs() / 60;
        if running && (1..60).contains(&minutes) {
            format!("{minutes}m")
        } else {
            format::duration_slot(elapsed)
        }
    }
}

/// The most workflows the card lists in full.
const WORKFLOW_ROWS: usize = 8;

/// One row of the workflows card, in words.
struct WorkflowRow {
    state: State,
    name: String,
    word: String,
    /// `1/4 tasks`, or nothing for a run that planned none.
    tasks: String,
    /// The share of its tasks that are merged.
    done: Option<f64>,
    spend: String,
    took: String,
}

/// The cards in words, one fact a line, for a screen reader: each CLI's windows, then what
/// was spent and by whom.
pub fn spoken(usage: &UsageSummary, names: &DisplayNames) -> Vec<String> {
    let mut lines = Vec::new();
    for plan in &usage.plans {
        let name = match &plan.plan {
            Some(tier) => format!("{} {tier}", plan.name),
            None => plan.name.clone(),
        };
        if plan.pays_per_token {
            lines.push(format!("{name}: pays per token, no plan to track"));
        } else if plan.windows.is_empty() {
            lines.push(format!("{name}: no reading yet"));
        }
        for reading in &plan.windows {
            let used = match reading.used {
                Some(used) => format!("{:.0}% used", used.max(0.0) * 100.0),
                None if reading.status == QuotaStatus::Rejected => "out".to_owned(),
                None => "not known".to_owned(),
            };
            lines.push(format!("{name}, {} window: {used}", window_label(reading)));
        }
    }
    let period = period_words(usage.period);
    lines.push(format!(
        "spend {period}: {}, {} tokens",
        format::spend(&usage.total),
        format::tokens(usage.total.total_tokens())
    ));
    for share in &usage.by_agent {
        let who = names.worker(&WorkerRef::new(share.agent.clone(), share.model.clone()));
        lines.push(format!("{who}: {}", format::spend(&share.spent)));
    }
    lines.push(format!("workflows {period}: {}", usage.runs.len()));
    lines
}

/// A window's share used, with one that's out counted as full.
pub(super) fn used_of(reading: &QuotaReading) -> f64 {
    match reading.used {
        Some(used) => used,
        None if reading.status == QuotaStatus::Rejected => 1.0,
        None => 0.0,
    }
}

/// `5h`, `7d`, or the window's own name where it's another.
pub(super) fn window_label(reading: &QuotaReading) -> String {
    let window = reading.window.as_deref().unwrap_or_default();
    if window.contains("five") || window.contains("5h") {
        "5h".to_owned()
    } else if window.contains("seven") || window.contains("week") || window.contains("7d") {
        "7d".to_owned()
    } else if window.contains("day") {
        "day".to_owned()
    } else if window.is_empty() {
        "now".to_owned()
    } else {
        window.replace('_', "-")
    }
}

/// The CLI's name as the bottom line has it, where a header has little room.
fn short_name(plan: &PlanUsage) -> String {
    match plan.agent.as_str() {
        "claude-code" => "Claude".to_owned(),
        "codex" => "Codex".to_owned(),
        "antigravity-cli" => "agy".to_owned(),
        _ => plan.name.clone(),
    }
}

/// Cells between a CLI's two windows.
const WINDOW_GAP: usize = 4;

/// How the plan usage rows lay their windows out, the same for every CLI.
struct WindowLayout {
    /// Every window shows, rather than each CLI's fullest alone.
    all: bool,
    /// Cells of each meter.
    meter: usize,
    /// Cells for a reset time, or 0 when they've given way.
    reset_slot: usize,
}

fn share_of(part: f64, whole: f64) -> String {
    if whole > 0.0 {
        format::percent(part / whole)
    } else {
        String::new()
    }
}

fn count(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

/// How a period reads after an amount.
pub fn period_words(period: UsagePeriod) -> &'static str {
    match period {
        UsagePeriod::Today => "today",
        UsagePeriod::Week => "this week",
        UsagePeriod::Month => "this month",
        UsagePeriod::All => "in all",
    }
}

fn run_state(run: &RunSummary) -> State {
    match run.state {
        RunState::Running | RunState::Pausing => State::Working,
        RunState::Parked if run.needs_you() => State::NeedsYou,
        RunState::Parked => State::Waiting,
        RunState::Merged | RunState::PullRequest | RunState::BranchLeft => State::Done,
        RunState::Crashed | RunState::Failed => State::Failed,
        _ => State::Stopped,
    }
}

fn run_word(run: &RunSummary) -> &'static str {
    match run.state {
        RunState::Running => "running",
        RunState::Pausing => "pausing",
        RunState::Parked if run.needs_you() => "needs you",
        RunState::Parked => "waiting",
        RunState::Crashed => "crashed",
        RunState::Merged => "merged",
        RunState::PullRequest => "pull request",
        RunState::BranchLeft => "branch kept",
        RunState::Failed => "failed",
        _ => "stopped",
    }
}

#[cfg(test)]
#[path = "cards_tests.rs"]
mod tests;
