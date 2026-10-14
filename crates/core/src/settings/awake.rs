//! Keeping the computer awake (`[awake]`). A long run stops when the laptop sleeps, so
//! Argonauts Code holds sleep off while anything is working. `/awake` holds it off for longer
//! and keeps the screen on too, and `/awake off` lets the computer sleep whatever is working.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AwakeSettings {
    /// Keep the computer awake while anything is working. On macOS this is the built-in
    /// `caffeinate`, tied to Argonauts Code, so it ends when Argonauts Code does.
    pub while_working: bool,
    /// Keep the screen on too while anything is working. Off by default: the work goes on
    /// behind a dark screen, and `/awake` holds the screen when you want to watch.
    pub screen_while_working: bool,
}

impl Default for AwakeSettings {
    fn default() -> Self {
        Self {
            while_working: true,
            screen_while_working: false,
        }
    }
}
