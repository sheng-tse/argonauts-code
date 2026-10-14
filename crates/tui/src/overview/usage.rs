//! The Usage tab: a card per CLI with each plan window's bar, share and reset, read live from
//! the CLI for the account signed in, and a sub-tab per CLI laid out like Claude Code's own
//! `/usage`. A reset is said two ways, the time left and the clock: `resets in 2h 28m · today
//! 10:40pm`, `Fri 12:15am` for early tomorrow, `Tue Oct 13, 10:00pm` later on.

use argonauts_core::api::{PlanUsage, UsageSummary};
use argonauts_core::plan_usage::{Behavior, ExtraUsage, LiveUsage, LiveWindow};
use ratatui::text::Span;
use time::{Date, Duration, OffsetDateTime, Time};

use super::cards::cli_role;
use crate::format;
use crate::text::{sanitize, str_width, truncate_spans, wrap_spans};
use crate::theme::Role;
use crate::widgets::bars;
use crate::widgets::{Drawn, Frame, Kit, Level, ljust, rjust, side_by_side, spread};

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;

/// Below this many columns the cards stack, each kept whole.
pub(super) const SIDE_BY_SIDE_FROM: usize = 130;
/// At most this many cards side by side.
const CARDS_ACROSS: usize = 3;
/// Columns between two cards side by side.
const CARD_GAP: usize = 1;
/// A window's label column on a card, as wide as the widest label within these, and where
/// its reset line starts under it.
const LABEL: usize = 10;
const LABEL_MOST: usize = 14;
const PERCENT: usize = 5;
/// From this share of a window used, its line turns amber.
const NEAR_LIMIT: f64 = 0.9;
/// A reset before this hour tomorrow reads as the weekday and the time alone.
const DAWN: u8 = 6;
/// The widest bar on a CLI's own page, as Claude Code's `/usage` keeps it.
const PAGE_BAR: usize = 50;
/// Claude Code's `/usage` words when it can't read, said for every CLI. The CLI's own page
/// says why after them.
const LOAD_FAILED: &str = "failed to load usage data";

/// What the tab draws from.
pub(super) struct UsageTab<'a> {
    pub usage: &'a UsageSummary,
    pub now: OffsetDateTime,
    /// 0 for every CLI, else the CLI's place in the plans plus one.
    pub cli: usize,
    /// `u`: what's left of each window rather than what's used.
    pub left: bool,
    /// How long the conversation has gone on, and how much of that its agent worked.
    pub talked: Option<(std::time::Duration, std::time::Duration)>,
    /// Lines the conversation's edits added and removed.
    pub changed: (usize, usize),
}

