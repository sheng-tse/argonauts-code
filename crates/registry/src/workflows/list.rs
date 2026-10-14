//! What the `/workflow` picker lists.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::lookup::{Located, STARLARK, STARLARK_LATER, YAML, load};
use super::name::is_workflow_name;
use super::{WorkflowDirs, WorkflowInfo};

/// Every workflow for the `/workflow` picker: the built-ins, then the files in `global`, then
/// in `project`. A file shadows an earlier workflow of the same name. A file that doesn't
/// load is listed too, with why as its description.
pub fn workflows_in(dirs: &WorkflowDirs<'_>) -> Vec<WorkflowInfo> {
    let mut all: Vec<WorkflowInfo> = dirs
        .builtins
        .iter()
        .map(|(name, text)| describe(Located::builtin(name, text), dirs))
        .collect();
    for dir in std::iter::once(dirs.global).chain(dirs.project) {
        for (name, path) in workflow_files(dir) {
            let info = if path.extension().is_some_and(|ext| ext == STARLARK) {
                WorkflowInfo {
                    name,
                    source: path.display().to_string(),
                    description: STARLARK_LATER.to_owned(),
                }
            } else {
                match std::fs::read_to_string(&path) {
                    Ok(text) => describe(Located::file(&name, &path, text), dirs),
                    Err(error) => WorkflowInfo {
                        name,
                        source: path.display().to_string(),
                        description: error.to_string(),
                    },
                }
            };
            match all.iter_mut().find(|known| known.name == info.name) {
                Some(known) => *known = info,
                None => all.push(info),
            }
        }
    }
    all
}

/// Every name `find_workflow` could find, sorted, for the not-found error.
pub(super) fn known_names(dirs: &WorkflowDirs<'_>) -> String {
    let mut names: Vec<String> = dirs
        .builtins
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    for dir in std::iter::once(dirs.global).chain(dirs.project) {
        names.extend(workflow_files(dir).into_keys());
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

fn describe(located: Located, dirs: &WorkflowDirs<'_>) -> WorkflowInfo {
    let name = located.name.clone();
    let source = located.source.clone();
    let description = match load(located, dirs) {
        Ok(found) => found.workflow.description,
        Err(error) => error.reason(),
    };
    WorkflowInfo {
        name,
        source,
        description,
    }
}

/// The `*.yaml` and `*.star` files in `dir` by name. A YAML file wins over a Starlark one of
/// the same name, as it does in lookup. A missing folder has none.
fn workflow_files(dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut files = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|stem| stem.to_str()),
            path.extension().and_then(|ext| ext.to_str()),
        ) else {
            continue;
        };
        let wanted = ext == YAML || (ext == STARLARK && !files.contains_key(stem));
        if wanted && is_workflow_name(stem) && path.is_file() {
            let stem = stem.to_owned();
            files.insert(stem, path);
        }
    }
    files
}
