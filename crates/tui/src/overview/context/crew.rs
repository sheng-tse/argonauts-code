//! The crew's agents, last on the Context tab: each works in a window of its own, outside the
//! main session's. A row an agent with its window as a bar, what it used, its calls and its
//! spend, and under it where its session starts afresh and what fills it. An agent whose session
//! ended keeps the row its session left, marked done or failed, with when it ended.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::context::{ContextCategory, ContextKind};
use argonauts_core::views::{ContextRow, EndedHow, SessionLeft};
use ratatui::style::Style;
use ratatui::text::Span;
use time::OffsetDateTime;

use super::pane::Section;
use super::place::{Columns, GAP, at, clip, ending, place};
use super::usage::{GridGlyphs, bold, level_role, tinted};
use super::words::{color_role, count, deferred_name, time_of};
use crate::format;
use crate::kit::{Drawn, Kit, Level, Levels, bars, merge};
use crate::text::{
    KEEP, Part, join_parts, parts_that_fit, sanitize_line, spans_width, str_width, truncate_spans,
};
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
    /// When the run finished, for one listed after it did.
    pub finished: Option<OffsetDateTime>,
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
            format::tokens_beside(row.used, window),
            format::window(window)
        ),
        None => format!("{about}{}", format::tokens(row.used)),
    }
}

