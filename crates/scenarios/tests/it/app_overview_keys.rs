//! The Overview's keys on the real app, at 120 × 38 and 177 × 40. Overview › Overview's year
//! and figures have nothing to pick, so ↓ leaves the keys on the sub-tabs, where → still goes
//! along them, and the key line offers no ↓ there. Overview › Models takes them into its
//! models as before, and Shift+Tab from there back to Overview brings them up to the sub-tabs.

use anyhow::Context as _;
use argonauts_scenarios::WAIT;
use argonauts_testkit::Key;
use argonauts_testkit::approved::{self, Drawing, Manifest, Step};
use argonauts_testkit::pty::Screen;

use crate::drawings_support::bring;

/// The sizes the screen is held to, as `cols × rows`.
const SIZES: [(u16, u16); 2] = [(120, 38), (177, 40)];

/// Models' own block, which Overview › Overview doesn't draw.
const MODELS: &str = "★ Favorite: ";

/// The panel's key line, the row that ends with its way out.
fn key_line(screen: &Screen) -> String {
    let line = screen
        .visible
        .iter()
        .rev()
        .find(|row| row.contains("esc closes"));
    line.cloned().unwrap_or_default()
}

#[test]
fn app_down_stays_on_the_overview_sub_tabs_where_nothing_below_moves() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    // A home with usage from every CLI the app ran, and more models than Models shows at once.
    let mut entry = (manifest.drawings.iter())
        .find(|entry| entry.file == "behaviors/label-map@120x38.txt")
        .cloned()
        .context("a home with usage")?;
    entry.steps = vec![
        Step::Wait("shift\\+tab to cycle".to_owned()),
        Step::Send("/overview\r".to_owned()),
        Step::Wait("Yes, import it".to_owned()),
        Step::Press("2".to_owned()),
        Step::Wait("Overview    Models    Behaviors".to_owned()),
    ];
    for (cols, rows) in SIZES {
        let size = format!("{cols}x{rows}");
        let mut drawing = Drawing::load(&dir.join(&entry.file))?;
        (drawing.cols, drawing.rows) = (cols, rows);
        let mut at = bring(&entry, &dir, &drawing)?;
        let pty = &mut at.pty;

        pty.press(Key::Down)?;
        let on_sub_tabs = pty.wait_for_visible("↑  tabs", WAIT)?;
        let keys = key_line(&on_sub_tabs);
        assert!(!keys.contains('↓'), "{size}: no ↓ offered: {keys}");
        // A ↓ with nothing to go into keeps → on the sub-tabs.
        pty.press(Key::Down)?;
        pty.press(Key::Right)?;
        pty.wait_for_visible(MODELS, WAIT)
            .with_context(|| format!("{size}: → after ↓ still goes along the sub-tabs"))?;

        pty.press(Key::Down)?;
        pty.press(Key::Down)?;
        pty.wait_for_visible("3-\\d+ of \\d+ models", WAIT)
            .with_context(|| format!("{size}: ↓ goes into the models and pages them"))?;

        pty.press(Key::ShiftTab)?;
        pty.wait_until("Overview › Overview", WAIT, |screen| {
            !screen.visible_text().contains(MODELS)
        })?;
        pty.press(Key::Right)?;
        pty.wait_for_visible(MODELS, WAIT)
            .with_context(|| format!("{size}: Shift+Tab left the keys on the sub-tabs"))?;
    }
    Ok(())
}
