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

/// A pass stopped after it wrote records and answers but before its state leaves both
/// longer than the state says. The next pass takes them back and reads the file again from
/// where the state left it, so nothing counts twice and nothing is lost.
#[test]
fn a_pass_cut_short_before_its_state_was_saved_counts_nothing_twice() {
    let world = world();
    let main = world.claude.join("projects/-work-app/s1.jsonl");
    let answer = |id: &str, at: &str, input: u64| {
        claude_answer(id, at, "claude-opus-5-5", [input, 0, 0, 0], false)
    };
    write(&main, &[answer("m1", "2026-10-04T13:20:00.000Z", 10)]);
    world.import();
    let state_file = world.records_dir.join("state.json");
    let answers_file = world.store.answers_file();
    let state_before = fs::read(&state_file).unwrap();
    let answers_before = fs::read(&answers_file).unwrap();
    write(&main, &[answer("m2", "2026-10-05T09:00:00.000Z", 7)]);
    world.import();
    // As if it stopped right after appending its records.
    fs::write(&state_file, &state_before).unwrap();
    fs::write(&answers_file, &answers_before).unwrap();
    world.import();
    let records = world.store.records();
    assert_eq!(records.iter().map(|r| r.input).sum::<u64>(), 17);
    assert_eq!(records.len(), 2);
    // Read once more, nothing changes.
    world.import();
    assert_eq!(
        world.store.records().iter().map(|r| r.input).sum::<u64>(),
        17
    );
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

fn summed(records: &[Record], pick: &dyn Fn(&Record) -> bool) -> [u64; 4] {
    records.iter().filter(|r| pick(r)).fold([0; 4], |sum, r| {
        [
            sum[0] + r.input,
            sum[1] + r.output,
            sum[2] + r.cache_read,
            sum[3] + r.cache_write,
        ]
    })
}

/// `claude_answer` with some of its fields changed.
fn claude_answer_with(id: &str, at: &str, usage: [u64; 4], change: serde_json::Value) -> String {
    let mut line: serde_json::Value =
        serde_json::from_str(&claude_answer(id, at, "claude-opus-5-5", usage, false)).unwrap();
    for (key, value) in change.as_object().unwrap() {
        line[key] = value.clone();
    }
    line.to_string()
}

#[test]
fn a_workflows_agents_deep_in_the_session_folder_are_its_subagents() {
    let world = world();
    write(
        &world.claude.join("projects/-work-app/s1.jsonl"),
        &[claude_answer(
            "m1",
            "2026-10-04T13:20:00.000Z",
            "claude-opus-5-5",
            [1, 1, 1, 1],
            false,
        )],
    );
    let run = world
        .claude
        .join("projects/-work-app/s1/subagents/workflows/wf-7");
    for (name, id) in [("agent-a1.jsonl", "w1"), ("agent-a2.jsonl", "w2")] {
        write(
            &run.join(name),
            &[claude_answer(
                id,
                "2026-10-04T13:30:00.000Z",
                "claude-haiku-5-5",
                [10, 20, 30, 40],
                true,
            )],
        );
    }
    // Not a transcript: only an agent's own file counts.
    write(&run.join("journal.jsonl"), &[claude_question()]);
    world.import();
    let records = world.store.records();
    assert_eq!(summed(&records, &|r| r.subagent), [20, 40, 60, 80]);
    assert_eq!(summed(&records, &|r| !r.subagent), [1, 1, 1, 1]);
    let sessions: std::collections::BTreeSet<u32> = records.iter().map(|r| r.session).collect();
    assert_eq!(sessions.len(), 1, "{records:?}");
}

#[test]
fn an_answer_streamed_over_several_lines_counts_its_final_output_once() {
    let world = world();
    let main = world.claude.join("projects/-work-app/s1.jsonl");
    write(
        &main,
        &[
            claude_answer(
                "m1",
                "2026-10-04T13:20:00.000Z",
                "claude-opus-5-5",
                [5, 1, 900, 60],
                false,
            ),
            claude_question(),
            claude_answer(
                "m1",
                "2026-10-04T13:20:02.000Z",
                "claude-opus-5-5",
                [5, 12, 900, 60],
                false,
            ),
            claude_answer(
                "m1",
                "2026-10-04T13:20:09.000Z",
                "claude-opus-5-5",
                [5, 340, 900, 60],
                false,
            ),
        ],
    );
    world.import();
    assert_eq!(summed(&world.store.records(), &|_| true), [5, 340, 900, 60]);
    // The same answer goes on after the pass: only what grew is added.
    write(
        &main,
        &[claude_answer(
            "m1",
            "2026-10-04T13:20:15.000Z",
            "claude-opus-5-5",
            [5, 410, 900, 60],
            false,
        )],
    );
    world.import();
    assert_eq!(summed(&world.store.records(), &|_| true), [5, 410, 900, 60]);
}

#[test]
fn an_empty_count_is_zero_and_the_line_still_counts() {
    let world = world();
    let line = serde_json::json!({
        "type": "assistant",
        "timestamp": "2026-10-04T13:20:00.000Z",
        "cwd": "/work/app",
        "isSidechain": null,
        "message": {
            "id": "m1",
            "model": "claude-opus-5-5",
            "usage": {
                "input_tokens": 7,
                "output_tokens": 30,
                "cache_read_input_tokens": null,
                "cache_creation_input_tokens": null,
                "server_tool_use": null
            }
        }
    });
    let no_model = serde_json::json!({
        "type": "assistant",
        "timestamp": "2026-10-04T14:20:00.000Z",
        "message": {"id": "m2", "usage": {"input_tokens": 3, "output_tokens": 4}}
    });
    write(
        &world.claude.join("projects/-work-app/s1.jsonl"),
        &[line.to_string(), no_model.to_string()],
    );
    world.import();
    let records = world.store.records();
    assert_eq!(summed(&records, &|_| true), [10, 34, 0, 0]);
    assert_eq!(world.store.state().unreadable, 0);
}

#[test]
fn copies_made_by_a_branch_or_a_fork_count_once() {
    let world = world();
    let original = world.claude.join("projects/-work-app/s1.jsonl");
    write(
        &original,
        &[
            claude_answer(
                "m1",
                "2026-10-04T13:20:00.000Z",
                "claude-opus-5-5",
                [1, 10, 100, 1000],
                false,
            ),
            claude_answer(
                "m2",
                "2026-10-04T13:25:00.000Z",
                "claude-opus-5-5",
                [2, 20, 200, 2000],
                false,
            ),
        ],
    );
    // A branch copies the lines and says where from; its own answers come after.
    write(
        &world.claude.join("projects/-work-app/s2.jsonl"),
        &[
            claude_answer_with(
                "m1",
                "2026-10-04T13:20:00.000Z",
                [1, 10, 100, 1000],
                serde_json::json!({"forkedFrom": {"sessionId": "s1", "messageUuid": "u1"}}),
            ),
            claude_answer(
                "m3",
                "2026-10-04T15:00:00.000Z",
                "claude-opus-5-5",
                [3, 30, 300, 3000],
                false,
            ),
        ],
    );
    // A forked session's copy carries no mark, only the same answer ids. Its name sorts
    // first, so it's read before the original.
    write(
        &world.claude.join("projects/-work-app/r0.jsonl"),
        &[claude_answer(
            "m2",
            "2026-10-04T13:25:00.000Z",
            "claude-opus-5-5",
            [2, 20, 200, 2000],
            false,
        )],
    );
    world.import();
    assert_eq!(
        summed(&world.store.records(), &|_| true),
        [6, 60, 600, 6000]
    );
}

#[test]
fn lines_that_cant_be_read_are_counted_not_hidden() {
    let world = world();
    let main = world.claude.join("projects/-work-app/s1.jsonl");
    let whole = claude_answer(
        "m1",
        "2026-10-04T13:20:00.000Z",
        "claude-opus-5-5",
        [1, 2, 3, 4],
        false,
    );
    let cut = &whole[..whole.len() - 9];
    write(&main, &[cut.to_owned(), whole.clone(), claude_question()]);
    let mut file = OpenOptions::new().append(true).open(&main).unwrap();
    file.write_all(b"{\"usage\": \"\xff\xfe\"}\n").unwrap();
    world.import();
    assert_eq!(summed(&world.store.records(), &|_| true), [1, 2, 3, 4]);
    assert_eq!(world.store.state().unreadable, 2);
}

/// A fake `zstd -dc -- <file>` that prints the file as it is, for made-up compressed
/// files that are plain text inside.
fn plain_zstd(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("zstd");
    fs::write(&path, "#!/bin/sh\nexec cat \"$3\"\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn codex_start(at: &str, id: &str, forked_from: Option<&str>) -> Vec<String> {
    vec![
        codex_line(
            at,
            "session_meta",
            serde_json::json!({"id": id, "cwd": "/work/minitorch", "source": "cli", "forked_from_id": forked_from}),
        ),
        codex_line(
            at,
            "turn_context",
            serde_json::json!({"cwd": "/work/minitorch", "model": "gpt-5.6"}),
        ),
    ]
}

#[test]
fn a_codex_rollout_moved_and_compressed_carries_on_where_it_was_read() {
    let world = world();
    let zstd = plain_zstd(world._dir.path());
    let locations = Locations {
        zstd: Some(zstd),
        ..world.locations()
    };
    let id = "019a1b2c-0000-7000-8000-000000000001";
    let live = world.codex.join(format!(
        "sessions/2026/02/03/rollout-2026-02-03T10-00-00-{id}.jsonl"
    ));
    let mut lines = codex_start("2026-02-03T10:00:00.000Z", id, None);
    lines.push(codex_total("2026-02-03T10:01:00.000Z", 100, 60, 10));
    write(&live, &lines);
    import(&world.store, &locations, &|_, _| {}).unwrap();
    // Codex adds a turn, archives the rollout and compresses it.
    lines.push(codex_total("2026-02-03T11:01:00.000Z", 300, 160, 30));
    let packed = world.codex.join(format!(
        "archived_sessions/rollout-2026-02-03T10-00-00-{id}.jsonl.zst"
    ));
    write(&packed, &lines);
    fs::remove_file(&live).unwrap();
    import(&world.store, &locations, &|_, _| {}).unwrap();
    let records = world.store.records();
    assert_eq!(
        summed(&records, &|_| true),
        [140, 30, 160, 0],
        "{records:?}"
    );
    // Read once it's finished, and not again.
    let seen = std::cell::RefCell::new(Vec::new());
    import(&world.store, &locations, &|done, of| {
        seen.borrow_mut().push((done, of))
    })
    .unwrap();
    assert_eq!(seen.into_inner(), vec![(0, 0)]);
    let sessions: std::collections::BTreeSet<u32> = records.iter().map(|r| r.session).collect();
    assert_eq!(sessions.len(), 1);
}

#[test]
fn a_codex_rollout_kept_both_plain_and_compressed_counts_once() {
    let world = world();
    let locations = Locations {
        zstd: Some(plain_zstd(world._dir.path())),
        ..world.locations()
    };
    let id = "019a1b2c-0000-7000-8000-000000000002";
    let mut lines = codex_start("2026-02-03T10:00:00.000Z", id, None);
    lines.push(codex_total("2026-02-03T10:01:00.000Z", 100, 60, 10));
    let day = world.codex.join("sessions/2026/02/03");
    write(
        &day.join(format!("rollout-2026-02-03T10-00-00-{id}.jsonl")),
        &lines,
    );
    write(
        &day.join(format!("rollout-2026-02-03T10-00-00-{id}.jsonl.zst")),
        &lines,
    );
    import(&world.store, &locations, &|_, _| {}).unwrap();
    assert_eq!(summed(&world.store.records(), &|_| true), [40, 10, 60, 0]);
}

#[test]
fn a_forked_codex_session_counts_only_its_own_turns() {
    let world = world();
    let parent = "019a1b2c-0000-7000-8000-00000000000a";
    let child = "019a1b2c-0000-7000-8000-00000000000b";
    let mut lines = codex_start("2026-02-03T10:00:00.000Z", parent, None);
    lines.push(codex_total("2026-02-03T10:01:00.000Z", 1000, 600, 100));
    write(
        &world.codex.join(format!(
            "sessions/2026/02/03/rollout-2026-02-03T10-00-00-{parent}.jsonl"
        )),
        &lines,
    );
    // The fork's total starts from the parent's 1000 in, 600 cached, 100 out.
    let mut forked = codex_start("2026-02-03T12:00:00.000Z", child, Some(parent));
    let mut first: serde_json::Value =
        serde_json::from_str(&codex_total("2026-02-03T12:01:00.000Z", 1300, 800, 130)).unwrap();
    first["payload"]["info"]["last_token_usage"] =
        serde_json::json!({"input_tokens": 300, "cached_input_tokens": 200, "output_tokens": 30});
    forked.push(first.to_string());
    forked.push(codex_total("2026-02-03T12:05:00.000Z", 1400, 850, 140));
    write(
        &world.codex.join(format!(
            "sessions/2026/02/03/rollout-2026-02-03T12-00-00-{child}.jsonl"
        )),
        &forked,
    );
    world.import();
    let records = world.store.records();
    // The parent's 400 + 100 + 600, and the fork's own 150 + 40 + 250.
    assert_eq!(
        summed(&records, &|_| true),
        [550, 140, 850, 0],
        "{records:?}"
    );
}

/// A made-up `stats-cache.json` in the shape Claude Code writes it.
fn stats_cache(days: &[(&str, u64, u64)], through: &str) -> String {
    serde_json::json!({
        "version": 5,
        "lastComputedDate": through,
        "dailyActivity": days.iter().map(|(date, _, sessions)| serde_json::json!({
            "date": date, "messageCount": 40, "sessionCount": sessions, "toolCallCount": 9
        })).collect::<Vec<_>>(),
        "dailyModelTokens": days.iter().map(|(date, tokens, _)| serde_json::json!({
            "date": date, "tokensByModel": {"claude-opus-5-5": tokens, "<synthetic>": 5}
        })).collect::<Vec<_>>(),
        "dailyModelTokensVersion": 5,
        "modelUsage": {
            "claude-opus-5-5": {
                "inputTokens": 10, "outputTokens": 40, "cacheReadInputTokens": 900,
                "cacheCreationInputTokens": 50, "webSearchRequests": 0, "costUSD": 0,
                "contextWindow": 0, "maxOutputTokens": 0
            }
        },
        "totalSessions": 6,
        "totalMessages": 120,
        "longestSession": null,
        "firstSessionDate": "2025-08-01T09:00:00.000Z",
        "hourCounts": {"9": 3},
        "shotDistribution": {}
    })
    .to_string()
}

/// Claude Code's own count is the total its /stats keeps, or a period's days added up, with
/// the last day it has counted.
#[test]
fn claude_codes_own_count_is_its_stats_total_or_a_periods_days() {
    use time::macros::date;

    let world = world();
    let count = |first| claude_count(&world.locations(), first);
    assert_eq!(count(None), None, "/stats hasn't kept a total yet");
    std::fs::create_dir_all(&world.claude).unwrap();
    let days = [
        ("2026-09-20", 4_000, 1),
        ("2026-10-02", 3_000, 2),
        ("2026-10-03", 2_000, 1),
    ];
    std::fs::write(
        world.claude.join("stats-cache.json"),
        stats_cache(&days, "2026-10-03"),
    )
    .unwrap();
    let through = date!(2026 - 10 - 03);
    // Every day: the four kinds of each model's tokens, 10 + 40 + 900 + 50.
    let tokens = |tokens| Some(OwnCount { tokens, through });
    assert_eq!(count(None), tokens(1_000));
    assert_eq!(count(Some(date!(2026 - 10 - 02))), tokens(5_000));
    assert_eq!(count(Some(date!(2026 - 10 - 03))), tokens(2_000));
    assert_eq!(
        count(Some(date!(2026 - 10 - 04))),
        None,
        "a period it has counted no day of"
    );
}

fn claude_day_tokens(records: &[Record], day: time::Date) -> (u64, bool) {
    let mine: Vec<&Record> = records
        .iter()
        .filter(|r| r.cli == "claude-code" && r.at.date() == day)
        .collect();
    let total = mine
        .iter()
        .map(|r| r.input + r.output + r.cache_read + r.cache_write)
        .sum();
    (total, mine.iter().any(|r| r.whole_day))
}

#[test]
fn days_whose_transcripts_are_gone_come_from_claude_codes_own_totals() {
    let world = world();
    fs::create_dir_all(&world.claude).unwrap();
    fs::write(
        world.claude.join("stats-cache.json"),
        stats_cache(
            &[
                ("2025-08-01", 1_000, 2),
                ("2026-10-03", 4_000, 1),
                ("2026-10-04", 9_999, 1),
                // Not counted yet when the file says it counted up to the 4th.
                ("2026-10-05", 7_777, 1),
            ],
            "2026-10-04",
        ),
    )
    .unwrap();
    write(
        &world.claude.join("projects/-work-app/s1.jsonl"),
        &[claude_answer(
            "m1",
            "2026-10-04T13:20:00.000Z",
            "claude-opus-5-5",
            [1, 2, 3, 4],
            false,
        )],
    );
    world.import();
    let records = world.store.records();
    assert_eq!(
        claude_day_tokens(&records, time::macros::date!(2025 - 08 - 01)),
        (1_000, true)
    );
    assert_eq!(
        claude_day_tokens(&records, time::macros::date!(2026 - 10 - 03)),
        (4_000, true)
    );
    assert_eq!(
        claude_day_tokens(&records, time::macros::date!(2026 - 10 - 04)),
        (10, false),
        "a day with a transcript is the transcript's"
    );
    assert_eq!(
        claude_day_tokens(&records, time::macros::date!(2026 - 10 - 05)),
        (0, false)
    );
    // Split like the model's tokens overall, the day's two sessions sharing it.
    let first: Vec<&Record> = records
        .iter()
        .filter(|r| r.at.date() == time::macros::date!(2025 - 08 - 01))
        .collect();
    assert_eq!(first.len(), 2);
    assert_eq!(
        summed(&records, &|r| r.at.year() == 2025),
        [10, 40, 900, 50]
    );
    assert!(first.iter().all(|r| r.folder.is_empty() && !r.subagent));
    // Read again, nothing is added twice.
    world.import();
    assert_eq!(world.store.records().len(), records.len());
}

#[test]
fn a_day_taken_from_the_totals_gives_way_when_its_transcript_turns_up() {
    let world = world();
    fs::create_dir_all(&world.claude).unwrap();
    fs::write(
        world.claude.join("stats-cache.json"),
        stats_cache(&[("2026-10-03", 4_000, 1)], "2026-10-03"),
    )
    .unwrap();
    world.import();
    let day = time::macros::date!(2026 - 10 - 03);
    assert_eq!(
        claude_day_tokens(&world.store.records(), day),
        (4_000, true)
    );
    write(
        &world.claude.join("projects/-work-app/s1.jsonl"),
        &[claude_answer(
            "m1",
            "2026-10-03T08:00:00.000Z",
            "claude-opus-5-5",
            [100, 200, 300, 400],
            false,
        )],
    );
    world.import();
    assert_eq!(
        claude_day_tokens(&world.store.records(), day),
        (1_000, false)
    );
}

#[test]
fn totals_that_cant_be_read_are_said_not_guessed() {
    let world = world();
    fs::create_dir_all(&world.claude).unwrap();
    fs::write(
        world.claude.join("stats-cache.json"),
        "{\"dailyModelTokens\": 3",
    )
    .unwrap();
    world.import();
    assert!(world.store.records().is_empty());
    assert!(world.store.state().stats_cache_problem.is_some());
}

#[test]
fn records_from_an_older_import_are_replaced_once_not_added_to() {
    let world = world();
    // What an older import kept: an undercounted answer and the file marked as read.
    let main = world.claude.join("projects/-work-app/s1.jsonl");
    write(
        &main,
        &[
            claude_answer(
                "m1",
                "2026-10-04T13:20:00.000Z",
                "claude-opus-5-5",
                [5, 1, 0, 0],
                false,
            ),
            claude_answer(
                "m1",
                "2026-10-04T13:20:05.000Z",
                "claude-opus-5-5",
                [5, 90, 0, 0],
                false,
            ),
        ],
    );
    let old = Record {
        at: datetime!(2026-10-04 13:00 UTC),
        cli: "claude-code".into(),
        model: "claude-opus-5-5".into(),
        folder: "/work/app".into(),
        input: 5,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        session: 1,
        subagent: false,
        whole_day: false,
    };
    world.store.append(&[old]).unwrap();
    let mut state = ImportState {
        import: Some(true),
        sessions: 1,
        ..ImportState::default()
    };
    state.files.insert(
        key(&main),
        FileRead {
            session: 1,
            read: fs::metadata(&main).unwrap().len(),
            ..FileRead::default()
        },
    );
    world.store.save_state(&state).unwrap();

    world.import();
    let records = world.store.records();
    assert_eq!(summed(&records, &|_| true), [5, 90, 0, 0], "{records:?}");
    let state = world.store.state();
    assert_eq!((state.version, state.import), (VERSION, Some(true)));
    assert!(!world.store.next().answers_file().parent().unwrap().exists());
    // Once is enough: the next pass reads only what's new.
    world.import();
    assert_eq!(world.store.records().len(), records.len());
}

#[test]
fn a_replacement_stopped_between_its_two_moves_is_finished() {
    let world = world();
    world.store.save_state(&ImportState::default()).unwrap();
    let next = world.store.next();
    next.save_state(&ImportState {
        import: Some(true),
        version: VERSION,
        ..ImportState::default()
    })
    .unwrap();
    // The old folder was moved aside, and then everything stopped.
    let old = world.records_dir.with_extension("old");
    fs::rename(&world.records_dir, &old).unwrap();
    let store = Store::at(&world.records_dir);
    assert_eq!(store.state().version, VERSION);
    assert!(!old.exists());
}

#[test]
fn the_found_line_says_what_couldnt_be_read() {
    let found = Some("Claude Code since Oct 2025, 812 sessions".to_owned());
    let mut state = ImportState::default();
    assert_eq!(found_line(found.clone(), &state), found);
    state.unreadable = 3;
    assert_eq!(
        found_line(found.clone(), &state).as_deref(),
        Some("Claude Code since Oct 2025, 812 sessions; 3 lines couldn't be read")
    );
    state.unreadable = 1;
    state.skipped = 2;
    assert_eq!(
        found_line(None, &state).as_deref(),
        Some("nothing; 1 line couldn't be read; 2 compressed files need zstd")
    );
}

#[test]
fn a_compressed_rollout_left_for_want_of_zstd_is_read_once_zstd_is_there() {
    let world = world();
    let id = "019a1b2c-0000-7000-8000-000000000003";
    let mut lines = codex_start("2026-02-03T10:00:00.000Z", id, None);
    lines.push(codex_total("2026-02-03T10:01:00.000Z", 100, 60, 10));
    write(
        &world.codex.join(format!(
            "archived_sessions/rollout-2026-02-03T10-00-00-{id}.jsonl.zst"
        )),
        &lines,
    );
    world.import();
    world.import();
    assert!(world.store.records().is_empty());
    assert_eq!(
        world.store.state().skipped,
        1,
        "counted once, not each pass"
    );
    let locations = Locations {
        zstd: Some(plain_zstd(world._dir.path())),
        ..world.locations()
    };
    import(&world.store, &locations, &|_, _| {}).unwrap();
    assert_eq!(summed(&world.store.records(), &|_| true), [40, 10, 60, 0]);
    assert_eq!(world.store.state().skipped, 0);
}
