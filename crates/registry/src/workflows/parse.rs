//! YAML to `Workflow`. The core types refuse unknown keys, so a misspelled rule fails to load
//! instead of vanishing. serde-saphyr reads the untagged `Count` and `StepWorkers` and the
//! JSON values of `params` straight from YAML, so there's no intermediate value.

use argonauts_core::hash::{canonical_json, sha256_hex};
use argonauts_core::workflow::Workflow;

use super::WorkflowError;

/// `file` names the text in errors: a path, or `built-in <name>`. The message ends with the
/// line and column when the parser knows them.
pub fn parse_workflow(text: &str, file: &str) -> Result<Workflow, WorkflowError> {
    serde_saphyr::from_str::<Workflow>(text).map_err(|error| WorkflowError::Parse {
        file: file.to_owned(),
        // One line: the snippet form spans several, and errors show in a single row.
        message: error.without_snippet().to_string(),
    })
}

/// The SHA-256 of the workflow's canonical JSON.
pub fn workflow_version(workflow: &Workflow) -> String {
    // Serializing can't fail: every map in a workflow has string keys.
    let json = serde_json::to_value(workflow).unwrap_or_default();
    sha256_hex(canonical_json(&json).as_bytes())
}
