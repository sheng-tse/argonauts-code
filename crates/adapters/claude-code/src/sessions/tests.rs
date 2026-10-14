use std::path::Path;

use argonauts_core::Adapter as _;
use time::macros::datetime;

use super::*;
use crate::ClaudeCodeAdapter;

const LOOP: &str = "0a1b2c3d-1111-4aaa-8bbb-000000000001";
const EXPORT: &str = "0a1b2c3d-2222-4aaa-8bbb-000000000002";

/// A home folder holding the recorded `~/.claude/projects`. The fixture can't sit in a
/// `.claude` folder itself, which git ignores here.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("session-store/projects");
    copy_dir(&fixtures, &home.path().join(".claude/projects"));
    home
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn scan(home: &Path) -> CliScan {
    CliScan::new("/work/shop", home)
}

#[test]
fn lists_this_folders_sessions_newest_first() {
    let home = home();
    let sessions = ClaudeCodeAdapter
        .cli_sessions(&scan(home.path()), 20)
        .unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    // Left out: a `-p` session, one with only a command, one carried on under another id, one
    // from `/work-shop`, whose folder name reads the same, and another folder's.
    assert_eq!(ids, vec![EXPORT, LOOP]);

    let export = &sessions[0];
    assert_eq!(export.agent_id.0.as_str(), "claude-code");
    assert_eq!(
        export.title,
        "Fix the flaky export test. It fails on CI about once a day."
    );
    assert_eq!(
        export.prompt.as_deref(),
        Some("Fix the flaky export test.\n\nIt fails on CI about once a day.")
    );
    assert_eq!(export.folder, Path::new("/work/shop"));
    assert_eq!(export.updated, datetime!(2026-09-29 08:05 UTC));
    assert_eq!(export.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(export.branch.as_deref(), Some("fix-export"));
    assert_eq!(
        export.last_message.as_deref(),
        Some("The test races the writer's flush. I made it wait for the file.")
    );

    // A name you gave it wins over Claude Code's own title.
    let input = &sessions[1];
    assert_eq!(input.title, "Input loop notes");
    assert_eq!(
        input.prompt.as_deref(),
        Some("How does the input loop\nread keys?")
    );
    assert_eq!(input.updated, datetime!(2026-09-28 10:04 UTC));
    assert_eq!(input.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(
        input.last_message.as_deref(),
        Some("A paste arrives as one event, so it never runs a command.")
    );
}

#[test]
fn the_newest_written_are_read_first_up_to_the_limit() {
    let home = home();
    let dir = home.path().join(".claude/projects/-work-shop");
    let touch = |id: &str, secs: u64| {
        let file = File::options()
            .append(true)
            .open(dir.join(format!("{id}.jsonl")))
            .unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    };
    touch(LOOP, 2_000_000_000);
    touch(EXPORT, 1_000_000_000);
    let sessions = list(&scan(home.path()), 1).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, LOOP);
}

#[test]
fn a_missing_store_lists_nothing() {
    let empty = tempfile::tempdir().unwrap();
    assert!(list(&scan(empty.path()), 20).unwrap().is_empty());
    let home = home();
    let elsewhere = CliScan::new("/work/none", home.path());
    assert!(list(&elsewhere, 20).unwrap().is_empty());
}

#[test]
fn turns_leave_out_commands_and_subagents() {
    let home = home();
    let turns = ClaudeCodeAdapter
        .cli_session_turns(&scan(home.path()), LOOP, 10)
        .unwrap();
    let said: Vec<(Speaker, &str)> = turns.iter().map(|t| (t.from, t.text.as_str())).collect();
    assert_eq!(
        said,
        vec![
            (Speaker::User, "How does the input loop\nread keys?"),
            (
                Speaker::Agent,
                "I'll read the loop first.\n\nIt reads keys in one loop and sends each to the \
                 focused view."
            ),
            (Speaker::User, "And the paste handling?"),
            (
                Speaker::Agent,
                "A paste arrives as one event,\nso it never runs a command."
            ),
        ]
    );
    let last = turns_of(&home, LOOP, 1);
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].from, Speaker::Agent);
}

