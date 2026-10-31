//! Files that building or testing leaves in a worktree: compiled bytecode, object files,
//! installed packages, and the caches tools write. What counts as build output varies by
//! language and tool, so by default the engine's commits take every file git doesn't ignore.
//! `merge.leave_out` names untracked files to leave out, and `merge.leave_out_builtin` turns
//! on the built-in list. Only names no source tree uses for its own files are on it: folders
//! such as `build/`, `dist/`, or `target/`, and files such as `*.obj`, are often source, so
//! they're left to `merge.leave_out`. The secret scan reads the list's files as text either
//! way, since most are binary and git shows no lines for a binary file.
//!
//! A pattern ending in `/` names a folder, any other a file. Without a `/` inside, it matches
//! at any depth; with one, from the repo's root. `*` and `?` match within one name. Case is
//! ignored, as on the file systems where it matters.

/// The built-in list of build and cache output, left out of commits only when
/// `merge.leave_out_builtin` is on. `merge.leave_out` names more, with the list on or off.
pub const BUILTIN: &[&str] = &[
    // Python
    "__pycache__/",
    ".pytest_cache/",
    ".mypy_cache/",
    ".ruff_cache/",
    ".tox/",
    ".nox/",
    ".venv/",
    ".eggs/",
    "?*.egg-info/",
    "htmlcov/",
    ".coverage",
    "?*.pyc",
    "?*.pyo",
    // JavaScript
    "node_modules/",
    ".next/",
    ".nuxt/",
    ".svelte-kit/",
    ".parcel-cache/",
    ".turbo/",
    // JVM and native builds
    ".gradle/",
    "?*.class",
    "?*.o",
    // Desktop leftovers
    ".DS_Store",
    "Thumbs.db",
];

/// Build and cache output: the patterns a setting names, after the built-in list when it's on.
/// The default names nothing, as the settings do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOutput {
    patterns: Vec<Pattern>,
}

impl Default for BuildOutput {
    fn default() -> Self {
        Self::of(false, &[])
    }
}

impl BuildOutput {
    /// The built-in list and `extra`, such as `merge.leave_out`. Blank patterns are skipped.
    pub fn with(extra: &[String]) -> Self {
        Self::of(true, extra)
    }

    /// `extra` alone, or after the built-in list when `builtin` is set.
    pub fn of(builtin: bool, extra: &[String]) -> Self {
        let listed: &[&str] = if builtin { BUILTIN } else { &[] };
        let patterns = listed
            .iter()
            .copied()
            .chain(extra.iter().map(String::as_str))
            .filter_map(Pattern::parse)
            .collect();
        Self { patterns }
    }

    /// Whether `path`, relative to the repo with `/` between folders, is build output.
    pub fn contains(&self, path: &str) -> bool {
        self.matched(path).is_some()
    }

    /// What made `path` build output: the outermost folder it is in, such as
    /// `pkg/__pycache__`, or the file itself.
    pub fn matched(&self, path: &str) -> Option<Match> {
        self.matches(path).into_iter().next()
    }

    /// Everything that makes `path` build output: each folder it is in that a pattern names,
    /// outermost first, then the file itself when a pattern names it. A folder the repo keeps
    /// files in is its own, so the inner ones still count then.
    pub fn matches(&self, path: &str) -> Vec<Match> {
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        let Some((name, folders)) = parts.split_last() else {
            return Vec::new();
        };
        let mut found: Vec<Match> = (0..folders.len())
            .filter(|&end| {
                self.patterns
                    .iter()
                    .any(|pattern| pattern.folder && pattern.matches_at(folders, end))
            })
            .map(|end| Match {
                path: folders[..=end].join("/"),
                folder: true,
            })
            .collect();
        if self
            .patterns
            .iter()
            .any(|pattern| !pattern.folder && pattern.matches_file(folders, name))
        {
            found.push(Match {
                path: parts.join("/"),
                folder: false,
            });
        }
        found
    }

