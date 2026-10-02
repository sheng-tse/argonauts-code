use argonauts_core::api::DiffScope;
use argonauts_core::app::{AnswerTarget, AppChoice, AppEvent, ChoiceKind, UiCommand, ViewRequest};
use argonauts_core::events::checkpoint::{AnswerAction, AnswerOption, FreeText};
use argonauts_core::names::{CheckpointId, ChoiceId};
use argonauts_core::{CardId, RunId};
use serde_json::{Value, json};

use super::{CardList, Pick, Reader, Typed};
use crate::UiOptions;

const RUN: &str = "run-01";
const EVIDENCE: &str = "3f1a";

fn run_event(seq: u64, data: Value) -> AppEvent {
    serde_json::from_value(json!({"type": "run", "event": {"seq": seq,
        "ts": "2026-09-30T12:00:00Z", "run_id": RUN, "card_id": null, "session": null,
        "source": "conductor", "kind": {"type": "checkpoint", "data": data}}}))
    .expect("a checkpoint event")
}

fn option(key: char, label: &str, action: Value) -> Value {
    json!({"key": key.to_string(), "label": label, "action": action, "recommended": false,
           "detail": null})
}

fn opened(id: &str, kind: &str, packet: Value, options: Vec<Value>, free_text: &str) -> Value {
    json!({"event": "opened", "id": id, "kind": kind, "title": format!("{kind} · {RUN}"),
        "card": null, "packet": packet, "evidence_sha256": EVIDENCE, "mode": "supervised",
        "decide_with": "rules", "options": options, "free_text": free_text, "needs_you": true})
}

fn merge_row(card: &str, title: &str, owner: Option<&str>) -> Value {
    json!({"card": card, "title": title, "kind": {"kind": "build"},
        "stat": {"files_changed": 2, "insertions": 30, "deletions": 4}, "owner": owner,
        "gates_passed": true, "review": "approve"})
}

fn merge(id: &str, rows: Vec<Value>) -> Value {
    let packet = json!({"kind": "merge", "target": "main",
        "branch": format!("argonauts/{RUN}/integration"), "rows": rows,
        "suite_passed_on_latest_main": true, "gates": [],
        "combined": {"files_changed": 4, "insertions": 60, "deletions": 8},
        "diff_path": "/nonexistent/argonauts/combined.diff", "tree_sha": "t1",
        "spent": {"input_tokens": 1200, "output_tokens": 900, "cached_tokens": 0,
                  "cost_usd": 0.46, "cost_partial": false, "cost_estimated": false},
        "limit": null, "partial": false});
    let options = vec![
        option('y', "Merge", json!({"action": "merge"})),
        option(
            'p',
            "Open a pull request",
            json!({"action": "pull_request"}),
        ),
        option(
            'b',
            "Request changes on some cards",
            json!({"action": "request_changes"}),
        ),
        option('k', "Keep the branch", json!({"action": "keep_branch"})),
        option('x', "Abandon the run", json!({"action": "abandon"})),
        option('d', "Show the diff", json!({"action": "show_diff"})),
    ];
    opened(id, "merge", packet, options, "guidance")
}

fn two_card_merge() -> Value {
    merge(
        "cp-001",
        vec![
            merge_row("card-002", "Add the JSON writer", Some("claude-code/opus")),
            merge_row("card-003", "Wire the --json flag", Some("codex/gpt-5.6")),
        ],
    )
}

fn plan() -> Value {
    let packet = json!({"kind": "plan", "summary": "A JSON writer beside the CSV one",
        "judging": null, "near_tie": false, "decisions": [],
        "graph": {"cards": [], "critical_path": []},
        "acceptance_tests": ["--json prints valid JSON"],
        "budget": {"lines": [
            {"line": "cards", "amount": {"unit": "usd", "amount": 6.0}},
            {"line": "review", "amount": {"unit": "usd", "amount": 2.0}}],
            "total": {"unit": "usd", "amount": 10.0},
            "estimate_low": 7.0, "estimate_high": 9.5},
        "new_dependencies": [], "flags": []});
    let options = vec![
        option('y', "Approve the plan", json!({"action": "approve"})),
        option(
            'e',
            "Approve with edits",
            json!({"action": "approve_with_edits"}),
        ),
        option('x', "Abandon", json!({"action": "abandon"})),
        option('d', "Show the plan", json!({"action": "show_diff"})),
    ];
    opened("cp-002", "plan", packet, options, "edits")
}

