//! Where the history of a folder with no git of its own is kept, and whether it's in use.
//!
//! That history is a git repository in the folder's state under Argonauts' home, with your
//! folder as its work tree. Every git call for the folder names both, so the folder gets no
//! `.git` and nothing in it points here. The app makes and records it; the engine only needs
//! to know where it is.

use std::path::{Path, PathBuf};

use crate::layout::repo_state;

/// In the history's folder while it's turned off. The history itself stays, so turning it
/// back on loses nothing.
pub const OFF: &str = "argonauts-off";

/// Where the history of the project with this state folder is kept.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join(repo_state::HIDDEN_GIT)
}

/// Whether the history at `dir` exists and is in use.
pub fn is_on(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && !dir.join(OFF).exists()
}

/// The hidden history `folder` works with: none when it has a `.git` of its own, or keeps no
/// history at all.
pub fn of(folder: &Path, state_dir: &Path) -> Option<PathBuf> {
    let hidden = dir(state_dir);
    (!folder.join(".git").exists() && is_on(&hidden)).then_some(hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_uses_its_hidden_history_only_while_it_is_on_and_it_has_no_git() {
        let folder = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        assert_eq!(of(folder.path(), state.path()), None);

        let hidden = dir(state.path());
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(of(folder.path(), state.path()), Some(hidden.clone()));

        std::fs::write(hidden.join(OFF), "").unwrap();
        assert_eq!(of(folder.path(), state.path()), None);
        std::fs::remove_file(hidden.join(OFF)).unwrap();

        std::fs::create_dir(folder.path().join(".git")).unwrap();
        assert_eq!(of(folder.path(), state.path()), None);
    }
}
