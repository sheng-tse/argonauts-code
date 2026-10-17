//! `[main_session]`: the session you talk to. It is one real session of the CLI you chose, and
//! it hands work to the crew by calling a tool Argonauts Code gave it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MainSessionSettings {
    /// Give the main session Argonauts Code's tools where its CLI takes them, so your message goes
    /// to it as you typed it and it starts a workflow by calling one. Off, it gets none: with a
    /// crew workflow in use each reply ends with a verdict, answer or change, that code reads
    /// instead, as it does for a CLI that can't take tools.
    pub tools: bool,
    /// Claude Code thinks before it answers you, as its Thinking mode does. Off, the session
    /// you talk to answers without thinking.
    pub thinking: bool,
    /// Each app session starts with `/fast` on: answers at the lowest effort the model takes,
    /// and in Claude Code's own fast mode where the model has one, which can cost more.
    pub fast: bool,
    /// The language answers come in, such as `japanese` or `ja`. Empty leaves it to the CLI.
    pub language: String,
}

impl Default for MainSessionSettings {
    fn default() -> Self {
        Self {
            tools: true,
            thinking: true,
            fast: false,
            language: String::new(),
        }
    }
}