fn handoff() -> Value {
    let packet = json!({"kind": "handoff", "card": "card-003", "title": "Middleware",
        "stopped": "claude-code/opus", "reason": "usage limit", "resets_at": null,
        "work": {"files_changed": 4, "insertions": 96, "deletions": 12}, "gates_ran": false,
        "candidates": [{"worker": "codex/gpt-5.6", "quota_left": 0.62, "quota_text": "62%",
            "track_record": null, "cost": {"unit": "free"}, "needs_confirm": false,
            "may_run_out": false}],
        "mode": "ask"});
    let options = vec![
        option(
            'a',
            "codex/gpt-5.6",
            json!({"action": "candidate", "worker": "codex/gpt-5.6"}),
        ),
        option('w', "Wait until the reset", json!({"action": "wait"})),
        option('d', "Show the diff", json!({"action": "show_diff"})),
    ];
    opened("cp-003", "handoff", packet, options, "none")
}

fn waiting_on(card: Value) -> Reader {
    let mut reader = Reader::new(&UiOptions::default());
    reader.event(&run_event(1, card));
    reader
}

fn checkpoint(id: &str) -> AnswerTarget {
    AnswerTarget::Checkpoint {
        run: RunId::new(RUN).expect("a run id"),
        id: CheckpointId::parse(id).expect("a checkpoint id"),
    }
}

fn answer(id: &str, action: AnswerAction, text: Option<&str>, cards: &[&str]) -> UiCommand {
    UiCommand::Answer {
        to: checkpoint(id),
        action,
        text: text.map(str::to_owned),
        evidence_sha256: Some(EVIDENCE.to_owned()),
        cards: cards
            .iter()
            .map(|card| CardId::new(*card).expect("a card id"))
            .collect(),
        remember: false,
    }
}

fn submit(text: &str) -> Vec<UiCommand> {
    vec![UiCommand::Submit { text: text.into() }]
}

#[test]
fn words_after_a_number_and_letters_are_a_message() {
    let mut reader = waiting_on(two_card_merge());
    for text in [
        "1 more thing, the tests look flaky",
        "y is this failing?",
        "x marks the spot",
        "1 more thing: the tests look flaky",
        "y",
        "d",
    ] {
        let said = reader.typed(text);
        assert_eq!(said.commands, submit(text), "{text}");
    }
}

#[test]
fn merge_and_abandon_ask_again() {
    let mut reader = waiting_on(two_card_merge());
    let asked = reader.typed("1");
    assert!(asked.commands.is_empty());
    assert_eq!(asked.lines, vec!["merge: type 1 again to merge."]);
    // Chat in between cancels, so a later `1` asks again.
    let chat = reader.typed("is the flag tested?");
    assert_eq!(chat.lines, vec!["merge: cancelled, nothing was sent."]);
    assert_eq!(chat.commands, submit("is the flag tested?"));
    assert!(reader.typed("1: ship it").commands.is_empty());
    let merged = reader.typed("1");
    assert_eq!(
        merged.commands,
        vec![answer("cp-001", AnswerAction::Merge, Some("ship it"), &[])]
    );
    assert_eq!(merged.lines, vec!["you: 1. Merge, ship it"]);

    let mut reader = waiting_on(two_card_merge());
    assert!(reader.typed("5").commands.is_empty());
    let other = reader.typed("2");
    assert_eq!(
        other.lines,
        vec![
            "abandon the run: cancelled, nothing was sent.",
            "you: 2. Open a pull request"
        ]
    );
    assert_eq!(
        other.commands,
        vec![answer("cp-001", AnswerAction::PullRequest, None, &[])]
    );
}

