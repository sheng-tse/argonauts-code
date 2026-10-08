//! `[switch]`: what a new main model on another CLI starts from. On the same CLI the session
//! carries on with the new model and keeps everything.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SwitchSettings {
    /// How much of the conversation the new CLI's session gets.
    pub carry: Carry,
    /// How many of the latest exchanges go along word for word.
    pub recent: usize,
}

impl Default for SwitchSettings {
    fn default() -> Self {
        Self {
            carry: Carry::Summary,
            recent: 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Carry {
    /// A summary the model being left writes, the recent exchanges in full, and the
    /// transcript's path. The summary is one short turn made right after the switch.
    Summary,
    /// The recent exchanges in full and the transcript's path, at no cost.
    Recent,
    /// Every exchange in full and the transcript's path.
    All,
}

impl Carry {
    pub fn as_str(self) -> &'static str {
        match self {
            Carry::Summary => "summary",
            Carry::Recent => "recent",
            Carry::All => "all",
        }
    }
}
