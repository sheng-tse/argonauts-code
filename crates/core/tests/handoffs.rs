//! The handoff contracts: replacement lists per vendor, strategies, checkpoints, quality debt,
//! the original-returned card, and `/checkpoints` and `/revert`.

use argonauts_core::app::{AppEvent, ChoiceKind, UiCommand};
use argonauts_core::engine::EngineCommand;
use argonauts_core::events::card::{CardEvent, CardSpec};
use argonauts_core::events::checkpoint::{CheckpointKind, MergeRow};
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::handoff::{CheckpointTag, QualityDebt};
use argonauts_core::input::{Handler, ParsedInput, SLASH_COMMANDS, SlashCommand, parse_input};
use argonauts_core::limits::{Candidate, CandidateQuery, HandoffPacket, HandoffPlanner, Shortlist};
use argonauts_core::names::{CheckpointId, RoleName, VendorId, WorkerRef};
use argonauts_core::settings::handoff::{
    BrokenWork, FallbackEntry, FallbackRules, HandoffMode, HandoffSettings, HandoffStrategy,
    MergeRule, OnReturn, UnknownTier, VendorReplacement,
};
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::settings::{SAFETY_KEYS, Settings, Span};
use argonauts_core::{CardId, RunId};
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
        "card_id": "card-002",
        "source": "conductor",
        "kind": kind,
    })
}

fn diff_stat() -> Value {
    json!({"files_changed": 2, "insertions": 18, "deletions": 4})
}

fn worker(text: &str) -> WorkerRef {
    text.parse().unwrap()
}

fn debt() -> Value {
    json!({"since": 1, "by": "codex/gpt-x", "needs": "claude-code/opus"})
}

fn candidate() -> Value {
    json!({"worker": "codex/gpt-x", "quota_left": 0.62, "quota_text": "62%",
        "track_record": {"passed": 9, "of": 11}, "cost": {"unit": "quota", "amount": 0.1},
        "needs_confirm": false, "may_run_out": false})
}

fn handoff_plan() -> Value {
    json!({"card": "card-002", "title": "Redis store", "stopped": "claude-code/opus",
        "reason": "usage limit, resets in 2h 10m", "resets_at": "2026-09-29T18:00:00Z",
        "work": diff_stat(), "gates_ran": false, "candidates": [candidate()], "mode": "ask"})
}

fn merge_row() -> Value {
    json!({"card": "card-002", "title": "Redis store", "kind": {"kind": "build"},
        "stat": diff_stat(), "owner": "codex", "gates_passed": true, "review": "approve"})
}

fn card_spec() -> CardSpec {
    serde_json::from_value(json!({"id": "card-002", "title": "Redis store",
        "task": "Add the token bucket store", "kind": {"kind": "build"},
        "owned_paths": ["src/limits/store.ts"], "read_only_paths": [], "forbidden_paths": [],
        "hidden_paths": [], "depends_on": [], "criteria": ["G1"], "owner": "claude-code/opus",
        "reviewers": ["codex/gpt-x"],
        "budget": {"limit": {"unit": "usd", "amount": 3.5}, "max_minutes": 20.0,
                   "max_tool_calls": null, "inactivity_min": 5.0},
        "predictions": null}))
    .unwrap()
}

