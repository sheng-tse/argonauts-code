//! The `/` menu's Work group: beside each workflow's command, whether it is in use, ready, or
//! needs setting up first.

use std::collections::BTreeMap;
use std::time::Instant;

use argonauts_core::setup_sheet::WorkflowStatus;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

use super::*;
use crate::bridge::transcript::picture_check::assert_rows_match;
use crate::commands::command_items;
use crate::render::Clock;
use crate::theme::{Glyphs, Theme};

/// The picture's menu rows run to the last column the menu leaves empty, so the picture is the
/// screen at 119 columns.
const WIDTH: u16 = 119;

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn statuses() -> BTreeMap<String, WorkflowStatus> {
    [
        ("captain", WorkflowStatus::InUse),
        ("orchestrator", WorkflowStatus::SetUpFirst),
        ("council", WorkflowStatus::Ready),
    ]
    .into_iter()
    .map(|(name, status)| (name.to_owned(), status))
    .collect()
}

/// `/` typed, with the pick moved down to `name`.
fn picked(name: &str) -> BottomState {
    let mut state = BottomState::new();
    state.menus.commands = command_items(None);
    state.menus.workflow_statuses = statuses();
    state.composer.set_text("/");
    let at = state
        .menus
        .commands
        .iter()
        .position(|item| item.name == name)
        .expect("the command");
    for _ in 0..at {
        state
            .menus
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), "/");
    }
    state
}

/// The input and the Work group's first rows, as the picture draws them. Under them the
/// picture skips to Runs, where the screen goes on with the rest of Work down to the pick.
const WORK_ROWS: std::ops::Range<usize> = 0..11;

#[test]
fn the_work_group_says_how_each_workflow_stands() {
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    // The pick on Work's last command starts the menu at Work's name.
    let mut lines: Vec<Line<'static>> = picked("diff").render(&ctx).lines;
    lines.truncate(WORK_ROWS.len());
    assert_rows_match(
        "slash_work_group",
        &lines,
        (
            include_str!("../../tests/golden/bottom/slash_work_group.txt"),
            include_str!("../../tests/golden/bottom/slash_work_group.html"),
        ),
        WORK_ROWS,
        &theme,
        &[],
    );
}

/// The word `name`'s row ends with, if any.
fn word_of(menus: &Menus, name: &str) -> Option<String> {
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let rows = menus.rows("/", &ctx, 20).expect("the menu");
    rows.iter()
        .map(|row| crate::text::plain_text(std::slice::from_ref(row)))
        .find(|row| row.trim_start().starts_with(&format!("/{name} ")))
        .and_then(|row| {
            row.split("  ")
                .filter(|part| !part.is_empty())
                .last()
                .map(str::trim)
                .map(str::to_owned)
        })
}

#[test]
fn only_a_workflows_own_command_says_how_it_stands() {
    let mut menus = Menus::default();
    menus.commands = vec![
        MenuItem::new("spec-first", "Write the spec first")
            .in_group(crate::commands::YOUR_WORKFLOWS),
        MenuItem::new("simplify", "Simplify the code").in_group("Claude Code"),
    ];
    menus.workflow_statuses = [
        ("spec-first".to_owned(), WorkflowStatus::SetUpFirst),
        ("simplify".to_owned(), WorkflowStatus::Ready),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        word_of(&menus, "spec-first").as_deref(),
        Some("set up first")
    );
    // The main agent's own /simplify runs in place of a workflow of that name.
    assert_eq!(
        word_of(&menus, "simplify").as_deref(),
        Some("Simplify the code")
    );
}
