//! What part a file plays in a project, from its path alone.

use argonauts_core::behaviors::Label;
use serde_json::Value;

/// The keys each CLI names a tool's file or folder by: Claude Code's `file_path` and `path`,
/// OpenCode's `filePath`, and agy's `AbsolutePath`, `TargetFile` and the rest.
const PATH_KEYS: [&str; 10] = [
    "file_path",
    "filePath",
    "notebook_path",
    "path",
    "AbsolutePath",
    "TargetFile",
    "File",
    "DirectoryPath",
    "SearchDirectory",
    "SearchPath",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileRole {
    Source,
    Test,
    Config,
    Doc,
    /// Argonauts' own run records, under `~/.argonauts/state`.
    Records,
}

/// The file or folder a tool call names, if it names one.
pub(crate) fn path_of(input: &Value) -> Option<&str> {
    PATH_KEYS
        .iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
        .filter(|path| !path.is_empty())
}

pub(crate) fn role_of(path: &str, root: Option<&str>) -> FileRole {
    if is_records(path) {
        FileRole::Records
    } else {
        project_role(path, root)
    }
}

pub(super) fn is_records(path: &str) -> bool {
    path.contains(".argonauts/state/")
}

/// Where a path lies, as far as its project's folder says.
enum Inside<'a> {
    /// Below the project's folder or a task's worktree, as the part below it.
    Project(&'a str),
    /// Outside the project's folder, which is known.
    Elsewhere,
    /// Relative, or absolute with no folder known to hold it.
    Unknown(&'a str),
}

/// The part of `path` below its project: below `root`, the project's folder, or below a
/// task's worktree, `…/worktrees/<project>/<run>/<task>/`. The folders above a project say
/// nothing of its files: a project kept under `~/test/` holds no more tests than another.
fn inside<'a>(path: &'a str, root: Option<&str>) -> Inside<'a> {
    if let Some(rest) = root
        .map(|root| root.trim_end_matches('/'))
        .filter(|root| !root.is_empty())
        .and_then(|root| path.strip_prefix(root))
        .and_then(|rest| rest.strip_prefix('/'))
    {
        return Inside::Project(rest);
    }
    if let Some((_, below)) = path.rsplit_once("/worktrees/")
        && let Some(rest) = below.splitn(4, '/').nth(3)
    {
        return Inside::Project(rest);
    }
    match root {
        Some(_) if path.starts_with('/') => Inside::Elsewhere,
        _ => Inside::Unknown(path),
    }
}

/// A file's role in the project whose folder is `root`, when it's known, never Argonauts'
/// records: an edit is to a file of some kind, wherever it lies.
pub(super) fn project_role(path: &str, root: Option<&str>) -> FileRole {
    let folders = match inside(path, root) {
        Inside::Project(rest) | Inside::Unknown(rest) => rest.to_ascii_lowercase(),
        Inside::Elsewhere => String::new(),
    };
    let path = path.to_ascii_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    let in_folder = |folder: &str| {
        let below = folders.rsplit_once('/').map_or("", |(dirs, _)| dirs);
        below == folder
            || below.starts_with(&format!("{folder}/"))
            || below.contains(&format!("/{folder}/"))
            || below.ends_with(&format!("/{folder}"))
    };
    let tests = TEST_FOLDERS.iter().any(|folder| in_folder(folder));
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    let test_name = stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
        || stem.ends_with(".test")
        || stem.ends_with(".spec")
        || stem.ends_with("_tests")
        || stem == "conftest"
        || (stem == "tests" && CODE_EXTENSIONS.contains(&extension))
        || (extension.is_empty() && TEST_FOLDERS.contains(&stem))
        || (stem.starts_with("test") && extension == "go");
    if tests || test_name {
        return FileRole::Test;
    }
    const CONFIG_NAMES: [&str; 8] = [
        "makefile",
        "dockerfile",
        "justfile",
        ".gitignore",
        ".editorconfig",
        "go.mod",
        "setup.py",
        "setup.cfg",
    ];
    const CONFIG_EXTENSIONS: [&str; 9] = [
        "toml", "yaml", "yml", "json", "ini", "cfg", "conf", "lock", "env",
    ];
    if CONFIG_NAMES.contains(&name)
        || CONFIG_EXTENSIONS.contains(&extension)
        || name.starts_with("requirements")
    {
        return FileRole::Config;
    }
    const DOC_EXTENSIONS: [&str; 5] = ["md", "mdx", "rst", "txt", "adoc"];
    if DOC_EXTENSIONS.contains(&extension) || ["readme", "license", "changelog"].contains(&stem) {
        return FileRole::Doc;
    }
    FileRole::Source
}