#[test]
fn the_handoff_example_parses_with_every_replacement_list() {
    let settings: Settings = toml::from_str(
        r#"
[handoff]
mode = "ask"
strategy = "first"
on_return = "ask"
checkpoint = true
merge_rule = "upgrade_review"
unknown_tier = "weaker"
max_handoffs = 3

[handoff.replace.claude-code]
with = ["codex/<model>", "antigravity-cli/<model>"]
strategy = "spread"

[handoff.replace.codex]
with = ["claude-code/sonnet"]

[handoff.replace.antigravity-cli]
with = ["claude-code/sonnet", { worker = "opencode/<model>", max_budget_usd = 2 }]

[roles.reviewer.handoff]
fallback = ["ollama/<model>"]

[roles.owner.handoff.replace.claude-code]
with = ["codex/<model>"]
on_return = "review"
"#,
    )
    .unwrap();
    let replace = &settings.handoff.replace;
    assert_eq!(replace.len(), 3);
    let vendor = |id: &str| VendorId(id.to_owned());
    assert_eq!(
        replace[&vendor("claude-code")],
        VendorReplacement {
            with: vec![
                FallbackEntry::Worker(worker("codex/<model>")),
                FallbackEntry::Worker(worker("antigravity-cli/<model>")),
            ],
            strategy: Some(HandoffStrategy::Spread),
            on_return: None,
        }
    );
    assert_eq!(
        replace[&vendor("antigravity-cli")].with[1],
        FallbackEntry::WithRules(FallbackRules {
            worker: worker("opencode/<model>"),
            max_budget_usd: Some(2.0),
            instructions: None,
            read_only: false,
            allow_api_billing: false,
        })
    );
    // Everything the example sets is also the default.
    assert_eq!(
        HandoffSettings {
            replace: Default::default(),
            race_count: Some(2),
            ..settings.handoff.clone()
        },
        HandoffSettings::built_in()
    );

    let reviewer = &settings.roles[&RoleName::Reviewer].handoff;
    assert_eq!(
        reviewer.fallback,
        [FallbackEntry::Worker(worker("ollama/<model>"))]
    );
    let owner = &settings.roles[&RoleName::Owner].handoff.replace[&vendor("claude-code")];
    assert_eq!(owner.with, [FallbackEntry::Worker(worker("codex/<model>"))]);
    assert_eq!(owner.on_return, Some(OnReturn::Review));
    assert_eq!(owner.strategy, None);

    let json = serde_json::to_value(&settings).unwrap();
    assert_eq!(serde_json::from_value::<Settings>(json).unwrap(), settings);
}

#[test]
fn the_new_handoff_defaults_hold() {
    let handoff = HandoffSettings::built_in();
    assert_eq!(handoff.mode, HandoffMode::Ask);
    assert!(handoff.fallback.is_empty() && handoff.replace.is_empty());
    assert_eq!(handoff.ask_timeout, Span::new("none"));
    assert!(!handoff.allow_api_billing && !handoff.return_to_original);
    assert_eq!(handoff.broken_work, BrokenWork::Continue);
    assert_eq!(handoff.strategy, Some(HandoffStrategy::First));
    assert_eq!(handoff.race_count, Some(2));
    assert_eq!(handoff.on_return, Some(OnReturn::Ask));
    assert_eq!(handoff.checkpoint, Some(true));
    assert_eq!(handoff.merge_rule, Some(MergeRule::UpgradeReview));
    assert_eq!(handoff.unknown_tier, Some(UnknownTier::Weaker));
    assert_eq!(handoff.max_handoffs, Some(3));
    assert_eq!(VendorReplacement::default().strategy, None);

    // A role table that sets nothing leaves every optional key unset.
    let unset = HandoffSettings::default();
    assert_eq!(
        (
            unset.strategy,
            unset.race_count,
            unset.on_return,
            unset.checkpoint
        ),
        (None, None, None, None)
    );
    assert_eq!(
        (unset.merge_rule, unset.unknown_tier, unset.max_handoffs),
        (None, None, None)
    );
    let applied = HandoffSettings::applied(&handoff, None, &VendorId("codex".into()));
    assert_eq!(
        HandoffSettings::applied(&unset, Some(&unset), &VendorId("codex".into())),
        applied
    );
    assert_eq!(applied.strategy, HandoffStrategy::First);
    assert_eq!(applied.max_handoffs, 3);

    let settings = Settings::default();
    assert_eq!(settings.handoff, handoff);
    assert_eq!(
        settings.sessions.rotate_at,
        RotateAt::Text("200k".to_owned())
    );
    assert!(!settings.approvals.merge.rules.allow_debt);
}