#[test]
fn an_agents_turn_keeps_what_it_thought_and_ran_in_order() {
    let home = home();
    let turns = turns_of(&home, LOOP, 10);
    assert!(turns[0].steps.is_empty(), "a person's turn has no steps");
    let read = CliCall {
        ok: Some(true),
        output: Some("fn read_keys() { loop { ... } }".to_owned()),
        ..CliCall::new(
            "toolu_1",
            "Read",
            serde_json::json!({"file_path": "/work/shop/src/input.rs"}),
        )
    };
    assert_eq!(
        turns[1].steps,
        vec![
            // Claude Code keeps only that it thought, with the words signed away.
            CliStep::Thought {
                text: String::new()
            },
            CliStep::Wrote {
                text: "I'll read the loop first.".to_owned()
            },
            CliStep::Ran(read),
            CliStep::Wrote {
                text: "It reads keys in one loop and sends each to the focused view.".to_owned()
            },
        ]
    );
}

/// A command left running reports after your next prompt. Its result still reaches the call
/// that started it, in the turn before.
#[test]
fn a_result_reaches_its_call_in_an_earlier_turn() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".claude/projects/-work-shop");
    fs::create_dir_all(&dir).unwrap();
    let record = |kind: &str, content: serde_json::Value| {
        serde_json::json!({
            "type": kind, "cwd": "/work/shop", "entrypoint": "cli",
            "message": {"role": kind, "model": "claude-opus-5-5", "content": content},
            "timestamp": "2026-09-30T12:00:00Z"
        })
    };
    let lines = [
        record("user", serde_json::json!("Run the slow suite")),
        record(
            "assistant",
            serde_json::json!([{"type": "tool_use", "id": "toolu_slow", "name": "Bash",
                                "input": {"command": "make slow"}}]),
        ),
        record("user", serde_json::json!("And what does tax.rs do?")),
        record(
            "user",
            serde_json::json!([
                {"type": "tool_result", "tool_use_id": "toolu_slow", "content": "42 passed"}
            ]),
        ),
        record(
            "assistant",
            serde_json::json!([{"type": "text", "text": "It rounds the totals."}]),
        ),
    ];
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(dir.join("0a1b2c3d-8888.jsonl"), text).unwrap();

    let turns = turns(&scan(home.path()), "0a1b2c3d-8888", 10).unwrap();
    assert_eq!(turns.len(), 4, "{turns:?}");
    let [CliStep::Ran(slow)] = turns[1].steps.as_slice() else {
        panic!("{:?}", turns[1].steps);
    };
    assert_eq!(slow.ok, Some(true));
    assert_eq!(slow.output.as_deref(), Some("42 passed"));
    assert_eq!(turns[3].text, "It rounds the totals.");
}

/// One line of a transcript: a `kind` record with `content`, and what its tool's result says
/// beyond its text.
fn line(kind: &str, content: serde_json::Value, result: serde_json::Value) -> String {
    let record = serde_json::json!({
        "type": kind, "cwd": "/work/shop", "entrypoint": "cli",
        "isSidechain": false,
        "message": {"role": kind, "model": "claude-opus-5-5", "content": content},
        "toolUseResult": result,
        "timestamp": "2026-09-30T12:00:00Z"
    });
    format!("{record}\n")
}

fn call_line(id: &str, name: &str, input: serde_json::Value) -> String {
    let call = serde_json::json!([{"type": "tool_use", "id": id, "name": name, "input": input}]);
    line("assistant", call, serde_json::Value::Null)
}

fn result_line(id: &str, text: &str, result: serde_json::Value) -> String {
    let content = serde_json::json!([{"type": "tool_result", "tool_use_id": id, "content": text}]);
    line("user", content, result)
}

