//! `approved/MANIFEST.toml`: each drawing with the release it was approved for, its approval's
//! number and its hash, how the app is brought to its screen, the test that holds its cells when
//! the real app can't draw them from the fake CLIs, the decision that approved a state at one
//! of its two sizes for now, why a drawing the app can't draw yet sits out of the comparison,
//! and the cells a screen may draw otherwise, each with the approval that allows it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use serde::Deserialize;

use super::drawing::{Drawing, size_of};
use crate::pty::{Key, Pty};
use crate::screen::{REAL_SIZES, Size};

pub const MANIFEST: &str = "MANIFEST.toml";

/// The project's folder under `HOME` that the drawings' headers name, `~/code/tasks-api`.
pub const DRAWN_FOLDER: &str = "code/tasks-api";

/// The cell, in pixels, the drawings are drawn at, which the runner's terminal reports: the
/// shape the Behaviors map sizes its picture for when a terminal doesn't say (tui's
/// `ASSUMED_CELL`).
pub const DRAWN_CELL: (u16, u16) = (17, 43);

/// The instant the drawings' screens are drawn at, in the time zone they write times in. The
/// panel tests draw at it too.
pub const DRAWN_AT: &str = "2026-10-20T08:40:00-04:00";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default, rename = "drawing")]
    pub drawings: Vec<Entry>,
    #[serde(default, rename = "departure")]
    pub departures: Vec<Departure>,
}

/// A drawing, and how the app reaches its screen.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Its path under `approved/`.
    pub file: String,
    pub release: String,
    /// The number of the approval that settled it.
    pub approval: u32,
    /// `sha256:` and the hash of the file.
    pub hash: String,
    /// A folder under `approved/` whose `argonauts/` is copied into `ARGONAUTS_HOME` and whose
    /// `home/` into `HOME` before the app starts: the settings and records the screen shows.
    #[serde(default)]
    pub home: Option<String>,
    /// Fake CLI rules under `approved/` for the requests the drawing makes, placed before the
    /// scenarios' default rules, which play every other request.
    #[serde(default)]
    pub clis: Option<String>,
    /// Where the app's project sits under `HOME`, as the header names it after `~/`, when the
    /// drawing names another than [`DRAWN_FOLDER`].
    #[serde(default)]
    pub folder: Option<String>,
    /// The instant the app's time starts at, RFC 3339, when the drawing is drawn at another than
    /// [`DRAWN_AT`]. Its offset is the time zone the screen writes times in.
    #[serde(default)]
    pub clock: Option<String>,
    /// Whether the terminal shows pictures, as Ghostty does. `false` plays a terminal with no
    /// picture protocol, where the app draws its maps in braille.
    #[serde(default = "shows_pictures")]
    pub pictures: bool,
    /// Whether the project is a git repository. `false` starts the app in a folder git has
    /// never seen.
    #[serde(default = "is_a_repository")]
    pub git: bool,
    /// What brings the app from its start to the drawn screen.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The decision that approved this state at its one size for now, where every other state
    /// is drawn at both of [`DRAWN_SIZES`]. It goes once the other size is drawn.
    #[serde(default)]
    pub one_size: Option<u32>,
    /// Why the app doesn't draw this screen yet. The drawings runner leaves a held drawing out
    /// unless it is picked by name.
    #[serde(default)]
    pub held: Option<Held>,
}

/// A drawing the app can't draw yet, which the drawings runner leaves out until it can.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Held {
    /// The number of the approval that lets it wait.
    pub approval: u32,
    /// What the app doesn't draw yet, and what it waits for.
    pub why: String,
}

/// The sizes every state is drawn at: 120 × 38 and 177 × 40.
pub const DRAWN_SIZES: [Size; 2] = [REAL_SIZES[0], REAL_SIZES[1]];

/// One step towards a drawn screen.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Waits until the visible rows match this regex, `^` and `$` matching at each row.
    Wait(String),
    /// Types this text; `\r` in it is Enter.
    Send(String),
    /// Presses a key by its name ([`key_named`]).
    Press(String),
    /// Moves the mouse, no button held, to this column and row from 0, as the terminal reports
    /// a move once the app asks for every one.
    Point([u16; 2]),
}