    /// The built-in list as git pathspecs, for a `git diff` that picks the files out, or
    /// leaves them out when `exclude` is true.
    pub fn pathspecs(exclude: bool) -> Vec<String> {
        let magic = if exclude {
            "exclude,glob,icase"
        } else {
            "glob,icase"
        };
        BUILTIN
            .iter()
            .filter_map(|pattern| Pattern::parse(pattern))
            .map(|pattern| {
                let anchor = if pattern.rooted { "" } else { "**/" };
                let tail = if pattern.folder { "/**" } else { "" };
                format!(":({magic}){anchor}{}{tail}", pattern.parts.join("/"))
            })
            .collect()
    }
}

/// Files a setting names for another reason, in the same patterns, such as
/// `commit.always_keep`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilePatterns(BuildOutput);

impl FilePatterns {
    pub fn new(patterns: &[String]) -> Self {
        Self(BuildOutput::of(false, patterns))
    }

    /// Whether a pattern names `path`, relative to the repo with `/` between folders, or a
    /// folder it is in.
    pub fn contains(&self, path: &str) -> bool {
        self.0.contains(path)
    }
}

/// Why a path is build output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// The folder, without a trailing `/`, or the file.
    pub path: String,
    pub folder: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    /// Lowercase names, from the root when `rooted`.
    parts: Vec<String>,
    folder: bool,
    rooted: bool,
}

impl Pattern {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let folder = text.ends_with('/');
        let text = text.trim_start_matches("./").trim_matches('/');
        let parts: Vec<String> = text
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        if parts.is_empty() || parts.iter().any(|part| part == "..") {
            return None;
        }
        Some(Self {
            rooted: parts.len() > 1,
            parts,
            folder,
        })
    }

    /// Whether the pattern names the folder `folders[end]`.
    fn matches_at(&self, folders: &[&str], end: usize) -> bool {
        let len = self.parts.len();
        if end + 1 < len || (self.rooted && end + 1 != len) {
            return false;
        }
        let start = end + 1 - len;
        self.parts
            .iter()
            .zip(&folders[start..=end])
            .all(|(pattern, name)| glob(pattern, &name.to_ascii_lowercase()))
    }

    fn matches_file(&self, folders: &[&str], name: &str) -> bool {
        let Some((last, outer)) = self.parts.split_last() else {
            return false;
        };
        if !glob(last, &name.to_ascii_lowercase()) {
            return false;
        }
        if !self.rooted {
            return true;
        }
        outer.len() == folders.len()
            && outer
                .iter()
                .zip(folders)
                .all(|(pattern, folder)| glob(pattern, &folder.to_ascii_lowercase()))
    }
}

