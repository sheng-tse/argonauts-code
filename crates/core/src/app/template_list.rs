//! What `/templates` lists: the workflows and the prompts, with the workflow in use and the
//! files you changed marked.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateList {
    /// Where the reference copies are, as the screen shows it: `~/.argonauts/templates/`.
    pub folder: String,
    /// Every workflow by the name it shows with, the built-in ones first.
    pub workflows: Vec<TemplateEntry>,
    /// The prompts by the role that reads them: `main`, `orchestrator`, `workers`.
    pub prompts: Vec<TemplateEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateEntry {
    pub name: String,
    /// The workflow runs use now.
    pub in_use: bool,
    /// Whose file takes the built-in's place, when one does.
    pub changed: Option<ChangedIn>,
}

impl TemplateEntry {
    pub fn built_in(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            in_use: false,
            changed: None,
        }
    }
}

/// Where a file of your own sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangedIn {
    /// `~/.argonauts/`, yours in every folder.
    Yours,
    /// This folder's `.argonauts/`.
    Here,
}

impl ChangedIn {
    pub fn mark(self) -> &'static str {
        match self {
            ChangedIn::Yours => "(yours)",
            ChangedIn::Here => "(changed here)",
        }
    }
}
