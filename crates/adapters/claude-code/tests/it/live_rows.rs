//! A tool call is known the moment the model starts writing it. Claude Code 2.1.295 streams a
//! turn in this order: the block's start with the call's id and name, its input in pieces, the
//! whole call as an assistant message, and only then the block's stop. The next call can start
//! before the one before it has its result.

use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_core::{Adapter, AgentEvent};
use serde_json::{Value, json};

fn feed(lines: &[Value]) -> Vec<AgentEvent> {
    let mut parser = ClaudeCodeAdapter.parser();
    lines
        .iter()
        .flat_map(|line| parser.parse_stdout_line(&line.to_string()))
        .collect()
}

fn init() -> Value {
    json!({"type": "system", "subtype": "init", "cwd": "/work/captain", "session_id": "s-1",
           "tools": ["Bash", "Edit", "Read"], "model": "claude-haiku-5-5"})
}

fn message_start(id: &str) -> Value {
    json!({"type": "stream_event", "parent_tool_use_id": null, "event": {
        "type": "message_start", "message": {"id": id, "usage": {"input_tokens": 3, "output_tokens": 1}}}})
}

fn block_start(index: u32, id: &str, name: &str) -> Value {
    json!({"type": "stream_event", "parent_tool_use_id": null, "event": {
        "type": "content_block_start", "index": index,
        "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}, "caller": {"type": "direct"}}}})
}

fn input_piece(index: u32, json: &str) -> Value {
    json!({"type": "stream_event", "parent_tool_use_id": null, "event": {
        "type": "content_block_delta", "index": index,
        "delta": {"type": "input_json_delta", "partial_json": json}}})
}

fn whole_call(message: &str, id: &str, name: &str, input: Value) -> Value {
    json!({"type": "assistant", "parent_tool_use_id": null, "message": {
        "id": message, "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
}

fn block_stop(index: u32) -> Value {
    json!({"type": "stream_event", "parent_tool_use_id": null, "event": {
        "type": "content_block_stop", "index": index}})
}

fn message_stop() -> Value {
    json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "message_stop"}})
}

fn result_of(id: &str, text: &str) -> Value {
    json!({"type": "user", "parent_tool_use_id": null, "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": text}]}})
}

fn turn_end() -> Value {
    json!({"type": "result", "subtype": "success", "is_error": false, "result": "Done.",
           "usage": {"input_tokens": 3, "output_tokens": 9}})
}

/// The tool events in order, as `pending Edit e1`, `started e1`, `finished e1`, `dropped e1`.
fn tool_steps(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolPending { id, name, .. } => Some(format!("pending {name} {id}")),
            AgentEvent::ToolStarted { id, .. } => Some(format!("started {id}")),
            AgentEvent::ToolFinished { id, .. } => Some(format!("finished {id}")),
            AgentEvent::ToolDropped { id, .. } => Some(format!("dropped {id}")),
            _ => None,
        })
        .collect()
}

#[test]
fn a_call_is_known_as_the_model_starts_writing_it() {
    let edit = json!({"file_path": "/work/captain/invoice/money.py",
                      "old_string": "a", "new_string": "b"});
    let events = feed(&[
        init(),
        message_start("m1"),
        block_start(0, "e1", "Edit"),
        input_piece(0, "{\"file_path\": \"/work/captain/invoice/"),
        whole_call("m1", "e1", "Edit", edit),
        block_stop(0),
        block_start(1, "b1", "Bash"),
        result_of("e1", "The file has been updated successfully."),
        whole_call(
            "m1",
            "b1",
            "Bash",
            json!({"command": "python3 -m unittest -q"}),
        ),
        block_stop(1),
        message_stop(),
        result_of("b1", "Ran 8 tests in 0.002s\n\nOK"),
        turn_end(),
    ]);
    assert_eq!(
        tool_steps(&events),
        [
            "pending Edit e1",
            "started e1",
            "pending Bash b1",
            "finished e1",
            "started b1",
            "finished b1",
        ]
    );
}

#[test]
fn a_call_the_model_never_finished_is_dropped() {
    // The message ends without the call, as when a request is cut off and tried again.
    let events = feed(&[
        init(),
        message_start("m1"),
        block_start(0, "w1", "Write"),
        input_piece(0, "{\"file_path\": "),
        message_stop(),
        message_start("m2"),
        block_start(0, "w2", "Write"),
        input_piece(0, "{\"file_path\": "),
        turn_end(),
    ]);
    assert_eq!(
        tool_steps(&events),
        [
            "pending Write w1",
            "dropped w1",
            "pending Write w2",
            "dropped w2"
        ]
    );
}

#[test]
fn a_stream_cut_mid_call_drops_it_with_the_session() {
    let mut parser = ClaudeCodeAdapter.parser();
    let mut events = Vec::new();
    for line in [init(), message_start("m1"), block_start(0, "r1", "Read")] {
        events.extend(parser.parse_stdout_line(&line.to_string()));
    }
    events.extend(parser.finish(Some(1)));
    assert_eq!(tool_steps(&events), ["pending Read r1", "dropped r1"]);
}

#[test]
fn text_blocks_and_thinking_have_no_row() {
    let events = feed(&[
        init(),
        message_start("m1"),
        json!({"type": "stream_event", "parent_tool_use_id": null, "event": {
            "type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}}),
        json!({"type": "stream_event", "parent_tool_use_id": null, "event": {
            "type": "content_block_start", "index": 1,
            "content_block": {"type": "thinking", "thinking": "", "signature": ""}}}),
        message_stop(),
        turn_end(),
    ]);
    assert!(tool_steps(&events).is_empty(), "{events:#?}");
}
