//! `get_context_usage` asked with `full`, on two replies made up from the shapes in Claude
//! Code 2.1.296's program text: a 1M session at 75%, and the same session 22.4k short of
//! compacting. No session answered them. Their rows, grid and lists follow the program's own
//! rules, and their numbers are the ones the Context tab's pictures are drawn from.
//!
//! `<name>.reply.json` is the reply as Claude Code would send it, and `<name>.usage.json` the
//! same count as Argonauts Code keeps it, which the screen's tests draw.

use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_core::AgentEvent;
use argonauts_core::adapter::Adapter;
use argonauts_core::events::context::{ContextDetail, ContextKind, ContextUsage};
use serde_json::{Value, json};

const FULL: (&str, &str) = (
    include_str!("data/context-usage-full.reply.json"),
    include_str!("data/context-usage-full.usage.json"),
);
const NEAR_FULL: (&str, &str) = (
    include_str!("data/context-usage-near-full.reply.json"),
    include_str!("data/context-usage-near-full.usage.json"),
);

/// What the parser makes of `reply`, answering the request the adapter writes for `full`.
fn answered(reply: &str) -> ContextUsage {
    let asked: Value = serde_json::from_str(
        &ClaudeCodeAdapter
            .context_usage_line(ContextDetail::Full)
            .expect("Claude Code can say"),
    )
    .unwrap();
    assert_eq!(
        asked["request"],
        json!({"subtype": "get_context_usage", "detail": "full"})
    );
    let reply: Value = serde_json::from_str(reply).unwrap();
    let line = json!({"type": "control_response", "response": {
        "subtype": "success", "request_id": asked["request_id"], "response": reply}});
    let mut parser = ClaudeCodeAdapter.parser();
    let events = parser.parse_stdout_line(&line.to_string());
    let [AgentEvent::ContextBreakdown { usage }] = events.as_slice() else {
        panic!("{events:?}");
    };
    (**usage).clone()
}

#[test]
fn every_field_of_the_reply_is_kept() {
    for (reply, kept) in [FULL, NEAR_FULL] {
        let kept: ContextUsage = serde_json::from_str(kept).unwrap();
        assert_eq!(answered(reply), kept);
    }
}

#[test]
fn the_reply_at_three_quarters_reads_as_the_picture_counts_it() {
    let usage = answered(FULL.0);
    assert_eq!(
        (usage.total_tokens, usage.raw_window, usage.percentage),
        (750_000, 1_000_000, 75)
    );
    assert_eq!(usage.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(usage.compacts_at(), Some(967_000));
    assert_eq!(usage.auto_compact_source, "model-default");

    // Ten rows of twenty cells for a 1M window: 153 used, 40 free, and the buffer's 7 last.
    assert_eq!(usage.grid.len(), 10);
    assert!(usage.grid.iter().all(|row| row.len() == 20));
    let cells: Vec<_> = usage.grid.iter().flatten().collect();
    let of = |name: &str| cells.iter().filter(|cell| cell.category == name).count();
    assert_eq!(
        (of("Messages"), of("Free space"), of("Autocompact buffer")),
        (144, 40, 7)
    );
    assert_eq!(cells[199].category, "Autocompact buffer");
    // The part of a cell a row fills decides its glyph.
    let part = |name: &str| cells.iter().rfind(|cell| cell.category == name).unwrap();
    assert!((part("MCP tools").fullness - 0.508).abs() < 1e-9);
    assert!(part("Memory files").fullness > 0.7);

    // The rows out of the window keep their kind and their tokens.
    assert_eq!(usage.tokens_of(ContextKind::Deferred), 25_213 + 1_910);
    assert_eq!(usage.tokens_of(ContextKind::Used), usage.total_tokens);
    assert_eq!(usage.tokens_of(ContextKind::Free), 217_000);

    let waiting: Vec<&str> = usage
        .mcp_tools
        .iter()
        .filter(|tool| !tool.loaded)
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(
        waiting,
        [
            "mcp__github__list_commits",
            "mcp__linear__create_issue",
            "mcp__linear__search_issues"
        ]
    );
    assert_eq!(usage.agents[0].source, "projectSettings");
    assert_eq!(usage.memory_files[2].scope, "Local");
    let skills = usage.skills.as_ref().unwrap();
    assert_eq!((skills.tokens, skills.items.len()), (710, 5));

    let messages = usage.messages.as_ref().unwrap();
    assert_eq!(messages.tool_result_tokens, 528_600);
    assert_eq!(
        messages
            .tools
            .iter()
            .map(|tool| tool.result_tokens)
            .sum::<u64>(),
        messages.tool_result_tokens
    );
    assert_eq!(
        (messages.tools[0].name.as_str(), messages.tools[0].total()),
        ("Bash", 180_200)
    );
    let last = usage.api_usage.unwrap();
    assert_eq!(
        (last.sent(), last.cache_read_input_tokens),
        (750_000, 741_600)
    );
}
