//! What a session's context holds, as a CLI that can say reports it. Claude Code answers
//! `get_context_usage` with everything its own /context draws, and the whole reply is kept.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How a CLI is asked for its context. `Summary` answers from the last response's usage and
/// local estimates; `Full` counts each category with extra token-count calls, as Claude
/// Code's own /context does.
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

/// What a category is to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    Used,
    Free,
    /// Held back past the point where the session compacts.
    Buffer,
    /// Loaded on demand, so out of the window until used.
    Deferred,
}

/// One row of the CLI's categories, in its order and by its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCategory {
    pub name: String,
    pub tokens: u64,
    /// The CLI's own name for the row's colour, such as Claude Code's `promptBorder`.
    pub color: String,
    pub kind: ContextKind,
}

/// One cell of the grid the CLI lays its categories out on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextCell {
    pub category: String,
    pub color: String,
    pub filled: bool,
    /// The whole category's tokens, the same in each of its cells.
    pub tokens: u64,
    /// The whole category's share of the window, in percent.
    pub percentage: u32,
    /// How much of this cell the category fills, from 0 to 1.
    pub fullness: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFile {
    pub path: String,
    /// Where the file belongs, such as `Project`, `User` or `Local`.
    pub scope: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub server: String,
    pub tokens: u64,
    /// False for a tool found by search and loaded when first called.
    pub loaded: bool,
}

/// A custom agent the session can start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextAgent {
    pub name: String,
    /// As the CLI says it, such as `projectSettings`.
    pub source: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSkill {
    pub name: String,
    /// As the CLI says it, such as `built-in` or `plugin`.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    pub tokens: u64,
}

/// The skills the session lists, and how many of them fit its listing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContextSkills {
    pub total: u32,
    pub included: u32,
    pub tokens: u64,
    pub items: Vec<ContextSkill>,
}

/// The slash commands the session lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ContextCommands {
    pub total: u32,
    pub included: u32,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedTokens {
    pub name: String,
    pub tokens: u64,
}

/// What one tool's calls and results take of the messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolTokens {
    pub name: String,
    pub call_tokens: u64,
    pub result_tokens: u64,
}

impl ToolTokens {
    pub fn total(&self) -> u64 {
        self.call_tokens.saturating_add(self.result_tokens)
    }
}

/// A built-in tool that loads on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredTool {
    pub name: String,
    pub tokens: u64,
    pub loaded: bool,
}

/// What the messages are made of.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MessageBreakdown {
    pub tool_call_tokens: u64,
    pub tool_result_tokens: u64,
    pub attachment_tokens: u64,
    pub assistant_tokens: u64,
    pub user_tokens: u64,
    /// Output a tool sent to a file, of which the context holds only where it is.
    pub redirected_tokens: u64,
    /// The messages' tokens no kind accounts for.
    pub unattributed_tokens: u64,
    /// By tool, the largest first.
    pub tools: Vec<ToolTokens>,
    /// By kind of attachment, the largest first.
    pub attachments: Vec<NamedTokens>,
}

/// The last request's tokens, as the API counted them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApiUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl ApiUsage {
    /// Everything the request sent, cached or not.
    pub fn sent(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }
}

/// A session's context as its CLI counts it: every field of Claude Code's `get_context_usage`
/// reply, under Argonauts Code's names.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ContextUsage {
    /// In the CLI's order, the rows out of the window included.
    pub categories: Vec<ContextCategory>,
    /// What the window holds now.
    pub total_tokens: u64,
    /// The window the session works in.
    pub window: u64,
    /// The window the percentage and the shares are of. Claude Code 2.1.296 sends the same as
    /// `window`.
    pub raw_window: u64,
    /// `total_tokens` of `raw_window`, in whole percent, as the CLI rounds it.
    pub percentage: u32,
    /// The grid's rows of cells, as the CLI laid them out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grid: Vec<Vec<ContextCell>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub auto_compact: bool,
    /// Where the CLI compacts on its own, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<u64>,
    /// What set the window the session compacts in, such as `settings`, `model-default` or
    /// `auto`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auto_compact_source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_files: Vec<MemoryFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_tools: Vec<McpTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<ContextAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<ContextSkills>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_commands: Option<ContextCommands>,
    /// Claude Code 2.1.296 names these three in its schema and never sends them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_builtin_tools: Vec<DeferredTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_tools: Vec<NamedTokens>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_prompt_sections: Vec<NamedTokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<MessageBreakdown>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_usage: Option<ApiUsage>,
}

