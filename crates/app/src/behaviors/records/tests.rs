use argonauts_core::base::names::{RoleName, SessionKey};
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::{AgentEvent, RunId};
use serde_json::json;

use super::*;
use crate::client::test_support::{agent, event, session_started, write_journal};

const REPO: &str = "council-0123456789ab";

fn a_run(session: &SessionKey, command: &str, finish: bool) -> Vec<RunEvent> {
    let mut events = vec![
        session_started(0, session, None, RoleName::Worker, "codex"),
        agent(
            1,
            session,
            AgentEvent::ToolStarted {
                id: "item_1".to_owned(),
                name: "command_execution".to_owned(),
                input: json!({"command": command}),
                parent: None,
            },
        ),
    ];
    if finish {
        events.push(event(
            2,
            RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::Merged,
                detail: None,
            }),
        ));
    }
    events
}

struct Home {
    _dir: tempfile::TempDir,
    paths: Paths,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        Self { _dir: dir, paths }
    }

    fn log(&self, run: &str) -> PathBuf {
        let dir = self
            .paths
            .home()
            .join("state")
            .join(REPO)
            .join("runs")
            .join(run);
        fs::create_dir_all(&dir).unwrap();
        dir.join(EVENT_LOG_FILE)
    }

    fn kept(&self, run: &str) -> PathBuf {
        self.paths
            .home()
            .join("behaviors/labels")
            .join(REPO)
            .join(format!("{run}.json"))
    }
}

fn labels(actions: &[Action]) -> Vec<(String, String)> {
    actions
        .iter()
        .map(|action| (action.run.clone(), action.behavior.id()))
        .collect()
}

#[test]
fn every_run_of_every_project_is_read_and_an_ended_one_is_kept() {
    let home = Home::new();
    let session = SessionKey::numbered(1);
    let ended = RunId::new("run-20261020-120000-aaaa").unwrap();
    let going = RunId::new("run-20261020-130000-bbbb").unwrap();
    write_journal(
        &home.log(ended.as_str()),
        &a_run(&session, "git log -3", true),
    );
    write_journal(
        &home.log(going.as_str()),
        &a_run(&session, "cargo test", false),
    );

    let records = Records::of(&home.paths);
    let actions = records.actions(None);
    assert_eq!(
        labels(&actions),
        [
            (ended.to_string(), "git_history".to_owned()),
            (going.to_string(), "run_tests".to_owned()),
        ]
    );
    assert!(actions.iter().all(|action| action.project() == "council"));
    assert!(home.kept(ended.as_str()).is_file(), "an ended run is kept");
    assert!(
        !home.kept(going.as_str()).exists(),
        "nothing is written while a run goes"
    );

    // What was kept is read back, not the log.
    let kept = fs::read_to_string(home.kept(ended.as_str())).unwrap();
    fs::write(
        home.kept(ended.as_str()),
        kept.replace("git_history", "git_status"),
    )
    .unwrap();
    let again = records.actions(None);
    assert_eq!(again[0].behavior.id(), "git_status");
}

#[test]
fn a_read_for_the_inputs_keeps_nothing() {
    let home = Home::new();
    let session = SessionKey::numbered(1);
    let ended = RunId::new("run-20261020-120000-aaaa").unwrap();
    write_journal(
        &home.log(ended.as_str()),
        &a_run(&session, "git log -3", true),
    );
    let actions = Records::of(&home.paths).actions_with_input(None);
    assert_eq!(
        labels(&actions),
        [(ended.to_string(), "git_history".to_owned())]
    );
    assert!(!home.kept(ended.as_str()).exists());
}

#[test]
fn a_log_that_grew_or_older_labels_are_read_again() {
    let home = Home::new();
    let session = SessionKey::numbered(1);
    let run = "run-20261020-120000-aaaa";
    write_journal(&home.log(run), &a_run(&session, "git log -3", true));
    let records = Records::of(&home.paths);
    records.actions(None);

    let kept = fs::read_to_string(home.kept(run)).unwrap();
    let older = kept.replace(&format!("\"rules\":{RULES_VERSION}"), "\"rules\":0");
    fs::write(home.kept(run), older.replace("git_history", "git_status")).unwrap();
    assert_eq!(records.actions(None)[0].behavior.id(), "git_history");

    let more = agent(
        3,
        &session,
        AgentEvent::ToolStarted {
            id: "item_2".to_owned(),
            name: "command_execution".to_owned(),
            input: json!({"command": "ls"}),
            parent: None,
        },
    );
    write_journal(&home.log(run), &[more]);
    assert_eq!(records.actions(None).len(), 2);
}

#[test]
fn a_deleted_run_s_labels_go_with_it() {
    let home = Home::new();
    let session = SessionKey::numbered(1);
    let run = "run-20261020-120000-aaaa";
    write_journal(&home.log(run), &a_run(&session, "git log -3", true));
    let records = Records::of(&home.paths);
    records.actions(None);
    assert!(home.kept(run).is_file());

    fs::remove_dir_all(home.log(run).parent().unwrap()).unwrap();
    assert!(records.actions(None).is_empty());
    assert!(!home.kept(run).exists());
}

#[test]
fn a_period_keeps_only_its_actions() {
    let home = Home::new();
    let session = SessionKey::numbered(1);
    let run = "run-20261020-120000-aaaa";
    write_journal(&home.log(run), &a_run(&session, "git log -3", true));
    let records = Records::of(&home.paths);
    let at = records.actions(None)[0].at;
    assert_eq!(records.actions(Some(at)).len(), 1);
    assert!(
        records
            .actions(Some(at + time::Duration::seconds(1)))
            .is_empty()
    );
}

