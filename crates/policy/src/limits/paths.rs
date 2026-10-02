//! Protected and forbidden paths (`.argonauts/**` and `.agents/**` always,
//! `limits.protected_paths`, and the card's `forbidden_paths`), and instruction files anywhere in
//! a diff, which block auto-merge.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use argonauts_core::events::card::CardSpec;
use argonauts_core::policy::{HardLimit, Violation};
use argonauts_core::settings::Settings;

use super::diff::{FileChange, file_name};
use super::violation;

/// Protected whatever the settings say: Argonauts' own files, and the reviewed skills crew
/// sessions load.
pub const ALWAYS_PROTECTED: &[&str] = &[".argonauts/**", ".agents/**"];

/// Files other agents read as instructions. A new or changed one changes what a later session
/// does, so it's flagged.
pub const INSTRUCTION_FILES: &[&str] = &[
    "SKILL.md",
    "AGENTS.md",
    "AGENTS.override.md",
    "CLAUDE.md",
    "GEMINI.md",
    "HERMES.md",
    ".hermes.md",
];

pub fn is_instruction_file(path: &str) -> bool {
    let name = file_name(path);
    INSTRUCTION_FILES
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// Glob patterns read the way `.gitignore` reads them: `*` stays inside one directory, a
/// pattern without a slash matches at any depth, and a directory covers everything under it.
/// Case is ignored, since on a case-insensitive disk `.Argonauts/x` is `.argonauts/x`.
pub struct PathRules {
    set: GlobSet,
    /// Patterns that aren't valid globs, with why.
    pub invalid: Vec<String>,
}

impl PathRules {
    pub fn new<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Self {
        let mut builder = GlobSetBuilder::new();
        let mut invalid = Vec::new();
        for pattern in patterns {
            for glob in expand(pattern) {
                let built = GlobBuilder::new(&glob)
                    .literal_separator(true)
                    .case_insensitive(true)
                    .backslash_escape(true)
                    .build();
                match built {
                    Ok(glob) => {
                        builder.add(glob);
                    }
                    Err(err) => invalid.push(format!("`{pattern}` ({err})")),
                }
            }
        }
        invalid.dedup();
        let set = builder.build().unwrap_or_else(|err| {
            invalid.push(format!("the whole set ({err})"));
            GlobSet::empty()
        });
        Self { set, invalid }
    }

    /// The protected set: the built-in paths plus `limits.protected_paths`.
    pub fn protected(settings: &Settings) -> Self {
        let configured = settings.limits.protected_paths.iter().map(String::as_str);
        Self::new(ALWAYS_PROTECTED.iter().copied().chain(configured))
    }

    pub fn matches(&self, path: &str) -> bool {
        self.set.is_match(path)
    }

    /// Whether a path, or a path an owned glob like `.github/**` names, is covered. A broad glob
    /// like `src/**` isn't, since the diff gate checks what the card actually changes.
    pub fn covers(&self, path_or_glob: &str) -> bool {
        self.matches(path_or_glob) || self.matches(&sample_path(path_or_glob))
    }
}

fn expand(pattern: &str) -> Vec<String> {
    let trimmed = pattern.trim();
    let anchored = trimmed.starts_with('/');
    let base = trimmed.trim_start_matches('/').trim_end_matches('/');
    if base.is_empty() {
        return Vec::new();
    }
    let mut globs = vec![base.to_owned()];
    if !base.ends_with("**") {
        globs.push(format!("{base}/**"));
    }
    if !anchored && !base.contains('/') {
        let deep: Vec<String> = globs.iter().map(|glob| format!("**/{glob}")).collect();
        globs.extend(deep);
    }
    globs
}

/// One concrete path a glob matches, such as `.github/x` for `.github/**`.
pub fn sample_path(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len());
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                } else {
                    out.push('x');
                }
            }
            '*' | '?' => out.push('x'),
            '[' => {
                let class: String = chars.by_ref().take_while(|&c| c != ']').collect();
                let first = class.chars().next();
                match first {
                    Some('!' | '^') | None => out.push('x'),
                    Some(first) => out.push(first),
                }
            }
            '{' => {
                let group: String = chars.by_ref().take_while(|&c| c != '}').collect();
                out.push_str(group.split(',').next().unwrap_or(""));
            }
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

pub fn check(card: &CardSpec, changes: &[FileChange], settings: &Settings) -> Vec<Violation> {
    let protected = PathRules::protected(settings);
    let forbidden = PathRules::new(card.forbidden_paths.iter().map(String::as_str));
    let touched = |rules: &PathRules| -> Vec<String> {
        let mut paths: Vec<String> = changes
            .iter()
            .flat_map(FileChange::paths)
            .filter(|path| rules.matches(path))
            .map(str::to_owned)
            .collect();
        paths.sort();
        paths.dedup();
        paths
    };

    let mut violations = Vec::new();
    let invalid: Vec<&str> = protected
        .invalid
        .iter()
        .chain(&forbidden.invalid)
        .map(String::as_str)
        .collect();
    if !invalid.is_empty() {
        violations.push(violation(
            HardLimit::ProtectedPath,
            format!(
                "these path patterns aren't valid globs, so what they protect can't be checked: {}",
                invalid.join(", ")
            ),
            Vec::new(),
        ));
    }
    let protected_touched = touched(&protected);
    if !protected_touched.is_empty() {
        violations.push(violation(
            HardLimit::ProtectedPath,
            "changes protected paths",
            protected_touched,
        ));
    }
    let forbidden_touched = touched(&forbidden);
    if !forbidden_touched.is_empty() {
        violations.push(violation(
            HardLimit::ProtectedPath,
            format!("changes paths {} may not touch", card.id),
            forbidden_touched,
        ));
    }
    let mut instructions: Vec<String> = changes
        .iter()
        .flat_map(FileChange::paths)
        .filter(|path| is_instruction_file(path))
        .map(str::to_owned)
        .collect();
    instructions.sort();
    instructions.dedup();
    if !instructions.is_empty() {
        violations.push(instruction_flag(instructions));
    }
    violations
}

/// Blocks auto-merge, not the card: the merge waits for you with these named.
pub fn instruction_flag(paths: Vec<String>) -> Violation {
    violation(
        HardLimit::InstructionFile,
        format!(
            "changes instruction files other agents read ({}), so the merge waits for you",
            paths.join(", ")
        ),
        paths,
    )
}
