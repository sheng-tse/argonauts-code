//! Loads every mod: the built-in ones, then yours from `~/.argonauts/mods/`, then the
//! project's from `.argonauts/mods/`. A later file replaces an earlier mod of the same name. A
//! file with a problem is named and skipped, and the rest keep working.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ModFile, ModPlace, builtin, check};
use crate::settings::bottom_line::{BottomLineSettings, DRAWN_BY_THE_LINE};
use crate::settings::ui::UiSettings;

/// A mod file larger than this isn't read. A real one is a few hundred bytes.
const MOST_BYTES: u64 = 64 * 1024;

/// Where a mod came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModOrigin {
    BuiltIn,
    /// `~/.argonauts/mods/`.
    Yours,
    /// `.argonauts/mods/` in the repository.
    Project,
}

impl ModOrigin {
    pub fn word(self) -> &'static str {
        match self {
            ModOrigin::BuiltIn => "built-in",
            ModOrigin::Yours => "yours",
            ModOrigin::Project => "project",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedMod {
    pub file: ModFile,
    pub origin: ModOrigin,
    /// The file it was read from. `None` for a built-in one.
    pub path: Option<PathBuf>,
    /// Its file names `bottom-line` among its places. That only asks: `file.places` holds
    /// `bottom-line` when the bottom line's own file lists the mod, whatever this says.
    pub asks_for_line: bool,
}

impl LoadedMod {
    /// The same mod with its places as the place lists in `ui` have them, when it is one of
    /// the built-in cards. The band above the input and the usage panel's tabs draw those cards
    /// themselves from the lists, so the lists and not the mod's own file say where one shows.
    pub fn placed_by_lists(mut self, ui: &UiSettings) -> Self {
        let Some(card) = builtin::closed_card(&self.file.name) else {
            return self;
        };
        let places = &mut self.file.places;
        for place in ModPlace::ALL {
            let Some(list) = place.list() else {
                continue;
            };
            let listed = ui.place(list).contains(&card);
            match (places.contains(&place), listed) {
                (true, false) => places.retain(|have| *have != place),
                (false, true) => places.push(place),
                _ => {}
            }
        }
        self
    }
}

/// The file of a mod that draws itself, such as the bottom line's, for the part of Argonauts Code
/// that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnFile {
    pub name: String,
    pub origin: ModOrigin,
    pub path: Option<PathBuf>,
    pub text: String,
}

/// A file that wasn't loaded, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModProblem {
    pub path: PathBuf,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModSet {
    /// The built-in cards first, in their own order, then yours and the project's by name.
    pub mods: Vec<LoadedMod>,
    /// The files of the mods that draw themselves, the one in use for each.
    pub own: Vec<OwnFile>,
    pub problems: Vec<ModProblem>,
    /// The bottom line as it reads its files: the project's keys over yours, over the
    /// built-in line.
    pub line: BottomLineSettings,
}

impl ModSet {
    /// Only the built-in ones, as a start with no folder gives.
    pub fn built_in() -> Self {
        let mut set = Self::unplaced();
        set.place_on_line();
        set
    }

    /// The built-in ones, before the bottom line's list has had its say.
    fn unplaced() -> Self {
        let mods = builtin::cards()
            .into_iter()
            .map(|file| LoadedMod {
                file,
                origin: ModOrigin::BuiltIn,
                path: None,
                asks_for_line: false,
            })
            .collect();
        let own = builtin::BUILTIN
            .iter()
            .filter(|builtin| builtin::draws_itself(builtin.name))
            .map(|builtin| OwnFile {
                name: builtin.name.to_owned(),
                origin: ModOrigin::BuiltIn,
                path: None,
                text: builtin.text.to_owned(),
            })
            .collect();
        Self {
            mods,
            own,
            problems: Vec::new(),
            line: BottomLineSettings::default(),
        }
    }

    /// Everything, from the folders that exist.
    pub fn load(yours: Option<&Path>, project: Option<&Path>) -> Self {
        let mut set = Self::unplaced();
        let mut line_files = Vec::new();
        for (dir, origin) in [(yours, ModOrigin::Yours), (project, ModOrigin::Project)] {
            if let Some(dir) = dir {
                line_files.extend(set.add_dir(dir, origin));
            }
        }
        let layers = line_files
            .iter()
            .map(|(path, text)| (path.as_path(), text.as_str()));
        set.line = BottomLineSettings::layered(layers).0;
        set.place_on_line();
        set
    }