/// A subagent's own transcript, where Claude Code marks every record as off the main chain.
fn subagent_file(lines: &[String]) -> String {
    lines
        .concat()
        .replace(r#""isSidechain":false"#, r#""isSidechain":true"#)
}

/// The calls among `steps`.
fn calls(steps: &[CliStep]) -> Vec<&CliCall> {
    steps
        .iter()
        .filter_map(|step| match step {
            CliStep::Ran(call) => Some(call),
            _ => None,
        })
        .collect()
}

/// Claude Code keeps what a subagent did in a file of its own beside the session, and the
/// result of the call that started it names the file.
#[test]
fn a_call_that_started_a_subagent_holds_what_the_subagent_did() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".claude/projects/-work-shop");
    let subagents = dir.join("0a1b2c3d-9999/subagents");
    fs::create_dir_all(&subagents).unwrap();
    let text = serde_json::Value::String;
    let none = serde_json::Value::Null;
    let session = [
        line(
            "user",
            text("Where is the tax rounded?".into()),
            none.clone(),
        ),
        call_line(
            "toolu_agent",
            "Agent",
            serde_json::json!({"description": "Find the rounding", "prompt": "Look in src"}),
        ),
        result_line(
            "toolu_agent",
            "It is in src/tax.rs.",
            serde_json::json!({"status": "completed", "agentId": "a1b2c3d4e5f60718"}),
        ),
        // A result that names a file outside the folder is never followed.
        call_line(
            "toolu_out",
            "Agent",
            serde_json::json!({"description": "x"}),
        ),
        result_line(
            "toolu_out",
            "done",
            serde_json::json!({"agentId": "../../../0a1b2c3d-9999"}),
        ),
        line(
            "assistant",
            serde_json::json!([{"type": "text", "text": "In src/tax.rs."}]),
            none.clone(),
        ),
    ];
    fs::write(dir.join("0a1b2c3d-9999.jsonl"), session.concat()).unwrap();
    let explorer = [
        line("user", text("Look in src".into()), none.clone()),
        call_line(
            "toolu_grep",
            "Grep",
            serde_json::json!({"pattern": "round"}),
        ),
        result_line("toolu_grep", "src/tax.rs:7", none.clone()),
        call_line(
            "toolu_inner",
            "Agent",
            serde_json::json!({"description": "Read it"}),
        ),
        result_line(
            "toolu_inner",
            "It rounds half up.",
            serde_json::json!({"agentId": "a0000002"}),
        ),
        line(
            "assistant",
            serde_json::json!([{"type": "text", "text": "It is in src/tax.rs."}]),
            none.clone(),
        ),
    ];
    fs::write(
        subagents.join("agent-a1b2c3d4e5f60718.jsonl"),
        subagent_file(&explorer),
    )
    .unwrap();
    // The inner one names itself as its own subagent, which is read no deeper than the limit.
    let reader = [
        call_line(
            "toolu_read",
            "Read",
            serde_json::json!({"file_path": "src/tax.rs"}),
        ),
        result_line("toolu_read", "fn total() {}", none.clone()),
        call_line(
            "toolu_self",
            "Agent",
            serde_json::json!({"description": "Again"}),
        ),
        result_line(
            "toolu_self",
            "again",
            serde_json::json!({"agentId": "a0000002"}),
        ),
    ];
    fs::write(
        subagents.join("agent-a0000002.jsonl"),
        subagent_file(&reader),
    )
    .unwrap();

    let turns = turns(&scan(home.path()), "0a1b2c3d-9999", 10).unwrap();
    let asked = calls(&turns[1].steps);
    let [agent, outside] = asked.as_slice() else {
        panic!("{:?}", turns[1].steps);
    };
    assert_eq!(agent.output.as_deref(), Some("It is in src/tax.rs."));
    assert!(outside.steps.is_empty(), "{:?}", outside.steps);
    // The subagent's prompt is the call's input, and what it wrote stays among its steps.
    assert_eq!(
        agent.steps.last(),
        Some(&CliStep::Wrote {
            text: "It is in src/tax.rs.".to_owned()
        })
    );
    let explored = calls(&agent.steps);
    let [grep, inner] = explored.as_slice() else {
        panic!("{:?}", agent.steps);
    };
    assert_eq!((grep.name.as_str(), grep.ok), ("Grep", Some(true)));
    assert_eq!(grep.output.as_deref(), Some("src/tax.rs:7"));
    let read = calls(&inner.steps);
    assert_eq!(read[0].name, "Read");
    assert_eq!(read[0].output.as_deref(), Some("fn total() {}"));

    // It ends: the one that names itself is read down to the limit, where its call holds nothing.
    let mut deepest = *inner;
    let mut files_read = 1;
    while let Some(next) = calls(&deepest.steps).get(1).copied() {
        deepest = next;
        files_read += 1;
    }
    assert!(deepest.steps.is_empty(), "{:?}", deepest.steps);
    assert_eq!(files_read, SUBAGENT_DEPTH);

    // The list reads titles and dates alone, never a subagent's file.
    let listed = list(&scan(home.path()), 10).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].turns, 1);
}

