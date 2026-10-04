use ratatui::text::{Line, Span};

use crate::bottom::BottomState;
use crate::cell::HistoryCell;
use crate::history::{History, LiveBlock};
use crate::render::RenderCtx;
use crate::text::truncate_line;
use crate::theme::Role;

/// A block's header and one more row, kept while there is room for every block to keep them.
const BLOCK_MIN_ROWS: usize = 2;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveView {
    pub lines: Vec<Line<'static>>,
    /// How many of the first `lines` continue the transcript, such as a streaming answer or a
    /// working agent's block. They trail right under history, and the rest pin to the bottom.
    pub trailing: usize,
    /// (column, row) within `lines`.
    pub cursor: Option<(u16, u16)>,
}

/// The blocks get at most half of `max_height`, so a terminal made a little shorter pushes only
/// printed history into scrollback, never a frozen copy of a live block. Every block shrinks to
/// its header and a line, then its header alone, before the oldest give way to a
/// `+N more working` row. Cards and the composer always stay visible.
pub fn compose_live(
    blocks: &[LiveBlock],
    history: &History,
    bottom: &BottomState,
    ctx: &RenderCtx<'_>,
    max_height: u16,
) -> LiveView {
    let bottom_view = bottom.render_live(ctx, usize::from(max_height), text_streaming(history));
    compose(blocks, history, bottom_view, ctx, max_height)
}

/// As [`compose_live`], with a docked view's rows where the input, the mode line, and the
/// panel go. The working line stays above it.
pub fn compose_docked(
    blocks: &[LiveBlock],
    history: &History,
    bottom: &BottomState,
    ctx: &RenderCtx<'_>,
    max_height: u16,
    view: Vec<Line<'static>>,
) -> LiveView {
    let bottom_view =
        bottom.render_docked(ctx, usize::from(max_height), text_streaming(history), view);
    compose(blocks, history, bottom_view, ctx, max_height)
}

fn text_streaming(history: &History) -> bool {
    history
        .stream_source()
        .is_some_and(|source| !source.trim().is_empty())
}

fn compose(
    blocks: &[LiveBlock],
    history: &History,
    bottom_view: crate::bottom::BottomView,
    ctx: &RenderCtx<'_>,
    max_height: u16,
) -> LiveView {
    let max = usize::from(max_height);
    let blocks: Vec<&LiveBlock> = blocks.iter().filter(|b| !b.lines.is_empty()).collect();
    let above = !blocks.is_empty() || history.printed_any();
    let separator = usize::from(above && bottom_view.lines.len() < max);
    let room = max
        .saturating_sub(bottom_view.lines.len() + separator)
        .min(max / 2);

    let mut lines = fit_blocks(&blocks, history, ctx, room);
    let trailing = lines.len();
    if separator == 1 {
        lines.push(Line::default());
    }
    let offset = lines.len() as u16;
    lines.extend(bottom_view.lines);
    LiveView {
        lines,
        trailing,
        cursor: bottom_view
            .cursor
            .map(|(column, row)| (column, row.saturating_add(offset))),
    }
}

