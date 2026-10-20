//! The fallback contracts: fallback lists per agent, strategies, checkpoints, quality debt,
//! the original-returned task, and `/rewind`.

use argonauts_core::app::{AppEvent, ChoiceKind, UiCommand};
use argonauts_core::engine::EngineCommand;
use argonauts_core::events::agent_status::FallbackPlan;
use argonauts_core::events::approval::{ApprovalKind, MergeRow};
use argonauts_core::events::task::{TaskEvent, TaskSpec};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::fallback::{CheckpointTag, QualityDebt};
use argonauts_core::input::{Handler, ParsedInput, SLASH_COMMANDS, parse_input};
use argonauts_core::limits::{
    Candidate, CandidateQuery, FallbackNote, FallbackPlanner, Shortlist, TranscriptPaths,
};
use argonauts_core::names::{AgentId, ApprovalId, ModelRef, RoleName};
use argonauts_core::settings::context::ContextSize;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::fallback::{
    AgentFallbacks, BrokenWork, FallbackContext, FallbackEntry, FallbackMode, FallbackRules,
    FallbackSettings, FallbackStrategy, FallbackTable, MergeRule, OnReturn, OrderStrategy, Pick,
    UnknownTier,
};
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::settings::{SAFETY_KEYS, Settings, Span};
use argonauts_core::{RunId, TaskId};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::oneshot;

/// Parses `value` as `T`, writes it back, and checks nothing was lost or added.
fn round_trip<T: Serialize + DeserializeOwned + std::fmt::Debug + PartialEq>(value: Value) -> T {
    let parsed: T =
        serde_json::from_value(value.clone()).unwrap_or_else(|err| panic!("{err}\n{value:#}"));
    let written = serde_json::to_value(&parsed).unwrap();
    assert_eq!(written, value, "the JSON changed on the way through");
    let again: T = serde_json::from_value(written).unwrap();
    assert_eq!(again, parsed);
    parsed
}

fn envelope(kind: Value) -> Value {
    json!({
        "seq": 7,
        "ts": "2026-09-29T12:00:00Z",
        "run_id": "run-01",
        "task_id": "task-002",
        "source": "argonauts-code",
        "kind": kind,
    })
}

fn diff_stat() -> Value {
    json!({"files_changed": 2, "insertions": 18, "deletions": 4})
}

fn worker(text: &str) -> ModelRef {
    text.parse().unwrap()
}

fn debt() -> Value {
    json!({"since": 1, "by": "codex/gpt-x", "needs": "claude-code/opus"})
}

fn candidate() -> Value {
    json!({"worker": "codex/gpt-x", "plan_usage_left": 0.62, "plan_usage_text": "62%",
        "track_record": {"passed": 9, "of": 11}, "cost": {"unit": "plan_usage", "amount": 0.1},
        "needs_confirm": false, "may_run_out": false})
}

fn fallback_plan() -> Value {
    json!({"task": "task-002", "title": "Redis store", "stopped": "claude-code/opus",
        "reason": "usage limit, resets in 2h 10m", "resets_at": "2026-09-29T18:00:00Z",
        "work": diff_stat(), "checks_ran": false, "candidates": [candidate()], "mode": "ask"})
}

fn merge_row() -> Value {
    json!({"task": "task-002", "title": "Redis store", "kind": {"kind": "build"},
        "stat": diff_stat(), "worker": "codex", "checks_passed": true, "review": "approve"})
}

fn task_spec() -> TaskSpec {
    serde_json::from_value(json!({"id": "task-002", "title": "Redis store",
        "task": "Add the token bucket store", "kind": {"kind": "build"},
        "owned_paths": ["src/limits/store.ts"], "read_only_paths": [], "forbidden_paths": [],
        "hidden_paths": [], "depends_on": [], "criteria": ["G1"], "worker": "claude-code/opus",
        "reviewers": ["codex/gpt-x"],
        "limit": {"limit": {"unit": "usd", "amount": 3.5}, "max_minutes": 20.0,
                   "max_tool_calls": null, "inactivity_min": 5.0},
        "predictions": null}))
    .unwrap()
}

