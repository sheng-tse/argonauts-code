//! Each workspace member's `src/`, read from disk once, with the production lines of its files.
//!
//! A production line is a non-blank line outside the tests: files named `tests.rs` or
//! `*_tests.rs`, files under a `tests/` folder, files a parent declares under `#[cfg(test)]`
//! (or `#[cfg(any(test, ...))]`), and each top-level item under that attribute.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub struct Member {
    /// As the root manifest names it: `crates/tui`.
    pub path: String,
    /// Every `.rs` file under its `src/`, from the workspace's top.
    sources: Vec<String>,
    /// Every folder under its `src/`, from the workspace's top.
    folders: BTreeSet<String>,
    /// Each file outside the tests, with its production lines.
    pub counted: Vec<(String, usize)>,
}

impl Member {
    pub fn lines(&self) -> usize {
        self.counted.iter().map(|(_, lines)| lines).sum()
    }

    fn src(&self) -> String {
        format!("{}/src", self.path)
    }

    /// The file the crate starts from: `lib.rs`, or `main.rs` for a crate that is only a binary.
    pub fn entry(&self) -> &'static str {
        let lib = format!("{}/lib.rs", self.src());
        if self.sources.contains(&lib) {
            "lib.rs"
        } else {
            "main.rs"
        }
    }

    /// The `.rs` files beside the entry file, at the top of `src/`.
    pub fn loose(&self) -> impl Iterator<Item = &str> {
        let src = self.src();
        self.sources.iter().map(String::as_str).filter(move |path| {
            path.strip_prefix(src.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|name| !name.contains('/') && name != "lib.rs" && name != "main.rs")
        })
    }

    /// Each `x.rs` that sits beside a folder `x/`, so its module is written in two places.
    pub fn beside_their_folders(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().map(String::as_str).filter(|path| {
            path.strip_suffix(".rs")
                .is_some_and(|folder| self.folders.contains(folder))
        })
    }
}

/// The members' sources, read once for every check.
pub fn members() -> &'static [Member] {
    static MEMBERS: LazyLock<Vec<Member>> = LazyLock::new(|| {
        let root = workspace();
        member_paths(&root)
            .into_iter()
            .map(|path| read(&root, path))
            .collect()
    });
    &MEMBERS
}

pub fn member(path: &str) -> Option<&'static Member> {
    members().iter().find(|member| member.path == path)
}

fn member_paths(root: &Path) -> Vec<String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("{}: {e}", manifest.display()));
    let table: toml::Table = toml::from_str(&text).unwrap_or_else(|e| panic!("Cargo.toml: {e}"));
    table
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .expect("Cargo.toml lists the workspace's members")
        .iter()
        .map(|member| member.as_str().expect("each member is a path").to_owned())
        .collect()
}

fn read(root: &Path, path: String) -> Member {
    let mut sources = Vec::new();
    let mut folders = BTreeSet::new();
    walk(root, &format!("{path}/src"), &mut sources, &mut folders);
    sources.sort();
    let texts: Vec<(String, String)> = sources
        .iter()
        .map(|file| {
            let text =
                std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            (file.clone(), text)
        })
        .collect();
    let test_only = declared_for_tests(&texts);
    let src = format!("{path}/src/");
    let counted = texts
        .iter()
        .filter(|(file, _)| {
            let below_src = file.strip_prefix(src.as_str()).unwrap_or(file);
            !is_test_file(below_src) && !under(file, &test_only)
        })
        .map(|(file, text)| (file.clone(), production_lines(text)))
        .collect();
    Member {
        path,
        sources,
        folders,
        counted,
    }
}

fn walk(root: &Path, folder: &str, sources: &mut Vec<String>, folders: &mut BTreeSet<String>) {
    let entries = std::fs::read_dir(root.join(folder)).unwrap_or_else(|e| panic!("{folder}: {e}"));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("{folder}: {e}"));
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{folder}/{name}");
        if entry.path().is_dir() {
            folders.insert(path.clone());
            walk(root, &path, sources, folders);
        } else if name.ends_with(".rs") {
            sources.push(path);
        }
    }
}

/// A test file by its name or by a `tests/` folder it sits in, from below `src/`.
fn is_test_file(below_src: &str) -> bool {
    let (folders, name) = below_src.rsplit_once('/').unwrap_or(("", below_src));
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || folders.split('/').any(|folder| folder == "tests")
}

fn under(path: &str, test_only: &BTreeSet<String>) -> bool {
    test_only.contains(path)
        || path
            .match_indices('/')
            .any(|(at, _)| test_only.contains(&path[..at]))
}

static TEST_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^#\[cfg\((?:test|any\(test\b[^\]]*)\)\]").expect("the attribute's pattern")
});
static MODULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod (\w+);").expect("the module's pattern")
});
static PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*#\[path\s*=\s*"([^"]+)"\]"#).expect("the path's pattern"));

/// The files and folders of every module declared only for tests, from the workspace's top.
fn declared_for_tests(texts: &[(String, String)]) -> BTreeSet<String> {
    let mut marked = BTreeSet::new();
    for (file, text) in texts {
        let (folder, name) = file.rsplit_once('/').unwrap_or(("", file));
        let children = if matches!(name, "mod.rs" | "lib.rs" | "main.rs") {
            folder.to_owned()
        } else {
            file.trim_end_matches(".rs").to_owned()
        };
        let lines: Vec<&str> = text.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            if !TEST_ONLY.is_match(line.trim()) {
                continue;
            }
            let mut next = at + 1;
            let mut path = None;
            while let Some(attribute) = lines.get(next).filter(|line| line.trim().starts_with("#["))
            {
                if let Some(found) = PATH.captures(attribute) {
                    path = Some(found[1].to_owned());
                }
                next += 1;
            }
            let Some(module) = lines.get(next).and_then(|line| MODULE.captures(line)) else {
                continue;
            };
            match path {
                // A `#[path]` is read from the declaring file's own folder.
                Some(path) => {
                    marked.insert(format!("{folder}/{path}"));
                }
                None => {
                    marked.insert(format!("{children}/{}.rs", &module[1]));
                    marked.insert(format!("{children}/{}", &module[1]));
                }
            }
        }
    }
    marked
}

/// The non-blank lines of a file outside its top-level `#[cfg(test)]` items. An item ends at
/// its own line when that line ends in `;` or `}`, else at the next `}` in the first column.
fn production_lines(text: &str) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    let mut count = 0;
    let mut at = 0;
    while at < lines.len() {
        if TEST_ONLY.is_match(lines[at]) {
            let mut end = at + 1;
            while lines.get(end).is_some_and(|line| line.starts_with("#[")) {
                end += 1;
            }
            let one_line = lines
                .get(end)
                .is_some_and(|line| line.trim_end().ends_with([';', '}']));
            if !one_line {
                while lines.get(end).is_some_and(|line| *line != "}") {
                    end += 1;
                }
            }
            at = end + 1;
            continue;
        }
        count += usize::from(!lines[at].trim().is_empty());
        at += 1;
    }
    count
}
