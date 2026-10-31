use argonauts_core::base::names::{RoleName, SessionKey};
use argonauts_core::behaviors::Label;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::workflows::steps::judgment::ReviewOutput;
use serde_json::json;
use time::Duration;

use super::*;
use crate::client::test_support::{
    agent, event, run_started, session_started, task_event, task_spec,
};

fn tool(seq: u64, session: &SessionKey, id: &str, name: &str, input: Value) -> RunEvent {
    agent(
        seq,
        session,
        AgentEvent::ToolStarted {
            id: id.to_owned(),
            name: name.to_owned(),
            input,
            parent: None,
        },
    )
}

fn finished(seq: u64, session: &SessionKey, id: &str, ok: bool) -> RunEvent {
    agent(
        seq,
        session,
        AgentEvent::ToolFinished {
            id: id.to_owned(),
            ok,
            output: None,
            parent: None,
        },
    )
}

fn reviewed(seq: u64, task: &TaskId, verdict: ReviewVerdict) -> RunEvent {
    RunEvent {
        task_id: Some(task.clone()),
        ..event(
            seq,
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer: ModelRef::new("claude-code", None),
                blind: true,
                round: 1,
                review: ReviewOutput {
                    verdict,
                    summary: String::new(),
                    comments: Vec::new(),
                    tamper_suspected: false,
                },
                picked_by: None,
            }),
        )
    }
}

fn merged(seq: u64, task: &TaskId) -> RunEvent {
    task_event(
        seq,
        task,
        TaskEvent::State {
            from: Some(TaskState::Merging),
            to: TaskState::Merged,
            reason: String::new(),
        },
    )
}

/// A captain run: Codex builds the first task, a review sends it back, the second try is
/// approved and merged; Claude Code's try at the second task never merges.
fn a_run(finish: bool) -> Vec<RunEvent> {
    let (first_try, second_try, review, other) = (
        SessionKey::numbered(1),
        SessionKey::numbered(2),
        SessionKey::numbered(3),
        SessionKey::numbered(4),
    );
    let (one, two) = (task_spec(1), task_spec(2));
    let mut events = vec![
        run_started(0, "rate limits", None),
        session_started(1, &first_try, Some(&one.id), RoleName::Worker, "codex"),
        agent(
            2,
            &first_try,
            AgentEvent::Init {
                session_id: None,
                model: Some("gpt-6-luna".to_owned()),
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            },
        ),
        tool(
            3,
            &first_try,
            "item_1",
            "command_execution",
            json!({"command": "/bin/zsh -lc 'git status --short && git show 11db213'"}),
        ),
        finished(4, &first_try, "item_1", true),
        reviewed(5, &one.id, ReviewVerdict::RequestChanges),
        session_started(6, &second_try, Some(&one.id), RoleName::Worker, "codex"),
        tool(
            7,
            &second_try,
            "item_1",
            "file_change",
            json!({"changes": [{"path": "/w/ratelimit/bucket.py", "kind": "update"}]}),
        ),
        finished(8, &second_try, "item_1", true),
        session_started(9, &review, Some(&one.id), RoleName::Reviewer, "claude-code"),
        tool(
            10,
            &review,
            "toolu_1",
            "Read",
            json!({"file_path": "/w/tests/test_bucket.py"}),
        ),
        finished(11, &review, "toolu_1", true),
        reviewed(12, &one.id, ReviewVerdict::Approve),
        merged(13, &one.id),
        session_started(14, &other, Some(&two.id), RoleName::Worker, "claude-code"),
        tool(
            15,
            &other,
            "toolu_1",
            "Bash",
            json!({"command": "python3 -m pytest -q"}),
        ),
        finished(16, &other, "toolu_1", false),
        tool(17, &other, "toolu_2", "generate_image", json!({})),
    ];
    if finish {
        events.push(event(
            18,
            RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::Merged,
                detail: None,
            }),
        ));
    }
    events
}

fn catalog() -> &'static ModelCatalog {
    argonauts_registry::models::shipped_catalog()
}

