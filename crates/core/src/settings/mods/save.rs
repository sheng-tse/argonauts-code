//! Saving a whole mod file, for when the main session writes the mod you asked it for. The
//! text is checked first, so nothing is written that `/mods` would then leave out.

use std::io;
use std::path::{Path, PathBuf};

use super::{check, is_mod_name};
use crate::paths::{create_private_dir_all, write_private_file};

/// `<mods_dir>/<name>.toml`, the file of the mod `name`.
pub fn path_of(mods_dir: &Path, name: &str) -> PathBuf {
    mods_dir.join(format!("{name}.toml"))
}

/// Saves `text` as the mod `name` in `mods_dir`, in place of any file it had, and says
/// where. `mods_dir` is `~/.argonauts/mods` for a mod of yours, or the project's
/// `.argonauts/mods`. A text that wouldn't load is never written: every problem comes back,
/// in the words `/mods` and Doctor use, so whoever wrote it can fix it.
///
/// A project's mod that runs a command still waits for you to trust it on its page.
pub fn save(mods_dir: &Path, name: &str, text: &str) -> Result<PathBuf, Vec<String>> {
    if !is_mod_name(name) {
        return Err(vec![format!(
            "the name {name:?} won't do; use lowercase letters, digits, - and _, starting with \
             a letter"
        )]);
    }
    let problems = check::problems(name, text);
    if !problems.is_empty() {
        return Err(problems);
    }
    let path = path_of(mods_dir, name);
    write_whole(&path, text).map_err(|err| vec![format!("it couldn't be written: {err}")])?;
    Ok(path)
}

/// Writes through a draft beside the file, so a reader never sees half of it.
pub fn write_whole(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    let draft = path.with_extension("toml.saving");
    write_private_file(&draft, text.as_bytes())?;
    std::fs::rename(&draft, path)
}

#[cfg(test)]
mod tests {
    use super::super::{ModSet, builtin};
    use super::*;

    const BUILD: &str = "name = \"build\"\nbody = \"meters\"\nplaces = [\"above\"]\n\n\
                         [data]\ncommand = [\"./build.sh\"]\nevery = \"30s\"\n";

    #[test]
    fn a_mod_that_loads_is_saved_where_the_loader_finds_it() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("mods");
        let path = save(&dir, "build", BUILD).unwrap();
        assert_eq!(path, dir.join("build.toml"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BUILD);
        let set = ModSet::load(Some(&dir), None);
        assert!(set.get("build").is_some());
        assert!(set.problems.is_empty(), "{:?}", set.problems);
        // No draft is left beside it.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn a_mod_that_wouldnt_load_is_never_written_and_says_why() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("mods");
        let problems = save(&dir, "build", "name = \"build\"\nbody = \"pie\"\n").unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("\"pie\"")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("[data]")),
            "{problems:?}"
        );
        assert!(!dir.exists());
        // The file you had stays as it was.
        save(&dir, "build", BUILD).unwrap();
        save(&dir, "build", "body = ").unwrap_err();
        assert_eq!(
            std::fs::read_to_string(dir.join("build.toml")).unwrap(),
            BUILD
        );
    }

    #[test]
    fn a_name_that_cant_name_a_mod_is_refused_before_anything_is_touched() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("mods");
        for name in ["../build", "Build", "", "a/b", "mods"] {
            let text = BUILD.replace("build", name);
            assert!(save(&dir, name, &text).is_err(), "{name:?}");
        }
        assert!(!dir.exists());
        assert!(!home.path().join("build.toml").exists());
    }

    #[test]
    fn the_bottom_line_s_file_is_checked_by_its_own_reader() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("mods");
        let problems = save(&dir, builtin::BOTTOM_LINE, "form = \"round\"\n").unwrap_err();
        assert_eq!(problems.len(), 1, "{problems:?}");
        let shipped = builtin::builtin(builtin::BOTTOM_LINE).unwrap().text;
        let path = save(&dir, builtin::BOTTOM_LINE, shipped).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), shipped);
    }
}
