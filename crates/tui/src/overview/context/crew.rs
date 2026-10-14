//! The crew's agents, last on the Context tab: each works in a window of its own, outside the
//! main session's. A row an agent with its window as a bar, what it used, its calls and its
//! spend, and under it where its session starts afresh and what fills it.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::context::{ContextCategory, ContextKind};
use argonauts_core::views::ContextRow;
use ratatui::style::Style;
use ratatui::text::Span;

use super::pane::Section;
use super::place::{Columns, GAP, at, clip, ending, place};
use super::usage::{GridGlyphs, bold, level_role, tinted};
use super::words::{color_role, count, deferred_name};
use crate::format;
use crate::kit::{Drawn, Kit, Level, Levels, bars, merge};
use crate::text::{KEEP, Part, join_parts, parts_that_fit, sanitize_line, spans_width, str_width};
use crate::theme::Role;

/// The cells of an agent's window.
const BAR_CELLS: usize = 20;
/// The table's columns, from the section's start: the model, the window, and where the
/// used tokens, the percent, the calls and the spend end.
const MODEL: usize = 22;
const WINDOW: usize = 49;
const USED_END: usize = 82;
const PERCENT_END: usize = 88;
const CALLS_END: usize = 95;
const SPEND_END: usize = 105;

/// One run's agents, as the section draws them.
pub(super) struct Crew<'a> {
    /// The run's name, as the conversation knows it.
    pub title: String,
    pub rows: Vec<&'a ContextRow>,
}

/// How full an agent's session is against the point where it starts afresh, since that is
/// where something happens to it.
pub(super) fn row_level(row: &ContextRow) -> Level {
    let against = row.cap.or(row.window).filter(|against| *against > 0);
    match against {
        Some(against) => Levels::CONTEXT.level(row.used as f64 / against as f64 * 100.0),
        None => Level::Ok,
    }
}

/// `~13.4k/272k`: what the session holds of its window, marked where it is worked out.
pub(super) fn used_of(row: &ContextRow) -> String {
    let about = if row.estimated { "~" } else { "" };
    match row.window.or(row.cap).filter(|window| *window > 0) {
        Some(window) => format!(
            "{about}{}/{}",
            format::tokens(row.used),
            format::tokens(window)
        ),
        None => format!("{about}{}", format::tokens(row.used)),
    }
}

/// The session's fill of its window in whole percent.
pub(super) fn percent(row: &ContextRow) -> Option<String> {
    let window = row.window.or(row.cap).filter(|window| *window > 0)?;
    Some(format!(
        "{:.0}%",
        (row.used as f64 / window as f64 * 100.0).round()
    ))
}

/// What fills a session, as the bar and the line under it show it: Claude Code's own two rows
/// as one, then each other row that holds something, each with its colour.
pub(super) fn parts(categories: &[ContextCategory]) -> Vec<(String, u64, Role)> {
    let used = categories
        .iter()
        .filter(|row| row.kind == ContextKind::Used && row.tokens > 0);
    let mut parts: Vec<(String, u64, Role)> = Vec::new();
    for row in used {
        // Claude Code 2.1.296's names for what it brings itself.
        if matches!(row.name.as_str(), "System prompt" | "System tools") {
            match parts.iter_mut().find(|(name, _, _)| name == "system") {
                Some((_, tokens, _)) => *tokens += row.tokens,
                None => parts.push(("system".to_owned(), row.tokens, Role::Dim)),
            }
        } else {
            parts.push((
                deferred_name(sanitize_line(&row.name).trim()),
                row.tokens,
                color_role(&row.color),
            ));
        }
    }
    parts
}

