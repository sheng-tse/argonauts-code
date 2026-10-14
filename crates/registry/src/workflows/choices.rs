//! What the `/workflow` picker lists.

use std::path::{Path, PathBuf};

use super::builtin::builtins;
use super::name::is_workflow_name;
use crate::WorkflowChoice;
use crate::resolve::leaves;
use argonauts_core::required;
use argonauts_core::settings::workflows::WORKFLOW_FILE_KEYS;

/// The built-in workflows, in the order the picker shows them.
pub fn workflow_choices() -> Vec<WorkflowChoice> {
    builtins()
        .map(|workflow| WorkflowChoice {
            name: workflow.name.to_owned(),
            source: "built-in".to_owned(),
            description: workflow.description(),
            keys: keys_of(workflow.text),
        })
        .collect()
}

/// Every workflow you can pick: the built-ins, then the `*.toml` files in `global`, then in
/// `project`. A file shadows an earlier workflow of the same name. Its description is the first
/// line of its leading comment, or the shadowed workflow's when it has none. A file whose name
/// isn't a workflow name, such as `my team.toml`, can't be picked, so it isn't listed.
pub fn workflow_choices_in(global: &Path, project: Option<&Path>) -> Vec<WorkflowChoice> {
    let mut all = workflow_choices();
    for dir in std::iter::once(global).chain(project) {
        for (name, path) in toml_workflow_files(dir) {
            // A file that can't be read lists with no keys; resolving it reports the error.
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let description = about_of(&text)
                .or_else(|| script_description(&path, &text))
                .or_else(|| description_of(&text));
            let source = path.display().to_string();
            let keys = keys_of(&text);
            match all.iter_mut().find(|known| known.name == name) {
                Some(known) => {
                    known.source = source;
                    known.keys = keys;
                    if let Some(description) = description {
                        known.description = description;
                    }
                }
                None => all.push(WorkflowChoice {
                    name,
                    source,
                    description: description.unwrap_or_default(),
                    keys,
                }),
            }
        }
    }
    // A name known only by its phases is a workflow too, with no settings of its own.
    for dir in std::iter::once(global).chain(project) {
        for (name, path) in phases_files(dir) {
            if all.iter().any(|known| known.name == name) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            all.push(WorkflowChoice {
                name,
                source: path.display().to_string(),
                description: description_of(&text).unwrap_or_default(),
                keys: Vec::new(),
            });
        }
    }
    all
}

/// The first non-empty line of the comment a file starts with.
fn description_of(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty())
        .take_while(|line| line.starts_with('#'))
        .map(|line| line.trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// The dotted settings a workflow file sets. Its name, steps and phases aren't settings, and
/// text that doesn't parse sets none.
pub(crate) fn keys_of(text: &str) -> Vec<String> {
    let Ok(mut table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    for key in WORKFLOW_FILE_KEYS {
        table.remove(key);
    }
    leaves(&table).into_iter().map(|(key, _)| key).collect()
}

/// What the file says it is for: its `about` line.
pub(crate) fn about_of(text: &str) -> Option<String> {
    let table = text.parse::<toml::Table>().ok()?;
    let about = table.get("about")?.as_str()?.trim();
    (!about.is_empty()).then(|| about.to_owned())
}

/// A workflow may be a script, in the format Claude Code writes its own, in place of a file of
/// steps.
pub(crate) const SCRIPT_EXTENSIONS: [&str; 2] = ["js", "mjs"];

/// A workflow may also be its phases alone, as YAML or as a `.star` file that builds them. Such
/// a file carries no settings, so it never replaces a file of steps, a script or a
/// built-in of the same name: it only gives that workflow its phases.
pub(crate) const PHASES_EXTENSIONS: [&str; 2] = ["star", "yaml"];

/// The workflow files in `dir`, by name, sorted: `*.toml`, and scripts. A name with both lists
/// once, as its `.toml`. A missing folder has none.
pub(crate) fn toml_workflow_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let is = |path: &Path, wanted: &[&str]| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| wanted.contains(&extension))
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && (is(path, &["toml"]) || is(path, &SCRIPT_EXTENSIONS)))
        // The steps every workflow must contain sit beside the workflows, and are none.
        .filter(|path| path.file_name().and_then(|name| name.to_str()) != Some(required::FILE))
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_owned();
            is_workflow_name(&name).then_some((name, path))
        })
        .collect();
    // `.toml` sorts after `.js` and `.mjs`, so the last of a name is the one kept.
    files.sort();
    files.reverse();
    files.dedup_by(|later, kept| later.0 == kept.0);
    files.reverse();
    files
}

