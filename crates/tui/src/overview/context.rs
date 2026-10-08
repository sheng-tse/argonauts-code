//! The Context tab's cards: the picked session's context as a bar with
//! the point where it rotates, and a row for every live session.
//!
//! The conversation's own session is drawn by category where its CLI counts them, with
//! what fills the categories that list their items, as Claude Code's /context does. Any
//! other session is drawn as used and free.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::context::{ContextItem, ContextPart as Part};
use argonauts_core::views::{ContextRow, ContextView as ContextData};
use ratatui::text::Span;

use crate::format;
use crate::state::ContextBreakdown;
use crate::text::{sanitize_line, str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{
    ContextCard, ContextPart, Drawn, Form, Frame, Header, Kit, Level, Levels, Limit, LimitKind,
    bars, ljust, rjust,
};

/// Labels past this are cut, so the numbers keep their room.
const MAX_LABEL: usize = 28;
/// The least cells a session's meter takes before the column goes.
const LEAST_METER: usize = 8;
/// Under this width the list doesn't name each session's model.
const WORKERS_FROM: usize = 90;
/// The most sessions the list shows at once. The rest scroll under the picked one.
const SESSION_ROWS: usize = 10;
/// The least cells a column of a category's items takes.
const ITEMS_COLUMN: usize = 24;
/// The most items a category lists before it counts the rest.
const ITEM_ROWS: usize = 4;

/// What the cards draw from.
#[derive(Debug, Clone, Copy)]
pub struct ContextCards<'a> {
    pub view: &'a ContextData,
    pub names: &'a DisplayNames,
    /// The session the top card shows, by its row.
    pub selected: usize,
    /// The mark before the picked row.
    pub mark: &'a str,
    /// The conversation's context by category, when its CLI has counted it.
    pub breakdown: Option<&'a ContextBreakdown>,
    /// `a` or `/context all`: every item of every category, each with where it comes from,
    /// and not only the largest few.
    pub all: bool,
}