/// Folders that hold tests, and a folder named so is one of them.
const TEST_FOLDERS: [&str; 5] = ["test", "tests", "__tests__", "spec", "specs"];

/// Files of code, which a test is written in: Rust's `tests.rs` and Django's `tests.py` hold
/// a module of tests, and only such a file removed takes tests with it.
const CODE_EXTENSIONS: [&str; 18] = [
    "rs", "py", "js", "mjs", "cjs", "jsx", "ts", "tsx", "go", "rb", "java", "kt", "swift", "c",
    "cc", "cpp", "cs", "php",
];

/// Whether removing `path` removes tests: a test file of code, or a folder of tests whole. A
/// cache or a fixture inside a folder of tests is neither.
pub(super) fn holds_tests(path: &str, root: Option<&str>) -> bool {
    let path = path.trim_end_matches('/');
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if TEST_FOLDERS.contains(&name.as_str()) {
        return true;
    }
    let code = name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| CODE_EXTENSIONS.contains(&extension));
    code && project_role(path, root) == FileRole::Test
}

pub(super) fn read(path: &str, root: Option<&str>) -> Label {
    match role_of(path, root) {
        FileRole::Source => Label::ReadSource,
        FileRole::Test => Label::ReadTest,
        FileRole::Config => Label::ReadConfig,
        FileRole::Doc => Label::ReadDoc,
        FileRole::Records => Label::ReadRecords,
    }
}

pub(super) fn wrote(path: &str, root: Option<&str>) -> Label {
    match project_role(path, root) {
        FileRole::Test => Label::WriteTest,
        FileRole::Config => Label::WriteConfig,
        FileRole::Doc => Label::WriteDoc,
        FileRole::Source | FileRole::Records => Label::WriteSource,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_by_path() {
        let cases = [
            ("src/store.js", FileRole::Source),
            ("/w/run/task/ratelimit/bucket.py", FileRole::Source),
            ("tests/test_money.py", FileRole::Test),
            ("/w/task/test/store.test.js", FileRole::Test),
            ("pkg/slug_test.go", FileRole::Test),
            ("src/__tests__/cli.js", FileRole::Test),
            ("spec/models/user_spec.rb", FileRole::Test),
            ("crates/app/src/work_ledger_tests.rs", FileRole::Test),
            ("conftest.py", FileRole::Test),
            ("package.json", FileRole::Config),
            ("Cargo.toml", FileRole::Config),
            ("requirements-dev.txt", FileRole::Config),
            (".github/workflows/ci.yml", FileRole::Config),
            ("README.md", FileRole::Doc),
            ("docs/notes.txt", FileRole::Doc),
            (
                "/Users/you/.argonauts/state/repo-1/runs/run-1/events.jsonl",
                FileRole::Records,
            ),
        ];
        for (path, role) in cases {
            assert_eq!(role_of(path, None), role, "{path}");
        }
        assert_eq!(
            role_of("crates/app/src/views/tests.rs", None),
            FileRole::Test
        );
        assert_eq!(role_of("polls/tests.py", None), FileRole::Test);
        assert_eq!(role_of("fixtures/tests.json", None), FileRole::Config);
    }

    #[test]
    fn test_folders_count_only_inside_the_project() {
        let root = Some("/Users/you/test/proj");
        assert_eq!(
            role_of("/Users/you/test/proj/src/app.py", root),
            FileRole::Source
        );
        assert_eq!(
            role_of("/Users/you/test/proj/tests/helpers.py", root),
            FileRole::Test
        );
        assert_eq!(role_of("src/app.py", root), FileRole::Source);
        assert_eq!(role_of("/tmp/test/notes.py", root), FileRole::Source);
        assert_eq!(role_of("/tmp/test/test_x.py", root), FileRole::Test);
        let worktree = "/Users/you/.argonauts/worktrees/proj-1/run-1/test-task/src/app.py";
        assert_eq!(role_of(worktree, None), FileRole::Source);
        let in_tests = "/Users/you/.argonauts/worktrees/proj-1/run-1/task-1/test/store.js";
        assert_eq!(role_of(in_tests, None), FileRole::Test);
    }

    #[test]
    fn a_record_written_to_is_a_file_of_its_kind() {
        let records = "/Users/you/.argonauts/state/repo-1/notes.md";
        assert_eq!(read(records, None), Label::ReadRecords);
        assert_eq!(wrote(records, None), Label::WriteDoc);
    }
}