fn fit_blocks(
    blocks: &[&LiveBlock],
    history: &History,
    ctx: &RenderCtx<'_>,
    room: usize,
) -> Vec<Line<'static>> {
    let wants: Vec<usize> = blocks.iter().map(|b| b.lines.len()).collect();
    let gaps: Vec<usize> = blocks.iter().map(|b| usize::from(b.gap_before)).collect();
    let total: usize = wants.iter().sum::<usize>() + gaps.iter().sum::<usize>();
    if total <= room {
        return stack(blocks, &wants, 0, history, ctx);
    }

    let mins_at =
        |least: usize| -> Vec<usize> { wants.iter().map(|want| (*want).min(least)).collect() };
    let fits_all =
        |mins: &[usize]| mins.iter().zip(&gaps).map(|(m, g)| m + g).sum::<usize>() <= room;
    let mins = [BLOCK_MIN_ROWS, 1]
        .into_iter()
        .map(mins_at)
        .find(|mins| fits_all(mins))
        .unwrap_or_else(|| mins_at(1));
    let mut first = 0;
    let more_gap = |first: usize| {
        gaps.first()
            .copied()
            .unwrap_or(0)
            .min(usize::from(first > 0))
    };
    loop {
        let more_row = if first > 0 { 1 + more_gap(first) } else { 0 };
        let need: usize = (first..blocks.len())
            .map(|i| mins[i] + gaps[i])
            .sum::<usize>()
            + more_row;
        if need <= room || first == blocks.len() {
            break;
        }
        first += 1;
    }

    let more_row = if first > 0 { 1 + more_gap(first) } else { 0 };
    let shown = first..blocks.len();
    let gap_rows: usize = shown.clone().map(|i| gaps[i]).sum();
    let budget = room.saturating_sub(more_row + gap_rows);
    let allot = water_fill(&wants[shown.clone()], &mins[shown.clone()], budget);

    let mut lines = Vec::new();
    if first > 0 && room >= more_row {
        if more_gap(first) == 1 {
            lines.push(Line::default());
        }
        let more = Line::from(Span::styled(
            format!("  +{first} more working"),
            ctx.theme.style(Role::Dim),
        ));
        lines.push(truncate_line(&more, usize::from(ctx.width)));
    }
    let mut rows = vec![0; blocks.len()];
    rows[shown.clone()].copy_from_slice(&allot);
    lines.extend(stack(blocks, &rows, first, history, ctx));
    lines.truncate(room);
    lines
}

/// Rows per block: each starts at its minimum, then every block that wants more gets one row
/// per round until the budget runs out, the newest first within a round.
fn water_fill(wants: &[usize], mins: &[usize], budget: usize) -> Vec<usize> {
    let mut allot = mins.to_vec();
    let mut left = budget.saturating_sub(mins.iter().sum());
    while left > 0 {
        let mut raised = false;
        for (given, want) in allot.iter_mut().zip(wants).rev() {
            if left == 0 {
                break;
            }
            if *given < *want {
                *given += 1;
                left -= 1;
                raised = true;
            }
        }
        if !raised {
            break;
        }
    }
    allot
}

fn stack(
    blocks: &[&LiveBlock],
    rows: &[usize],
    first: usize,
    history: &History,
    ctx: &RenderCtx<'_>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (block, &budget) in blocks.iter().zip(rows).skip(first) {
        if budget == 0 {
            continue;
        }
        if block.gap_before {
            lines.push(Line::default());
        }
        lines.extend(squeeze_block(block, budget, history, ctx));
    }
    lines
}

