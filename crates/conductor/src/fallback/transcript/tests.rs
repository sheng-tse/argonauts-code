use std::path::{Path, PathBuf};

use argonauts_core::events::session::{
    EndReason, InputPart, InputPartKind, LaunchRecord, PromptVia, SessionEnd, SessionEvent,
    SessionInput, SessionStarted,
};
use argonauts_core::events::task::TaskEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::FallbackPlanner;
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::session_runner::{Access, RunnerKind};
use argonauts_core::settings::fallback::{AgentFallbacks, FallbackContext};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::{AgentEvent, ErrorCategory, Spend, TaskId, TodoItem, TodoStatus};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::*;
use crate::fallback::testing::{Fixture, TASK, agent_id, run, task, worker};
use crate::git::Git;
use crate::testutil::{TestRepo, git_test_env};

struct Log {
    events: Vec<RunEvent>,
}

impl Log {
    fn new() -> Self {
        Self { events: Vec::new() }
    }

    fn push(&mut self, task: Option<&str>, session: Option<u32>, kind: RunEventKind) {
        self.events.push(RunEvent {
            seq: self.events.len() as u64,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: run(),
            task_id: task.map(|id| TaskId::new(id).expect("a valid task id")),
            session: session.map(SessionKey::numbered),
            source: "argonauts-code".to_owned(),
            kind,
        });
    }

    fn worktree(&mut self, path: &Path) {
        let event = TaskEvent::Worktree {
            path: path.to_path_buf(),
            branch: format!("argonauts/run-01/{TASK}"),
            base: "abc1234".to_owned(),
        };
        self.push(Some(TASK), None, RunEventKind::Task(event));
    }

    fn started(&mut self, session: u32, who: &str, access: Access) {
        let role = match access {
            Access::Build | Access::Experiment => RoleName::Worker,
            _ => RoleName::Reviewer,
        };
        let started = SessionStarted {
            worker: worker(who),
            agent_id: worker(who).agent_id(),
            provider: "anthropic".to_owned(),
            kind: RunnerKind::Cli,
            role,
            step: None,
            access,
            lifetime: SessionLifetime::Task,
            resumed_from: None,
            follow_key: None,
            workflow_step: None,
            label: None,
            asked: None,
            asked_by: None,
            asked_why: None,
            forked: false,
        };
        self.session(session, SessionEvent::Started(Box::new(started)));
    }

    fn input(&mut self, session: u32, parts: &[(&str, &str)]) {
        let parts = parts
            .iter()
            .map(|(label, text)| InputPart {
                kind: InputPartKind::Message,
                label: (*label).to_owned(),
                approx_tokens: 1,
                text: (*text).to_owned(),
                source: None,
            })
            .collect();
        let input = SessionInput {
            launch: LaunchRecord {
                program: "claude".to_owned(),
                args: vec!["<prompt>".to_owned()],
                cwd: PathBuf::from("/w"),
                env_set: Vec::new(),
                env_removed: Vec::new(),
                prompt_via: PromptVia::Argument,
                sandbox: None,
            },
            parts,
            queued: Vec::new(),
        };
        self.session(session, SessionEvent::Input(Box::new(input)));
    }

    fn ended(&mut self, session: u32, reason: EndReason) {
        let end = SessionEnd {
            cli_session: None,
            exit_code: Some(1),
            signal: None,
            duration_ms: 1_000,
            reason,
            spend: Spend::default(),
            tool_calls: 0,
            result_text: None,
            structured: None,
            last_message: None,
            raw_log: None,
        };
        self.session(session, SessionEvent::Ended(Box::new(end)));
    }

    fn session(&mut self, session: u32, event: SessionEvent) {
        self.push(Some(TASK), Some(session), RunEventKind::Session(event));
    }

    /// Agent events carry only their session, as the runner records them.
    fn agent(&mut self, session: u32, event: AgentEvent) {
        self.push(None, Some(session), RunEventKind::Agent(event));
    }

