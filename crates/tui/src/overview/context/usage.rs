//! The top of the Context tab, as Claude Code's /context draws it: the title, the grid of
//! cells and the legend beside it. Then what Argonauts Code adds: whose each group of rows is
//! and what it comes to, how far the session is from compacting, what loads on demand, and the
//! last request's cache use.

use argonauts_core::events::context::{ContextCategory, ContextCell, ContextKind, ContextUsage};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::place::{Columns, at, ending, place};
use super::words::{color_role, deferred_name, share};
use crate::format;
use crate::kit::{Drawn, Kit, Level, Levels, ljust, rjust};
use crate::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::Role;

/// The legend's columns: a row's name, its tokens, and its share.
pub(super) const NAME_CELLS: usize = 25;
pub(super) const TOKENS_CELLS: usize = 7;
pub(super) const SHARE_CELLS: usize = 8;
/// The columns beside the legend: whose a group of rows is, and what the group comes to.
const GROUP_CELLS: usize = 17;
const SUBTOTAL_CELLS: usize = 6;
/// The columns of the lines under the grid: what a line is about, then its number.
pub(super) const LABEL_CELLS: usize = 22;
pub(super) const VALUE_CELLS: usize = 20;
/// A cell this full of its category draws whole, as Claude Code draws it.
pub(super) const WHOLE_FROM: f64 = 0.7;

/// The grid's glyphs: a cell mostly full, one partly full, free space, and the room held
/// back for compacting.
#[derive(Debug, Clone, Copy)]
pub(super) struct GridGlyphs {
    pub whole: &'static str,
    pub part: &'static str,
    pub free: &'static str,
    pub held: &'static str,
}

impl GridGlyphs {
    pub fn for_kit(kit: &Kit<'_>) -> Self {
        if kit.theme.glyphs.rule.is_ascii() {
            Self {
                whole: "#",
                part: "+",
                free: ".",
                held: "x",
            }
        } else {
            Self {
                whole: "\u{26C1}",
                part: "\u{26C0}",
                free: "\u{26F6}",
                held: "\u{26DD}",
            }
        }
    }
}

/// How close the session is to where something happens to it: the point it compacts at, or
/// the end of its window when it never compacts by itself.
pub(super) fn level(usage: &ContextUsage) -> Level {
    let limit = usage.limit();
    if limit == 0 {
        return Level::Ok;
    }
    Levels::CONTEXT.level(usage.total_tokens as f64 / limit as f64 * 100.0)
}

/// The colour a number takes at `level`, and none while nothing is close.
pub(super) fn level_role(level: Level) -> Option<Role> {
    match level {
        Level::Ok => None,
        Level::Warn => Some(Role::Warn),
        Level::Danger => Some(Role::Danger),
    }
}

pub(super) fn bold(kit: &Kit<'_>, text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), kit.bold(Role::Text))
}

/// Text in `role` when there is one, else in the terminal's own colour.
pub(super) fn tinted(kit: &Kit<'_>, text: impl Into<String>, role: Option<Role>) -> Span<'static> {
    match role {
        Some(role) => Span::styled(text.into(), kit.fg(role)),
        None => Span::raw(text.into()),
    }
}

/// What the title row names the session by.
pub(super) struct Title<'a> {
    /// `counted 6s ago`, when the count's age is known.
    pub counted: Option<String>,
    /// The model's name, drawn in its colour, when it is known by one.
    pub model: Option<(String, Style)>,
    /// The model's id, as the CLI gives it.
    pub model_id: Option<&'a str>,
}

/// A count measured in the session's window, which takes the window's divisor, so the parts
/// of a 64k window add up to 64k.
pub(super) fn in_window(usage: &ContextUsage, tokens: u64) -> String {
    format::tokens_beside(tokens, usage.raw_window)
}

