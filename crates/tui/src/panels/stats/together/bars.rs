//! The box's bars: each part's cells by its share of a measure, a CLI's models side by side
//! with a blank cell between CLIs, and what each part draws in, in the bars and as the swatch
//! before its name in the table.

use ratatui::style::Style;
use ratatui::text::Span;

use super::Part;
use crate::base::kit::{Kit, merge};
use crate::base::theme::Role;
use crate::panels::stats::draw::cells_of;

/// A part's cells in a bar without color, in the order of its parts, when it has no texture
/// of its own.
const FILLS: [&str; 4] = ["█", "▓", "▒", "░"];

/// A part's swatch in the table, two cells of what it draws in the bars.
pub(super) fn swatch(kit: &Kit<'_>, part: &Part<'_>) -> Span<'static> {
    Span::styled(glyph(kit, part).repeat(2), colored(kit, part.fill.style))
}

/// The cell a part draws in: its texture, or without one a solid cell, and without color a
/// fill of its own.
fn glyph(kit: &Kit<'_>, part: &Part<'_>) -> &'static str {
    part.fill.texture.unwrap_or(if kit.has_color() {
        "█"
    } else {
        FILLS[part.rank % FILLS.len()]
    })
}

fn colored(kit: &Kit<'_>, style: Style) -> Style {
    if kit.has_color() { style } else { Style::new() }
}

/// One run of a bar: the parts it draws, by their place among the box's parts, and what they
/// used together.
struct Run {
    parts: Vec<usize>,
    used: u64,
}

/// A bar `cells` wide of what each of `parts` used of `measure`: each group in turn, a blank
/// cell between them, each its exact share of the cells. In a group the parts that don't fold
/// come in their order, then one run for those that do. Where only some parts are `lit`, the
/// others keep their cells and glyphs in grey, and a folded run splits between the two.
pub(super) fn bar(
    kit: &Kit<'_>,
    parts: &[Part<'_>],
    measure: usize,
    lit: Option<&[bool]>,
    cells: usize,
) -> Vec<Span<'static>> {
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|at| (parts[*at].group, parts[*at].folds, parts[*at].rank));
    let mut groups: Vec<(usize, Vec<Run>)> = Vec::new();
    for at in order {
        let part = &parts[at];
        let used = part.used(measure);
        if used == 0 {
            continue;
        }
        if groups.last().is_none_or(|(group, _)| *group != part.group) {
            groups.push((part.group, Vec::new()));
        }
        let Some((_, runs)) = groups.last_mut() else {
            continue;
        };
        match runs.last_mut() {
            Some(run) if part.folds && parts[run.parts[0]].folds => {
                run.parts.push(at);
                run.used += used;
            }
            _ => runs.push(Run {
                parts: vec![at],
                used,
            }),
        }
    }
    if groups.is_empty() {
        return vec![Span::styled(
            kit.glyphs.free.repeat(cells),
            kit.fg(Role::Faint),
        )];
    }
    let sums: Vec<u64> = groups
        .iter()
        .map(|(_, runs)| runs.iter().map(|run| run.used).sum())
        .collect();
    let gaps = groups.len() - 1;
    let widths = cells_of(&sums, sums.iter().sum(), cells.saturating_sub(gaps));
    let grey = colored(kit, kit.fg(Role::Faint));
    let mut out = Vec::new();
    for (k, ((_, runs), width)) in groups.iter().zip(widths).enumerate() {
        if k > 0 {
            out.push(Span::raw(" "));
        }
        let used: Vec<u64> = runs.iter().map(|run| run.used).collect();
        let runs_cells = cells_of(&used, used.iter().sum(), width);
        for (run, run_cells) in runs.iter().zip(runs_cells) {
            let first = &parts[run.parts[0]];
            let glyph = glyph(kit, first);
            let style = colored(kit, first.fill.style);
            let Some(lit) = lit else {
                out.push(Span::styled(glyph.repeat(run_cells), style));
                continue;
            };
            let shown: u64 = (run.parts.iter())
                .filter(|at| lit[**at])
                .map(|at| parts[*at].used(measure))
                .sum();
            let split = cells_of(&[shown, run.used - shown], run.used, run_cells);
            out.push(Span::styled(glyph.repeat(split[0]), style));
            out.push(Span::styled(glyph.repeat(split[1]), grey));
        }
    }
    merge(out)
}