#[test]
fn the_handoff_walk_takes_each_list_and_key_from_the_most_specific_level() {
    let settings: Settings = toml::from_str(
        r#"
[handoff]
fallback = ["opencode/top-fallback"]
strategy = "first"
on_return = "keep"
max_handoffs = 5
[handoff.replace.claude-code]
with = ["codex/top-replace"]
strategy = "spread"
on_return = "redo"

[roles.reviewer.handoff]
fallback = ["ollama/role-fallback"]
race_count = 4

[roles.owner.handoff]
fallback = ["ollama/role-fallback"]
strategy = "race"
return_to_original = true
[roles.owner.handoff.replace.claude-code]
with = ["codex/role-replace"]
on_return = "review"
"#,
    )
    .unwrap();
    let top = &settings.handoff;
    let owner = settings
        .roles
        .get(&RoleName::Owner)
        .map(|role| &role.handoff);
    let reviewer = settings
        .roles
        .get(&RoleName::Reviewer)
        .map(|role| &role.handoff);
    let claude = VendorId("claude-code".into());
    let codex = VendorId("codex".into());
    let list = |text: &str| vec![FallbackEntry::Worker(worker(text))];

    // Level 2: the role's own list for the vendor, and its key over the role's table.
    let applied = HandoffSettings::applied(top, owner, &claude);
    assert_eq!(applied.candidates, list("codex/role-replace"));
    assert_eq!(applied.on_return, OnReturn::Review);
    assert_eq!(applied.strategy, HandoffStrategy::Race);

    // Level 3: the role's fallback, and `return_to_original` as that table's `on_return`.
    let applied = HandoffSettings::applied(top, owner, &codex);
    assert_eq!(applied.candidates, list("ollama/role-fallback"));
    assert_eq!(applied.on_return, OnReturn::HandBack);
    assert_eq!(applied.strategy, HandoffStrategy::Race);

    // A role that sets only its fallback keeps the vendor map's keys.
    let applied = HandoffSettings::applied(top, reviewer, &claude);
    assert_eq!(applied.candidates, list("ollama/role-fallback"));
    assert_eq!(applied.strategy, HandoffStrategy::Spread);
    assert_eq!(applied.on_return, OnReturn::Redo);
    assert_eq!(applied.race_count, 4);
    assert_eq!(applied.max_handoffs, 5);

    // Levels 4 and 5, for a role with no table.
    let applied = HandoffSettings::applied(top, None, &claude);
    assert_eq!(applied.candidates, list("codex/top-replace"));
    assert_eq!(applied.strategy, HandoffStrategy::Spread);
    let applied = HandoffSettings::applied(top, None, &codex);
    assert_eq!(applied.candidates, list("opencode/top-fallback"));
    assert_eq!(applied.strategy, HandoffStrategy::First);
    assert_eq!(applied.on_return, OnReturn::Keep);

    // Nothing set anywhere: the router offers the crew, and the built-in keys apply.
    let applied = HandoffSettings::applied(&HandoffSettings::default(), None, &codex);
    assert!(applied.candidates.is_empty());
    assert_eq!(applied.race_count, 2);
    assert!(applied.checkpoint);
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
fn settings_with_the_handoff_keys_set_pass_the_schema() {
    let settings: Settings = toml::from_str(
        r#"
[handoff]
mode = "auto"
fallback = ["codex/gpt-x", { worker = "openrouter/a/b", max_budget_usd = 2.0,
            instructions = "keep it small", read_only = true, allow_api_billing = true }]
ask_timeout = "10m"
allow_api_billing = true
return_to_original = true
broken_work = "restart"
strategy = "race"
race_count = 3
on_return = "hand_back"
checkpoint = false
merge_rule = "none"
unknown_tier = "equal"
max_handoffs = 5
[handoff.replace.claude-code]
with = ["codex/gpt-x"]
strategy = "spread"
on_return = "redo"
[roles.owner.handoff.replace.codex]
with = ["claude-code/opus"]
on_return = "keep"
[sessions]
rotate_at = "1m"
[approvals.merge.rules]
allow_debt = true
"#,
    )
    .unwrap();
    assert_eq!(settings.handoff.strategy, Some(HandoffStrategy::Race));
    assert_eq!(settings.handoff.on_return, Some(OnReturn::HandBack));
    assert!(settings.approvals.merge.rules.allow_debt);
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
fn api_billing_on_handoff_is_a_safety_key_at_both_levels() {
    assert!(SAFETY_KEYS.contains(&"handoff.allow_api_billing"));
    assert!(SAFETY_KEYS.contains(&"roles.*.handoff.allow_api_billing"));
}

#[test]
fn checkpoint_tags_name_their_tag_label_and_side_branch() {
    let tag: CheckpointTag = round_trip(json!({"run": "run-01", "card": "card-002", "n": 1}));
    assert_eq!(
        tag,
        CheckpointTag {
            run: RunId::new("run-01").unwrap(),
            card: CardId::new("card-002").unwrap(),
            n: 1,
        }
    );
    assert_eq!(tag.tag_name(), "argonauts/run-01/card-002/handoff-1");
    assert_eq!(tag.label(), "handoff-1");
    assert_eq!(
        tag.replaced_branch(),
        "argonauts/run-01/card-002-replaced-1"
    );
    assert_eq!(CheckpointTag::parse_label("handoff-1"), Some(1));
    assert_eq!(CheckpointTag::parse_label("handoff-12"), Some(12));
    for bad in [
        "handoff-0",
        "handoff-",
        "handoff-x",
        "handoff-+1",
        "handoff--1",
        "1",
        "",
    ] {
        assert_eq!(CheckpointTag::parse_label(bad), None, "{bad:?}");
    }
    let debt: QualityDebt = round_trip(debt());
    assert_eq!(debt.needs, worker("claude-code/opus"));
}

#[test]
fn the_new_card_events_keep_their_wire_format() {
    let card = |data: Value| envelope(json!({"type": "card", "data": data}));
    for event in [
        card(json!({"event": "checkpoint_tagged", "n": 1, "commit": "c3",
            "from": "claude-code/opus", "to": ["codex/gpt-x"],
            "reason": "usage limit, resets at 3:45 PM"})),
        card(json!({"event": "checkpoint_tagged", "n": 2, "commit": "c7",
            "from": "codex/gpt-x", "to": ["claude-code/sonnet", "ollama/qwen3"],
            "reason": "outage"})),
        card(
            json!({"event": "quality_debt", "since": 1, "by": "codex/gpt-x",
            "needs": "claude-code/opus"}),
        ),
        card(json!({"event": "debt_cleared", "by": "claude-code/opus"})),
        card(json!({"event": "reverted", "to": 1,
            "kept_on": "argonauts/run-01/card-002-replaced-1"})),
    ] {
        round_trip::<RunEvent>(event);
    }
}

#[test]
fn commits_name_their_author_and_older_ones_still_read() {
    let card = |data: Value| envelope(json!({"type": "card", "data": data}));
    let by_replacement = card(
        json!({"event": "committed", "branch": "argonauts/run-01/card-002",
        "sha": "c0ffee", "message": "card-002: Redis store", "stat": diff_stat(),
        "by": "codex/gpt-x"}),
    );
    let committed = |event: RunEvent| match event.kind {
        RunEventKind::Card(CardEvent::Committed(info)) => info,
        other => panic!("not a commit: {other:?}"),
    };
    let info = committed(round_trip::<RunEvent>(by_replacement));
    assert_eq!(info.by, Some(worker("codex/gpt-x")));

    let older = card(
        json!({"event": "committed", "branch": "argonauts/run-01/card-002",
        "sha": null, "message": "card-002: your take-over", "stat": diff_stat()}),
    );
    assert_eq!(committed(round_trip::<RunEvent>(older)).by, None);
}

#[test]
fn the_original_returned_card_and_its_answers_keep_their_wire_format() {
    let checkpoint = |data: Value| envelope(json!({"type": "checkpoint", "data": data}));
    let option = |key: &str, label: &str, action: &str| {
        let recommended = key == "r";
        json!({"key": key, "label": label, "action": {"action": action},
               "recommended": recommended, "detail": null})
    };
    round_trip::<RunEvent>(checkpoint(json!({"event": "opened", "id": "cp-005",
        "kind": "original_returned", "title": "Claude Code is back · card-002",
        "card": "card-002",
        "packet": {"kind": "original_returned", "card": "card-002", "title": "Redis store",
                   "original": "claude-code/opus", "replacement": "codex/gpt-x", "since": 1,
                   "commits": 3, "changed": diff_stat(), "tests_passed": true,
                   "debt": [debt()]},
        "evidence_sha256": "e3b0", "mode": "supervised", "decide_with": "rules",
        "options": [option("r", "review it", "upgrade_review"),
                    option("o", "hand it back", "hand_back"),
                    option("u", "redo it", "redo"),
                    option("y", "keep the replacement", "keep_replacement"),
                    option("t", "take over", "take_over"),
                    option("d", "show the diff", "show_diff")],
        "free_text": "none", "needs_you": true})));
    for action in [
        "upgrade_review",
        "hand_back",
        "redo",
        "keep_replacement",
        "wait",
    ] {
        round_trip::<RunEvent>(checkpoint(json!({"event": "decided", "id": "cp-005",
            "action": {"action": action}, "text": null, "by": {"party": "user"},
            "evidence_sha256": "e3b0", "shadow": null, "cards": []})));
    }
    round_trip::<CheckpointKind>(json!("original_returned"));
}

#[test]
fn handoff_plans_without_a_strategy_keep_their_older_json() {
    let plan: HandoffPlan = round_trip(handoff_plan());
    assert_eq!(plan.strategy, HandoffStrategy::First);
    assert_eq!(plan.race_count, 1);
    assert!(plan.also_moving.is_empty());

    let mut spread = handoff_plan();
    spread["strategy"] = json!("spread");
    spread["also_moving"] = json!(["card-003", "card-005"]);
    let plan: HandoffPlan = round_trip(spread);
    assert_eq!(plan.also_moving.len(), 2);

    let mut race = handoff_plan();
    race["strategy"] = json!("race");
    race["race_count"] = json!(3);
    assert_eq!(round_trip::<HandoffPlan>(race).race_count, 3);
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

impl HandoffPlanner for FixedPlanner {
    fn candidates(&self, _query: &CandidateQuery) -> Vec<Candidate> {
        self.candidates.clone()
    }

    fn packet(&self, _card: &CardSpec, _events: &[RunEvent]) -> HandoffPacket {
        unreachable!("only the shortlist is asked for")
    }
}

#[test]
fn the_default_shortlist_hands_each_card_to_the_first_candidate() {
    let planner = FixedPlanner {
        candidates: vec![serde_json::from_value(candidate()).unwrap()],
    };
    let query = CandidateQuery {
        card: card_spec(),
        role: RoleName::Owner,
        stopped: worker("claude-code/opus"),
        exclude_families: Vec::new(),
        fallback: Vec::new(),
        all_valid: false,
    };
    let shortlist = planner.shortlist(&query);
    assert_eq!(
        shortlist,
        Shortlist {
            strategy: HandoffStrategy::First,
            race_count: 1,
            candidates: planner.candidates(&query),
        }
    );
    round_trip::<Shortlist>(json!({"strategy": "first", "race_count": 1,
        "candidates": [candidate()]}));
}

#[test]
fn remembering_a_pick_and_confirming_a_revert_keep_their_wire_format() {
    let command: UiCommand =
        round_trip(json!({"type": "remember_pick", "run": "run-01", "id": "cp-005"}));
    assert_eq!(
        command,
        UiCommand::RememberPick {
            run: RunId::new("run-01").unwrap(),
            id: CheckpointId::parse("cp-005").unwrap(),
        }
    );
    round_trip::<AppEvent>(json!({"type": "choice", "choice": {"id": "ch-009",
        "kind": "confirm_revert", "title": "Revert card-002 to handoff-1?", "body": [],
        "options": [], "free_text": "none", "bell": false}}));
    round_trip::<ChoiceKind>(json!("confirm_revert"));
}

fn parsed(text: &str) -> Value {
    serde_json::to_value(parse_input(text).unwrap_or_else(|err| panic!("{text}: {err:?}"))).unwrap()
}

#[test]
fn checkpoints_and_revert_parse_to_their_wire_format() {
    let checkpoints = json!({"input": "slash",
        "command": {"command": "checkpoints", "card": "card-002"}});
    assert_eq!(parsed("/checkpoints card-002"), checkpoints);
    round_trip::<ParsedInput>(checkpoints);
    assert_eq!(
        parsed("/checkpoints"),
        json!({"input": "slash", "command": {"command": "checkpoints", "card": null}})
    );

    let revert = json!({"input": "slash",
        "command": {"command": "revert", "card": "card-002", "checkpoint": 1}});
    assert_eq!(parsed("/revert card-002 handoff-1"), revert);
    assert_eq!(parsed("/revert card-002 1"), revert);
    round_trip::<ParsedInput>(revert);
    for bad in [
        "/revert card-002 handoff-0",
        "/revert card-002",
        "/revert handoff-1",
    ] {
        assert!(parse_input(bad).is_err(), "{bad}");
    }

    for name in ["checkpoints", "revert"] {
        let spec = SLASH_COMMANDS.iter().find(|spec| spec.name == name);
        assert_eq!(spec.map(|spec| spec.handler), Some(Handler::App), "{name}");
    }
    assert!(matches!(
        parse_input("/revert card-002 handoff-2"),
        Ok(ParsedInput::Slash {
            command: SlashCommand::Revert { checkpoint: 2, .. }
        })
    ));
}

#[tokio::test]
async fn a_revert_answers_on_its_reply_channel() {
    let (reply, answer) = oneshot::channel();
    let command = EngineCommand::Revert {
        card: CardId::new("card-002").unwrap(),
        checkpoint: 1,
        reply,
    };
    let EngineCommand::Revert {
        checkpoint, reply, ..
    } = command
    else {
        panic!("not a revert");
    };
    assert_eq!(checkpoint, 1);
    reply.send(Err("card-002 is merged".into())).unwrap();
    assert_eq!(answer.await.unwrap(), Err("card-002 is merged".to_owned()));
}
