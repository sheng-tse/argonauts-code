//! The line under the input: which parts it has, on which side, and how the plan usage on it
//! draws. It is a mod, so its file sits with the mods, `mods/bottom-line.toml`, under
//! `~/.argonauts/` for every repository and under `.argonauts/` for one.
//!
//! ```toml
//! form  = "fit"                    # fit, or pin bars, compact, gauges, text
//! left  = ["mode", "artifact"]     # "mode" always stays: it says whether Argonauts will ask
//! right = ["plan-usage"]
//!
//! [plan-usage]
//! show    = "all"                  # all, main, near-limit
//! windows = "both"                 # both (for the CLI in use), fullest, 5h, 7d
//! as      = "used"                 # used, left
//! resets  = "near-limit"           # near-limit, always, never
//!
//! [narrow]
//! drop = ["spend", "branch"]       # what goes first on a narrow terminal
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::Table;

/// Where the file sits under `~/.argonauts/` and under a repository's `.argonauts/`.
pub const FILE: &str = "mods/bottom-line.toml";

/// The part that says whether Argonauts will ask. It can be moved, never left out.
pub const MODE: &str = "mode";
/// The artifact the conversation published, shown only once there is one.
pub const ARTIFACT: &str = "artifact";
pub const BRANCH: &str = "branch";
pub const PLAN_USAGE: &str = "plan-usage";
pub const SPEND: &str = "spend";
pub const CONTEXT: &str = "context";
pub const TOKENS: &str = "tokens";
pub const AWAKE: &str = "awake";
pub const STATS: &str = "stats";
pub const CHECKS: &str = "checks";
/// The workflow under way, with how many of its tasks are merged.
pub const RUN: &str = "run";

/// The parts the line draws from its own numbers before it looks for a mod of the name, and
/// that are no built-in card. A file of yours by one of these names never shows on the line.
pub const DRAWN_BY_THE_LINE: [&str; 5] = [MODE, ARTIFACT, BRANCH, AWAKE, RUN];

/// The parts the line knows by name. Any other name in `left` or `right` is a mod's, such as
/// `run`: the workflow under way, with how many of its tasks are merged.
pub const PARTS: [&str; 10] = [
    MODE, ARTIFACT, BRANCH, PLAN_USAGE, SPEND, CONTEXT, TOKENS, AWAKE, STATS, CHECKS,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BottomLineSettings {
    pub form: LineForm,
    /// Part names from the left edge on, joined by ` · `.
    pub left: Vec<String>,
    /// Part names against the right edge.
    pub right: Vec<String>,
    #[serde(rename = "plan-usage")]
    pub plan_usage: PlanUsageLine,
    pub narrow: NarrowLine,
}

impl Default for BottomLineSettings {
    fn default() -> Self {
        Self {
            form: LineForm::default(),
            left: vec![MODE.to_owned(), ARTIFACT.to_owned()],
            right: vec![PLAN_USAGE.to_owned()],
            plan_usage: PlanUsageLine::default(),
            narrow: NarrowLine::default(),
        }
    }
}

/// How each CLI's plan usage draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineForm {
    /// The fullest layout the terminal has room for, laid out again on every resize.
    #[default]
    Fit,
    /// A bar per window: both for the CLI in use, the fuller one for the others.
    Bars,
    /// One bar per CLI, its fuller window.
    Compact,
    /// One cell per window, the five hours then the seven days.
    Gauges,
    /// Words and numbers alone.
    Text,
}

