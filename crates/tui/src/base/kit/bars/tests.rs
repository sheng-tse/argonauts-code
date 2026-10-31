use super::*;
use crate::base::text::spans_width;
use crate::base::theme::{ColorLevel, Glyphs, Theme};

fn dark() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn text(spans: &[Span<'_>]) -> String {
    spans.iter().map(|s| s.content.as_ref()).collect()
}

/// The context mockup's parts: seven categories and free, of a 1M window.
fn context_stack() -> Stack {
    let parts = vec![
        (3_400, Role::CatSystem),
        (12_000, Role::CatTools),
        (2_600, Role::CatMcp),
        (1_100, Role::CatAgents),
        (4_900, Role::CatMemory),
        (2_200, Role::CatSkills),
        (186_000, Role::CatMessages),
        (787_800, Role::CatFree),
    ];
    Stack {
        parts,
        total: 1_000_000,
        tick: Some(950_000),
        held_from: Some(950_000),
        track: Role::CatFree,
    }
}

#[test]
fn every_part_gets_a_cell_and_a_cell_holds_at_most_one_boundary() {
    let stack = context_stack();
    for cells in [40, 60, 90, 120, 196] {
        let units = stack.units(cells, 8);
        assert_eq!(units.len(), cells * 8);
        for &(value, role) in &stack.parts {
            if value > 0 {
                let n = units.iter().filter(|r| **r == role).count();
                assert!(n >= 8, "{role:?} has {n} eighths at {cells} cells");
            }
        }
        for (c, cell) in units.chunks(8).enumerate() {
            let boundaries = cell.windows(2).filter(|w| w[0] != w[1]).count();
            assert!(boundaries <= 1, "cell {c} at {cells} cells: {cell:?}");
        }
    }
}

#[test]
fn a_short_bar_folds_parts_smaller_than_a_cell_into_other() {
    let stack = context_stack();
    let folded = stack.folded(30);
    let roles: Vec<Role> = folded.iter().map(|p| p.1).collect();
    // A cell of 30 is 33.3k tokens, so only messages and free keep their own parts.
    assert_eq!(roles, [Role::CatMessages, Role::CatOther, Role::CatFree]);
    let other = folded.iter().find(|p| p.1 == Role::CatOther).map(|p| p.0);
    assert_eq!(other, Some(3_400 + 12_000 + 2_600 + 1_100 + 4_900 + 2_200));
    let one_small = Stack {
        parts: vec![
            (500_000, Role::CatMessages),
            (10_000, Role::CatTools),
            (490_000, Role::CatFree),
        ],
        ..context_stack()
    };
    assert_eq!(
        one_small.folded(30),
        one_small.parts,
        "one small part has nothing to fold with"
    );
    assert_eq!(
        stack.folded(40),
        stack.parts,
        "40 cells and up keep every part"
    );
}

#[test]
fn the_tick_lands_on_the_caps_cell_and_held_follows_it() {
    let theme = plain();
    let kit = Kit::new(&theme);
    for cells in [30, 60, 90, 120] {
        let row = text(&stacked(&kit, &context_stack(), cells));
        let tick = row.chars().position(|c| c == '│');
        assert_eq!(
            tick,
            Some(round(0.95 * cells as f64).min(cells - 1)),
            "{row}"
        );
        assert_eq!(row.chars().count(), cells);
    }
    let theme = dark();
    let kit = Kit::new(&theme);
    let row = stacked(&kit, &context_stack(), 90);
    let last = row.last().map(|s| s.style.bg);
    assert_eq!(last, Some(theme.color(Role::Held)), "past the tick is held");
}

#[test]
fn the_mockups_bar_draws_its_edges_in_eighths() {
    let theme = dark();
    let kit = Kit::new(&theme);
    let row = stacked(&kit, &context_stack(), 90);
    assert_eq!(spans_width(&row), 90);
    let glyphs: String = text(&row).chars().filter(|c| *c != ' ').collect();
    // tools into mcp, five small parts a cell each, messages into free at seven eighths, the tick.
    assert_eq!(glyphs, "▏▏▏▏▏▉▏");
    let plain_theme = plain();
    let row = text(&stacked(&Kit::new(&plain_theme), &context_stack(), 90));
    assert_eq!(row, format!("{}{}│░░░", "█".repeat(23), "░".repeat(63)));
}

#[test]
fn an_empty_or_unknown_whole_is_all_track() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let empty = Stack {
        parts: Vec::new(),
        total: 0,
        tick: None,
        held_from: None,
        track: Role::CatFree,
    };
    assert_eq!(text(&stacked(&kit, &empty, 12)), "░".repeat(12));
}

