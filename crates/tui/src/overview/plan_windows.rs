//! The plan usage card's rows: one per window, grouped under each CLI, in fixed columns so
//! every row lines up. The CLI's name and plan, the window's name in words, a bar in the
//! CLI's colour, the share after it, and when the window resets in the Usage tab's words:
//!
//! ```text
//! Claude Code      Max    5-hour             ━━━━━━━━━━━━━━━━━━━━━━   0%   resets in 2h 28m · today 10:40pm
//!                         week               ━━━━━━━━━━━━━━━━━━━━━━  36%   resets in 5d 2h · Tue Oct 13, 10:00pm
//! ```
//!
//! The windows are the ones the CLI's live reading names, by the words the Usage tab shows,
//! never the keys a CLI files them under. On a narrow card the reset words shorten and then
//! go, then the plan, then the CLI's name shortens; the bar keeps at least a few cells.

use argonauts_core::api::PlanUsage;
use argonauts_core::limits::{QuotaReading, QuotaStatus};
use argonauts_core::plan_usage::LiveWindow;
use ratatui::text::Span;
use time::OffsetDateTime;

use super::cards::{UsageCards, cli_role};
use super::usage::{left_words, reset_words};
use crate::text::{str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::bars;
use crate::widgets::{Drawn, Kit, Level, ljust, rjust};

/// The widest bar a window gets.
const BAR: usize = 22;
/// The narrowest bar before the columns around it give way.
const LEAST_BAR: usize = 10;
/// The share, right-aligned: `  36%`, ` 100%`.
const SHARE: usize = 5;
/// The widest window name before its length shortens, `5-hour` to `5h`, and then is cut.
const NAME_MOST: usize = 16;
/// Blank cells after the CLI's name, after its plan, and after a window's name.
const NAME_GAP: usize = 2;
const PLAN_GAP: usize = 3;
const WORDS_GAP: usize = 3;
/// Blank cells between the share and the reset words.
const RESET_GAP: usize = 3;
/// Said of a window that has no reset time yet: it starts with the next request.
pub(super) const NOT_STARTED: &str = "starts with your next message";

/// One window of one CLI, as the card draws it.
#[derive(Debug, Clone)]
pub(super) struct WindowRow {
    /// `5-hour`, `week`, `Fable week`.
    pub words: String,
    pub used: Option<f64>,
    pub resets_at: Option<OffsetDateTime>,
    /// The CLI says it's out.
    pub out: bool,
    /// The CLI says it's near its end.
    pub warned: bool,
}

/// A CLI's windows in words: from its live reading where it has one, every window that
/// reading names, else from the readings it reported as it worked.
pub(super) fn window_rows(plan: &PlanUsage) -> Vec<WindowRow> {
    let mut rows: Vec<WindowRow> = match plan.live.as_ref().filter(|live| !live.windows.is_empty())
    {
        Some(live) => live.windows.iter().map(live_row).collect(),
        None => plan.windows.iter().map(reading_row).collect(),
    };
    // Never the same name twice on one CLI: a second one says which bucket it is.
    let mut seen = std::collections::HashSet::new();
    for (at, row) in rows.iter_mut().enumerate() {
        if !seen.insert(row.words.clone()) {
            row.words = format!("{} {}", row.words, at + 1);
            seen.insert(row.words.clone());
        }
    }
    rows
}

fn live_row(window: &LiveWindow) -> WindowRow {
    WindowRow {
        words: window_words(&window.label, window.note.as_deref()),
        used: window.used,
        resets_at: window.resets_at,
        out: false,
        warned: false,
    }
}

fn reading_row(reading: &QuotaReading) -> WindowRow {
    let key = reading.window.as_deref().unwrap_or("now");
    WindowRow {
        words: key_words(key),
        used: reading.used,
        resets_at: reading.resets_at,
        out: reading.status == QuotaStatus::Rejected,
        warned: reading.status == QuotaStatus::Warning,
    }
}

/// A window's key in words: `five_hour` reads `5-hour`, `seven_day_opus` reads `Opus week`,
/// and `model:Fable` files a model's own window under its name.
fn key_words(key: &str) -> String {
    let key = key.rsplit(':').next().unwrap_or(key);
    let spaced = key
        .replace("five_hour", "5-hour")
        .replace("seven_day", "week")
        .replace('_', " ");
    let mut words: Vec<String> = spaced
        .split_whitespace()
        .map(|word| length_word(word).unwrap_or(word).to_owned())
        .collect();
    // The length goes after the name, as in `Fable week`.
    if words.len() > 1 && length_word(&words[0]).is_some() {
        let length = words.remove(0);
        words.push(length);
    }
    if let Some(first) = words.first_mut()
        && length_word(first).is_none()
    {
        let mut chars = first.chars();
        if let Some(head) = chars.next() {
            *first = head.to_uppercase().chain(chars).collect();
        }
    }
    words.join(" ")
}

/// A window's name in plain words, its length said the same way for every CLI: `session`
/// and `5h` read `5-hour`, `weekly` and `7d` read `week`. A label without its length takes
/// it from the note beside it, as `Claude, GPT` with `5-hour`.
pub(super) fn window_words(label: &str, note: Option<&str>) -> String {
    let mut words: Vec<String> = label
        .split_whitespace()
        .map(|word| length_word(word).unwrap_or(word).to_owned())
        .collect();
    let has_length = words.iter().any(|word| length_word(word).is_some());
    if !has_length && let Some(note) = note {
        words.push(length_word(note).unwrap_or(note).to_owned());
    }
    words.join(" ")
}

fn length_word(word: &str) -> Option<&'static str> {
    Some(match word {
        "session" | "5h" | "five hour" | "five_hour" | "5-hour" => "5-hour",
        "week" | "weekly" | "7d" | "7-day" | "seven_day" | "seven day" => "week",
        "day" | "daily" | "1d" => "day",
        _ => return None,
    })
}