impl UsageTab<'_> {
    /// The sub-tab picked, which the plans may have fewer of since it was picked.
    fn picked(&self) -> Option<&PlanUsage> {
        self.cli
            .checked_sub(1)
            .and_then(|at| self.usage.plans.get(at))
    }

    /// `All CLIs  Claude Code  Codex  Antigravity CLI` and which is picked, for the panel's
    /// row of sub-tabs, which fills the picked one as it fills every row's.
    pub fn sub_tabs(&self) -> (Vec<String>, usize) {
        let picked = if self.picked().is_some() { self.cli } else { 0 };
        let names = std::iter::once("All CLIs".to_owned())
            .chain(self.usage.plans.iter().map(|plan| plan.name.clone()))
            .collect();
        (names, picked)
    }

    /// `read live 8:12pm · r reads again`, or that a read is on its way.
    pub fn read_words(&self) -> String {
        let plans = &self.usage.plans;
        if plans.iter().any(|plan| plan.reading) {
            return "reading live… · r reads again".to_owned();
        }
        let newest = plans
            .iter()
            .filter_map(|plan| plan.live.as_ref())
            .map(|live| live.read_at)
            .max();
        let Some(at) = newest else {
            return "r reads again".to_owned();
        };
        let clock = clock_words(at, self.now, true);
        let clock = clock.strip_prefix("today ").unwrap_or(&clock);
        format!("read live {clock} · r reads again")
    }

    /// The tab's blocks, top to bottom.
    pub fn blocks(&self, kit: &Kit<'_>, width: usize) -> Vec<Drawn> {
        if let Some(plan) = self.picked() {
            return self.page(kit, plan, width);
        }
        let mut blocks = Vec::new();
        blocks.extend(self.conversation(kit, width));
        if self.usage.plans.is_empty() {
            blocks.push(line(vec![
                kit.dim(" No CLI in the crew reports a plan yet"),
            ]));
        }
        blocks.extend(self.cards(kit, width));
        blocks.extend(self.most_usage(kit, width));
        blocks
    }

    /// `This conversation · $0.42 · 52m 30s, 4m 12s of it working · +182 −21 lines · 612k
    /// tokens`.
    fn conversation(&self, kit: &Kit<'_>, width: usize) -> Option<Drawn> {
        let spent = self.usage.conversation.as_ref()?;
        let mut parts = vec![format::spend(spent)];
        if let Some((long, worked)) = self.talked {
            parts.push(format!(
                "{}, {} of it working",
                talk_words(long),
                talk_words(worked)
            ));
        }
        if self.changed != (0, 0) {
            let (added, removed) = self.changed;
            parts.push(format!("+{added} −{removed} lines"));
        }
        let tokens = spent.total_tokens();
        if tokens > 0 {
            parts.push(format!("{} tokens", format::tokens(tokens)));
        }
        let spans = vec![
            Span::raw(" "),
            kit.strong("This conversation"),
            kit.dim(format!(" · {}", parts.join(" · "))),
        ];
        Some(line(truncate_spans(&spans, width)))
    }

    /// `Most of your usage · last 24h · 95% long context · …`, from the CLI that says.
    fn most_usage(&self, kit: &Kit<'_>, width: usize) -> Option<Drawn> {
        let behaviors = self
            .usage
            .plans
            .iter()
            .filter_map(|plan| plan.live.as_ref()?.behaviors.as_ref())
            .find(|behaviors| !behaviors.day.is_empty() || !behaviors.week.is_empty())?;
        let (span, list) = if behaviors.day.is_empty() {
            ("last 7d", &behaviors.week)
        } else {
            ("last 24h", &behaviors.day)
        };
        let mut spans = vec![
            Span::raw(" "),
            kit.strong("Most of your usage"),
            kit.dim(format!(" · {span} · ")),
            Span::raw(behavior_list(list)),
            kit.dim(" · enter for more"),
        ];
        if crate::text::spans_width(&spans) > width {
            spans.truncate(4);
        }
        Some(line(truncate_spans(&spans, width)))
    }

    /// The cards, three to a row from [`SIDE_BY_SIDE_FROM`] columns, else one under another.
    /// Cards in a row are drawn as tall as the tallest, as boxes in a grid are.
    fn cards(&self, kit: &Kit<'_>, width: usize) -> Vec<Drawn> {
        let plans = &self.usage.plans;
        let across = if width >= SIDE_BY_SIDE_FROM {
            CARDS_ACROSS.min(plans.len()).max(1)
        } else {
            1
        };
        let room = width.saturating_sub(1);
        let card_width = (room.saturating_sub(CARD_GAP * (across - 1))) / across;
        let inner = Frame::view(card_width + 2).inner();
        let label = self.label_width();
        plans
            .chunks(across)
            .map(|row| {
                let mut bodies: Vec<Drawn> = row
                    .iter()
                    .map(|plan| self.card_body(kit, plan, inner, label))
                    .collect();
                let tallest = bodies.iter().map(Drawn::height).max().unwrap_or(0);
                for body in &mut bodies {
                    while body.height() < tallest {
                        body.push(Vec::new());
                    }
                }
                let frame = Frame {
                    left: 0,
                    width: card_width,
                    toggle: None,
                    active: false,
                };
                let mut drawn = bodies.into_iter().map(|body| frame.draw(kit, body));
                let mut joined = drawn.next().unwrap_or_default();
                for (at, card) in drawn.enumerate() {
                    let column = (at + 1) * (card_width + CARD_GAP);
                    joined = side_by_side(joined, card, column);
                }
                joined.indent(1);
                joined
            })
            .collect()
    }

    /// `● Claude Code` with `Max · in use` at the right, then each window: its bar and share,
    /// and under it when it resets.
    fn card_body(&self, kit: &Kit<'_>, plan: &PlanUsage, inner: usize, label: usize) -> Drawn {
        let mut body = Drawn::new();
        let mut name = vec![
            kit.span("● ", cli_role(&plan.agent)),
            kit.strong(plan.name.clone()),
        ];
        name = truncate_spans(&name, inner);
        let notes = notes(plan);
        let right = if notes.is_empty() {
            Vec::new()
        } else {
            vec![kit.dim(notes.join(" · "))]
        };
        body.push(spread(name, right, inner));
        let Some(live) = self.shown(plan) else {
            body.push(Vec::new());
            for row in self.none_rows(kit, plan, inner, &[]) {
                body.push(row);
            }
            return body;
        };
        let mut rows: Vec<(Vec<Span<'static>>, String)> = Vec::new();
        for window in live.windows.iter().filter(|window| !window.page_only) {
            let resets = window
                .resets_at
                .map(|at| reset_words(at, self.now))
                .unwrap_or_default();
            let under = match &window.note {
                Some(note) if resets.is_empty() => note.clone(),
                Some(note) => format!("{note} · {resets}"),
                None => resets,
            };
            let used = self.used_now(window);
            let row = self.window_row(kit, plan, used, &window.label, inner, label);
            rows.push((row, under));
        }
        match &live.extra {
            Some(extra) if extra.enabled => {
                let row = self.window_row(kit, plan, extra.share(), "extra", inner, label);
                rows.push((row, extra_words(extra, self.now, " of ")));
            }
            Some(_) => {
                let row = vec![kit.dim(ljust("extra", label + 1)), kit.dim("off")];
                rows.push((row, String::new()));
            }
            None => {}
        }
        // A card's reset lines start together, under the bars where they fit.
        let indent = rows
            .iter()
            .filter(|(_, under)| !under.is_empty())
            .map(|(_, under)| inner.saturating_sub(str_width(under)))
            .min()
            .unwrap_or(0)
            .min(label + 1);
        for (row, under) in rows {
            body.push(Vec::new());
            body.push(row);
            push_under(&mut body, kit, &under, inner, indent);
        }
        if let Some(taker) = &plan.moved_to {
            body.push(Vec::new());
            body.push(vec![
                kit.span(format!("its tasks go to {taker}"), Role::Warning),
            ]);
        }
        body
    }

    /// The label column, as wide as the widest label on any card, so the bars line up.
    fn label_width(&self) -> usize {
        self.usage
            .plans
            .iter()
            .filter_map(|plan| plan.live.as_ref())
            .flat_map(|live| &live.windows)
            .filter(|window| !window.page_only)
            .map(|window| str_width(&window.label))
            .max()
            .unwrap_or(0)
            .clamp(LABEL, LABEL_MOST)
    }

    /// The reading to draw, when there's one of the account signed in.
    fn shown<'a>(&self, plan: &'a PlanUsage) -> Option<&'a LiveUsage> {
        plan.live
            .as_ref()
            .filter(|live| !live.windows.is_empty() || live.extra.is_some())
    }

    /// Why a card has no reading to draw, with the CLI's own words when a read failed, then
    /// when the reading kept from before was read, and for whom.
    fn none_rows(
        &self,
        kit: &Kit<'_>,
        plan: &PlanUsage,
        width: usize,
        indent: &[Span<'static>],
    ) -> Vec<Vec<Span<'static>>> {
        let why = self.why_none(plan);
        let said = match plan.failed.as_deref().filter(|_| why == LOAD_FAILED) {
            Some(failed) => format!("{why}: {}", sanitize(failed)),
            None => why.to_owned(),
        };
        let mut rows: Vec<Vec<Span<'static>>> = wrap_spans(&[kit.dim(said)], width, indent, indent)
            .into_iter()
            .map(|row| row.spans)
            .collect();
        if let Some(last) = &plan.last {
            let mut words = format!("last read {}", clock_words(last.read_at, self.now, true));
            if let Some(account) = &last.account {
                words.push_str(&format!(" for {}", sanitize(account)));
            }
            rows.extend(
                wrap_spans(&[kit.dim(words)], width, indent, indent)
                    .into_iter()
                    .map(|row| row.spans),
            );
        }
        rows
    }

    /// A window's share, unknown once its reset has passed: it starts again from nothing, and
    /// only a new read says how much is used since.
    fn used_now(&self, window: &LiveWindow) -> Option<f64> {
        window
            .used
            .filter(|_| window.resets_at.is_none_or(|at| at > self.now))
    }

    fn why_none(&self, plan: &PlanUsage) -> &'static str {
        if plan.pays_per_token {
            "pays per token, so there is no plan to track"
        } else if plan.down {
            "can't be reached"
        } else if plan.reading {
            "reading live…"
        } else if plan.failed.is_some() {
            LOAD_FAILED
        } else if plan.live.is_some() {
            "no plan windows for this account"
        } else {
            "no live reading yet"
        }
    }

    /// `session    ██████░░░░░░  14%`, amber from 90% and red once full.
    fn window_row(
        &self,
        kit: &Kit<'_>,
        plan: &PlanUsage,
        used: Option<f64>,
        name: &str,
        inner: usize,
        label: usize,
    ) -> Vec<Span<'static>> {
        let level = level(used);
        let fill = match level {
            Level::Ok => cli_role(&plan.agent),
            other => other.fill_role(),
        };
        let label_role = match level {
            Level::Ok => Role::Dim,
            other => other.number_role(),
        };
        let shown = used.map(|used| self.shown_share(used));
        let meter = inner.saturating_sub(label + 1 + 1 + PERCENT).max(4);
        let mut row = vec![
            kit.span(ljust(&cut(name, label), label), label_role),
            Span::raw(" "),
        ];
        row.extend(bars::half_meter(
            kit,
            shown.unwrap_or(0.0),
            meter,
            fill,
            None,
        ));
        row.push(Span::raw(" "));
        row.push(Span::styled(
            rjust(&percent(shown), PERCENT),
            kit.bold(level.strong_role()),
        ));
        row
    }

    fn shown_share(&self, used: f64) -> f64 {
        if self.left {
            (1.0 - used).clamp(0.0, 1.0)
        } else {
            used
        }
    }

    /// One CLI's page, as Claude Code's `/usage` lays its own out: each window's title, its
    /// bar with the share, and when it resets; then the extra usage, and what made up most of
    /// the usage where the CLI says.
    fn page(&self, kit: &Kit<'_>, plan: &PlanUsage, width: usize) -> Vec<Drawn> {
        let inner = width.saturating_sub(2);
        let bar = inner.saturating_sub(12).clamp(4, PAGE_BAR);
        let mut blocks = Vec::new();
        let mut head = vec![
            Span::raw(" "),
            kit.span("● ", cli_role(&plan.agent)),
            kit.strong(plan.name.clone()),
        ];
        let notes = notes(plan);
        if !notes.is_empty() {
            head.push(kit.dim(format!(" · {}", notes.join(" · "))));
        }
        blocks.push(line(truncate_spans(&head, width)));
        let Some(live) = self.shown(plan) else {
            // The CLI's own words, which can run longer than the page is wide.
            let mut block = Drawn::new();
            for row in self.none_rows(kit, plan, width, &[Span::raw(" ")]) {
                block.push(row);
            }
            blocks.push(block);
            return blocks;
        };
        for window in &live.windows {
            blocks.push(self.page_window(kit, plan, window, bar, width));
        }
        if let Some(extra) = &live.extra {
            let title = if plan.agent == "claude-code" {
                "Usage credits"
            } else {
                "Extra usage"
            };
            let mut section = Drawn::new();
            section.push(vec![Span::raw(" "), kit.strong(title)]);
            if extra.enabled {
                let share = extra.share();
                let mut row = vec![Span::raw(" ")];
                row.extend(bars::half_meter(
                    kit,
                    share.unwrap_or(0.0),
                    bar,
                    cli_role(&plan.agent),
                    None,
                ));
                row.push(kit.span(format!("  {}", spent_words(extra, " / ")), Role::Bright));
                section.push(truncate_spans(&row, width));
                let resets = month_start_after(self.now);
                let words = format!(
                    "Resets in {} · {}",
                    left_words(resets - self.now),
                    clock_words(resets, self.now, false)
                );
                section.push(vec![Span::raw(" "), kit.dim(words)]);
            } else {
                section.push(vec![Span::raw(" "), kit.dim(format!("{title} are off"))]);
            }
            blocks.push(section);
        }
        if let Some(behaviors) = &live.behaviors
            && (!behaviors.day.is_empty() || !behaviors.week.is_empty())
        {
            let mut section = Drawn::new();
            section.push(vec![
                Span::raw(" "),
                kit.strong("What's contributing to your limits usage?"),
            ]);
            for (span, list) in [("Last 24h", &behaviors.day), ("Last 7d", &behaviors.week)] {
                if !list.is_empty() {
                    let words = format!(" {span}: {}", behavior_list(list));
                    section.push(truncate_spans(&[Span::raw(words)], width));
                }
            }
            section.push(vec![
                Span::raw(" "),
                kit.dim("Approximate, from the sessions on this machine. The parts overlap."),
            ]);
            blocks.push(section);
        }
        blocks
    }

    fn page_window(
        &self,
        kit: &Kit<'_>,
        plan: &PlanUsage,
        window: &LiveWindow,
        bar: usize,
        width: usize,
    ) -> Drawn {
        let used = self.used_now(window);
        let level = level(used);
        let fill = match level {
            Level::Ok => cli_role(&plan.agent),
            other => other.fill_role(),
        };
        let shown = used.map(|used| self.shown_share(used));
        let mut section = Drawn::new();
        section.push(vec![Span::raw(" "), kit.strong(window.title.clone())]);
        let mut row = vec![Span::raw(" ")];
        row.extend(bars::half_meter(kit, shown.unwrap_or(0.0), bar, fill, None));
        let word = if self.left { "left" } else { "used" };
        row.push(Span::styled(
            format!("  {} {word}", percent(shown)),
            kit.bold(level.strong_role()),
        ));
        section.push(truncate_spans(&row, width));
        // No note before the reset: the title names the window's length, which a card's note
        // stands in for.
        if let Some(at) = window.resets_at {
            let mut words = reset_words(at, self.now);
            if let Some(first) = words.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            section.push(truncate_spans(&[Span::raw(" "), kit.dim(words)], width));
        }
        section
    }
}

