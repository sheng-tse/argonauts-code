//! The `get_context_usage` reply, read whole: the rows, the grid and the lists Claude Code's
//! own /context draws, and the numbers it keeps to itself, such as what the messages are made
//! of and the last request's cache use.

use argonauts_core::AgentEvent;
use argonauts_core::runs::events::context::{
    ApiUsage, ContextAgent, ContextCategory, ContextCell, ContextCommands, ContextKind,
    ContextSkill, ContextSkills, ContextUsage, DeferredTool, McpTool, MemoryFile, MessageBreakdown,
    NamedTokens, ToolTokens,
};
use serde_json::Value;

use crate::parser::json::{array_at, bool_at, str_at, string_at, u64_at};

/// The reply as an event. `None` when it names no window.
pub fn breakdown(reply: &Value) -> Option<AgentEvent> {
    usage(reply).map(|usage| AgentEvent::ContextBreakdown {
        usage: Box::new(usage),
    })
}

/// Every field of the reply. A field a later version drops reads as empty, as Claude Code's
/// own reader of the reply takes it.
pub fn usage(reply: &Value) -> Option<ContextUsage> {
    let window = u64_at(reply, "maxTokens");
    if window == 0 {
        return None;
    }
    let raw_window = Some(u64_at(reply, "rawMaxTokens"))
        .filter(|raw| *raw > 0)
        .unwrap_or(window);
    Some(ContextUsage {
        categories: array_at(reply, "categories")
            .iter()
            .filter_map(category)
            .collect(),
        total_tokens: u64_at(reply, "totalTokens"),
        window,
        raw_window,
        percentage: u32_at(reply, "percentage"),
        grid: array_at(reply, "gridRows")
            .iter()
            .map(|row| row.as_array().map_or(&[][..], Vec::as_slice))
            .map(|row| row.iter().map(cell).collect())
            .collect(),
        model: string_at(reply, "model").filter(|model| !model.is_empty()),
        auto_compact: bool_at(reply, "isAutoCompactEnabled"),
        auto_compact_threshold: Some(u64_at(reply, "autoCompactThreshold")).filter(|at| *at > 0),
        auto_compact_source: string_at(reply, "autocompactSource")
            .unwrap_or_else(|| "model-default".to_owned()),
        memory_files: array_at(reply, "memoryFiles")
            .iter()
            .filter_map(memory_file)
            .collect(),
        mcp_tools: array_at(reply, "mcpTools")
            .iter()
            .filter_map(mcp_tool)
            .collect(),
        agents: array_at(reply, "agents").iter().filter_map(agent).collect(),
        skills: reply
            .get("skills")
            .filter(|skills| skills.is_object())
            .map(skills),
        slash_commands: reply
            .get("slashCommands")
            .filter(|commands| commands.is_object())
            .map(|commands| ContextCommands {
                total: u32_at(commands, "totalCommands"),
                included: u32_at(commands, "includedCommands"),
                tokens: u64_at(commands, "tokens"),
            }),
        deferred_builtin_tools: array_at(reply, "deferredBuiltinTools")
            .iter()
            .filter_map(|tool| {
                Some(DeferredTool {
                    name: string_at(tool, "name")?,
                    tokens: u64_at(tool, "tokens"),
                    loaded: bool_at(tool, "isLoaded"),
                })
            })
            .collect(),
        system_tools: named(array_at(reply, "systemTools")),
        system_prompt_sections: named(array_at(reply, "systemPromptSections")),
        messages: reply
            .get("messageBreakdown")
            .filter(|messages| messages.is_object())
            .map(messages),
        api_usage: reply
            .get("apiUsage")
            .filter(|usage| usage.is_object())
            .map(|usage| ApiUsage {
                input_tokens: u64_at(usage, "input_tokens"),
                output_tokens: u64_at(usage, "output_tokens"),
                cache_creation_input_tokens: u64_at(usage, "cache_creation_input_tokens"),
                cache_read_input_tokens: u64_at(usage, "cache_read_input_tokens"),
            }),
    })
}

