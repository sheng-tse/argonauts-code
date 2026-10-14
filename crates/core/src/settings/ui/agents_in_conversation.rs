//! When a crew agent shows in the conversation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `[ui] agents_in_conversation`: an agent at work lives in the list under the input with what it's
/// doing now, and lands in the conversation once it finishes; or it has a live block in the
/// conversation that prints its rows as they finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsInConversation {
    /// Once, when it finishes, so nothing half made sits in the conversation inviting you to open
    /// it.
    #[default]
    WhenDone,
    /// A live block from its start, with its rows printed as they finish.
    AsTheyWork,
}