impl LineForm {
    pub const ALL: [LineForm; 5] = [
        LineForm::Fit,
        LineForm::Bars,
        LineForm::Compact,
        LineForm::Gauges,
        LineForm::Text,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlanUsageLine {
    pub show: PlanUsageShow,
    pub windows: PlanUsageWindows,
    #[serde(rename = "as")]
    pub shown_as: PlanUsageAs,
    pub resets: PlanUsageResets,
}

/// Which CLIs the line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanUsageShow {
    /// Every CLI in the crew.
    #[default]
    All,
    /// Only the one the main session is on.
    Main,
    /// Those at 90% or out.
    NearLimit,
}

/// Which plan windows a CLI shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PlanUsageWindows {
    /// Both for the CLI in use, the fuller one for the others.
    #[default]
    #[serde(rename = "both")]
    Both,
    #[serde(rename = "fullest")]
    Fullest,
    #[serde(rename = "5h")]
    FiveHours,
    #[serde(rename = "7d")]
    SevenDays,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanUsageAs {
    #[default]
    Used,
    Left,
}

/// When a window's reset time shows beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanUsageResets {
    /// From 90% used, and while a CLI is out.
    #[default]
    NearLimit,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NarrowLine {
    /// The parts that go first when the terminal is too narrow, first to go first. The line
    /// then gives up the rest in its own fixed order.
    pub drop: Vec<String>,
}

impl Default for NarrowLine {
    fn default() -> Self {
        Self {
            drop: vec![SPEND.to_owned(), BRANCH.to_owned()],
        }
    }
}

impl BottomLineSettings {
    pub fn global_file(argonauts_home: &Path) -> PathBuf {
        argonauts_home.join(FILE)
    }

    pub fn project_file(repo_root: &Path) -> PathBuf {
        repo_root.join(".argonauts").join(FILE)
    }

    /// One file's settings. A key it leaves out keeps its default.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let table: Table = toml::from_str(text).map_err(|error| error.to_string())?;
        Self::from_table(table)
    }

    fn from_table(table: Table) -> Result<Self, String> {
        let settings: Self = table.try_into().map_err(|error| error.to_string())?;
        Ok(settings.tidied())
    }

    /// The file's text, as `/mods bottom-line` saves it.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Both files read and laid over the defaults, the repository's keys over yours. A file
    /// that doesn't read is left out and named in the problems, for Doctor.
    pub fn load(argonauts_home: &Path, repo_root: Option<&Path>) -> (Self, Vec<String>) {
        let mut files = vec![Self::global_file(argonauts_home)];
        files.extend(repo_root.map(Self::project_file));
        let read: Vec<(PathBuf, String)> = files
            .into_iter()
            .filter_map(|path| fs::read_to_string(&path).ok().map(|text| (path, text)))
            .collect();
        Self::layered(
            read.iter()
                .map(|(path, text)| (path.as_path(), text.as_str())),
        )
    }

    /// `layers` from the least to the most specific. Each must read by itself to count.
    pub fn layered<'a>(
        layers: impl IntoIterator<Item = (&'a Path, &'a str)>,
    ) -> (Self, Vec<String>) {
        let mut merged = Table::new();
        let mut problems = Vec::new();
        for (path, text) in layers {
            match toml::from_str::<Table>(text).map_err(|error| error.to_string()) {
                Ok(table) if Self::from_table(table.clone()).is_ok() => overlay(&mut merged, table),
                Ok(table) => {
                    let error = Self::from_table(table).err().unwrap_or_default();
                    problems.push(format!("{}: {}", path.display(), first_line(&error)));
                }
                Err(error) => problems.push(format!("{}: {}", path.display(), first_line(&error))),
            }
        }
        (Self::from_table(merged).unwrap_or_default(), problems)
    }

    /// The same settings with `mode` on the line and no part named twice. A part named on
    /// both sides stays where it comes first, the left before the right.
    pub fn tidied(mut self) -> Self {
        let mut seen: Vec<String> = Vec::new();
        for side in [&mut self.left, &mut self.right] {
            side.retain(|part| {
                let part = part.trim();
                let fresh = !part.is_empty() && !seen.iter().any(|known| known == part);
                if fresh {
                    seen.push(part.to_owned());
                }
                fresh
            });
            for part in side.iter_mut() {
                *part = part.trim().to_owned();
            }
        }
        if !seen.iter().any(|part| part == MODE) {
            self.left.insert(0, MODE.to_owned());
        }
        self
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Every part on the line, the left side first.
    pub fn parts(&self) -> impl Iterator<Item = &str> {
        self.left.iter().chain(&self.right).map(String::as_str)
    }

    pub fn has(&self, part: &str) -> bool {
        self.parts().any(|named| named == part)
    }
}

