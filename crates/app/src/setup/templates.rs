//! What `/setup` writes beside your answers, so every place you can change explains itself: a
//! `README.md` in `.argonauts/` and in `workflows/`, `roles/` and `mods/`, and the starter
//! mods. A README is Argonauts Code's own page and is written again when a newer version has more
//! to say, until you make it yours by deleting its first line. A starter mod is yours from the
//! start and is never written twice, and neither is the program one of them runs.

use std::path::{Path, PathBuf};

use argonauts_core::NoteLevel;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::settings::mods::{MODS_DIR, builtin};

use crate::env::AppEnv;
use crate::mods::files;

/// How a README Argonauts Code wrote starts. A file without this line is yours and is left alone.
const WRITTEN_BY: &str = "<!-- Written by Argonauts Code";

/// A page of the guide: the folder it's written in, and its text.
struct Page {
    /// The topic, as the main session asks for it.
    topic: &'static str,
    /// The folder under `.argonauts/`. Empty for `.argonauts/` itself.
    folder: &'static str,
    text: &'static str,
}

const PAGES: [Page; 4] = [
    Page {
        topic: "argonauts",
        folder: "",
        text: include_str!("readme/argonauts.md"),
    },
    // The workflows' own page, which `/workflow new` writes beside a new workflow as well.
    Page {
        topic: "workflows",
        folder: "workflows",
        text: argonauts_runner::README,
    },
    Page {
        topic: "roles",
        folder: "roles",
        text: argonauts_core::template!("roles/README.md"),
    },
    Page {
        topic: "mods",
        folder: MODS_DIR,
        text: builtin::README,
    },
];

const README: &str = "README.md";

/// A page as it's written: its first line says who wrote it and how to keep your edits, and
/// `{version}` is this version.
fn written(text: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        "{WRITTEN_BY} {version}. Delete this line to make the page yours: /setup then leaves \
         it alone. -->\n{}",
        text.replace("{version}", version)
    )
}

/// The guide's page on `topic`, as `/setup` writes it: `argonauts`, `workflows`, `roles` or
/// `mods`. The main session's guide tool opens it where `/setup` hasn't written it yet.
pub(crate) fn guide(topic: &str) -> Option<String> {
    PAGES
        .iter()
        .find(|page| page.topic == topic)
        .map(|page| written(page.text))
}

/// The topics [`guide`] knows.
#[cfg(test)]
pub(crate) fn guide_topics() -> Vec<&'static str> {
    PAGES.iter().map(|page| page.topic).collect()
}

/// Writes one README unless the file there is yours.
fn write_readme(dir: &Path, text: &str) -> std::io::Result<bool> {
    let path = dir.join(README);
    match std::fs::read_to_string(&path) {
        Ok(have) if !have.starts_with(WRITTEN_BY) => return Ok(false),
        Ok(have) if have == text => return Ok(false),
        _ => {}
    }
    create_private_dir_all(dir)?;
    write_private_file(&path, text.as_bytes())?;
    Ok(true)
}

/// Writes the four READMEs under `root`, an `.argonauts` folder, and returns those it wrote.
fn write_readmes(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut wrote = Vec::new();
    for page in &PAGES {
        let dir = root.join(page.folder);
        if write_readme(&dir, &written(page.text))? {
            wrote.push(dir.join(README));
        }
    }
    Ok(wrote)
}

/// Writes each starter mod that's missing into `dir`. A file you have is never touched.
fn write_starters(dir: &Path) -> anyhow::Result<()> {
    let written = builtin::STARTERS
        .iter()
        .filter(|(name, _)| builtin::is_written_by_setup(name));
    for (name, text) in written {
        let path = files::mod_path(dir, name);
        if path.exists() {
            continue;
        }
        files::write(&path, text)?;
        // The program goes in with its mod and never later: once the mod is yours, so is
        // what it runs, and a program you deleted stays deleted.
        for (_, file, program) in builtin::STARTER_PROGRAMS
            .iter()
            .filter(|(of, _, _)| of == name)
        {
            write_program(&dir.join(file), program)?;
        }
    }
    Ok(())
}