/// `*` matches any run of characters and `?` any one, within a single name.
fn glob(pattern: &str, name: &str) -> bool {
    let (pattern, name): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), name.chars().collect());
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((at, from)) => {
                    p = at + 1;
                    n = from + 1;
                    star = Some((at, from + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> BuildOutput {
        BuildOutput::of(true, &[])
    }

    #[test]
    fn bytecode_packages_build_folders_and_tool_caches_are_build_output() {
        for path in [
            "__pycache__/core.cpython-312.pyc",
            "stats/__pycache__/core.cpython-312.pyc",
            "legacy/core.pyc",
            "core.PYO",
            "build/Main.class",
            "src/lib.o",
            ".pytest_cache/v/cache/nodeids",
            "pkg/.mypy_cache/3.12/core.data.json",
            ".ruff_cache/0.6.0/123456",
            "node_modules/left-pad/index.js",
            "web/node_modules/.bin/vite",
            "textkit.egg-info/PKG-INFO",
            ".DS_Store",
            "docs/.DS_Store",
            ".coverage",
        ] {
            assert!(builtin().contains(path), "{path}");
        }
    }

    #[test]
    fn source_and_lookalikes_are_not() {
        for path in [
            "core.py",
            "src/main.rs",
            "docs/objects.md",
            "class",
            ".o",
            "pycache/notes.txt",
            "src/__pycache__",
            "tests/test_pyc.py",
            "build.gradle",
            "src/targets.rs",
            "distance.py",
            ".egg-info/x",
            "",
        ] {
            assert!(!builtin().contains(path), "{path:?}");
        }
    }

    /// The review's probe: folders named like build output, and a 3D model, that are source.
    #[test]
    fn build_dist_target_and_obj_are_source_unless_a_setting_says_so() {
        for path in [
            "textkit/build/__init__.py",
            "textkit/build/steps.py",
            "action/dist/index.js",
            "assets/cube.obj",
            "docs/target/README.md",
            "build/release.sh",
            "target/debug/app",
        ] {
            assert!(!builtin().contains(path), "{path:?}");
        }
        let output = BuildOutput::with(&["target/".to_owned(), "*.obj".to_owned()]);
        assert!(output.contains("target/debug/app"));
        assert!(output.contains("assets/cube.obj"));
    }

    #[test]
    fn the_built_in_list_is_opt_in() {
        let off = BuildOutput::default();
        assert_eq!(off, BuildOutput::of(false, &[]));
        assert!(!off.contains("__pycache__/core.cpython-312.pyc"));
        assert!(!off.contains(".DS_Store"));
        // `merge.leave_out` still names files with the list off.
        let only_logs = BuildOutput::of(false, &["*.log".to_owned()]);
        assert!(only_logs.contains("run.log"));
        assert!(!only_logs.contains("core.pyc"));
    }

    #[test]
    fn a_match_names_the_outermost_folder_or_the_file() {
        let found = builtin()
            .matched("web/node_modules/pkg/__pycache__/x.pyc")
            .unwrap();
        assert_eq!(found.path, "web/node_modules");
        assert!(found.folder);
        let found = builtin().matched("stats/core.pyc").unwrap();
        assert_eq!(found.path, "stats/core.pyc");
        assert!(!found.folder);
    }

    #[test]
    fn every_match_comes_outermost_first_and_the_file_last() {
        let output = BuildOutput::with(&["build/".to_owned()]);
        let found = output.matches("build/__pycache__/x.cpython-312.pyc");
        let paths: Vec<(&str, bool)> = found
            .iter()
            .map(|found| (found.path.as_str(), found.folder))
            .collect();
        assert_eq!(
            paths,
            [
                ("build", true),
                ("build/__pycache__", true),
                ("build/__pycache__/x.cpython-312.pyc", false)
            ]
        );
        assert!(
            output.matches("build/release.sh")
                == [Match {
                    path: "build".into(),
                    folder: true
                }]
        );
        assert!(output.matches("src/main.rs").is_empty());
    }

    #[test]
    fn a_setting_adds_folders_files_and_rooted_paths() {
        let extra = [
            "coverage/".to_owned(),
            "*.log".to_owned(),
            "docs/_build/".to_owned(),
            "  ".to_owned(),
            "../escape".to_owned(),
        ];
        let output = BuildOutput::with(&extra);
        assert!(output.contains("coverage/index.html"));
        assert!(output.contains("web/coverage/lcov.info"));
        assert!(output.contains("logs/run.LOG"));
        assert!(output.contains("docs/_build/html/index.html"));
        assert!(!output.contains("site/docs/_build/index.html"), "rooted");
        assert!(
            !output.contains("docs/_build"),
            "a file named like the folder"
        );
        assert!(!builtin().contains("logs/run.log"));
        assert!(
            output.contains("core.pyc"),
            "the built-in list still applies"
        );
    }

    #[test]
    fn globs_stay_within_one_name() {
        assert!(glob("*.pyc", "a.pyc"));
        assert!(glob("*.pyc", ".pyc"));
        assert!(!glob("?*.pyc", ".pyc"));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXbYY"));
        assert!(glob("*", ""));
        assert!(glob("t?rget", "target"));
    }

    #[test]
    fn pathspecs_pick_or_leave_out_the_same_files() {
        let picked = BuildOutput::pathspecs(false);
        let left_out = BuildOutput::pathspecs(true);
        assert_eq!(picked.len(), left_out.len());
        assert_eq!(picked.len(), BUILTIN.len());
        assert!(picked.contains(&":(glob,icase)**/__pycache__/**".to_owned()));
        assert!(left_out.contains(&":(exclude,glob,icase)**/?*.pyc".to_owned()));
        assert!(picked.contains(&":(glob,icase)**/.ds_store".to_owned()));
    }
}
