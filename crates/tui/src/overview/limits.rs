//! The Limits tab's own pieces, under the spend and plan usage cards: each
//! task of the run the screen follows with what it spent, against the limit of a task where
//! you set one, and the limits themselves as a list, where Enter opens one to change.
//!
//! The list comes from the controller as the rows `/limits` lists, so a limit added there
//! shows here without this file knowing its name.

use std::collections::BTreeMap;

use argonauts_core::TaskId;
use argonauts_core::app::SettingRow;
use argonauts_core::events::task::TaskState;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use ratatui::text::Span;

use super::status::task_state;
use crate::format;
use crate::kit::{Drawn, Frame, Header, Kit, Level, States, bars, ljust, rjust, spread};
use crate::state::RunState;
use crate::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::views::settings::words::value_words;

/// From this share of a limit on, a task's spend shows amber.
const NEAR_LIMIT: f64 = 0.9;
/// The most tasks the card lists before it counts the rest.
const TASK_ROWS: usize = 8;
/// The most limits the list shows at once. The rest scroll under the picked one.
const LIMIT_ROWS: usize = 8;
/// The least cells a task's meter takes before the column goes.
const LEAST_METER: usize = 8;
/// Under this width the list leaves out each limit's key.
const KEYS_FROM: usize = 90;

/// What one task of a run has spent, as its sessions reported it.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskSpend {
    pub task: TaskId,
    pub title: String,
    pub state: TaskState,
    pub usd: f64,
    pub tokens: u64,
}

/// Each task of `run` with what its sessions spent on it, in the run's order.
pub fn tasks_of(run: &RunState) -> Vec<TaskSpend> {
    let mut spent: BTreeMap<&TaskId, (f64, u64)> = BTreeMap::new();
    for session in run.sessions.values() {
        if let Some(task) = &session.task {
            let sum = spent.entry(task).or_default();
            sum.0 += session.cost_usd.unwrap_or(0.0);
            sum.1 += session.tokens_in + session.tokens_out;
        }
    }
    run.tasks
        .values()
        .map(|task| {
            let (usd, tokens) = spent.get(&task.id).copied().unwrap_or_default();
            TaskSpend {
                task: task.id.clone(),
                title: task.title.clone(),
                state: task.state.unwrap_or(TaskState::Waiting),
                usd,
                tokens,
            }
        })
        .collect()
}

/// What the tab's own pieces draw from.
#[derive(Debug, Clone, Copy)]
pub struct LimitsCards<'a> {
    pub tasks: &'a [TaskSpend],
    /// The limit of a task that has none of its own, when you set one.
    pub task_limit: Option<LimitAmount>,
    pub limits: &'a [SettingRow],
}