    fn message(&mut self, session: u32, text: &str) {
        self.agent(
            session,
            AgentEvent::Message {
                text: text.to_owned(),
                parent: None,
            },
        );
    }

    fn tool(&mut self, session: u32, id: &str, name: &str, input: Value) {
        self.agent(
            session,
            AgentEvent::ToolStarted {
                id: id.to_owned(),
                name: name.to_owned(),
                input,
                parent: None,
            },
        );
    }

    fn finished(&mut self, session: u32, id: &str, ok: bool, output: Option<&str>) {
        self.agent(
            session,
            AgentEvent::ToolFinished {
                id: id.to_owned(),
                ok,
                output: output.map(str::to_owned),
                parent: None,
            },
        );
    }

    fn changed(&mut self, session: u32, path: &str, kind: &str) {
        self.agent(
            session,
            AgentEvent::FileChanged {
                path: path.to_owned(),
                kind: kind.to_owned(),
            },
        );
    }
}

fn task_id() -> TaskId {
    TaskId::new(TASK).expect("a valid task id")
}

fn transcript_of(log: &Log) -> String {
    let session = old_session(&task_id(), &log.events).expect("a building session");
    render(
        &task(TASK),
        &session,
        &log.events,
        "it stopped without a recorded reason",
    )
}

/// The session's events after a prompt, with one building session.
fn one_session() -> Log {
    let mut log = Log::new();
    log.started(1, "claude-code/opus", Access::Build);
    log.input(1, &[("prompt", "Build it.")]);
    log
}

/// The block between the line that names a tool call and the next blank line after its fence.
fn fenced_after<'a>(text: &'a str, line: &str) -> &'a str {
    let start = text.find(line).expect("the tool's line") + line.len();
    let rest = &text[start..];
    let open = rest.find("```\n").expect("an opening fence") + 4;
    let close = rest[open..].find("\n```").expect("a closing fence");
    &rest[open..open + close]
}