/// `Context Usage  counted 6s ago`, the model, and `750k/1M tokens (75%)` flush right, with
/// the percent in the colour of how close the session is to compacting. Where the three don't
/// fit one row, the model takes the next.
pub(super) fn title(
    kit: &Kit<'_>,
    columns: &Columns,
    usage: &ContextUsage,
    title: &Title<'_>,
) -> Drawn {
    let mut name = vec![bold(kit, "Context Usage")];
    if let Some(counted) = &title.counted {
        name.push(kit.dim(format!("  {counted}")));
    }
    let mut model = Vec::new();
    if let Some((shown, style)) = &title.model {
        model.push(Span::styled(
            shown.clone(),
            style.add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(id) = title.model_id {
        let gap = if model.is_empty() { "" } else { "  " };
        model.push(kit.dim(format!("{gap}{}", sanitize_line(id).trim())));
    }
    let percent = Span::styled(
        format!("{}%", usage.percentage),
        match level_role(level(usage)) {
            Some(role) => kit.fg(role).add_modifier(Modifier::BOLD),
            None => kit.bold(Role::Text),
        },
    );
    let totals = vec![
        bold(kit, in_window(usage, usage.total_tokens)),
        kit.dim(format!("/{} tokens (", format::window(usage.raw_window))),
        percent,
        kit.dim(")"),
    ];
    let mut drawn = Drawn::new();
    let beside = columns
        .legend
        .filter(|legend| legend + spans_width(&model) + 2 + spans_width(&totals) <= columns.edge);
    match beside {
        Some(legend) => drawn.push(place([
            at(columns.indent(), name),
            at(legend, model),
            ending(columns.edge, totals),
        ])),
        None => {
            drawn.push(place([
                at(columns.indent(), name),
                ending(columns.edge, totals),
            ]));
            if !model.is_empty() {
                drawn.push(place([at(columns.indent(), model)]));
            }
        }
    }
    drawn
}

/// Claude Code's red line for a context past its window, in its words.
pub(super) fn over_limit(usage: &ContextUsage, no_compacting: bool) -> Option<String> {
    if usage.total_tokens <= usage.raw_window {
        return None;
    }
    let over = in_window(usage, usage.total_tokens - usage.raw_window);
    let window = format::window(usage.raw_window);
    Some(if usage.auto_compact_source == "auto" {
        let run = if no_compacting {
            "/clear"
        } else {
            "/compact or /clear"
        };
        format!(
            "Context exceeds the {window}-token limit by {over} tokens \u{2014} run {run} to continue."
        )
    } else {
        let run = if no_compacting { "/clear" } else { "/compact" };
        format!(
            "Context is {over} tokens past the {window}-token compaction window \u{2014} run {run} to reduce usage."
        )
    })
}

/// One row of the legend, and the group of rows it belongs to.
struct LegendRow<'a> {
    glyph: &'static str,
    role: Role,
    row: &'a ContextCategory,
    group: Group,
    /// Free space draws its amount in the colour of how close the session is to compacting.
    amount: Option<Role>,
}

/// Whose a row of the legend is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    ClaudeCode,
    Setup,
    Conversation,
    Unused,
}

impl Group {
    fn label(self) -> &'static str {
        match self {
            Group::ClaudeCode => "Claude Code's own",
            Group::Setup => "your setup",
            Group::Conversation => "conversation",
            Group::Unused => "not used yet",
        }
    }
}