/// The files of phases in `dir`, by name, sorted. A missing folder has none.
fn phases_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| PHASES_EXTENSIONS.contains(&extension))
        })
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_owned();
            is_workflow_name(&name).then_some((name, path))
        })
        .collect();
    files.sort();
    files.dedup_by(|later, kept| later.0 == kept.0);
    files
}

/// A script's own `description`, from its `meta`.
fn script_description(path: &Path, text: &str) -> Option<String> {
    let script = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| SCRIPT_EXTENSIONS.contains(&extension));
    if !script {
        return None;
    }
    let after = text
        .split_once("description")?
        .1
        .trim_start()
        .strip_prefix(':')?;
    let after = after.trim_start();
    let quote = after
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"' | '`'))?;
    let said = after[1..].split(quote).next()?.trim();
    (!said.is_empty()).then(|| said.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_workflow_comes_first_and_every_contract_workflow_is_listed() {
        let names: Vec<String> = workflow_choices().into_iter().map(|p| p.name).collect();
        assert_eq!(names[0], "council");
        assert_eq!(names.len(), 12);
        assert!(names.iter().any(|n| n == "orchestrator"));
    }

    #[test]
    fn your_own_workflows_are_listed_and_a_file_hides_its_namesake() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("home/workflows");
        let project = dir.path().join("repo/.argonauts/workflows");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(global.join("council.toml"), "[critique]\nrounds = 2\n").unwrap();
        std::fs::write(
            global.join("my-team.toml"),
            "\n# Two reviewers on every task\n#\n# Ours.\n[review]\nper_task = 2\n",
        )
        .unwrap();
        std::fs::write(
            global.join("captain.toml"),
            "# Captain, but mine\n[captain]\ntries = 2\n",
        )
        .unwrap();
        std::fs::write(project.join("my-team.toml"), "workflow_note = 1\n").unwrap();
        std::fs::write(project.join("notes.md"), "not a workflow").unwrap();
        std::fs::write(
            project.join(required::FILE),
            "[[step]]\nname = \"benchmark\"\nrole = \"worker\"\n",
        )
        .unwrap();
        // Names the lookup refuses aren't listed, since picking one would break every resolve.
        std::fs::write(project.join("my team.toml"), "[captain]\ntries = 2\n").unwrap();
        std::fs::write(project.join(".hidden.toml"), "[captain]\ntries = 2\n").unwrap();
        let all = workflow_choices_in(&global, Some(&project));
        assert_eq!(all.len(), workflow_choices().len() + 1);
        let find = |name: &str| all.iter().find(|workflow| workflow.name == name).unwrap();
        let council = find("council");
        assert!(
            council.source.ends_with("home/workflows/council.toml"),
            "{council:?}"
        );
        assert_eq!(council.keys, ["critique.rounds"]);
        assert!(!council.description.is_empty());
        let builtin_council = workflow_choices()
            .into_iter()
            .find(|p| p.name == "council")
            .unwrap();
        assert_eq!(council.description, builtin_council.description);
        let mine = find("my-team");
        assert!(
            mine.source
                .ends_with("repo/.argonauts/workflows/my-team.toml")
        );
        // The project's file has no header, so the global file's description stays.
        assert_eq!(mine.description, "Two reviewers on every task");
        assert_eq!(mine.keys, ["workflow_note"]);
        let captain = find("captain");
        assert_eq!(captain.description, "Captain, but mine");
        assert!(captain.source.ends_with("home/workflows/captain.toml"));
        assert_eq!(find("best-of-n").source, "built-in");
        assert_eq!(
            workflow_choices_in(&dir.path().join("none"), None),
            workflow_choices()
        );
    }
}