#[test]
fn each_action_is_labelled_with_who_took_it() {
    let actions = of_run("run-1", &a_run(true), &BTreeMap::new(), catalog());
    let rows: Vec<_> = actions
        .iter()
        .map(|action| {
            (
                action.session.as_str(),
                action.cli.as_str(),
                action.model.as_deref(),
                action.role,
                action.behavior.id(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "s-001",
                "codex",
                Some("gpt-6-luna"),
                StatsRole::Worker,
                "git_status+git_history".to_owned()
            ),
            (
                "s-002",
                "codex",
                None,
                StatsRole::Worker,
                "edit_source".to_owned()
            ),
            (
                "s-003",
                "claude-code",
                None,
                StatsRole::Reviewer,
                "read_test".to_owned()
            ),
            (
                "s-004",
                "claude-code",
                None,
                StatsRole::Worker,
                "run_tests_failed".to_owned()
            ),
            (
                "s-004",
                "claude-code",
                None,
                StatsRole::Worker,
                "unlabelled".to_owned()
            ),
        ]
    );
    assert!(
        actions
            .iter()
            .all(|action| action.workflow.as_deref() == Some("captain"))
    );
    assert_eq!(actions[0].what, "git status --short && git show 11db213");
    assert_eq!(actions[1].what, "/w/ratelimit/bucket.py");
    assert_eq!(actions[3].ok, Some(false));
}

/// Work a review sent back is sent back even when the task merged later; the try that merged
/// and the review that passed it are merged; a task that never merged is dropped.
#[test]
fn each_action_s_outcome_is_the_first_verdict_after_it() {
    let outcomes: Vec<_> = of_run("run-1", &a_run(true), &BTreeMap::new(), catalog())
        .iter()
        .map(|action| action.outcome)
        .collect();
    assert_eq!(
        outcomes,
        [
            Some(Outcome::SentBack),
            Some(Outcome::Merged),
            Some(Outcome::Merged),
            Some(Outcome::Dropped),
            Some(Outcome::Dropped),
        ]
    );
}

#[test]
fn while_the_run_goes_an_open_task_has_no_outcome_yet() {
    let actions = of_run("run-1", &a_run(false), &BTreeMap::new(), catalog());
    assert_eq!(actions[3].outcome, None);
    assert_eq!(actions[0].outcome, Some(Outcome::SentBack));
}

#[test]
fn the_main_agent_s_own_turns_are_its_actions() {
    let lines = [
        json!({"conversation": "turn_started", "turn": "turn-001", "text": "hi", "worker": "claude-code/haiku", "read_only": false, "commit": "abc", "uncommitted": []}),
        json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_started", "id": "t1", "name": "Bash", "input": {"command": "git log -3"}, "parent": null}}),
        json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_finished", "id": "t1", "ok": true, "output": null, "parent": null}}),
        json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_started", "id": "t2", "name": "Edit", "input": {"file_path": "tests/test_a.py", "old_string": "def test_a():", "new_string": ""}, "parent": null}}),
        json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_finished", "id": "t2", "ok": true, "output": null, "parent": null}}),
    ];
    let events: Vec<ConversationEvent> = lines
        .into_iter()
        .map(|line| serde_json::from_value(line).unwrap())
        .collect();
    let at = time::macros::datetime!(2026-10-20 12:00 UTC);
    let times = crate::behaviors::turn_times::TurnTimes::all_at(at);
    let actions = of_conversation("conversation-001", &events, &times, None, catalog());
    let behaviors: Vec<_> = actions.iter().map(|action| action.behavior).collect();
    assert_eq!(
        behaviors,
        [
            Behavior::One(Label::GitHistory),
            Behavior::One(Label::EditTestRemoved)
        ]
    );
    assert!(
        actions
            .iter()
            .all(|action| action.role == StatsRole::Main && action.at == at)
    );
    assert_eq!(actions[0].model.as_deref(), Some("claude-haiku-5-5"));
}

#[test]
fn what_it_did_is_one_short_line() {
    let long = "x ".repeat(400);
    let what = what_of(&json!({"command": long}));
    assert_eq!(what.chars().count(), WHAT_CAP + 1);
    assert!(what.ends_with('…'));
    assert_eq!(what_of(&json!({"command": ["git", "log"]})), "git log");
    assert_eq!(what_of(&json!({"AbsolutePath": "/w/a.py"})), "/w/a.py");
}

fn ended(seq: u64, session: &SessionKey, reason: EndReason) -> RunEvent {
    use argonauts_core::runs::events::session::SessionEnd;
    crate::client::test_support::session_event(
        seq,
        session,
        None,
        RunEventKind::Session(SessionEvent::Ended(Box::new(SessionEnd {
            cli_session: None,
            exit_code: None,
            signal: None,
            duration_ms: 0,
            reason,
            spend: Default::default(),
            tool_calls: 0,
            result_text: None,
            structured: None,
            last_message: None,
            raw_log: None,
        }))),
    )
}

fn diffed(seq: u64, session: &SessionKey, path: &str, hunk: &str) -> RunEvent {
    use argonauts_core::runs::events::session::FileDiff;
    crate::client::test_support::session_event(
        seq,
        session,
        None,
        RunEventKind::Session(SessionEvent::FileDiff(FileDiff {
            path: path.to_owned(),
            added: 0,
            removed: 0,
            hunk: hunk.to_owned(),
            truncated: false,
        })),
    )
}

#[test]
fn a_codex_file_change_is_read_by_the_diff_taken_after_it() {
    let session = SessionKey::numbered(1);
    let one = task_spec(1);
    let change = |id: &str| json!({"changes": [{"path": "/w/run/task-1/tests/test_bucket.py", "kind": "update"}], "id": id});
    let removed =
        "@@ -1,4 +1,2 @@\n def test_a():\n     assert a()\n-def test_b():\n-    assert b()\n";
    let and_added = format!("{removed}@@ -9,1 +7,3 @@\n x = 1\n+def test_c():\n+    assert c()\n");
    let events = vec![
        run_started(0, "tests", None),
        session_started(1, &session, Some(&one.id), RoleName::Worker, "codex"),
        tool(2, &session, "item_1", "file_change", change("1")),
        finished(3, &session, "item_1", true),
        diffed(4, &session, "tests/test_bucket.py", removed),
        tool(5, &session, "item_2", "file_change", change("2")),
        finished(6, &session, "item_2", true),
        diffed(7, &session, "tests/test_bucket.py", &and_added),
    ];
    let labels: Vec<String> = of_run("run-1", &events, &BTreeMap::new(), catalog())
        .iter()
        .map(|action| action.behavior.id())
        .collect();
    assert_eq!(
        labels,
        ["edit_test_removed", "edit_test_added"],
        "each change by what it added to the diff before it"
    );
}

#[test]
fn a_failed_sessions_work_ends_as_its_task_does_unless_it_was_set_aside() {
    let (failed, retry) = (SessionKey::numbered(1), SessionKey::numbered(2));
    let one = task_spec(1);
    let error = EndReason::CliError {
        category: argonauts_core::ErrorCategory::Overloaded,
        message: "overloaded".to_owned(),
    };
    let base = |rewound: bool| {
        let mut events = vec![
            run_started(0, "tests", None),
            session_started(1, &failed, Some(&one.id), RoleName::Worker, "codex"),
            tool(
                2,
                &failed,
                "item_1",
                "command_execution",
                json!({"command": "rm -rf tests/test_flaky.py"}),
            ),
            finished(3, &failed, "item_1", true),
            ended(4, &failed, error.clone()),
        ];
        if rewound {
            events.push(task_event(
                5,
                &one.id,
                TaskEvent::Rewound {
                    to: 0,
                    kept_on: "argonauts/run-1/task-1-replaced-0".to_owned(),
                },
            ));
        }
        events.extend([
            session_started(6, &retry, Some(&one.id), RoleName::Worker, "codex"),
            tool(
                7,
                &retry,
                "item_1",
                "command_execution",
                json!({"command": "pytest"}),
            ),
            finished(8, &retry, "item_1", true),
            merged(9, &one.id),
        ]);
        events
    };
    let carried = of_run("run-1", &base(false), &BTreeMap::new(), catalog());
    let ends: Vec<_> = carried.iter().map(|a| (a.outcome, a.ended)).collect();
    assert_eq!(
        ends,
        [
            (Some(Outcome::Merged), Some(SessionEnded::Failed)),
            (Some(Outcome::Merged), None)
        ],
        "the next try carried its worktree on, and that work merged"
    );
    let set_aside = of_run("run-1", &base(true), &BTreeMap::new(), catalog());
    let outcomes: Vec<_> = set_aside.iter().map(|a| a.outcome).collect();
    assert_eq!(outcomes, [Some(Outcome::Dropped), Some(Outcome::Merged)]);
    let second = |seq: i64| Some(crate::client::test_support::base_ts() + Duration::seconds(seq));
    assert_eq!(
        set_aside.iter().map(|a| a.settled_at).collect::<Vec<_>>(),
        [second(5), second(9)],
        "dropped when its branch went back, the rest when it merged"
    );
}

/// Each outcome keeps the time of the event that settled it, so the map can tell work dropped
/// this week from work dropped long ago.
#[test]
fn an_outcome_keeps_when_it_was_settled() {
    let second = |seq: i64| Some(crate::client::test_support::base_ts() + Duration::seconds(seq));
    let settled: Vec<_> = of_run("run-1", &a_run(true), &BTreeMap::new(), catalog())
        .iter()
        .map(|action| action.settled_at)
        .collect();
    assert_eq!(
        settled,
        [second(5), second(13), second(13), second(18), second(18)],
        "sent back by the review, merged, and dropped when the run finished"
    );
    let going = of_run("run-1", &a_run(false), &BTreeMap::new(), catalog());
    assert_eq!(going[3].settled_at, None, "nothing has settled it yet");
}

/// A crash is told apart from the CLI's other failures, and a session's end keeps its time.
#[test]
fn a_crashed_session_is_a_crash_and_keeps_when_it_ended() {
    let (crashed, stuck, stopped) = (
        SessionKey::numbered(1),
        SessionKey::numbered(2),
        SessionKey::numbered(3),
    );
    let one = task_spec(1);
    let mut events = vec![run_started(0, "tests", None)];
    for (at, (session, reason)) in [
        (
            &crashed,
            EndReason::Crashed {
                message: "exited with signal 11".to_owned(),
            },
        ),
        (
            &stuck,
            EndReason::CliError {
                category: argonauts_core::ErrorCategory::Overloaded,
                message: "overloaded".to_owned(),
            },
        ),
        (&stopped, EndReason::ContextFull),
    ]
    .into_iter()
    .enumerate()
    {
        let seq = 1 + 4 * at as u64;
        events.extend([
            session_started(seq, session, Some(&one.id), RoleName::Worker, "codex"),
            tool(
                seq + 1,
                session,
                "item_1",
                "command_execution",
                json!({"command": "pytest"}),
            ),
            finished(seq + 2, session, "item_1", false),
            ended(seq + 3, session, reason),
        ]);
    }
    let actions = of_run("run-1", &events, &BTreeMap::new(), catalog());
    let ends: Vec<_> = actions.iter().map(|a| a.ended).collect();
    assert_eq!(
        ends,
        [
            Some(SessionEnded::Crashed),
            Some(SessionEnded::Failed),
            Some(SessionEnded::Stopped)
        ]
    );
    let second = |seq: i64| Some(crate::client::test_support::base_ts() + Duration::seconds(seq));
    assert_eq!(
        actions.iter().map(|a| a.ended_at).collect::<Vec<_>>(),
        [second(4), second(8), second(12)]
    );
    assert_eq!(SessionEnded::Crashed.name(), "crashed");
}