    /// The bottom line shows what its own file lists, so that list says whether a mod is on
    /// the line. A mod's own `bottom-line` only asks to be.
    fn place_on_line(&mut self) {
        let line = &self.line;
        for loaded in &mut self.mods {
            let places = &mut loaded.file.places;
            let asks = places.contains(&ModPlace::BottomLine);
            let listed = line.has(&loaded.file.name);
            loaded.asks_for_line = asks;
            match (asks, listed) {
                (true, false) => places.retain(|place| *place != ModPlace::BottomLine),
                (false, true) => places.push(ModPlace::BottomLine),
                _ => {}
            }
        }
    }

    /// Reads one folder's files. Returns the bottom line's file when the folder has one that
    /// reads, since the line lays the project's over yours key by key.
    fn add_dir(&mut self, dir: &Path, origin: ModOrigin) -> Option<(PathBuf, String)> {
        let mut line_file = None;
        for path in toml_files(dir) {
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let stem = stem.to_owned();
            match read_file(&path, &stem) {
                Ok(Read::Card(file)) => self.put(LoadedMod {
                    file,
                    origin,
                    path: Some(path),
                    asks_for_line: false,
                }),
                Ok(Read::Own(text)) => {
                    if stem == builtin::BOTTOM_LINE {
                        line_file = Some((path.clone(), text.clone()));
                    }
                    let own = OwnFile {
                        name: stem.clone(),
                        origin,
                        path: Some(path),
                        text,
                    };
                    match self.own.iter_mut().find(|file| file.name == stem) {
                        Some(slot) => *slot = own,
                        None => self.own.push(own),
                    }
                }
                Err(problems) => self.problems.push(ModProblem { path, problems }),
            }
        }
        line_file
    }

    /// Adds a mod, in place of one of the same name.
    fn put(&mut self, loaded: LoadedMod) {
        match self
            .mods
            .iter_mut()
            .find(|have| have.file.name == loaded.file.name)
        {
            Some(slot) => *slot = loaded,
            None => self.mods.push(loaded),
        }
    }

    pub fn get(&self, name: &str) -> Option<&LoadedMod> {
        self.mods.iter().find(|loaded| loaded.file.name == name)
    }

    pub fn own(&self, name: &str) -> Option<&OwnFile> {
        self.own.iter().find(|file| file.name == name)
    }

    /// The mods that show in `place`, in order.
    pub fn placed(&self, place: ModPlace) -> impl Iterator<Item = &LoadedMod> {
        self.mods
            .iter()
            .filter(move |loaded| loaded.file.shows(place))
    }

    /// Whether `loaded` is a file of yours or the project's that asks for the bottom line by a
    /// name the line draws itself, so the line shows its own part and never this file. A
    /// starter as `/setup` wrote it draws what that part draws, so it isn't one.
    pub fn hidden_by_line(&self, loaded: &LoadedMod) -> bool {
        let name = loaded.file.name.as_str();
        let on_line = loaded.asks_for_line || self.line.has(name);
        if loaded.origin == ModOrigin::BuiltIn || !on_line || !DRAWN_BY_THE_LINE.contains(&name) {
            return false;
        }
        let starter = builtin::STARTERS
            .iter()
            .find(|(starter, _)| *starter == name)
            .and_then(|(_, text)| check::read(name, text).ok());
        match starter {
            Some(mut starter) => {
                starter.places.clone_from(&loaded.file.places);
                starter != loaded.file
            }
            None => true,
        }
    }

