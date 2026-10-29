use std::path::Path;

use argonauts_core::Adapter as _;
use argonauts_core::cli_sessions::CliCall;
use time::macros::datetime;

use super::*;
use crate::CodexAdapter;

const INPUT: &str = "019a5770-e3f9-7f92-ab2b-5ba3b7602e8a";
const EXPORT: &str = "019a5771-0a0a-7b7b-8c8c-000000000002";

/// A home folder holding the recorded `~/.codex` store. The fixture can't be a `.codex` folder
/// itself, so each test copies it in.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("session-store");
    copy_dir(&fixtures, &home.path().join(".codex"));
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
    let sessions = CodexAdapter::new()
        .cli_sessions(&scan(home.path()), 20)
        .unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    // Left out: an exec run, a subagent, another folder's, and one with no words of yours.
    assert_eq!(ids, vec![EXPORT, INPUT]);

    // An older rollout names no source, and the index no title.
    let export = &sessions[0];
    assert_eq!(export.agent_id.0.as_str(), "codex");
    assert_eq!(
        export.title,
        "Fix the flaky export test. It fails on CI about once a day."
    );
    assert_eq!(export.updated, datetime!(2026-09-29 14:03 UTC));
    assert_eq!(export.branch, None);
    assert_eq!(
        export.last_message.as_deref(),
        Some("The test races the writer's flush.")
    );

    let input = &sessions[1];
    assert_eq!(input.title, "Input handling, explained");
    assert_eq!(
        input.prompt.as_deref(),
        Some("Explain the input handling\nin detail")
    );
    assert_eq!(input.folder, Path::new("/work/shop"));
    assert_eq!(input.updated, datetime!(2026-09-28 09:05:06 UTC));
    assert_eq!(input.model.as_deref(), Some("gpt-6.1-sol-mini"));
    assert_eq!(input.branch.as_deref(), Some("main"));
    assert_eq!(
        input.last_message.as_deref(),
        Some("A paste arrives as one event. So it never runs a command.")
    );
}

#[test]
fn the_newest_written_are_read_first_up_to_the_limit() {
    let home = home();
    let set = |day: &str, name: &str, secs: u64| {
        let path = home
            .path()
            .join(".codex/sessions/2026/09")
            .join(day)
            .join(name);
        let file = File::options().append(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    };
    set(
        "28",
        &format!("rollout-2026-09-28T09-00-00-{INPUT}.jsonl"),
        2_000_000_000,
    );
    set(
        "29",
        &format!("rollout-2026-09-29T14-00-00-{EXPORT}.jsonl"),
        1_000_000_000,
    );
    let sessions = list(&scan(home.path()), 1).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, INPUT);
}

#[test]
fn a_missing_store_lists_nothing() {
    let empty = tempfile::tempdir().unwrap();
    assert!(list(&scan(empty.path()), 20).unwrap().is_empty());
}

#[test]
fn turns_leave_out_what_codex_adds() {
    let home = home();
    let all = CodexAdapter::new()
        .cli_session_turns(&scan(home.path()), INPUT, 10)
        .unwrap();
    let said: Vec<(Speaker, &str)> = all.iter().map(|t| (t.from, t.text.as_str())).collect();
    assert_eq!(
        said,
        vec![
            (Speaker::User, "Explain the input handling\nin detail"),
            (Speaker::Agent, "It reads keys in one loop."),
            (Speaker::User, "And the paste handling?"),
            (
                Speaker::Agent,
                "A paste arrives as one event.\n\nSo it never runs a command."
            ),
        ]
    );
    let last = turns(&scan(home.path()), INPUT, 2).unwrap();
    assert_eq!(last[0].text, "And the paste handling?");
    // Each prompt has when you sent it, which tells the turns made in Codex's own screen.
    assert_eq!(all[0].sent, Some(datetime!(2026-09-28 09:01:00.200 UTC)));
    assert_eq!(all[1].sent, None);
    assert!(last[0].sent.is_some());
}

#[test]
fn an_agents_turn_keeps_what_it_ran_as_the_live_stream_names_it() {
    let home = home();
    let all = turns(&scan(home.path()), INPUT, 10).unwrap();
    assert!(all[0].steps.is_empty(), "a person's turn has no steps");
    let search = CliCall {
        ok: Some(true),
        output: Some("src/input.rs:3:fn read_key()".to_owned()),
        ..CliCall::new(
            "call_1",
            "command_execution",
            serde_json::json!({"command": "rg read_key"}),
        )
    };
    assert_eq!(
        all[1].steps,
        vec![
            // Codex keeps its reasoning encrypted, so only that it thought is known.
            CliStep::Thought {
                text: String::new()
            },
            CliStep::Ran(search),
            CliStep::Wrote {
                text: "It reads keys in one loop.".to_owned()
            },
        ]
    );
}

