use argonauts_core::behavior_map::{Grouping, MapRequest};
use argonauts_core::behaviors::LABELS;
use argonauts_core::crew_stats::{CrewStats, TaskRecord};
use argonauts_core::display_names::DisplayNames;
use serde_json::{Value, json};
use time::macros::datetime;

use super::read::{Outcome, Records, input_words, project_name};
use super::{Recorded, build};
use crate::behaviors::labeller::{self, ToolCall};

/// The place the label list gives `words`, with `y` up as the map draws it.
fn place_of(words: &str) -> (f64, f64) {
    let info = LABELS
        .iter()
        .find(|info| info.words == words)
        .unwrap_or_else(|| panic!("{words} is on the list"));
    (f64::from(info.place.0), 1.0 - f64::from(info.place.1))
}

fn action(n: usize, cli: &str, model: &str, tool: &str, input: Value) -> Recorded {
    Recorded {
        key: format!("council-0123456789ab/run-1/s-{n}/call-{n}"),
        session: format!("council-0123456789ab/run-1/s-{n}"),
        run: Some("run-1".to_owned()),
        cli: cli.to_owned(),
        model: Some(model.to_owned()),
        role: "builder".to_owned(),
        agent: "build:due-flag".to_owned(),
        workflow: "spec-first".to_owned(),
        project: "council".to_owned(),
        at: datetime!(2026-10-20 12:30 UTC),
        nth: 1,
        of: 1,
        tool: tool.to_owned(),
        behavior: labeller::label(&ToolCall {
            name: tool,
            input: &input,
            ok: Some(true),
            patch: None,
            root: None,
        }),
        what: input_words(&input),
        failed: false,
        in_task: true,
        outcome: None,
        settled_at: None,
        ended: None,
        ended_at: None,
        place: None,
        run_shows: None,
    }
}

/// Haiku reads and edits; Luna reads git history and answers; one call no label fits.
fn records() -> Records {
    let mut actions = Vec::new();
    let mut n = 0;
    let mut add = |count: usize, cli: &str, model: &str, tool: &str, input: Value| {
        for _ in 0..count {
            n += 1;
            actions.push(action(n, cli, model, tool, input.clone()));
        }
    };
    add(
        12,
        "claude-code",
        "claude-haiku-5-5",
        "Read",
        json!({"file_path": "src/a.rs"}),
    );
    add(
        6,
        "claude-code",
        "claude-haiku-5-5",
        "Edit",
        json!({"file_path": "src/a.rs"}),
    );
    add(
        2,
        "claude-code",
        "claude-haiku-5-5",
        "Bash",
        json!({"command": "git log -3"}),
    );
    add(
        5,
        "codex",
        "gpt-6-luna",
        "command_execution",
        json!({"command": "/bin/zsh -lc 'git log --oneline'"}),
    );
    add(4, "codex", "gpt-6-luna", "structured_output", json!({}));
    add(
        1,
        "codex",
        "gpt-6-luna",
        "Read",
        json!({"file_path": "src/a.rs"}),
    );
    add(
        3,
        "antigravity-cli",
        "gemini-3.8-flash",
        "Read",
        json!({"path": "b.rs"}),
    );
    add(
        1,
        "claude-code",
        "claude-haiku-5-5",
        "KillShell",
        json!({"shell_id": "bash_1"}),
    );
    Records {
        actions,
        runs: 1,
        conversations: 0,
    }
}

fn request(grouping: Grouping) -> MapRequest {
    MapRequest {
        grouping,
        ..MapRequest::default()
    }
}

#[test]
fn each_labelled_action_is_a_dot_near_its_labels_place_and_the_rest_are_counted() {
    let map = build(
        &records(),
        request(Grouping::Model),
        &DisplayNames::default(),
    );
    assert_eq!(map.actions.len(), 33);
    assert_eq!(map.unlabelled, 1);
    let names: Vec<(&str, usize)> = map
        .groups
        .iter()
        .map(|group| (group.name.as_str(), group.actions))
        .collect();
    assert_eq!(
        names,
        [
            ("Haiku 5.5", 20),
            ("GPT 6 Luna", 10),
            ("Gemini 3.8 Flash", 3)
        ],
        "largest first"
    );
    for action in &map.actions {
        let place = place_of(&action.label);
        let off = (f64::from(action.x) - place.0).hypot(f64::from(action.y) - place.1);
        assert!(off < 0.1, "{} is {off} from its place", action.label);
        assert_eq!(action.behavior, action.label);
        assert_eq!(map.groups[action.group].name, action.model);
    }
    let luna = map
        .actions
        .iter()
        .find(|a| a.model == "GPT 6 Luna")
        .unwrap();
    assert_eq!(luna.input, "git log --oneline", "without the login shell");
    assert_eq!(luna.cli, "Codex");
    assert_eq!(map.projects, ["council"]);
}

