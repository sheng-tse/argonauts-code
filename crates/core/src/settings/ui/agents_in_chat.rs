//! When a crew agent shows in the chat.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `[ui] agents_in_chat`: an agent at work lives in the list under the input with what it's
/// doing now, and lands in the chat once it finishes; or it has a live block in the chat that
/// prints its rows as they finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsInChat {
    /// Once, when it finishes, so nothing half made sits in the chat inviting you to open it.
    #[default]
    WhenDone,
    /// A live block from its start, with its rows printed as they finish.
    AsTheyWork,
}