/// Writes a program you can run, unless one is there.
fn write_program(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if path.exists() {
        return Ok(());
    }
    write_private_file(path, text.as_bytes())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// What writing the setup's pages did: what failed, as notices, since the setup goes on
/// without them, and the project's pages it wrote.
#[derive(Debug, Default)]
pub(crate) struct Written {
    pub notices: Vec<(NoteLevel, String)>,
    pub in_project: Vec<PathBuf>,
}

/// Writes the READMEs and the starter mods into `~/.argonauts/`, and the READMEs into the
/// project's `.argonauts/` when it has one and you said yes to them there.
pub(crate) fn write(env: &AppEnv, in_project: bool) -> Written {
    let mut notices = Vec::new();
    let home = env.paths.home();
    let mut warn = |what: &str, err: &dyn std::fmt::Display| {
        notices.push((NoteLevel::Warn, format!("Couldn't write {what}: {err}")));
    };
    if let Err(err) = write_readmes(home) {
        warn("the READMEs", &err);
    }
    if let Err(err) = write_starters(&home.join(MODS_DIR)) {
        warn("the starter mods", &format!("{err:#}"));
    }
    let project = super::finish::project_dir(env);
    let mut wrote = Vec::new();
    if in_project && project.is_dir() {
        match write_readmes(&project) {
            Ok(pages) => wrote = pages,
            Err(err) => warn("the project's READMEs", &err),
        }
    }
    Written {
        notices,
        in_project: wrote,
    }
}

/// Where a repo keeps whether you want the guide pages written into its `.argonauts/`.
const GUIDES_ANSWER: &str = "guide-pages";

/// Your answer about the guide pages in this repo, once you gave one.
pub(crate) fn guides_answer(env: &AppEnv) -> Option<bool> {
    let text = std::fs::read_to_string(env.state_dir.join(GUIDES_ANSWER)).ok()?;
    match text.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

pub(crate) fn keep_guides_answer(env: &AppEnv, yes: bool) -> std::io::Result<()> {
    create_private_dir_all(&env.state_dir)?;
    let answer = if yes { "yes\n" } else { "no\n" };
    write_private_file(&env.state_dir.join(GUIDES_ANSWER), answer.as_bytes())
}

#[cfg(test)]
mod tests {
    use argonauts_core::paths::Paths;
    use argonauts_testkit::TempRepo;

    use super::*;

    fn env(repo: &TempRepo) -> AppEnv {
        AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap()
    }

    fn files_under(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut folders = vec![dir.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    found.push(path.strip_prefix(dir).unwrap().display().to_string());
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn setup_writes_a_readme_in_every_place_you_can_change_and_four_starter_mods() {
        let repo = TempRepo::new().unwrap();
        let env = env(&repo);
        assert!(write(&env, false).notices.is_empty());
        let written: Vec<String> = files_under(repo.home())
            .into_iter()
            .filter(|path| path.ends_with(".md") || path.starts_with("mods/"))
            .collect();
        assert_eq!(
            written,
            [
                "README.md",
                "mods/README.md",
                "mods/bottom-line.toml",
                "mods/branch.toml",
                "mods/checks.sh",
                "mods/checks.toml",
                "mods/stats.toml",
                "roles/README.md",
                "workflows/README.md",
            ]
        );
        // The project has no folder yet, so nothing is put in the repository.
        assert!(!repo.root().join(".argonauts").exists());
    }

    #[test]
    fn a_starter_s_program_can_run_and_is_written_once_with_its_mod() {
        use std::os::unix::fs::PermissionsExt as _;
        let repo = TempRepo::new().unwrap();
        let env = env(&repo);
        write(&env, false);
        let program = repo.home().join("mods/checks.sh");
        let mode = std::fs::metadata(&program).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // Deleted, it stays deleted: the mod is yours by now.
        std::fs::remove_file(&program).unwrap();
        write(&env, false);
        assert!(!program.exists());
        // And yours is never written over when the mod comes back.
        std::fs::write(&program, "#!/bin/sh\necho mine\n").unwrap();
        std::fs::remove_file(repo.home().join("mods/checks.toml")).unwrap();
        write(&env, false);
        assert!(repo.home().join("mods/checks.toml").exists());
        assert_eq!(
            std::fs::read_to_string(&program).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
    }

    #[test]
    fn a_project_gets_the_readmes_only_after_a_yes_and_no_mods() {
        let repo = TempRepo::new().unwrap();
        let dot = repo.root().join(".argonauts");
        std::fs::create_dir_all(&dot).unwrap();
        let env = env(&repo);
        let not_here = write(&env, false);
        assert!(not_here.notices.is_empty() && not_here.in_project.is_empty());
        assert!(files_under(&dot).is_empty());

        let written = write(&env, true);
        assert!(written.notices.is_empty());
        assert_eq!(written.in_project.len(), 4);
        assert_eq!(
            files_under(&dot),
            [
                "README.md",
                "mods/README.md",
                "roles/README.md",
                "workflows/README.md"
            ]
        );
        // Once written, the pages are said only when they change.
        assert!(write(&env, true).in_project.is_empty());

        assert_eq!(guides_answer(&env), None);
        keep_guides_answer(&env, true).unwrap();
        assert_eq!(guides_answer(&env), Some(true));
    }

    #[test]
    fn a_readme_says_its_version_and_every_page_ends_with_it() {
        let version = env!("CARGO_PKG_VERSION");
        for topic in guide_topics() {
            let page = guide(topic).unwrap();
            assert!(
                page.starts_with(&format!("{WRITTEN_BY} {version}.")),
                "{topic}"
            );
            assert!(!page.contains("{version}"), "{topic}");
            // The workflows page is the runner's, which `/workflow new` writes too.
            if topic != "workflows" {
                assert!(
                    page.trim_end()
                        .ends_with(&format!("those of Argonauts Code {version}.")),
                    "{topic}"
                );
            }
        }
        assert_eq!(guide_topics(), ["argonauts", "workflows", "roles", "mods"]);
        assert!(guide("limits").is_none());
    }

    #[test]
    fn a_page_you_made_yours_is_left_alone_and_argonauts_own_is_written_again() {
        let repo = TempRepo::new().unwrap();
        let env = env(&repo);
        write(&env, false);
        let mods = repo.home().join("mods/README.md");
        let roles = repo.home().join("roles/README.md");
        // Yours: the first line is gone.
        std::fs::write(&mods, "# My notes on mods\n").unwrap();
        // Argonauts Code's: an older version's page.
        std::fs::write(
            &roles,
            format!("{WRITTEN_BY} 0.0.0. -->\n# Roles, as they were\n"),
        )
        .unwrap();
        std::fs::write(repo.home().join("mods/checks.toml"), "# mine\n").unwrap();
        write(&env, false);
        assert_eq!(
            std::fs::read_to_string(&mods).unwrap(),
            "# My notes on mods\n"
        );
        assert_eq!(
            std::fs::read_to_string(&roles).unwrap(),
            guide("roles").unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(repo.home().join("mods/checks.toml")).unwrap(),
            "# mine\n"
        );
    }
}
