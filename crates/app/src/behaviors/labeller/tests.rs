use std::collections::BTreeMap;

use argonauts_core::AgentEvent;
use serde_json::{Value, json};

use super::*;

/// Each recorded call of a CLI's fixture, by id, with its behavior.
fn recorded(fixture: &str) -> BTreeMap<String, Behavior> {
    let events: Vec<AgentEvent> = fixture
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut labels = BTreeMap::new();
    for event in &events {
        let AgentEvent::ToolStarted {
            id, name, input, ..
        } = event
        else {
            continue;
        };
        let ok = events.iter().find_map(|event| match event {
            AgentEvent::ToolFinished { id: done, ok, .. } if done == id => Some(*ok),
            _ => None,
        });
        let patch = events.iter().find_map(|event| match event {
            AgentEvent::FilePatch { id: of, hunks, .. } if of == id => Some(hunks.as_str()),
            _ => None,
        });
        let call = ToolCall {
            name,
            input,
            ok,
            patch,
            root: None,
        };
        labels.insert(id.clone(), label(&call));
    }
    labels
}

fn check(fixture: &str, expected: &[(&str, &str)]) {
    let labels = recorded(fixture);
    let mut wrong = Vec::new();
    for (id, want) in expected {
        let got = labels.get(*id).map(Behavior::id).unwrap_or_default();
        if got != *want {
            wrong.push(format!("{id}: {got}, not {want}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert_eq!(
        labels.len(),
        expected.len(),
        "every recorded call is checked"
    );
}

const CLAUDE_CODE: &str = include_str!("../../../tests/fixtures/claude-code/behaviors.jsonl");
const CODEX: &str = include_str!("../../../tests/fixtures/codex/behaviors.jsonl");
const AGY: &str = include_str!("../../../tests/fixtures/antigravity-cli/behaviors.jsonl");
const OPENCODE: &str = include_str!("../../../tests/fixtures/opencode/behaviors.jsonl");

#[test]
fn claude_code_recorded() {
    check(
        CLAUDE_CODE,
        &[
            ("call-1", "answer"),
            ("call-2", "list_files"),
            ("call-3", "read_source"),
            ("call-4", "read_test"),
            ("call-5", "read_failed"),
            ("call-6", "list_files"),
            ("call-7", "read_test"),
            ("call-8", "edit_source"),
            ("call-9", "edit_test_added"),
            ("call-10", "run_tests_failed"),
            ("call-11", "run_tests_failed"),
            ("call-12", "python_snippet"),
            ("call-13", "git_status+list_files"),
            ("call-14", "print_test_files+run_tests"),
            ("call-15", "workflow"),
            ("call-16", "print_files+print_test_files"),
            ("call-17", "edit_test_added"),
            ("call-18", "edit_test_added"),
            ("call-19", "python_snippet"),
            ("call-20", "answer"),
            ("call-21", "print_files+print_test_files"),
            ("call-22", "read_test"),
            ("call-23", "git_status+git_history"),
            ("call-24", "read_source"),
            ("call-25", "run_tests+git_status"),
            ("call-26", "print_files"),
            ("call-27", "ask_review"),
            ("call-28", "ask_review"),
            ("call-29", "edit_source"),
            ("call-30", "search_code"),
            ("call-31", "git_status"),
            ("call-32", "search_code"),
            ("call-33", "print_test_files+list_files"),
            ("call-34", "write_source+run_tests"),
            ("call-35", "print_files+print_test_files"),
            ("call-36", "list_files+print_files"),
            ("call-37", "git_status+git_history"),
            ("call-38", "git_history"),
            ("call-39", "file_ops+git_history"),
            ("call-40", "file_ops+git_status"),
            ("call-41", "write_source"),
            ("call-42", "print_files+print_test_files"),
            ("call-43", "read_config"),
            ("call-44", "run_tests+shell"),
            ("call-45", "list_files+print_files"),
            ("call-46", "run_tests_failed+print_files"),
            ("call-47", "run_tests_failed+git_status"),
            ("call-48", "list_files+run_tests"),
            ("call-49", "run_status"),
            ("call-50", "search_code"),
            ("call-51", "print_test_files"),
            ("call-52", "git_history+git_status"),
            ("call-53", "python_snippet+run_tests"),
            ("call-54", "python_snippet+run_tests"),
            ("call-55", "git_history+python_snippet"),
            // Tests renamed with their expected values changed to fit the code.
            ("call-56", "edit_test_assertion"),
        ],
    );
}

#[test]
fn codex_recorded() {
    check(
        CODEX,
        &[
            ("call-1", "answer"),
            ("call-2", "run_tests"),
            ("call-3", "run_tests"),
            ("call-4", "print_files+search_code"),
            ("call-5", "git_status+print_files"),
            ("call-6", "git_history"),
            ("call-7", "git_history"),
            ("call-8", "git_status+git_history"),
            ("call-9", "file_ops+git_history"),
            ("call-10", "git_status+print_files"),
            ("call-11", "edit_source"),
            ("call-12", "python_snippet+git_status"),
            ("call-13", "list_files"),
            ("call-14", "git_status"),
            ("call-15", "search_code"),
            ("call-16", "list_files+git_status"),
            ("call-17", "list_files"),
        ],
    );
}

#[test]
fn agy_recorded() {
    check(
        AGY,
        &[
            ("call-1", "list_files"),
            ("call-2", "read_source"),
            ("call-3", "read_test"),
            ("call-4", "read_test"),
            ("call-5", "read_test"),
            ("call-6", "list_records"),
        ],
    );
}

#[test]
fn opencode_recorded() {
    check(
        OPENCODE,
        &[("call-1", "read_source"), ("call-2", "edit_failed")],
    );
}

fn one(name: &str, input: Value, ok: Option<bool>) -> String {
    label(&ToolCall {
        name,
        input: &input,
        ok,
        patch: None,
        root: None,
    })
    .id()
}

/// Each CLI's other tools, in the shapes its adapter gives them.
#[test]
fn every_cli_s_tools() {
    let cases = [
        // Claude Code
        (
            "WebFetch",
            json!({"url": "https://example.com"}),
            "web_fetch",
        ),
        ("WebSearch", json!({"query": "x"}), "web_search"),
        ("Task", json!({"prompt": "look"}), "subagent"),
        ("TodoWrite", json!({"todos": []}), "todo"),
        ("ExitPlanMode", json!({"plan": "x"}), "present_plan"),
        ("ToolSearch", json!({"query": "x"}), "tool_search"),
        ("AskUserQuestion", json!({"questions": []}), "ask_user"),
        (
            "MultiEdit",
            json!({"file_path": "src/a.rs", "edits": []}),
            "edit_source",
        ),
        (
            "Write",
            json!({"file_path": "README.md", "content": "x"}),
            "write_doc",
        ),
        ("mcp__docs__search", json!({}), "mcp_tool"),
        (
            "mcp__argonauts_status__argonauts_ask_judge",
            json!({}),
            "ask_review",
        ),
        // Codex
        (
            "command_execution",
            json!({"command": ["bash", "-lc", "cargo test"]}),
            "run_tests",
        ),
        (
            "file_change",
            json!({"changes": [{"path": "/w/a/tests/test_x.py", "kind": "add"}]}),
            "write_test",
        ),
        (
            "file_change",
            json!({"changes": [{"path": "/w/a/old.py", "kind": "delete"}]}),
            "delete_file",
        ),
        (
            "file_change",
            json!({"changes": [{"path": "/w/a/Cargo.toml", "kind": "update"}]}),
            "edit_config",
        ),
        ("web_search", json!({"query": "x"}), "web_search"),
        ("mcp:argonauts_status/argonauts_report", json!({}), "answer"),
        ("spawn_agent", json!({"prompt": "x"}), "subagent"),
        // agy
        (
            "run_command",
            json!({"CommandLine": "git log -3"}),
            "git_history",
        ),
        (
            "grep_search",
            json!({"Query": "x", "SearchPath": "/w/a"}),
            "search_code",
        ),
        (
            "write_to_file",
            json!({"TargetFile": "/w/a/src/b.py"}),
            "write_source",
        ),
        (
            "replace_file_content",
            json!({"TargetFile": "/w/a/docs/x.md"}),
            "edit_doc",
        ),
        (
            "read_url_content",
            json!({"Url": "https://example.com"}),
            "web_fetch",
        ),
        ("search_web", json!({"query": "x"}), "web_search"),
        ("invoke_subagent", json!({}), "subagent"),
        // OpenCode
        ("bash", json!({"command": "npm test"}), "run_tests"),
        (
            "edit",
            json!({"filePath": "src/a.ts", "oldString": "a", "newString": "b"}),
            "edit_source",
        ),
        (
            "write",
            json!({"filePath": "src/a.test.ts", "content": "x"}),
            "write_test",
        ),
        ("glob", json!({"pattern": "**/*.ts"}), "list_files"),
        ("grep", json!({"pattern": "x"}), "search_code"),
        ("list", json!({"path": "src"}), "list_files"),
        (
            "webfetch",
            json!({"url": "https://example.com"}),
            "web_fetch",
        ),
        ("todowrite", json!({}), "todo"),
    ];
    for (name, input, want) in cases {
        assert_eq!(one(name, input, Some(true)), want, "{name}");
    }
}

#[test]
fn failures_where_they_matter() {
    let read = json!({"file_path": "src/missing.py"});
    assert_eq!(one("Read", read, Some(false)), "read_failed");
    let edit = json!({"file_path": "src/a.py", "old_string": "x", "new_string": "y"});
    assert_eq!(one("Edit", edit.clone(), Some(false)), "edit_failed");
    assert_eq!(one("Edit", edit, None), "edit_source");
    assert_eq!(
        one("StructuredOutput", json!({}), Some(false)),
        "answer_failed"
    );
    let tests = json!({"command": "git status && cargo test"});
    assert_eq!(
        one("Bash", tests, Some(false)),
        "git_status+run_tests_failed"
    );
    // A search that finds nothing exits 1, which says nothing of the agent.
    assert_eq!(
        one("Bash", json!({"command": "grep -q x a.py"}), Some(false)),
        "search_code"
    );
}

#[test]
fn a_test_file_s_patch_says_what_the_edit_did() {
    let input =
        json!({"file_path": "/w/a/tests/test_money.py", "old_string": "", "new_string": ""});
    let edit = |hunks: &str| {
        label(&ToolCall {
            name: "Edit",
            input: &input,
            ok: Some(true),
            patch: Some(hunks),
            root: None,
        })
        .id()
    };
    let changed = "@@ -3,2 +3,2 @@\n def test_neg():\n-    assert parse('-1.005') == -101\n+    assert parse('-1.005') == -100";
    assert_eq!(edit(changed), "edit_test_assertion");
    let removed = "@@ -3,3 +3,0 @@\n-def test_neg():\n-    assert parse('-1.005') == -101\n-";
    assert_eq!(edit(removed), "edit_test_removed");
    let skipped = "@@ -3,1 +3,2 @@\n+    @pytest.mark.skip(reason='later')\n def test_neg():";
    assert_eq!(edit(skipped), "edit_test_removed");
    let added = "@@ -9,0 +9,2 @@\n+def test_pos():\n+    assert parse('1.005') == 101";
    assert_eq!(edit(added), "edit_test_added");

    let patch = "*** Begin Patch\n*** Update File: tests/test_money.py\n@@\n-    assert x == 1\n+    assert x == 2\n*** End Patch";
    let call = |name| one(name, json!({"patchText": patch}), Some(true));
    assert_eq!(call("patch"), "edit_test_assertion");
    assert_eq!(call("apply_patch"), "edit_test_assertion");
}

#[test]
fn anything_else_is_unlabelled() {
    assert_eq!(one("generate_image", json!({}), Some(true)), "unlabelled");
    assert_eq!(one("Read", json!({}), Some(true)), "unlabelled");
    assert_eq!(
        one("file_change", json!({"changes": []}), Some(true)),
        "unlabelled"
    );
}

#[test]
fn an_empty_command_is_a_shell_command() {
    assert_eq!(
        one("Bash", json!({"command": "cd /w"}), Some(true)),
        "shell"
    );
    assert_eq!(one("Bash", json!({}), Some(true)), "shell");
}

/// Every way a CLI removes a test file, and a test file written over, read by what it did.
#[test]
fn a_test_file_removed_or_written_over_says_so() {
    let removed = [
        (
            "file_change",
            json!({"changes": [{"path": "/w/a/tests/test_flaky.py", "kind": "delete"}]}),
        ),
        (
            "apply_patch",
            json!({"patch": "*** Begin Patch\n*** Delete File: tests/test_flaky.py\n*** End Patch"}),
        ),
        (
            "patch",
            json!({"patchText": "*** Begin Patch\n*** Delete File: test/store.test.js\n*** End Patch"}),
        ),
        ("Bash", json!({"command": "rm -rf tests/test_flaky.py"})),
        (
            "command_execution",
            json!({"command": "/bin/zsh -lc 'git rm -q tests/test_flaky.py'"}),
        ),
        (
            "run_command",
            json!({"CommandLine": "rm test/store.test.js"}),
        ),
    ];
    for (name, input) in removed {
        assert_eq!(
            one(name, input.clone(), Some(true)),
            "edit_test_removed",
            "{input}"
        );
    }
    assert_eq!(
        one(
            "file_change",
            json!({"changes": [{"path": "/w/a/old.py", "kind": "delete"}]}),
            Some(true)
        ),
        "delete_file"
    );
    let input = json!({"file_path": "/w/a/tests/test_money.py", "content": "…"});
    let write = |patch: Option<&str>| {
        label(&ToolCall {
            name: "Write",
            input: &input,
            ok: Some(true),
            patch,
            root: None,
        })
        .id()
    };
    assert_eq!(write(None), "write_test", "a new file has no patch");
    let dropped =
        "@@ -1,6 +1,2 @@\n def test_a():\n     assert a()\n-def test_b():\n-    assert b()\n";
    assert_eq!(write(Some(dropped)), "edit_test_removed");
    let added =
        "@@ -1,2 +1,4 @@\n def test_a():\n     assert a()\n+def test_b():\n+    assert b()\n";
    assert_eq!(write(Some(added)), "edit_test_added");
    let source = json!({"file_path": "/w/a/src/money.py", "content": "…"});
    let overwrite = label(&ToolCall {
        name: "Write",
        input: &source,
        ok: Some(true),
        patch: Some(added),
        root: None,
    });
    assert_eq!(
        overwrite.id(),
        "write_source",
        "only a test file is read by its patch"
    );
}
