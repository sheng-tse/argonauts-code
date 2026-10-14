//! Dependencies: new entries in `package.json`, `Cargo.toml`, `pyproject.toml`,
//! `requirements*.txt`, or `go.mod` that aren't on `limits.dependency_allowlist`, and any
//! lockfile edit. Argonauts Code adds allowlisted dependencies itself, in its own commits, so a
//! task's diff has no reason to add any.
//!
//! A hunk may start inside a section whose header it doesn't show. Then an entry that looks
//! like a dependency counts as one.

use std::collections::BTreeSet;

use argonauts_core::policy::{HardLimit, Violation};
use argonauts_core::settings::Settings;

use super::diff::{
    Array, FileChange, FileStatus, GoBlock, Line, LineKind, TomlPlace, file_name, go_blocks,
    json_objects, json_string_entry, toml_key, toml_places, toml_table_header, toml_value,
};
use super::violation;

pub const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "go.sum",
    "poetry.lock",
    "uv.lock",
    "pdm.lock",
    "Pipfile.lock",
    "Gemfile.lock",
    "composer.lock",
];

pub fn is_lockfile(path: &str) -> bool {
    let name = file_name(path);
    LOCKFILES.iter().any(|lock| name.eq_ignore_ascii_case(lock))
}

pub fn check(changes: &[FileChange], settings: &Settings) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut locks: Vec<String> = changes
        .iter()
        .flat_map(FileChange::paths)
        .filter(|path| is_lockfile(path))
        .map(str::to_owned)
        .collect();
    locks.sort();
    locks.dedup();
    if !locks.is_empty() {
        violations.push(violation(
            HardLimit::Lockfile,
            "edits lockfiles, which only Argonauts Code updates",
            locks,
        ));
    }

    let allowlist = &settings.limits.dependency_allowlist;
    let mut names = BTreeSet::new();
    let mut manifests = Vec::new();
    for change in changes {
        let Some(manifest) = Manifest::of(&change.path) else {
            continue;
        };
        let off: Vec<String> = new_entries(manifest, change)
            .into_iter()
            .filter(|name| !manifest.ecosystem().allows(name, allowlist))
            .collect();
        if !off.is_empty() {
            names.extend(off);
            manifests.push(change.path.clone());
        }
    }
    if !names.is_empty() {
        let names: Vec<String> = names.into_iter().collect();
        violations.push(violation(
            HardLimit::Dependency,
            format!(
                "adds dependencies that aren't on your allowlist: {}",
                names.join(", ")
            ),
            manifests,
        ));
    }
    violations
}

/// A dependency a plan names, such as `serde` or `lodash@4.17.21`. An allowlist entry without a
/// version allows any version; one with a version allows only that exact entry.
pub fn allowed_at_point(name: &str, allowlist: &[String]) -> bool {
    let name = name.trim();
    allowlist.iter().map(|entry| entry.trim()).any(|entry| {
        entry == name || (entry == without_version(entry) && entry == without_version(name))
    })
}

