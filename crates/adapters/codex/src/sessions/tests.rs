use std::path::Path;

use argonauts_core::Adapter as _;
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

fn scan(home: &Path) -> VendorScan {
    VendorScan::new("/work/shop", home)
}

#[test]
fn lists_this_folders_sessions_newest_first() {
    let home = home();
    let sessions = CodexAdapter::new()
        .vendor_sessions(&scan(home.path()), 20)
        .unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    // Left out: an exec run, a subagent, another folder's, and one with no words of yours.
    assert_eq!(ids, vec![EXPORT, INPUT]);

    // An older rollout names no source, and the index no title.
    let export = &sessions[0];
    assert_eq!(export.vendor.0.as_str(), "codex");
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
fn turns_leave_out_tools_and_what_codex_adds() {
    let home = home();
    let all = CodexAdapter::new()
        .vendor_session_turns(&scan(home.path()), INPUT, 10)
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