    /// Every mod's name, the ones that draw themselves included.
    pub fn names(&self) -> Vec<&str> {
        self.mods
            .iter()
            .map(|loaded| loaded.file.name.as_str())
            .chain(self.own.iter().map(|file| file.name.as_str()))
            .collect()
    }
}

enum Read {
    Card(ModFile),
    Own(String),
}

fn read_file(path: &Path, stem: &str) -> Result<Read, Vec<String>> {
    let size = std::fs::metadata(path)
        .map_err(|err| vec![format!("it can't be read: {err}")])?
        .len();
    if size > MOST_BYTES {
        return Err(vec![format!(
            "it's {} KB, and a mod file is at most {} KB",
            size / 1024,
            MOST_BYTES / 1024
        )]);
    }
    let text =
        std::fs::read_to_string(path).map_err(|err| vec![format!("it can't be read: {err}")])?;
    if stem == builtin::BOTTOM_LINE {
        // The line leaves out a file that doesn't read, so the built-in one stays in use.
        return match super::bottom_line::read(&text) {
            Ok(_) => Ok(Read::Own(text)),
            Err(why) => Err(vec![why]),
        };
    }
    if stem == builtin::STATS {
        // A stats file that doesn't read is named here, as Doctor and /mods name any.
        return match crate::settings::stats::StatsSettings::check(&text) {
            Ok(()) => Ok(Read::Own(text)),
            Err(why) => Err(vec![why]),
        };
    }
    if builtin::draws_itself(stem) {
        // Its own reader says what's wrong with its keys. Here only that it's TOML at all.
        return match text.parse::<toml::Table>() {
            Ok(_) => Ok(Read::Own(text)),
            Err(err) => Err(vec![format!("it isn't valid TOML: {}", err.message())]),
        };
    }
    check::read(stem, &text).map(Read::Card)
}

/// The `.toml` files in `dir`, by name. A folder that isn't there has none.
fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .collect();
    files.sort();
    files
}