/// A command left running reports after your next prompt. Its result still reaches the call
/// that started it, in the turn before.
#[test]
fn a_result_reaches_its_call_in_an_earlier_turn() {
    let home = home();
    let store = home.path().join(".codex/sessions/2026/09/28");
    let file = store.join(format!("rollout-2026-09-28T09-00-00-{INPUT}.jsonl"));
    let recorded = fs::read_to_string(&file).unwrap();
    let prompt = serde_json::json!({
        "timestamp": "2026-09-28T09:01:03.500Z", "type": "response_item",
        "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "And the mouse?"}]}
    });
    // Your next prompt lands between the call and its output.
    let lines: Vec<String> = recorded
        .lines()
        .flat_map(|line| {
            let output = line.contains("\"function_call_output\"") && line.contains("call_1");
            output
                .then(|| prompt.to_string())
                .into_iter()
                .chain([line.to_owned()])
        })
        .collect();
    assert_eq!(lines.len(), recorded.lines().count() + 1);
    fs::write(&file, lines.join("\n")).unwrap();

    let all = turns(&scan(home.path()), INPUT, 10).unwrap();
    assert_eq!(all[2].text, "And the mouse?", "{all:?}");
    let ran: Vec<&CliCall> = all[1]
        .steps
        .iter()
        .filter_map(|step| match step {
            CliStep::Ran(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(ran.len(), 1, "{:?}", all[1].steps);
    assert_eq!(ran[0].ok, Some(true));
    assert_eq!(
        ran[0].output.as_deref(),
        Some("src/input.rs:3:fn read_key()")
    );
}

#[test]
fn a_patch_reads_as_a_file_change_with_each_files_hunks() {
    let patch = "*** Begin Patch\n*** Update File: src/tax.rs\n@@ fn total()\n     let sum = \
                 add();\n-    sum.round()\n+    sum.floor()\n*** Add File: notes.md\n+# Notes\n\
                 *** Delete File: old.rs\n*** End Patch";
    let call = calls::call("call_7", "apply_patch", Some(patch));
    assert_eq!(call.name, "file_change");
    assert_eq!(
        call.input,
        serde_json::json!({"changes": [
            {"path": "src/tax.rs", "kind": "update"},
            {"path": "notes.md", "kind": "add"},
            {"path": "old.rs", "kind": "delete"},
        ]})
    );
    let hunks: Vec<(&str, &str, bool)> = call
        .patches
        .iter()
        .map(|patch| (patch.path.as_str(), patch.hunks.as_str(), patch.numbered))
        .collect();
    assert_eq!(
        hunks,
        vec![
            (
                "src/tax.rs",
                "@@ -1,2 +1,2 @@\n     let sum = add();\n-    sum.round()\n+    sum.floor()\n",
                false
            ),
            ("notes.md", "@@ -1,0 +1,1 @@\n+# Notes\n", false),
        ]
    );
    // As a function, the patch is the `input` of its arguments.
    let wrapped = serde_json::json!({ "input": patch }).to_string();
    assert_eq!(
        calls::call("call_8", "apply_patch", Some(&wrapped)).patches,
        call.patches
    );
}

#[test]
fn a_calls_result_says_how_it_ended_in_each_shape_codex_wrote() {
    let text = |text: &str| serde_json::Value::String(text.to_owned());
    assert_eq!(
        calls::result(Some(&text("found"))),
        (true, Some("found".to_owned()))
    );
    let json = r#"{"output":"boom\n","metadata":{"exit_code":2,"duration_seconds":0.1}}"#;
    assert_eq!(
        calls::result(Some(&text(json))),
        (false, Some("boom\n".to_owned()))
    );
    let plain = "Exit code: 0\nWall time: 0.2 seconds\nOutput:\nok\n";
    assert_eq!(
        calls::result(Some(&text(plain))),
        (true, Some("ok\n".to_owned()))
    );
    let object = serde_json::json!({"content": "denied", "success": false});
    assert_eq!(
        calls::result(Some(&object)),
        (false, Some("denied".to_owned()))
    );
    assert_eq!(calls::result(None), (true, None));
}

#[test]
fn turns_of_an_unknown_or_unsafe_id_fail() {
    let home = home();
    let missing = turns(&scan(home.path()), "019a0000", 4).unwrap_err();
    assert!(missing.to_string().contains("no session"), "{missing}");
    assert!(turns(&scan(home.path()), "../x", 4).is_err());
}

#[test]
fn what_codex_adds_is_told_from_your_words() {
    assert!(added_by_codex(
        "<environment_context>\n<cwd>/w</cwd>\n</environment_context>"
    ));
    assert!(added_by_codex("# AGENTS.md instructions for /w\n\nhi"));
    assert!(!added_by_codex("<div> is off by one"));
    assert!(!added_by_codex("Read AGENTS.md first"));
}

#[test]
fn every_folder_lists_the_other_folders_sessions_with_where_each_ran() {
    let home = home();
    let adapter = CodexAdapter::new();
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
    let adapter = CodexAdapter::new();
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
