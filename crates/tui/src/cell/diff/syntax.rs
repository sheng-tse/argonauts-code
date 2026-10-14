//! Syntax colours for a diff's rows, by the file's language. As Claude Code colours an edit, one
//! highlighter reads each hunk's context and added rows in order, which is the new file's text,
//! and a removed row stays plain.
//!
//! A live agent block redraws all its diffs every frame, so the roles are kept for the last few
//! hunks. They don't depend on the theme, which picks the colours when the rows are drawn.

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use super::{DiffKind, DiffLine};
use crate::markdown::highlight::LineHighlighter;
use crate::text::sanitize_line;
use crate::theme::Role;

/// In hunks.
const CACHE_SIZE: usize = 64;

/// Each row's roles as byte ranges into its sanitized text, indexed like the hunk's lines. A row
/// with none is drawn plain.
pub(super) type HunkRoles = Arc<Vec<Vec<(Range<usize>, Role)>>>;

/// `file` names the changed file, whose name picks the language. `None`, or a language with no
/// grammar, leaves every row plain.
pub(super) fn hunk_roles(lines: &[DiffLine], file: Option<&str>) -> HunkRoles {
    let Some(file) = file else {
        return Arc::new(vec![Vec::new(); lines.len()]);
    };
    let key = hunk_key(lines, file);
    if let Some(hit) = cached(key) {
        return hit;
    }
    let roles = Arc::new(compute(lines, file));
    remember(key, roles.clone());
    roles
}

fn compute(lines: &[DiffLine], file: &str) -> Vec<Vec<(Range<usize>, Role)>> {
    // A shebang names the language of a file without an extension, but only the file's own
    // first line can carry one.
    let first = lines
        .iter()
        .find(|line| line.kind != DiffKind::Removed && line.new_line == Some(1))
        .map(|line| sanitize_line(&line.text));
    let mut new = LineHighlighter::for_file(file, first.as_deref());
    if !new.colors() {
        return vec![Vec::new(); lines.len()];
    }
    // The old file's lines, context and removed in order, go through a highlighter of their
    // own, so a removed line is coloured as the code it was.
    let old_first = lines
        .iter()
        .find(|line| line.kind != DiffKind::Added && line.old_line == Some(1))
        .map(|line| sanitize_line(&line.text));
    let mut old = LineHighlighter::for_file(file, old_first.as_deref().or(first.as_deref()));
    lines
        .iter()
        .map(|line| {
            let text = sanitize_line(&line.text);
            match line.kind {
                DiffKind::Removed => old.roles(&text),
                DiffKind::Added => new.roles(&text),
                DiffKind::Context => {
                    old.roles(&text);
                    new.roles(&text)
                }
            }
        })
        .collect()
}

fn hunk_key(lines: &[DiffLine], file: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    file.hash(&mut hasher);
    for line in lines {
        line.kind.hash(&mut hasher);
        line.new_line.hash(&mut hasher);
        line.text.hash(&mut hasher);
    }
    hasher.finish()
}

fn cache() -> &'static Mutex<VecDeque<(u64, HunkRoles)>> {
    static CACHE: OnceLock<Mutex<VecDeque<(u64, HunkRoles)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(CACHE_SIZE)))
}

fn cached(key: u64) -> Option<HunkRoles> {
    let mut entries = cache().lock().ok()?;
    let index = entries.iter().position(|(k, _)| *k == key)?;
    // Move the hit to the back, so the least recently used entry goes first.
    let entry = entries.remove(index)?;
    let hit = entry.1.clone();
    entries.push_back(entry);
    Some(hit)
}

fn remember(key: u64, roles: HunkRoles) {
    let Ok(mut entries) = cache().lock() else {
        return;
    };
    if entries.len() >= CACHE_SIZE {
        entries.pop_front();
    }
    entries.push_back((key, roles));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: DiffKind, new_line: Option<u32>, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old_line: None,
            new_line,
            text: text.to_owned(),
        }
    }

    fn roles_of(roles: &[(Range<usize>, Role)], text: &str) -> Vec<(String, Role)> {
        roles
            .iter()
            .map(|(range, role)| (text[range.clone()].to_owned(), *role))
            .collect()
    }

    #[test]
    fn removed_rows_take_the_language_as_added_and_context_rows_do() {
        let lines = [
            line(DiffKind::Context, Some(4), "fn main() {"),
            line(DiffKind::Removed, None, "    let x = 1;"),
            line(DiffKind::Added, Some(5), "    let x: u32 = 2;"),
        ];
        let roles = hunk_roles(&lines, Some("src/main.rs"));
        let removed = roles_of(&roles[1], &lines[1].text);
        assert!(
            removed.contains(&("let".to_owned(), Role::SyntaxKeyword)),
            "{removed:?}"
        );
        let added = roles_of(&roles[2], &lines[2].text);
        assert!(
            added.contains(&("let".to_owned(), Role::SyntaxKeyword)),
            "{added:?}"
        );
        assert!(roles[0].iter().any(|(_, role)| *role != Role::Text));
        assert_eq!(hunk_roles(&lines, Some("src/main.rs")), roles);
    }

    /// A function deleted whole, as an edit with nothing put back is, is still drawn in its
    /// language's colours.
    #[test]
    fn a_hunk_of_only_removed_lines_is_coloured() {
        let lines = [
            line(DiffKind::Removed, None, "fn old() -> u32 {"),
            line(DiffKind::Removed, None, "    7"),
            line(DiffKind::Removed, None, "}"),
        ];
        let roles = hunk_roles(&lines, Some("src/lib.rs"));
        let first = roles_of(&roles[0], &lines[0].text);
        assert!(
            first.contains(&("fn".to_owned(), Role::SyntaxKeyword)),
            "{first:?}"
        );
    }

    #[test]
    fn an_unknown_language_or_no_file_stays_plain() {
        let lines = [line(DiffKind::Added, Some(1), "let x = 1;")];
        assert!(
            hunk_roles(&lines, Some("notes.unknownext"))
                .iter()
                .all(Vec::is_empty)
        );
        assert!(hunk_roles(&lines, None).iter().all(Vec::is_empty));
    }

    #[test]
    fn a_shebang_names_the_language_of_a_file_without_an_extension() {
        let lines = [
            line(DiffKind::Added, Some(1), "#!/bin/bash"),
            line(DiffKind::Added, Some(2), "echo \"hi\""),
        ];
        let roles = hunk_roles(&lines, Some("bin/run"));
        assert!(
            roles[1].iter().any(|(_, role)| *role != Role::Text),
            "{roles:?}"
        );
    }
}