/// A row by its `kind`, as Claude Code's schema asks ("Classify on this, never on the English
/// name"). A reply from before `kind` is classified as Claude Code classifies it then, by the
/// free and buffer rows' names and `isDeferred`. A kind this version doesn't know is left out,
/// since its room in the window isn't known.
fn category(row: &Value) -> Option<ContextCategory> {
    let name = str_at(row, "name")?;
    let kind = match str_at(row, "kind") {
        Some("used") => ContextKind::Used,
        Some("free") => ContextKind::Free,
        Some("buffer") => ContextKind::Buffer,
        Some("deferred") => ContextKind::Deferred,
        Some(_) => return None,
        None => match name {
            "Free space" => ContextKind::Free,
            "Autocompact buffer" | "Compact buffer" => ContextKind::Buffer,
            _ if bool_at(row, "isDeferred") => ContextKind::Deferred,
            _ => ContextKind::Used,
        },
    };
    Some(ContextCategory {
        name: name.to_owned(),
        tokens: u64_at(row, "tokens"),
        color: color(row),
        kind,
    })
}

/// Claude Code reads a missing colour as `inactive`.
fn color(row: &Value) -> String {
    string_at(row, "color").unwrap_or_else(|| "inactive".to_owned())
}

fn cell(cell: &Value) -> ContextCell {
    ContextCell {
        category: string_at(cell, "categoryName").unwrap_or_default(),
        color: color(cell),
        filled: bool_at(cell, "isFilled"),
        tokens: u64_at(cell, "tokens"),
        percentage: u32_at(cell, "percentage"),
        fullness: cell
            .get("squareFullness")
            .and_then(Value::as_f64)
            .unwrap_or(1.0),
    }
}

fn memory_file(file: &Value) -> Option<MemoryFile> {
    Some(MemoryFile {
        path: string_at(file, "path")?,
        scope: string_at(file, "type").unwrap_or_default(),
        tokens: u64_at(file, "tokens"),
    })
}

/// A tool the reply doesn't mark is loaded, as it was before tools loaded on demand.
fn mcp_tool(tool: &Value) -> Option<McpTool> {
    Some(McpTool {
        name: string_at(tool, "name")?,
        server: string_at(tool, "serverName").unwrap_or_default(),
        tokens: u64_at(tool, "tokens"),
        loaded: tool.get("isLoaded").and_then(Value::as_bool) != Some(false),
    })
}

fn agent(agent: &Value) -> Option<ContextAgent> {
    Some(ContextAgent {
        name: string_at(agent, "agentType")?,
        source: string_at(agent, "source").unwrap_or_default(),
        tokens: u64_at(agent, "tokens"),
    })
}

fn skills(skills: &Value) -> ContextSkills {
    ContextSkills {
        total: u32_at(skills, "totalSkills"),
        included: u32_at(skills, "includedSkills"),
        tokens: u64_at(skills, "tokens"),
        items: array_at(skills, "skillFrontmatter")
            .iter()
            .filter_map(|skill| {
                Some(ContextSkill {
                    name: string_at(skill, "name")?,
                    source: string_at(skill, "source").unwrap_or_default(),
                    plugin: string_at(skill, "pluginName").filter(|plugin| !plugin.is_empty()),
                    tokens: u64_at(skill, "tokens"),
                })
            })
            .collect(),
    }
}

fn messages(messages: &Value) -> MessageBreakdown {
    MessageBreakdown {
        tool_call_tokens: u64_at(messages, "toolCallTokens"),
        tool_result_tokens: u64_at(messages, "toolResultTokens"),
        attachment_tokens: u64_at(messages, "attachmentTokens"),
        assistant_tokens: u64_at(messages, "assistantMessageTokens"),
        user_tokens: u64_at(messages, "userMessageTokens"),
        redirected_tokens: u64_at(messages, "redirectedContextTokens"),
        unattributed_tokens: u64_at(messages, "unattributedTokens"),
        tools: array_at(messages, "toolCallsByType")
            .iter()
            .filter_map(|tool| {
                Some(ToolTokens {
                    name: string_at(tool, "name")?,
                    call_tokens: u64_at(tool, "callTokens"),
                    result_tokens: u64_at(tool, "resultTokens"),
                })
            })
            .collect(),
        attachments: named(array_at(messages, "attachmentsByType")),
    }
}

fn named(rows: &[Value]) -> Vec<NamedTokens> {
    rows.iter()
        .filter_map(|row| {
            Some(NamedTokens {
                name: string_at(row, "name")?,
                tokens: u64_at(row, "tokens"),
            })
        })
        .collect()
}