#[test]
fn renders_each_kind_of_event() {
    let mut log = Log::new();
    log.worktree(Path::new("/w/task-003"));
    // An earlier session on the task, and a reviewer after: neither is the old session.
    log.started(1, "codex/gpt-5", Access::Build);
    log.message(1, "first try");
    log.ended(
        1,
        EndReason::Crashed {
            message: "boom".to_owned(),
        },
    );
    log.started(2, "claude-code/opus", Access::Build);
    log.started(3, "codex/gpt-5", Access::ReadOnly);
    log.message(3, "a reviewer's words");

    log.input(
        2,
        &[
            ("prompt", "You own task-003.\n"),
            ("task", "Add the rate-limit middleware."),
        ],
    );
    log.agent(
        2,
        AgentEvent::Reasoning {
            text: "thinking hard".to_owned(),
        },
    );
    for part in ["I'll start", " by reading."] {
        log.agent(
            2,
            AgentEvent::TextDelta {
                text: part.to_owned(),
                parent: None,
            },
        );
    }
    log.message(2, "I'll start by reading.");
    log.tool(2, "t1", "Read", json!({"file_path": "src/cli.rs"}));
    log.finished(2, "t1", true, Some("fn main() {}\n"));
    log.tool(
        2,
        "t2",
        "Edit",
        json!({"file_path": "src/cli.rs", "old_string": "a\nb\nc", "new_string": "a\nB\nC\nD\nc"}),
    );
    log.finished(2, "t2", true, Some("The file has been updated."));
    log.changed(2, "/w/task-003/src/cli.rs", "update");
    log.tool(
        2,
        "t3",
        "Write",
        json!({"file_path": "src/limit.rs", "content": "one\ntwo\n"}),
    );
    log.finished(2, "t3", true, Some("File created"));
    log.changed(2, "src/limit.rs", "add");
    log.tool(2, "t4", "Bash", json!({"command": "cargo test"}));
    log.finished(
        2,
        "t4",
        false,
        Some("Exit code 1\n\u{1b}[1m\u{1b}[31merror[E0425]\u{1b}[0m: cannot find value `x`\n"),
    );
    log.tool(2, "t5", "Grep", json!({"pattern": "TODO"}));
    log.finished(2, "t5", true, Some(""));
    let item = |text: &str, status| TodoItem {
        text: text.to_owned(),
        status,
    };
    log.agent(
        2,
        AgentEvent::Todo {
            items: vec![
                item("Read the CLI", TodoStatus::Completed),
                item("Fix the test", TodoStatus::InProgress),
                item("Write docs", TodoStatus::Pending),
            ],
        },
    );
    log.input(2, &[("guidance", "Use the existing limiter.")]);
    log.agent(
        2,
        AgentEvent::TextDelta {
            text: "Switching to the limiter.".to_owned(),
            parent: None,
        },
    );
    log.tool(
        2,
        "c1",
        "command_execution",
        json!({"command": ["bash", "-lc", "cargo build"]}),
    );
    log.finished(2, "c1", true, Some("   Compiling x\n"));
    log.tool(
        2,
        "f1",
        "file_change",
        json!({"changes": [
            {"path": "src/a.rs", "kind": "add"},
            {"path": "src/b.rs", "kind": "delete"},
        ]}),
    );
    log.finished(2, "f1", true, None);
    log.changed(2, "src/a.rs", "add");
    log.changed(2, "docs/c.md", "update");
    log.agent(
        2,
        AgentEvent::ToolStarted {
            id: "s1".to_owned(),
            name: "Read".to_owned(),
            input: json!({"file_path": "README.md"}),
            parent: Some("t9".to_owned()),
        },
    );
    log.tool(2, "t6", "Bash", json!({"command": "sleep 100"}));
    log.message(2, "Stopping here.");
    log.agent(
        2,
        AgentEvent::Result {
            text: Some("Stopping here.".to_owned()),
            structured: None,
            is_error: false,
        },
    );
    log.agent(
        2,
        AgentEvent::Error {
            category: ErrorCategory::UsageLimit,
            message: "You've hit your limit".to_owned(),
            retryable: false,
        },
    );
    log.ended(2, EndReason::UsageLimit { resets_at: None });

    let session = old_session(&task_id(), &log.events).expect("a building session");
    assert_eq!(session.number, 2);
    assert_eq!(session.worktree, Some(Path::new("/w/task-003")));
    let text = render(&task(TASK), &session, &log.events, "unused");
    let expected = "\
# task-003 Middleware · claude-code/opus · why it stopped: usage limit

Written from the run's event log. Each tool's output is cut to 40 lines; the raw log has all of it.

## Prompt

```
You own task-003.
Add the rate-limit middleware.
```

## Session

I'll start by reading.

Read src/cli.rs

```
fn main() {}
```

Edited src/cli.rs (+3 −1)

```
The file has been updated.
```

Created src/limit.rs (+2 −0)

```
File created
```

Ran `cargo test` → exit 1

```
error[E0425]: cannot find value `x`
```

Used Grep: TODO

To-do list:

- [x] Read the CLI
- [~] Fix the test
- [ ] Write docs

Sent to the agent (guidance):

```
Use the existing limiter.
```

Switching to the limiter.

Ran `bash -lc cargo build` → exit 0

```
   Compiling x
```

Created src/a.rs, Deleted src/b.rs

Edited docs/c.md

↳ Read README.md

Ran `sleep 100` → no result before it stopped

Stopping here.

Error, usage limit: You've hit your limit

";
    assert_eq!(text, expected);
}