#[test]
fn each_group_is_compared_with_the_rest_and_a_small_one_says_why_it_has_no_overlay() {
    let map = build(
        &records(),
        request(Grouping::Model),
        &DisplayNames::default(),
    );
    let luna = &map.comparisons[1];
    assert_eq!((luna.mine, luna.others), (10, 23));
    assert!(luna.overlay.as_ref().is_some_and(|grid| grid.len() == 3600));
    let rows: Vec<(&str, usize, usize)> = luna
        .rows
        .iter()
        .map(|row| (row.behavior.as_str(), row.mine, row.others))
        .collect();
    assert_eq!(
        rows,
        [
            ("returned its structured answer", 4, 0),
            ("read git history", 5, 2)
        ]
    );
    assert_eq!(
        luna.rows[1].lit.len(),
        7,
        "every action under the label lights"
    );
    let gemini = &map.comparisons[2];
    assert!(gemini.overlay.is_none());
    assert_eq!(
        gemini.why_not.as_deref(),
        Some("3 actions are too few to draw where it acts: it takes 5")
    );
}

#[test]
fn the_summaries_list_is_in_farthest_point_order_each_row_lighting_its_neighbours() {
    let mut map = build(
        &records(),
        request(Grouping::Model),
        &DisplayNames::default(),
    );
    map.placing = argonauts_core::behavior_map::Placing::Summaries {
        by: "Qwen3 Coder 30B on Ollama".to_owned(),
    };
    map.comparisons = super::compare_all(&map);
    let haiku = &map.comparisons[0];
    assert!(!haiku.rows.is_empty() && haiku.rows.len() <= 8);
    let first = &haiku.rows[0];
    assert_eq!(first.lit.len(), 14, "the action and the 13 nearest");
    assert_eq!(first.label.as_deref(), Some(first.behavior.as_str()));
    let places: std::collections::BTreeSet<&str> =
        haiku.rows.iter().map(|row| row.behavior.as_str()).collect();
    assert!(
        places.len() >= 2,
        "the order spreads across the map: {places:?}"
    );
    assert!(
        map.comparisons[2].rows.is_empty(),
        "a small group has no ranking"
    );
}

/// Codex records no model for a session on its default. Its actions count as that model's, as
/// Stats files the same sessions, in the groups, the hover lines and the world's stars alike,
/// and never as a model called Codex.
#[test]
fn an_action_that_names_no_model_counts_as_its_clis_default() {
    use argonauts_tui::format::WallClock;

    let names = DisplayNames {
        defaults: [("codex".to_owned(), "gpt-6-luna".to_owned())].into(),
        ..DisplayNames::default()
    };
    let mut records = records();
    let codex = records.actions.iter_mut().filter(|a| a.cli == "codex");
    for action in codex.take(4) {
        action.model = None;
    }
    super::spell_models(&mut records.actions, &names);

    let map = build(&records, request(Grouping::Model), &names);
    let groups: Vec<(&str, usize)> = map
        .groups
        .iter()
        .map(|group| (group.name.as_str(), group.actions))
        .collect();
    assert_eq!(
        groups,
        [
            ("Haiku 5.5", 20),
            ("GPT 6 Luna", 10),
            ("Gemini 3.8 Flash", 3)
        ]
    );
    let codex = map.actions.iter().filter(|a| a.cli == "Codex");
    assert!(codex.clone().count() == 10 && codex.clone().all(|a| a.model == "GPT 6 Luna"));

    let end = datetime!(2026-10-21 00:00 UTC);
    let all_time = super::world::Period::new(end, None, WallClock::pinned(end));
    let world = super::world::facts(&records.actions, &all_time, &names, None);
    let stars: Vec<(&str, &str)> = world
        .models
        .iter()
        .map(|star| (star.agent.as_str(), star.model.as_str()))
        .filter(|(agent, _)| *agent == "codex")
        .collect();
    assert_eq!(stars, [("codex", "gpt-6-luna")]);

    // Stats files a turn that named no model under the same model.
    let mut stats = CrewStats {
        tasks: vec![TaskRecord {
            agent: "codex".to_owned(),
            model: None,
            kind: None,
            sent_back: 0,
            reasons: Vec::new(),
            merged: true,
            reviewed: true,
        }],
        ..CrewStats::default()
    };
    crate::work_ledger::name_unnamed_models(&mut stats, &names.defaults);
    assert_eq!(stats.tasks[0].model.as_deref(), Some("gpt-6-luna"));
}