/// Lays `over` on `under`, table by table, so a file that sets one key of `[plan-usage]`
/// keeps the others from the file under it.
fn overlay(under: &mut Table, over: Table) {
    for (key, value) in over {
        match (under.get_mut(&key), value) {
            (Some(toml::Value::Table(under)), toml::Value::Table(over)) => overlay(under, over),
            (_, value) => {
                under.insert(key, value);
            }
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AS_WRITTEN: &str = r#"
form  = "fit"
left  = ["mode", "branch"]
right = ["plan-usage", "spend"]

[plan-usage]
show    = "all"
windows = "both"
as      = "used"
resets  = "near-limit"

[narrow]
drop = ["spend", "branch"]
"#;

    #[test]
    fn the_file_reads_with_exactly_the_keys_the_page_draws() {
        let settings = BottomLineSettings::from_toml(AS_WRITTEN).unwrap();
        assert_eq!(settings.form, LineForm::Fit);
        assert_eq!(settings.left, ["mode", "branch"]);
        assert_eq!(settings.right, ["plan-usage", "spend"]);
        assert_eq!(settings.plan_usage, PlanUsageLine::default());
        assert_eq!(settings.narrow.drop, ["spend", "branch"]);

        let every_value = r#"
form = "gauges"
[plan-usage]
show = "near-limit"
windows = "7d"
as = "left"
resets = "never"
"#;
        let settings = BottomLineSettings::from_toml(every_value).unwrap();
        assert_eq!(settings.form, LineForm::Gauges);
        assert_eq!(settings.plan_usage.show, PlanUsageShow::NearLimit);
        assert_eq!(settings.plan_usage.windows, PlanUsageWindows::SevenDays);
        assert_eq!(settings.plan_usage.shown_as, PlanUsageAs::Left);
        assert_eq!(settings.plan_usage.resets, PlanUsageResets::Never);
        for (form, word) in LineForm::ALL
            .iter()
            .zip(["fit", "bars", "compact", "gauges", "text"])
        {
            let read = BottomLineSettings::from_toml(&format!("form = \"{word}\"")).unwrap();
            assert_eq!(read.form, *form);
        }
        for windows in ["both", "fullest", "5h", "7d"] {
            let text = format!("[plan-usage]\nwindows = \"{windows}\"");
            assert!(BottomLineSettings::from_toml(&text).is_ok(), "{windows}");
        }
    }

    #[test]
    fn what_ships_is_the_mode_the_artifact_and_plan_usage() {
        let settings = BottomLineSettings::default();
        assert_eq!(settings.left, ["mode", "artifact"]);
        assert_eq!(settings.right, ["plan-usage"]);
        assert_eq!(settings.form, LineForm::Fit);
        assert_eq!(BottomLineSettings::from_toml("").unwrap(), settings);
    }

    /// The starter file as `/setup` writes it, comments and all, is the line as it ships. The
    /// settings page saves the same keys, so its file needs no translating.
    #[test]
    fn the_starter_file_reads_as_the_line_ships() {
        let starter = r#"# The line under the input. Each name is a mod.
form  = "fit"                   # fit: the fullest layout the terminal has room for · or pin bars, compact, gauges, text
left  = ["mode", "artifact"]    # "mode" always stays: it tells you whether Argonauts will ask
right = ["plan-usage"]          # more for either side: branch · spend · context · tokens · awake · stats · checks, or a mod of yours

[plan-usage]                    # how that mod draws when it sits on this line
show    = "all"                 # all: every CLI in the crew · main: only the one in use · near-limit
windows = "both"                # both (for the one in use) · fullest · 5h · 7d
as      = "used"                # used · left
resets  = "near-limit"          # near-limit · always · never

[narrow]
drop = ["spend", "branch"]      # what goes first; then bars step down to compact, then gauges
"#;
        assert_eq!(
            BottomLineSettings::from_toml(starter).unwrap(),
            BottomLineSettings::default()
        );
    }

    #[test]
    fn what_it_writes_reads_back_the_same() {
        let settings = BottomLineSettings {
            form: LineForm::Compact,
            left: vec!["mode".into(), "branch".into(), "my-mod".into()],
            right: vec!["plan-usage".into(), "spend".into()],
            plan_usage: PlanUsageLine {
                show: PlanUsageShow::Main,
                windows: PlanUsageWindows::FiveHours,
                shown_as: PlanUsageAs::Left,
                resets: PlanUsageResets::Always,
            },
            narrow: NarrowLine {
                drop: vec!["my-mod".into()],
            },
        };
        let written = settings.to_toml();
        assert!(written.contains("[plan-usage]"), "{written}");
        assert!(written.contains("as = \"left\""), "{written}");
        assert!(written.contains("windows = \"5h\""), "{written}");
        assert_eq!(BottomLineSettings::from_toml(&written).unwrap(), settings);
    }

    #[test]
    fn mode_can_move_but_never_leaves() {
        let moved = BottomLineSettings::from_toml("left = [\"branch\", \"mode\"]").unwrap();
        assert_eq!(moved.left, ["branch", "mode"]);
        let right = BottomLineSettings::from_toml("left = []\nright = [\"mode\"]").unwrap();
        assert_eq!(
            (right.left.len(), right.right.as_slice()),
            (0, &["mode".to_owned()][..])
        );
        let left_out = BottomLineSettings::from_toml("left = [\"branch\"]").unwrap();
        assert_eq!(left_out.left, ["mode", "branch"]);
        let twice =
            BottomLineSettings::from_toml("left = [\"mode\", \"spend\"]\nright = [\"spend\"]");
        let twice = twice.unwrap();
        assert_eq!((twice.left.len(), twice.right.len()), (2, 0));
    }

    #[test]
    fn a_repository_sets_a_key_and_keeps_the_rest_of_yours() {
        let yours = "form = \"bars\"\n[plan-usage]\nshow = \"main\"\nresets = \"always\"\n";
        let repo = "[plan-usage]\nshow = \"all\"\n";
        let (settings, problems) = BottomLineSettings::layered([
            (Path::new("home/mods/bottom-line.toml"), yours),
            (Path::new("repo/.argonauts/mods/bottom-line.toml"), repo),
        ]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(settings.form, LineForm::Bars);
        assert_eq!(settings.plan_usage.show, PlanUsageShow::All);
        assert_eq!(settings.plan_usage.resets, PlanUsageResets::Always);
    }

    #[test]
    fn a_bad_file_is_named_and_left_out() {
        let (settings, problems) = BottomLineSettings::layered([
            (
                Path::new("home/mods/bottom-line.toml"),
                "form = \"compact\"\n",
            ),
            (
                Path::new("repo/mods/bottom-line.toml"),
                "form = \"sideways\"\n",
            ),
            (Path::new("other/mods/bottom-line.toml"), "shape = 3\n"),
        ]);
        assert_eq!(settings.form, LineForm::Compact);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("repo/mods/bottom-line.toml: "));
        assert!(problems[1].contains("shape"), "{problems:?}");
    }

    #[test]
    fn the_two_files_load_from_their_folders() {
        let dir =
            std::env::temp_dir().join(format!("argonauts-bottom-line-{}", std::process::id()));
        let home = dir.join("home");
        let repo = dir.join("repo");
        let global = BottomLineSettings::global_file(&home);
        let project = BottomLineSettings::project_file(&repo);
        assert!(project.ends_with(".argonauts/mods/bottom-line.toml"));
        fs::create_dir_all(global.parent().unwrap()).unwrap();
        fs::create_dir_all(project.parent().unwrap()).unwrap();
        fs::write(&global, "right = [\"plan-usage\", \"spend\"]\n").unwrap();
        fs::write(&project, "form = \"text\"\n").unwrap();

        let (settings, problems) = BottomLineSettings::load(&home, Some(&repo));
        assert!(problems.is_empty());
        assert_eq!(settings.right, ["plan-usage", "spend"]);
        assert_eq!(settings.form, LineForm::Text);
        let (settings, _) = BottomLineSettings::load(&dir.join("nowhere"), None);
        assert_eq!(settings, BottomLineSettings::default());
        fs::remove_dir_all(&dir).unwrap();
    }
}