/// The rows the legend shows, in Claude Code's order: every used row that holds something,
/// then free space, then the room held back for auto-compact. With auto-compact off the small
/// reserve it keeps is an ordinary row, as Claude Code draws it.
fn legend_rows<'a>(usage: &'a ContextUsage, glyphs: GridGlyphs) -> Vec<LegendRow<'a>> {
    let holds = |row: &&ContextCategory| row.tokens > 0;
    let ordinary = |row: &&ContextCategory| match row.kind {
        ContextKind::Used => true,
        ContextKind::Buffer => !usage.auto_compact,
        ContextKind::Free | ContextKind::Deferred => false,
    };
    let mut rows: Vec<LegendRow<'a>> = usage
        .categories
        .iter()
        .filter(ordinary)
        .filter(holds)
        .map(|row| LegendRow {
            glyph: glyphs.whole,
            role: color_role(&row.color),
            row,
            // Claude Code 2.1.296's names for what it brings itself and for the conversation.
            group: match (row.kind, row.name.as_str()) {
                (ContextKind::Buffer, _) => Group::Unused,
                (_, "System prompt" | "System tools") => Group::ClaudeCode,
                (_, "Messages") => Group::Conversation,
                _ => Group::Setup,
            },
            amount: None,
        })
        .collect();
    rows.extend(
        usage
            .of_kind(ContextKind::Free)
            .filter(holds)
            .map(|row| LegendRow {
                glyph: glyphs.free,
                role: Role::Faint,
                row,
                group: Group::Unused,
                amount: level_role(level(usage)),
            }),
    );
    if usage.auto_compact {
        rows.extend(
            usage
                .of_kind(ContextKind::Buffer)
                .filter(holds)
                .map(|row| LegendRow {
                    glyph: glyphs.held,
                    role: color_role(&row.color),
                    row,
                    group: Group::Unused,
                    amount: None,
                }),
        );
    }
    rows
}

/// The legend's rows as spans. With `groups`, a bracket joins the neighbouring rows of one
/// group, and its first row says whose they are and what they come to.
fn legend(kit: &Kit<'_>, usage: &ContextUsage, groups: bool) -> Vec<Vec<Span<'static>>> {
    let rows = legend_rows(usage, GridGlyphs::for_kit(kit));
    let mut out = Vec::with_capacity(rows.len());
    for (at, entry) in rows.iter().enumerate() {
        let name = truncate_spans(
            &[Span::raw(sanitize_line(&entry.row.name).trim().to_owned())],
            NAME_CELLS,
        );
        let name: String = name.iter().map(|span| span.content.as_ref()).collect();
        let mut line = vec![
            Span::styled(format!("{} ", entry.glyph), kit.fg(entry.role)),
            Span::raw(ljust(&name, NAME_CELLS)),
            tinted(
                kit,
                rjust(&in_window(usage, entry.row.tokens), TOKENS_CELLS),
                entry.amount,
            ),
            kit.dim(rjust(
                &share(entry.row.tokens, usage.raw_window),
                SHARE_CELLS,
            )),
        ];
        if groups {
            let same = |other: Option<&LegendRow<'_>>| {
                other.is_some_and(|other| other.group == entry.group)
            };
            let before = at.checked_sub(1).and_then(|before| rows.get(before));
            let (first, last) = (!same(before), !same(rows.get(at + 1)));
            let edge = match (first, last) {
                (true, true) => "\u{2500}",
                (true, false) => "\u{256E}",
                (false, true) => "\u{256F}",
                (false, false) => "\u{2502}",
            };
            line.push(Span::raw("   "));
            line.push(Span::styled(format!("{edge} "), kit.fg(Role::Faint)));
            if first {
                let subtotal: u64 = rows[at..]
                    .iter()
                    .take_while(|row| row.group == entry.group)
                    .map(|row| row.row.tokens)
                    .sum();
                line.push(kit.dim(ljust(entry.group.label(), GROUP_CELLS)));
                line.push(Span::raw(rjust(
                    &in_window(usage, subtotal),
                    SUBTOTAL_CELLS,
                )));
            }
        }
        out.push(line);
    }
    out
}

