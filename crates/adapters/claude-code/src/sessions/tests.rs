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

fn scan(home: &Path) -> VendorScan {
    VendorScan::new("/work/shop", home)
}

#[test]
fn lists_this_folders_sessions_newest_first() {
    let home = home();
    let sessions = ClaudeCodeAdapter
        .vendor_sessions(&scan(home.path()), 20)
        .unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    // Left out: a `-p` session, one with only a command, one carried on under another id, one
    // from `/work-shop`, whose folder name reads the same, and another folder's.
    assert_eq!(ids, vec![EXPORT, LOOP]);

    let export = &sessions[0];
    assert_eq!(export.vendor.0.as_str(), "claude-code");
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
    let elsewhere = VendorScan::new("/work/none", home.path());
    assert!(list(&elsewhere, 20).unwrap().is_empty());
}

#[test]
fn turns_leave_out_tools_commands_and_subagents() {
    let home = home();
    let turns = ClaudeCodeAdapter
        .vendor_session_turns(&scan(home.path()), LOOP, 10)
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

fn turns_of(home: &tempfile::TempDir, id: &str, count: usize) -> Vec<VendorTurn> {
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
    let sessions = list(&VendorScan::new(folder, home.path()), 20).unwrap();
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
