//! The approved drawings on the real app: each drawing's steps bring the binary, in a terminal
//! of the drawing's size that plays Ghostty, to the drawn screen, which is compared with the
//! drawing cell for cell. A drawing whose figures the fake CLIs can't give is only reached
//! here, and the test the manifest names for it holds its cells. With `ARGONAUTS_BIN` pointing
//! at a release build, that build is held to them; `ARGONAUTS_DRAWINGS` keeps the drawings
//! whose path holds its text.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::look::logos::LOGO_CELL_PIXELS;
use argonauts_scenarios::{Scenario, WAIT, no_servers_of_yours, past_the_question_guard};
use argonauts_testkit::approved::{
    self, Aspect, Colour, DRAWN_FOLDER, Drawing, Drawn, Entry, Manifest, Palette, Region, Rgb,
    Step, Terminal,
};
use argonauts_testkit::screen::REAL_SIZES;
use argonauts_testkit::{Pty, PtyCommand, argonauts_bin};

use crate::app_support::AGENTS;

/// Variables from the developer's shell that would change how the app paints.
const PAINT_ENV: [&str; 4] = ["NO_COLOR", "FORCE_COLOR", "CLICOLOR_FORCE", "LC_ALL"];

/// Moves the scenario's repo to `folder` under its `HOME`, so the header names the project
/// `~/<folder>` on every run, and returns where it now is.
fn place_project(scenario: &Scenario, folder: &str) -> anyhow::Result<PathBuf> {
    let root = scenario.repo.root();
    let project = scenario.repo.user_home().join(folder);
    if let Some(parent) = project.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::rename(root, &project)
        .with_context(|| format!("moving {} to {}", root.display(), project.display()))?;
    // The scenario stops the run hosts working in the repo by finding them through its old path.
    std::os::unix::fs::symlink(&project, root)
        .with_context(|| format!("linking {} to {}", root.display(), project.display()))?;
    Ok(project)
}

/// The app on its default settings, working in `project`, in a terminal `cols` × `rows` that
/// plays Ghostty: true colour, the kitty protocol's pictures, and `terminal`'s own colours,
/// which it tells the app when asked, so the app picks its light or dark theme from them.
fn launch(
    scenario: &Scenario,
    project: &Path,
    (cols, rows): (u16, u16),
    terminal: Terminal,
) -> anyhow::Result<Pty> {
    let binary = argonauts_bin().context("finding the argonauts binary")?;
    let mut command = PtyCommand::new(binary)
        .args(no_servers_of_yours(&[]))
        .cwd(project)
        .envs(scenario.envs())
        .env_remove("ARGONAUTS_SESSION")
        .env("TERM_PROGRAM", "ghostty")
        .env("COLORTERM", "truecolor")
        .takes_pictures()
        .cell_pixels(LOGO_CELL_PIXELS.0, LOGO_CELL_PIXELS.1)
        .own_colours(terminal.text, terminal.background)
        .size(rows, cols);
    for name in PAINT_ENV {
        command = command.env_remove(name);
    }
    Pty::spawn(command)
}

/// Waits until two looks at the screen agree, so the frame read next is whole.
fn still(pty: &Pty) -> anyhow::Result<()> {
    pty.wait_until("a still screen", WAIT, |_| true)?;
    Ok(())
}

/// The fake CLIs for `entry`: the rules its `clis` file holds, first, then the default agents'
/// rules, which play every other request. The rules are written into `scratch`, where the fakes
/// read them while the scenario runs, beside the files their `emit_file` steps name, which sit
/// beside the `clis` file.
fn scenario_of(entry: &Entry, dir: &Path, scratch: &Path) -> anyhow::Result<Scenario> {
    let Some(clis) = &entry.clis else {
        return Scenario::new(AGENTS);
    };
    let path = dir.join(clis);
    let rules = fs::read_to_string(&path).with_context(|| format!("reading {clis}"))?;
    for name in emitted_files(&rules) {
        let from = path.with_file_name(name);
        fs::copy(&from, scratch.join(name))
            .with_context(|| format!("copying {}", from.display()))?;
    }
    let agents = fs::read_to_string(argonauts_scenarios::scenario_file(AGENTS))
        .with_context(|| format!("reading {AGENTS}"))?;
    let first_rule = agents
        .find("\n[[rule]]")
        .with_context(|| format!("{AGENTS} has no rules"))?;
    let (head, defaults) = agents.split_at(first_rule);
    let file = scratch.join("agents.toml");
    fs::write(&file, format!("{head}\n{rules}{defaults}"))
        .with_context(|| format!("writing {}", file.display()))?;
    Scenario::from_file(&file)
}

/// The files `rules` has the fakes print whole, one `emit_file = '…'` a line.
fn emitted_files(rules: &str) -> impl Iterator<Item = &str> {
    rules.lines().filter_map(|line| {
        let name = line.trim().strip_prefix("emit_file")?.trim_start();
        let name = name.strip_prefix('=')?.trim();
        name.strip_prefix('\'')?.strip_suffix('\'')
    })
}

/// The app at the screen `entry`'s steps bring it to, with the scenario and the fakes' rules it
/// runs on, which last as long as it does.
struct AtScreen {
    pty: Pty,
    _scenario: Scenario,
    _rules: tempfile::TempDir,
}

