//! The `gh` a folder's GitHub calls go to.

use std::path::Path;

use argonauts_conductor::gh::{Gh, GhError};

/// The `gh` on `PATH`. In this crate's own tests it is only ever the fake a test put in place
/// for its folder, so no test of the app can reach GitHub.
pub(crate) fn gh_for(dir: &Path) -> Result<Gh, GhError> {
    #[cfg(test)]
    return fakes::for_folder(dir);
    #[cfg(not(test))]
    Gh::find(dir)
}

#[cfg(test)]
pub(crate) mod fakes {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use argonauts_conductor::gh::{Gh, GhError};

    static FAKES: Mutex<BTreeMap<PathBuf, Gh>> = Mutex::new(BTreeMap::new());

    /// `gh` for the folder `dir` and everything under it.
    pub(crate) fn put(dir: &Path, gh: Gh) {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_owned());
        FAKES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(dir, gh);
    }

    pub(super) fn for_folder(dir: &Path) -> Result<Gh, GhError> {
        let fakes = FAKES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        dir.ancestors()
            .find_map(|folder| fakes.get(folder).cloned())
            .ok_or(GhError::NotInstalled)
    }
}