/// The settings the picture of fallback settings lists, as a file: every key reads, and with
/// them every role's fallbacks stay on Claude Code, in the order each list gives, the main
/// session's among them.
#[test]
fn the_pictured_fallback_settings_read_as_written() {
    let settings: Settings = toml::from_str(
        r#"
[fallback]
mode = "auto"
order = []
same_cli = true

[fallback.retry]
tries = 1
wait = "5s"
backoff = "fixed"

[fallback.replace.claude-code]
with = ["claude-code/sonnet", "codex/gpt-5.6"]

[fallback.main]
order = ["claude-code/sonnet", "codex"]

[health]
cooldown = "5m"
"#,
    )
    .unwrap();
    let claude = AgentId("claude-code".to_owned());
    let listed = |role: RoleName| -> (Vec<String>, bool) {
        let applied = settings.fallback.applied_to(role, &claude);
        let models = applied
            .candidates
            .iter()
            .map(|entry| entry.model().to_string());
        (models.collect(), applied.same_cli)
    };
    for role in [RoleName::Reviewer, RoleName::Worker] {
        let (models, same_cli) = listed(role);
        assert_eq!(models, ["claude-code/sonnet", "codex/gpt-5.6"], "{role:?}");
        assert!(same_cli, "{role:?} stays on the CLI that stopped");
    }
    let (models, same_cli) = listed(RoleName::Main);
    assert_eq!(models, ["claude-code/sonnet", "codex"]);
    assert!(
        same_cli,
        "the main session follows the key every role follows"
    );

    let retry = settings
        .fallback
        .applied_to(RoleName::Worker, &claude)
        .retry;
    assert_eq!(retry.tries, 1);
    assert_eq!(retry.wait_before(1), std::time::Duration::from_secs(5));
    assert_eq!(retry.wait_before(3), std::time::Duration::from_secs(5));
    let cooldown = settings.health.cooldown.parse().ok().flatten();
    assert_eq!(cooldown, Some(std::time::Duration::from_secs(300)));
}

#[test]
fn the_fallback_example_parses_with_every_list() {
    let settings: Settings = toml::from_str(
        r#"
[fallback]
mode = "auto"
strategy = "first"
on_return = "ask"
checkpoint = true
merge_rule = "upgrade_review"
unknown_tier = "weaker"
max_fallbacks = 3
same_cli = false
same_step = "ask"
context = "note+transcript"

[fallback.retry]
tries = 1
wait = "5s"
backoff = "fixed"

[fallback.replace.claude-code]
with = ["codex/<model>", "antigravity-cli/<model>"]
strategy = "spread"

[fallback.replace.codex]
with = ["claude-code/sonnet"]

[fallback.replace.antigravity-cli]
with = ["claude-code/sonnet", { model = "opencode/<model>", max_spend_usd = 2 }]

[fallback.reviewer]
order = ["ollama/<model>"]

[fallback.worker.replace.claude-code]
with = ["codex/<model>"]
on_return = "review"
"#,
    )
    .unwrap();
    let replace = &settings.fallback.replace;
    assert_eq!(replace.len(), 3);
    let agent_id = |id: &str| AgentId(id.to_owned());
    assert_eq!(
        replace[&agent_id("claude-code")],
        AgentFallbacks {
            with: vec![
                FallbackEntry::Model(worker("codex/<model>")),
                FallbackEntry::Model(worker("antigravity-cli/<model>")),
            ],
            strategy: Some(OrderStrategy::Spread),
            on_return: None,
            context: None,
        }
    );
    assert_eq!(
        replace[&agent_id("antigravity-cli")].with[1],
        FallbackEntry::WithRules(FallbackRules {
            model: worker("opencode/<model>"),
            max_spend_usd: Some(2.0),
            instructions: None,
            read_only: false,
            allow_api_billing: false,
        })
    );
    // Everything the example sets is also the default.
    assert_eq!(
        FallbackTable {
            replace: Default::default(),
            together: Some(1),
            ..settings.fallback.every_role.clone()
        },
        FallbackTable::built_in()
    );

    let reviewer = &settings.fallback.roles[&CrewRole::Reviewer];
    assert_eq!(
        reviewer.order,
        [FallbackEntry::Model(worker("ollama/<model>"))]
    );
    let worker_role = &settings.fallback.roles[&CrewRole::Worker].replace[&agent_id("claude-code")];
    assert_eq!(
        worker_role.with,
        [FallbackEntry::Model(worker("codex/<model>"))]
    );
    assert_eq!(worker_role.on_return, Some(OnReturn::Review));
    assert_eq!(worker_role.strategy, None);

    let json = serde_json::to_value(&settings).unwrap();
    assert_eq!(serde_json::from_value::<Settings>(json).unwrap(), settings);
}

