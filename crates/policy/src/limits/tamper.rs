//! Test tampering: deleted test files, added skip or focus markers, and changed test or build
//! config. A script can't prove an assertion got weaker, so a diff that leaves fewer tests or
//! assertions than before is flagged for the reviewer, and an open flag blocks auto-approval.

use argonauts_core::policy::{HardLimit, Violation};

use super::diff::{
    FileChange, FileStatus, LineKind, file_name, json_entry, json_objects, toml_key, toml_places,
    toml_table_header, toml_value,
};
use super::violation;

pub fn check(changes: &[FileChange]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let deleted = paths_where(changes, deletes_test);
    if !deleted.is_empty() {
        violations.push(violation(
            HardLimit::TestDeleted,
            "deletes test files",
            deleted,
        ));
    }
    let skipped = paths_where(changes, adds_skip_marker);
    if !skipped.is_empty() {
        violations.push(violation(
            HardLimit::SkipMarker,
            "adds skip or focus markers",
            skipped,
        ));
    }
    let configs = paths_where(changes, changes_test_config);
    if !configs.is_empty() {
        violations.push(violation(
            HardLimit::TestConfigChanged,
            "changes test or build config",
            configs,
        ));
    }
    violations.extend(weakening(changes));
    violations
}

fn paths_where(changes: &[FileChange], test: fn(&FileChange) -> bool) -> Vec<String> {
    changes
        .iter()
        .filter(|change| test(change))
        .map(|change| change.path.clone())
        .collect()
}

const TEST_DIRS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "spec",
    "testing",
    "testdata",
    "e2e",
    "__snapshots__",
];

/// Test code, fixtures, and snapshots, by the usual names across languages.
pub fn is_test_path(path: &str) -> bool {
    let name = file_name(path);
    let in_test_dir = path
        .split('/')
        .rev()
        .skip(1)
        .any(|dir| TEST_DIRS.iter().any(|test| dir.eq_ignore_ascii_case(test)));
    in_test_dir || is_test_file_name(name)
}

fn is_test_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".snap") {
        return true;
    }
    let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
    let lower_stem = stem.to_ascii_lowercase();
    ["test", "tests", "conftest"].contains(&lower_stem.as_str())
        || lower_stem.starts_with("test_")
        || lower_stem.starts_with("test-")
        || ["_test", "-test", "_tests", "_spec", "-spec"]
            .iter()
            .any(|suffix| lower_stem.ends_with(suffix))
        || [".test.", ".spec."].iter().any(|infix| lower.contains(infix))
        // Java, Kotlin, and C# name the class: `ParserTest.java`, `ParserTests.cs`.
        || ["Test", "Tests", "Spec"].iter().any(|suffix| {
            stem.strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with(|c: char| c.is_ascii_alphanumeric()))
        })
}

fn deletes_test(change: &FileChange) -> bool {
    match change.status {
        FileStatus::Deleted => is_test_path(&change.path),
        // Moving a test where no runner finds it deletes it just the same.
        FileStatus::Renamed => {
            change.old_path.as_deref().is_some_and(is_test_path) && !is_test_path(&change.path)
        }
        FileStatus::Added | FileStatus::Modified => false,
    }
}

struct Marker {
    text: &'static str,
    /// Also a common call outside tests, like Rust's `iter().skip(1)`, so it counts only in a
    /// test file or on a test framework's own object.
    test_files_only: bool,
}

const fn marker(text: &'static str, test_files_only: bool) -> Marker {
    Marker {
        text,
        test_files_only,
    }
}

const SKIP_MARKERS: &[Marker] = &[
    marker(".skip(", true),
    marker(".only(", true),
    marker(".todo(", true),
    marker("xit(", false),
    marker("xdescribe(", false),
    marker("xtest(", false),
    marker("fit(", true),
    marker("fdescribe(", true),
    marker("#[ignore", false),
    marker("@pytest.mark.skip", false),
    marker("@pytest.mark.skipif", false),
    marker("@pytest.mark.xfail", false),
    marker("pytest.skip(", false),
    marker("@unittest.skip", false),
    marker("t.Skip(", false),
    marker("t.Skipf(", false),
    marker("t.SkipNow(", false),
    marker("@Disabled", false),
    marker("@Ignore", false),
];

