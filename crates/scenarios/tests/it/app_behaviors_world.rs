//! The Behaviors world on the real app: the map's picture over the cells its drawing gives it,
//! the line under the map for what the pointer rests on, and a universe that keeps moving when
//! nothing happened lately, at 120 × 38 and 177 × 40 in a terminal that takes pictures as
//! Ghostty does. With `ARGONAUTS_BIN` pointing at a release
//! build, that build is held to them.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use argonauts_scenarios::WAIT;
use argonauts_testkit::Pty;
use argonauts_testkit::approved::{self, Drawing, Entry, Manifest, Step};
use argonauts_testkit::pty::deadline;
use argonauts_testkit::screen::{Cell, Grid, PLACEHOLDER};

use crate::drawings_support::{bring, found, reach};

/// The Behaviors map's cells: the first rows under the sub-tabs with cells that `picture`
/// says show a picture other than a logo, one after another, row by row from the sub-tabs'
/// own row. The pointer's line under them may show a picture of its own. The drawing's
/// pictures go under no id, and the screen's logos are placeholders.
fn map_cells(grid: &Grid, picture: impl Fn(&Cell) -> bool) -> anyhow::Result<Vec<(u16, u16)>> {
    let (tabs, _) = found(grid, SUB_TABS).with_context(|| format!("no {SUB_TABS:?}"))?;
    let in_row = |row: u16| -> Vec<(u16, u16)> {
        (0..grid.cols())
            .filter(|&col| picture(grid.cell(row, col)))
            .map(|col| (row - tabs, col))
            .collect()
    };
    Ok((tabs..grid.rows())
        .map(in_row)
        .skip_while(Vec::is_empty)
        .take_while(|cells| !cells.is_empty())
        .flatten()
        .collect())
}

/// The Behaviors map's sub-tabs, above the map.
const SUB_TABS: &str = "1 Map · 2 Planet";

/// The Behaviors map's picture shows over the cells the approved label map gives it, at
/// 120 × 38 and 177 × 40, in a terminal that takes pictures as Ghostty does. The app places it
/// straight on the screen from the map's first cell, once it is drawn off the frame.
#[test]
fn the_behaviors_map_shows_its_picture_at_both_sizes() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let entry = manifest
        .drawings
        .iter()
        .find(|entry| entry.file == "behaviors/label-list-down-1@120x38.txt")
        .context("a drawing of the label map's list")?;
    let reached = Drawing::load(&dir.join(&entry.file))?;
    for (cols, rows) in [(120, 38), (177, 40)] {
        let approved = Drawing::load(&dir.join(format!("behaviors/label-map@{cols}x{rows}.txt")))?;
        let wanted = map_cells(&approved.grid(), |cell| {
            cell.picture.is_some_and(|picture| picture.id == 0)
        })?;
        assert!(!wanted.is_empty(), "{cols}x{rows}: no map in the drawing");
        let mut drawing = reached.clone();
        (drawing.cols, drawing.rows) = (cols, rows);
        let at = bring(entry, &dir, &drawing)?;
        let on_screen = || {
            map_cells(&at.pty.cells(), |cell| {
                cell.picture.is_some_and(|picture| picture.shows)
                    && !cell.text.starts_with(PLACEHOLDER)
            })
        };
        let until = deadline::after(WAIT);
        let mut shown = on_screen()?;
        while shown != wanted && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
            shown = on_screen()?;
        }
        assert!(
            shown == wanted,
            "{cols}x{rows}: the map's picture shows over {} cells, not the drawing's {}:\n{}",
            shown.len(),
            wanted.len(),
            at.pty.screen().visible.join("\n")
        );
        at.pty.check_pictures()?;
    }
    Ok(())
}

/// The hint under the Behaviors map while nothing on it is pointed at.
const MAP_HINT: &str = "Point at anything on the map to see what it is.";

/// The label map's drawing at `cols` × `rows`, whose steps point at its home's hurricane.
fn label_map(manifest: &Manifest, cols: u16, rows: u16) -> anyhow::Result<&Entry> {
    let file = format!("behaviors/label-map@{cols}x{rows}.txt");
    manifest
        .drawings
        .iter()
        .find(|entry| entry.file == file)
        .with_context(|| format!("no {file}"))
}