impl LimitsCards<'_> {
    /// A row a task: its mark, its id, its title, a meter, and what it spent. With a limit
    /// the meter fills toward it. With none it shows the task's share of the run's spend,
    /// so the costly task still stands out.
    pub fn tasks(&self, kit: &Kit<'_>, frame: Frame) -> Option<Drawn> {
        if self.tasks.is_empty() {
            return None;
        }
        let inner = frame.inner();
        let total: f64 = self.tasks.iter().map(|task| task.usd).sum();
        let measured = self.measured_limit();
        let count = match self.tasks.len() {
            1 => "1 task".to_owned(),
            n => format!("{n} tasks"),
        };
        let limit = match measured {
            Some(limit) => format!("{} a task", limit_words(&limit)),
            None => "no limit set".to_owned(),
        };
        let header = Header {
            amount: Some(format::usd(total)),
            tail: vec![kit.dim(format!(" · {count} · {limit}"))],
            ..Header::new("tasks", Role::Success)
        };
        let mut body = Drawn::new();
        body.push(header.draw(kit, inner));

        let shown = &self.tasks[..self.tasks.len().min(TASK_ROWS)];
        let widest = |cell: &dyn Fn(&TaskSpend) -> String| {
            shown
                .iter()
                .map(|task| str_width(&cell(task)))
                .max()
                .unwrap_or(0)
        };
        let id_w = widest(&|task| task.task.to_string()) + 2;
        let spend_w = widest(&|task| format::usd(task.usd)) + 2;
        // The mark, the id, the spend, and the percent in its five cells.
        let fixed = 2 + id_w + spend_w + 5;
        let title_w = (widest(&|task| clean(&task.title)) + 2).min(inner.saturating_sub(fixed) / 2);
        let meter_w = match inner.saturating_sub(fixed + title_w + 2) {
            room if room >= LEAST_METER => room,
            _ => 0,
        };
        let states = States::default();
        for task in shown {
            let (share, level) = self.share(task, measured, total);
            let title = truncate_spans(&[Span::raw(clean(&task.title))], title_w.max(2) - 2);
            let title: String = title.iter().map(|span| span.content.as_ref()).collect();
            let mut row = vec![
                states.span(kit, task_state(task.state)),
                Span::raw(" "),
                kit.dim(ljust(task.task.as_ref(), id_w)),
                Span::raw(ljust(&title, title_w)),
            ];
            if meter_w > 0 {
                row.extend(bars::half_meter(
                    kit,
                    share,
                    meter_w,
                    level.fill_role(),
                    None,
                ));
                row.push(Span::raw("  "));
            }
            row.push(Span::raw(rjust(&format::usd(task.usd), spend_w)));
            row.push(kit.span(
                rjust(&format!("{:.0}%", share * 100.0), 5),
                level.strong_role(),
            ));
            body.push(truncate_spans(&row, inner));
        }
        if self.tasks.len() > shown.len() {
            body.push(vec![
                kit.dim(format!("  and {} more", self.tasks.len() - shown.len())),
            ]);
        }
        Some(frame.draw(kit, body))
    }

    /// The limit a task's spend can be read against: dollars or tokens. Time and plan
    /// usage aren't counted by task on this screen.
    fn measured_limit(&self) -> Option<LimitAmount> {
        self.task_limit.filter(|limit| {
            limit.amount > 0.0 && matches!(limit.unit, LimitUnit::Usd | LimitUnit::Tokens)
        })
    }

    /// How far the task's meter fills, and its level. Without a limit nothing is near one.
    fn share(&self, task: &TaskSpend, limit: Option<LimitAmount>, total: f64) -> (f64, Level) {
        let Some(limit) = limit else {
            let share = if total > 0.0 { task.usd / total } else { 0.0 };
            return (share, Level::Ok);
        };
        let used = match limit.unit {
            LimitUnit::Tokens => task.tokens as f64,
            _ => task.usd,
        };
        let share = used / limit.amount;
        let level = if share >= 1.0 {
            Level::Danger
        } else if share >= NEAR_LIMIT {
            Level::Warn
        } else {
            Level::Ok
        };
        (share, level)
    }

    /// The limits, a row each: what it limits, its value in words, where the value came
    /// from, and its key. `picked` is the row the keys are on.
    pub fn list(&self, kit: &Kit<'_>, width: usize, picked: usize, mark: &str) -> Option<Drawn> {
        if self.limits.is_empty() {
            return None;
        }
        let rows = self.limits;
        let picked = picked.min(rows.len() - 1);
        let any_set = rows.iter().any(is_set);
        let note = if any_set {
            "at a limit the task falls back, or the run stops"
        } else {
            "none set, so nothing stops a run"
        };
        let mut block = Drawn::new();
        let left = vec![kit.strong(" Limits"), kit.dim(format!(" · {note}"))];
        let right = vec![kit.dim("enter to change ")];
        // On a narrow panel the hint goes, since the keys under the tab say it too.
        block.push(if spans_width(&left) + spans_width(&right) < width {
            spread(left, right, width)
        } else {
            truncate_spans(&left, width)
        });

        let labels: Vec<String> = rows.iter().map(|row| label(&row.key)).collect();
        let values: Vec<String> = rows.iter().map(|row| value(&row.key, &row.value)).collect();
        let layers: Vec<&str> = rows.iter().map(layer).collect();
        let column = |cells: &mut dyn Iterator<Item = usize>| cells.max().unwrap_or(0) + 3;
        let label_w = column(&mut labels.iter().map(|cell| str_width(cell)));
        let value_w = column(&mut values.iter().map(|cell| str_width(cell)));
        let layer_w = column(&mut layers.iter().map(|cell| str_width(cell)));
        let first = (picked + 1).saturating_sub(LIMIT_ROWS);
        for (at, row) in rows.iter().enumerate().skip(first).take(LIMIT_ROWS) {
            let on = at == picked;
            let mut line = vec![if on {
                kit.span(format!(" {mark} "), Role::Question)
            } else {
                Span::raw("   ")
            }];
            line.push(if on {
                kit.strong(ljust(&labels[at], label_w))
            } else {
                Span::raw(ljust(&labels[at], label_w))
            });
            let cell = ljust(&values[at], value_w);
            line.push(match (is_cap(&row.key), is_set(row)) {
                (true, true) => kit.strong(cell),
                (true, false) => kit.dim(cell),
                (false, _) => Span::raw(cell),
            });
            line.push(kit.dim(ljust(layers[at], layer_w)));
            if width >= KEYS_FROM {
                line.push(kit.dim(row.key.clone()));
            }
            block.push(truncate_spans(&line, width));
        }
        if rows.len() > LIMIT_ROWS {
            block.push(vec![kit.dim(format!(
                "   {} of {} · ↑↓ shows the rest",
                picked + 1,
                rows.len()
            ))]);
        }
        Some(block)
    }
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// `$1.00` or `200k tokens`.
fn limit_words(limit: &LimitAmount) -> String {
    match limit.unit {
        LimitUnit::Tokens => format!("{} tokens", format::tokens(limit.amount.max(0.0) as u64)),
        _ => format::usd(limit.amount),
    }
}

