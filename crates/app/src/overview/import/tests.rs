//! Every file here is made up in a temporary folder. No test reads a real session file.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use time::macros::datetime;

use super::*;

/// What a made-up answer says, which must never reach the records.
const SAID: &str = "the secret plan for the launch";

fn write(path: &Path, lines: &[String]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

fn claude_answer(id: &str, at: &str, model: &str, usage: [u64; 4], sidechain: bool) -> String {
    serde_json::json!({
        "type": "assistant",
        "timestamp": at,
        "cwd": "/work/app",
        "sessionId": "s1",
        "isSidechain": sidechain,
        "message": {
            "id": id,
            "model": model,
            "role": "assistant",
            "content": [{"type": "text", "text": SAID}],
            "usage": {
                "input_tokens": usage[0],
                "output_tokens": usage[1],
                "cache_read_input_tokens": usage[2],
                "cache_creation_input_tokens": usage[3]
            }
        }
    })
    .to_string()
}

fn claude_question() -> String {
    serde_json::json!({
        "type": "user",
        "timestamp": "2026-10-04T13:00:00.000Z",
        "cwd": "/work/app",
        "message": {"role": "user", "content": SAID}
    })
    .to_string()
}

fn codex_line(at: &str, kind: &str, payload: serde_json::Value) -> String {
    serde_json::json!({"timestamp": at, "type": kind, "payload": payload}).to_string()
}

fn codex_total(at: &str, input: u64, cached: u64, output: u64) -> String {
    codex_line(
        at,
        "event_msg",
        serde_json::json!({
            "type": "token_count",
            "info": {
                "total_token_usage": {
                    "input_tokens": input,
                    "cached_input_tokens": cached,
                    "output_tokens": output,
                    "reasoning_output_tokens": 0,
                    "total_tokens": input + output
                },
                "last_token_usage": null,
                "model_context_window": 400000
            },
            "rate_limits": null
        }),
    )
}

struct World {
    _dir: tempfile::TempDir,
    store: Store,
    claude: PathBuf,
    codex: PathBuf,
    records_dir: PathBuf,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let records_dir = dir.path().join("argonauts/usage/imported");
    World {
        store: Store::at(&records_dir),
        claude: dir.path().join("home/.claude"),
        codex: dir.path().join("home/.codex"),
        records_dir,
        _dir: dir,
    }
}

impl World {
    fn locations(&self) -> Locations {
        Locations {
            claude: Some(self.claude.clone()),
            codex: Some(self.codex.clone()),
            zstd: None,
        }
    }

    fn import(&self) -> Vec<(u32, u32)> {
        let seen = std::cell::RefCell::new(Vec::new());
        import(&self.store, &self.locations(), &|done, of| {
            seen.borrow_mut().push((done, of));
        })
        .unwrap();
        seen.into_inner()
    }
}

#[test]
fn claude_codes_answers_count_once_each_with_their_subagents_in_the_same_session() {
    let world = world();
    let main = world.claude.join("projects/-work-app/s1.jsonl");
    write(
        &main,
        &[
            claude_question(),
            claude_answer(
                "m1",
                "2026-10-04T13:20:00.000Z",
                "claude-opus-5-5",
                [10, 20, 300, 40],
                false,
            ),
            // The same answer's next block repeats its usage.
            claude_answer(
                "m1",
                "2026-10-04T13:20:01.000Z",
                "claude-opus-5-5",
                [10, 20, 300, 40],
                false,
            ),
            claude_answer(
                "m2",
                "2026-10-04T13:40:00.000Z",
                "<synthetic>",
                [1, 1, 1, 1],
                false,
            ),
            // Left to the subagent's own file.
            claude_answer(
                "m3",
                "2026-10-04T13:45:00.000Z",
                "claude-haiku-4-5",
                [5, 5, 5, 5],
                true,
            ),
            claude_answer(
                "m4",
                "2026-10-04T14:05:00.000Z",
                "claude-opus-5-5",
                [1, 2, 3, 4],
                false,
            ),
        ],
    );
    let agent = world
        .claude
        .join("projects/-work-app/s1/subagents/agent-a1.jsonl");
    write(
        &agent,
        &[claude_answer(
            "m3",
            "2026-10-04T13:45:00.000Z",
            "claude-haiku-4-5",
            [5, 5, 5, 5],
            true,
        )],
    );
    world.import();
    let records = world.store.records();
    let summed = |pick: &dyn Fn(&Record) -> bool| -> [u64; 4] {
        records.iter().filter(|r| pick(r)).fold([0; 4], |sum, r| {
            [
                sum[0] + r.input,
                sum[1] + r.output,
                sum[2] + r.cache_read,
                sum[3] + r.cache_write,
            ]
        })
    };
    assert_eq!(summed(&|r| !r.subagent), [11, 22, 303, 44]);
    assert_eq!(summed(&|r| r.subagent), [5, 5, 5, 5]);
    assert!(
        records
            .iter()
            .all(|r| r.cli == "claude-code" && r.folder == "/work/app")
    );
    let sessions: std::collections::BTreeSet<u32> = records.iter().map(|r| r.session).collect();
    assert_eq!(
        sessions.len(),
        1,
        "the subagent's answers are its session's: {records:?}"
    );
    let first = records
        .iter()
        .find(|r| r.model == "claude-opus-5-5")
        .unwrap();
    assert_eq!(
        first.at,
        datetime!(2026-10-04 13:00 UTC),
        "kept to the hour"
    );
    let kept = fs::read_to_string(world.records_dir.join("records.jsonl")).unwrap();
    assert!(
        !kept.contains("secret"),
        "what was said is never kept: {kept}"
    );
    let state = fs::read_to_string(world.records_dir.join("state.json")).unwrap();
    assert!(!state.contains("secret"), "{state}");
}

#[test]
fn afterwards_only_what_is_new_is_read() {
    let world = world();
    let main = world.claude.join("projects/-work-app/s1.jsonl");
    write(
        &main,
        &[claude_answer(
            "m1",
            "2026-10-04T13:20:00.000Z",
            "claude-opus-5-5",
            [10, 0, 0, 0],
            false,
        )],
    );
    world.import();
    assert_eq!(world.store.records().len(), 1);
    // Nothing new: nothing opened, nothing added.
    assert_eq!(world.import(), vec![(0, 0)]);
    assert_eq!(world.store.records().len(), 1);
    // A new answer, and half of another still being written.
    write(
        &main,
        &[claude_answer(
            "m2",
            "2026-10-05T09:00:00.000Z",
            "claude-opus-5-5",
            [7, 0, 0, 0],
            false,
        )],
    );
    let half = claude_answer(
        "m3",
        "2026-10-05T09:30:00.000Z",
        "claude-opus-5-5",
        [100, 0, 0, 0],
        false,
    );
    let mut file = OpenOptions::new().append(true).open(&main).unwrap();
    write!(file, "{}", &half[..half.len() / 2]).unwrap();
    world.import();
    let input: u64 = world.store.records().iter().map(|r| r.input).sum();
    assert_eq!(input, 17, "the half line waits");
    writeln!(file, "{}", &half[half.len() / 2..]).unwrap();
    world.import();
    let records = world.store.records();
    assert_eq!(records.iter().map(|r| r.input).sum::<u64>(), 117);
    assert!(records.iter().all(|r| r.session == records[0].session));
}

#[test]
fn codex_answers_are_the_growth_of_its_running_total() {
    let world = world();
    let log = world
        .codex
        .join("sessions/2026/02/03/rollout-2026-02-03T10-00-00-abc.jsonl");
    write(
        &log,
        &[
            codex_line(
                "2026-02-03T10:00:00.000Z",
                "session_meta",
                serde_json::json!({"id": "abc", "cwd": "/work/minitorch", "instructions": SAID, "source": "cli"}),
            ),
            codex_line(
                "2026-02-03T10:00:01.000Z",
                "turn_context",
                serde_json::json!({"cwd": "/work/minitorch", "model": "gpt-5.6", "approval_policy": "never"}),
            ),
            codex_line(
                "2026-02-03T10:00:02.000Z",
                "response_item",
                serde_json::json!({"type": "message", "content": [{"type": "input_text", "text": SAID}]}),
            ),
            codex_total("2026-02-03T10:01:00.000Z", 100, 60, 10),
            // Repeated as it often is: nothing new.
            codex_total("2026-02-03T10:01:01.000Z", 100, 60, 10),
            codex_total("2026-02-03T11:30:00.000Z", 250, 160, 30),
        ],
    );
    let helper = world
        .codex
        .join("archived_sessions/rollout-2026-02-04T08-00-00-def.jsonl");
    write(
        &helper,
        &[
            codex_line(
                "2026-02-04T08:00:00.000Z",
                "session_meta",
                serde_json::json!({"id": "def", "cwd": "/work/minitorch", "source": {"subagent": "review"}}),
            ),
            codex_line(
                "2026-02-04T08:00:01.000Z",
                "turn_context",
                serde_json::json!({"cwd": "/work/minitorch", "model": "gpt-5.6-luna"}),
            ),
            codex_total("2026-02-04T08:05:00.000Z", 50, 0, 5),
        ],
    );
    // Without zstd a compressed log is left out, and counted as such.
    let packed = world
        .codex
        .join("sessions/2026/01/02/rollout-2026-01-02T08-00-00-old.jsonl.zst");
    write(&packed, &["not really compressed".to_owned()]);
    world.import();
    let records = world.store.records();
    let main: Vec<&Record> = records.iter().filter(|r| r.model == "gpt-5.6").collect();
    assert_eq!(main.len(), 2, "two hours: {records:?}");
    assert_eq!(
        (main[0].input, main[0].cache_read, main[0].output),
        (40, 60, 10)
    );
    assert_eq!(
        (main[1].input, main[1].cache_read, main[1].output),
        (50, 100, 20)
    );
    assert!(
        main.iter()
            .all(|r| r.folder == "/work/minitorch" && !r.subagent)
    );
    let helper: Vec<&Record> = records
        .iter()
        .filter(|r| r.model == "gpt-5.6-luna")
        .collect();
    assert_eq!(helper.len(), 1);
    assert!(helper[0].subagent);
    assert_ne!(helper[0].session, main[0].session);
    assert_eq!(world.store.state().skipped, 1);
    let kept = fs::read_to_string(world.records_dir.join("records.jsonl")).unwrap();
    assert!(!kept.contains("secret"), "{kept}");
}

#[test]
fn the_answer_to_the_question_decides_the_status() {
    let mut state = ImportState::default();
    assert_eq!(status(&state), ImportStatus::Unasked);
    state.import = Some(false);
    assert_eq!(status(&state), ImportStatus::Declined);
    state.import = Some(true);
    assert!(matches!(
        status(&state),
        ImportStatus::Done | ImportStatus::Running { .. }
    ));
}

#[test]
fn missing_folders_import_nothing() {
    let world = world();
    world.import();
    assert!(world.store.records().is_empty());
}