#[test]
fn a_stall_the_agent_got_past_is_no_error() {
    let mut log = one_session();
    log.tool(1, "t1", "Bash", json!({"command": "cargo test"}));
    log.agent(
        1,
        AgentEvent::Error {
            category: ErrorCategory::Stalled,
            message: "Claude Code has sent nothing for 45 seconds, not even its heartbeat".into(),
            retryable: true,
        },
    );
    log.finished(1, "t1", true, Some("test result: ok"));
    log.message(1, "The tests pass.");
    let text = transcript_of(&log);
    assert!(!text.contains("Error,"), "{text}");
    assert!(!text.contains("heartbeat"), "{text}");
    assert!(text.contains("The tests pass."), "{text}");
}

#[test]
fn a_session_without_its_end_takes_the_reason_given() {
    let mut log = one_session();
    log.message(1, "Working.");
    let text = transcript_of(&log);
    assert!(
        text.starts_with(
            "# task-003 Middleware · claude-code/opus · why it stopped: it stopped without a \
             recorded reason\n"
        ),
        "{text}"
    );
}

#[test]
fn a_long_output_keeps_its_first_and_last_lines() {
    let mut log = one_session();
    let output: Vec<String> = (1..=100).map(|n| format!("line {n}")).collect();
    log.tool(1, "t1", "Bash", json!({"command": "cargo test"}));
    log.finished(1, "t1", true, Some(&output.join("\n")));
    let text = transcript_of(&log);

    let mut kept: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
    kept.push("… 60 lines cut (full output in the raw log)".to_owned());
    kept.extend((81..=100).map(|n| format!("line {n}")));
    assert_eq!(
        fenced_after(&text, "Ran `cargo test` → exit 0"),
        kept.join("\n")
    );

    // Forty lines fit whole, and one long line is cut on its own.
    let mut log = one_session();
    let mut forty: Vec<String> = (1..=39).map(|n| format!("line {n}")).collect();
    forty.push("y".repeat(5_000));
    log.tool(1, "t1", "Bash", json!({"command": "cat big"}));
    log.finished(1, "t1", true, Some(&forty.join("\n")));
    let text = transcript_of(&log);
    let block = fenced_after(&text, "Ran `cat big` → exit 0");
    assert_eq!(block.lines().count(), 40);
    assert!(!block.contains("cut (full output"));
    let last = block.lines().last().expect("the long line");
    assert_eq!(last.chars().count(), 1_000);
    assert!(last.ends_with('…'));
}

#[test]
fn fences_outlast_the_backticks_inside() {
    let mut log = one_session();
    log.tool(1, "t1", "Read", json!({"file_path": "README.md"}));
    log.finished(1, "t1", true, Some("```rust\nfn x() {}\n```\n"));
    log.tool(1, "t2", "Bash", json!({"command": "echo `date`"}));
    log.finished(1, "t2", true, None);
    let text = transcript_of(&log);
    assert!(
        text.contains("Read README.md\n\n````\n```rust\nfn x() {}\n```\n````\n"),
        "{text}"
    );
    assert!(text.contains("Ran `` echo `date` `` → exit 0\n"), "{text}");
}

#[test]
fn the_oldest_turns_go_past_400_kb() {
    let mut log = one_session();
    let filler = "x".repeat(20 * 1024);
    for turn in 0..30 {
        log.message(1, &format!("turn {turn:02} {filler}"));
        log.tool(1, &format!("t{turn}"), "Bash", json!({"command": "true"}));
        log.finished(1, &format!("t{turn}"), true, None);
    }
    let text = transcript_of(&log);
    assert!(text.len() <= MAX_BYTES, "{} bytes", text.len());
    assert!(text.contains("Build it."), "the prompt stays");

    let kept: Vec<usize> = (0..30)
        .filter(|turn| text.contains(&format!("turn {turn:02} ")))
        .collect();
    let cut = 30 - kept.len();
    assert!(cut > 0 && !kept.is_empty());
    // The newest turns are the ones kept.
    assert_eq!(kept, (cut..30).collect::<Vec<_>>());
    let note = format!(
        "\n## Session\n\n… {cut} earlier turns cut, to keep this file under about 400 KB (the \
         raw log has everything)\n\nturn {cut:02} "
    );
    assert!(text.contains(&note), "{}", &text[..300]);
}