#[test]
fn the_new_fallback_defaults_hold() {
    let fallback = FallbackTable::built_in();
    // An agent that runs out picks its fallback itself.
    assert_eq!(fallback.mode, FallbackMode::Auto);
    assert!(fallback.order.is_empty() && fallback.replace.is_empty());
    assert_eq!(fallback.ask_timeout, Span::new("none"));
    assert!(!fallback.allow_api_billing);
    assert_eq!(fallback.broken_work, BrokenWork::Continue);
    assert_eq!(fallback.strategy, Some(OrderStrategy::First));
    // One fallback at a time, and each role's own way of combining several.
    assert_eq!(fallback.together, Some(1));
    assert_eq!(fallback.pick, None);
    assert_eq!(fallback.on_return, Some(OnReturn::Ask));
    assert_eq!(fallback.checkpoint, Some(true));
    assert_eq!(fallback.merge_rule, Some(MergeRule::UpgradeReview));
    assert_eq!(fallback.unknown_tier, Some(UnknownTier::Weaker));
    assert_eq!(fallback.max_fallbacks, Some(3));
    assert_eq!(fallback.context, Some(FallbackContext::NoteAndTranscript));
    assert_eq!(fallback.summarizer, None);
    assert_eq!(AgentFallbacks::default().strategy, None);
    assert_eq!(AgentFallbacks::default().context, None);

    // A role table that sets nothing leaves every optional key unset.
    let unset = FallbackTable::default();
    assert_eq!(
        (
            unset.strategy,
            unset.together,
            unset.on_return,
            unset.checkpoint
        ),
        (None, None, None, None)
    );
    assert_eq!(
        (unset.merge_rule, unset.unknown_tier, unset.max_fallbacks),
        (None, None, None)
    );
    assert_eq!((unset.context, unset.summarizer.as_ref()), (None, None));
    let applied = FallbackTable::applied(&fallback, None, &AgentId("codex".into()));
    assert_eq!(
        FallbackTable::applied(&unset, Some(&unset), &AgentId("codex".into())),
        applied
    );
    assert_eq!(applied.mode, FallbackMode::Auto);
    // No list anywhere: the policy builder offers the other installed, logged-in CLIs.
    assert!(applied.candidates.is_empty());
    assert_eq!(applied.strategy, FallbackStrategy::First);
    assert_eq!(applied.max_fallbacks, 3);
    assert_eq!(applied.context, FallbackContext::NoteAndTranscript);
    assert_eq!(applied.summarizer, None);

    let settings = Settings::default();
    assert_eq!(settings.fallback.every_role, fallback);
    assert!(settings.fallback.roles.is_empty());
    // A session uses the largest window its CLI offers, and starts fresh only near its end.
    assert_eq!(settings.sessions.context, ContextSize::default());
    assert_eq!(settings.sessions.context.cap(), Ok(None));
    assert_eq!(settings.sessions.rotate_at, RotateAt::Share(0.9));
    assert!(!settings.approvals.rules.merge.allow_debt);
}