/// With no default known for its CLI, an action that names no model keeps none, and its
/// group reads as the CLI alone.
#[test]
fn an_action_whose_clis_default_nobody_knows_reads_as_the_cli() {
    let mut records = records();
    for action in records.actions.iter_mut().filter(|a| a.cli == "codex") {
        action.model = None;
    }
    super::spell_models(&mut records.actions, &DisplayNames::default());
    assert!(
        records
            .actions
            .iter()
            .all(|a| a.cli != "codex" || a.model.is_none())
    );
    let map = build(&records, request(Grouping::Model), &DisplayNames::default());
    assert!(map.groups.iter().any(|group| group.name == "Codex"));
}

#[test]
fn the_groups_follow_the_grouping() {
    let by_cli = build(&records(), request(Grouping::Cli), &DisplayNames::default());
    let names: Vec<&str> = by_cli.groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Claude Code", "Codex", "agy"]);
    let by_role = build(
        &records(),
        request(Grouping::Role),
        &DisplayNames::default(),
    );
    assert_eq!(by_role.groups[0].name, "builder");
    assert_eq!(
        by_role.comparisons[0].why_not.as_deref(),
        Some("No other role has 5 actions to compare it with")
    );
    let mut done = records();
    done.actions[0].outcome = Some(Outcome::Merged);
    done.actions[1].run = None;
    done.actions[2].in_task = false;
    let by_outcome = build(&done, request(Grouping::Outcome), &DisplayNames::default());
    let names: Vec<&str> = by_outcome.groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(
        names,
        ["not finished", "merged", "in conversation", "no task"]
    );
}

/// A dot keeps whether its call failed and what became of its work, which the world reads.
#[test]
fn a_dot_says_whether_it_failed_and_how_its_work_ended() {
    use argonauts_core::behavior_map::ActionOutcome;

    let mut records = records();
    records.actions[0].failed = true;
    records.actions[0].outcome = Some(Outcome::SentBack);
    records.actions[1].outcome = Some(Outcome::Dropped);
    let map = build(&records, request(Grouping::Model), &DisplayNames::default());
    let first: Vec<_> = map
        .actions
        .iter()
        .take(3)
        .map(|action| (action.failed, action.outcome))
        .collect();
    assert_eq!(
        first,
        [
            (true, Some(ActionOutcome::SentBack)),
            (false, Some(ActionOutcome::Dropped)),
            (false, None)
        ]
    );
}

/// The map shows the period's actions, counting only the runs and conversations they came
/// from, while the world reads every action recorded.
#[test]
fn the_period_s_records_count_their_own_runs_and_conversations() {
    use crate::behaviors::actions::Action;

    let at = |day: u8| time::Date::from_calendar_date(2026, time::Month::October, day).unwrap();
    let mut actions = Vec::new();
    for (run, day, conversation) in [
        ("run-1", 3, false),
        ("run-2", 20, false),
        ("conversation-1", 21, true),
    ] {
        actions.push(Action {
            run: run.to_owned(),
            at: at(day).midnight().assume_utc(),
            conversation,
            ..Action::sample()
        });
    }
    let all = super::read::of_actions(actions);
    assert_eq!((all.runs, all.conversations), (2, 1));
    let since = all.since(Some(at(10).midnight().assume_utc()));
    assert_eq!(since.actions.len(), 2);
    assert_eq!((since.runs, since.conversations), (1, 1));
    assert_eq!(all.since(None), all);
}

#[test]
fn a_run_that_named_no_workflow_groups_under_no_workflow() {
    let action = crate::behaviors::actions::Action {
        workflow: None,
        ..crate::behaviors::actions::Action::sample()
    };
    let records = super::read::of_actions(vec![action]);
    assert_eq!(records.actions[0].workflow, "no workflow");
}

