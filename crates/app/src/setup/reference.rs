//! The reference copies in `~/.argonauts/templates/`: every built-in workflow and prompt, an
//! example instructions file for each role, and the starter mods, each part with its README.
//! They are written the first time Argonauts Code starts and again when another version does,
//! so they always say what this version runs. Nothing reads them back: a copy takes effect in
//! `.argonauts/` or `~/.argonauts/`.

use std::path::{Path, PathBuf};

use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::prompt_files::{self, PROMPTS_DIR, ROLES_DIR};
use argonauts_core::settings::mods::{MODS_DIR, builtin};
use argonauts_core::{layout, templates, workflow_files};

use crate::env::AppEnv;
use crate::prompts::catalog;

const README: &str = "README.md";

/// Holds the version that wrote the folder, so a start of the same version writes nothing.
const WRITTEN_BY: &str = ".written-by";

pub(crate) fn dir(env: &AppEnv) -> PathBuf {
    env.paths.home().join(templates::DIR)
}

/// Every file of the folder, by its path in it.
pub(crate) fn files() -> Vec<(PathBuf, String)> {
    let version = env!("CARGO_PKG_VERSION");
    let page = |text: &str| text.replace("{version}", version);
    let mut files = vec![(PathBuf::from(README), page(templates::README))];

    let workflows = Path::new(layout::user_files::WORKFLOWS);
    files.push((workflows.join(README), page(workflow_files::README)));
    for (name, text) in workflow_files::BUILT_IN {
        files.push((workflows.join(format!("{name}.toml")), (*text).to_owned()));
    }

    let prompts = Path::new(PROMPTS_DIR);
    let all = catalog::all();
    files.push((prompts.join(README), page(&prompt_files::readme(&all))));
    for template in &all {
        let file = prompts.join(format!("{}.md", template.id));
        files.push((file, template.text.to_owned()));
    }
    // The judge's rules until `.argonauts/approval-rules.md` holds yours.
    files.push((
        prompts.join("approvals/default-rules.md"),
        argonauts_crew::approvals::evidence::DEFAULT_RULES.to_owned(),
    ));

    let roles = Path::new(ROLES_DIR);
    files.push((roles.join(README), page(templates::ROLES_README)));
    for (role, text) in templates::ROLE_EXAMPLES {
        files.push((roles.join(format!("{role}.md")), (*text).to_owned()));
    }

    let mods = Path::new(MODS_DIR);
    files.push((mods.join(README), page(builtin::README)));
    for built_in in builtin::BUILTIN {
        let file = mods.join(format!("{}.toml", built_in.name));
        files.push((file, built_in.text.to_owned()));
    }
    for (name, text) in builtin::STARTERS {
        let file = mods.join(format!("{name}.toml"));
        if !files.iter().any(|(have, _)| *have == file) {
            files.push((file, (*text).to_owned()));
        }
    }
    for (_, file, program) in builtin::STARTER_PROGRAMS {
        files.push((mods.join(file), (*program).to_owned()));
    }
    files
}

/// Writes the folder unless this version already did. `Err` says what couldn't be written,
/// and the app goes on without it.
pub(crate) fn refresh(env: &AppEnv) -> Result<(), String> {
    let dir = dir(env);
    let version = env!("CARGO_PKG_VERSION");
    let stamp = dir.join(WRITTEN_BY);
    if std::fs::read_to_string(&stamp).is_ok_and(|by| by.trim() == version) {
        return Ok(());
    }
    let write = || -> std::io::Result<()> {
        for (file, text) in files() {
            let path = dir.join(file);
            if let Some(folder) = path.parent() {
                create_private_dir_all(folder)?;
            }
            write_private_file(&path, text.as_bytes())?;
        }
        // Last, so a write that stopped halfway is done again at the next start.
        write_private_file(&stamp, version.as_bytes())
    };
    write().map_err(|err| format!("Couldn't write {}: {err}", env.shown(&dir)))
}

#[cfg(test)]
mod tests {
    use argonauts_core::paths::Paths;
    use argonauts_testkit::TempRepo;

    use super::*;

    fn env(repo: &TempRepo) -> AppEnv {
        AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap()
    }

    /// The folder Argonauts Code is built from, in the core crate.
    fn source() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/templates")
    }

    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut folders = vec![dir.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    found.push(path.strip_prefix(dir).unwrap().to_path_buf());
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn the_copies_are_the_repositorys_templates_file_for_file() {
        let mut written: Vec<PathBuf> = files().into_iter().map(|(file, _)| file).collect();
        written.sort();
        assert_eq!(written, files_under(&source()));
        for (file, text) in files() {
            // A README says the version it was written by, and the prompts' lists its files.
            if file.ends_with(README) {
                assert!(!text.contains("{version}"), "{}", file.display());
                continue;
            }
            let built_from = std::fs::read_to_string(source().join(&file)).unwrap();
            assert_eq!(text, built_from, "{}", file.display());
        }
    }

    #[test]
    fn every_part_has_a_readme() {
        let files = files();
        for part in ["", "workflows", "prompts", "roles", "mods"] {
            let readme = Path::new(part).join(README);
            assert!(
                files
                    .iter()
                    .any(|(file, text)| *file == readme && !text.trim().is_empty()),
                "{}",
                readme.display()
            );
        }
    }

    #[test]
    fn the_first_start_writes_them_and_another_version_writes_them_again() {
        let repo = TempRepo::new().unwrap();
        let env = env(&repo);
        let dir = dir(&env);
        refresh(&env).unwrap();
        let council = dir.join("workflows/council.toml");
        assert_eq!(
            std::fs::read_to_string(&council).unwrap(),
            workflow_files::built_in("council").unwrap()
        );
        assert!(dir.join("prompts/frames/reviewer.md").is_file());
        assert!(dir.join("roles/reviewer.md").is_file());
        assert!(dir.join("mods/checks.sh").is_file());

        // The same version leaves the folder alone, even a file someone changed there.
        std::fs::write(&council, "changed").unwrap();
        refresh(&env).unwrap();
        assert_eq!(std::fs::read_to_string(&council).unwrap(), "changed");

        // Another version's start brings every file back to what it runs.
        std::fs::write(dir.join(WRITTEN_BY), "0.0.0").unwrap();
        refresh(&env).unwrap();
        assert_eq!(
            std::fs::read_to_string(&council).unwrap(),
            workflow_files::built_in("council").unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(WRITTEN_BY)).unwrap(),
            env!("CARGO_PKG_VERSION")
        );
    }
}