#[test]
fn the_fallback_walk_takes_each_list_and_key_from_the_most_specific_level() {
    let settings: Settings = toml::from_str(
        r#"
[fallback]
order = ["opencode/top-fallback"]
strategy = "first"
on_return = "keep"
max_fallbacks = 5
[fallback.replace.claude-code]
with = ["codex/top-replace"]
strategy = "spread"
on_return = "redo"

[fallback.reviewer]
order = ["ollama/role-fallback"]
pick = "judge"

[fallback.worker]
order = ["ollama/role-fallback"]
together = 2
on_return = "give_back"
[fallback.worker.replace.claude-code]
with = ["codex/role-replace"]
on_return = "review"
"#,
    )
    .unwrap();
    let top = &settings.fallback;
    let worker_role = top.for_role(RoleName::Worker);
    let reviewer = top.for_role(RoleName::Reviewer);
    let claude = AgentId("claude-code".into());
    let codex = AgentId("codex".into());
    let list = |text: &str| vec![FallbackEntry::Model(worker(text))];

    // Level 2: the role's own list for the agent, and its key over the role's table.
    let applied = FallbackTable::applied(top, worker_role, &claude);
    assert_eq!(applied.candidates, list("codex/role-replace"));
    assert_eq!(applied.on_return, OnReturn::Review);
    assert_eq!(applied.strategy, FallbackStrategy::Race);

    // Level 3: the role's fallback, with that table's `on_return`.
    let applied = FallbackTable::applied(top, worker_role, &codex);
    assert_eq!(applied.candidates, list("ollama/role-fallback"));
    assert_eq!(applied.on_return, OnReturn::GiveBack);
    assert_eq!(applied.strategy, FallbackStrategy::Race);

    // A role that sets only its fallback keeps the agent map's keys.
    let applied = FallbackTable::applied(top, reviewer, &claude);
    assert_eq!(applied.candidates, list("ollama/role-fallback"));
    assert_eq!(applied.strategy, FallbackStrategy::Spread);
    assert_eq!(applied.on_return, OnReturn::Redo);
    assert_eq!((applied.together, applied.pick), (1, Some(Pick::Judge)));
    assert_eq!(applied.max_fallbacks, 5);

    // Several at once is the engine's race, whatever the order's strategy says, and how their
    // results combine is the role's own way unless a table says.
    let together = top.applied_to(RoleName::Worker, &claude);
    assert_eq!(
        (together.together, together.strategy),
        (2, FallbackStrategy::Race)
    );
    assert_eq!(together.pick, Some(Pick::FirstPassing));
    // Each finer role follows one of the crew's five: the experiment and the drafting agent the
    // workers, the critic the reviewers.
    assert_eq!(top.applied_to(RoleName::Experiment, &claude), together);
    assert_eq!(
        top.applied_to(RoleName::Draft, &claude).pick,
        Some(Pick::Judge)
    );
    assert_eq!(
        top.applied_to(RoleName::Critic, &claude).pick,
        Some(Pick::Judge)
    );
    assert_eq!(top.for_role(RoleName::Judge), None);

    // Levels 4 and 5, for a role with no table.
    let applied = FallbackTable::applied(top, None, &claude);
    assert_eq!(applied.candidates, list("codex/top-replace"));
    assert_eq!(applied.strategy, FallbackStrategy::Spread);
    let applied = FallbackTable::applied(top, None, &codex);
    assert_eq!(applied.candidates, list("opencode/top-fallback"));
    assert_eq!(applied.strategy, FallbackStrategy::First);
    assert_eq!(applied.on_return, OnReturn::Keep);

    // Nothing set anywhere: the router offers the crew, and the built-in keys apply.
    let applied = FallbackTable::applied(&FallbackTable::default(), None, &codex);
    assert!(applied.candidates.is_empty());
    assert_eq!((applied.together, applied.pick), (1, None));
    assert!(applied.checkpoint);
    let unset = FallbackSettings::default();
    assert_eq!(
        unset.applied_to(RoleName::Reviewer, &codex).pick,
        Some(Pick::Merge),
        "reviews keep every finding"
    );
}

#[test]
fn the_fallback_context_walks_like_strategy_and_the_summarizer_like_checkpoint() {
    let settings: Settings = toml::from_str(
        r#"
[fallback]
context = "note"
summarizer = "ollama/llama3.2"
[fallback.replace.claude-code]
context = "note+summary"

[fallback.reviewer]
summarizer = "codex/cheap"

[fallback.worker]
context = "fresh"
[fallback.worker.replace.claude-code]
context = "note+transcript"
"#,
    )
    .unwrap();
    let top = &settings.fallback;
    let role = |name: RoleName| top.for_role(name);
    let (worker_role, reviewer) = (role(RoleName::Worker), role(RoleName::Reviewer));
    let claude = AgentId("claude-code".into());
    let codex = AgentId("codex".into());
    let applied = |role: Option<&FallbackTable>, agent_id: &AgentId| {
        FallbackTable::applied(top, role, agent_id)
    };

    assert_eq!(
        applied(worker_role, &claude).context,
        FallbackContext::NoteAndTranscript
    );
    assert_eq!(applied(worker_role, &codex).context, FallbackContext::Fresh);
    assert_eq!(
        applied(reviewer, &claude).context,
        FallbackContext::NoteAndSummary
    );
    assert_eq!(applied(reviewer, &codex).context, FallbackContext::Note);
    assert_eq!(applied(None, &codex).context, FallbackContext::Note);

    // The summarizer has no per-agent form: the role's own, then the top level's.
    assert_eq!(
        applied(reviewer, &claude).summarizer,
        Some(worker("codex/cheap"))
    );
    assert_eq!(
        applied(worker_role, &claude).summarizer,
        Some(worker("ollama/llama3.2"))
    );

    let unset = FallbackTable::applied(&FallbackTable::default(), None, &codex);
    assert_eq!(unset.context, FallbackContext::NoteAndTranscript);
    assert_eq!(unset.summarizer, None);
}