fn squeeze_block(
    block: &LiveBlock,
    rows: usize,
    history: &History,
    ctx: &RenderCtx<'_>,
) -> Vec<Line<'static>> {
    if block.lines.len() <= rows {
        return block.lines.clone();
    }
    let mut lines = match block.id.and_then(|id| history.active(id)) {
        Some(cell) => cell.lines_within(ctx, rows),
        None => block.lines[block.lines.len() - rows..].to_vec(),
    };
    lines.truncate(rows);
    lines
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::bottom::CardPrompt;
    use crate::cell::{Cell, HistoryCell};
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    #[derive(Debug)]
    struct Agent {
        name: &'static str,
        rows: usize,
    }

    impl HistoryCell for Agent {
        fn lines(&self, _ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
            (0..self.rows)
                .map(|i| Line::from(format!("{} {i}", self.name)))
                .collect()
        }
    }

    fn live_agents(history: &mut History, ctx: &RenderCtx<'_>, sizes: &[usize]) -> Vec<LiveBlock> {
        const NAMES: [&str; 4] = ["a", "b", "c", "d"];
        sizes
            .iter()
            .zip(NAMES)
            .map(|(&rows, name)| {
                let cell: Box<dyn HistoryCell> = Box::new(Agent { name, rows });
                let id = history.push_active(Cell::Custom(cell));
                LiveBlock {
                    id: Some(id),
                    gap_before: true,
                    lines: history.active(id).unwrap().lines(ctx),
                }
            })
            .collect()
    }

    fn bottom() -> BottomState {
        BottomState::new()
    }

    #[test]
    fn everything_fits_with_a_blank_row_above_the_bottom() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut history = History::new();
        let blocks = live_agents(&mut history, &ctx, &[2]);
        let view = compose_live(&blocks, &history, &bottom(), &ctx, 20);
        assert_eq!(
            plain_text(&view.lines),
            "\na 0\na 1\n\n────────────────────────────────────────\n❯ Try \"review the last commit\"\n────────────────────────────────────────\n  ǁ supervised mode on"
        );
        assert_eq!(view.cursor, Some((2, 5)));
    }

    #[test]
    fn no_blank_row_on_an_empty_screen() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let history = History::new();
        let view = compose_live(&[], &history, &bottom(), &ctx, 20);
        assert_eq!(view.lines.len(), 4);
        assert_eq!(view.cursor, Some((2, 1)));
    }

    #[test]
    fn blocks_shrink_before_any_is_hidden() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut history = History::new();
        let blocks = live_agents(&mut history, &ctx, &[6, 6, 2]);
        // Half of 22 rows for the blocks, less their gaps: 8 rows for 14 wanted.
        let view = compose_live(&blocks, &history, &bottom(), &ctx, 22);
        assert_eq!(view.lines.len(), 16);
        let text = plain_text(&view.lines);
        assert!(text.contains("a 0"), "{text}");
        assert!(text.contains("b 0"), "{text}");
        assert!(text.contains("c 0\nc 1"), "{text}");
        assert!(text.contains("b 5"), "{text}");
        assert!(!text.contains("more working"), "{text}");

        let blocks = live_agents(&mut History::new(), &ctx, &[6, 6, 6]);
        let view = compose_live(&blocks, &history, &bottom(), &ctx, 14);
        let text = plain_text(&view.lines);
        assert!(text.starts_with("\na 0\n\nb 0\n\nc 0\n"), "{text}");
        assert!(!text.contains("more working"), "{text}");
    }

    #[test]
    fn blocks_take_at_most_half_the_height() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut history = History::new();
        let blocks = live_agents(&mut history, &ctx, &[30]);
        let view = compose_live(&blocks, &history, &bottom(), &ctx, 34);
        // Seventeen rows of block with its gap, a blank row, and the bottom.
        assert_eq!(view.lines.len(), 17 + 1 + 4);
    }

    #[test]
    fn the_oldest_blocks_hide_last_behind_one_row() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut history = History::new();
        let blocks = live_agents(&mut history, &ctx, &[6, 6, 6, 6]);
        // Seven rows can't hold four headers with their gaps.
        let view = compose_live(&blocks, &history, &bottom(), &ctx, 14);
        let text = plain_text(&view.lines);
        assert!(
            text.starts_with("\n  +2 more working\n\nc 0\n\nd 0\nd 1\n\n"),
            "{text}"
        );
        assert_eq!(view.lines.len(), 12);

        let view = compose_live(&blocks, &history, &bottom(), &ctx, 10);
        let text = plain_text(&view.lines);
        assert!(text.contains("  +3 more working"), "{text}");
        assert!(text.contains("d 0") && !text.contains("c 0"), "{text}");
    }

    #[test]
    fn the_stream_tail_keeps_its_newest_rows() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let history = History::new();
        let tail = LiveBlock {
            id: None,
            gap_before: false,
            lines: (0..10).map(|i| Line::from(format!("tail {i}"))).collect(),
        };
        let view = compose_live(&[tail], &history, &bottom(), &ctx, 8);
        let text = plain_text(&view.lines);
        assert!(text.starts_with("tail 7\ntail 8\ntail 9\n\n"), "{text}");
    }

    #[test]
    fn the_card_and_composer_always_stay() {
        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut history = History::new();
        let blocks = live_agents(&mut history, &ctx, &[6, 6]);
        let mut bottom = bottom();
        bottom.card = Some(CardPrompt::new(
            1,
            "Merge?",
            "Merge?",
            vec![
                crate::bottom::CardOption::new("Yes"),
                crate::bottom::CardOption::new("No"),
            ],
        ));
        let view = compose_live(&blocks, &history, &bottom, &ctx, 9);
        let text = plain_text(&view.lines);
        assert!(text.contains("◆ Merge?"), "{text}");
        assert!(text.contains("1. Yes"), "{text}");
        assert!(text.contains("ǁ supervised mode on"), "{text}");
        assert!(view.lines.len() <= 9);
    }
}