/// The plan and whether the main agent runs on it: `Max · in use`.
fn notes(plan: &PlanUsage) -> Vec<String> {
    let mut notes: Vec<String> = plan.plan.iter().cloned().collect();
    if plan.in_use {
        notes.push("in use".to_owned());
    }
    if plan.down {
        notes.push("down".to_owned());
    }
    notes
}

fn level(used: Option<f64>) -> Level {
    match used {
        Some(used) if used >= 1.0 => Level::Danger,
        Some(used) if used >= NEAR_LIMIT => Level::Warn,
        _ => Level::Ok,
    }
}

fn percent(share: Option<f64>) -> String {
    match share {
        Some(share) => format!("{:.0}%", (share * 100.0).clamp(0.0, 100.0).floor()),
        None => format::UNKNOWN.to_owned(),
    }
}

/// The line under a window at `indent`, or split at its ` · ` where even the card's whole
/// width is too narrow, so neither the time left nor the clock is cut.
fn push_under(body: &mut Drawn, kit: &Kit<'_>, words: &str, inner: usize, indent: usize) {
    if words.is_empty() {
        return;
    }
    if indent + str_width(words) <= inner {
        body.push(vec![
            Span::raw(" ".repeat(indent)),
            kit.dim(words.to_owned()),
        ]);
        return;
    }
    for part in words.splitn(2, " · ") {
        let indent = indent.min(inner.saturating_sub(str_width(part)));
        let row = vec![Span::raw(" ".repeat(indent)), kit.dim(part.to_owned())];
        body.push(truncate_spans(&row, inner));
    }
}

