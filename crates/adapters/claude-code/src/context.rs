//! How full the main agent's context is. Each API request sends the whole conversation, so its
//! input plus cache reads and writes is what the context holds. Subagents have their own. What
//! it holds by category comes from `get_context_usage`, the numbers /context draws.

use std::collections::BTreeMap;

use argonauts_core::AgentEvent;
use argonauts_core::events::context::{ContextCategory, ContextItem, ContextPart};
use serde_json::Value;

use crate::json::{array_at, bool_at, str_at, string_at, u64_at};

#[derive(Debug, Default)]
pub struct ContextTracker {
    /// The main model's id from `init`, which names its `modelUsage` entry.
    model: Option<String>,
    used: Option<u64>,
    /// Learned from the first result, since nothing earlier reports it.
    window: Option<u64>,
}

impl ContextTracker {
    pub fn set_model(&mut self, model: Option<String>) {
        self.model = model;
    }

    /// Only changes are reported, since every content block repeats its message's usage.
    pub fn observe(&mut self, usage: &Value) -> Option<AgentEvent> {
        let used = u64_at(usage, "input_tokens")
            .saturating_add(u64_at(usage, "cache_read_input_tokens"))
            .saturating_add(u64_at(usage, "cache_creation_input_tokens"));
        if used == 0 || self.used == Some(used) {
            return None;
        }
        self.used = Some(used);
        Some(self.event(used))
    }

    /// Reads the window size from a result's `modelUsage`, and reports the last fill again
    /// once the size is first known.
    pub fn observe_result(&mut self, result: &Value) -> Option<AgentEvent> {
        let window = self.window_in(result.get("modelUsage")?)?;
        let first = self.window.is_none();
        self.window = Some(window);
        let used = self.used?;
        first.then(|| self.event(used))
    }

    fn window_in(&self, model_usage: &Value) -> Option<u64> {
        let models = model_usage.as_object()?;
        let entry = match (&self.model, models.len()) {
            (Some(model), _) if models.contains_key(model) => models.get(model)?,
            // With one model there is no doubt which window it is.
            (_, 1) => models.values().next()?,
            _ => return None,
        };
        entry.get("contextWindow")?.as_u64().filter(|w| *w > 0)
    }

    fn event(&self, used: u64) -> AgentEvent {
        AgentEvent::ContextFill {
            used,
            size: self.window,
            sent: None,
        }
    }
}

/// The `get_context_usage` reply as a breakdown. Rows are classified by `kind`, as Claude
/// Code's schema asks ("Classify on this, never on the English name"): a `used` row goes into
/// the category its name says, or `other` when the name is new; `free` is the room left;
/// `buffer` is the compaction reserve; `deferred` rows are out of the window and left out.
/// `None` when the reply names no window.
pub fn breakdown(reply: &Value) -> Option<AgentEvent> {
    let window = u64_at(reply, "maxTokens");
    if window == 0 {
        return None;
    }
    let mut parts: BTreeMap<ContextPart, u64> = BTreeMap::new();
    for row in array_at(reply, "categories") {
        let Some(part) = part_of(row) else {
            continue;
        };
        let tokens = u64_at(row, "tokens");
        if tokens > 0 {
            *parts.entry(part).or_default() += tokens;
        }
    }
    let auto_compact_at = bool_at(reply, "isAutoCompactEnabled")
        .then(|| u64_at(reply, "autoCompactThreshold"))
        .filter(|at| *at > 0);
    Some(AgentEvent::ContextBreakdown {
        categories: parts
            .into_iter()
            .map(|(part, tokens)| ContextCategory { part, tokens })
            .collect(),
        auto_compact_at,
        window,
        model: string_at(reply, "model").filter(|model| !model.is_empty()),
        memory_files: items(array_at(reply, "memoryFiles"), "path", "type"),
        // A tool loaded on demand takes room only once it's loaded.
        mcp_tools: items(
            array_at(reply, "mcpTools")
                .iter()
                .filter(|tool| tool.get("isLoaded").and_then(Value::as_bool) != Some(false)),
            "name",
            "serverName",
        ),
        agents: items(array_at(reply, "agents"), "agentType", "source"),
        skills: reply
            .get("skills")
            .map(|skills| items(array_at(skills, "skillFrontmatter"), "name", "source"))
            .unwrap_or_default(),
    })
}

