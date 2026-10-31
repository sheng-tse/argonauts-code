//! The usage cards: plan usage, spend, tokens, history and workflows, each drawn from one
//! [`UsageSummary`]. The Usage tab stacks them in full, and the band above the input pins any
//! of them, compact or folded to a line.
//!
//! Plan usage rows sit back to back, so their meters are half meters, which never merge into
//! a slab the way block meters would, and the tokens bar is a half stack of the same height.
//! The spend card, one row a model under a thin bar, is drawn in `spend.rs`.

use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::limits::{PlanUsageReading, PlanUsageStatus};
use argonauts_core::runs::api::{PlanUsage, RunState, RunSummary, UsagePeriod, UsageSummary};
use argonauts_core::settings::ui::ModName;
use ratatui::text::Span;
use time::{Date, OffsetDateTime};

use super::cards::spend::SpendCard;
use crate::base::format::{self, ClockStyle};
use crate::base::kit::bars::{self, Stack};
use crate::base::kit::{
    Badge, Chart, Drawn, Entry, Form, Frame, Header, Kit, State, States, bodies, ljust, rjust,
    spread,
};
use crate::base::text::{spans_width, str_width, truncate_spans};
use crate::base::theme::Role;

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
    pub fn of(name: ModName) -> Option<Self> {
        match name {
            ModName::PlanUsage => Some(UsageCard::PlanUsage),
            ModName::Spend => Some(UsageCard::Spend),
            ModName::Tokens => Some(UsageCard::Tokens),
            ModName::History => Some(UsageCard::History),
            ModName::Runs => Some(UsageCard::Workflows),
            _ => None,
        }
    }

    pub fn name(self) -> ModName {
        match self {
            UsageCard::PlanUsage => ModName::PlanUsage,
            UsageCard::Spend => ModName::Spend,
            UsageCard::Tokens => ModName::Tokens,
            UsageCard::History => ModName::History,
            UsageCard::Workflows => ModName::Runs,
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
            UsageCard::PlanUsage | UsageCard::Spend | UsageCard::Tokens | UsageCard::Workflows => {
                Role::Question
            }
            UsageCard::History => Role::AgentOrange,
        }
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
            UsageCard::PlanUsage => self.window_rows(kit, inner, full),
            UsageCard::Spend => self.spend().body(kit, inner, full),
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
            UsageCard::PlanUsage => match self.fullest_window() {
                Some((plan, row)) => {
                    let level = self.row_level(&row);
                    let used = self.shown(row.used.unwrap_or(0.0));
                    vec![Span::styled(
                        format!("{} {} {:.0}%", plan.name, row.words, used * 100.0),
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

    fn spend(&self) -> SpendCard<'_> {
        SpendCard {
            usage: self.usage,
            names: self.names,
            today: self.now.date(),
        }
    }

    fn header(&self, kit: &Kit<'_>, card: UsageCard, width: usize) -> Vec<Span<'static>> {
        let left = Header::new(card.title(), card.glyph_role()).left(kit);
        if card == UsageCard::Spend {
            return self.spend().header(kit, left, width);
        }
        let usage = self.usage;
        let period = period_words(usage.period);
        // What gives way first on a narrow card, then what stays.
        let (opening, keep): (Vec<Span<'static>>, Vec<Span<'static>>) = match card {
            UsageCard::PlanUsage => {
                let limit = match usage.plan_limit {
                    Some(limit) => vec![
                        kit.dim("your limit "),
                        kit.strong(format!("{:.0}%", limit * 100.0)),
                        kit.dim(" · highest "),
                    ],
                    None => vec![kit.dim("no limit set · highest ")],
                };
                match self.fullest_window() {
                    Some((plan, row)) => {
                        let level = self.row_level(&row);
                        // The badge keeps room for `100%` at its left, which parts it from
                        // the name.
                        let mut keep = vec![kit.strong(format!("{} {}", plan.name, row.words))];
                        let used = row.used.unwrap_or(0.0);
                        keep.extend(Badge::percent(self.shown(used), level).spans(kit));
                        (limit, keep)
                    }
                    None => (Vec::new(), vec![kit.dim("no reading yet")]),
                }
            }
            // The spend card writes its own header.
            UsageCard::Spend => (Vec::new(), Vec::new()),
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
        let right = if spans_width(&opening) + spans_width(&keep) <= room {
            [opening, keep].concat()
        } else {
            truncate_spans(&keep, room)
        };
        spread(left, right, width)
    }

    /// A share of a window as shown: what's used, or what's left of it.
    fn shown(&self, used: f64) -> f64 {
        if self.left {
            (1.0 - used).max(0.0)
        } else {
            used
        }
    }

    /// Why a CLI has no meters to show, when it hasn't: out, down, billed per token, or not
    /// read yet. The words follow its name.
    pub(super) fn no_windows(&self, kit: &Kit<'_>, plan: &PlanUsage) -> Option<Vec<Span<'static>>> {
        let moved = plan
            .moved_to
            .as_ref()
            .map(|taker| kit.dim(format!(" · its tasks moved to {taker}")));
        let out = plan
            .windows
            .iter()
            .find(|reading| reading.status == PlanUsageStatus::Rejected && !reading.wrap_up);
        let mut words = if plan.local {
            vec![kit.dim("  local, no plan")]
        } else if plan.pays_per_token {
            vec![kit.dim(" pays per token, so there is no plan to track")]
        } else if let Some(reading) = out {
            let until = reading
                .resets_at
                .map_or_else(String::new, |at| format!(" until {}", self.reset_time(at)));
            vec![kit.span(format!("  out{until}"), Role::Danger)]
        } else if plan.down {
            vec![kit.span("  can't be reached", Role::Danger)]
        } else if super::cards::plan_windows::window_rows(plan).is_empty() {
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
        format::reset_time(at, self.now, ClockStyle::now())
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
                .map(|(name, tokens, role)| {
                    Entry::new(*role, *name, format::tokens(*tokens))
                        .shared(share_of(*tokens as f64, all as f64))
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut body = bodies::half_stack(kit, &stack, &entries, width, 2);
        if let Some(note) = self.no_cache_note().filter(|_| full) {
            body.push(truncate_spans(&[kit.dim(note)], width));
        }
        body
    }

    /// `Ollama reports no cache, so local input counts in full`: a local server says none of
    /// a prompt came from cache, so each step counts its whole prompt again, and the input
    /// reads far larger than what any one context held.
    fn no_cache_note(&self) -> Option<String> {
        let mut servers: Vec<String> = Vec::new();
        let uncached = self.usage.by_agent.iter().filter(|share| {
            share.local && share.spent.input_tokens > 0 && share.spent.cached_tokens == 0
        });
        for share in uncached {
            // A CLI names the server in its model's route, as OpenCode's `ollama/…`; an
            // endpoint is the server itself.
            let server = share
                .model
                .as_deref()
                .and_then(|model| model.split_once('/'))
                .map_or(share.agent_id.as_str(), |(server, _)| server);
            let name = self.names.cli(server);
            if !servers.contains(&name) {
                servers.push(name);
            }
        }
        let (named, verb) = match servers.as_slice() {
            [] => return None,
            [one] => (one.clone(), "reports"),
            [rest @ .., last] => (format!("{} and {last}", rest.join(", ")), "report"),
        };
        Some(format!(
            "{named} {verb} no cache, so local input counts in full"
        ))
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
            last_role: Role::Warn,
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
        if plan.local {
            lines.push(format!("{name}: local, no plan"));
        } else if plan.pays_per_token {
            lines.push(format!("{name}: pays per token, no plan to track"));
        } else if plan.windows.is_empty() {
            lines.push(format!("{name}: no reading yet"));
        }
        for reading in &plan.windows {
            let used = match reading.used {
                Some(used) => format!("{:.0}% used", used.max(0.0) * 100.0),
                None if reading.status == PlanUsageStatus::Rejected => "out".to_owned(),
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
    let today = usage.history.last().map_or(Date::MIN, |day| day.day);
    let spend = SpendCard {
        usage,
        names,
        today,
    };
    lines.extend(spend.spoken());
    lines.push(format!("workflows {period}: {}", usage.runs.len()));
    lines
}

/// A window's share used, with one that's out counted as full.
pub(super) fn used_of(reading: &PlanUsageReading) -> f64 {
    match reading.used {
        Some(used) => used,
        None if reading.status == PlanUsageStatus::Rejected => 1.0,
        None => 0.0,
    }
}

/// `5h`, `7d`, or the window's own name where it's another.
pub(super) fn window_label(reading: &PlanUsageReading) -> String {
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
        RunState::Waiting if run.needs_you() => State::NeedsYou,
        RunState::Waiting => State::Waiting,
        RunState::Merged | RunState::PullRequest | RunState::BranchLeft => State::Done,
        RunState::Failed => State::Failed,
        _ => State::Stopped,
    }
}

fn run_word(run: &RunSummary) -> &'static str {
    match run.state {
        RunState::Running => "running",
        RunState::Pausing => "pausing",
        RunState::Waiting if run.needs_you() => "needs you",
        RunState::Waiting => "waiting",
        RunState::NeverStarted => "never started",
        RunState::FromAnotherBuild => "from an older Argonauts",
        RunState::Merged => "merged",
        RunState::PullRequest => "pull request",
        RunState::BranchLeft => "branch kept",
        RunState::Failed => "failed",
        _ => "stopped",
    }
}

pub(crate) mod plan_windows;
pub(crate) mod spend;
#[cfg(test)]
mod tests;