fn line(spans: Vec<Span<'static>>) -> Drawn {
    let mut drawn = Drawn::new();
    drawn.push(spans);
    drawn
}

fn cut(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_owned();
    }
    let spans = truncate_spans(&[Span::raw(text.to_owned())], width);
    spans.iter().map(|span| span.content.as_ref()).collect()
}

/// `95% long context · 90% subagent-heavy`.
fn behavior_list(list: &[Behavior]) -> String {
    list.iter()
        .map(|behavior| {
            format!(
                "{:.0}% {}",
                (behavior.share * 100.0).round(),
                behavior_words(&behavior.key)
            )
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Claude Code's names for what made up the usage, in a few words.
fn behavior_words(key: &str) -> String {
    match key {
        "long_context" => "long context".to_owned(),
        "subagent_heavy" => "subagent-heavy".to_owned(),
        "high_parallel" => "4+ sessions at once".to_owned(),
        "cache_miss" => "cache misses".to_owned(),
        "cron" => "scheduled".to_owned(),
        other => other.replace('_', " "),
    }
}

/// `$0 of $100 · resets in 23d · Sun Nov 1`. Extra usage resets on the first of the month,
/// as Claude Code counts it.
fn extra_words(extra: &ExtraUsage, now: OffsetDateTime, of: &str) -> String {
    let resets = month_start_after(now);
    format!(
        "{} · resets in {} · {}",
        spent_words(extra, of),
        left_words(resets - now),
        clock_words(resets, now, false)
    )
}

/// `$0 of $100` on a card and `$0 / $100 spent` on the CLI's page, as Claude Code words it,
/// or `$12.40 spent` with no cap.
fn spent_words(extra: &ExtraUsage, of: &str) -> String {
    let spent = money(extra.spent, &extra.currency);
    match extra.limit {
        Some(limit) if of == " / " => format!("{spent} / {} spent", money(limit, &extra.currency)),
        Some(limit) => format!("{spent}{of}{}", money(limit, &extra.currency)),
        None => format!("{spent} spent"),
    }
}

fn money(amount: f64, currency: &str) -> String {
    let number = if (amount - amount.round()).abs() < 0.005 {
        format!("{:.0}", amount.round())
    } else {
        format!("{amount:.2}")
    };
    if currency.eq_ignore_ascii_case("USD") {
        format!("${number}")
    } else {
        format!("{number} {}", currency.to_ascii_uppercase())
    }
}

/// Midnight starting next month, on the clock `now` is shown in.
fn month_start_after(now: OffsetDateTime) -> OffsetDateTime {
    let date = now.date();
    let (year, month) = match date.month().next() {
        time::Month::January => (date.year() + 1, time::Month::January),
        next => (date.year(), next),
    };
    let first = Date::from_calendar_date(year, month, 1).unwrap_or(date);
    first.with_time(Time::MIDNIGHT).assume_offset(now.offset())
}

/// `52m 30s`, or `1h 5m` from the hour on.
fn talk_words(span: std::time::Duration) -> String {
    let seconds = span.as_secs();
    match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}

/// `resets in 2h 28m · today 10:40pm`, or, once it has passed, when it did.
pub(super) fn reset_words(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let clock = clock_words(at, now, true);
    if at <= now {
        format!("reset {clock} · reading again")
    } else {
        format!("resets in {} · {clock}", left_words(at - now))
    }
}

/// The time left: minutes in the last hour, hours and minutes within the day, days and hours
/// within the week, then days.
pub(super) fn left_words(left: Duration) -> String {
    let seconds = left.whole_seconds().max(1);
    let minutes = (seconds + 59) / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    if minutes < 24 * 60 {
        return match (minutes / 60, minutes % 60) {
            (hours, 0) => format!("{hours}h"),
            (hours, minutes) => format!("{hours}h {minutes}m"),
        };
    }
    let hours = (seconds + 1800) / 3600;
    if hours < 7 * 24 {
        return match (hours / 24, hours % 24) {
            (days, 0) => format!("{days}d"),
            (days, hours) => format!("{days}d {hours}h"),
        };
    }
    format!("{}d", (seconds + 43_200) / 86_400)
}

/// When on the clock: `today 10:40pm` within today, the weekday and time before dawn
/// tomorrow (`Fri 12:15am`), and the weekday, date and time after that (`Tue Oct 13,
/// 10:00pm`). Without the time, `today` or the weekday and date (`Sun Nov 1`).
pub(super) fn clock_words(at: OffsetDateTime, now: OffsetDateTime, with_time: bool) -> String {
    let at = at.to_offset(now.offset());
    let weekday = &at.weekday().to_string()[..3];
    let time = twelve_hour(at);
    let days = (at.date() - now.date()).whole_days();
    match (days, with_time) {
        (0, true) => format!("today {time}"),
        (0, false) => "today".to_owned(),
        (1, true) if at.hour() < DAWN => format!("{weekday} {time}"),
        (_, true) => format!("{weekday} {} {}, {time}", month(at), at.day()),
        (_, false) => format!("{weekday} {} {}", month(at), at.day()),
    }
}

fn month(at: OffsetDateTime) -> String {
    at.month().to_string()[..3].to_owned()
}

/// `10:40pm`, `12:15am`, `10:00pm`.
pub(super) fn twelve_hour(at: OffsetDateTime) -> String {
    let suffix = if at.hour() < 12 { "am" } else { "pm" };
    let hour = match at.hour() % 12 {
        0 => 12,
        hour => hour,
    };
    format!("{hour}:{:02}{suffix}", at.minute())
}