/// One row of the grid: a glyph and a blank for each cell, in the cell's colour.
fn grid_row(kit: &Kit<'_>, usage: &ContextUsage, cells: &[ContextCell]) -> Vec<Span<'static>> {
    let glyphs = GridGlyphs::for_kit(kit);
    let mut row = Vec::with_capacity(cells.len() * 2);
    for cell in cells {
        let kind = usage
            .category(&cell.category)
            .map_or(ContextKind::Used, |category| category.kind);
        let (glyph, role) = match kind {
            ContextKind::Free => (glyphs.free, Role::Faint),
            ContextKind::Buffer if usage.auto_compact => (glyphs.held, color_role(&cell.color)),
            _ if cell.fullness >= WHOLE_FROM => (glyphs.whole, color_role(&cell.color)),
            _ => (glyphs.part, color_role(&cell.color)),
        };
        row.push(Span::styled(glyph, kit.fg(role)));
        row.push(Span::raw(" "));
    }
    row
}

/// The grid with the legend beside it under its caption, or under the grid where the panel
/// is too narrow for both.
pub(super) fn grid_and_legend(kit: &Kit<'_>, columns: &Columns, usage: &ContextUsage) -> Drawn {
    const CAPTION: &str = "Estimated usage by category";
    let indent = columns.indent();
    let grid: Vec<Vec<Span<'static>>> = usage
        .grid
        .iter()
        .map(|cells| grid_row(kit, usage, cells))
        .collect();
    let legend = legend(kit, usage, columns.designed());
    let mut drawn = Drawn::new();
    // A count without a grid leaves the legend nothing to stand beside.
    let Some(beside) = columns.legend.filter(|_| !grid.is_empty()) else {
        for row in grid {
            drawn.push(place([at(indent, row)]));
        }
        if drawn.height() > 0 {
            drawn.push(Vec::new());
        }
        drawn.push(place([at(indent, vec![kit.dim(CAPTION)])]));
        for row in legend {
            drawn.push(place([at(indent, row)]));
        }
        return drawn;
    };
    drawn.push(place([at(beside, vec![kit.dim(CAPTION)])]));
    // A grid wider than its room pushes the legend right on every row alike.
    let widest = grid.iter().map(|row| spans_width(row)).max().unwrap_or(0);
    let beside = beside.max(indent + widest + 1);
    let (mut grid, mut legend) = (grid.into_iter(), legend.into_iter());
    loop {
        let (cells, entry) = (grid.next(), legend.next());
        if cells.is_none() && entry.is_none() {
            break;
        }
        let mut row = vec![Span::raw(" ".repeat(indent))];
        row.extend(cells.unwrap_or_default());
        let pad = beside.saturating_sub(spans_width(&row));
        row.push(Span::raw(" ".repeat(pad)));
        row.extend(entry.unwrap_or_default());
        drawn.push(row);
    }
    drawn
}

/// The lines under the grid, a label, a number and a note each: Claude Code's auto-compact
/// window with how far the session is from compacting, what loads on demand, and the last
/// request with its cache use.
pub(super) fn window_lines(
    kit: &Kit<'_>,
    columns: &Columns,
    usage: &ContextUsage,
    per_turn: Option<u64>,
) -> Drawn {
    let mut drawn = Drawn::new();
    let mut line = |label: &str, value: String, note: Vec<Span<'static>>| {
        let indent = columns.indent();
        let head = vec![
            Span::raw(" ".repeat(indent)),
            bold(kit, ljust(label, LABEL_CELLS)),
            Span::raw(ljust(&value, VALUE_CELLS)),
        ];
        let at_note =
            (indent + LABEL_CELLS + VALUE_CELLS).max(indent + LABEL_CELLS + str_width(&value) + 2);
        if at_note + spans_width(&note) <= columns.width {
            let mut row = head;
            let pad = at_note.saturating_sub(spans_width(&row));
            row.push(Span::raw(" ".repeat(pad)));
            row.extend(note);
            drawn.push(row);
        } else {
            drawn.push(head);
            drawn.push(place([at(indent + 2, note)]));
        }
    };

    let window = format::window(usage.raw_window);
    let value = match usage.auto_compact_source.as_str() {
        "auto" => "auto".to_owned(),
        "experiment" | "clientdata" => format!("auto ({window} tokens)"),
        "unknown-model" => format!("{window} tokens (default for an unrecognized model)"),
        _ => format!("{window} tokens"),
    };
    line(
        "Auto-compact window:",
        value,
        compacting(kit, usage, per_turn),
    );

    let mut deferred: Vec<&ContextCategory> = usage
        .of_kind(ContextKind::Deferred)
        .filter(|row| row.tokens > 0)
        .collect();
    deferred.sort_by_key(|row| std::cmp::Reverse(row.tokens));
    if !deferred.is_empty() {
        let parts: Vec<String> = deferred
            .iter()
            .map(|row| {
                format!(
                    "{} {}",
                    deferred_name(&row.name),
                    in_window(usage, row.tokens)
                )
            })
            .collect();
        line(
            "Loaded on demand:",
            in_window(usage, usage.tokens_of(ContextKind::Deferred)),
            vec![kit.dim(format!(
                "{} \u{b7} out of the window until used",
                parts.join(" \u{b7} ")
            ))],
        );
    }

    if let Some(last) = usage.api_usage.filter(|last| last.sent() > 0) {
        let cached = (last.cache_read_input_tokens as f64 / last.sent() as f64 * 100.0).round();
        line(
            "Last request:",
            format!("{} in", in_window(usage, last.sent())),
            vec![kit.dim(format!(
                "{} from cache ({cached:.0}%) \u{b7} {} written to cache \u{b7} {} out",
                in_window(usage, last.cache_read_input_tokens),
                in_window(usage, last.cache_creation_input_tokens),
                in_window(usage, last.output_tokens),
            ))],
        );
    }
    drawn
}

