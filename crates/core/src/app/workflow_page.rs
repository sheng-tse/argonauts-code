//! `/workflow edit <name>`: a workflow file of steps as a page. It shows the steps in order,
//! each with its keys; every change is checked and, when the steps still run, saved to the
//! file with its comments kept.

use serde::{Deserialize, Serialize};

/// The whole page, sent when it opens and again after every change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPage {
    pub name: String,
    /// The file, with `~` for home.
    pub path: String,
    /// The workflow's one line on what it is for.
    pub about: String,
    pub check: PageCheck,
    /// The file's own keys first, then one group per step in the file's order.
    pub groups: Vec<PageGroup>,
    /// What the last change did, or why it wasn't saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// For the page already open, so nothing opens for it once that page has closed.
    #[serde(default)]
    pub refresh: bool,
}

/// Whether the steps would run as the file stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageCheck {
    pub valid: bool,
    /// `the file is valid · about 9 to 13 agents if Plan lists 3 tasks`, or what is wrong
    /// with it, a line each.
    pub lines: Vec<String>,
}

/// The file's own keys, or one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageGroup {
    /// Which step it is, from 0. `None` for the file's own keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    /// `Workflow`, or the step's name.
    pub title: String,
    /// A step's role, then who would take it and how, as a dry run says them. For the file's
    /// own keys `how` alone, such as `adapt false · no goal`.
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub who: String,
    #[serde(default)]
    pub how: String,
    pub fields: Vec<PageField>,
}

/// One key of the file or of a step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageField {
    pub key: String,
    pub label: String,
    /// As the file has it. Empty when the file doesn't set it.
    pub value: String,
    /// Every value it takes, when they are few. Empty for a key you type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// The key may be left out, so unset is one of its values.
    #[serde(default)]
    pub optional: bool,
    /// One line on what the key does.
    pub about: String,
}

/// One change made on the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum WorkflowChange {
    /// A key of a step, or of the file itself when `step` is `None`. An empty value takes the
    /// key out.
    Set {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u32>,
        key: String,
        value: String,
    },
    /// A new step after this one, or at the end.
    AddStep {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<u32>,
    },
    RemoveStep {
        step: u32,
    },
    /// One place up, or one place down.
    MoveStep {
        step: u32,
        up: bool,
    },
    /// Hand the file to `$VISUAL` or `$EDITOR`.
    OpenInEditor,
}
