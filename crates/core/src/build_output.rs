//! Files that building or testing leaves in a worktree: compiled bytecode, object files, and
//! the caches test tools write. Nobody means to commit them, so the engine's own commits leave
//! them out while they're untracked, and the secret scan reads them as text, since most are
//! binary and git shows no lines for a binary file.

/// Folders that hold only build output, at any depth.
const FOLDERS: &[&str] = &["__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache"];
/// File extensions of build output, without the dot.
const EXTENSIONS: &[&str] = &["pyc", "pyo", "class", "o", "obj"];

/// Whether `path`, relative to the repo with `/` between folders, is build output. Case is
/// ignored, as on the file systems where it matters.
pub fn is_build_output(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('/').filter(|part| !part.is_empty()).collect();
    let Some(name) = parts.pop() else {
        return false;
    };
    parts.iter().any(|folder| FOLDERS.contains(folder))
        || name
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && EXTENSIONS.contains(&ext))
}

/// The same files as git pathspecs, for a `git diff` or `git add` that picks them out, or
/// leaves them out when `exclude` is true.
pub fn pathspecs(exclude: bool) -> Vec<String> {
    let magic = if exclude {
        "exclude,glob,icase"
    } else {
        "glob,icase"
    };
    let folders = FOLDERS.iter().map(|folder| format!("**/{folder}/**"));
    // `?` keeps a bare `.o` out, as `is_build_output` does.
    let files = EXTENSIONS.iter().map(|ext| format!("**/?*.{ext}"));
    folders
        .chain(files)
        .map(|pattern| format!(":({magic}){pattern}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_objects_and_tool_caches_are_build_output() {
        for path in [
            "__pycache__/core.cpython-312.pyc",
            "stats/__pycache__/core.cpython-312.pyc",
            "legacy/core.pyc",
            "core.PYO",
            "build/Main.class",
            "src/lib.o",
            "win/lib.obj",
            ".pytest_cache/v/cache/nodeids",
            "pkg/.mypy_cache/3.12/core.data.json",
            ".ruff_cache/0.6.0/123456",
        ] {
            assert!(is_build_output(path), "{path}");
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
            "",
        ] {
            assert!(!is_build_output(path), "{path:?}");
        }
    }

    #[test]
    fn pathspecs_pick_or_leave_out_the_same_files() {
        let picked = pathspecs(false);
        let left_out = pathspecs(true);
        assert_eq!(picked.len(), left_out.len());
        assert!(picked.contains(&":(glob,icase)**/__pycache__/**".to_owned()));
        assert!(left_out.contains(&":(exclude,glob,icase)**/?*.pyc".to_owned()));
    }
}