#[test]
fn an_edit_keeps_the_patch_its_result_reported() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".claude/projects/-work-shop");
    fs::create_dir_all(&dir).unwrap();
    let record = |kind: &str, content: serde_json::Value| {
        serde_json::json!({
            "type": kind, "cwd": "/work/shop", "entrypoint": "cli",
            "message": {"role": kind, "model": "claude-opus-5-5", "content": content},
            "timestamp": "2026-09-30T12:00:00Z"
        })
    };
    let input = serde_json::json!({
        "file_path": "/work/shop/src/tax.rs", "old_string": "round()", "new_string": "floor()"
    });
    let mut result = record(
        "user",
        serde_json::json!([{"type": "tool_result", "tool_use_id": "toolu_9", "content": "ok"}]),
    );
    result["toolUseResult"] = serde_json::json!({
        "filePath": "/work/shop/src/tax.rs",
        "structuredPatch": [{"oldStart": 7, "oldLines": 1, "newStart": 7, "newLines": 1,
                             "lines": ["-    total.round()", "+    total.floor()"]}]
    });
    let failed = serde_json::json!([
        {"type": "tool_result", "tool_use_id": "toolu_10", "is_error": true, "content": "exit 1"}
    ]);
    let lines = [
        record("user", serde_json::json!("Fix the rounding")),
        record(
            "assistant",
            serde_json::json!([{"type": "thinking", "thinking": "Round down, not half up."}]),
        ),
        record(
            "assistant",
            serde_json::json!([{"type": "tool_use", "id": "toolu_9", "name": "Edit", "input": input}]),
        ),
        result,
        record(
            "assistant",
            serde_json::json!([{"type": "tool_use", "id": "toolu_10", "name": "Bash",
                                "input": {"command": "cargo test"}}]),
        ),
        record("user", failed),
        record(
            "assistant",
            serde_json::json!([{"type": "tool_use", "id": "toolu_11", "name": "Bash",
                                "input": {"command": "cargo test tax"}}]),
        ),
    ];
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(dir.join("0a1b2c3d-7777.jsonl"), text).unwrap();

    let turns = turns(&scan(home.path()), "0a1b2c3d-7777", 10).unwrap();
    assert_eq!(turns.len(), 2);
    // Nothing was written, so no answer carries into a fresh session.
    assert_eq!(turns[1].text, "");
    let [
        CliStep::Thought { text },
        CliStep::Ran(edit),
        CliStep::Ran(failed),
        CliStep::Ran(cut),
    ] = turns[1].steps.as_slice()
    else {
        panic!("{:?}", turns[1].steps);
    };
    assert_eq!(text, "Round down, not half up.");
    assert_eq!(edit.ok, Some(true));
    assert_eq!(
        edit.patches,
        vec![CliPatch {
            path: "/work/shop/src/tax.rs".to_owned(),
            hunks: "@@ -7,1 +7,1 @@\n-    total.round()\n+    total.floor()\n".to_owned(),
            numbered: true,
        }]
    );
    assert_eq!(
        (failed.ok, failed.output.as_deref()),
        (Some(false), Some("exit 1"))
    );
    // The transcript ends before this call does.
    assert_eq!((cut.ok, cut.output.as_deref()), (None, None));
}

fn turns_of(home: &tempfile::TempDir, id: &str, count: usize) -> Vec<CliTurn> {
    turns(&scan(home.path()), id, count).unwrap()
}