/// Cells of a drawing a screen draws otherwise, and the approval that allows it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Departure {
    pub file: String,
    /// The drawing's own row and its first and last columns, counted from its first cell as a
    /// report counts them, since a drawing that says `at ?` sits wherever the screen shows it.
    pub row: u16,
    pub cols: [u16; 2],
    /// What the screen draws there instead, and why.
    pub instead: String,
    pub approval: u32,
}

fn shows_pictures() -> bool {
    true
}

fn is_a_repository() -> bool {
    true
}

impl Entry {
    /// Where the app's project sits under `HOME` for this drawing's screen.
    pub fn folder(&self) -> &str {
        self.folder.as_deref().unwrap_or(DRAWN_FOLDER)
    }

    /// The instant the app's time starts at for this drawing's screen.
    pub fn clock(&self) -> &str {
        self.clock.as_deref().unwrap_or(DRAWN_AT)
    }
}

impl Manifest {
    pub fn load(dir: &Path) -> anyhow::Result<Manifest> {
        let path = dir.join(MANIFEST);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Whether a departure lets the screen draw the drawing `file`'s own cell at `row` and `col`
    /// otherwise.
    pub fn excuses(&self, file: &str, row: u16, col: u16) -> bool {
        self.departures.iter().any(|departure| {
            departure.file == file
                && departure.row == row
                && (departure.cols[0]..=departure.cols[1]).contains(&col)
        })
    }

    /// What is wrong with the manifest and the drawings in `dir`, each in a sentence: a drawing
    /// that is missing, unlisted, unreadable or changed since its hash; a project folder
    /// outside the home; a step that can't run; a held drawing with no approval or no reason; a
    /// state drawn at one size without the decision that allows it, or at both with one; a
    /// departure that names no drawing's cells or no approval.
    pub fn problems(&self, dir: &Path) -> Vec<String> {
        let mut problems = Vec::new();
        let mut listed = BTreeSet::new();
        for entry in &self.drawings {
            let file = &entry.file;
            if !listed.insert(file.as_str()) {
                problems.push(format!("{file} is listed twice"));
            }
            problems.extend(
                entry_problems(entry, dir)
                    .into_iter()
                    .map(|p| format!("{file}: {p}")),
            );
        }
        problems.extend(size_problems(&self.drawings));
        for file in drawing_files(dir) {
            if !listed.contains(file.as_str()) {
                problems.push(format!("{file} isn't listed in {MANIFEST}"));
            }
        }
        for departure in &self.departures {
            let at = format!(
                "{}, row {} cols {}-{}",
                departure.file, departure.row, departure.cols[0], departure.cols[1]
            );
            if !listed.contains(departure.file.as_str()) {
                problems.push(format!(
                    "{at}: departs from a drawing the manifest doesn't list"
                ));
                continue;
            }
            if departure.approval == 0 {
                problems.push(format!(
                    "{at}: a departure needs the number of its approval"
                ));
            }
            if departure.instead.trim().is_empty() {
                problems.push(format!(
                    "{at}: a departure says what the screen draws instead"
                ));
            }
            // A drawing that doesn't read is reported by its own row.
            if let Ok(drawing) = Drawing::load(&dir.join(&departure.file)) {
                let [first, last] = departure.cols;
                let inside =
                    first <= last && departure.row < drawing.height && last < drawing.width;
                if !inside {
                    problems.push(format!("{at}: these cells aren't in the drawing"));
                }
            }
        }
        problems
    }
}

/// Each state is drawn at both of [`DRAWN_SIZES`], or at one with `one_size` naming the decision
/// that approved it so, so a state that loses a size is noticed.
fn size_problems(drawings: &[Entry]) -> Vec<String> {
    let mut states: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for entry in drawings {
        let state = entry
            .file
            .rsplit_once('@')
            .map_or(entry.file.as_str(), |(state, _)| state);
        states.entry(state).or_default().push(entry);
    }
    let mut problems = Vec::new();
    for (state, entries) in states {
        let drawn: BTreeSet<(u16, u16)> = entries
            .iter()
            .filter_map(|entry| size_of(&entry.file))
            .filter(|&(cols, rows)| DRAWN_SIZES.contains(&Size::new(cols, rows)))
            .collect();
        let both = drawn.len() == DRAWN_SIZES.len();
        for entry in entries {
            let file = &entry.file;
            match (both, entry.one_size) {
                (false, None) => problems.push(format!(
                    "{file}: {state} is drawn at one size, and no `one_size` names the decision \
                     that approved it so"
                )),
                (true, Some(_)) => problems.push(format!(
                    "{file}: {state} is drawn at both sizes, so its `one_size` goes"
                )),
                (false, Some(0)) => problems.push(format!(
                    "{file}: `one_size` needs the number of its decision"
                )),
                _ => {}
            }
        }
    }
    problems
}

fn entry_problems(entry: &Entry, dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    if entry.release.trim().is_empty() {
        problems.push("no release".to_owned());
    }
    if entry.approval == 0 {
        problems.push("no approval number".to_owned());
    }
    if let Some(held) = &entry.held {
        if held.approval == 0 {
            problems
                .push("a held drawing needs the number of the approval that holds it".to_owned());
        }
        if held.why.trim().is_empty() {
            problems.push("a held drawing says what the app doesn't draw yet".to_owned());
        }
    }
    let path = dir.join(&entry.file);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let hash = hash_of(&bytes);
            if entry.hash != hash {
                problems.push(format!(
                    "changed since it was approved: its hash is now {hash}"
                ));
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if let Err(err) = std::str::from_utf8(&bytes)
                .map_err(|err| anyhow!("{err}"))
                .and_then(|text| Drawing::parse(name, text))
            {
                problems.push(format!("{err:#}"));
            }
        }
        Err(err) => problems.push(format!("{err}")),
    }
    for (what, folder) in [("home", &entry.home), ("clis", &entry.clis)] {
        if let Some(folder) = folder
            && !dir.join(folder).exists()
        {
            problems.push(format!("its {what}, {folder}, isn't there"));
        }
    }
    if let Some(folder) = &entry.folder {
        let inside = !folder.is_empty()
            && Path::new(folder)
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
        if !inside {
            problems.push(format!(
                "its folder, {folder}, isn't a path inside the home"
            ));
        }
    }
    for step in &entry.steps {
        match step {
            Step::Wait(pattern) => {
                if let Err(err) = regex::RegexBuilder::new(pattern).multi_line(true).build() {
                    problems.push(format!("the wait {pattern:?}: {err}"));
                }
            }
            Step::Press(name) if key_named(name).is_none() => {
                problems.push(format!("no key is named {name:?}"));
            }
            Step::Press(_) | Step::Send(_) | Step::Point(_) => {}
        }
    }
    problems
}