#[test]
fn the_main_agent_s_turns_come_from_the_conversations() {
    let home = Home::new();
    let dir = home
        .paths
        .home()
        .join("state")
        .join(REPO)
        .join("conversation");
    fs::create_dir_all(&dir).unwrap();
    let lines = [
        json!({"conversation": "turn_started", "turn": "turn-001", "text": "hi", "worker": "codex", "read_only": true, "commit": "abc", "uncommitted": []}),
        json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_started", "id": "t1", "name": "command_execution", "input": {"command": "rg -n slug"}, "parent": null}}),
    ];
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(dir.join("conversation-001.jsonl"), text + "{torn").unwrap();
    let actions = Records::of(&home.paths).actions(None);
    assert_eq!(
        labels(&actions),
        [("conversation-001".to_owned(), "search_code".to_owned())]
    );
    assert_eq!(
        actions[0].role,
        argonauts_core::behaviors::crew_stats::StatsRole::Main
    );
}

#[test]
fn project_names_lose_their_hash() {
    assert_eq!(project_name("council-0123456789ab"), "council");
    assert_eq!(project_name("my-crew-0123456789ab"), "my-crew");
    assert_eq!(project_name("plain"), "plain");
    assert_eq!(project_name("my-app-v2"), "my-app-v2");
}

/// A log written before a field was renamed keeps its sessions' actions: the start line that
/// no longer reads still names the worker, and the CLI's own start names the model.
#[test]
fn an_older_log_s_sessions_keep_their_actions() {
    let home = Home::new();
    let run = "run-20261004-072329-63c3";
    let older = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/older-run/events.jsonl");
    fs::copy(older, home.log(run)).unwrap();
    let actions = Records::of(&home.paths).actions(None);
    assert_eq!(actions.len(), 1);
    let action = &actions[0];
    assert_eq!(action.behavior.id(), "answer");
    assert_eq!(action.cli, "claude-code");
    assert_eq!(action.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(action.session, "s-001");
    assert_eq!(action.ok, Some(true));
    assert_eq!(
        action.outcome, None,
        "how its task ended may not read either"
    );
}

#[test]
fn the_same_conversation_in_two_projects_is_two_actions_with_two_keys() {
    let home = Home::new();
    for (repo, command) in [
        ("alpha-3f9a1c2b4d5e", "git log -3"),
        ("beta-4e8b2d3c5f6a", "pytest"),
    ] {
        let dir = home
            .paths
            .home()
            .join("state")
            .join(repo)
            .join("conversation");
        fs::create_dir_all(&dir).unwrap();
        let lines = [
            json!({"conversation": "turn_started", "turn": "turn-001", "text": "hi", "worker": "codex", "read_only": true, "commit": "abc", "uncommitted": []}),
            json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_started", "id": "item_1", "name": "command_execution", "input": {"command": command}, "parent": null}}),
        ];
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        fs::write(dir.join("conversation-001.jsonl"), text).unwrap();
    }
    let actions = Records::of(&home.paths).actions(None);
    let projects: Vec<&str> = actions.iter().map(Action::project).collect();
    assert_eq!(projects, ["alpha", "beta"]);
    let records = crate::views::behaviors::read::of_actions(actions);
    assert_ne!(records.actions[0].key, records.actions[1].key);
    assert_eq!(
        records.actions[0].of, 1,
        "each project's conversation is its own session"
    );
}

#[test]
fn touching_a_conversation_leaves_a_past_days_counts_as_they_were() {
    use time::macros::date;

    let home = Home::new();
    let dir = home
        .paths
        .home()
        .join("state")
        .join(REPO)
        .join("conversation");
    fs::create_dir_all(&dir).unwrap();
    let lines = [
        json!({"conversation": "turn_started", "turn": "turn-001", "text": "hi", "worker": "codex", "read_only": true, "commit": "abc", "uncommitted": [], "at": "2026-10-18T09:00:00Z"}),
        json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_started", "id": "item_1", "name": "command_execution", "input": {"command": "git log -3"}, "parent": null}}),
        json!({"conversation": "agent", "turn": "turn-001", "event": {"type": "tool_started", "id": "item_2", "name": "command_execution", "input": {"command": "pytest"}, "parent": null}}),
    ];
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    let journal = dir.join("conversation-001.jsonl");
    fs::write(&journal, text).unwrap();
    let count = |from, until| {
        let actions = Records::of(&home.paths).actions(None);
        super::super::report::count(&actions, (from, until), |_, model| {
            model.unwrap_or("default").to_owned()
        })
    };
    let first = count(date!(2026 - 10 - 18), date!(2026 - 10 - 19));
    assert_eq!(
        first.behaviors["default"]
            .values()
            .flat_map(|labels| labels.values())
            .sum::<u32>(),
        2
    );
    let next_day = std::time::SystemTime::from(time::macros::datetime!(2026-10-19 12:00 UTC));
    fs::File::options()
        .append(true)
        .open(&journal)
        .unwrap()
        .set_modified(next_day)
        .unwrap();
    assert_eq!(count(date!(2026 - 10 - 18), date!(2026 - 10 - 19)), first);
    assert!(
        count(date!(2026 - 10 - 19), date!(2026 - 10 - 20))
            .behaviors
            .is_empty(),
        "the next day counts none of them again"
    );
}
