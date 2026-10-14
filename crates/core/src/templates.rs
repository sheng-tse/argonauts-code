//! The repository's `templates/` folder, which the program is built from: the workflows, the
//! prompts, an example instructions file for each role, and the starter mods, each part with a
//! README. The same files are written to `~/.argonauts/templates/` as reference copies.

/// The text of a built-in template, by its path under `templates/`. Every crate that builds
/// one in sits two folders below the repository's top.
#[macro_export]
macro_rules! template {
    ($path:expr) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../templates/",
            $path
        ))
    };
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
    use crate::prompt_files::{RoleFileMode, YourFile, parse_role_file};
    use crate::settings::crew::CrewRole;

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
