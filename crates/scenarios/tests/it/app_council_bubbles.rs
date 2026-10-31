//! The talk's bubbles on the real app: pictures under each message's words, placed by the app
//! and kept in the terminal until it takes them off. With `ARGONAUTS_BIN` pointing at a release
//! build, that build is held to them.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use argonauts_scenarios::WAIT;
use argonauts_testkit::Pty;
use argonauts_testkit::approved::{self, Drawing, Entry, Manifest, Step};
use argonauts_testkit::pty::{Key, deadline};
use argonauts_testkit::screen::Grid;

use crate::drawings_support::{bring, reach, still};

/// The ids the talk's bubble pictures go by, apart from the logos', the mascot's and the map's.
const BUBBLES: std::ops::Range<u32> = 0x4342_0000..0x4343_0000;

/// Words from the first row of each message the talk's fakes write after your request: every
/// seat's first answer, then the note keeper's.
const MESSAGES: [&str; 5] = [
    "A token bucket per API key, in Redis: we run",
    "A sliding window counter per key",
    "A token bucket per key in Redis, checked",
    "An in-memory bucket on each node",
    KEEPER,
];
const KEEPER: &str = "Two points, and every answer";

/// The cells of `row` that show a bubble's picture.
fn bubble_cells(grid: &Grid, row: u16) -> usize {
    (0..grid.cols())
        .filter(|&col| {
            grid.cell(row, col)
                .picture
                .as_ref()
                .is_some_and(|picture| picture.shows && BUBBLES.contains(&picture.id))
        })
        .count()
}

fn bubbles_shown(grid: &Grid) -> usize {
    (0..grid.rows()).map(|row| bubble_cells(grid, row)).sum()
}

/// The row of `grid` that shows `words`.
fn row_with(grid: &Grid, words: &str) -> Option<u16> {
    (0..grid.rows()).find(|&row| grid.text(row, 0, grid.cols()).contains(words))
}

/// How a message's words and its bubble showed, as often as the screen can be read.
#[derive(Debug, Default, Clone, Copy)]
struct Seen {
    words: Option<Instant>,
    bubble: Option<Instant>,
    /// A look found the words with no bubble under them.
    bare: bool,
}

/// Watches the talk until the note keeper's message and every message seen before it show in
/// their bubbles, or the wait runs out. A message that came and went before the first look
/// counts as unseen.
fn watch_messages(pty: &Pty) -> [Seen; MESSAGES.len()] {
    let deadline = deadline::after(WAIT);
    let mut seen = [Seen::default(); MESSAGES.len()];
    while Instant::now() < deadline {
        let grid = pty.cells();
        let now = Instant::now();
        for (words, seen) in MESSAGES.iter().zip(&mut seen) {
            let Some(row) = row_with(&grid, words) else {
                continue;
            };
            seen.words.get_or_insert(now);
            if bubble_cells(&grid, row) > 0 {
                seen.bubble.get_or_insert(now);
            } else {
                seen.bare = true;
            }
        }
        let framed = |seen: &Seen| seen.words.is_none() || seen.bubble.is_some();
        if seen[MESSAGES.len() - 1].bubble.is_some() && seen.iter().all(framed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    seen
}

/// Each message's time from its words to its bubble, for the log.
fn gaps(seen: &[Seen]) -> String {
    MESSAGES
        .iter()
        .zip(seen)
        .map(|(words, seen)| match (seen.words, seen.bubble) {
            (None, _) => format!("{words:?} unseen"),
            (Some(_), None) => format!("{words:?} never in its bubble"),
            (Some(at), Some(framed)) => format!("{words:?} {:?}", framed.duration_since(at)),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The talk drawing's steps up to your request, so the seats' messages land while the test
/// looks, on the full screen or the inline one, and the step that comes after.
fn talk_until_sent(inline: bool, home: &Path) -> anyhow::Result<(Entry, Vec<Step>, Drawing)> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let talk = manifest
        .drawings
        .iter()
        .find(|entry| entry.file == "council/talk@120x38.txt")
        .context("the talk drawing")?;
    let approved = Drawing::load(&dir.join(&talk.file))?;
    let mut sent = talk.clone();
    let rest = sent.steps.split_off(sent.steps.len() - 1);
    if inline {
        let own = dir.join(sent.home.as_deref().context("the talk's home")?);
        let config = fs::read_to_string(own.join("argonauts/config.toml"))?;
        fs::create_dir_all(home.join("argonauts"))?;
        fs::write(
            home.join("argonauts/config.toml"),
            format!("{config}\n[ui]\nscreen = \"inline\"\n"),
        )?;
        sent.home = Some(home.to_string_lossy().into_owned());
    }
    Ok((sent, rest, approved))
}

/// A message shows in its bubble from the first frame its words show in, on the full screen
/// and the inline one. The bubbles are pictures under the words, which stay in the terminal
/// until the app takes them off: opening a seat's own session from the talk takes every one
/// off, and going back puts each where its rows show again.
#[test]
fn bubbles_leave_with_the_talk_and_come_back_with_it() -> anyhow::Result<()> {
    let dir = approved::dir();
    for inline in [false, true] {
        let home = tempfile::tempdir().context("making a home for the inline screen")?;
        let (sent, rest, approved) = talk_until_sent(inline, home.path())?;
        let screen = if inline { "inline" } else { "full" };
        for (cols, rows) in [(120, 38), (177, 40)] {
            let mut drawing = approved.clone();
            (drawing.cols, drawing.rows) = (cols, rows);
            let mut at = bring(&sent, &dir, &drawing)?;
            let seen = watch_messages(&at.pty);
            let gaps = gaps(&seen);
            eprintln!("words to their bubble, {screen} screen at {cols}x{rows}: {gaps}");
            let shown = |seen: &Seen| seen.words.is_some();
            let framed = |seen: &Seen| seen.words.is_none() || seen.bubble.is_some();
            assert!(
                seen.iter().any(shown)
                    && seen[MESSAGES.len() - 1].words.is_some()
                    && seen.iter().all(framed)
                    && !seen.iter().any(|seen| seen.bare),
                "a message showed without its bubble on the {screen} screen at {cols}x{rows}: \
                 {gaps}\n{}",
                at.pty.screen().visible_text()
            );
            reach(&mut at.pty, &rest)?;
            let pty = &mut at.pty;
            pty.wait_until("the talk's bubbles", WAIT, |_| {
                bubbles_shown(&pty.cells()) > 0
            })?;

            pty.send_at_once("\u{1b}[1;2A")?;
            pty.wait_for_visible("to open", WAIT)?;
            pty.press(Key::Enter)?;
            let private = pty.wait_for_visible("esc to go back to the talk", WAIT)?;
            still(pty)?;
            assert_eq!(
                bubbles_shown(&pty.cells()),
                0,
                "a bubble shows over the seat's session on the {screen} screen at \
                 {cols}x{rows}:\n{}",
                private.visible_text()
            );
            pty.check_pictures()?;

            pty.press(Key::Esc)?;
            pty.wait_for_visible("Write to the council", WAIT)?;
            pty.wait_until("the bubbles back", WAIT, |_| {
                bubbles_shown(&pty.cells()) > 0
            })?;
            pty.check_pictures()?;
        }
    }
    Ok(())
}