fn u32_at(value: &Value, key: &str) -> u32 {
    u32::try_from(u64_at(value, key)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kinds(usage: &ContextUsage) -> Vec<(&str, u64, ContextKind)> {
        usage
            .categories
            .iter()
            .map(|row| (row.name.as_str(), row.tokens, row.kind))
            .collect()
    }

    /// A reply shaped as Claude Code's schema says, with a row and a kind it may add later.
    #[test]
    fn rows_keep_their_names_colours_and_kinds() {
        let reply = json!({
            "categories": [
                {"name": "System prompt", "tokens": 3400, "color": "promptBorder", "kind": "used"},
                {"name": "MCP server instructions", "tokens": 600,
                 "color": "green_FOR_SUBAGENTS_ONLY", "kind": "used"},
                {"name": "MCP tools (deferred)", "tokens": 9000, "color": "inactive",
                 "isDeferred": true, "kind": "deferred"},
                {"name": "Hooks", "tokens": 300, "kind": "used"},
                {"name": "Shared cache", "tokens": 70, "color": "inactive", "kind": "shared"},
                {"name": "Autocompact buffer", "tokens": 50000, "color": "inactive",
                 "kind": "buffer"},
                {"name": "Free space", "tokens": 737500, "color": "promptBorder", "kind": "free"}
            ],
            "totalTokens": 4300,
            "maxTokens": 1000000,
            "percentage": 0,
            "isAutoCompactEnabled": true,
            "autoCompactThreshold": 950000
        });
        let usage = usage(&reply).unwrap();
        assert_eq!(
            kinds(&usage),
            [
                ("System prompt", 3400, ContextKind::Used),
                // Its own row, as Claude Code draws it.
                ("MCP server instructions", 600, ContextKind::Used),
                ("MCP tools (deferred)", 9000, ContextKind::Deferred),
                // A name this version doesn't know is still a used row.
                ("Hooks", 300, ContextKind::Used),
                ("Autocompact buffer", 50000, ContextKind::Buffer),
                ("Free space", 737500, ContextKind::Free),
            ]
        );
        assert_eq!(usage.categories[1].color, "green_FOR_SUBAGENTS_ONLY");
        assert_eq!(usage.categories[3].color, "inactive", "a missing colour");
        assert_eq!((usage.window, usage.raw_window), (1_000_000, 1_000_000));
        assert_eq!(usage.compacts_at(), Some(950_000));
        assert_eq!(usage.auto_compact_source, "model-default");
        assert!(usage.messages.is_none() && usage.api_usage.is_none());
    }

    #[test]
    fn a_reply_without_kinds_is_classified_as_claude_code_did() {
        let reply = json!({"maxTokens": 200000, "isAutoCompactEnabled": false,
        "autoCompactThreshold": 160000,
        "categories": [
            {"name": "Messages", "tokens": 5000},
            {"name": "System tools (deferred)", "tokens": 700, "isDeferred": true},
            {"name": "Compact buffer", "tokens": 3000},
            {"name": "Free space", "tokens": 192000}
        ]});
        let usage = usage(&reply).unwrap();
        assert_eq!(
            kinds(&usage),
            [
                ("Messages", 5000, ContextKind::Used),
                ("System tools (deferred)", 700, ContextKind::Deferred),
                ("Compact buffer", 3000, ContextKind::Buffer),
                ("Free space", 192000, ContextKind::Free),
            ]
        );
        assert_eq!(usage.compacts_at(), None, "auto-compact is off");
    }

    #[test]
    fn a_tool_is_loaded_unless_the_reply_says_it_waits() {
        let reply = json!({"maxTokens": 200000, "mcpTools": [
            {"name": "mcp__docs__read", "serverName": "docs", "tokens": 2000},
            {"name": "mcp__docs__write", "serverName": "docs", "tokens": 900, "isLoaded": false}
        ]});
        let tools = usage(&reply).unwrap().mcp_tools;
        assert_eq!(
            tools
                .iter()
                .map(|tool| (tool.name.as_str(), tool.loaded))
                .collect::<Vec<_>>(),
            [("mcp__docs__read", true), ("mcp__docs__write", false)]
        );
    }

    #[test]
    fn a_reply_with_no_window_gives_nothing() {
        assert_eq!(breakdown(&json!({"categories": []})), None);
    }
}
