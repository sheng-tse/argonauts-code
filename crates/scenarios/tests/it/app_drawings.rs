//! The approved drawings on the real app: each drawing's steps bring the binary, in a terminal
//! of the drawing's size that plays Ghostty, to the drawn screen, which is compared with the
//! drawing cell for cell. With `ARGONAUTS_BIN` pointing at a release build, that build is held
//! to them; `ARGONAUTS_DRAWINGS` keeps the drawings whose path holds its text. A drawing the
//! manifest holds, saying what the app doesn't draw yet, is compared only when that variable
//! picks it.

use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::Context as _;
use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::approved::{
    self, Aspect, Colour, DRAWN_AT, DRAWN_FOLDER, Drawing, Drawn, Entry, Manifest, Palette, Region,
    Rgb, Terminal,
};
use argonauts_testkit::pty::deadline;
use argonauts_testkit::screen::REAL_SIZES;

use crate::app_support::AGENTS;
use crate::drawings_support::{
    AtScreen, Room, argonauts_home_in_home, bring, found, launch, place_project, still,
};

/// The screen the app shows now, once the pictures it placed show, on the terminal `drawing`
/// was made on.
fn screen_of(at: &AtScreen, drawing: &Drawing) -> anyhow::Result<Drawn> {
    at.pty.wait_for_pictures(WAIT)?;
    Drawn::capture(&at.pty, drawing.cols, drawing.rows)
}

/// Variable naming the part of a drawing's path that picks the drawings to reach, so one panel
/// can be checked on its own.
const ONLY_ENV: &str = "ARGONAUTS_DRAWINGS";

/// Brings the app to `entry`'s screen and compares it with the drawing; the error is the
/// report of every difference, or why the screen wasn't reached.
fn compare_with_the_app(entry: &Entry, manifest: &Manifest, dir: &Path) -> Result<(), String> {
    let file = &entry.file;
    if entry.steps.is_empty() {
        return Err(format!("{file}: has no steps"));
    }
    let failed = |err: anyhow::Error| format!("{file}: {err:#}");
    let drawing = Drawing::load(&dir.join(file)).map_err(failed)?;
    let at = bring(entry, dir, &drawing).map_err(failed)?;
    let palette = Palette::for_terminal(drawing.terminal);
    // A busy machine can show a frame the app is still drawing, or the map before its picture,
    // after two looks agreed, so a difference counts once it outlasts a wait.
    let until = deadline::after(WAIT);
    loop {
        let compared = screen_of(&at, &drawing)
            .map_err(failed)
            .and_then(|drawn| approved::check(manifest, dir, file, &drawn, &palette));
        if compared.is_ok() || Instant::now() >= until {
            return compared;
        }
        still(&at.pty).map_err(failed)?;
    }
}

#[test]
fn every_drawing_matches_the_app() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let only = std::env::var(ONLY_ENV).unwrap_or_default();
    let mut matched = 0;
    let mut held = 0;
    let mut failed = Vec::new();
    for entry in manifest
        .drawings
        .iter()
        .filter(|entry| entry.file.contains(&only))
    {
        if let Some(hold) = entry.held.as_ref().filter(|_| only.is_empty()) {
            eprintln!(
                "{}: held by approval {}: {}\n",
                entry.file, hold.approval, hold.why
            );
            held += 1;
            continue;
        }
        match compare_with_the_app(entry, &manifest, &dir) {
            Ok(()) => matched += 1,
            Err(report) => {
                // Said at once, so a run cut short by its time limit still names each one unlike the app.
                eprintln!("{report}\n");
                failed.push(entry.file.as_str());
            }
        }
    }
    anyhow::ensure!(
        matched + failed.len() > 0,
        "{ONLY_ENV}={only:?} picks no drawing"
    );
    eprintln!("{matched} drawings match the app, and {held} are held");
    assert!(
        failed.is_empty(),
        "{} drawings are unlike the app or weren't reached, each reported above:\n{}",
        failed.len(),
        failed.join("\n")
    );
    Ok(())
}

