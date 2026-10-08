//! What a session's context holds, by category, as a CLI that can say reports it. Claude Code
//! answers `get_context_usage` with the numbers its own /context draws.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How a CLI is asked for its breakdown, `ui.widgets.context.detail`. `Summary` answers from
/// the last response's usage and local estimates; `Full` counts each category with extra
/// token-count calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextDetail {
    #[default]
    Summary,
    Full,
}

impl ContextDetail {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Full => "full",
        }
    }
}

/// The parts of a context bar, in the order /context draws them: what's used, then what's
/// free, then the reserve past the compaction point, drawn held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPart {
    SystemPrompt,
    Tools,
    McpTools,
    Agents,
    MemoryFiles,
    Skills,
    Messages,
    /// A category the CLI added that Argonauts doesn't know yet.
    Other,
    Free,
    Buffer,
}

impl ContextPart {
    /// The name the legend shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemPrompt => "system prompt",
            Self::Tools => "tools",
            Self::McpTools => "mcp tools",
            Self::Agents => "agents",
            Self::MemoryFiles => "memory files",
            Self::Skills => "skills",
            Self::Messages => "messages",
            Self::Other => "other",
            Self::Free => "free",
            Self::Buffer => "buffer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCategory {
    pub part: ContextPart,
    pub tokens: u64,
}

/// One entry under a category: a memory file, an MCP tool, an agent, or a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItem {
    pub name: String,
    pub tokens: u64,
    /// Where it comes from: a memory file's scope, a tool's server, an agent's or a skill's
    /// source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