/// Where a row goes, by its `kind`. A reply from before `kind` is classified as Claude Code
/// classifies it then: by the free and buffer rows' names and `isDeferred`.
fn part_of(row: &Value) -> Option<ContextPart> {
    let name = str_at(row, "name").unwrap_or_default();
    let kind = str_at(row, "kind").unwrap_or_else(|| match name {
        "Free space" => "free",
        "Autocompact buffer" | "Compact buffer" => "buffer",
        _ if bool_at(row, "isDeferred") => "deferred",
        _ => "used",
    });
    match kind {
        "used" => Some(used_part(name)),
        "free" => Some(ContextPart::Free),
        "buffer" => Some(ContextPart::Buffer),
        // `deferred`, and a kind Claude Code adds later, whose room in the window isn't known.
        _ => None,
    }
}

/// A used row by its name, as Claude Code 2.1.289 names them.
fn used_part(name: &str) -> ContextPart {
    match name {
        "System prompt" => ContextPart::SystemPrompt,
        "System tools" => ContextPart::Tools,
        "MCP tools" | "MCP server instructions" => ContextPart::McpTools,
        "Custom agents" => ContextPart::Agents,
        "Memory files" => ContextPart::MemoryFiles,
        "Skills" => ContextPart::Skills,
        "Messages" => ContextPart::Messages,
        _ => ContextPart::Other,
    }
}