#[test]
fn a_turn_too_big_alone_keeps_its_latest_steps() {
    let mut log = one_session();
    log.message(1, "Running everything.");
    let output: Vec<String> = (0..40)
        .map(|n| format!("{n:02} {}", "z".repeat(40)))
        .collect();
    let output = output.join("\n");
    for step in 0..400 {
        let id = format!("t{step}");
        log.tool(
            1,
            &id,
            "Bash",
            json!({"command": format!("step {step:03}")}),
        );
        log.finished(1, &id, true, Some(&output));
    }
    let text = transcript_of(&log);
    assert!(text.len() <= MAX_BYTES, "{} bytes", text.len());
    assert!(!text.contains("Running everything."));
    assert!(text.contains("Ran `step 399` → exit 0"));

    let first_kept = (0..400)
        .find(|step| text.contains(&format!("Ran `step {step:03}`")))
        .expect("some steps kept");
    // The message is the turn's first step, then every call before the first kept.
    let note = format!(
        "… the first {} steps of this turn cut, to keep this file under about 400 KB",
        first_kept + 1
    );
    assert!(text.contains(&note), "{}", &text[..400]);
}

#[test]
fn the_old_session_is_the_last_one_that_builds() {
    let mut log = Log::new();
    assert!(old_session(&task_id(), &log.events).is_none());

    log.started(1, "codex/gpt-5", Access::Build);
    log.started(2, "codex/gpt-5", Access::ReadOnly);
    // Another task's session is never this task's.
    log.push(
        Some("task-009"),
        Some(7),
        RunEventKind::Session(SessionEvent::Ended(Box::new(SessionEnd {
            cli_session: None,
            exit_code: None,
            signal: None,
            duration_ms: 0,
            reason: EndReason::Completed,
            spend: Spend::default(),
            tool_calls: 0,
            result_text: None,
            structured: None,
            last_message: None,
            raw_log: None,
        }))),
    );
    let session = old_session(&task_id(), &log.events).expect("one building session");
    assert_eq!((session.key, session.number), (&SessionKey::numbered(1), 1));
    assert_eq!(session.worktree, None);

    log.started(3, "claude-code/opus", Access::Experiment);
    log.worktree(Path::new("/w/task-003"));
    let session = old_session(&task_id(), &log.events).expect("two building sessions");
    assert_eq!((session.key, session.number), (&SessionKey::numbered(3), 2));
    assert_eq!(session.started.worker, worker("claude-code/opus"));
    assert_eq!(session.worktree, Some(Path::new("/w/task-003")));
}