/// Objects whose `.skip(` and `.only(` are test markers wherever they appear.
const TEST_RECEIVERS: &[&str] = &[
    "it",
    "test",
    "describe",
    "context",
    "suite",
    "specify",
    "bench",
    "concurrent",
];

fn adds_skip_marker(change: &FileChange) -> bool {
    let test_file = is_test_path(&change.path);
    change.status != FileStatus::Deleted
        && change.added().any(|line| has_skip_marker(line, test_file))
}

pub fn has_skip_marker(line: &str, test_file: bool) -> bool {
    SKIP_MARKERS.iter().any(|marker| {
        find_word(line, marker.text, Boundary::Word).any(|at| {
            !marker.test_files_only
                || test_file
                || (marker.text.starts_with('.') && TEST_RECEIVERS.contains(&receiver(line, at)))
        })
    })
}

#[derive(Clone, Copy)]
enum Boundary {
    /// `xit(` but not `exit(`, and `#[ignore]` but not `#[ignored]`.
    Word,
    /// `def test_` in `def test_parse`.
    Prefix,
    /// `assert` in `self.assertEqual`, too.
    PrefixAfterDot,
}

/// Where `needle` starts at a word boundary: an identifier char (or a `.`) can't come right
/// before it, and for a whole word none can come right after it either.
fn find_word<'a>(
    line: &'a str,
    needle: &'a str,
    boundary: Boundary,
) -> impl Iterator<Item = usize> + 'a {
    let after_dot = matches!(boundary, Boundary::PrefixAfterDot);
    let whole = matches!(boundary, Boundary::Word);
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(needle)
        .map(|(at, _)| at)
        .filter(move |&at| {
            let starts_word = needle.starts_with(is_ident);
            let before_ok = !starts_word
                || line[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !is_ident(c) && (after_dot || c != '.'));
            let ends_word = whole && needle.ends_with(is_ident);
            let after_ok = !ends_word
                || line[at + needle.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !is_ident(c));
            before_ok && after_ok
        })
}

/// The identifier right before a `.` at `at`, such as `describe` in `describe.only(`.
fn receiver(line: &str, at: usize) -> &str {
    let before = &line[..at];
    let start = before
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |at| at + 1);
    &before[start..]
}

const CONFIG_PREFIXES: &[&str] = &[
    "jest.config.",
    "vitest.config.",
    "vitest.workspace.",
    "playwright.config.",
    "cypress.config.",
    "karma.conf.",
    ".mocharc",
    ".nycrc",
];

const CONFIG_FILES: &[&str] = &["pytest.ini", "tox.ini", "nextest.toml"];

fn changes_test_config(change: &FileChange) -> bool {
    if change.paths().any(is_test_config_file) {
        return true;
    }
    match change.file_name() {
        "pyproject.toml" | "setup.cfg" => toml_changes(change, python_test_table, PYTEST_KEYS),
        "Cargo.toml" => toml_changes(change, cargo_test_table, CARGO_TEST_KEYS),
        "package.json" => package_json_test_config(change),
        _ => false,
    }
}

fn is_test_config_file(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    CONFIG_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || CONFIG_FILES.contains(&name.as_str())
        || path.ends_with(".cargo/config.toml")
        || path.ends_with(".cargo/config")
}

/// Keys that only mean something in a test section, for hunks that don't show their table.
const PYTEST_KEYS: &[&str] = &[
    "addopts",
    "testpaths",
    "python_files",
    "python_classes",
    "python_functions",
    "norecursedirs",
    "markers",
    "filterwarnings",
    "xfail_strict",
    "required_plugins",
    "minversion",
    "usefixtures",
    "fail_under",
    "omit",
];

const CARGO_TEST_KEYS: &[&str] = &[
    "harness",
    "doctest",
    "opt-level",
    "debug-assertions",
    "overflow-checks",
    "lto",
    "codegen-units",
    "panic",
    "incremental",
];

fn python_test_table(table: &str, _key: Option<&str>) -> bool {
    [
        "tool.pytest",
        "tool.coverage",
        "tool.tox",
        "tool.nox",
        "tool.hatch.envs",
        "tool:pytest",
        "coverage:",
    ]
    .iter()
    .any(|prefix| table.starts_with(prefix))
}