/// Every file under `dir` named as a drawing, `<panel>/<state>@<cols>x<rows>.txt`, as a path
/// under `dir`.
fn drawing_files(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
                continue;
            }
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| size_of(name).is_some());
            if let (true, Ok(relative)) = (named, path.strip_prefix(dir)) {
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found
}

/// A drawing's hash as the manifest writes it.
pub fn hash_of(bytes: &[u8]) -> String {
    format!("sha256:{}", argonauts_core::sha256_hex(bytes))
}

/// The key a step names: `Enter`, `Esc`, `Tab`, `Shift+Tab`, `Backspace`, `Up`, `Down`, `Left`,
/// `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Ctrl+<letter>`, or a single character.
pub fn key_named(name: &str) -> Option<Key> {
    let key = match name {
        "Enter" => Key::Enter,
        "Esc" => Key::Esc,
        "Tab" => Key::Tab,
        "Shift+Tab" => Key::ShiftTab,
        "Backspace" => Key::Backspace,
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        _ => {
            if let Some(letter) = name.strip_prefix("Ctrl+") {
                let mut letters = letter.chars();
                let letter = letters.next().filter(char::is_ascii_alphabetic)?;
                return letters.next().is_none().then_some(Key::Ctrl(letter));
            }
            let mut chars = name.chars();
            let c = chars.next()?;
            return chars.next().is_none().then_some(Key::Char(c));
        }
    };
    Some(key)
}

/// Runs `steps` in `pty`, each wait taking up to `wait`.
pub fn reach(pty: &mut Pty, steps: &[Step], wait: Duration) -> anyhow::Result<()> {
    for step in steps {
        match step {
            Step::Wait(pattern) => {
                pty.wait_for_visible(pattern, wait)?;
            }
            Step::Send(text) => pty.send(text)?,
            Step::Press(name) => {
                let key = key_named(name).ok_or_else(|| anyhow!("no key is named {name:?}"))?;
                pty.press(key)?;
            }
            Step::Point([col, row]) => pty.send_at_once(&pointer_moved(*col, *row))?,
        }
    }
    Ok(())
}