/// A count measured in the session's window, in the window's divisor.
fn in_window(row: &ContextRow, tokens: u64) -> String {
    match row.window.or(row.cap).filter(|window| *window > 0) {
        Some(window) => format::tokens_beside(tokens, window),
        None => format::tokens(tokens),
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
    // A session that ended never starts afresh.
    let tick = row.cap.filter(|cap| *cap < window && row.ended.is_none());
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
    let Some(cap) = row.cap.filter(|cap| *cap > 0 && row.ended.is_none()) else {
        return Vec::new();
    };
    let level = row_level(row);
    let words = match (level, cap.checked_sub(row.used)) {
        (_, None | Some(0)) => "starts afresh now".to_owned(),
        (Level::Ok, _) => format!("starts afresh at {}", in_window(row, cap)),
        (_, Some(left)) => format!("starts afresh in {}", in_window(row, left)),
    };
    vec![
        kit.dim(format!("{} ", kit.glyphs.line_tick)),
        kit.span(words, level_role(level).unwrap_or(Role::Dim)),
    ]
}

/// What fills the session, a swatch and a few words a part, in `room` cells: where they
/// don't all fit, parts are left out whole, the smallest first. A session its CLI hasn't
/// counted yet says when it will be, and one whose CLI never says says that.
pub(super) fn filled_with(
    kit: &Kit<'_>,
    row: &ContextRow,
    names: &DisplayNames,
    room: usize,
) -> Vec<Span<'static>> {
    let filling = parts(&row.categories);
    if filling.is_empty() && row.counts {
        return vec![kit.dim(not_counted(row, names))];
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
                kit.dim(entry(row, name, *tokens)),
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

/// Why a session whose CLI counts has no count: the main session is counted as each answer
/// ends, a run's agent while it works, and one that ended first never was.
fn not_counted(row: &ContextRow, names: &DisplayNames) -> String {
    match (&row.run, &row.ended) {
        (None, _) => "counted when an answer ends".to_owned(),
        (Some(_), None) => "counting\u{2026}".to_owned(),
        (Some(_), Some(_)) => {
            let cli = sanitize_line(&names.cli(&row.worker.agent))
                .trim()
                .to_owned();
            format!("ended before {cli} counted it")
        }
    }
}

/// The cells between two parts of what fills a session.
const PART_GAP: usize = 3;

/// What fills each agent's session, a column a part across the whole crew, each as wide as
/// its widest entry, so every swatch and name stays in place from row to row. Where the
/// columns don't all fit, a part's column is left out, the smallest first, and the largest
/// always stays. A row's part whose column was left out takes a column that row has nothing
/// in, where it fits, so a row loses a part only when it has no such place.
struct Legend {
    /// Each part's name and the cells its column takes, in the order the parts first come.
    columns: Vec<(String, usize)>,
}

impl Legend {
    fn across(rows: &[&ContextRow], swatch: &str, room: usize) -> Self {
        // A part's name, its column's cells, and its largest count, by which it is kept.
        let mut columns: Vec<(String, usize, u64)> = Vec::new();
        for row in rows {
            for (name, tokens, _) in parts(&row.categories) {
                let cells = str_width(swatch) + 1 + str_width(&entry(row, &name, tokens));
                match columns.iter_mut().find(|(known, ..)| *known == name) {
                    Some((_, widest, largest)) => {
                        *widest = (*widest).max(cells);
                        *largest = (*largest).max(tokens);
                    }
                    None => columns.push((name, cells, tokens)),
                }
            }
        }
        let cells = |columns: &[(String, usize, u64)]| {
            let gaps = PART_GAP * columns.len().saturating_sub(1);
            columns.iter().map(|(_, cells, _)| cells).sum::<usize>() + gaps
        };
        while columns.len() > 1 && cells(&columns) > room {
            let smallest = columns
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, _, largest))| *largest)
                .map(|(at, _)| at)
                .unwrap_or_default();
            columns.remove(smallest);
        }
        Self {
            columns: columns
                .into_iter()
                .map(|(name, cells, _)| (name, cells))
                .collect(),
        }
    }

    /// `row`'s parts in their columns, blank where it has none of a part. A row its CLI
    /// hasn't said what fills reads as [`filled_with`] has it.
    fn row(
        &self,
        kit: &Kit<'_>,
        row: &ContextRow,
        names: &DisplayNames,
        room: usize,
    ) -> Vec<Span<'static>> {
        let filling = parts(&row.categories);
        if filling.is_empty() {
            return filled_with(kit, row, names, room);
        }
        let swatch = GridGlyphs::for_kit(kit).whole;
        let width = |(name, tokens, _): &(String, u64, Role)| {
            str_width(swatch) + 1 + str_width(&entry(row, name, *tokens))
        };
        // The row's parts with no column of their own, largest first.
        let mut unplaced: Vec<&(String, u64, Role)> = filling
            .iter()
            .filter(|(part, ..)| !self.columns.iter().any(|(name, _)| name == part))
            .collect();
        unplaced.sort_by_key(|(_, tokens, _)| std::cmp::Reverse(*tokens));
        let mut spans = Vec::new();
        // Blank cells owed before the next part shown.
        let mut owed = 0;
        for (nth, (name, cells)) in self.columns.iter().enumerate() {
            if nth > 0 {
                owed += PART_GAP;
            }
            let own = filling.iter().find(|(part, ..)| part == name);
            let shown = own.or_else(|| {
                let fits = unplaced.iter().position(|part| width(part) <= *cells)?;
                Some(unplaced.remove(fits))
            });
            let Some((name, tokens, role)) = shown else {
                owed += cells;
                continue;
            };
            if owed > 0 {
                spans.push(Span::raw(" ".repeat(owed)));
            }
            let words = entry(row, name, *tokens);
            owed = cells.saturating_sub(str_width(swatch) + 1 + str_width(&words));
            spans.push(Span::styled(format!("{swatch} "), kit.fg(*role)));
            spans.push(kit.dim(words));
        }
        spans
    }
}

