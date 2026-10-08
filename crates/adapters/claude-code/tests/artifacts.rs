//! Claude Code's Artifact tool in the stream. No artifact was published to
//! record this, since that makes a page on the account that runs the check: the lines in
//! `data/artifact-2.1.292.stdout.jsonl` are built to the shape Claude Code 2.1.292's program
//! text gives them. The call is a `tool_use` named `Artifact` whose input names the file. The
//! result's text is the tool's own, `Published <file> at <url> (Version N)…`, and the user
//! line carries the tool's structured result (`url`, `path`, `title`, `updated`, `seq`).
//! What a real stream may send beyond that isn't known here.

use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_core::AgentEvent;
use argonauts_core::adapter::Adapter;
use argonauts_core::artifacts::{self, Artifact};
use serde_json::Value;

const STREAM: &str = include_str!("data/artifact-2.1.292.stdout.jsonl");
const CHART_PAGE: &str = "https://claude.ai/code/artifact/0b1e7c9a-4f1d-4c57-9a55-2f6f0d3a8e11";
const REPORT: &str = "https://claude.ai/code/artifact/7d2c4b18-93aa-4e0f-8c61-5b0e9f1a2c33";

/// Every call of the tool with how it ended: its input, whether it went through, what it
/// said, and the subagent that made it.
fn calls() -> Vec<(Value, bool, String, Option<String>)> {
    let mut parser = ClaudeCodeAdapter.parser();
    let events: Vec<AgentEvent> = STREAM
        .lines()
        .flat_map(|line| parser.parse_stdout_line(line))
        .collect();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Unknown { .. })),
        "{events:#?}"
    );
    let mut calls = Vec::new();
    for event in &events {
        let AgentEvent::ToolStarted {
            id,
            name,
            input,
            parent,
        } = event
        else {
            continue;
        };
        assert_eq!(name, artifacts::TOOL);
        let ended = events.iter().find_map(|event| match event {
            AgentEvent::ToolFinished {
                id: done,
                ok,
                output,
                ..
            } if done == id => Some((*ok, output.clone().unwrap_or_default())),
            _ => None,
        });
        let (ok, output) = ended.unwrap_or_else(|| panic!("{id} never finished"));
        calls.push((input.clone(), ok, output, parent.clone()));
    }
    calls
}

#[test]
fn a_published_page_reads_from_its_call_and_result() {
    let calls = calls();
    assert_eq!(calls.len(), 5);
    let pages: Vec<Option<Artifact>> = calls
        .iter()
        .map(|(input, ok, output, _)| {
            (*ok && artifacts::publishes(artifacts::TOOL, input))
                .then(|| artifacts::published(input, output))
                .flatten()
        })
        .collect();
    let named = |page: &Option<Artifact>| {
        page.as_ref()
            .map(|page| (page.name.clone(), page.url.clone()))
    };
    assert_eq!(
        named(&pages[0]),
        Some(("weekly-usage-chart".to_owned(), CHART_PAGE.to_owned()))
    );
    // Published again, it is the same page at the same address.
    assert_eq!(named(&pages[1]), named(&pages[0]));
    // A subagent's own, with the call that started the subagent as its parent.
    assert_eq!(
        named(&pages[2]),
        Some(("test-report".to_owned(), REPORT.to_owned()))
    );
    assert_eq!(calls[2].3.as_deref(), Some("toolu_agent0001"));
    // A refused publish and a listing publish nothing, whatever addresses they mention.
    assert_eq!(pages[3], None);
    assert!(!calls[3].1);
    assert_eq!(pages[4], None);
    assert!(calls[4].2.contains(CHART_PAGE));
}