/// The key below its section: `run_usd` of `limits.run_usd`.
fn name(key: &str) -> &str {
    key.split_once('.').map_or(key, |(_, name)| name)
}

/// A cap holds an amount and stops something once it's reached. The other keys say how the
/// caps behave.
fn is_cap(key: &str) -> bool {
    let name = name(key);
    let amount = ["usd", "tokens", "minutes", "plan_usage"];
    name.starts_with("spend") || amount.iter().any(|unit| name.ends_with(unit))
}

/// A cap you set, or one the defaults set for you.
fn is_set(row: &SettingRow) -> bool {
    is_cap(&row.key) && !row.value.trim().is_empty()
}

/// What the limit limits, in words. A key this list doesn't know reads as its own name.
fn label(key: &str) -> String {
    let name = name(key);
    let known = match name {
        "run_usd" | "spend_per_run" => "Spend for a run",
        "usd_per_day" | "spend_per_day" => "Spend for a day",
        "run_tokens" => "Tokens for a run",
        "run_plan_usage" => "Plan usage for a run",
        "run_minutes" => "Time for a run",
        "per_task_usd" => "Spend for a task",
        "per_task_tokens" => "Tokens for a task",
        "per_task_plan_usage" => "Plan usage for a task",
        "per_task_minutes" => "Time for a task",
        "plan_usage" => "Plan usage",
        "stop_runs" => "Stop at a limit",
        "soft_stop" => "Wrap up from",
        "when_reached" => "When a limit is reached",
        "from_history" => "Task limits from history",
        "history_min" => "Runs before history counts",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    if let Some(line) = name.strip_prefix("lines.") {
        return format!("Share for {}", line.replace('_', " "));
    }
    let words = name.replace(['_', '.'], " ");
    let mut letters = words.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => words,
    }
}

/// The value in words and percents: `$3.00`, `2M tokens`, `60m`, `85% of a limit`, never
/// `0.85`.
fn value(key: &str, toml: &str) -> String {
    let name = name(key);
    let text = toml.trim();
    if text.is_empty() {
        return "not set".to_owned();
    }
    let percent = |of: &str| {
        let share = text.parse::<f64>().ok()?;
        Some(format!("{:.0}% of {of}", share * 100.0))
    };
    let number = text.parse::<f64>().ok();
    let words = if name.ends_with("usd") || name.starts_with("spend") {
        number.map(format::usd)
    } else if name.ends_with("tokens") {
        number.map(|tokens| format!("{} tokens", format::tokens(tokens.max(0.0) as u64)))
    } else if name.ends_with("minutes") {
        number.map(|minutes| format!("{minutes:.0}m"))
    } else if name.ends_with("plan_usage") {
        percent("a window")
    } else if name == "soft_stop" {
        percent("a limit")
    } else if name.starts_with("lines.") {
        percent("the run's")
    } else {
        match text {
            "true" => Some("on".to_owned()),
            "false" => Some("off".to_owned()),
            _ => None,
        }
    };
    words.unwrap_or_else(|| value_words(text).replace('_', " "))
}

/// Where the value came from, in a word or two.
fn layer(row: &SettingRow) -> &str {
    match row.layer.as_str() {
        "built-in" => "default",
        "global" => "everywhere",
        "project" => "this project",
        "project-local" => "just you",
        "workflow" => "the workflow",
        "flag" => "--set",
        "session" => "this session",
        other => other,
    }
}

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;
