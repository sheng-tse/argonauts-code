//! Keeping the computer awake (`[awake]`). A long run stops when the laptop sleeps, so
//! Argonauts holds sleep off while anything is working. `/awake` holds it off for longer,
//! and `/awake off` lets the computer sleep whatever is working.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AwakeSettings {
    /// Keep the computer awake while anything is working. On macOS this is the built-in
    /// `caffeinate`, tied to Argonauts, so it ends when Argonauts does.
    pub while_working: bool,
}

impl Default for AwakeSettings {
    fn default() -> Self {
        Self {
            while_working: true,
        }
    }
}
