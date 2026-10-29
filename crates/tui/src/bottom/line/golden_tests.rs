//! The line at 120 and 80 columns in three moments, cell for cell: nothing running, with the
//! CLI in use and the one used most; a run going, with Codex at 92% and agy out while a task
//! waits on it; and a terminal that draws no logos, with dots and names. A logo's two cells
//! read as its picture's stand-ins, `◧◨`.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::*;
use crate::bottom::{ApprovalMode, BottomRow, ModeState};
use crate::kit::logo::golden::{count, pictured, with_stand_ins};
use crate::render::Clock;
use crate::theme::{Glyphs, Theme};

fn chip(id: &str, name: &str, windows: &[(&str, f64)]) -> CliUse {
    let mut cli = CliUse::new(name, Role::for_cli(id));
    cli.logo = Some(argonauts_core::logos::logo_for(id, None));
    cli.windows = windows
        .iter()
        .map(|(label, used)| WindowUse::new(*label, *used))
        .collect();
    cli
}

fn claude() -> CliUse {
    let mut claude = chip("claude-code", "Claude Code", &[("5h", 0.71), ("7d", 0.59)]);
    claude.main = true;
    claude
}

/// Nothing running: the CLI in use, then the one used most.
fn at_rest() -> Vec<CliUse> {
    vec![claude(), chip("codex", "Codex", &[("7d", 0.84)])]
}

/// A run going: Codex at 92% with its reset time, and agy out while a task waits on it.
fn held_up() -> Vec<CliUse> {
    let mut codex = chip("codex", "Codex", &[("5h", 0.92)]);
    codex.windows[0].resets = Some("4:40pm".into());
    let mut agy = chip("antigravity-cli", "agy", &[]);
    agy.state = CliState::Out {
        until: Some("Tue 3:45pm".into()),
    };
    vec![claude(), codex, agy]
}

fn theme(logos: bool) -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    if logos { pictured(&theme) } else { theme }
}

/// The row as a terminal shows it, with its logos as their stand-ins, and how many it drew.
fn row(clis: Vec<CliUse>, agents: usize, logos: bool, width: u16) -> (String, usize) {
    let theme = theme(logos);
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    let mode = BottomRow {
        approval: ApprovalMode::Supervised,
        background_agents: agents,
        footer_hints: true,
        ..BottomRow::default()
    };
    let line = BottomLine {
        right: vec![LinePart::PlanUsage(clis)],
        ..BottomLine::default()
    };
    let state = ModeState {
        line: Some(&line),
        ..ModeState::default()
    };
    let drawn = mode.line(&ctx, state);
    let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
    buf.set_line(0, 0, &drawn, width);
    let logos = count(&buf);
    let shown = with_stand_ins(&buf);
    let text: String = (0..width).map(|x| shown[(x, 0)].symbol()).collect();
    (text.trim_end().to_owned(), logos)
}

#[test]
fn the_line_at_120_columns_is_its_picture() {
    assert_eq!(
        row(at_rest(), 0, true, 120),
        (
            "  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts      ◧◨ 5h ▄▄▄▄▁▁  71%  7d ▄▄▄▄▁▁  59%   ◧◨ 7d ▄▄▄▄▄▁  84%".to_owned(),
            2
        )
    );
    assert_eq!(
        row(held_up(), 3, true, 120),
        (
            "  ǁ supervised mode on · ←  3 agents       ◧◨ 5h ▄▄▄▄▁▁  71%  7d ▄  59%   ◧◨ 5h ▄▄▄▄▄▄  92% 4:40pm   ◧◨ out · Tue 3:45pm".to_owned(),
            3
        )
    );
    // Dots and names are wider than logos, so the footer's hint goes to make room. The cycle
    // hint goes only after it, and here it still fits.
    assert_eq!(
        row(at_rest(), 0, false, 120),
        (
            "  ǁ supervised mode on (shift+tab to cycle)        ● Claude Code 5h ▄▄▄▄▁▁  71%  7d ▄▄▄▄▁▁  59%   ○ Codex 7d ▄▄▄▄▄▁  84%".to_owned(),
            0
        )
    );
}

#[test]
fn the_line_at_80_columns_is_its_picture() {
    assert_eq!(
        row(at_rest(), 0, true, 80).0,
        "  ǁ supervised mode on     ◧◨ 5h ▄▄▄▄▁▁  71%  7d ▄▄▄▄▁▁  59%   ◧◨ 7d ▄▄▄▄▄▁  84%"
    );
    assert_eq!(
        row(held_up(), 3, true, 80).0,
        "  ǁ supervised · ←  3 agents          ◧◨ 5h ▄▄▄▄▁▁  71%   ◧◨ 92% 4:40pm   ◧◨ out"
    );
    assert_eq!(
        row(at_rest(), 0, false, 80).0,
        "  ǁ supervised mode on     ● Claude Code 5h ▄▄▄▄▁▁  71%   ○ Codex 7d ▄▄▄▄▄▁  84%"
    );
}

/// Each number ends in the same column whatever its digits, and a bar's track shows thin.
#[test]
fn a_chips_numbers_line_up_after_its_bars() {
    let numbers = [0.0, 0.05, 0.42, 1.0].map(|used| {
        let mut cli = chip("claude-code", "Claude Code", &[("5h", used)]);
        cli.main = true;
        row(vec![cli], 0, true, 120).0
    });
    let ends: Vec<&str> = numbers
        .iter()
        .map(|row| row.rsplit("5h ").next().unwrap_or_default())
        .collect();
    assert_eq!(
        ends,
        ["▁▁▁▁▁▁   0%", "▄▁▁▁▁▁   5%", "▄▄▄▁▁▁  42%", "▄▄▄▄▄▄ 100%"]
    );
}