fn items<'a>(
    rows: impl IntoIterator<Item = &'a Value>,
    name: &str,
    source: &str,
) -> Vec<ContextItem> {
    rows.into_iter()
        .filter_map(|row| {
            Some(ContextItem {
                name: string_at(row, name)?,
                tokens: u64_at(row, "tokens"),
                source: string_at(row, source).filter(|source| !source.is_empty()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fill(used: u64, size: Option<u64>) -> Option<AgentEvent> {
        Some(AgentEvent::ContextFill {
            used,
            size,
            sent: None,
        })
    }

    #[test]
    fn input_and_cache_tokens_fill_the_context() {
        let mut tracker = ContextTracker::default();
        tracker.set_model(Some("claude-haiku-4-5".into()));
        let usage = json!({"input_tokens": 10, "cache_read_input_tokens": 4563,
                           "cache_creation_input_tokens": 1798, "output_tokens": 43});
        assert_eq!(tracker.observe(&usage), fill(6371, None));
        assert_eq!(tracker.observe(&usage), None);
        assert_eq!(tracker.observe(&json!({"output_tokens": 5})), None);

        let result = json!({"modelUsage": {
            "claude-haiku-4-5": {"contextWindow": 200000},
            "claude-sonnet-5": {"contextWindow": 1000000}
        }});
        assert_eq!(tracker.observe_result(&result), fill(6371, Some(200000)));
        assert_eq!(tracker.observe_result(&result), None);
        assert_eq!(
            tracker.observe(&json!({"input_tokens": 7000})),
            fill(7000, Some(200000))
        );
    }

    #[test]
    fn an_unknown_model_among_several_gives_no_window() {
        let mut tracker = ContextTracker::default();
        let result = json!({"modelUsage": {"a": {"contextWindow": 1}, "b": {"contextWindow": 2}}});
        assert_eq!(tracker.observe_result(&result), None);
        assert_eq!(tracker.observe(&json!({"input_tokens": 3})), fill(3, None));
        let single = json!({"modelUsage": {"a": {"contextWindow": 1000}}});
        assert_eq!(tracker.observe_result(&single), fill(3, Some(1000)));
    }

    /// A reply shaped as Claude Code 2.1.289's schema says, with a row it may add later.
    fn usage_reply() -> Value {
        json!({
            "categories": [
                {"name": "System prompt", "tokens": 3400, "color": "promptBorder", "kind": "used"},
                {"name": "System tools", "tokens": 12000, "color": "inactive", "kind": "used"},
                {"name": "MCP tools", "tokens": 2000, "color": "cyan", "kind": "used"},
                {"name": "MCP server instructions", "tokens": 600, "color": "green",
                 "kind": "used"},
                {"name": "MCP tools (deferred)", "tokens": 9000, "color": "inactive",
                 "isDeferred": true, "kind": "deferred"},
                {"name": "Custom agents", "tokens": 1100, "color": "permission", "kind": "used"},
                {"name": "Memory files", "tokens": 4900, "color": "claude", "kind": "used"},
                {"name": "Skills", "tokens": 2200, "color": "warning", "kind": "used"},
                {"name": "Hooks", "tokens": 300, "color": "inactive", "kind": "used"},
                {"name": "Messages", "tokens": 186000, "color": "purple", "kind": "used"},
                {"name": "Autocompact buffer", "tokens": 50000, "color": "inactive",
                 "kind": "buffer"},
                {"name": "Free space", "tokens": 737500, "color": "promptBorder", "kind": "free"}
            ],
            "totalTokens": 212200,
            "maxTokens": 1000000,
            "rawMaxTokens": 1000000,
            "percentage": 21,
            "gridRows": [],
            "model": "claude-opus-5-5",
            "memoryFiles": [{"path": "/repo/CLAUDE.md", "type": "Project", "tokens": 4900}],
            "mcpTools": [
                {"name": "mcp__docs__read", "serverName": "docs", "tokens": 2000,
                 "isLoaded": true},
                {"name": "mcp__docs__write", "serverName": "docs", "tokens": 900,
                 "isLoaded": false}
            ],
            "agents": [{"agentType": "reviewer", "source": "projectSettings", "tokens": 1100}],
            "skills": {"totalSkills": 1, "includedSkills": 1, "tokens": 2200,
                       "skillFrontmatter": [{"name": "docs", "source": "plugin", "tokens": 2200}]},
            "autoCompactThreshold": 950000,
            "isAutoCompactEnabled": true,
            "apiUsage": null
        })
    }

    fn parts(event: &AgentEvent) -> Vec<(ContextPart, u64)> {
        let AgentEvent::ContextBreakdown { categories, .. } = event else {
            panic!("{event:?}");
        };
        categories.iter().map(|c| (c.part, c.tokens)).collect()
    }

    #[test]
    fn the_reply_is_classified_by_kind_in_contexts_order() {
        let event = breakdown(&usage_reply()).unwrap();
        assert_eq!(
            parts(&event),
            [
                (ContextPart::SystemPrompt, 3400),
                (ContextPart::Tools, 12000),
                // MCP's instructions count with its tools.
                (ContextPart::McpTools, 2600),
                (ContextPart::Agents, 1100),
                (ContextPart::MemoryFiles, 4900),
                (ContextPart::Skills, 2200),
                (ContextPart::Messages, 186000),
                // A name this version doesn't know.
                (ContextPart::Other, 300),
                (ContextPart::Free, 737500),
                // Held past the compaction point. The deferred tools are out of the window.
                (ContextPart::Buffer, 50000),
            ]
        );
        let AgentEvent::ContextBreakdown {
            auto_compact_at,
            window,
            model,
            memory_files,
            mcp_tools,
            agents,
            skills,
            ..
        } = event
        else {
            unreachable!()
        };
        assert_eq!((auto_compact_at, window), (Some(950_000), 1_000_000));
        assert_eq!(model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(memory_files[0].name, "/repo/CLAUDE.md");
        assert_eq!(memory_files[0].source.as_deref(), Some("Project"));
        // A tool that isn't loaded takes no room yet.
        assert_eq!(mcp_tools.len(), 1);
        assert_eq!(mcp_tools[0].source.as_deref(), Some("docs"));
        assert_eq!(agents[0].name, "reviewer");
        assert_eq!((skills[0].name.as_str(), skills[0].tokens), ("docs", 2200));
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
        let event = breakdown(&reply).unwrap();
        assert_eq!(
            parts(&event),
            [
                (ContextPart::Messages, 5000),
                (ContextPart::Free, 192000),
                (ContextPart::Buffer, 3000),
            ]
        );
        let AgentEvent::ContextBreakdown {
            auto_compact_at, ..
        } = event
        else {
            unreachable!()
        };
        assert_eq!(auto_compact_at, None, "auto-compact is off");
    }

    #[test]
    fn a_reply_with_no_window_gives_nothing() {
        assert_eq!(breakdown(&json!({"categories": []})), None);
    }
}