#[test]
fn show_the_diff_answers_nothing() {
    let mut reader = waiting_on(two_card_merge());
    let shown = reader.typed("6");
    assert!(shown.commands.is_empty(), "{:?}", shown.commands);
    assert_eq!(shown.lines[0], "you: 6. Show the diff");
    assert!(
        shown.lines[1].starts_with("warning: Can't open the diff at "),
        "{:?}",
        shown.lines
    );
    // The card still takes numbers.
    assert_eq!(
        reader.typed("2").commands,
        vec![answer("cp-001", AnswerAction::PullRequest, None, &[])]
    );

    // One card's merge asks the app for that card's diff.
    let one = merge(
        "cp-001",
        vec![merge_row("card-002", "Add the JSON writer", None)],
    );
    let mut reader = waiting_on(one);
    assert_eq!(
        reader.typed("6").commands,
        vec![UiCommand::Request {
            view: ViewRequest::Diff {
                run: RunId::new(RUN).expect("a run id"),
                card: CardId::new("card-002").expect("a card id"),
                scope: DiffScope::Card,
            }
        }]
    );
    assert!(matches!(
        reader.typed("2").commands.as_slice(),
        [UiCommand::Answer { .. }]
    ));
}

#[test]
fn show_the_plan_reads_the_details() {
    let mut reader = waiting_on(plan());
    let shown = reader.typed("4");
    assert!(shown.commands.is_empty());
    insta::assert_snapshot!("show_the_plan", shown.lines.join("\n"));
}

#[test]
fn request_changes_names_cards() {
    let mut reader = waiting_on(two_card_merge());
    let unknown = reader.typed("3 card-009: tighten it");
    assert!(unknown.commands.is_empty());
    assert_eq!(
        unknown.lines,
        vec![
            "warning: card-009 isn't one of this merge's cards (card-002, card-003), so nothing was sent."
        ]
    );
    let named = reader.typed("3 card-003: keep the old flag");
    assert_eq!(
        named.commands,
        vec![answer(
            "cp-001",
            AnswerAction::RequestChanges,
            Some("keep the old flag"),
            &["card-003"]
        )]
    );
    assert_eq!(
        named.lines,
        vec!["you: 3. Request changes on some cards, card-003, keep the old flag"]
    );

    let mut every = waiting_on(two_card_merge());
    let said = every.typed("3");
    assert_eq!(
        said.commands,
        vec![answer("cp-001", AnswerAction::RequestChanges, None, &[])]
    );
    assert_eq!(
        said.lines,
        vec!["you: 3. Request changes on some cards, every card"]
    );
}

#[test]
fn a_spot_check_takes_its_numbers() {
    let mut reader = Reader::new(&UiOptions::default());
    let mut quiet = two_card_merge();
    quiet["needs_you"] = json!(false);
    reader.event(&run_event(1, quiet));
    reader.event(&run_event(
        2,
        json!({"event": "decided", "id": "cp-001", "action": {"action": "merge"},
            "text": null, "by": {"party": "rules"}, "evidence_sha256": EVIDENCE,
            "shadow": null, "cards": []}),
    ));
    let queued = reader.event(&run_event(
        3,
        json!({"event": "spot_check_queued", "id": "cp-001"}),
    ));
    insta::assert_snapshot!("spot_check", queued.lines.join("\n"));

    let evidence = reader.typed("3");
    assert!(evidence.commands.is_empty());
    assert!(
        evidence
            .lines
            .iter()
            .any(|line| line.starts_with("details: merge")),
        "{:?}",
        evidence.lines
    );
    let checked = reader.typed("2");
    assert_eq!(
        checked.commands,
        vec![UiCommand::SpotCheck {
            run: RunId::new(RUN).expect("a run id"),
            id: CheckpointId::parse("cp-001").expect("an id"),
            correct: false,
        }]
    );
    // Answered, the spot check no longer takes numbers.
    assert_eq!(reader.typed("1").commands, submit("1"));
}

#[test]
fn remember_this_pick_goes_with_a_candidate() {
    let mut reader = Reader::new(&UiOptions::default());
    let said = reader.event(&run_event(1, handoff()));
    assert!(
        said.lines
            .contains(&"  4. Remember this pick, off".to_owned()),
        "{:?}",
        said.lines
    );
    let on = reader.typed("4");
    assert!(on.commands.is_empty());
    assert_eq!(on.lines, vec!["remember this pick: on"]);
    let picked = reader.typed("1");
    let [
        UiCommand::Answer {
            action, remember, ..
        },
    ] = picked.commands.as_slice()
    else {
        panic!("{:?}", picked.commands);
    };
    assert!(matches!(action, AnswerAction::Candidate { .. }));
    assert!(*remember);
}

