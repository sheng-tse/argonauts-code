//! Council on the real app, from the first key: a Council you switch to opens straight into its
//! conversation, and one started from a supervised session waits for you at its plan, at
//! 120 × 38 and 177 × 40. With `ARGONAUTS_BIN` pointing at a release build, that build is held
//! to them.

use anyhow::Context as _;
use argonauts_scenarios::WAIT;
use argonauts_testkit::approved::{self, Drawing, Manifest};

use crate::drawings_support::bring;

/// A Council you switch to opens straight into its conversation, from the first key: your
/// request is the first bubble under the heading and the seats start on their first answers,
/// with no goal to approve, no design step and no workflow or agent rows before the heading.
#[test]
fn council_opens_straight_into_its_conversation() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let entry = manifest
        .drawings
        .iter()
        .find(|entry| entry.file == "council/sent@120x38.txt")
        .context("the sent drawing")?;
    let approved = Drawing::load(&dir.join(&entry.file))?;
    for (cols, rows) in [(120, 38), (177, 40)] {
        let mut drawing = approved.clone();
        (drawing.cols, drawing.rows) = (cols, rows);
        let at = bring(entry, &dir, &drawing)?;
        // The working line counts the seats once its first second is up.
        let screen = at.pty.wait_for("seats agree", WAIT)?.text();
        let heading = screen
            .find("each one is its own session")
            .with_context(|| format!("the Council heading at {cols}x{rows}:\n{screen}"))?;
        let before = &screen[..heading];
        for word in [
            "Run this workflow",
            "Goal approved",
            "main:design",
            "Workflow(",
            "read:",
            "agents started",
        ] {
            assert!(
                !screen.contains(word),
                "{word:?} at {cols}x{rows}:\n{screen}"
            );
        }
        assert!(
            before.contains("Switched to Council"),
            "{cols}x{rows}:\n{screen}"
        );
        // Your request shows once, in its bubble under the heading, read by every seat.
        let asked = screen.find("Add rate limiting");
        let read_by = screen.find("Read by");
        assert!(
            screen.matches("Add rate limiting").count() == 1
                && asked
                    .zip(read_by)
                    .is_some_and(|(at, read)| heading < at && at < read),
            "your request at {cols}x{rows}:\n{screen}"
        );
        assert!(
            screen.contains("Rate limit the API"),
            "the run's name keeps your casing at {cols}x{rows}:\n{screen}"
        );
        assert!(
            screen.contains("0/4 seats agree") && !screen.contains("4/4"),
            "the seats' count at {cols}x{rows}:\n{screen}"
        );
    }
    Ok(())
}

/// A Council started from a supervised session asks you to approve its plan, as any run does in
/// supervised mode, and the bottom line keeps saying supervised while the run goes on.
#[test]
fn a_supervised_council_waits_for_you_at_its_plan() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let entry = manifest
        .drawings
        .iter()
        .find(|entry| entry.file == "council/plan@120x38.txt")
        .context("the plan drawing")?;
    let approved = Drawing::load(&dir.join(&entry.file))?;
    for (cols, rows) in [(120, 38), (177, 40)] {
        let mut drawing = approved.clone();
        (drawing.cols, drawing.rows) = (cols, rows);
        // The steps end on "◆ Approve this plan?", which only a plan waiting for you shows.
        let at = bring(entry, &dir, &drawing)?;
        let screen = at.pty.screen().visible_text();
        assert!(
            screen.contains("supervised mode on") && !screen.contains("auto mode on"),
            "{cols}x{rows}:\n{screen}"
        );
        assert!(
            !screen.contains("waits for you only in supervised mode"),
            "{cols}x{rows}:\n{screen}"
        );
    }
    Ok(())
}
