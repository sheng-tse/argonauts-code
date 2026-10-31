//! The repo's files and folders for the `@` list, and how what you type picks among them.

use std::collections::BTreeSet;

/// More would only scroll; a few more letters narrow the list faster.
const MAX_MATCHES: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoFiles {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// Relative, with `/` between folders, a folder's ending in `/`.
    path: String,
    /// Where the last part of the path starts, as a byte offset.
    name_at: usize,
    depth: usize,
}

impl Entry {
    fn new(path: String) -> Self {
        let bare = path.trim_end_matches('/');
        let name_at = bare.rfind('/').map_or(0, |at| at + 1);
        let depth = bare.matches('/').count();
        Self {
            path,
            name_at,
            depth,
        }
    }

    fn is_folder(&self) -> bool {
        self.path.ends_with('/')
    }
}

/// A path that matched, and the byte offsets of the letters that matched, to show in bold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch<'a> {
    pub path: &'a str,
    pub letters: Vec<usize>,
}

impl RepoFiles {
    /// From the files git lists, adding every folder that holds one.
    pub fn new<S: AsRef<str>>(files: &[S]) -> Self {
        let mut paths: BTreeSet<String> = BTreeSet::new();
        for file in files {
            let file = file.as_ref().trim_matches('/');
            if file.is_empty() {
                continue;
            }
            for (at, _) in file.match_indices('/') {
                paths.insert(format!("{}/", &file[..at]));
            }
            paths.insert(file.to_owned());
        }
        Self {
            entries: paths.into_iter().map(Entry::new).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `name`, as written after an `@`, is one of the files or folders. A folder may
    /// be written with or without its `/`.
    pub fn contains(&self, name: &str) -> bool {
        let name = name.trim_start_matches("./");
        let find = |path: &str| {
            self.entries
                .binary_search_by(|e| e.path.as_str().cmp(path))
                .is_ok()
        };
        !name.is_empty() && (find(name) || (!name.ends_with('/') && find(&format!("{name}/"))))
    }

    /// With nothing typed, the repo's top level, files first, as Claude Code lists it. Typing
    /// matches anywhere in the path, best first: a name that starts with what you typed, or
    /// the path when you typed one from the top, then a word in the path that does, then the
    /// letters together anywhere, then the letters in order with gaps. Within each, shallower and
    /// shorter paths come first. A folder you've completed lists what's in it, not itself.
    pub fn matches(&self, query: &str, limit: usize) -> Vec<FileMatch<'_>> {
        let limit = limit.min(MAX_MATCHES);
        if query.is_empty() {
            let mut top: Vec<&Entry> = self.entries.iter().filter(|e| e.depth == 0).collect();
            top.sort_by_cached_key(|e| (e.is_folder(), e.path.to_lowercase()));
            return top
                .into_iter()
                .take(limit)
                .map(|e| FileMatch {
                    path: &e.path,
                    letters: Vec::new(),
                })
                .collect();
        }
        let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        let mut found: Vec<(u8, &Entry, Vec<usize>)> = self
            .entries
            .iter()
            .filter(|e| has_in_order(&e.path, &query))
            .filter(|e| !(e.is_folder() && equal(&e.path, &query)))
            .filter_map(|e| {
                let (tier, letters) = match_path(e, &query)?;
                Some((tier, e, letters))
            })
            .collect();
        found.sort_by_cached_key(|(tier, e, _)| {
            (
                *tier,
                e.depth,
                e.path.chars().count(),
                e.path.to_lowercase(),
            )
        });
        found
            .into_iter()
            .take(limit)
            .map(|(_, e, letters)| FileMatch {
                path: &e.path,
                letters,
            })
            .collect()
    }
}

fn same(c: char, q: char) -> bool {
    c.to_lowercase().eq(std::iter::once(q))
}

/// Every way to match needs the letters in order, so this cheap pass, with nothing to
/// allocate, drops most of a big repo before the ranking looks closer.
fn has_in_order(path: &str, query: &[char]) -> bool {
    let mut wanted = query.iter().peekable();
    for c in path.chars() {
        if wanted.next_if(|&&q| same(c, q)).is_some() && wanted.peek().is_none() {
            return true;
        }
    }
    wanted.peek().is_none()
}

fn equal(path: &str, query: &[char]) -> bool {
    path.chars().count() == query.len() && path.chars().zip(query).all(|(c, &q)| same(c, q))
}

fn starts_word(chars: &[(usize, char)], i: usize) -> bool {
    i == 0 || matches!(chars[i - 1].1, '/' | '-' | '_' | '.' | ' ')
}

/// The tier of the best way `query` (lowercase) matches the entry, and its letters.
fn match_path(entry: &Entry, query: &[char]) -> Option<(u8, Vec<usize>)> {
    let chars: Vec<(usize, char)> = entry.path.char_indices().collect();
    let run_at = |from: usize| -> Option<Vec<usize>> {
        let rest = chars.get(from..)?;
        (rest.len() >= query.len() && rest.iter().zip(query).all(|(&(_, c), &q)| same(c, q)))
            .then(|| rest[..query.len()].iter().map(|&(at, _)| at).collect())
    };
    let name = chars
        .iter()
        .position(|&(at, _)| at == entry.name_at)
        .unwrap_or(0);
    // A path typed from the top, such as `src/ma`, is as good as a name; a single word that
    // only starts the top folder, such as `c` in `crates/core/x.rs`, is a word like any other.
    let from_top = || run_at(0).filter(|_| query.contains(&'/'));
    if let Some(letters) = run_at(name).or_else(from_top) {
        return Some((0, letters));
    }
    let words = (0..chars.len()).filter(|&i| starts_word(&chars, i));
    for start in words {
        if let Some(letters) = run_at(start) {
            return Some((1, letters));
        }
    }
    for start in 1..chars.len() {
        if let Some(letters) = run_at(start) {
            return Some((2, letters));
        }
    }
    // Letters in order with gaps, each taken as late as it can be so the match leans on the
    // file's own name rather than the folders above it.
    let mut letters = Vec::with_capacity(query.len());
    let mut next = chars.len();
    for &q in query.iter().rev() {
        let at = chars[..next].iter().rposition(|&(_, c)| same(c, q))?;
        letters.push(chars[at].0);
        next = at;
    }
    letters.reverse();
    Some((3, letters))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> RepoFiles {
        RepoFiles::new(&[
            "README.md",
            "Cargo.toml",
            ".gitignore",
            "src/main.rs",
            "src/menu.rs",
            "src/util/mod.rs",
            "docs/manual.md",
            "tests/main_test.rs",
        ])
    }

    fn paths(files: &RepoFiles, query: &str) -> Vec<String> {
        files
            .matches(query, 20)
            .into_iter()
            .map(|m| m.path.to_owned())
            .collect()
    }

    #[test]
    fn folders_come_from_the_files_in_them() {
        let all = paths(&files(), "/");
        assert!(all.contains(&"src/".to_owned()), "{all:?}");
        assert!(all.contains(&"src/util/".to_owned()), "{all:?}");
        assert_eq!(RepoFiles::new(&["a/b/c.rs"]).entries.len(), 3);
        assert!(RepoFiles::new::<&str>(&[]).is_empty());
    }

    #[test]
    fn a_name_after_an_at_sign_is_found_with_or_without_a_folders_slash() {
        let files = files();
        assert!(files.contains("src/main.rs"));
        assert!(files.contains("./README.md"));
        assert!(files.contains("src/util"));
        assert!(files.contains("src/util/"));
        assert!(!files.contains("src/main.rs/"));
        assert!(!files.contains("codex"));
        assert!(!files.contains(""));
    }

    #[test]
    fn nothing_typed_lists_the_top_level_files_first() {
        assert_eq!(
            paths(&files(), ""),
            [
                ".gitignore",
                "Cargo.toml",
                "README.md",
                "docs/",
                "src/",
                "tests/"
            ]
        );
    }

    #[test]
    fn a_name_that_starts_with_the_query_comes_first_then_shallower_paths() {
        let found = paths(&files(), "ma");
        assert_eq!(
            found[..3],
            ["src/main.rs", "docs/manual.md", "tests/main_test.rs"]
        );
        assert_eq!(paths(&files(), "MENU"), ["src/menu.rs"]);
    }

    #[test]
    fn a_path_prefix_lists_what_a_folder_holds_and_not_the_folder() {
        assert_eq!(
            paths(&files(), "src/"),
            ["src/util/", "src/main.rs", "src/menu.rs", "src/util/mod.rs"]
        );
        // `src/menu.rs` has the letters in order too, so it follows.
        assert_eq!(
            paths(&files(), "src/u"),
            ["src/util/", "src/util/mod.rs", "src/menu.rs"]
        );
    }

    #[test]
    fn a_word_that_only_starts_the_top_folder_ranks_below_names() {
        let files = RepoFiles::new(&["crates/core/lib.rs", "crates/tui/x.rs", "Cargo.toml"]);
        assert_eq!(
            paths(&files, "c")[..3],
            ["crates/", "Cargo.toml", "crates/core/"]
        );
    }

    #[test]
    fn then_a_word_then_letters_together_then_letters_in_order() {
        let found = paths(&files(), "test");
        assert_eq!(found[0], "tests/");
        assert!(found.contains(&"tests/main_test.rs".to_owned()));
        // `ain` sits inside `main` with no word starting there.
        assert_eq!(
            paths(&files(), "ain")[..2],
            ["src/main.rs", "tests/main_test.rs"]
        );
        // Letters in order: s, m, then r, across folders.
        let fuzzy = paths(&files(), "smr");
        assert!(fuzzy.contains(&"src/main.rs".to_owned()), "{fuzzy:?}");
        assert!(paths(&files(), "zzz").is_empty());
    }

    #[test]
    fn matched_letters_are_byte_offsets_in_the_path() {
        let files = files();
        let found = files.matches("mod", 5);
        assert_eq!(found[0].path, "src/util/mod.rs");
        assert_eq!(found[0].letters, [9, 10, 11]);
        let fuzzy = files.matches("umr", 5);
        let m = fuzzy.iter().find(|m| m.path == "src/util/mod.rs").unwrap();
        assert_eq!(m.letters, [4, 9, 13]);
    }

    #[test]
    fn the_list_stops_at_the_limit() {
        let many: Vec<String> = (0..300).map(|n| format!("f{n}.txt")).collect();
        let files = RepoFiles::new(&many);
        assert_eq!(files.matches("f", 1000).len(), MAX_MATCHES);
        assert_eq!(files.matches("f", 5).len(), 5);
    }
}