impl ContextCards<'_> {
    /// Tokens in context over every live session.
    pub fn total(&self) -> u64 {
        self.view.rows.iter().map(|row| row.used).sum()
    }

    /// The picked session: what's in its context of its window, on a two-row bar with a
    /// tick where it compacts or Argonauts starts it afresh, and how often that happened.
    pub fn session(&self, kit: &Kit<'_>, frame: Frame) -> Option<Drawn> {
        let row = self.view.rows.get(self.selected)?;
        // Only the conversation's own session reports its categories.
        let breakdown = self.breakdown.filter(|_| row.run.is_none());
        let window = breakdown
            .map(|breakdown| breakdown.window)
            .filter(|window| *window > 0)
            .or(row.window)
            .or(row.cap);
        let compacts = breakdown
            .and_then(|breakdown| breakdown.auto_compact_at)
            .map(|at| Limit {
                at,
                kind: LimitKind::Compacts,
            });
        let rotates = row.cap.map(|at| Limit {
            at,
            kind: LimitKind::Rotates,
        });
        let limit = compacts
            .or(rotates)
            .filter(|limit| window.is_some_and(|window| limit.at < window));
        let parts = match breakdown {
            Some(breakdown) => categories(breakdown),
            None => vec![in_context(row.used)],
        };
        let card = ContextCard {
            subject: Some(format!(
                "{} · {}",
                label(row),
                sanitize_line(&self.names.worker(&row.worker))
            )),
            used: Some(row.used),
            window,
            limit,
            parts,
            estimated: false,
            bar_rows: 2,
            ..ContextCard::estimate(None, None)
        };
        let inner = frame.inner();
        let mut body = card.body(kit, inner, Form::Full);
        if let Some(breakdown) = breakdown {
            let items = items(kit, breakdown, inner, self.all);
            if items.height() > 0 {
                body.push(Vec::new());
                body.append(items);
            }
        }
        // Its calls and spend are in the list below. How often it started afresh isn't.
        if row.rotations > 0 {
            let afresh = format!("started afresh {} so far", times(row.rotations));
            body.push(truncate_spans(&[kit.dim(afresh)], inner));
        }
        Some(frame.draw(kit, body))
    }

    /// Every live session: a meter of its context with the tick where it rotates, used of
    /// its window, the percent in its level's color, its calls and its spend. On a narrow
    /// card the model goes first, then the meter, then the label is cut.
    pub fn sessions(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        let inner = frame.inner();
        let rows = &self.view.rows;
        let header = Header {
            tail: vec![kit.dim(format!(
                "{} live · {} in context",
                rows.len(),
                format::tokens(self.total())
            ))],
            ..Header::new("sessions", Role::Card)
        };
        let mut body = Drawn::new();
        body.push(header.draw(kit, inner));
        if rows.is_empty() {
            body.push(vec![kit.dim("No live sessions")]);
            return frame.draw(kit, body);
        }
        let cells = |cell: &dyn Fn(&ContextRow) -> String| -> Vec<String> {
            rows.iter().map(cell).collect()
        };
        let widest = |cells: &[String], heading: &str| {
            cells
                .iter()
                .map(|cell| str_width(cell))
                .chain([str_width(heading)])
                .max()
                .unwrap_or(0)
        };
        let labels = cells(&label);
        let workers = cells(&|row| sanitize_line(&self.names.worker(&row.worker)).into_owned());
        let used = cells(&used_of);
        let calls = cells(&|row| row.tool_calls.to_string());
        let spends = cells(&|row| {
            if row.spent.cost_usd > 0.0 || row.spent.cost_partial {
                format::spend(&row.spent)
            } else {
                String::new()
            }
        });
        let label_w = widest(&labels, "Session").min(MAX_LABEL) + 2;
        let worker_w = if inner >= WORKERS_FROM {
            widest(&workers, "Worker") + 2
        } else {
            0
        };
        let (used_w, calls_w, spend_w) = (
            widest(&used, "Used") + 2,
            widest(&calls, "Calls") + 2,
            widest(&spends, "Spend") + 2,
        );
        // The mark, the columns of words, and the percent in its five cells.
        let fixed = 2 + label_w + worker_w + used_w + 5 + calls_w + spend_w;
        let meter_w = match inner.saturating_sub(fixed + 2) {
            room if room >= LEAST_METER => room,
            _ => 0,
        };
        let label_w = label_w
            .min(inner.saturating_sub(fixed - label_w + meter_w))
            .max(4);

        let mut heading = vec![Span::raw("  "), kit.dim(ljust("Session", label_w))];
        if worker_w > 0 {
            heading.push(kit.dim(ljust("Worker", worker_w)));
        }
        if meter_w > 0 {
            heading.push(kit.dim(ljust("Context", meter_w + 2)));
        }
        heading.push(kit.dim(rjust("Used", used_w)));
        heading.push(Span::raw(" ".repeat(5)));
        heading.push(kit.dim(rjust("Calls", calls_w)));
        heading.push(kit.dim(rjust("Spend", spend_w)));
        body.push(truncate_spans(&heading, inner));

        // The window of rows that keeps the picked one in view.
        let first = (self.selected + 1).saturating_sub(SESSION_ROWS);
        for (at, row) in rows.iter().enumerate().skip(first).take(SESSION_ROWS) {
            let picked = at == self.selected;
            let name = truncate_spans(&[Span::raw(labels[at].clone())], label_w - 2);
            let name: String = name.iter().map(|span| span.content.as_ref()).collect();
            let (share, level) = fill(row);
            let mut line = vec![if picked {
                kit.span(format!("{} ", self.mark), Role::Card)
            } else {
                Span::raw("  ")
            }];
            line.push(if picked {
                kit.strong(ljust(&name, label_w))
            } else {
                Span::raw(ljust(&name, label_w))
            });
            if worker_w > 0 {
                line.push(kit.dim(ljust(&workers[at], worker_w)));
            }
            if meter_w > 0 {
                line.extend(bars::half_meter(
                    kit,
                    share.unwrap_or(0.0),
                    meter_w,
                    level.fill_role(),
                    tick(row),
                ));
                line.push(Span::raw("  "));
            }
            line.push(Span::raw(rjust(&used[at], used_w)));
            line.push(match share {
                Some(share) => kit.span(
                    rjust(&format!("{:.0}%", share * 100.0), 5),
                    level.strong_role(),
                ),
                None => Span::raw(" ".repeat(5)),
            });
            line.push(kit.dim(rjust(&calls[at], calls_w)));
            line.push(kit.dim(rjust(&spends[at], spend_w)));
            body.push(truncate_spans(&line, inner));
        }
        if rows.len() > SESSION_ROWS {
            body.push(vec![kit.dim(format!(
                "  {} of {} · ↑↓ shows the rest",
                self.selected.min(rows.len() - 1) + 1,
                rows.len()
            ))]);
        }
        frame.draw(kit, body)
    }
}