#[tokio::test]
async fn no_commit_in_the_worktree_takes_the_transcript() {
    let repo = TestRepo::new();
    let git = Git::with_env(git_test_env());
    let base = git
        .head_commit(&repo.root)
        .await
        .expect("git runs")
        .expect("a first commit");
    let tree = repo.dir.path().join("trees/run-01/task-003");
    git.add_worktree(&repo.root, &tree, "argonauts/run-01/task-003", &base)
        .await
        .expect("a worktree");
    let run_dir = repo.dir.path().join("state/runs/run-01");

    let paths = write("# task-003\n", 1, &tree, &run_dir, &task_id()).expect("written");
    assert_eq!(
        paths.worktree,
        tree.join(".argonauts/fallback/1/transcript.md")
    );
    assert_eq!(
        paths.run,
        run_dir.join("tasks/task-003/fallback-1/transcript.md")
    );
    for path in [&paths.worktree, &paths.run] {
        assert_eq!(
            std::fs::read_to_string(path).expect("a copy"),
            "# task-003\n"
        );
    }
    assert_eq!(
        git.commit_all(&tree, "nothing").await.expect("git runs"),
        None
    );

    // A later fallback's transcript stays out too, and the exclude lists the folder once.
    write("# task-003 again\n", 2, &tree, &run_dir, &task_id()).expect("written");
    std::fs::write(tree.join("limit.rs"), "fn limit() {}\n").expect("a file");
    let sha = git
        .commit_all(&tree, "work")
        .await
        .expect("git runs")
        .expect("a commit");
    let files = repo.git(&["show", "--name-only", "--format=", &sha]);
    assert_eq!(files, "limit.rs");
    let exclude = std::fs::read_to_string(repo.root.join(".git/info/exclude")).expect("exclude");
    assert_eq!(
        exclude
            .lines()
            .filter(|line| *line == EXCLUDE_ENTRY)
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn the_runs_copy_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("a temp dir");
    let tree = temp.path().join("tree");
    let paths = write("# t\n", 1, &tree, &temp.path().join("run"), &task_id()).expect("written");
    let mode = std::fs::metadata(&paths.run)
        .expect("the run's copy")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// A session that stopped on plan usage, in a worktree at `tree`.
fn stopped_in(tree: &Path) -> Log {
    let mut log = Log::new();
    log.worktree(tree);
    log.started(1, "claude-code/opus", Access::Build);
    log.input(1, &[("prompt", "Build it.")]);
    log.message(1, "Halfway there.");
    log.ended(1, EndReason::UsageLimit { resets_at: None });
    log
}

#[test]
fn the_note_names_the_transcript_when_its_context_wants_one() {
    let temp = tempfile::tempdir().expect("a temp dir");
    let tree = temp.path().join("tree");
    let run_dir = temp.path().join("run");
    let log = stopped_in(&tree);
    let task = task(TASK);

    let fixture = Fixture::new();
    let note = fixture
        .planner()
        .with_run_dir(run_dir.clone())
        .note(&task, &log.events);
    let paths = note.transcript.expect("note+transcript is the default");
    assert_eq!(paths.worktree, worktree_path(&tree, 1));
    assert_eq!(paths.run, run_path(&run_dir, &task.id, 1));
    let text = std::fs::read_to_string(&paths.worktree).expect("the worktree's copy");
    assert!(
        text.starts_with(
            "# task-003 Middleware · claude-code/opus · why it stopped: usage limit\n"
        )
    );
    assert!(text.contains("Halfway there."));
    assert_eq!(
        std::fs::read_to_string(&paths.run).expect("the run's copy"),
        text
    );

    // Without the run's folder the planner writes nothing.
    assert_eq!(fixture.planner().note(&task, &log.events).transcript, None);

    let mut summary = Fixture::new();
    summary.settings_mut().fallback.context = Some(FallbackContext::NoteAndSummary);
    let note = summary
        .planner()
        .with_run_dir(run_dir.clone())
        .note(&task, &log.events);
    assert!(note.transcript.is_some(), "the summary is made from it");
}

#[test]
fn fresh_and_note_get_no_transcript() {
    let temp = tempfile::tempdir().expect("a temp dir");
    let tree = temp.path().join("tree");
    let run_dir = temp.path().join("run");
    let log = stopped_in(&tree);
    let task = task(TASK);

    for context in [FallbackContext::Fresh, FallbackContext::Note] {
        let mut fixture = Fixture::new();
        fixture.settings_mut().fallback.context = Some(context);
        let note = fixture
            .planner()
            .with_run_dir(run_dir.clone())
            .note(&task, &log.events);
        assert_eq!(note.transcript, None, "{context:?}");
    }
    // The stopped agent's own rule wins over the global one.
    let mut fixture = Fixture::new();
    fixture.settings_mut().fallback.replace.insert(
        agent_id("claude-code"),
        AgentFallbacks {
            context: Some(FallbackContext::Note),
            ..AgentFallbacks::default()
        },
    );
    let note = fixture
        .planner()
        .with_run_dir(run_dir.clone())
        .note(&task, &log.events);
    assert_eq!(note.transcript, None);
    assert!(!tree.join(WORKTREE_FOLDER).exists());
    assert!(!run_dir.join("tasks").exists());
}