/// An agent's window as a thin bar: its parts in their colours where its CLI says what fills
/// it, else all of it in `own`. The rest is track, with a dim `┃` where the session starts
/// afresh. `None` while the window isn't known.
pub(super) fn window_bar(
    kit: &Kit<'_>,
    row: &ContextRow,
    own: Style,
) -> Option<Vec<Span<'static>>> {
    let window = row.window.filter(|window| *window > 0)?;
    let tick = row.cap.filter(|cap| *cap < window);
    if !kit.has_color() {
        // Without colour the parts can't be told apart, so the bar says only how full.
        let ratio = row.used as f64 / window as f64;
        let tick = tick.map(|cap| cap as f64 / window as f64);
        return Some(bars::line_meter(kit, ratio, BAR_CELLS, Role::BarFill, tick));
    }
    let cells_of = |tokens: u64| {
        let cells = (tokens as f64 / window as f64 * BAR_CELLS as f64).round_ties_even();
        (cells.max(0.0) as usize).max(1)
    };
    let mut cells: Vec<Option<Style>> = Vec::with_capacity(BAR_CELLS);
    let parts = parts(&row.categories);
    if parts.is_empty() {
        if row.used > 0 {
            cells.extend(vec![Some(own); cells_of(row.used)]);
        }
    } else {
        for (_, tokens, role) in parts {
            cells.extend(vec![Some(kit.fg(role)); cells_of(tokens)]);
        }
    }
    cells.resize(BAR_CELLS, None);
    let tick = tick.map(|cap| {
        let cell = (cap as f64 / window as f64 * BAR_CELLS as f64).round_ties_even() as usize;
        cell.min(BAR_CELLS - 1)
    });
    let glyphs = kit.glyphs;
    let bar = cells
        .into_iter()
        .enumerate()
        .map(|(cell, style)| match style {
            Some(style) => Span::styled(glyphs.line_fill, style),
            None if Some(cell) == tick => Span::styled(glyphs.line_tick, kit.fg(Role::Dim)),
            None => Span::styled(glyphs.line_fill, kit.fg(Role::Faint)),
        })
        .collect();
    Some(merge(bar))
}

/// `starts afresh at 180k`, or how far that is once the session is close, in the colour of how
/// close. Nothing where no point is set.
pub(super) fn afresh(kit: &Kit<'_>, row: &ContextRow) -> Vec<Span<'static>> {
    let Some(cap) = row.cap.filter(|cap| *cap > 0) else {
        return Vec::new();
    };
    let level = row_level(row);
    let words = match (level, cap.checked_sub(row.used)) {
        (_, None | Some(0)) => "starts afresh now".to_owned(),
        (Level::Ok, _) => format!("starts afresh at {}", format::tokens(cap)),
        (_, Some(left)) => format!("starts afresh in {}", format::tokens(left)),
    };
    vec![
        kit.dim(format!("{} ", kit.glyphs.line_tick)),
        kit.span(words, level_role(level).unwrap_or(Role::Dim)),
    ]
}

/// What fills the session, a swatch and a few words a part, in `room` cells: where they
/// don't all fit, parts are left out whole, the smallest first. A session its CLI hasn't
/// counted yet says so, and so does one whose CLI never says.
pub(super) fn filled_with(
    kit: &Kit<'_>,
    row: &ContextRow,
    names: &DisplayNames,
    room: usize,
) -> Vec<Span<'static>> {
    let filling = parts(&row.categories);
    if filling.is_empty() && row.counts {
        return vec![kit.dim("not counted yet")];
    }
    if filling.is_empty() {
        return vec![kit.dim(format!(
            "{} says how full, not with what",
            names.cli(&row.worker.agent)
        ))];
    }
    let swatch = GridGlyphs::for_kit(kit).whole;
    // A part's rank is how many of the others are smaller, and the largest always stays.
    let smaller = |tokens: u64| {
        filling
            .iter()
            .filter(|(_, other, _)| *other < tokens)
            .count()
    };
    let largest = filling.iter().map(|(_, tokens, _)| *tokens).max();
    let filling: Vec<Part> = filling
        .iter()
        .map(|(name, tokens, role)| {
            let spans = vec![
                Span::styled(format!("{swatch} "), kit.fg(*role)),
                kit.dim(format!("{name} {}", format::tokens(*tokens))),
            ];
            let rank = if Some(*tokens) == largest {
                KEEP
            } else {
                smaller(*tokens) as u16
            };
            Part::joined(Span::raw("   "), spans, rank)
        })
        .collect();
    join_parts(&filling, &parts_that_fit(&filling, room))
}

