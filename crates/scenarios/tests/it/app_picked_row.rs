//! A run's row picked under the input takes the keys from it, as Claude Code moves the cursor
//! out of its input: the input drops its cursor and the line under it shows only the row's keys,
//! until esc gives them back.

use argonauts_scenarios::WAIT;
use argonauts_testkit::Key;
use argonauts_testkit::screen::Grid;

use crate::app_support::*;

/// The sizes the screen is held to, as `cols × rows`.
const SIZES: [(u16, u16); 2] = [(120, 38), (177, 40)];

const ROW_KEYS: &str = "enter to view · x to stop";

/// Whether the input, the row under a rule that starts with `❯`, draws its block cursor.
fn input_has_cursor(grid: &Grid) -> bool {
    (1..grid.rows()).any(|row| {
        let under_a_rule = grid.text(row - 1, 0, grid.cols()).starts_with('─');
        let cells = grid.row(row);
        under_a_rule
            && cells.first().is_some_and(|cell| cell.text == "❯")
            && cells.iter().any(|cell| cell.inverse)
    })
}

#[test]
fn app_a_picked_run_row_takes_the_cursor_and_the_key_line() -> anyhow::Result<()> {
    for (cols, rows) in SIZES {
        let scenario = scenario()?;
        let mut pty = scenario.launch_sized(&[], rows, cols)?;
        wait_ready(&scenario, &pty)?;
        start_captain(&scenario, &mut pty, requests::SLOW_TASK)?;
        wait_worker_started(&scenario)?;
        pty.wait_for_visible("shift\\+tab to cycle", WAIT)?;
        assert!(
            input_has_cursor(&pty.cells()),
            "{cols}x{rows}: the input has its cursor"
        );

        pty.press(Key::Down)?;
        let picked = pty.wait_for_visible(ROW_KEYS, WAIT)?;
        let key_line = picked
            .visible
            .iter()
            .find(|row| row.contains(ROW_KEYS))
            .expect("the key line");
        // The bottom line's own parts keep their place at the right end.
        assert!(
            key_line.trim_start().starts_with(ROW_KEYS),
            "{cols}x{rows}: the row's keys come first, with no mode: {key_line}"
        );
        assert!(
            !picked.visible_text().contains("shift+tab to cycle"),
            "{cols}x{rows}: the mode is left out:\n{}",
            picked.visible_text()
        );
        assert!(
            picked
                .visible
                .iter()
                .any(|row| row.trim_start().starts_with('❯')),
            "{cols}x{rows}: the row has its ❯:\n{}",
            picked.visible_text()
        );
        assert!(
            !input_has_cursor(&pty.cells()),
            "{cols}x{rows}: the input drops its cursor"
        );

        pty.press(Key::Esc)?;
        let back = pty.wait_for_visible("shift\\+tab to cycle", WAIT)?;
        assert!(
            !back.visible_text().contains(ROW_KEYS),
            "{cols}x{rows}: the row gives the keys back:\n{}",
            back.visible_text()
        );
        assert!(
            input_has_cursor(&pty.cells()),
            "{cols}x{rows}: the cursor comes back"
        );
    }
    Ok(())
}
