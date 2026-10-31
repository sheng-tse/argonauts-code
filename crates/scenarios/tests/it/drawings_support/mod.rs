//! Shared steps for the tests that hold the real app to the approved drawings: the app started
//! as the drawings were made, in a terminal of their size that plays Ghostty, and brought to a
//! drawing's screen by the steps its manifest row lists.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::base::clock::CLOCK_ENV;
use argonauts_core::base::paths::HOME_ENV;
use argonauts_scenarios::{Scenario, WAIT, no_servers_of_yours, past_the_question_guard};
use argonauts_testkit::approved::{self, DRAWN_CELL, Drawing, Drawn, Entry, Step, Terminal};
use argonauts_testkit::{Pty, PtyCommand, argonauts_bin};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::app_support::AGENTS;

/// Variables from the developer's shell that would change how the app paints.
const PAINT_ENV: [&str; 4] = ["NO_COLOR", "FORCE_COLOR", "CLICOLOR_FORCE", "LC_ALL"];

/// Moves the scenario's repo to `folder` under its `HOME`, so the header names the project
/// `~/<folder>` on every run, and returns where it now is.
pub(crate) fn place_project(scenario: &Scenario, folder: &str) -> anyhow::Result<PathBuf> {
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

/// `~/.argonauts`, where the app keeps its state on a real machine and names it so on screen: a
/// link to the scenario's own state folder, which its run hosts and fakes find by that path.
pub(crate) fn argonauts_home_in_home(scenario: &Scenario) -> anyhow::Result<PathBuf> {
    let link = scenario.repo.user_home().join(".argonauts");
    std::os::unix::fs::symlink(scenario.repo.home(), &link)
        .with_context(|| format!("linking {}", link.display()))?;
    Ok(link)
}

/// How the terminal the app starts in plays: its size, its own colours, and whether it shows
/// pictures.
#[derive(Clone, Copy)]
pub(crate) struct Room {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) terminal: Terminal,
    pub(crate) pictures: bool,
}

/// The app on its default settings, working in `project` with its state in `own_home`, its time
/// starting at `clock`, in a terminal `cols` × `rows` with true colour and `terminal`'s own
/// colours, which it tells the app when asked, so the app picks its light or dark theme from
/// them. With `pictures` it plays Ghostty and answers the kitty protocol's pictures; without, it
/// names no terminal and leaves the graphics query unanswered, as a terminal with no pictures
/// does (the pty names none).
pub(crate) fn launch(
    scenario: &Scenario,
    project: &Path,
    own_home: &Path,
    clock: &str,
    room: Room,
) -> anyhow::Result<Pty> {
    let binary = argonauts_bin().context("finding the argonauts binary")?;
    let mut command = PtyCommand::new(binary)
        .args(no_servers_of_yours(&[]))
        .cwd(project)
        .envs(scenario.envs())
        .env(HOME_ENV, own_home)
        .env_remove("ARGONAUTS_SESSION")
        .env(CLOCK_ENV, clock)
        .env("TZ", zone_of(clock)?)
        .env("COLORTERM", "truecolor")
        .cell_pixels(DRAWN_CELL.0, DRAWN_CELL.1)
        .own_colours(room.terminal.text, room.terminal.background)
        .size(room.rows, room.cols);
    if room.pictures {
        command = command.env("TERM_PROGRAM", "ghostty").takes_pictures();
    }
    for name in PAINT_ENV {
        command = command.env_remove(name);
    }
    Pty::spawn(command)
}

/// The time zone of `instant`'s offset, as `TZ` names a zone with no name: `-04:00` is `<-04>4`,
/// since POSIX counts hours west of Greenwich.
fn zone_of(instant: &str) -> anyhow::Result<String> {
    let at = OffsetDateTime::parse(instant, &Rfc3339)
        .with_context(|| format!("{instant} isn't an RFC 3339 instant"))?;
    let (hours, minutes, _) = at.offset().as_hms();
    let east = if at.offset().is_negative() { '-' } else { '+' };
    let (hours, minutes) = (hours.unsigned_abs(), minutes.unsigned_abs());
    let west = if at.offset().is_negative() { "" } else { "-" };
    Ok(format!(
        "<{east}{hours:02}{minutes:02}>{west}{hours}:{minutes:02}"
    ))
}

/// Waits until two looks at the screen agree, so the frame read next is whole.
pub(crate) fn still(pty: &Pty) -> anyhow::Result<()> {
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
pub(crate) struct AtScreen {
    pub(crate) pty: Pty,
    pub(crate) scenario: Scenario,
    _rules: tempfile::TempDir,
}

/// Brings the app to `entry`'s screen on the terminal `drawing` was made on, and waits for it
/// to hold still.
pub(crate) fn bring(entry: &Entry, dir: &Path, drawing: &Drawing) -> anyhow::Result<AtScreen> {
    bring_with(entry, dir, drawing, |_| Ok(()))
}

/// [`bring`], with `change` making its changes to the scenario's files once `entry`'s home is
/// in place and before the app starts.
pub(crate) fn bring_with(
    entry: &Entry,
    dir: &Path,
    drawing: &Drawing,
    change: impl FnOnce(&Scenario) -> anyhow::Result<()>,
) -> anyhow::Result<AtScreen> {
    let scratch = tempfile::tempdir().context("making a folder for the fakes' rules")?;
    let scenario = scenario_of(entry, dir, scratch.path())?;
    let project = place_project(&scenario, entry.folder())?;
    if !entry.git {
        let git = project.join(".git");
        fs::remove_dir_all(&git).with_context(|| format!("removing {}", git.display()))?;
    }
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
    change(&scenario)?;
    let own_home = argonauts_home_in_home(&scenario)?;
    let room = Room {
        cols: drawing.cols,
        rows: drawing.rows,
        terminal: drawing.terminal,
        pictures: entry.pictures,
    };
    let mut pty = launch(&scenario, &project, &own_home, entry.clock(), room)?;
    reach(&mut pty, &entry.steps)?;
    still(&pty)?;
    Ok(AtScreen {
        pty,
        scenario,
        _rules: scratch,
    })
}

/// Runs `steps` as someone reading the screen would: after each wait, past the moment a
/// question that just showed takes only the arrows, so the key that answers it counts.
pub(crate) fn reach(pty: &mut Pty, steps: &[Step]) -> anyhow::Result<()> {
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

/// Where `word` is first written on the screen, a letter a cell.
pub(crate) fn found(drawn: &Drawn, word: &str) -> Option<(u16, u16)> {
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