/// The line under the map, from the row that starts it to the row before the map's summary:
/// each row's words up to the first wide gap, where the list beside the line begins.
fn line_under_the_map(screen: &str, starts: &str) -> Option<String> {
    let rows: Vec<&str> = screen.lines().collect();
    let first = rows.iter().position(|row| row.contains(starts))?;
    let words = rows[first..]
        .iter()
        .take_while(|row| !row.contains("Label map ·"))
        .map(|row| row.trim_start().split("   ").next().unwrap_or_default())
        .filter(|words| !words.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(words)
}

/// Moves the pointer off the map and to `at` until the screen matches `said`: the map takes a
/// move only once its world is ready, which a debug build takes a while to make, and a person
/// who sees nothing happen moves the mouse again.
fn point_until(pty: &mut Pty, at: [u16; 2], said: &str) -> anyhow::Result<()> {
    let until = deadline::after(WAIT);
    loop {
        reach(pty, &[Step::Point([0, 0]), Step::Point(at)])?;
        match pty.wait_for_visible(said, Duration::from_secs(1)) {
            Ok(_) => return Ok(()),
            Err(err) if Instant::now() >= until => return Err(err),
            Err(_) => {}
        }
    }
}

/// Pointed at, the label map's hurricane says what it is, whole, at 120 × 38 and 177 × 40, and
/// once the pointer moves off the map the line goes and the hint comes back.
#[test]
fn the_hurricane_line_leaves_when_the_pointer_leaves_the_map() -> anyhow::Result<()> {
    const HURRICANE: &str = "Hurricane · 27 ‘tried an edit that didn't apply’ actions within one hour, 8 days ago at 14:01";
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    for (cols, rows) in [(120, 38), (177, 40)] {
        let mut entry = label_map(&manifest, cols, rows)?.clone();
        let point = entry
            .steps
            .iter()
            .position(|step| matches!(step, Step::Point(_)))
            .context("the drawing's steps point at the hurricane")?;
        let to_the_hurricane = entry.steps.split_off(point);
        let [Step::Point(hurricane), Step::Wait(said)] = to_the_hurricane.as_slice() else {
            anyhow::bail!("{}: the steps end on a point and a wait", entry.file);
        };
        let drawing = Drawing::load(&dir.join(&entry.file))?;
        let mut at = bring(&entry, &dir, &drawing)?;
        point_until(&mut at.pty, *hurricane, said)?;
        let screen = at.pty.screen().visible_text();
        let line = line_under_the_map(&screen, "Hurricane ·");
        assert_eq!(
            line.as_deref(),
            Some(HURRICANE),
            "{cols}x{rows}: the hurricane's line isn't whole:\n{screen}"
        );
        // The sub-tabs' row is above the map, so the pointer there is off it.
        let (tabs, _) = found(&at.pty.cells(), SUB_TABS).context("the sub-tabs")?;
        reach(&mut at.pty, &[Step::Point([2, tabs])])?;
        let screen = at.pty.wait_until("the map's hint back", WAIT, |screen| {
            let text = screen.visible_text();
            text.contains(MAP_HINT) && !text.contains("Hurricane")
        })?;
        assert!(
            !screen.visible_text().contains('…'),
            "{cols}x{rows}:\n{}",
            screen.visible_text()
        );
    }
    Ok(())
}

/// With the keys gone down into the label map and the mouse never moved, the pointer rests on
/// the map's busiest place, and the line under the map says which action is there instead of
/// the mouse's hint.
#[test]
fn going_into_the_map_names_what_its_busiest_place_holds() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    for (cols, rows) in [(120, 38), (177, 40)] {
        let mut entry = label_map(&manifest, cols, rows)?.clone();
        let summary = entry
            .steps
            .iter()
            .position(|step| matches!(step, Step::Wait(text) if text.starts_with("Label map")))
            .context("the drawing's steps wait for the map")?;
        entry.steps.truncate(summary + 1);
        entry.steps.extend([
            Step::Wait(MAP_HINT.replace('.', "\\.")),
            Step::Press("Down".to_owned()),
            // The home sets vim keys, which the key line names, and the arrows work as well.
            Step::Wait("(↓|j)  into the map".to_owned()),
            Step::Press("Down".to_owned()),
        ]);
        let drawing = Drawing::load(&dir.join(&entry.file))?;
        let at = bring(&entry, &dir, &drawing)?;
        // An action's line: its behavior, its model, its count of that model's actions and of
        // every model's, and when. A long one goes on in the row under it, which takes the
        // place of the dot between the two.
        let an_action = |screen: &str| {
            line_under_the_map(screen, "◌ ").is_some_and(|line| {
                [" of its ", " actions", " by every model · "]
                    .iter()
                    .all(|words| line.contains(words))
            })
        };
        let screen = at
            .pty
            .wait_until("an action's line under the map", WAIT, |screen| {
                an_action(&screen.visible_text())
            })?
            .visible_text();
        assert!(
            !screen.contains(MAP_HINT),
            "{cols}x{rows}: the hint stays with the keys in the map:\n{screen}"
        );
        let line = line_under_the_map(&screen, "◌ ");
        assert!(
            line.as_deref().is_some_and(|line| !line.contains('…')),
            "{cols}x{rows}: the action's line is cut: {line:?}\n{screen}"
        );
    }
    Ok(())
}

