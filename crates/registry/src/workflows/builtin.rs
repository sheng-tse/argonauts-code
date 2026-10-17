//! The built-in workflows. Each is one file: what it is for, its steps, the settings it changes
//! on top of the defaults, and its phases. The built-in settings are `council`'s, so its file
//! sets no key, and the ablations change one key of it each.

use argonauts_core::workflow_files;

use super::choices::about_of;

/// What runs when no setting names a workflow.
pub(crate) const DEFAULT_WORKFLOW: &str = "captain";

/// The workflow the built-in settings are, so its file sets no key.
pub(crate) const BASE_WORKFLOW: &str = "council";

#[derive(Debug, Clone, Copy)]
pub(crate) struct BuiltinWorkflow {
    pub name: &'static str,
    /// Its file.
    pub text: &'static str,
}

impl BuiltinWorkflow {
    /// What its file says it is for.
    pub fn description(&self) -> String {
        about_of(self.text).unwrap_or_default()
    }
}

/// In the order the picker shows them, the default first.
pub(crate) fn builtins() -> impl Iterator<Item = BuiltinWorkflow> {
    workflow_files::BUILT_IN
        .iter()
        .map(|&(name, text)| BuiltinWorkflow { name, text })
}

pub(crate) fn builtin(name: &str) -> Option<BuiltinWorkflow> {
    builtins().find(|workflow| workflow.name == name)
}