/// The conversation's context as its CLI last counted it, by category, for the card above
/// the input. It needs no answer from the controller: the screen holds the count.
pub fn conversation(breakdown: &ContextBreakdown) -> ContextCard {
    let parts = categories(breakdown);
    let used = parts.iter().filter_map(|part| part.tokens).sum();
    let window = Some(breakdown.window).filter(|window| *window > 0);
    let compacts = breakdown.auto_compact_at.map(|at| Limit {
        at,
        kind: LimitKind::Compacts,
    });
    ContextCard {
        used: Some(used),
        window,
        limit: compacts.filter(|limit| window.is_some_and(|window| limit.at < window)),
        parts,
        estimated: false,
        ..ContextCard::estimate(None, None)
    }
}

/// A session that reports only how full it is: `used` of the `cap` where Argonauts starts
/// it afresh, in a `window` that may be larger.
pub fn session_use(subject: String, used: u64, cap: u64, window: Option<u64>) -> ContextCard {
    let window = window.filter(|window| *window > 0).unwrap_or(cap);
    let rotates = Limit {
        at: cap,
        kind: LimitKind::Rotates,
    };
    ContextCard {
        subject: Some(subject),
        used: Some(used),
        window: Some(window),
        limit: Some(rotates).filter(|limit| limit.at < window),
        parts: vec![in_context(used)],
        estimated: false,
        ..ContextCard::estimate(None, None)
    }
}

/// The categories a CLI counted, as the bar's parts in its order.
fn categories(breakdown: &ContextBreakdown) -> Vec<ContextPart> {
    breakdown
        .categories
        .iter()
        .filter_map(|category| {
            let role = part_role(category.part)?;
            Some(ContextPart::new(
                category.part.label(),
                role,
                Some(category.tokens),
            ))
        })
        .collect()
}

/// The one part of a session that doesn't say what fills it.
fn in_context(used: u64) -> ContextPart {
    ContextPart::new("in context", Role::CatMessages, Some(used))
}

/// The color of a category the bar draws. Free and the held-back reserve are the bar's own
/// to draw, from the window and the point where the session compacts.
fn part_role(part: Part) -> Option<Role> {
    Some(match part {
        Part::SystemPrompt => Role::CatSystem,
        Part::Tools => Role::CatTools,
        Part::McpTools => Role::CatMcp,
        Part::Agents => Role::CatAgents,
        Part::MemoryFiles => Role::CatMemory,
        Part::Skills => Role::CatSkills,
        Part::Messages => Role::CatMessages,
        Part::Other => Role::CatOther,
        Part::Free | Part::Buffer => return None,
    })
}

/// What fills the categories that list their items, as Claude Code's /context shows them:
/// a column a category, each item with its tokens, the largest few and a count of the rest.
/// Columns that don't fit side by side go under the others. With `all`, every item shows,
/// a row each across the card, with where it comes from.
fn items(kit: &Kit<'_>, breakdown: &ContextBreakdown, width: usize, all: bool) -> Drawn {
    let groups: Vec<(&str, &[ContextItem])> = [
        ("Memory files", breakdown.memory_files.as_slice()),
        ("MCP tools", breakdown.mcp_tools.as_slice()),
        ("Skills", breakdown.skills.as_slice()),
        ("Custom agents", breakdown.agents.as_slice()),
    ]
    .into_iter()
    .filter(|(_, items)| !items.is_empty())
    .collect();
    let mut body = Drawn::new();
    if groups.is_empty() {
        return body;
    }
    if all {
        return every_item(kit, &groups, width);
    }
    let beside = (width / ITEMS_COLUMN).clamp(1, groups.len());
    let column = width / beside;
    let glyphs = &kit.theme.glyphs;
    for band in groups.chunks(beside) {
        let rows = band
            .iter()
            .map(|(_, items)| items.len().min(ITEM_ROWS + 1))
            .max()
            .unwrap_or(0);
        let heading = band.iter().flat_map(|(title, items)| {
            let cell = format!("{title} · {}", items.len());
            [kit.dim(ljust(&cell, column))]
        });
        body.push(truncate_spans(&heading.collect::<Vec<_>>(), width));
        for at in 0..rows {
            let mut line = Vec::new();
            for (_, items) in band {
                let shown = items.len().min(ITEM_ROWS + 1);
                let branch = if at + 1 == shown {
                    glyphs.tree_end
                } else {
                    glyphs.tree_mid
                };
                // The last row a long list has room for counts what it leaves out.
                let (name, amount) = match items.get(at) {
                    None => {
                        line.push(Span::raw(" ".repeat(column)));
                        continue;
                    }
                    Some(_) if at == ITEM_ROWS => (
                        format!("and {} more", items.len() - ITEM_ROWS),
                        String::new(),
                    ),
                    Some(item) => (
                        sanitize_line(&item.name).trim().to_owned(),
                        format::tokens(item.tokens),
                    ),
                };
                // Branch, space, name, space, the tokens in five cells, and two between columns.
                let name_w = column.saturating_sub(2 + 1 + 5 + 2);
                let name = truncate_spans(&[Span::raw(name)], name_w);
                let name: String = name.iter().map(|span| span.content.as_ref()).collect();
                line.push(kit.dim(format!("{branch} ")));
                line.push(Span::raw(ljust(&name, name_w + 1)));
                line.push(kit.dim(ljust(&rjust(&amount, 5), 7)));
            }
            body.push(truncate_spans(&line, width));
        }
    }
    body
}