/// The universe keeps moving with a home whose newest action is three weeks old, so no star
/// breathes and nothing flies: the pictures over its galaxies, well away from your planet in
/// its middle, change as each galaxy's glow breathes and its stars twinkle.
#[test]
fn a_quiet_universe_keeps_moving() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let mut entry = manifest
        .drawings
        .iter()
        .find(|entry| entry.file == "behaviors/label-list-down-1@120x38.txt")
        .context("a drawing of the label map's list")?
        .clone();
    // The home's newest action is on 2026-10-19.
    entry.clock = Some("2026-11-09T08:40:00-05:00".to_string());
    let tabs = entry
        .steps
        .iter()
        .position(|step| matches!(step, Step::Wait(text) if text.contains("3 Universe")))
        .context("the drawing's steps reach the sub-tabs")?;
    entry.steps.truncate(tabs + 1);
    entry.steps.push(Step::Press("3".to_string()));
    for (cols, rows) in [(120, 38), (177, 40)] {
        let mut drawing = Drawing::load(&dir.join(&entry.file))?;
        (drawing.cols, drawing.rows) = (cols, rows);
        let at = bring(&entry, &dir, &drawing)?;
        at.pty
            .wait_for_visible("Point at a galaxy, a star or a point", WAIT)?;
        at.pty.wait_for_pictures(WAIT)?;
        let pictures = || -> anyhow::Result<BTreeMap<(u16, u16), u32>> {
            let grid = at.pty.cells();
            let cells = map_cells(&grid, |cell| cell.picture.is_some_and(|p| p.shows))?;
            let (tabs, _) = found(&grid, SUB_TABS).context("the sub-tabs")?;
            Ok(cells
                .into_iter()
                .filter_map(|(row, col)| {
                    let picture = grid.cell(row + tabs, col).picture?;
                    Some(((row, col), picture.id))
                })
                .collect())
        };
        let look = || -> anyhow::Result<BTreeMap<(u16, u16), u32>> {
            std::thread::sleep(Duration::from_millis(300));
            pictures()
        };
        // The zoom out to the universe changes every picture in each frame and moves where they
        // show; once it ends, two looks find pictures over the same cells, most of them as they
        // were.
        let until = deadline::after(WAIT);
        let mut before = pictures()?;
        loop {
            let now = look()?;
            let (changed, of) = changed_of(&before, &now, |_| true);
            let settled = now.keys().eq(before.keys()) && 2 * changed <= of;
            before = now;
            if settled {
                break;
            }
            anyhow::ensure!(
                Instant::now() < until,
                "{cols}x{rows}: the universe never stopped zooming:\n{}",
                at.pty.screen().visible.join("\n")
            );
        }
        let middle = {
            let cols: Vec<u16> = before.keys().map(|(_, col)| *col).collect();
            let (left, right) = (cols.iter().min(), cols.iter().max());
            left.zip(right)
                .map_or(0, |(left, right)| (left + right) / 2)
        };
        let away = |cell: &(u16, u16)| cell.1.abs_diff(middle) > 12;
        // A glow's step or a twinkle changes the pictures over a galaxy or a star, some of the
        // cells away from the planet but never most of them, as a zoom or a redraw would.
        let until = deadline::after(WAIT);
        let mut looks = Vec::new();
        let moved = |looks: &[(usize, usize)]| {
            looks
                .iter()
                .filter(|(changed, of)| *changed > 0 && 2 * changed <= *of)
                .count()
        };
        while moved(&looks) < 3 && Instant::now() < until {
            let now = look()?;
            looks.push(changed_of(&before, &now, away));
            before = now;
        }
        assert!(
            moved(&looks) >= 3,
            "{cols}x{rows}: the pictures over the galaxies changed {} times; each look's changed \
             cells of those away from the planet: {looks:?}\n{}",
            moved(&looks),
            at.pty.screen().visible.join("\n")
        );
        at.pty.check_pictures()?;
    }
    Ok(())
}

/// How many of the cells `which` keeps of `now` show another picture than `before` had there,
/// and of how many.
fn changed_of(
    before: &BTreeMap<(u16, u16), u32>,
    now: &BTreeMap<(u16, u16), u32>,
    which: impl Fn(&(u16, u16)) -> bool,
) -> (usize, usize) {
    let kept: Vec<_> = now.iter().filter(|(cell, _)| which(cell)).collect();
    let changed = kept
        .iter()
        .filter(|(cell, id)| before.get(cell) != Some(id))
        .count();
    (changed, kept.len())
}