#[test]
fn a_project_is_its_folder_without_the_ids_hash() {
    let state = std::path::Path::new("/Users/you/.argonauts/state/council-0123456789ab");
    assert_eq!(project_name(state), "council");
    assert_eq!(project_name(std::path::Path::new("/x/my-app")), "my-app");
}

#[test]
fn an_actions_input_reads_as_one_line() {
    assert_eq!(
        input_words(&json!({"command": ["/bin/zsh", "-lc", "ls -la"]})),
        "ls -la"
    );
    assert_eq!(
        input_words(&json!({"command": "/bin/zsh -lc 'git status'"})),
        "git status"
    );
    assert_eq!(
        input_words(&json!({"file_path": "src/lib.rs", "limit": 4})),
        "src/lib.rs"
    );
    assert_eq!(input_words(&json!({"pattern": "TODO"})), "TODO");
    assert_eq!(input_words(&json!({"a": 1})), "{\"a\":1}");
}

mod records {
    use std::collections::BTreeMap;

    use argonauts_core::events::RunEvent;
    use serde_json::json;

    use super::super::read::{Outcome, Records, of_actions};
    use crate::behaviors::actions::{of_conversation, of_run};
    use crate::behaviors::turn_times::TurnTimes;

    fn event(
        seq: u64,
        session: Option<&str>,
        task: Option<&str>,
        kind: serde_json::Value,
    ) -> RunEvent {
        let mut line = json!({
            "seq": seq,
            "ts": "2026-10-20T12:30:00Z",
            "run_id": "run-20261020-123036-4e50",
            "source": "codex",
            "kind": kind,
        });
        if let Some(session) = session {
            line["session"] = json!(session);
        }
        if let Some(task) = task {
            line["task_id"] = json!(task);
        }
        serde_json::from_value(line).expect("a run event")
    }

    /// A run's events in the log's own shape, its start from a recorded run, so a change to the
    /// log's shape shows here. The run has ended unless its task is still running.
    fn run(review: Option<(&str, &str)>, last: &str) -> Vec<RunEvent> {
        let recorded = include_str!("../../../tests/fixtures/runs/default-crew-run.jsonl");
        let mut started: serde_json::Value =
            serde_json::from_str(recorded.lines().next().unwrap()).unwrap();
        started["kind"]["data"]["workflow"] = json!("spec-first");
        started["kind"]["data"]["repo_root"] = json!("/Users/you/github/council");
        let session = json!({"type": "session", "data": {"event": "started", "worker": "codex/gpt-6-luna",
            "agent_id": "codex", "provider": "codex", "kind": "cli", "role": "worker", "step": null,
            "access": "build", "lifetime": "task", "resumed_from": null, "follow_key": null,
            "label": "build:per-api-key"}});
        let tool = |id: &str, command: &str| {
            json!({"type": "agent", "data": {"type": "tool_started", "id": id, "name": "command_execution",
                   "input": {"command": command}, "parent": null}})
        };
        let finished = |id: &str| {
            json!({"type": "agent", "data": {"type": "tool_finished", "id": id, "ok": true,
                   "output": null, "parent": null}})
        };
        let mut events = vec![
            started["kind"].clone(),
            session,
            tool("c1", "git status"),
            finished("c1"),
            tool("c2", "git show ef3f187"),
            json!({"type": "agent", "data": {"type": "file_patch", "id": "c2", "path": "a.py",
                   "hunks": "@@ -1 +1 @@\n-a\n+b", "parent": null}}),
            tool("c3", "from a session never started"),
        ];
        if let Some((verdict, summary)) = review {
            events.push(
                json!({"type": "review", "data": {"event": "reviewed", "reviewer": "claude-code",
                "blind": true, "round": 1, "review": {"verdict": verdict, "summary": summary,
                "comments": [], "tamper_suspected": false}}}),
            );
        }
        events.push(
            json!({"type": "task", "data": {"event": "state", "from": "running", "to": last,
            "reason": ""}}),
        );
        let task_at = events.len() - 1;
        if last != "running" {
            events.push(
                json!({"type": "run", "data": {"event": "finished", "outcome": "failed", "detail": null}}),
            );
        }
        let whose = |at: usize| match at {
            0 => (None, None),
            6 => (Some("s-002"), None),
            _ if at == task_at => (None, Some("task-001")),
            _ if at > task_at => (None, None),
            _ => (Some("s-001"), Some("task-001")),
        };
        events
            .iter()
            .enumerate()
            .map(|(at, kind)| {
                let (session, task) = whose(at);
                event(at as u64, session, task, kind.clone())
            })
            .collect()
    }