#[test]
fn fallback_contexts_keep_their_words_and_labels() {
    for (context, word, label) in [
        (FallbackContext::Fresh, "fresh", "fresh"),
        (FallbackContext::Note, "note", "note"),
        (
            FallbackContext::NoteAndTranscript,
            "note+transcript",
            "note + transcript",
        ),
        (
            FallbackContext::NoteAndSummary,
            "note+summary",
            "note + summary",
        ),
    ] {
        assert_eq!(round_trip::<FallbackContext>(json!(word)), context);
        assert_eq!(context.label(), label);
        let settings: Settings =
            toml::from_str(&format!("[fallback]\ncontext = \"{word}\"\n")).unwrap();
        assert_eq!(settings.fallback.context, Some(context));
        let text = toml::to_string(&settings).unwrap();
        assert!(text.contains(&format!("context = \"{word}\"")), "{text}");
    }
    assert_eq!(
        FallbackContext::default(),
        FallbackContext::NoteAndTranscript
    );
    assert!(serde_json::from_value::<FallbackContext>(json!("note_and_transcript")).is_err());
    assert!(toml::from_str::<Settings>("[fallback]\ncontext = \"transcript\"\n").is_err());
}

fn older_note() -> Value {
    json!({"task": serde_json::to_value(task_spec()).unwrap(), "goal_sha256": null,
        "notes": [], "work": {"branch": "argonauts/run-01/task-002", "commit": null,
        "stat": diff_stat()}, "happened": ["Read src/limits/store.ts"], "last_todo": [],
        "checks": [], "why_stopped": "usage limit", "last_words": null, "limit_left": null})
}

#[test]
fn notes_carry_the_transcript_and_summary_and_older_ones_still_read() {
    let note: FallbackNote = round_trip(older_note());
    assert_eq!((note.transcript, note.summary), (None, None));

    let mut told = older_note();
    told["transcript"] = json!({
        "worktree": "/work/task-002/.argonauts/fallback/1/transcript.md",
        "run": "runs/run-01/tasks/task-002/fallback-1/transcript.md"});
    told["summary"] = json!("Tried a Lua script for the bucket; one test is still flaky.");
    let note: FallbackNote = round_trip(told);
    assert_eq!(
        note.transcript,
        Some(TranscriptPaths {
            worktree: "/work/task-002/.argonauts/fallback/1/transcript.md".into(),
            run: "runs/run-01/tasks/task-002/fallback-1/transcript.md".into(),
        })
    );
    assert!(
        note.summary
            .is_some_and(|summary| summary.contains("flaky"))
    );
}

#[test]
fn rotate_at_takes_a_share_a_count_or_a_token_count() {
    let rotate_at = |text: &str| {
        toml::from_str::<Settings>(&format!("[sessions]\nrotate_at = {text}"))
            .unwrap()
            .sessions
            .rotate_at
    };
    let window = Some(200_000);
    assert_eq!(rotate_at("0.9"), RotateAt::Share(0.9));
    assert_eq!(rotate_at("0.9").tokens(window), Ok(Some(180_000)));
    assert_eq!(rotate_at("0.9").tokens(None), Ok(None));
    assert_eq!(rotate_at("\"200k\"").tokens(None), Ok(Some(200_000)));
    assert_eq!(rotate_at("150000"), RotateAt::Tokens(150_000));
    assert_eq!(rotate_at("150000").tokens(window), Ok(Some(150_000)));
    assert_eq!(rotate_at("\"1m\"").tokens(window), Ok(Some(1_000_000)));

    round_trip::<RotateAt>(json!(0.9));
    round_trip::<RotateAt>(json!("200k"));
    round_trip::<RotateAt>(json!(150000));

    for bad in [
        RotateAt::Text("abc".into()),
        RotateAt::Text("0k".into()),
        RotateAt::Tokens(0),
        RotateAt::Share(1.5),
    ] {
        assert!(bad.tokens(window).is_err(), "{bad:?}");
    }
}