/// The SGR report of the mouse moving with no button held to `col`, `row` from 0: button 3,
/// plus 32 for a move, and 1-based cells.
fn pointer_moved(col: u16, row: u16) -> String {
    format!("\x1b[<35;{};{}M", col + 1, row + 1)
}

#[cfg(test)]
mod pointer_tests {
    #[test]
    fn a_move_is_reported_from_cell_one_with_no_button() {
        assert_eq!(super::pointer_moved(0, 0), "\x1b[<35;1;1M");
        assert_eq!(super::pointer_moved(30, 12), "\x1b[<35;31;13M");
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    fn rows(rows: &[(&str, Option<u32>)]) -> Vec<Entry> {
        let text: String = rows
            .iter()
            .map(|(file, one_size)| {
                let one_size = one_size
                    .map(|n| format!("one_size = {n}\n"))
                    .unwrap_or_default();
                format!(
                    "[[drawing]]\nfile = \"{file}\"\nrelease = \"0.2.1\"\napproval = 1\n\
                     hash = \"\"\n{one_size}"
                )
            })
            .collect();
        toml::from_str::<Manifest>(&text).unwrap().drawings
    }

    #[test]
    fn a_state_at_one_size_names_the_decision_that_approved_it_so() {
        let both = [
            ("limits/now@120x38.txt", None),
            ("limits/now@177x40.txt", None),
        ];
        assert!(size_problems(&rows(&both)).is_empty());
        assert!(size_problems(&rows(&[("stats/week@120x38.txt", Some(362))])).is_empty());
        let lost = size_problems(&rows(&[("limits/now@120x38.txt", None)]));
        assert_eq!(lost.len(), 1, "{lost:?}");
        assert!(lost[0].contains("drawn at one size"), "{lost:?}");
        let no_number = size_problems(&rows(&[("stats/week@120x38.txt", Some(0))]));
        assert!(no_number[0].contains("needs the number"), "{no_number:?}");
    }

    #[test]
    fn a_state_at_both_sizes_has_no_one_size() {
        let kept = [
            ("limits/now@120x38.txt", Some(362)),
            ("limits/now@177x40.txt", None),
        ];
        let problems = size_problems(&rows(&kept));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("so its `one_size` goes"),
            "{problems:?}"
        );
        // A size of its own, like 80 × 24, is neither of the two.
        let small = size_problems(&rows(&[("limits/now@80x24.txt", None)]));
        assert_eq!(small.len(), 1, "{small:?}");
    }
}

#[cfg(test)]
mod held_tests {
    use super::*;

    fn row(held: &str) -> anyhow::Result<Entry> {
        let text = format!(
            "[[drawing]]\nfile = \"limits/now@120x38.txt\"\nrelease = \"0.2.1\"\napproval = 1\n\
             hash = \"\"\n{held}"
        );
        let mut drawings = toml::from_str::<Manifest>(&text)?.drawings;
        Ok(drawings.remove(0))
    }

    fn held_problems(entry: &Entry) -> Vec<String> {
        entry_problems(entry, Path::new(""))
            .into_iter()
            .filter(|problem| problem.contains("held"))
            .collect()
    }

    #[test]
    fn a_held_drawing_names_its_approval_and_what_it_waits_for() {
        let held = row("held = { approval = 374, why = \"\"\"\\\n  Waits for its fakes.\"\"\" }\n")
            .unwrap();
        let expected = Held {
            approval: 374,
            why: "Waits for its fakes.".to_owned(),
        };
        assert_eq!(held.held, Some(expected));
        assert!(
            held_problems(&held).is_empty(),
            "{:?}",
            held_problems(&held)
        );
        assert_eq!(row("").unwrap().held, None);

        let unnumbered =
            held_problems(&row("held = { approval = 0, why = \"Waits.\" }\n").unwrap());
        assert_eq!(unnumbered.len(), 1, "{unnumbered:?}");
        assert!(unnumbered[0].contains("needs the number"), "{unnumbered:?}");
        let unsaid = held_problems(&row("held = { approval = 374, why = \" \" }\n").unwrap());
        assert_eq!(unsaid.len(), 1, "{unsaid:?}");
        assert!(unsaid[0].contains("doesn't draw yet"), "{unsaid:?}");
        assert!(row("held = { why = \"Waits.\" }\n").is_err());
        assert!(row("held = \"Waits.\"\n").is_err());
    }
}
