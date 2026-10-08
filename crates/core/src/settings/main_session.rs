//! `[main_session]`: the session you talk to. It is one real session of the CLI you chose, and
//! it hands work to the crew by calling a tool Argonauts gave it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MainSessionSettings {
    /// Give the main session Argonauts' tools where its CLI takes them, so your message goes to
    /// it as you typed it and it starts a workflow by calling one. Off, it gets none: with a
    /// crew workflow in use each reply ends with a verdict, answer or change, that code reads
    /// instead, as it does for a CLI that can't take tools.
    pub tools: bool,
}

impl Default for MainSessionSettings {
    fn default() -> Self {
        Self { tools: true }
    }
}