#[test]
fn settings_with_the_fallback_keys_set_pass_the_schema() {
    let settings: Settings = toml::from_str(
        r#"
[fallback]
mode = "auto"
order = ["codex/gpt-x", { model = "openrouter/a/b", max_spend_usd = 2.0,
            instructions = "keep it small", read_only = true, allow_api_billing = true }]
ask_timeout = "10m"
allow_api_billing = true
broken_work = "restart"
strategy = "spread"
together = 3
pick = "judge"
on_return = "give_back"
checkpoint = false
merge_rule = "none"
unknown_tier = "equal"
max_fallbacks = 5
context = "note+summary"
summarizer = "ollama/llama3.2"
[fallback.replace.claude-code]
with = ["codex/gpt-x"]
strategy = "spread"
on_return = "redo"
context = "fresh"
[fallback.worker]
summarizer = "codex/gpt-x"
[fallback.worker.replace.codex]
with = ["claude-code/opus"]
on_return = "keep"
context = "note"
[sessions]
rotate_at = "1m"
[approvals.rules.merge]
allow_debt = true
"#,
    )
    .unwrap();
    assert_eq!(settings.fallback.strategy, Some(OrderStrategy::Spread));
    assert_eq!(settings.fallback.together, Some(3));
    assert_eq!(settings.fallback.pick, Some(Pick::Judge));
    assert_eq!(settings.fallback.on_return, Some(OnReturn::GiveBack));
    assert_eq!(
        settings.fallback.context,
        Some(FallbackContext::NoteAndSummary)
    );
    assert!(settings.approvals.rules.merge.allow_debt);
    let value = serde_json::to_value(&settings).unwrap();
    let schema = schemars::schema_for!(Settings).to_value();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(serde_json::from_value::<Settings>(value).unwrap(), settings);
}

#[test]
fn api_billing_on_fallback_is_a_safety_key_at_both_levels() {
    assert!(SAFETY_KEYS.contains(&"fallback.allow_api_billing"));
    assert!(SAFETY_KEYS.contains(&"fallback.*.allow_api_billing"));
}

#[test]
fn checkpoint_tags_name_their_tag_label_and_side_branch() {
    let tag: CheckpointTag = round_trip(json!({"run": "run-01", "task": "task-002", "n": 1}));
    assert_eq!(
        tag,
        CheckpointTag {
            run: RunId::new("run-01").unwrap(),
            task: TaskId::new("task-002").unwrap(),
            n: 1,
        }
    );
    assert_eq!(tag.tag_name(), "argonauts/run-01/task-002/fallback-1");
    assert_eq!(tag.label(), "fallback-1");
    assert_eq!(
        tag.replaced_branch(),
        "argonauts/run-01/task-002-replaced-1"
    );
    assert_eq!(CheckpointTag::parse_label("fallback-1"), Some(1));
    assert_eq!(CheckpointTag::parse_label("fallback-12"), Some(12));
    for bad in [
        "fallback-0",
        "fallback-",
        "fallback-x",
        "fallback-+1",
        "fallback--1",
        "1",
        "",
    ] {
        assert_eq!(CheckpointTag::parse_label(bad), None, "{bad:?}");
    }
    let debt: QualityDebt = round_trip(debt());
    assert_eq!(debt.needs, worker("claude-code/opus"));
}

#[test]
fn the_new_task_events_keep_their_wire_format() {
    let task = |data: Value| envelope(json!({"type": "task", "data": data}));
    for event in [
        task(json!({"event": "checkpoint_tagged", "n": 1, "commit": "c3",
            "from": "claude-code/opus", "to": ["codex/gpt-x"],
            "reason": "usage limit, resets at 3:45 PM"})),
        task(json!({"event": "checkpoint_tagged", "n": 2, "commit": "c7",
            "from": "codex/gpt-x", "to": ["claude-code/sonnet", "ollama/qwen3"],
            "reason": "outage"})),
        task(
            json!({"event": "quality_debt", "since": 1, "by": "codex/gpt-x",
            "needs": "claude-code/opus"}),
        ),
        task(json!({"event": "debt_cleared", "by": "claude-code/opus"})),
        task(json!({"event": "rewound", "to": 1,
            "kept_on": "argonauts/run-01/task-002-replaced-1"})),
    ] {
        round_trip::<RunEvent>(event);
    }
}

