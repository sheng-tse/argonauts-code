//! The approved drawings: how a screen must look, cell for cell, at the terminal size its file
//! names. They live in `approved/` at the workspace root, exported from the approved pictures,
//! and `approved/MANIFEST.toml` lists each with the release it was approved for, the number of
//! its approval and its hash.
//!
//! A drawing is a text file named `<panel>/<state>@<cols>x<rows>.txt`:
//!
//! ```text
//! Limits: a box that names Codex beside its logo
//! screen 120x38
//! at ?,0
//! size 14x3
//! terminal fg=#c8d3f5 bg=#222436
//! text
//! ╭─ Limits ───╮
//! │ ￼￼ Codex   │
//! ╰────────────╯
//! paint
//! aa.bbbbbb.aaaa
//! a.LL.bbbbb...a
//! aaaaaaaaaaaaaa
//! key
//! . blank
//! a fg=#4f5468  ; ModBorder
//! b fg=#ffffff bold  ; Bright
//! L logo=codex
//! ```
//!
//! Its first line says what it shows. Then, before `text` and in any order:
//! - `screen <cols>x<rows>`, the terminal it is drawn for, as its file name says;
//! - `at <row>,<col>`, where its first cell sits on that screen, counted from 0, either `?`
//!   where the drawing doesn't say: the comparison then finds the one place the screen shows
//!   its text ([`Drawing::place`]);
//! - `size <cols>x<rows>`, its own cells;
//! - `terminal fg=#rrggbb bg=#rrggbb`, the terminal's own colours, which every unstyled cell
//!   is painted in.
//!
//! Its rows of text follow, one character a cell, so each must be one cell wide; a cell a
//! logo or a picture covers holds U+FFFC. Then `paint` and its rows, one key a cell, and `key`
//! and what each key paints. A row of either kind may stop early: the cells after it are
//! blanks on the terminal's background, the key `.`. A key paints `fg=#rrggbb`, with
//! `bg=#rrggbb`, `bold` and `italic` where given, for a character; `bg=#rrggbb` alone for a
//! blank on that background; `logo=<name>` for a logo's two cells, such as `logo=codex`, or
//! `logo=codex-2` for its picture numbered 2; or `picture=<name>` for any other picture's
//! cells. `.` is always `. blank`, on the terminal's own background, and `?` is always
//! `? the character alone`, a cell whose character alone is compared. After a `;` a key line
//! gives the theme's names for its colours.
//!
//! Every colour is compared as the terminal paints it ([`Palette`]), on the drawing's
//! terminal, so a cell painted whole in one colour, a blank on it or `█` in it, matches either.
//! The drawings don't draw underlines, so they aren't compared. A logo's or a picture's cells
//! match only where the screen holds the picture, as a terminal keeps one store of pictures
//! for each screen. The header's `Argonauts Code v<version>` reads as the binary's own
//! version.
//!
//! A screen may draw otherwise than its drawing only where a `[[departure]]` row of the
//! manifest names the drawing's cells, what the screen draws there instead and the approval
//! that allows it.

mod compare;
mod drawing;
mod drawn;
mod manifest;
mod paint;
mod place;
mod version;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub use compare::{Aspect, Difference, differences, differences_at, report};
pub use drawing::{At, COVERED, Drawing, Named, Paint, Region, drawing_text};
pub use drawn::Drawn;
pub use manifest::{
    DRAWN_AT, DRAWN_CELL, DRAWN_FOLDER, Departure, Entry, MANIFEST, Manifest, Step, hash_of,
    key_named, reach,
};
pub use paint::{Palette, Terminal};
pub use place::{Placement, same_mark};

pub use crate::screen::{Colour, Rgb};

use crate::home::bins::workspace_root;
use crate::pty::SHOTS_ENV;

/// `approved/` at the workspace root.
pub fn dir() -> PathBuf {
    workspace_root().join("approved")
}

/// Compares `drawn` with the drawing `file`, a path under `dir`, where it sits on the screen,
/// as `palette` paints both on the drawing's terminal, but for the cells the manifest's
/// departures for it name. The error reports every difference, or why the drawing couldn't be
/// read.
pub fn check(
    manifest: &Manifest,
    dir: &Path,
    file: &str,
    drawn: &Drawn,
    palette: &Palette,
) -> Result<(), String> {
    let drawing = Drawing::load(&dir.join(file)).map_err(|err| format!("{file}: {err:#}"))?;
    if (drawing.cols, drawing.rows) != (drawn.cols(), drawn.rows()) {
        return Err(format!(
            "{file}: drawn for {} × {}, but the screen is {} × {}",
            drawing.cols,
            drawing.rows,
            drawn.cols(),
            drawn.rows()
        ));
    }
    let placement = drawing.place(drawn);
    let found = differences_at(&drawing, &placement, drawn, palette, &|row, col| {
        manifest.excuses(file, row, col)
    });
    if found.is_empty() {
        return Ok(());
    }
    let mut text = report(file, &drawing, drawn, &placement, &found);
    let palette = palette.on(drawing.terminal);
    match saved_screen(file, &drawing, placement.region(), drawn, &palette) {
        Ok(Some(path)) => {
            let _ = write!(text, "\nThe screen, as a drawing: {}", path.display());
        }
        Ok(None) => {
            let _ = write!(
                text,
                "\nSet {SHOTS_ENV} to save the screen as a drawing beside the others."
            );
        }
        Err(err) => {
            let _ = write!(text, "\nSaving the screen as a drawing failed: {err:#}");
        }
    }
    Err(text)
}

/// [`check`] against `approved/` as Ghostty paints it with TokyoNight Moon, panicking with the
/// report, for a test that draws a screen and holds it to its drawing.
pub fn assert_matches(file: &str, drawn: &Drawn) {
    let dir = dir();
    let manifest = match Manifest::load(&dir) {
        Ok(manifest) => manifest,
        Err(err) => panic!("{err:#}"),
    };
    if let Err(report) = check(&manifest, &dir, file, drawn, &Palette::TOKYONIGHT_MOON) {
        panic!("{report}");
    }
}

/// Writes `region` of the screen, where the drawing sits or comes closest, as a drawing, its
/// colours named as the drawing names them, under `$ARGONAUTS_SHOTS_DIR/approved/`, so a
/// person can set the two side by side.
fn saved_screen(
    file: &str,
    drawing: &Drawing,
    region: Region,
    drawn: &Drawn,
    palette: &Palette,
) -> anyhow::Result<Option<PathBuf>> {
    use anyhow::Context as _;

    let Some(shots) = std::env::var_os(SHOTS_ENV).filter(|dir| !dir.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(&shots).join("approved").join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = drawing_text(drawn, region, palette, &drawing.names())?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests;