/// The app's first screen, read back at every real size: the header names the project as the
/// drawings do, written as a drawing it reads back with no difference, and a changed character
/// and a changed colour are each found in their own cell.
#[test]
fn the_first_screen_reads_back_cell_for_cell_at_every_real_size() -> anyhow::Result<()> {
    let palette = Palette::TOKYONIGHT_MOON;
    let nowhere = |_, _| false;
    for size in REAL_SIZES {
        let (cols, rows) = (size.cols, size.rows);
        let scenario = Scenario::new(AGENTS)?;
        let project = place_project(&scenario, DRAWN_FOLDER)?;
        let room = Room {
            cols,
            rows,
            terminal: palette.terminal(),
            pictures: true,
        };
        let own_home = argonauts_home_in_home(&scenario)?;
        let pty = launch(&scenario, &project, &own_home, DRAWN_AT, room)?;
        pty.wait_for_visible("Argonauts Code", WAIT)?;
        still(&pty)?;
        let drawn = Drawn::capture(&pty, cols, rows)?;
        let text = approved::drawing_text(&drawn, Region::whole(cols, rows), &palette, &[])?;
        let folder = format!("~/{DRAWN_FOLDER}");
        assert!(
            text.contains(&folder),
            "{cols} × {rows}: no {folder}:\n{text}"
        );
        let drawing = Drawing::parse(&format!("first@{cols}x{rows}.txt"), &text)?;
        let found = approved::differences(&drawing, &drawn, &palette, &nowhere);
        assert_eq!(found, [], "{cols} × {rows}:\n{text}");

        let letters: Vec<(u16, u16)> = (0..rows)
            .flat_map(|row| (0..cols).map(move |col| (row, col)))
            .filter(|&(row, col)| {
                let text = &drawn.cell(row, col).text;
                text.len() == 1 && text.chars().all(|c| c.is_ascii_alphanumeric())
            })
            .collect();
        let (Some(&first), Some(&last)) = (letters.first(), letters.last()) else {
            panic!("{cols} × {rows}: no letters on the first screen:\n{text}");
        };
        let mut changed = drawn.clone();
        let cell = changed.cell_mut(first.0, first.1);
        cell.text = if cell.text == "x" { "y" } else { "x" }.to_owned();
        changed.cell_mut(last.0, last.1).ink = Colour::Rgb(Rgb(1, 2, 3));
        let found: Vec<(u16, u16, Aspect)> =
            approved::differences(&drawing, &changed, &palette, &nowhere)
                .into_iter()
                .map(|difference| (difference.row, difference.col, difference.aspect))
                .collect();
        assert_eq!(
            found,
            [
                (first.0, first.1, Aspect::Text),
                (last.0, last.1, Aspect::TextColour)
            ],
            "{cols} × {rows}"
        );
    }
    Ok(())
}

/// On a terminal that says its background is white, as the light drawings' terminal does, the
/// real app draws its light theme: the bottom line's hint is dark on the white, where the dark
/// theme writes it in a light grey.
#[test]
fn a_white_terminal_gets_the_light_theme() -> anyhow::Result<()> {
    let white = Terminal {
        text: Rgb(0, 0, 0),
        background: Rgb(0xff, 0xff, 0xff),
    };
    let palette = Palette::for_terminal(white);
    let (cols, rows) = (120, 38);
    let scenario = Scenario::new(AGENTS)?;
    // A global settings file means the first start's setup is done, so the input has the keys
    // and the bottom line its hint, instead of the setup's first question.
    let config = scenario.repo.global_config();
    fs::write(&config, "").with_context(|| format!("writing {}", config.display()))?;
    let project = place_project(&scenario, DRAWN_FOLDER)?;
    let room = Room {
        cols,
        rows,
        terminal: white,
        pictures: true,
    };
    let own_home = argonauts_home_in_home(&scenario)?;
    let pty = launch(&scenario, &project, &own_home, DRAWN_AT, room)?;
    pty.wait_for_visible("shift\\+tab to cycle", WAIT)?;
    still(&pty)?;
    let drawn = Drawn::capture(&pty, cols, rows)?;
    let Some((row, col)) = found(&drawn, "shift+tab") else {
        panic!(
            "no hint on the screen:\n{}",
            pty.screen().visible.join("\n")
        );
    };
    let Rgb(red, green, blue) = match drawn.cell(row, col).ink {
        Colour::Own => palette.text,
        Colour::Indexed(index) => palette.indexed(index),
        Colour::Rgb(rgb) => rgb,
    };
    let light = u16::from(red) + u16::from(green) + u16::from(blue) > 3 * 128;
    assert!(
        !light,
        "the hint is #{red:02x}{green:02x}{blue:02x} on white:\n{}",
        pty.screen().visible.join("\n")
    );
    Ok(())
}