/// A number that changes when a mod file is added, removed or edited: each file's name, size
/// and time. Cheap enough to ask every second, so a saved file shows at once.
pub fn stamp(dirs: &[Option<&Path>]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for dir in dirs.iter().flatten() {
        for path in toml_files(dir) {
            path.hash(&mut hasher);
            if let Ok(meta) = std::fs::metadata(&path) {
                meta.len().hash(&mut hasher);
                meta.modified().ok().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::mods::{ModBody, ModSource};

    const MINE: &str = "body = \"text\"\nplaces = [\"above\"]\n[data]\ncommand = [\"date\"]\n";

    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let yours = temp.path().join("home/mods");
        let project = temp.path().join("repo/.argonauts/mods");
        std::fs::create_dir_all(&yours).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        (temp, yours, project)
    }

    #[test]
    fn with_no_folder_the_built_in_ones_load() {
        let set = ModSet::load(None, Some(Path::new("/nowhere/at/all")));
        assert_eq!(set, ModSet::built_in());
        assert_eq!(
            set.names(),
            [
                "context",
                "plan-usage",
                "spend",
                "tokens",
                "runs",
                "waiting",
                "stats",
                "bottom-line"
            ]
        );
        assert!(set.problems.is_empty());
    }

    #[test]
    fn your_file_adds_a_mod_and_the_project_s_wins_a_name() {
        let (_temp, yours, project) = dirs();
        std::fs::write(yours.join("clock.toml"), MINE).unwrap();
        std::fs::write(yours.join("notes.md"), "not a mod").unwrap();
        std::fs::write(
            project.join("clock.toml"),
            MINE.replace("\"text\"", "\"list\""),
        )
        .unwrap();
        let set = ModSet::load(Some(&yours), Some(&project));
        let clock = set.get("clock").unwrap();
        assert_eq!(clock.origin, ModOrigin::Project);
        assert_eq!(clock.file.body, ModBody::List);
        assert_eq!(
            clock.path.as_deref(),
            Some(project.join("clock.toml").as_path())
        );
        assert_eq!(set.mods.len(), ModSet::built_in().mods.len() + 1);
        assert_eq!(
            set.placed(ModPlace::Above)
                .map(|loaded| loaded.file.name.as_str())
                .collect::<Vec<_>>(),
            ["clock"]
        );
    }

    #[test]
    fn your_file_replaces_a_built_in_card_where_it_stood() {
        let (_temp, yours, _project) = dirs();
        let text = "body = \"meters\"\n[data]\nsource = \"context\"\n";
        std::fs::write(yours.join("context.toml"), text).unwrap();
        let set = ModSet::load(Some(&yours), None);
        assert_eq!(set.mods[0].file.name, "context");
        assert_eq!(set.mods[0].origin, ModOrigin::Yours);
        assert_eq!(set.mods[0].file.body, ModBody::Meters);
        assert_eq!(set.mods.len(), ModSet::built_in().mods.len());
    }

    #[test]
    fn a_bad_file_is_named_and_the_rest_load() {
        let (_temp, yours, _project) = dirs();
        std::fs::write(yours.join("broken.toml"), "body = \"pie\"\n").unwrap();
        std::fs::write(yours.join("clock.toml"), MINE).unwrap();
        let set = ModSet::load(Some(&yours), None);
        assert!(set.get("clock").is_some());
        assert!(set.get("broken").is_none());
        assert_eq!(set.problems.len(), 1);
        assert_eq!(set.problems[0].path, yours.join("broken.toml"));
        assert_eq!(set.problems[0].problems.len(), 2);
    }

    #[test]
    fn the_bottom_line_s_file_is_kept_as_text_for_its_own_reader() {
        let (_temp, yours, _project) = dirs();
        std::fs::write(yours.join("bottom-line.toml"), "form = \"bars\"\n").unwrap();
        std::fs::write(yours.join("stats.toml"), "period = \n").unwrap();
        let set = ModSet::load(Some(&yours), None);
        let line = set.own("bottom-line").unwrap();
        assert_eq!(line.origin, ModOrigin::Yours);
        assert_eq!(line.text, "form = \"bars\"\n");
        // The broken one is named, and the built-in one stays in use.
        assert_eq!(set.own("stats").unwrap().origin, ModOrigin::BuiltIn);
        assert_eq!(set.problems.len(), 1);
    }

    #[test]
    fn a_bottom_line_file_the_line_wouldnt_read_is_named_and_left_out() {
        let (_temp, yours, _project) = dirs();
        std::fs::write(yours.join("bottom-line.toml"), "form = \"wide\"\n").unwrap();
        let set = ModSet::load(Some(&yours), None);
        assert_eq!(set.own("bottom-line").unwrap().origin, ModOrigin::BuiltIn);
        assert_eq!(set.problems.len(), 1);
        assert!(
            set.problems[0].problems[0].starts_with("form: unknown variant `wide`"),
            "{:?}",
            set.problems[0].problems
        );
    }

    #[test]
    fn a_file_named_like_a_part_the_line_draws_itself_is_hidden_there_and_said() {
        let (_temp, yours, project) = dirs();
        let asking = MINE.replace("[\"above\"]", "[\"bottom-line\"]");
        std::fs::write(yours.join("artifact.toml"), &asking).unwrap();
        std::fs::write(yours.join("clock.toml"), &asking).unwrap();
        // Above the input only, it shows where it asks.
        std::fs::write(project.join("run.toml"), MINE).unwrap();
        let starter = builtin::STARTERS.iter().find(|(name, _)| *name == "branch");
        std::fs::write(yours.join("branch.toml"), starter.unwrap().1).unwrap();
        let set = ModSet::load(Some(&yours), Some(&project));
        let hidden: Vec<&str> = set
            .mods
            .iter()
            .filter(|loaded| set.hidden_by_line(loaded))
            .map(|loaded| loaded.file.name.as_str())
            .collect();
        // The branch starter as written draws what the line's own branch part does.
        assert_eq!(hidden, ["artifact"]);

        let edited = starter.unwrap().1.replace("--show-current", "--list");
        std::fs::write(yours.join("branch.toml"), edited).unwrap();
        let set = ModSet::load(Some(&yours), Some(&project));
        assert!(set.hidden_by_line(set.get("branch").unwrap()));
    }

    #[test]
    fn the_bottom_line_s_list_says_which_mods_are_on_the_line() {
        let (_temp, yours, project) = dirs();
        let on_line = |set: &ModSet| -> Vec<String> {
            set.placed(ModPlace::BottomLine)
                .map(|loaded| loaded.file.name.clone())
                .collect()
        };
        // As it ships the line lists plan usage, and no file of yours.
        assert_eq!(on_line(&ModSet::built_in()), ["plan-usage"]);
        // One file asks for the line and isn't listed. The other is listed and doesn't ask.
        let asking = MINE.replace("[\"above\"]", "[\"bottom-line\", \"above\"]");
        std::fs::write(yours.join("clock.toml"), asking).unwrap();
        std::fs::write(yours.join("pulse.toml"), MINE).unwrap();
        let listing = "right = [\"plan-usage\", \"pulse\"]\n";
        std::fs::write(yours.join("bottom-line.toml"), listing).unwrap();
        let set = ModSet::load(Some(&yours), Some(&project));
        assert_eq!(on_line(&set), ["plan-usage", "pulse"]);
        let clock = set.get("clock").unwrap();
        assert!(clock.asks_for_line);
        assert_eq!(clock.file.places, [ModPlace::Above]);
        let pulse = set.get("pulse").unwrap();
        assert!(!pulse.asks_for_line);
        assert_eq!(pulse.file.places, [ModPlace::Above, ModPlace::BottomLine]);
        assert!(pulse.file.shows(ModPlace::BottomLine));
        // The project's keys lie over yours, as the line itself reads the two files.
        let project_s = "left = [\"mode\", \"clock\"]\n";
        std::fs::write(project.join("bottom-line.toml"), project_s).unwrap();
        let set = ModSet::load(Some(&yours), Some(&project));
        assert_eq!(set.line.left, ["mode", "clock"]);
        assert_eq!(set.line.right, ["plan-usage", "pulse"]);
        assert_eq!(on_line(&set), ["plan-usage", "clock", "pulse"]);
        let clock = set.get("clock").unwrap();
        assert!(clock.asks_for_line);
        assert_eq!(clock.file.places, [ModPlace::BottomLine, ModPlace::Above]);
    }

    #[test]
    fn a_built_in_card_shows_where_the_place_lists_say() {
        use crate::settings::ui::{ModName, Place};

        let (_temp, yours, _project) = dirs();
        // Your copy of a built-in card, with places of its own, and a mod only you have.
        let copy = "body = \"meters\"\nplaces = [\"overview\", \"usage\"]\n[data]\nsource = \"plan_usage\"\n";
        std::fs::write(yours.join("plan-usage.toml"), copy).unwrap();
        std::fs::write(yours.join("clock.toml"), MINE).unwrap();
        let set = ModSet::load(Some(&yours), None);
        let mut ui = UiSettings::default();
        *ui.place_mut(Place::AboveInput) = vec![ModName::PlanUsage];
        *ui.place_mut(Place::Usage) = Vec::new();
        let places = |name: &str| {
            let loaded = set.get(name).unwrap().clone();
            loaded.placed_by_lists(&ui).file.places
        };
        // The lists say above, not usage, and the Status tab as it ships. No list keeps the
        // Overview tab or the line.
        assert_eq!(
            places("plan-usage"),
            [
                ModPlace::Overview,
                ModPlace::BottomLine,
                ModPlace::Above,
                ModPlace::Status
            ]
        );
        assert_eq!(places("clock"), [ModPlace::Above]);
        assert_eq!(builtin::closed_card("plan-usage"), Some(ModName::PlanUsage));
        assert_eq!(builtin::closed_card("waiting"), Some(ModName::Waiting));
        // Neither a mod of yours nor one that draws itself is a card of the lists.
        assert_eq!(builtin::closed_card("clock"), None);
        assert_eq!(builtin::closed_card("stats"), None);
        assert_eq!(builtin::closed_card("agent"), None);
    }

    #[test]
    fn the_stamp_changes_when_a_file_does() {
        let (_temp, yours, project) = dirs();
        let stamp_now = || stamp(&[Some(yours.as_path()), Some(project.as_path()), None]);
        let empty = stamp_now();
        std::fs::write(yours.join("clock.toml"), MINE).unwrap();
        let one = stamp_now();
        assert_ne!(empty, one);
        std::fs::write(yours.join("clock.toml"), format!("{MINE}# more\n")).unwrap();
        let edited = stamp_now();
        assert_ne!(one, edited);
        std::fs::write(yours.join("readme.md"), "words").unwrap();
        assert_eq!(stamp_now(), edited);
        std::fs::remove_file(yours.join("clock.toml")).unwrap();
        assert_eq!(stamp_now(), empty);
    }

    #[test]
    fn a_command_mod_keeps_its_command() {
        let (_temp, yours, _project) = dirs();
        std::fs::write(yours.join("clock.toml"), MINE).unwrap();
        let set = ModSet::load(Some(&yours), None);
        assert!(matches!(
            &set.get("clock").unwrap().file.data,
            ModSource::Command { command, .. } if command == &["date".to_owned()]
        ));
    }
}
