//! `approved/MANIFEST.toml`: each drawing with the release it was approved for, its approval's
//! number and its hash, how the app is brought to its screen, the test that holds its cells when
//! the real app can't draw them from the fake CLIs, and the cells a screen may draw otherwise,
//! each with the approval that allows it.

use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use serde::Deserialize;

use super::drawing::{Drawing, size_of};
use crate::pty::{Key, Pty};

pub const MANIFEST: &str = "MANIFEST.toml";

/// The project's folder under `HOME` that the drawings' headers name, `~/github/argonauts-code`.
pub const DRAWN_FOLDER: &str = "github/argonauts-code";

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
    /// What brings the app from its start to the drawn screen.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The test that holds the drawing's cells, `<file from the workspace root>::<test>`, for a
    /// screen whose figures the real app can't reach from the fake CLIs. The app is then only
    /// brought to the screen its steps name, and the test compares the cells.
    #[serde(default)]
    pub held_by: Option<String>,
}

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

impl Entry {
    /// Where the app's project sits under `HOME` for this drawing's screen.
    pub fn folder(&self) -> &str {
        self.folder.as_deref().unwrap_or(DRAWN_FOLDER)
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
    /// outside the home; a step that can't run; a holding test that isn't a test in the
    /// workspace around `dir`; a departure that names no drawing's cells or no approval.
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

fn entry_problems(entry: &Entry, dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    if entry.release.trim().is_empty() {
        problems.push("no release".to_owned());
    }
    if entry.approval == 0 {
        problems.push("no approval number".to_owned());
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
    if let Some(held_by) = &entry.held_by {
        let workspace = dir.parent().unwrap_or(dir);
        if let Err(problem) = holding_test(workspace, held_by) {
            problems.push(format!("its holding test, {held_by}, {problem}"));
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
            Step::Press(_) | Step::Send(_) => {}
        }
    }
    problems
}

/// Checks that `held_by`, `<file>::<test>`, names a test function in that file under
/// `workspace`, one that runs with the suite rather than only on `--ignored`.
fn holding_test(workspace: &Path, held_by: &str) -> Result<(), String> {
    let (file, test) = held_by
        .rsplit_once("::")
        .ok_or("isn't written <file>::<test>")?;
    let inside = Path::new(file)
        .components()
        .all(|part| matches!(part, Component::Normal(_)));
    if !inside || test.is_empty() {
        return Err("isn't written <file>::<test>, the file from the workspace root".to_owned());
    }
    let source = std::fs::read_to_string(workspace.join(file))
        .map_err(|err| format!("can't be read: {err}"))?;
    let lines: Vec<&str> = source.lines().collect();
    let named = format!("fn {test}(");
    let at = lines
        .iter()
        .position(|line| line.contains(&named))
        .ok_or("names no function in its file")?;
    // What stands between the item before and the function: its attributes and comments.
    let attributes: Vec<&str> = lines[..at]
        .iter()
        .rev()
        .map(|line| line.trim())
        .take_while(|line| !line.is_empty() && !line.ends_with(['{', '}', ';']))
        .collect();
    if !attributes.iter().any(|line| line.starts_with("#[test]")) {
        return Err("isn't a #[test]".to_owned());
    }
    if attributes.iter().any(|line| line.starts_with("#[ignore")) {
        return Err("is ignored, so the suite never runs it".to_owned());
    }
    Ok(())
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
        }
    }
    Ok(())
}

#[cfg(test)]
mod holding_tests {
    use super::*;

    const HELD: &str = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn draws_it() {}\n\n    \
                        #[test]\n    #[ignore = \"slow\"]\n    fn draws_it_slowly() {}\n\n    \
                        fn helper() {}\n}\n";

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/tui/src")).unwrap();
        std::fs::write(dir.path().join("crates/tui/src/drawn_tests.rs"), HELD).unwrap();
        dir
    }

    #[test]
    fn a_holding_test_is_a_test_the_suite_runs() {
        let dir = workspace();
        let at = |name: &str| {
            holding_test(
                dir.path(),
                &format!("crates/tui/src/drawn_tests.rs::{name}"),
            )
        };
        assert_eq!(at("draws_it"), Ok(()));
        assert_eq!(
            at("draws_it_slowly"),
            Err("is ignored, so the suite never runs it".to_owned())
        );
        assert_eq!(at("helper"), Err("isn't a #[test]".to_owned()));
        assert_eq!(at("draws"), Err("names no function in its file".to_owned()));
    }

    #[test]
    fn a_holding_test_names_its_file_inside_the_workspace() {
        let dir = workspace();
        let at = |held_by: &str| holding_test(dir.path(), held_by);
        assert!(at("draws_it").is_err());
        assert!(at("../outside.rs::draws_it").is_err());
        assert!(at("crates/tui/src/drawn_tests.rs::").is_err());
        let gone = at("crates/tui/src/gone.rs::draws_it").unwrap_err();
        assert!(gone.starts_with("can't be read"), "{gone}");
    }
}