/// `lodash@4` as `lodash`, and `@scope/pkg@1` as `@scope/pkg`.
fn without_version(entry: &str) -> &str {
    match entry.get(1..).and_then(|rest| rest.find('@')) {
        Some(at) => &entry[..at + 1],
        None => entry,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manifest {
    PackageJson,
    CargoToml,
    Pyproject,
    Requirements,
    GoMod,
}

impl Manifest {
    fn of(path: &str) -> Option<Self> {
        let name = file_name(path);
        let lower = name.to_ascii_lowercase();
        let in_requirements_dir = path
            .rsplit('/')
            .nth(1)
            .is_some_and(|dir| dir.eq_ignore_ascii_case("requirements"));
        match name {
            "package.json" => Some(Self::PackageJson),
            "Cargo.toml" => Some(Self::CargoToml),
            "pyproject.toml" => Some(Self::Pyproject),
            "go.mod" => Some(Self::GoMod),
            _ if lower.ends_with(".txt")
                && (lower.starts_with("requirements") || in_requirements_dir) =>
            {
                Some(Self::Requirements)
            }
            _ => None,
        }
    }

    fn ecosystem(self) -> Ecosystem {
        match self {
            Self::PackageJson => Ecosystem::Npm,
            Self::CargoToml => Ecosystem::Cargo,
            Self::Pyproject | Self::Requirements => Ecosystem::Python,
            Self::GoMod => Ecosystem::Go,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ecosystem {
    Npm,
    Cargo,
    Python,
    Go,
}

impl Ecosystem {
    /// Names the registry itself treats as one package compare equal: crates.io ignores case
    /// and `-` versus `_`, and PyPI also folds `.` and runs of them. npm and Go compare exactly.
    fn normalize(self, name: &str) -> String {
        match self {
            Self::Npm | Self::Go => name.to_owned(),
            Self::Cargo => name.to_ascii_lowercase().replace('_', "-"),
            Self::Python => {
                let mut out = String::with_capacity(name.len());
                for c in name.chars() {
                    if matches!(c, '-' | '_' | '.') {
                        if !out.ends_with('-') {
                            out.push('-');
                        }
                    } else {
                        out.push(c.to_ascii_lowercase());
                    }
                }
                out
            }
        }
    }

    fn allows(self, name: &str, allowlist: &[String]) -> bool {
        let wanted = self.normalize(name);
        allowlist.iter().any(|entry| {
            let entry = entry.trim();
            let entry = match self {
                Self::Python => pep508_name(entry).unwrap_or(entry),
                Self::Npm | Self::Cargo | Self::Go => without_version(entry),
            };
            self.normalize(entry) == wanted
        })
    }
}

/// Names added and not also removed, so a version bump isn't a new entry.
fn new_entries(manifest: Manifest, change: &FileChange) -> Vec<String> {
    if change.status == FileStatus::Deleted {
        return Vec::new();
    }
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for hunk in &change.hunks {
        for (kind, name) in entries(manifest, hunk) {
            match kind {
                LineKind::Added => added.push(name),
                LineKind::Removed => removed.push(name),
                LineKind::Context => {}
            }
        }
    }
    let ecosystem = manifest.ecosystem();
    let removed: BTreeSet<String> = removed.iter().map(|n| ecosystem.normalize(n)).collect();
    let mut new: Vec<String> = added
        .into_iter()
        .filter(|name| !removed.contains(&ecosystem.normalize(name)))
        .collect();
    new.sort();
    new.dedup();
    new
}

/// The dependency names on each changed line of a hunk.
fn entries(manifest: Manifest, hunk: &[Line]) -> Vec<(LineKind, String)> {
    match manifest {
        Manifest::PackageJson => package_json(hunk),
        Manifest::CargoToml => cargo_toml(hunk),
        Manifest::Pyproject => pyproject(hunk),
        Manifest::Requirements => requirements(hunk),
        Manifest::GoMod => go_mod(hunk),
    }
}

const NPM_SECTIONS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
    "overrides",
    "resolutions",
];

const NPM_TOP_LEVEL: &[&str] = &[
    "name",
    "version",
    "description",
    "main",
    "module",
    "browser",
    "types",
    "typings",
    "type",
    "license",
    "author",
    "homepage",
    "packageManager",
    "private",
];

fn package_json(hunk: &[Line]) -> Vec<(LineKind, String)> {
    json_objects(hunk)
        .into_iter()
        .filter(|(_, line)| line.kind != LineKind::Context)
        .filter_map(|(place, line)| {
            let (key, value) = json_string_entry(&line.text)?;
            let dependency = place.within(NPM_SECTIONS)
                || (place.innermost().is_none()
                    && !NPM_TOP_LEVEL.contains(&key)
                    && looks_like_npm_spec(value));
            dependency.then(|| (line.kind, key.to_owned()))
        })
        .collect()
}

/// `^1.2.0`, `~1`, `>=2 <3`, `*`, `latest`, `workspace:*`, `file:../x`, `github:user/repo`,
/// a URL, or `user/repo`. A script like `vitest run` isn't one.
fn looks_like_npm_spec(value: &str) -> bool {
    let value = value.trim();
    let versioned = value
        .trim_start_matches(['^', '~', '<', '>', '=', 'v', ' '])
        .starts_with(|c: char| c.is_ascii_digit());
    let schemes = [
        "workspace:",
        "file:",
        "link:",
        "npm:",
        "git+",
        "git:",
        "github:",
        "http:",
        "https:",
        "portal:",
        "patch:",
    ];
    versioned
        || ["*", "x", "latest", "next"].contains(&value)
        || schemes.iter().any(|scheme| value.starts_with(scheme))
        || (value.split('/').count() == 2
            && !value.starts_with('.')
            && !value.contains(char::is_whitespace))
}

const CARGO_KNOWN_KEYS: &[&str] = &[
    "name",
    "version",
    "edition",
    "rust-version",
    "authors",
    "description",
    "documentation",
    "readme",
    "homepage",
    "repository",
    "license",
    "license-file",
    "keywords",
    "categories",
    "workspace",
    "build",
    "links",
    "exclude",
    "include",
    "publish",
    "default-run",
    "resolver",
    "members",
    "default-members",
    "path",
    "test",
    "doctest",
    "bench",
    "doc",
    "harness",
    "crate-type",
    "required-features",
    "proc-macro",
    "opt-level",
    "debug",
    "strip",
    "debug-assertions",
    "overflow-checks",
    "lto",
    "panic",
    "incremental",
    "codegen-units",
    "rpath",
    "default",
];

/// `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, their `target.*` and
/// `workspace.*` forms, and `[patch.*]` and `[replace]`, which swap where a crate comes from.
fn cargo_dependency_table(table: &str) -> bool {
    let last = table.rsplit('.').next().unwrap_or(table);
    ["dependencies", "dev-dependencies", "build-dependencies"].contains(&last)
        || table.starts_with("patch.")
        || table == "replace"
}

fn cargo_toml(hunk: &[Line]) -> Vec<(LineKind, String)> {
    let mut found = Vec::new();
    for (place, line) in toml_places(hunk) {
        if line.kind == LineKind::Context {
            continue;
        }
        // `[dependencies.serde]` names the crate in its header.
        if let Some(table) = toml_table_header(&line.text) {
            if let Some((parent, name)) = table.rsplit_once('.')
                && cargo_dependency_table(parent)
            {
                found.push((line.kind, unquote(name).to_owned()));
            }
            continue;
        }
        let Some(key) = toml_key(&line.text) else {
            continue;
        };
        let name = key.split('.').next().unwrap_or(key).trim();
        let name = unquote(name);
        let dependency = match place.table {
            Some(table) if cargo_dependency_table(table) => true,
            // Inside `[dependencies.serde]`, keys are that crate's fields.
            Some(_) => false,
            None => {
                !CARGO_KNOWN_KEYS.contains(&name)
                    && (key.contains('.') || looks_like_cargo_spec(toml_value(&line.text)))
            }
        };
        if dependency {
            found.push((line.kind, name.to_owned()));
        }
    }
    found
}

fn looks_like_cargo_spec(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if let Some(table) = value.strip_prefix('{') {
        return ["version", "path", "git", "workspace", "registry", "package"]
            .iter()
            .any(|field| table.contains(field));
    }
    value
        .trim_matches('"')
        .trim_start_matches(['^', '~', '=', '<', '>', ' '])
        .starts_with(|c: char| c.is_ascii_digit() || c == '*')
}

/// Tables whose arrays list requirements, and poetry-style tables whose keys are packages.
fn pyproject_array_holds_requirements(table: &str, array: &str) -> bool {
    matches!(
        (table, array),
        ("project", "dependencies") | ("build-system", "requires")
    ) || table == "project.optional-dependencies"
        || table == "dependency-groups"
        || table.ends_with("dev-dependencies")
        || array == "dev-dependencies"
}

fn pyproject(hunk: &[Line]) -> Vec<(LineKind, String)> {
    let mut found = Vec::new();
    for (place, line) in toml_places(hunk) {
        if line.kind == LineKind::Context || toml_table_header(&line.text).is_some() {
            continue;
        }
        let TomlPlace { table, array } = place;
        if let (Some(key), Some(value)) = (toml_key(&line.text), toml_value(&line.text)) {
            if let Some(list) = value.strip_prefix('[') {
                // A one-line array, such as `dependencies = ["requests>=2"]`.
                let holds = match table {
                    Some(table) => pyproject_array_holds_requirements(table, key),
                    None => ["dependencies", "requires", "dev-dependencies"].contains(&key),
                };
                if holds {
                    for item in quoted_items(list) {
                        found.extend(pep508_name(item).map(|n| (line.kind, n.to_owned())));
                    }
                }
                continue;
            }
            let poetry_table = table.is_some_and(|t| t.ends_with("dependencies"));
            let unknown_poetry =
                table.is_none() && looks_like_cargo_spec(Some(value)) && key != "python";
            if (poetry_table && key != "python") || unknown_poetry {
                found.push((line.kind, key.to_owned()));
            }
            continue;
        }
        let items = quoted_items(&line.text);
        let holds = match (table, array) {
            (Some(table), Array::Inside(key)) => pyproject_array_holds_requirements(table, key),
            (None, Array::Inside(key)) => {
                ["dependencies", "requires", "dev-dependencies"].contains(&key)
            }
            (Some(table), Array::Unknown) => [
                "project",
                "project.optional-dependencies",
                "dependency-groups",
                "build-system",
            ]
            .contains(&table),
            // Somewhere unknown, only a requirement with a version or marker counts.
            (None, Array::Unknown) => {
                for item in &items {
                    if item.contains(['<', '>', '=', '~', '!', ';', '@', '['])
                        && let Some(name) = pep508_name(item)
                    {
                        found.push((line.kind, name.to_owned()));
                    }
                }
                continue;
            }
            (_, Array::Outside) => false,
        };
        if holds {
            for item in items {
                found.extend(pep508_name(item).map(|n| (line.kind, n.to_owned())));
            }
        }
    }
    found
}

fn requirements(hunk: &[Line]) -> Vec<(LineKind, String)> {
    hunk.iter()
        .filter(|line| line.kind != LineKind::Context)
        .filter_map(|line| {
            let code = match line.text.find(" #") {
                Some(at) => &line.text[..at],
                None => &line.text,
            };
            let code = code.trim();
            if code.is_empty() || code.starts_with('#') {
                return None;
            }
            // An option such as `-e git+https://…` or `--index-url …` changes what gets
            // installed too; it's named by the whole line.
            let name = if code.starts_with('-') {
                code.to_owned()
            } else {
                pep508_name(code).unwrap_or(code).to_owned()
            };
            Some((line.kind, name))
        })
        .collect()
}

fn go_mod(hunk: &[Line]) -> Vec<(LineKind, String)> {
    let mut found = Vec::new();
    for (block, line) in go_blocks(hunk) {
        if line.kind == LineKind::Context {
            continue;
        }
        let code = line.text.split("//").next().unwrap_or("").trim();
        if code.is_empty() || code == ")" || code.ends_with('(') {
            continue;
        }
        let (directive, rest) = match code.split_once(char::is_whitespace) {
            Some((word, rest)) if ["require", "replace"].contains(&word) => (word, rest.trim()),
            _ => match block {
                GoBlock::Inside(directive) => (directive, code),
                GoBlock::Outside => continue,
                // Either a requirement or a replacement: both change what gets built.
                GoBlock::Unknown if code.contains("=>") => ("replace", code),
                GoBlock::Unknown if looks_like_go_requirement(code) => ("require", code),
                GoBlock::Unknown => continue,
            },
        };
        let module = match directive {
            "require" => rest.split_whitespace().next(),
            "replace" => rest
                .split_once("=>")
                .and_then(|(_, target)| target.split_whitespace().next()),
            _ => None,
        };
        found.extend(module.map(|module| (line.kind, module.to_owned())));
    }
    found
}

fn looks_like_go_requirement(code: &str) -> bool {
    let mut words = code.split_whitespace();
    matches!(
        (words.next(), words.next()),
        (Some(module), Some(version)) if module.contains('.') && version.starts_with('v')
    )
}

/// The strings in `"a", "b>=1",`.
fn quoted_items(text: &str) -> Vec<&str> {
    text.split(['"', '\''])
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect()
}

/// The package name at the start of a PEP 508 requirement, such as `requests` in
/// `requests[socks]>=2; python_version>"3.8"`.
fn pep508_name(requirement: &str) -> Option<&str> {
    let requirement = requirement.trim();
    let end = requirement
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(requirement.len());
    let name = requirement[..end].trim_end_matches(['-', '_', '.']);
    name.starts_with(|c: char| c.is_ascii_alphanumeric())
        .then_some(name)
}

fn unquote(text: &str) -> &str {
    text.trim_matches(|c| c == '"' || c == '\'')
}