#[test]
fn a_block_meter_fills_by_eighths() {
    let theme = dark();
    let kit = Kit::new(&theme);
    let row = block_meter(&kit, 0.71, 20, Role::BarFill, Role::BarTrack);
    assert_eq!(spans_width(&row), 20);
    // 0.71 of 160 eighths is 113.6, so 114: fourteen full cells and a quarter cell.
    assert_eq!(text(&row).trim(), "▎");
    assert_eq!(row[0].style.bg, theme.color(Role::BarFill));
    let plain_theme = plain();
    let row = text(&block_meter(
        &Kit::new(&plain_theme),
        0.71,
        20,
        Role::BarFill,
        Role::BarTrack,
    ));
    assert_eq!(row, format!("{}{}", "█".repeat(14), "░".repeat(6)));
    let over = text(&block_meter(
        &Kit::new(&plain_theme),
        1.05,
        10,
        Role::Danger,
        Role::BarTrack,
    ));
    assert_eq!(over, "█".repeat(10));
}

#[test]
fn a_line_meter_has_half_cells_and_a_tick() {
    let theme = plain();
    let kit = Kit::new(&theme);
    assert_eq!(
        text(&line_meter(&kit, 0.75, 10, Role::BarFill, None)),
        "━━━━━━━╸──"
    );
    assert_eq!(
        text(&line_meter(&kit, 0.71, 10, Role::BarFill, None)),
        "━━━━━━━───"
    );
    assert_eq!(
        text(&line_meter(&kit, 0.5, 10, Role::BarFill, Some(0.8))),
        "━━━━━───┃─"
    );
}

#[test]
fn a_half_meter_fills_the_lower_half_of_each_cell_to_half_a_cell() {
    let theme = dark();
    let kit = Kit::new(&theme);
    let colors = |row: &[Span<'_>]| -> Vec<_> { row.iter().map(|span| span.style.fg).collect() };
    let (fill, track) = (theme.color(Role::BarFill), theme.color(Role::BarTrack));

    let row = half_meter(&kit, 0.75, 10, Role::BarFill, None);
    assert_eq!(text(&row), "▄▄▄▄▄▄▄▖▄▄");
    assert_eq!(colors(&row), [fill, track], "the half cell is fill");
    assert_eq!(row[0].content, "▄▄▄▄▄▄▄▖");
    assert!(row.iter().all(|span| span.style.bg.is_none()), "{row:?}");

    let row = half_meter(&kit, 0.5, 10, Role::BarFill, Some(0.8));
    assert_eq!(text(&row), "▄▄▄▄▄▄▄▄┃▄");
    assert_eq!(colors(&row), [fill, track, theme.color(Role::Tick), track]);
    assert_eq!(text(&half_meter(&kit, 0.0, 4, Role::BarFill, None)), "▄▄▄▄");
    assert_eq!(text(&half_meter(&kit, 2.0, 4, Role::BarFill, None)), "▄▄▄▄");
}

#[test]
fn a_half_meter_without_color_is_the_line_meter() {
    let theme = plain();
    let kit = Kit::new(&theme);
    assert_eq!(
        text(&half_meter(&kit, 0.75, 10, Role::BarFill, Some(0.9))),
        text(&line_meter(&kit, 0.75, 10, Role::BarFill, Some(0.9)))
    );
    let mut ascii = Theme::dark();
    ascii.glyphs = Glyphs::ASCII;
    let kit = Kit::new(&ascii);
    assert_eq!(
        text(&half_meter(&kit, 0.5, 6, Role::BarFill, None)),
        "===---"
    );
}

#[test]
fn a_half_stacked_bar_gives_each_part_its_cells_in_its_own_color() {
    let theme = dark();
    let kit = Kit::new(&theme);
    let stack = Stack {
        parts: vec![
            (60, Role::AgentOrange),
            (30, Role::AgentBlue),
            (1, Role::AgentCyan),
        ],
        total: 200,
        tick: None,
        held_from: None,
        track: Role::BarTrack,
    };
    let row = half_stacked(&kit, &stack, 20);
    assert_eq!(text(&row), "▄".repeat(20));
    let cells: Vec<(usize, _)> = row
        .iter()
        .map(|span| (span.content.chars().count(), span.style.fg))
        .collect();
    let color = |role| theme.color(role);
    // The smallest part still gets a cell, and what's unspent stays track.
    assert_eq!(
        cells,
        [
            (6, color(Role::AgentOrange)),
            (3, color(Role::AgentBlue)),
            (1, color(Role::AgentCyan)),
            (10, color(Role::BarTrack)),
        ]
    );
    assert!(row.iter().all(|span| span.style.bg.is_none()), "{row:?}");

    // The context bar keeps its tick and the part held back past it.
    let row = half_stacked(&kit, &context_stack(), 60);
    assert_eq!(spans_width(&row), 60);
    assert_eq!(text(&row).matches('┃').count(), 1);
    assert_eq!(
        row.last().map(|span| span.style.fg),
        Some(color(Role::Held))
    );
}

#[test]
fn a_half_stacked_bar_without_color_is_the_stacked_bar() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let stack = context_stack();
    assert_eq!(
        text(&half_stacked(&kit, &stack, 40)),
        text(&stacked(&kit, &stack, 40))
    );
}

