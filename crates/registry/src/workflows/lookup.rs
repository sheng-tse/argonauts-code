//! Finding workflows by name, and copying one out to edit.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

use argonauts_core::workflow::{Workflow, WorkflowRef};

use super::check::{MAX_DEPTH, check_workflow};
use super::list::known_names;
use super::parse::{parse_workflow, workflow_version};
use super::{BUILT_IN, FoundWorkflow, WorkflowDirs, WorkflowError, builtin_label};

pub(super) const YAML: &str = "yaml";
pub(super) const STARLARK: &str = "star";
pub(super) const STARLARK_LATER: &str = "Starlark workflows come later";

/// A workflow's text and where it came from, before it's parsed.
pub(super) struct Located {
    pub(super) name: String,
    /// `built-in`, or the file's path.
    pub(super) source: String,
    /// What errors name: the file's path, or `built-in <name>`.
    pub(super) label: String,
    pub(super) text: String,
}

impl Located {
    pub(super) fn builtin(name: &str, text: &str) -> Self {
        Self {
            name: name.to_owned(),
            source: BUILT_IN.to_owned(),
            label: builtin_label(name),
            text: text.to_owned(),
        }
    }

    pub(super) fn file(name: &str, path: &Path, text: String) -> Self {
        Self {
            name: name.to_owned(),
            source: path.display().to_string(),
            label: path.display().to_string(),
            text,
        }
    }
}

/// `.argonauts/workflows/<name>.yaml`, then `~/.argonauts/workflows/<name>.yaml`, then the
/// built-in, parsed and checked. Nesting is checked against the ceiling of 2; the engine
/// holds a run to a lower `workflows.max_depth`.
pub fn find_workflow(
    name: &WorkflowRef,
    dirs: &WorkflowDirs<'_>,
) -> Result<FoundWorkflow, WorkflowError> {
    load(locate(name.as_str(), dirs)?, dirs)
}

/// `/workflow copy`: writes the workflow's text as it's found now into the folder `to`, and
/// never replaces a file already there. The text isn't checked, since you copy it to edit.
pub fn copy_workflow(
    name: &WorkflowRef,
    dirs: &WorkflowDirs<'_>,
    to: &Path,
) -> Result<PathBuf, WorkflowError> {
    let located = locate(name.as_str(), dirs)?;
    std::fs::create_dir_all(to).map_err(|error| io_error(to, &error))?;
    let path = to.join(format!("{}.{YAML}", located.name));
    // `create_new` refuses an existing file in the same step that opens it, so nothing can
    // appear between a check and the write.
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return Err(WorkflowError::Refused(format!(
                "{} already exists, and a copy never replaces a file",
                path.display()
            )));
        }
        Err(error) => return Err(io_error(&path, &error)),
    };
    if let Err(error) = file.write_all(located.text.as_bytes()) {
        // Leave no half-written copy behind. The write error is the one worth reporting.
        let _ = std::fs::remove_file(&path);
        return Err(io_error(&path, &error));
    }
    Ok(path)
}

/// Parses and checks what `locate` found.
pub(super) fn load(
    located: Located,
    dirs: &WorkflowDirs<'_>,
) -> Result<FoundWorkflow, WorkflowError> {
    let workflow = parse_workflow(&located.text, &located.label)?;
    let problems = check_workflow(&workflow, Some(&located.name), dirs, MAX_DEPTH);
    if !problems.is_empty() {
        return Err(WorkflowError::Invalid {
            file: located.label,
            problems,
        });
    }
    Ok(FoundWorkflow {
        version: workflow_version(&workflow),
        workflow,
        source: located.source,
        text: located.text,
    })
}

/// Found and parsed but not checked, for following a `workflow` step without checking the
/// same files over and over.
pub(super) fn load_unchecked(
    name: &WorkflowRef,
    dirs: &WorkflowDirs<'_>,
) -> Result<Workflow, WorkflowError> {
    let located = locate(name.as_str(), dirs)?;
    parse_workflow(&located.text, &located.label)
}

/// The text of the workflow `name`: project, then global, then built-in.
pub(super) fn locate(name: &str, dirs: &WorkflowDirs<'_>) -> Result<Located, WorkflowError> {
    if !is_workflow_name(name) {
        return Err(WorkflowError::Refused(format!(
            "{name:?} isn't a workflow name; use letters, digits, '-', '_', and '.'"
        )));
    }
    for dir in dirs.project.into_iter().chain([dirs.global]) {
        if let Some(located) = in_folder(dir, name)? {
            return Ok(located);
        }
    }
    if let Some((builtin, text)) = dirs.builtins.iter().find(|(builtin, _)| *builtin == name) {
        return Ok(Located::builtin(builtin, text));
    }
    Err(WorkflowError::NotFound {
        name: name.to_owned(),
        known: known_names(dirs),
    })
}

/// `<dir>/<name>.yaml`, if it's there. A Starlark file of that name is refused, since it
/// would shadow the workflows further down.
fn in_folder(dir: &Path, name: &str) -> Result<Option<Located>, WorkflowError> {
    let path = dir.join(format!("{name}.{YAML}"));
    match std::fs::read_to_string(&path) {
        Ok(text) => return Ok(Some(Located::file(name, &path, text))),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(&path, &error)),
    }
    let starlark = dir.join(format!("{name}.{STARLARK}"));
    if starlark.is_file() {
        return Err(WorkflowError::Refused(format!(
            "{}: {STARLARK_LATER}",
            starlark.display()
        )));
    }
    Ok(None)
}

/// A name is a file stem in the workflows folder, so it can't reach outside it.
pub(super) fn is_workflow_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub(super) fn io_error(path: &Path, error: &std::io::Error) -> WorkflowError {
    WorkflowError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}