/// The screen `entry`'s steps bring the app to, from the home and fake CLIs it names, on the
/// terminal `drawing` was made on.
fn screen_of(entry: &Entry, dir: &Path, drawing: &Drawing) -> anyhow::Result<Drawn> {
    let at = bring(entry, dir, drawing)?;
    at.pty.wait_for_pictures(WAIT)?;
    Drawn::capture(&at.pty, drawing.cols, drawing.rows)
}

/// Brings the app to `entry`'s screen on the terminal `drawing` was made on, and waits for it
/// to hold still.
fn bring(entry: &Entry, dir: &Path, drawing: &Drawing) -> anyhow::Result<AtScreen> {
    let scratch = tempfile::tempdir().context("making a folder for the fakes' rules")?;
    let scenario = scenario_of(entry, dir, scratch.path())?;
    let project = place_project(&scenario, entry.folder())?;
    if let Some(home) = &entry.home {
        let home = dir.join(home);
        for (part, into) in [
            ("argonauts", scenario.repo.home()),
            ("home", scenario.repo.user_home()),
        ] {
            let from = home.join(part);
            if from.is_dir() {
                copy_folder(&from, into)?;
            }
        }
    }
    let size = (drawing.cols, drawing.rows);
    let mut pty = launch(&scenario, &project, size, drawing.terminal)?;
    reach(&mut pty, &entry.steps)?;
    still(&pty)?;
    Ok(AtScreen {
        pty,
        _scenario: scenario,
        _rules: scratch,
    })
}

/// Runs `steps` as someone reading the screen would: after each wait, past the moment a
/// question that just showed takes only the arrows, so the key that answers it counts.
fn reach(pty: &mut Pty, steps: &[Step]) -> anyhow::Result<()> {
    for step in steps {
        approved::reach(pty, std::slice::from_ref(step), WAIT)?;
        if matches!(step, Step::Wait(_)) {
            past_the_question_guard();
        }
    }
    Ok(())
}

fn copy_folder(from: &Path, into: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(into).with_context(|| format!("creating {}", into.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = into.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Variable naming the part of a drawing's path that picks the drawings to reach, so one panel
/// can be checked on its own.
const ONLY_ENV: &str = "ARGONAUTS_DRAWINGS";

/// What became of one drawing on the real app.
enum Outcome {
    Matched,
    /// Reached; the named test holds its cells.
    Reached(String),
    Failed(String),
}

fn outcome(entry: &Entry, manifest: &Manifest, dir: &Path) -> Outcome {
    let file = &entry.file;
    if entry.steps.is_empty() {
        return match &entry.held_by {
            Some(test) => Outcome::Reached(test.clone()),
            None => Outcome::Failed(format!(
                "{file}: has no steps and names no test that holds it"
            )),
        };
    }
    let drawing = match Drawing::load(&dir.join(file)) {
        Ok(drawing) => drawing,
        Err(err) => return Outcome::Failed(format!("{file}: {err:#}")),
    };
    // A held screen is only reached, so one that keeps moving, as the Council's typing bubbles
    // do, needn't be read whole.
    if let Some(test) = &entry.held_by {
        return match bring(entry, dir, &drawing) {
            Ok(_) => Outcome::Reached(test.clone()),
            Err(err) => Outcome::Failed(format!("{file}: {err:#}")),
        };
    }
    let drawn = match screen_of(entry, dir, &drawing) {
        Ok(drawn) => drawn,
        Err(err) => return Outcome::Failed(format!("{file}: {err:#}")),
    };
    let palette = Palette::for_terminal(drawing.terminal);
    match approved::check(manifest, dir, file, &drawn, &palette) {
        Ok(()) => Outcome::Matched,
        Err(report) => Outcome::Failed(report),
    }
}

#[test]
#[ignore = "OpenCode's very first run and count on the Context tab are reached but unlike \
            the app until the changes and tests that hold them land; the bottom line's logos \
            wait on their drawing"]
fn every_drawing_matches_the_app_or_a_test_that_draws_it() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let only = std::env::var(ONLY_ENV).unwrap_or_default();
    let mut matched = 0;
    let mut held = Vec::new();
    let mut failed = Vec::new();
    for entry in manifest
        .drawings
        .iter()
        .filter(|entry| entry.file.contains(&only))
    {
        match outcome(entry, &manifest, &dir) {
            Outcome::Matched => matched += 1,
            Outcome::Reached(test) => held.push(format!("{} by {test}", entry.file)),
            Outcome::Failed(report) => failed.push(report),
        }
    }
    eprintln!(
        "{matched} drawings match the app; {} are reached and held by the tests that draw them:\n{}",
        held.len(),
        held.join("\n")
    );
    assert!(
        failed.is_empty(),
        "{} drawings are unlike the app or weren't reached:\n\n{}",
        failed.len(),
        failed.join("\n\n")
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
        let pty = launch(&scenario, &project, (cols, rows), palette.terminal())?;
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

/// Where `word` is first written on the screen, a letter a cell.
fn found(drawn: &Drawn, word: &str) -> Option<(u16, u16)> {
    let letters: Vec<String> = word.chars().map(String::from).collect();
    let width = u16::try_from(letters.len()).ok()?;
    let cols = drawn.cols().checked_sub(width)?;
    (0..drawn.rows())
        .flat_map(|row| (0..=cols).map(move |col| (row, col)))
        .find(|&(row, col)| {
            (col..col + width)
                .zip(&letters)
                .all(|(at, letter)| drawn.cell(row, at).text == *letter)
        })
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
    let pty = launch(&scenario, &project, (cols, rows), white)?;
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