#[test]
fn a_plan_card_reads_its_budget_by_header() {
    let AppEvent::Run { event } = run_event(1, plan()) else {
        unreachable!()
    };
    let argonauts_core::events::RunEventKind::Checkpoint(
        argonauts_core::events::checkpoint::CheckpointEvent::Opened(opened),
    ) = &event.kind
    else {
        unreachable!()
    };
    let names = Default::default();
    let card = CardList::checkpoint(&event.run_id, opened, None, time::UtcOffset::UTC, &names);
    insta::assert_snapshot!("plan_card", card.lines().join("\n"));
}

#[test]
fn a_merge_row_without_an_owner_keeps_its_columns() {
    let mut reader = Reader::new(&UiOptions::default());
    let said = reader.event(&run_event(
        1,
        merge(
            "cp-001",
            vec![
                merge_row("card-002", "Add the JSON writer", None),
                merge_row("card-003", "Wire the --json flag", Some("codex/gpt-5.6")),
            ],
        ),
    ));
    insta::assert_snapshot!("merge_row_without_owner", said.lines.join("\n"));
}

fn choice(free_text: FreeText) -> CardList {
    let option = |key: char, label: &str, action: AnswerAction| AnswerOption {
        key,
        label: label.to_owned(),
        action,
        recommended: false,
        detail: None,
    };
    CardList::choice(&AppChoice {
        id: ChoiceId::numbered(1),
        kind: ChoiceKind::StartWork,
        title: "Start work?".to_owned(),
        body: vec!["It changes 3 files.".to_owned()],
        options: vec![
            option('s', "Solo", AnswerAction::Approve),
            option('c', "Crew run", AnswerAction::SendBack),
        ],
        free_text,
        bell: true,
    })
}

#[test]
fn numbers_pick_options_and_letters_never_do() {
    let card = choice(FreeText::None);
    let solo = Typed::Pick(Pick::Option {
        index: 0,
        note: None,
        cards: Vec::new(),
    });
    assert_eq!(card.parse("1"), solo);
    assert_eq!(card.parse(" 1. "), solo);
    assert!(matches!(
        card.parse("2"),
        Typed::Pick(Pick::Option { index: 1, .. })
    ));
    for message in ["3", "0", "s", "C", "sure", "2 more tests please", ": 1"] {
        assert_eq!(card.parse(message), Typed::Message, "{message}");
    }
    assert!(matches!(
        card.parse("2: more tests please"),
        Typed::Warning(_)
    ));
}

#[test]
fn notes_and_own_answers_follow_a_colon() {
    let guided = choice(FreeText::Guidance);
    assert_eq!(
        guided.parse("2: keep the old flag"),
        Typed::Pick(Pick::Option {
            index: 1,
            note: Some("keep the old flag".to_owned()),
            cards: Vec::new(),
        })
    );
    assert_eq!(guided.parse("2 keep the old flag"), Typed::Message);
    let open = choice(FreeText::Answer);
    assert_eq!(open.own_answer_number(), Some(3));
    assert_eq!(
        open.parse("3: use the v2 schema"),
        Typed::Pick(Pick::OwnAnswer {
            text: Some("use the v2 schema".to_owned())
        })
    );
    assert_eq!(open.parse("3"), Typed::Pick(Pick::OwnAnswer { text: None }));
    assert_eq!(open.command(&Pick::OwnAnswer { text: None }), None);
    let lines = open.lines();
    assert!(
        lines.contains(&"  3. Type my own answer".to_owned()),
        "{lines:?}"
    );
}

#[test]
fn an_answer_goes_to_its_card() {
    let card = choice(FreeText::None);
    let pick = Pick::Option {
        index: 1,
        note: None,
        cards: Vec::new(),
    };
    assert_eq!(
        card.command(&pick),
        Some(UiCommand::Answer {
            to: AnswerTarget::Choice {
                id: ChoiceId::numbered(1)
            },
            action: AnswerAction::SendBack,
            text: None,
            evidence_sha256: None,
            cards: Vec::new(),
            remember: false,
        })
    );
    assert_eq!(card.picked(&pick), "you: 2. Crew run");
}