/// `[profile.*]`, `[[test]]`, `[[bench]]`, and whether a target is tested at all.
fn cargo_test_table(table: &str, key: Option<&str>) -> bool {
    table.starts_with("profile")
        || table == "test"
        || table == "bench"
        || (["lib", "bin", "example"].contains(&table)
            && key.is_some_and(|key| ["test", "harness", "doctest", "bench"].contains(&key)))
}

fn toml_changes(
    change: &FileChange,
    test_table: fn(&str, Option<&str>) -> bool,
    unplaced_keys: &[&str],
) -> bool {
    change.hunks.iter().any(|hunk| {
        toml_places(hunk).into_iter().any(|(place, line)| {
            if line.kind == LineKind::Context {
                return false;
            }
            let key = toml_key(&line.text);
            match (toml_table_header(&line.text), place.table) {
                (Some(header), _) => test_table(header, None),
                (None, Some(table)) => test_table(table, key),
                (None, None) => key.is_some_and(|key| {
                    unplaced_keys.contains(&key)
                        || (key == "test" && toml_value(&line.text).is_some_and(is_bool))
                }),
            }
        })
    })
}

fn is_bool(value: &str) -> bool {
    matches!(value, "true" | "false")
}

const JS_TEST_TOOLS: &[&str] = &["jest", "mocha", "ava", "nyc", "c8", "vitest"];

/// The `test` scripts and the test runners' own config blocks.
fn package_json_test_config(change: &FileChange) -> bool {
    change.hunks.iter().any(|hunk| {
        json_objects(hunk).into_iter().any(|(place, line)| {
            if line.kind == LineKind::Context {
                return false;
            }
            if place.within(JS_TEST_TOOLS) {
                return true;
            }
            let Some((key, _)) = json_entry(&line.text) else {
                return false;
            };
            let test_script =
                key == "test" || key == "pretest" || key == "posttest" || key.starts_with("test:");
            match place.innermost() {
                Some("") => JS_TEST_TOOLS.contains(&key),
                Some("scripts") | None => test_script || JS_TEST_TOOLS.contains(&key),
                Some(_) => false,
            }
        })
    })
}

const TEST_DECLARATIONS: &[&str] = &[
    "#[test]",
    "#[tokio::test",
    "#[rstest",
    "def test_",
    "func Test",
    "it(",
    "test(",
    "@Test",
    "[Fact]",
    "[Test]",
    "[TestMethod]",
];

const ASSERTIONS: &[&str] = &["assert", "expect(", "t.Error", "t.Fatal", "require."];

/// Counted across the whole diff, so tests moved from one file to another cancel out.
fn weakening(changes: &[FileChange]) -> Option<Violation> {
    let mut tests = 0i64;
    let mut assertions = 0i64;
    let mut losing = Vec::new();
    for change in changes {
        let checks_asserts = is_test_path(&change.path) || change.path.ends_with(".rs");
        let count = |kind: LineKind, markers: &[&str], boundary: Boundary| -> i64 {
            let lines = change
                .hunks
                .iter()
                .flatten()
                .filter(|line| line.kind == kind);
            let hits = lines
                .filter(|line| {
                    markers
                        .iter()
                        .any(|marker| find_word(&line.text, marker, boundary).next().is_some())
                })
                .count();
            i64::try_from(hits).unwrap_or(i64::MAX)
        };
        let file_tests = count(LineKind::Added, TEST_DECLARATIONS, Boundary::Prefix)
            - count(LineKind::Removed, TEST_DECLARATIONS, Boundary::Prefix);
        let file_assertions = if checks_asserts {
            count(LineKind::Added, ASSERTIONS, Boundary::PrefixAfterDot)
                - count(LineKind::Removed, ASSERTIONS, Boundary::PrefixAfterDot)
        } else {
            0
        };
        if file_tests < 0 || file_assertions < 0 {
            losing.push(change.path.clone());
        }
        tests += file_tests;
        assertions += file_assertions;
    }
    if tests >= 0 && assertions >= 0 {
        return None;
    }
    let mut lost = Vec::new();
    if tests < 0 {
        lost.push(format!("{} fewer tests", -tests));
    }
    if assertions < 0 {
        lost.push(format!("{} fewer assertions", -assertions));
    }
    Some(violation(
        HardLimit::TamperFlag,
        format!(
            "may weaken the tests ({}), so the reviewer checks it and the merge waits for you",
            lost.join(", ")
        ),
        losing,
    ))
}