#[test]
fn a_stacked_line_keeps_each_parts_color() {
    let theme = dark();
    let kit = Kit::new(&theme);
    let row = stacked_line(&kit, &context_stack(), 60);
    assert_eq!(spans_width(&row), 60);
    let messages = row
        .iter()
        .find(|s| s.style.fg == theme.color(Role::CatMessages))
        .map(|s| s.content.to_string());
    assert!(messages.is_some_and(|m| m.starts_with('━')));
    assert!(text(&row).contains('┃'));
}

#[test]
fn pills_show_any_amount_over_nothing() {
    let theme = plain();
    let kit = Kit::new(&theme);
    assert_eq!(text(&pills(&kit, 0.0, 5, Role::BarFill)), "▱▱▱▱▱");
    assert_eq!(text(&pills(&kit, 0.01, 5, Role::BarFill)), "▰▱▱▱▱");
    assert_eq!(text(&pills(&kit, 0.85, 5, Role::Warn)), "▰▰▰▰▱");
    assert_eq!(text(&pills(&kit, 1.3, 5, Role::Danger)), "▰▰▰▰▰");
    let mut ascii = plain();
    ascii.glyphs = Glyphs::ASCII;
    assert_eq!(
        text(&pills(&Kit::new(&ascii), 0.4, 5, Role::BarFill)),
        "##---"
    );
}

#[test]
fn columns_stand_two_rows_high() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let rows = columns(
        &kit,
        &[1.0, 2.0, 4.0],
        2,
        Role::CatMessages,
        Some(Role::Accent),
    );
    let rows: Vec<String> = rows.iter().map(|r| text(r)).collect();
    assert_eq!(rows, ["    █", "▄ █ █"]);
}

#[test]
fn a_blank_always_parts_a_bar_from_what_follows() {
    let theme = crate::base::theme::Theme::dark();
    let kit = Kit::new(&theme);
    let mut row = thin_bar(&kit, 1.0, 4, Role::CliClaude);
    then(&mut row, vec![Span::raw("100%")]);
    let text: String = row.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "━━━━ 100%");
    let mut spaced = thin_bar(&kit, 0.5, 4, Role::CliClaude);
    then(&mut spaced, vec![Span::raw("  50%")]);
    let text: String = spaced.iter().map(|span| span.content.as_ref()).collect();
    assert_eq!(text, "━━━━  50%", "a field that starts blank gets no more");
    assert_eq!(touching_bar("model:Fable━━━━  0%"), Some(10));
    assert_eq!(touching_bar("━━━━100%"), Some(3));
    assert_eq!(touching_bar("week ━━━━ 36%  ▪▪▪ 4"), None);
}