/// `Sonnet 5.5 on Claude Code`, the model in its colour.
pub(super) fn model(
    kit: &Kit<'_>,
    row: &ContextRow,
    names: &DisplayNames,
    own: Style,
) -> Vec<Span<'static>> {
    let worker = names.running(&row.worker);
    let cli = sanitize_line(&names.cli(&worker.agent)).trim().to_owned();
    match names.model(&worker.agent, worker.model.as_deref()) {
        Some(model) => vec![
            Span::styled(sanitize_line(&model).trim().to_owned(), own),
            kit.dim(format!(" on {cli}")),
        ],
        None => vec![Span::styled(cli, own)],
    }
}

fn spend(row: &ContextRow) -> String {
    if row.spent.cost_usd > 0.0 || row.spent.cost_partial {
        format::spend(&row.spent)
    } else {
        String::new()
    }
}

fn label(kit: &Kit<'_>, row: &ContextRow) -> Vec<Span<'static>> {
    let working = kit
        .theme
        .glyphs
        .agent_working
        .first()
        .copied()
        .unwrap_or("*");
    // A label wider than its column would push the row's numbers out of line.
    let label = clip(sanitize_line(&row.label).trim(), MODEL - 2);
    vec![
        Span::styled(format!("{working} "), kit.fg(Role::StateWorking)),
        Span::styled(label, kit.fg(Role::Label)),
    ]
}

/// One run's agents: a header block, then a block an agent, the first under the column
/// heads. `own` gives the colour an agent's model draws in.
pub(super) fn sections(
    kit: &Kit<'_>,
    columns: &Columns,
    crew: &Crew<'_>,
    names: &DisplayNames,
    own: &dyn Fn(&ContextRow) -> Style,
) -> Vec<Section> {
    let start = columns.indent();
    let total: u64 = crew.rows.iter().map(|row| row.used).sum();
    let counted = vec![
        kit.dim(format!("{}  ", count(crew.rows.len(), "agent"))),
        bold(kit, format::tokens(total)),
    ];
    let name = bold(kit, "Crew agents");
    let title = sanitize_line(&crew.title).trim().to_owned();
    let mut hint = String::new();
    if !title.is_empty() {
        hint.push_str(&format!(" \u{b7} {title}"));
    }
    // The count is never what a narrow row loses: the words about the windows go first, and
    // then the end of the run's title.
    let about = " \u{b7} each in its own window, outside this one";
    let taken = start + spans_width(std::slice::from_ref(&name)) + GAP + spans_width(&counted);
    let room = columns.width.saturating_sub(taken);
    let hint = if str_width(&hint) + str_width(about) <= room {
        format!("{hint}{about}")
    } else {
        clip(&hint, room)
    };
    let mut header = Drawn::new();
    header.push(place([
        at(start, vec![name, kit.dim(hint)]),
        ending(columns.second().end, counted),
    ]));
    let mut sections = vec![Section::of(header)];
    let table = columns.designed();
    for (nth, row) in crew.rows.iter().enumerate() {
        let mut drawn = Drawn::new();
        let heads = table && nth == 0;
        if heads {
            drawn.push(place([
                at(start + 2, vec![kit.dim("agent")]),
                at(start + MODEL, vec![kit.dim("model")]),
                at(start + WINDOW, vec![kit.dim("window")]),
                ending(start + USED_END, vec![kit.dim("used")]),
                ending(start + CALLS_END, vec![kit.dim("calls")]),
                ending(start + SPEND_END, vec![kit.dim("spend")]),
            ]));
        }
        let level = row_level(row);
        let own = own(row);
        let bar = window_bar(kit, row, own).unwrap_or_default();
        let used = tinted(kit, used_of(row), level_role(level));
        let percent: Vec<Span<'static>> = percent(row)
            .map(|percent| kit.span(percent, level_role(level).unwrap_or(Role::Dim)))
            .into_iter()
            .collect();
        let hang = kit.dim(format!("{}  ", kit.theme.glyphs.result));
        if table {
            drawn.push(place([
                at(start, label(kit, row)),
                at(start + MODEL, model(kit, row, names, own)),
                at(start + WINDOW, bar),
                ending(start + USED_END, vec![used]),
                ending(start + PERCENT_END, percent),
                ending(
                    start + CALLS_END,
                    vec![Span::raw(row.tool_calls.to_string())],
                ),
                ending(start + SPEND_END, vec![Span::raw(spend(row))]),
            ]));
            let mut under = vec![hang];
            under.extend(afresh(kit, row));
            let room = columns.width.saturating_sub(start + WINDOW);
            drawn.push(place([
                at(start + 2, under),
                at(start + WINDOW, filled_with(kit, row, names, room)),
            ]));
        } else {
            // Too narrow for the table: the agent and its model, then its numbers, then what
            // fills it.
            let mut first = label(kit, row);
            first.push(Span::raw("  "));
            first.extend(model(kit, row, names, own));
            drawn.push(place([at(start, first)]));
            let mut numbers = vec![hang];
            numbers.extend(bar);
            numbers.push(Span::raw("  "));
            numbers.push(used);
            for percent in percent {
                numbers.push(Span::raw("  "));
                numbers.push(percent);
            }
            numbers.push(kit.dim(format!("  {}", count(row.tool_calls as usize, "call"))));
            let spend = spend(row);
            if !spend.is_empty() {
                numbers.push(Span::raw(format!("  {spend}")));
            }
            drawn.push(place([at(start + 2, numbers)]));
            let mut under = afresh(kit, row);
            if !under.is_empty() {
                under.push(Span::raw("   "));
            }
            let room = columns
                .width
                .saturating_sub(start + 5 + spans_width(&under));
            under.extend(filled_with(kit, row, names, room));
            drawn.push(place([at(start + 5, under)]));
        }
        sections.push(Section {
            drawn,
            pointer_row: usize::from(heads),
            agent: row.run.clone().map(|run| (run, row.session.clone())),
        });
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::Spend;
    use argonauts_core::names::SessionKey;

    fn row(used: u64, window: Option<u64>, cap: Option<u64>) -> ContextRow {
        ContextRow {
            run: None,
            session: SessionKey::numbered(1),
            label: "build:due-flag".to_owned(),
            worker: "claude-code/claude-sonnet-5-5".parse().unwrap(),
            used,
            window,
            cap,
            tool_calls: 14,
            spent: Spend::default(),
            rotations: 0,
            estimated: false,
            counts: false,
            categories: Vec::new(),
        }
    }

    fn category(name: &str, tokens: u64, color: &str) -> ContextCategory {
        ContextCategory {
            name: name.to_owned(),
            tokens,
            color: color.to_owned(),
            kind: ContextKind::Used,
        }
    }

    #[test]
    fn a_session_is_measured_against_where_it_starts_afresh() {
        assert_eq!(
            row_level(&row(36_200, Some(200_000), Some(180_000))),
            Level::Ok
        );
        assert_eq!(
            row_level(&row(150_000, Some(200_000), Some(180_000))),
            Level::Warn
        );
        assert_eq!(
            row_level(&row(171_400, Some(200_000), Some(180_000))),
            Level::Danger
        );
        assert_eq!(row_level(&row(171_400, Some(200_000), None)), Level::Warn);
        assert_eq!(row_level(&row(171_400, None, None)), Level::Ok);
    }

    #[test]
    fn what_a_session_holds_reads_against_its_window() {
        let mut worked_out = row(13_400, Some(272_000), Some(244_800));
        worked_out.estimated = true;
        assert_eq!(used_of(&worked_out), "~13.4k/272k");
        assert_eq!(percent(&worked_out).as_deref(), Some("5%"));
        assert_eq!(used_of(&row(36_200, None, None)), "36.2k");
        assert_eq!(percent(&row(36_200, None, None)), None);
    }

    #[test]
    fn claude_codes_own_rows_read_as_one_part() {
        let rows = [
            category("System prompt", 2_900, "promptBorder"),
            category("System tools", 12_100, "inactive"),
            category("MCP tools", 0, "cyan_FOR_SUBAGENTS_ONLY"),
            category("Memory files", 3_700, "claude"),
            category("Messages", 17_500, "purple_FOR_SUBAGENTS_ONLY"),
            ContextCategory {
                kind: ContextKind::Free,
                ..category("Free space", 130_800, "promptBorder")
            },
        ];
        assert_eq!(
            parts(&rows),
            [
                ("system".to_owned(), 15_000, Role::Dim),
                ("memory files".to_owned(), 3_700, Role::CliClaude),
                ("messages".to_owned(), 17_500, Role::Label),
            ]
        );
    }
}