/// `compacts at 967k · 217k from now · about 23 turns at 9.4k a turn`. The distance takes the
/// colour of how close it is. A session that never compacts by itself says so, with what is
/// left of its window.
fn compacting(kit: &Kit<'_>, usage: &ContextUsage, per_turn: Option<u64>) -> Vec<Span<'static>> {
    let left = usage.limit().saturating_sub(usage.total_tokens);
    let distance = level_role(level(usage)).unwrap_or(Role::Dim);
    let mut note = match usage.compacts_at() {
        Some(at) => vec![
            kit.dim(format!("compacts at {} \u{b7} ", in_window(usage, at))),
            kit.span(format!("{} from now", in_window(usage, left)), distance),
        ],
        None => vec![
            kit.dim("never compacts by itself \u{b7} "),
            kit.span(format!("{} left", in_window(usage, left)), distance),
        ],
    };
    if let Some(per_turn) = per_turn.filter(|tokens| *tokens > 0) {
        let turns = match left / per_turn {
            0 => "less than a turn".to_owned(),
            1 => "about 1 turn".to_owned(),
            turns => format!("about {turns} turns"),
        };
        note.push(kit.dim(format!(
            " \u{b7} {turns} at {} a turn",
            in_window(usage, per_turn)
        )));
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(total: u64, limit: Option<u64>) -> ContextUsage {
        ContextUsage {
            total_tokens: total,
            window: 1_000_000,
            raw_window: 1_000_000,
            auto_compact: limit.is_some(),
            auto_compact_threshold: limit,
            ..ContextUsage::default()
        }
    }

    #[test]
    fn the_level_is_measured_where_the_session_compacts() {
        assert_eq!(level(&usage(750_000, Some(967_000))), Level::Ok);
        assert_eq!(level(&usage(780_000, Some(967_000))), Level::Warn);
        assert_eq!(level(&usage(944_619, Some(967_000))), Level::Danger);
        // Without auto-compact the window's end is what it runs into.
        assert_eq!(level(&usage(944_619, None)), Level::Warn);
        assert_eq!(level(&usage(960_000, None)), Level::Danger);
    }

    #[test]
    fn a_context_past_its_window_is_said_in_claude_codes_words() {
        assert_eq!(over_limit(&usage(900_000, Some(967_000)), false), None);
        let mut over = usage(1_012_000, Some(967_000));
        over.auto_compact_source = "settings".to_owned();
        assert_eq!(
            over_limit(&over, false).as_deref(),
            Some(
                "Context is 12k tokens past the 1M-token compaction window \u{2014} run /compact to reduce usage."
            )
        );
        assert!(
            over_limit(&over, true)
                .unwrap()
                .ends_with("run /clear to reduce usage.")
        );
        over.auto_compact_source = "auto".to_owned();
        assert_eq!(
            over_limit(&over, false).as_deref(),
            Some(
                "Context exceeds the 1M-token limit by 12k tokens \u{2014} run /compact or /clear to continue."
            )
        );
    }
}