#[test]
fn commits_name_their_author_and_older_ones_still_read() {
    let task = |data: Value| envelope(json!({"type": "task", "data": data}));
    let by_fallback = task(
        json!({"event": "committed", "branch": "argonauts/run-01/task-002",
        "sha": "c0ffee", "message": "task-002: Redis store", "stat": diff_stat(),
        "by": "codex/gpt-x"}),
    );
    let committed = |event: RunEvent| match event.kind {
        RunEventKind::Task(TaskEvent::Committed(info)) => info,
        other => panic!("not a commit: {other:?}"),
    };
    let info = committed(round_trip::<RunEvent>(by_fallback));
    assert_eq!(info.by, Some(worker("codex/gpt-x")));

    let older = task(
        json!({"event": "committed", "branch": "argonauts/run-01/task-002",
        "sha": null, "message": "task-002: your take-over", "stat": diff_stat()}),
    );
    assert_eq!(committed(round_trip::<RunEvent>(older)).by, None);
}

#[test]
fn the_original_returned_task_and_its_answers_keep_their_wire_format() {
    let approval = |data: Value| envelope(json!({"type": "approval", "data": data}));
    let option = |key: &str, label: &str, action: &str| {
        let recommended = key == "r";
        json!({"key": key, "label": label, "action": {"action": action},
               "recommended": recommended, "detail": null})
    };
    round_trip::<RunEvent>(approval(json!({"event": "opened", "id": "approval-005",
        "kind": "original_returned", "title": "Claude Code is back · task-002",
        "task": "task-002",
        "packet": {"kind": "original_returned", "task": "task-002", "title": "Redis store",
                   "original": "claude-code/opus", "fallback": "codex/gpt-x", "since": 1,
                   "commits": 3, "changed": diff_stat(), "tests_passed": true,
                   "debt": [debt()]},
        "evidence_sha256": "e3b0", "mode": "supervised", "decide_with": "rules",
        "options": [option("r", "review it", "upgrade_review"),
                    option("o", "give it back", "give_back"),
                    option("u", "redo it", "redo"),
                    option("y", "keep the fallback", "keep_fallback"),
                    option("t", "take over", "take_over"),
                    option("d", "show the diff", "show_diff")],
        "free_text": "none", "needs_you": true})));
    for action in [
        "upgrade_review",
        "give_back",
        "redo",
        "keep_fallback",
        "wait",
    ] {
        round_trip::<RunEvent>(approval(json!({"event": "decided", "id": "approval-005",
            "action": {"action": action}, "text": null, "by": {"party": "user"},
            "evidence_sha256": "e3b0", "auto_preview": null, "tasks": []})));
    }
    round_trip::<ApprovalKind>(json!("original_returned"));
}

#[test]
fn fallback_plans_without_a_strategy_keep_their_older_json() {
    let plan: FallbackPlan = round_trip(fallback_plan());
    assert_eq!(plan.strategy, FallbackStrategy::First);
    assert_eq!(plan.race_count, 1);
    assert!(plan.also_moving.is_empty());

    let mut spread = fallback_plan();
    spread["strategy"] = json!("spread");
    spread["also_moving"] = json!(["task-003", "task-005"]);
    let plan: FallbackPlan = round_trip(spread);
    assert_eq!(plan.also_moving.len(), 2);

    let mut race = fallback_plan();
    race["strategy"] = json!("race");
    race["race_count"] = json!(3);
    assert_eq!(round_trip::<FallbackPlan>(race).race_count, 3);
}

#[test]
fn merge_rows_without_debt_keep_their_older_json() {
    let row: MergeRow = round_trip(merge_row());
    assert!(row.debt.is_empty());
    let mut indebted = merge_row();
    indebted["debt"] = json!([debt()]);
    let row: MergeRow = round_trip(indebted);
    assert_eq!(row.debt[0].since, 1);
}

struct FixedPlanner {
    candidates: Vec<Candidate>,
}

impl FallbackPlanner for FixedPlanner {
    fn candidates(&self, _query: &CandidateQuery) -> Vec<Candidate> {
        self.candidates.clone()
    }