impl ContextUsage {
    /// The rows of one kind, in the CLI's order.
    pub fn of_kind(&self, kind: ContextKind) -> impl Iterator<Item = &ContextCategory> {
        self.categories
            .iter()
            .filter(move |category| category.kind == kind)
    }

    /// The tokens of the rows of one kind.
    pub fn tokens_of(&self, kind: ContextKind) -> u64 {
        self.of_kind(kind).map(|category| category.tokens).sum()
    }

    /// The row named `name`, when the CLI sent one.
    pub fn category(&self, name: &str) -> Option<&ContextCategory> {
        self.categories
            .iter()
            .find(|category| category.name == name)
    }

    /// Where the session compacts on its own: the CLI's threshold while auto-compact is on.
    pub fn compacts_at(&self) -> Option<u64> {
        self.auto_compact_threshold
            .filter(|at| self.auto_compact && *at > 0)
    }

    /// The point the fill is measured against: where the session compacts, else its window.
    pub fn limit(&self) -> u64 {
        self.compacts_at().unwrap_or(self.window)
    }

    /// The same count without the grid and the lists, for a place that keeps only how full
    /// the session is and with what.
    pub fn summary(&self) -> ContextUsage {
        ContextUsage {
            categories: self.categories.clone(),
            total_tokens: self.total_tokens,
            window: self.window,
            raw_window: self.raw_window,
            percentage: self.percentage,
            model: self.model.clone(),
            auto_compact: self.auto_compact,
            auto_compact_threshold: self.auto_compact_threshold,
            auto_compact_source: self.auto_compact_source.clone(),
            ..ContextUsage::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, tokens: u64, kind: ContextKind) -> ContextCategory {
        ContextCategory {
            name: name.to_owned(),
            tokens,
            color: "inactive".to_owned(),
            kind,
        }
    }

    fn usage() -> ContextUsage {
        ContextUsage {
            categories: vec![
                row("System prompt", 6_888, ContextKind::Used),
                row("System tools (deferred)", 25_213, ContextKind::Deferred),
                row("Messages", 719_981, ContextKind::Used),
                row("Autocompact buffer", 33_000, ContextKind::Buffer),
                row("Free space", 217_000, ContextKind::Free),
            ],
            total_tokens: 726_869,
            window: 1_000_000,
            raw_window: 1_000_000,
            percentage: 73,
            auto_compact: true,
            auto_compact_threshold: Some(967_000),
            ..ContextUsage::default()
        }
    }

    #[test]
    fn rows_are_read_by_kind_and_by_name() {
        let usage = usage();
        assert_eq!(usage.tokens_of(ContextKind::Used), 726_869);
        assert_eq!(usage.tokens_of(ContextKind::Deferred), 25_213);
        assert_eq!(
            usage.category("Free space").map(|row| row.tokens),
            Some(217_000)
        );
        assert!(usage.category("Skills").is_none());
    }

    #[test]
    fn the_fill_is_measured_where_the_session_compacts() {
        let mut usage = usage();
        assert_eq!(
            (usage.compacts_at(), usage.limit()),
            (Some(967_000), 967_000)
        );
        usage.auto_compact = false;
        assert_eq!((usage.compacts_at(), usage.limit()), (None, 1_000_000));
    }

    #[test]
    fn a_summary_keeps_the_rows_and_drops_the_lists() {
        let mut usage = usage();
        usage.memory_files.push(MemoryFile {
            path: "CLAUDE.md".to_owned(),
            scope: "Project".to_owned(),
            tokens: 2_300,
        });
        let summary = usage.summary();
        assert_eq!(summary.categories, usage.categories);
        assert_eq!(summary.total_tokens, usage.total_tokens);
        assert!(summary.memory_files.is_empty());
    }
}