    /// The run's actions as the labels' records give them, in the project `council`.
    fn read(events: &[RunEvent]) -> Records {
        let catalog = argonauts_registry::models::shipped_catalog();
        let mut actions = of_run(
            "run-20261020-123036-4e50",
            events,
            &BTreeMap::new(),
            catalog,
        );
        for action in &mut actions {
            action.repo = "council-0123456789ab".to_owned();
        }
        of_actions(actions)
    }

    #[test]
    fn a_runs_tool_calls_carry_their_session_their_label_and_their_tasks_end() {
        let records = read(&run(None, "merged"));
        assert_eq!((records.runs, records.conversations), (1, 0));
        let actions = &records.actions;
        assert_eq!(
            actions.len(),
            2,
            "a call from a session that never started is left out"
        );
        let first = &actions[0];
        assert_eq!(
            first.key,
            "council-0123456789ab/run-20261020-123036-4e50/s-001/c1"
        );
        assert_eq!(first.cli, "codex");
        assert_eq!(first.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(first.role, "builder");
        assert_eq!(first.agent, "build:per-api-key");
        assert_eq!(first.workflow, "spec-first");
        assert_eq!(
            first.project, "council",
            "the state folder without its hash"
        );
        assert_eq!((first.nth, first.of), (1, 2));
        assert_eq!(first.behavior.words(), "checked git status or diff");
        assert_eq!(actions[1].behavior.words(), "read git history");
        assert_eq!(first.outcome, Some(Outcome::Merged));
        assert_eq!(first.run_shows.as_deref(), Some("Its task merged."));
        assert_eq!(first.place.as_deref(), Some("in the run's worktree"));
        assert_eq!(first.run.as_deref(), Some("run-20261020-123036-4e50"));
    }

    #[test]
    fn a_conversations_turns_are_the_main_agents_own() {
        let events: Vec<argonauts_core::app::ConversationEvent> = [
            json!({"conversation": "turn_started", "turn": "t-001", "text": "fix it",
                   "worker": "claude-code/claude-haiku-5-5", "read_only": false,
                   "commit": "abc", "uncommitted": []}),
            json!({"conversation": "agent", "turn": "t-001", "event": {"type": "tool_started", "id": "t1",
                   "name": "Read", "input": {"file_path": "a.rs"}, "parent": null}}),
            json!({"conversation": "turn_started", "turn": "t-002", "text": "what is this",
                   "worker": "claude-code/claude-haiku-5-5", "read_only": true,
                   "commit": "abc", "uncommitted": []}),
            json!({"conversation": "agent", "turn": "t-002", "event": {"type": "tool_started", "id": "t2",
                   "name": "Read", "input": {"file_path": "b.rs"}, "parent": null}}),
        ]
        .into_iter()
        .map(|line| serde_json::from_value(line).expect("a conversation event"))
        .collect();
        let catalog = argonauts_registry::models::shipped_catalog();
        let times = TurnTimes::all_at(time::OffsetDateTime::UNIX_EPOCH);
        let mut actions = of_conversation("c-001", &events, &times, None, catalog);
        for action in &mut actions {
            action.repo = "council-0123456789ab".to_owned();
        }
        let records = of_actions(actions);
        assert_eq!((records.runs, records.conversations), (0, 1));
        let actions = &records.actions;
        assert_eq!(actions.len(), 2, "every action the main agent took");
        assert_eq!(actions[0].key, "council-0123456789ab/c-001/t-001/t1");
        assert_eq!(actions[0].role, "main agent");
        assert_eq!(actions[0].workflow, "conversation");
        assert_eq!(actions[0].place.as_deref(), Some("in your folder"));
        assert_eq!(actions[0].run, None);
        assert_eq!(actions[0].behavior.words(), "read a source file");
    }

    #[test]
    fn the_review_after_an_action_is_its_outcome_and_its_words() {
        let back = read(&run(
            Some(("request_changes", "It copied a sibling's commit.")),
            "failed",
        ));
        assert_eq!(back.actions[0].outcome, Some(Outcome::SentBack));
        assert_eq!(
            back.actions[0].run_shows.as_deref(),
            Some("It copied a sibling's commit.")
        );
        let merged = read(&run(Some(("request_changes", "")), "merged"));
        assert_eq!(
            (
                merged.actions[0].outcome,
                merged.actions[0].run_shows.as_deref()
            ),
            (Some(Outcome::SentBack), Some("Review sent its task back.")),
            "the work it was part of went back, though the task merged in the end"
        );
        let dropped = read(&run(None, "cancelled"));
        assert_eq!(dropped.actions[0].outcome, Some(Outcome::Dropped));
        assert_eq!(
            dropped.actions[0].run_shows.as_deref(),
            Some("Its task was dropped.")
        );
        let going = read(&run(None, "running"));
        assert_eq!(
            (
                going.actions[0].outcome,
                going.actions[0].run_shows.as_deref()
            ),
            (None, None)
        );
    }
}

#[test]
fn the_label_list_labels_each_action_at_its_place_with_y_up() {
    let git_log = action(
        1,
        "claude-code",
        "claude-haiku-5-5",
        "Bash",
        json!({"command": "git log -3"}),
    );
    let placed = super::labelled(&git_log).expect("a label");
    assert_eq!(placed.label, "read git history");
    let (x, from_top) = argonauts_core::behaviors::Label::GitHistory.place();
    assert!((placed.place.0 - f64::from(x)).abs() < 1e-6);
    assert!((placed.place.1 - (1.0 - f64::from(from_top))).abs() < 1e-6);
}

#[test]
fn summaries_place_each_summarized_action_by_its_summary_and_marks_what_labels_only_group() {
    use crate::behaviors::deep::places::{KeptPlace, KeptPlaces};

    let records = records();
    // Every action but the last is summarized; Luna's structured answers say what they did.
    let summarized = &records.actions[..records.actions.len() - 1];
    let kept = KeptPlaces::new(
        "Qwen3 Coder 30B on Ollama".to_owned(),
        summarized.iter().enumerate().map(|(at, recorded)| {
            let summary = if recorded.tool == "structured_output" {
                "Accept task completion".to_owned()
            } else {
                format!("Summary {}", at % 3)
            };
            let place = KeptPlace {
                x: (at % 7) as f32 / 7.0,
                y: (at % 5) as f32 / 5.0,
                summary,
            };
            (recorded.key.clone(), place)
        }),
    );
    let map = super::build_summaries(
        &records,
        request(Grouping::Model),
        &DisplayNames::default(),
        &kept,
    );
    assert_eq!(
        map.placing,
        argonauts_core::behavior_map::Placing::Summaries {
            by: "Qwen3 Coder 30B on Ollama".to_owned()
        }
    );
    assert_eq!(
        map.actions.len(),
        summarized.len(),
        "one waits for the next update"
    );
    assert_eq!(map.unlabelled, 0);
    for action in &map.actions {
        assert_eq!(action.summary.as_deref(), Some(action.behavior.as_str()));
    }
    let answer = map
        .actions
        .iter()
        .find(|action| action.tool == "structured_output")
        .unwrap();
    assert_eq!(answer.label, "returned its structured answer");
    let marked: Vec<(&str, bool)> = map
        .comparisons
        .iter()
        .flat_map(|comparison| &comparison.rows)
        .map(|row| (row.label.as_deref().unwrap_or_default(), row.new))
        .collect();
    assert!(
        marked
            .iter()
            .all(|(label, new)| *new == (*label == "returned its structured answer")),
        "{marked:?}"
    );
    assert!(marked.iter().any(|(_, new)| *new), "{marked:?}");
}

mod setting {
    use argonauts_core::app::{AppEvent, UiCommand, ViewData, ViewRequest};
    use argonauts_core::behavior_map::MapRequest;
    use argonauts_core::settings::behaviors::BehaviorView;

    use crate::controller::test_support::{CliOptions, TestDriver};

    #[tokio::test]
    async fn the_map_carries_the_view_the_settings_ask_for() {
        let mut driver = TestDriver::start_with(CliOptions {
            flags: vec![r#"behaviors.view="braille""#.to_owned()],
            ..CliOptions::default()
        })
        .await;
        driver
            .send(UiCommand::Request {
                view: ViewRequest::Behaviors(MapRequest::default()),
            })
            .await;
        let shown = driver
            .until(|event| {
                matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Behaviors { .. }))
            })
            .await;
        let AppEvent::View { data } = shown else {
            unreachable!()
        };
        let ViewData::Behaviors { map } = *data else {
            unreachable!()
        };
        assert_eq!(map.view, BehaviorView::Braille);
    }
}
