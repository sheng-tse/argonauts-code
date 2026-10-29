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

/// What a session holds at its start, before its CLI counts it: what the last fresh session
/// on the same CLI and model held then, or, on a very first run, an estimate. A session starts
/// without messages, so none are listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartingCount {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// What the CLI holds back for compacting, past the point where the session compacts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub buffer: u64,
    /// In the CLI's order, each with its colour where the CLI has one.
    pub parts: Vec<StartPart>,
    /// The instruction files the CLI loads by itself, each by its own count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<NamedTokens>,
    /// The tools the CLI names as the session starts, where it lists them apart from what
    /// they take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<StartingTools>,
    pub from: StartedFrom,
    /// When the CLI's own count first comes. `None` for a CLI no recording ships for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_count: Option<FirstCount>,
}

/// What of a CLI's ends when its own count of a session first comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstCount {
    /// Its first answer, as Claude Code's count by category.
    Answer,
    /// Its first turn, whose usage totals every request in it, as Codex's.
    Turn,
    /// Its first step, a single request, as OpenCode's and agy's.
    Step,
}

impl FirstCount {
    pub fn word(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Turn => "turn",
            Self::Step => "step",
        }
    }

    /// Whether the first usage the CLI reports holds every request of its turn, so it says
    /// what the first request held only when no tool ran before it.
    pub fn totals_the_turn(self) -> bool {
        self == Self::Turn
    }
}

/// A part of what a session holds at its start: one the CLI names itself, or one Argonauts Code
/// names by what it holds, which the screen words with the CLI's name as it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "KeptPart")]
pub struct StartPart {
    /// What the part holds, for a part the CLI has no name of its own for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holds: Option<StartHolds>,
    /// The CLI's own name for the part. Empty where `holds` says what it is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub tokens: u64,
    /// Claude Code's name for the colour it draws in.
    pub color: String,
}

/// What a part of a session's start holds, where the CLI has no name of its own for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartHolds {
    /// The CLI's own prompt and tools.
    OwnPromptAndTools,
    /// The instruction files the CLI loads by itself.
    InstructionFiles,
    /// Argonauts Code's tools and the lines about them.
    AppTools,
}

impl StartPart {
    /// A part the CLI names itself.
    pub fn named(name: impl Into<String>, tokens: u64, color: impl Into<String>) -> Self {
        Self {
            holds: None,
            name: name.into(),
            tokens,
            color: color.into(),
        }
    }

    pub fn holding(holds: StartHolds, tokens: u64, color: impl Into<String>) -> Self {
        Self {
            holds: Some(holds),
            name: String::new(),
            tokens,
            color: color.into(),
        }
    }
}

/// A part as a start was kept: before parts were kept by what they hold, each had the words
/// the app gave it, which read back as what they name.
#[derive(Deserialize)]
struct KeptPart {
    #[serde(default)]
    holds: Option<StartHolds>,
    #[serde(default)]
    name: String,
    tokens: u64,
    color: String,
}

impl From<KeptPart> for StartPart {
    fn from(kept: KeptPart) -> Self {
        let worded = match kept.name.as_str() {
            _ if kept.holds.is_some() => kept.holds,
            name if name.ends_with("'s own prompt and tools") => {
                Some(StartHolds::OwnPromptAndTools)
            }
            "Instruction files" => Some(StartHolds::InstructionFiles),
            "Argonauts Code's tools" => Some(StartHolds::AppTools),
            _ => None,
        };
        match worded {
            Some(holds) => Self::holding(holds, kept.tokens, kept.color),
            None => Self::named(kept.name, kept.tokens, kept.color),
        }
    }
}

impl StartingCount {
    /// What the parts hold together.
    pub fn used(&self) -> u64 {
        self.parts.iter().map(|part| part.tokens).sum()
    }
}

/// How many tools a CLI names at a session's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartingTools {
    pub named: u32,
    /// How many of them a read-only session offers, where Argonauts Code gives it fewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<u32>,
}

/// Where a starting count comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "from")]
pub enum StartedFrom {
    /// The last fresh session on the same CLI and model, as it started.
    LastSession,
    /// The CLI's own prompt and tools as recorded for its `version`, and what this machine
    /// adds to them, counted at a token per 4 characters.
    Recording { version: String },
    /// Only what this machine adds, for a CLI no recording ships for.
    Unrecorded,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A start kept before parts were kept by what they hold reads them by their words, and
    /// one the CLI named stays named.
    #[test]
    fn a_start_kept_with_worded_parts_still_reads() {
        let kept = r#"{"agent":"codex","model":"gpt-6-luna","parts":[
            {"name":"Codex's own prompt and tools","tokens":15236,"color":"promptBorder","kind":"used"},
            {"name":"Instruction files","tokens":1200,"color":"claude","kind":"used"},
            {"name":"Argonauts Code's tools","tokens":900,"color":"cyan_FOR_SUBAGENTS_ONLY","kind":"used"},
            {"name":"System prompt","tokens":4380,"color":"promptBorder","kind":"used"}
        ],"from":{"from":"last_session"},"first_count":"turn"}"#;
        let start: StartingCount = serde_json::from_str(kept).expect("a kept start");
        assert_eq!(
            start.parts,
            [
                StartPart::holding(StartHolds::OwnPromptAndTools, 15_236, "promptBorder"),
                StartPart::holding(StartHolds::InstructionFiles, 1_200, "claude"),
                StartPart::holding(StartHolds::AppTools, 900, "cyan_FOR_SUBAGENTS_ONLY"),
                StartPart::named("System prompt", 4_380, "promptBorder"),
            ]
        );
        let again: StartingCount =
            serde_json::from_str(&serde_json::to_string(&start).expect("written"))
                .expect("read back");
        assert_eq!(again, start);
    }

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