/// `Claude, GPT 5-hour` in at most `width` cells: its length shortened to `5h`, then cut.
fn fitted_words(words: &str, width: usize) -> String {
    if str_width(words) <= width {
        return words.to_owned();
    }
    let short = words.replace("5-hour", "5h");
    if str_width(&short) <= width {
        return short;
    }
    truncate_spans(&[Span::raw(short)], width)
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// When a window resets, in the Usage tab's words, or that it starts with the next request.
pub(super) fn when_words(resets_at: Option<OffsetDateTime>, now: OffsetDateTime) -> String {
    match resets_at {
        Some(at) => reset_words(at, now),
        None => NOT_STARTED.to_owned(),
    }
}

/// How much of the reset words a row keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resets {
    /// `resets in 2h 28m · today 10:40pm`.
    Whole,
    /// `resets in 2h 28m`.
    Short,
    None,
}

/// The columns every row shares, in cells.
#[derive(Debug, Clone, Copy)]
struct Columns {
    name: usize,
    /// The CLI's name as the bottom line shortens it.
    short_names: bool,
    plan: usize,
    words: usize,
    bar: usize,
    resets: Resets,
}

impl UsageCards<'_> {
    /// The plan usage card's body in `width` cells: a row per window, grouped by CLI, the
    /// groups a blank row apart when `spaced`.
    pub(super) fn window_rows(&self, kit: &Kit<'_>, width: usize, spaced: bool) -> Drawn {
        let plans = &self.usage.plans;
        let mut body = Drawn::new();
        if plans.is_empty() {
            body.push(vec![kit.dim("No CLI in the crew reports a plan yet")]);
            return body;
        }
        let rows: Vec<Vec<WindowRow>> = plans.iter().map(window_rows).collect();
        let columns = self.columns(plans, &rows, width);
        if spaced {
            body.push(Vec::new());
        }
        for (at, (plan, windows)) in plans.iter().zip(&rows).enumerate() {
            if at > 0 && spaced {
                body.push(Vec::new());
            }
            let head = self.cli_cell(kit, plan, &columns);
            if let Some(why) = self.no_windows(kit, plan) {
                let mut row = head;
                let mut why = why.into_iter();
                if let Some(first) = why.next() {
                    row.push(Span::styled(
                        first.content.trim_start().to_owned(),
                        first.style,
                    ));
                }
                row.extend(why);
                body.push(truncate_spans(&row, width));
                continue;
            }
            for (k, window) in windows.iter().enumerate() {
                let mut row = if k == 0 {
                    head.clone()
                } else {
                    vec![Span::raw(" ".repeat(columns.name + columns.plan))]
                };
                row.extend(self.window_cells(kit, plan, window, &columns));
                body.push(truncate_spans(&row, width));
            }
            if let Some(words) = self.limit_reached(plan, windows) {
                let mut row = vec![Span::raw(" ".repeat(columns.name + columns.plan))];
                row.push(kit.span(words, Role::Warning));
                body.push(truncate_spans(&row, width));
            }
        }
        body
    }

    /// The columns that fit `width`, giving way in turn: the reset words shorten, then go,
    /// then the plan goes, then the CLI's names shorten.
    fn columns(&self, plans: &[PlanUsage], rows: &[Vec<WindowRow>], width: usize) -> Columns {
        let widest = |names: &mut dyn Iterator<Item = usize>| names.max().unwrap_or(0);
        let name = widest(&mut plans.iter().map(|plan| str_width(&plan.name))) + NAME_GAP;
        let short = widest(&mut plans.iter().map(|plan| str_width(&short_name(plan)))) + NAME_GAP;
        let plan_width = widest(
            &mut plans
                .iter()
                .filter_map(|plan| plan.plan.as_deref())
                .map(str_width),
        );
        let plan = if plan_width > 0 {
            plan_width + PLAN_GAP
        } else {
            0
        };
        let words = widest(&mut rows.iter().flatten().map(|row| str_width(&row.words)))
            .min(NAME_MOST)
            + WORDS_GAP;
        let reset = |resets: Resets| {
            let longest = rows
                .iter()
                .flatten()
                .map(|row| str_width(&self.reset_cell(row, resets)))
                .max()
                .unwrap_or(0);
            if longest == 0 { 0 } else { RESET_GAP + longest }
        };
        let tries = [
            (name, false, plan, Resets::Whole),
            (name, false, plan, Resets::Short),
            (name, false, plan, Resets::None),
            (name, false, 0, Resets::None),
            (short, true, 0, Resets::None),
        ];
        let fits = |(name, short_names, plan, resets): (usize, bool, usize, Resets)| {
            let fixed = name + plan + words + SHARE + reset(resets);
            let bar = width.saturating_sub(fixed).min(BAR);
            (bar >= LEAST_BAR).then_some(Columns {
                name,
                short_names,
                plan,
                words,
                bar,
                resets,
            })
        };
        tries.into_iter().find_map(fits).unwrap_or_else(|| {
            let fixed = short + words + SHARE;
            Columns {
                name: short,
                short_names: true,
                plan: 0,
                words,
                bar: width.saturating_sub(fixed).clamp(4, BAR),
                resets: Resets::None,
            }
        })
    }

    /// The CLI's name in bold and its plan dim, each in its column.
    fn cli_cell(&self, kit: &Kit<'_>, plan: &PlanUsage, columns: &Columns) -> Vec<Span<'static>> {
        let name = if columns.short_names {
            short_name(plan)
        } else {
            plan.name.clone()
        };
        let room = columns.name.saturating_sub(NAME_GAP);
        let name = fitted(&name, room);
        let mut cell = vec![
            kit.strong(name.clone()),
            Span::raw(" ".repeat(columns.name - str_width(&name))),
        ];
        if columns.plan > 0 {
            let tier = fitted(plan.plan.as_deref().unwrap_or(""), columns.plan - PLAN_GAP);
            cell.push(kit.dim(ljust(&tier, columns.plan)));
        }
        cell
    }

    /// `5-hour   ━━━━━━━━   0%   resets in 2h 28m · today 10:40pm`.
    fn window_cells(
        &self,
        kit: &Kit<'_>,
        plan: &PlanUsage,
        window: &WindowRow,
        columns: &Columns,
    ) -> Vec<Span<'static>> {
        let level = self.window_level(window);
        let fill = match level {
            Level::Ok => cli_role(&plan.agent),
            other => other.fill_role(),
        };
        let used = window.used.unwrap_or(if window.out { 1.0 } else { 0.0 });
        let shown = self.shown_share(used);
        let words = fitted_words(&window.words, columns.words - WORDS_GAP);
        let words_role = match level {
            Level::Ok => Role::Text,
            other => other.number_role(),
        };
        let mut row = vec![kit.span(ljust(&words, columns.words), words_role)];
        row.extend(bars::thin_bar(kit, shown, columns.bar, fill));
        let share = match window.used {
            Some(_) => format!("{:.0}%", shown.max(0.0) * 100.0),
            None if window.out => "out".to_owned(),
            None => crate::format::UNKNOWN.to_owned(),
        };
        let share_role = match level {
            Level::Ok => Role::Text,
            other => other.strong_role(),
        };
        bars::then(
            &mut row,
            vec![Span::styled(rjust(&share, SHARE), kit.bold(share_role))],
        );
        if columns.resets != Resets::None {
            row.push(Span::raw(" ".repeat(RESET_GAP)));
            row.push(kit.dim(self.reset_cell(window, columns.resets)));
        }
        row
    }

    fn reset_cell(&self, window: &WindowRow, resets: Resets) -> String {
        let words = when_words(window.resets_at, self.now);
        match resets {
            Resets::Whole => words,
            Resets::Short => match window.resets_at {
                Some(at) if at > self.now => format!("resets in {}", left_words(at - self.now)),
                _ => words.split(" · ").next().unwrap_or_default().to_owned(),
            },
            Resets::None => String::new(),
        }
    }

    /// Amber near the end of a window or at your own limit, red once it's out.
    fn window_level(&self, window: &WindowRow) -> Level {
        let used = window.used.unwrap_or(if window.out { 1.0 } else { 0.0 });
        let limit = self.usage.plan_limit.unwrap_or(0.9).min(0.9);
        if window.out || used >= 1.0 {
            Level::Danger
        } else if used >= limit || window.warned {
            Level::Warn
        } else {
            Level::Ok
        }
    }

    fn shown_share(&self, used: f64) -> f64 {
        if self.left {
            (1.0 - used).max(0.0)
        } else {
            used
        }
    }

    /// At your own limit, the words that say so and who takes the CLI's tasks.
    fn limit_reached(&self, plan: &PlanUsage, windows: &[WindowRow]) -> Option<String> {
        let limit = self.usage.plan_limit?;
        let fullest = windows
            .iter()
            .map(|window| window.used.unwrap_or(0.0))
            .fold(0.0, f64::max);
        (fullest >= limit).then(|| {
            let taker = plan
                .moved_to
                .as_ref()
                .map_or_else(String::new, |taker| format!(" · {taker} takes its tasks"));
            format!("limit reached{taker}")
        })
    }

    /// The fullest window across every CLI that has some left, by its words, for the card's
    /// header and its folded line.
    pub(super) fn fullest_window(&self) -> Option<(&PlanUsage, WindowRow)> {
        self.usage
            .plans
            .iter()
            .flat_map(|plan| window_rows(plan).into_iter().map(move |row| (plan, row)))
            .filter(|(_, row)| !row.out)
            .max_by(|a, b| {
                let used = |row: &WindowRow| row.used.unwrap_or(0.0);
                used(&a.1).total_cmp(&used(&b.1))
            })
    }

    /// The level of the fullest window, for the header's badge.
    pub(super) fn row_level(&self, row: &WindowRow) -> Level {
        self.window_level(row)
    }
}

/// The CLI's name as the bottom line has it, where a card has little room.
pub(super) fn short_name(plan: &PlanUsage) -> String {
    match plan.agent.as_str() {
        "claude-code" => "Claude".to_owned(),
        "codex" => "Codex".to_owned(),
        "antigravity-cli" => "agy".to_owned(),
        _ => plan.name.clone(),
    }
}

fn fitted(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_owned();
    }
    truncate_spans(&[Span::raw(text.to_owned())], width)
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[cfg(test)]
#[path = "plan_windows_tests.rs"]
mod tests;