/// Each category's items in full, one under the other: a heading with how many and what
/// they come to, then a row an item with its tokens and where it comes from.
fn every_item(kit: &Kit<'_>, groups: &[(&str, &[ContextItem])], width: usize) -> Drawn {
    let glyphs = &kit.theme.glyphs;
    let name_w = groups
        .iter()
        .flat_map(|(_, items)| items.iter())
        .map(|item| str_width(sanitize_line(&item.name).trim()))
        .max()
        .unwrap_or(0)
        .min(width.saturating_sub(2 + 7) / 2);
    let mut body = Drawn::new();
    for (at, (title, items)) in groups.iter().enumerate() {
        if at > 0 {
            body.push(Vec::new());
        }
        let total: u64 = items.iter().map(|item| item.tokens).sum();
        let heading = format!("{title} · {} · {}", items.len(), format::tokens(total));
        body.push(truncate_spans(&[kit.dim(heading)], width));
        for (row, item) in items.iter().enumerate() {
            let branch = if row + 1 == items.len() {
                glyphs.tree_end
            } else {
                glyphs.tree_mid
            };
            let name = truncate_spans(
                &[Span::raw(sanitize_line(&item.name).trim().to_owned())],
                name_w,
            );
            let name: String = name.iter().map(|span| span.content.as_ref()).collect();
            let mut line = vec![
                kit.dim(format!("{branch} ")),
                Span::raw(ljust(&name, name_w + 1)),
                kit.dim(rjust(&format::tokens(item.tokens), 6)),
            ];
            if let Some(source) = &item.source {
                line.push(kit.dim(format!("   {}", sanitize_line(source).trim())));
            }
            body.push(truncate_spans(&line, width));
        }
    }
    body
}

fn label(row: &ContextRow) -> String {
    sanitize_line(&row.label).trim().to_owned()
}

/// `212k/1M`, or the used tokens alone when the window isn't known.
fn used_of(row: &ContextRow) -> String {
    match row.window.or(row.cap) {
        Some(window) => format!("{}/{}", format::tokens(row.used), format::tokens(window)),
        None => format::tokens(row.used),
    }
}

/// How full the session's window is, and its level against the point where it rotates,
/// since that is where something happens.
fn fill(row: &ContextRow) -> (Option<f64>, Level) {
    let Some(window) = row.window.or(row.cap).filter(|window| *window > 0) else {
        return (None, Level::Ok);
    };
    let against = row.cap.filter(|cap| *cap > 0).unwrap_or(window);
    let level = Levels::CONTEXT.level(row.used as f64 / against as f64 * 100.0);
    (Some(row.used as f64 / window as f64), level)
}

/// Where the session rotates, as a share of its window, when that is short of the window.
fn tick(row: &ContextRow) -> Option<f64> {
    let window = row.window.filter(|window| *window > 0)?;
    let cap = row.cap.filter(|cap| *cap < window)?;
    Some(cap as f64 / window as f64)
}

fn times(n: u32) -> String {
    match n {
        1 => "once".to_owned(),
        2 => "twice".to_owned(),
        n => format!("{n} times"),
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