/// `messages 4.9k`, the count beside the window taking its divisor.
fn entry(row: &ContextRow, name: &str, tokens: u64) -> String {
    format!("{name} {}", in_window(row, tokens))
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

/// What the session spent, a dim `local` for one on this machine that spent nothing, and
/// nothing for any other that spent nothing.
fn spend(kit: &Kit<'_>, row: &ContextRow, local: bool) -> Vec<Span<'static>> {
    if row.spent.cost_usd > 0.0 || row.spent.cost_partial {
        vec![Span::raw(format::spend(&row.spent))]
    } else if local {
        vec![kit.dim("local")]
    } else {
        Vec::new()
    }
}

fn label(kit: &Kit<'_>, row: &ContextRow) -> Vec<Span<'static>> {
    let glyphs = &kit.theme.glyphs;
    let (mark, role) = match row.ended.as_ref().map(|left| &left.how) {
        None => (
            glyphs.agent_working.first().copied().unwrap_or("*"),
            Role::StateWorking,
        ),
        Some(EndedHow::Done) => (glyphs.agent_done, Role::Success),
        Some(EndedHow::Failed { .. }) => (glyphs.agent_failed, Role::Danger),
        Some(EndedHow::Stopped) => (glyphs.agent_done, Role::Dim),
    };
    // A label wider than its column would push the row's numbers out of line: it ends with
    // room for the gap before the model.
    let label = clip(sanitize_line(&row.label).trim(), MODEL - 2 - GAP);
    vec![
        Span::styled(format!("{mark} "), kit.fg(role)),
        Span::styled(label, kit.fg(Role::Label)),
    ]
}

/// `as it ended, 3:02 AM`, for a session that ended: why, in red, for one that failed, and
/// whom it stood in for, for a fallback. At most `room` cells.
fn as_it_ended(
    kit: &Kit<'_>,
    left: &SessionLeft,
    names: &DisplayNames,
    now: OffsetDateTime,
    room: usize,
) -> Span<'static> {
    let at = time_of(left.at, now);
    let (words, role) = match (&left.how, &left.fallback_for) {
        (EndedHow::Failed { why }, _) => {
            let why = sanitize_line(why).trim().to_owned();
            (format!("{why} \u{b7} {at}"), Role::Danger)
        }
        (EndedHow::Stopped, _) => (format!("stopped \u{b7} {at}"), Role::Dim),
        (EndedHow::Done, Some(stood_in_for)) => {
            let model = names
                .model(&stood_in_for.agent, stood_in_for.model.as_deref())
                .unwrap_or_else(|| names.cli(&stood_in_for.agent));
            let model = sanitize_line(&model).trim().to_owned();
            (format!("fallback for {model} \u{b7} {at}"), Role::Dim)
        }
        (EndedHow::Done, None) => (format!("as it ended, {at}"), Role::Dim),
    };
    kit.span(clip(&words, room), role)
}

/// One run's agents: a header block, then a block an agent, the first under the column
/// heads. `own` gives the colour an agent's model draws in, `local` whether it runs on this
/// machine, and `now` what an ended agent's time is read against.
pub(super) fn sections(
    kit: &Kit<'_>,
    columns: &Columns,
    crew: &Crew<'_>,
    names: &DisplayNames,
    own: &dyn Fn(&ContextRow) -> Style,
    local: &dyn Fn(&ContextRow) -> bool,
    now: OffsetDateTime,
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
    let about = match crew.finished {
        Some(at) => format!(
            " \u{b7} ended {}, each as its session left it",
            time_of(at, now)
        ),
        None => " \u{b7} each in its own window, outside this one".to_owned(),
    };
    let about = about.as_str();
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
    let room = columns.width.saturating_sub(start + WINDOW);
    let legend = Legend::across(&crew.rows, GridGlyphs::for_kit(kit).whole, room);
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
        let spent = spend(kit, row, local(row));
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
                at(
                    start + MODEL,
                    truncate_spans(&model(kit, row, names, own), WINDOW - MODEL - GAP),
                ),
                at(start + WINDOW, bar),
                ending(start + USED_END, vec![used]),
                ending(start + PERCENT_END, percent),
                ending(
                    start + CALLS_END,
                    vec![Span::raw(row.tool_calls.to_string())],
                ),
                ending(start + SPEND_END, spent),
            ]));
            let mut under = vec![hang];
            match &row.ended {
                Some(left) => {
                    let room = WINDOW.saturating_sub(2 + spans_width(&under) + GAP);
                    under.push(as_it_ended(kit, left, names, now, room));
                }
                None => under.extend(afresh(kit, row)),
            }
            drawn.push(place([
                at(start + 2, under),
                at(start + WINDOW, legend.row(kit, row, names, room)),
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
            if !spent.is_empty() {
                numbers.push(Span::raw("  "));
                numbers.extend(spent);
            }
            drawn.push(place([at(start + 2, numbers)]));
            let mut under = match &row.ended {
                Some(left) => {
                    let room = columns.width.saturating_sub(start + 5);
                    vec![as_it_ended(kit, left, names, now, room)]
                }
                None => afresh(kit, row),
            };
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
            fold: None,
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
            recorded: None,
            ended: None,
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

    fn kit_text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn a_crews_legend_lines_up_and_puts_a_part_left_out_where_its_row_has_room() {
        let mut theme = crate::theme::Theme::dark();
        theme.glyphs = crate::theme::Glyphs::UNICODE;
        let kit = Kit::new(&theme);
        let names = DisplayNames::default();
        let counted = |system: u64, mcp: u64, instructions: u64, messages: u64| {
            let mut row = row(20_000, Some(1_000_000), None);
            row.categories = vec![
                category("System prompt", 2_497, "promptBorder"),
                category("System tools", system, "inactive"),
                category("MCP tools", mcp, "cyan_FOR_SUBAGENTS_ONLY"),
                category(
                    "MCP server instructions",
                    instructions,
                    "green_FOR_SUBAGENTS_ONLY",
                ),
                category("Messages", messages, "purple_FOR_SUBAGENTS_ONLY"),
            ];
            row
        };
        let rows = [
            counted(4_407, 153, 273, 4_921),
            counted(9_868, 153, 273, 5_989),
            counted(6_131, 167, 0, 5_291),
        ];
        let rows: Vec<&ContextRow> = rows.iter().collect();
        let legend = Legend::across(&rows, "\u{26c1}", 64);
        let shown: Vec<String> = rows
            .iter()
            .map(|row| kit_text(&legend.row(&kit, row, &names, 64)))
            .collect();
        assert_eq!(
            shown,
            [
                "\u{26c1} system 6.9k    \u{26c1} MCP server instructions 273   \u{26c1} messages 4.9k",
                "\u{26c1} system 12.4k   \u{26c1} MCP server instructions 273   \u{26c1} messages 6k",
                "\u{26c1} system 8.6k    \u{26c1} MCP tools 167                 \u{26c1} messages 5.3k",
            ]
        );
        // A row with no place for it goes without it.
        let full = [counted(4_407, 153, 273, 4_921)];
        let full: Vec<&ContextRow> = full.iter().collect();
        let tight = Legend::across(&full, "\u{26c1}", 64);
        let shown = kit_text(&tight.row(&kit, full[0], &names, 64));
        assert!(!shown.contains("MCP tools"), "{shown}");
        // With room for every part, none is left out.
        let wide = Legend::across(&rows, "\u{26c1}", 120);
        assert!(kit_text(&wide.row(&kit, rows[2], &names, 120)).contains("MCP tools 167"));
    }

    /// A label longer than its column is cut where the gap before the model starts, so every
    /// model starts in one column. One as long as the longest pictured keeps every letter.
    #[test]
    fn a_long_label_is_cut_before_the_models_column() {
        let mut theme = crate::theme::Theme::dark();
        theme.glyphs = crate::theme::Glyphs::UNICODE;
        let kit = Kit::new(&theme);
        let labelled = |name: &str| {
            let mut row = row(20_000, Some(1_000_000), None);
            row.label = name.to_owned();
            kit_text(&label(&kit, &row))
        };
        for name in [
            "main",
            "review:due-dates-2",
            "build:add-due-dates",
            "sign-off:add-due-dates",
        ] {
            let drawn = labelled(name);
            assert!(str_width(&drawn) + GAP <= MODEL, "{name}: {drawn}");
        }
        assert!(labelled("review:due-dates-2").ends_with("review:due-dates-2"));
        assert!(labelled("build:add-due-dates").ends_with("build:add-due-dat\u{2026}"));
    }

    #[test]
    fn a_session_with_no_count_says_when_it_is_counted() {
        let names = DisplayNames::default();
        let mut main = row(27_755, Some(1_000_000), None);
        main.counts = true;
        assert_eq!(not_counted(&main, &names), "counted when an answer ends");
        let mut working = main.clone();
        working.run = Some(argonauts_core::RunId::new("run-07").unwrap());
        assert_eq!(not_counted(&working, &names), "counting\u{2026}");
        let mut ended = working.clone();
        ended.ended = Some(SessionLeft {
            at: OffsetDateTime::UNIX_EPOCH,
            how: EndedHow::Done,
            fallback_for: None,
        });
        assert_eq!(
            not_counted(&ended, &names),
            "ended before Claude Code counted it"
        );
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