#[test]
fn turns_of_an_unknown_or_unsafe_id_fail() {
    let home = home();
    let missing = turns(&scan(home.path()), "0a1b2c3d-9999", 4).unwrap_err();
    assert!(missing.to_string().contains("no session"), "{missing}");
    assert!(turns(&scan(home.path()), "../../etc/passwd", 4).is_err());
}

#[test]
fn folder_names_follow_claude_code() {
    assert_eq!(folder_name(Path::new("/work/my.app")), "-work-my-app");
    assert_eq!(
        folder_name(Path::new("/Users/me/github/argonauts")),
        "-Users-me-github-argonauts"
    );
    assert_eq!(folder_name(Path::new("/tmp/café")), "-tmp-caf-");
    assert_eq!(folder_name(Path::new("/tmp/a😀b")), "-tmp-a--b");
}

#[test]
fn a_long_folder_is_found_by_its_first_part() {
    let home = tempfile::tempdir().unwrap();
    let folder = format!("/work/{}", "deep/".repeat(50));
    let folder = folder.trim_end_matches('/');
    let name = folder_name(Path::new(folder));
    let dir = home
        .path()
        .join(".claude/projects")
        .join(format!("{}-1abc2de", &name[..LONGEST_FOLDER_NAME]));
    fs::create_dir_all(&dir).unwrap();
    let line = serde_json::json!({
        "type": "user", "cwd": folder, "entrypoint": "cli",
        "message": {"role": "user", "content": "Deep in"},
        "timestamp": "2026-09-30T12:00:00Z"
    });
    fs::write(dir.join("0a1b2c3d-8888.jsonl"), format!("{line}\n")).unwrap();
    let sessions = list(&CliScan::new(folder, home.path()), 20).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title, "Deep in");
}

#[test]
fn only_text_wrapped_in_tags_is_the_clis_own() {
    assert!(wrapped_in_tags(
        "<command-name>/model</command-name>\n<command-args></command-args>"
    ));
    assert!(wrapped_in_tags(
        "<bash-stdout>ok</bash-stdout><bash-stderr></bash-stderr>"
    ));
    assert!(!wrapped_in_tags("<div> is off by one"));
    assert!(!wrapped_in_tags("Why does <b>x</b> fail?"));
    assert!(!wrapped_in_tags("plain"));
}

#[test]
fn long_titles_fold_to_one_cut_line() {
    let long = "word ".repeat(100);
    let line = one_line(&long);
    assert_eq!(line.chars().count(), LINE_CHARS);
    assert!(line.ends_with('…'));
    assert_eq!(one_line("  a\n\tb  "), "a b");
}

#[test]
fn every_folder_lists_the_other_folders_sessions_with_where_each_ran() {
    let home = home();
    let adapter = ClaudeCodeAdapter;
    let here = adapter.cli_sessions(&scan(home.path()), 20).unwrap();
    let all = adapter
        .cli_sessions(&scan(home.path()).everywhere(), 20)
        .unwrap();
    assert!(all.len() > here.len(), "{} of {}", here.len(), all.len());
    assert!(here.iter().all(|s| s.folder == Path::new("/work/shop")));
    assert!(all.iter().any(|s| s.folder != Path::new("/work/shop")));
    for session in &here {
        assert!(all.iter().any(|s| s.id == session.id), "{}", session.id);
    }
    assert!(
        all.windows(2)
            .all(|pair| pair[0].updated >= pair[1].updated)
    );
}

#[test]
fn a_sessions_length_counts_your_prompts() {
    let home = home();
    let adapter = ClaudeCodeAdapter;
    let scan = scan(home.path());
    let sessions = adapter.cli_sessions(&scan, 20).unwrap();
    assert!(!sessions.is_empty());
    for session in sessions {
        let turns = adapter
            .cli_session_turns(&scan, &session.id, usize::MAX)
            .unwrap();
        let prompts = turns
            .iter()
            .filter(|turn| turn.from == Speaker::User)
            .count();
        assert!(prompts > 0);
        assert_eq!(session.turns as usize, prompts, "{}", session.id);
    }
}
