//! Workflow files: your own in `.argonauts/workflows/` and `~/.argonauts/workflows/`, and the
//! built-ins, which the crew engine ships and the app passes in. Looked up in turn:
//! project, then global, then built-in. Simple workflows are YAML; Starlark comes later.

mod builtin;
mod check;
mod choices;
mod files;
mod list;
mod lookup;
mod name;
mod parse;
mod placeholders;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use argonauts_core::workflow::Workflow;

pub(crate) use builtin::{DEFAULT_WORKFLOW, builtin};
pub use check::{MAX_DEPTH, check_workflow};
pub(crate) use choices::{PHASES_EXTENSIONS, SCRIPT_EXTENSIONS};
pub use choices::{workflow_choices, workflow_choices_in};
pub use files::{builtin_workflow, upgrade_workflows, write_workflow};
pub use list::workflows_in;
pub use lookup::{copy_workflow, find_workflow};
pub(crate) use name::{is_workflow_name, not_a_workflow_name};
pub use parse::{parse_workflow, workflow_version};

/// Where to look for workflows.
#[derive(Debug, Clone, Copy)]
pub struct WorkflowDirs<'a> {
    /// `~/.argonauts/workflows/`.
    pub global: &'a Path,
    /// `<repo>/.argonauts/workflows/`, when there's a repo.
    pub project: Option<&'a Path>,
    /// The crew engine's built-ins as (name, YAML text).
    pub builtins: &'a [(&'a str, &'a str)],
}

/// One row of the `/workflow` picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowInfo {
    pub name: String,
    /// `built-in`, or the file it came from.
    pub source: String,
    /// The workflow's own description, or why it doesn't load.
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FoundWorkflow {
    pub workflow: Workflow,
    /// `built-in`, or the file it came from.
    pub source: String,
    /// The text as found, for `/workflow show` and `diff`.
    pub text: String,
    /// The SHA-256 of its canonical JSON, as `WorkflowStarted.version` records it.
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowError {
    #[error("no workflow named {name:?}; known: {known}")]
    NotFound { name: String, known: String },
    /// With the line and column where the YAML allows.
    #[error("{file}: {message}")]
    Parse { file: String, message: String },
    #[error("{file}: {}", problems.join("; "))]
    Invalid { file: String, problems: Vec<String> },
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("refused: {0}")]
    Refused(String),
}

impl WorkflowError {
    /// The error without the file it names, for a row that already shows the file.
    fn reason(&self) -> String {
        match self {
            Self::Parse { message, .. } | Self::Io { message, .. } => message.clone(),
            Self::Invalid { problems, .. } => problems.join("; "),
            Self::Refused(message) => message.clone(),
            other => other.to_string(),
        }
    }
}

/// The label for built-in text in errors, where a file would give its path.
fn builtin_label(name: &str) -> String {
    format!("built-in {name}")
}

const BUILT_IN: &str = "built-in";
