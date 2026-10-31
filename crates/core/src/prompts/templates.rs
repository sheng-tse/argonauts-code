//! The crate's `templates/` folder, which the program is built from: the workflows, the
//! prompts, an example instructions file for each role, and the starter mods, each part with a
//! README. The same files are written to `~/.argonauts/templates/` as reference copies.

/// `(path under templates/, text)` for every built-in template, sorted by path.
pub const FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/templates.rs"));

/// The text of a built-in template, by its path under `templates/`. A path with no file is a
/// build error in the crate that names it.
#[macro_export]
macro_rules! template {
    ($path:expr) => {{
        const TEXT: &str = $crate::prompts::templates::text($path);
        TEXT
    }};
}

/// The text at `path` in [`FILES`]. `template!` calls it at build time, where a missing path
/// stops the build.
pub const fn text(path: &str) -> &'static str {
    let mut i = 0;
    while i < FILES.len() {
        if same(FILES[i].0, path) {
            return FILES[i].1;
        }
        i += 1;
    }
    panic!("no built-in template has this path")
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The folder under `~/.argonauts/` that holds the reference copies.
pub const DIR: &str = "templates";

/// What the folder is, and where a copy goes to take effect.
pub const README: &str = template!("README.md");

/// How your text reaches a role, beside the examples.
pub const ROLES_README: &str = template!("roles/README.md");

/// An example of your own instructions for each crew role, by the role's name.
pub const ROLE_EXAMPLES: &[(&str, &str)] = &[
    ("main", template!("roles/main.md")),
    ("orchestrator", template!("roles/orchestrator.md")),
    ("worker", template!("roles/worker.md")),
    ("reviewer", template!("roles/reviewer.md")),
    ("judge", template!("roles/judge.md")),
];

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::prompts::prompt_files::{RoleFileMode, YourFile, parse_role_file};
    use crate::settings::crew::CrewRole;

    #[test]
    fn the_table_holds_every_file_in_the_folder_once_in_path_order() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates");
        let mut on_disk = Vec::new();
        let mut folders = vec![root.clone()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    let under = path
                        .strip_prefix(&root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned();
                    let text = std::fs::read_to_string(&path).unwrap();
                    on_disk.push((under, text));
                }
            }
        }
        on_disk.sort();
        let built: Vec<(String, String)> = FILES
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect();
        assert_eq!(built, on_disk);
    }

    #[test]
    fn the_macro_reads_a_template_by_its_path() {
        assert_eq!(template!("roles/README.md"), ROLES_README);
        assert!(template!("workflows/council.toml").contains("kind  = \"talk\""));
        assert_eq!(text("README.md"), README);
    }

    #[test]
    fn every_crew_role_has_an_example_that_reads_as_a_role_file() {
        let roles: Vec<&str> = CrewRole::ALL.iter().map(|role| role.as_str()).collect();
        let examples: Vec<&str> = ROLE_EXAMPLES.iter().map(|(role, _)| *role).collect();
        assert_eq!(examples, roles);
        for (role, text) in ROLE_EXAMPLES {
            let shown = format!("roles/{role}.md");
            let file = YourFile {
                text: (*text).to_owned(),
                path: PathBuf::from(&shown),
                shown,
            };
            let example = parse_role_file(file)
                .unwrap()
                .unwrap_or_else(|| panic!("{role}'s example has no text"));
            assert_eq!(example.mode, RoleFileMode::Add, "{role}");
        }
    }
}