    fn note(&self, _task: &TaskSpec, _events: &[RunEvent]) -> FallbackNote {
        unreachable!("only the shortlist is asked for")
    }
}

#[test]
fn the_default_shortlist_hands_each_task_to_the_first_candidate() {
    let planner = FixedPlanner {
        candidates: vec![serde_json::from_value(candidate()).unwrap()],
    };
    let query = CandidateQuery {
        task: task_spec(),
        role: RoleName::Worker,
        stopped: worker("claude-code/opus"),
        exclude_providers: Vec::new(),
        fallback: Vec::new(),
        all_valid: false,
    };
    let shortlist = planner.shortlist(&query);
    assert_eq!(
        shortlist,
        Shortlist {
            strategy: FallbackStrategy::First,
            race_count: 1,
            pick: Pick::FirstPassing,
            candidates: planner.candidates(&query),
        }
    );
    round_trip::<Shortlist>(json!({"strategy": "first", "race_count": 1,
        "pick": "first_passing", "candidates": [candidate()]}));
}

#[test]
fn remembering_a_pick_and_confirming_a_rewind_keep_their_wire_format() {
    let command: UiCommand =
        round_trip(json!({"type": "remember_pick", "run": "run-01", "id": "approval-005"}));
    assert_eq!(
        command,
        UiCommand::RememberPick {
            run: RunId::new("run-01").unwrap(),
            id: ApprovalId::parse("approval-005").unwrap(),
        }
    );
    round_trip::<AppEvent>(json!({"type": "choice", "choice": {"id": "ch-009",
        "kind": "confirm_rewind", "title": "Rewind task-002 to fallback-1?", "body": [],
        "options": [], "free_text": "none", "bell": false}}));
    round_trip::<ChoiceKind>(json!("confirm_rewind"));
}

fn parsed(text: &str) -> Value {
    serde_json::to_value(parse_input(text).unwrap_or_else(|err| panic!("{text}: {err:?}"))).unwrap()
}

#[test]
fn rewind_parses_to_its_wire_format() {
    let rewind = json!({"input": "slash", "command": {"command": "rewind", "task": "task-002"}});
    assert_eq!(parsed("/rewind task-002"), rewind);
    round_trip::<ParsedInput>(rewind);
    assert_eq!(
        parsed("/rewind"),
        json!({"input": "slash", "command": {"command": "rewind", "task": null}})
    );
    for bad in [
        "/rewind task-002 fallback-1",
        "/rewind fallback-1",
        "/rewind run-01",
    ] {
        assert!(parse_input(bad).is_err(), "{bad}");
    }
    let spec = SLASH_COMMANDS.iter().find(|spec| spec.name == "rewind");
    assert_eq!(spec.map(|spec| spec.handler), Some(Handler::App));

    // The checkpoint you pick goes back as a command of its own; a confirm question comes first.
    let pick =
        round_trip::<UiCommand>(json!({"type": "rewind", "task": "task-002", "checkpoint": 2}));
    assert!(matches!(pick, UiCommand::Rewind { checkpoint: 2, .. }));
}

#[tokio::test]
async fn a_rewind_answers_on_its_reply_channel() {
    let (reply, answer) = oneshot::channel();
    let command = EngineCommand::Rewind {
        task: TaskId::new("task-002").unwrap(),
        checkpoint: 1,
        reply,
    };
    let EngineCommand::Rewind {
        checkpoint, reply, ..
    } = command
    else {
        panic!("not a rewind");
    };
    assert_eq!(checkpoint, 1);
    reply.send(Err("task-002 is merged".into())).unwrap();
    assert_eq!(answer.await.unwrap(), Err("task-002 is merged".to_owned()));
}

#[test]
fn a_key_that_names_no_role_is_passed_over_and_a_roles_bad_value_is_refused() {
    let settings: Settings = toml::from_str(
        "[fallback]\nstrategi = \"spread\"\n[fallback.crew]\nmode = \"wait\"\n\
         [fallback.judge]\nmode = \"wait\"\n",
    )
    .unwrap();
    let roles: Vec<CrewRole> = settings.fallback.roles.keys().copied().collect();
    assert_eq!(roles, [CrewRole::Judge]);
    assert_eq!(
        settings.fallback.strategy, None,
        "the misspelt key set nothing"
    );
    assert!(toml::from_str::<Settings>("[fallback.judge]\nmode = \"sideways\"\n").is_err());
}
