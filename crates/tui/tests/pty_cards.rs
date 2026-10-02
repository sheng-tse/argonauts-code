//! ◆ cards above the input: focus, keys, notes, the evidence hash, the bell, Esc, and the
//! guided setup's steps.

mod pty_support;

use pty_support::*;
use serde_json::{Value, json};

const MERGE_CARD: &str = "◆ Merge";
const PLACEHOLDER: &str = "↑ to answer the card, or type to guide the run";

/// A finished build waiting at its merge checkpoint `cp-001`.
fn merge_waiting() -> (RunScript, Script) {
    let mut run = RunScript::default();
    let events = vec![
        status_with(json!({"run": RUN, "title": "Add a --json flag to export"})),
        run.started("Add a --json flag to export", "crew"),
        run.merge_checkpoint("cp-001"),
        run.parked("checkpoint"),
        mode_parked(RUN, "checkpoint"),
    ];
    (run, Script::idle().all(0, events))
}

fn checkpoint_target(id: &str) -> Value {
    json!({"target": "checkpoint", "run": RUN, "id": id})
}

#[test]
fn card_keys_work_with_empty_prompt() {
    let (mut run, script) = merge_waiting();
    let decided = run.decided(
        "cp-001",
        json!({"action": "pull_request"}),
        json!({"party": "user"}),
    );
    let script = script.await_command("answer").at(0, decided);
    let mut harness = Harness::start(script);
    let screen = harness.wait_visible(MERGE_CARD);
    assert!(
        row_of(&screen, "❯ 1\\. Merge").is_none(),
        "a card never takes focus by itself:\n{}",
        screen.visible_text()
    );
    harness.wait_visible(PLACEHOLDER);
    harness.press(Key::Tab);
    harness.wait_visible("❯ 1\\. Merge");
    harness.press(Key::Down);
    harness.wait_visible("❯ 2\\. Open a pull request");
    harness.press(Key::Enter);
    let answer = harness.wait_command("answer");
    assert_eq!(answer["to"], checkpoint_target("cp-001"));
    assert_eq!(answer["action"], json!({"action": "pull_request"}));
    harness.wait_gone("Enter to choose");
}

#[test]
fn typed_text_never_answers_card() {
    let (_, script) = merge_waiting();
    let mut harness = Harness::start(script);
    harness.wait_visible(MERGE_CARD);
    for text in ["yes, let's merge", "1", "y"] {
        harness.submit(text);
    }
    harness.assert_no_command("answer");
    harness.wait_visible(MERGE_CARD);
}

#[test]
fn own_answer_option_opens_line_in_card() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .all(0, events)
        .at(0, run.question_checkpoint("cp-001", &crew[0]));
    let mut harness = Harness::start(script);
    harness.wait_visible("Keep a CSV fallback\\?");
    harness.press(Key::Up);
    harness.wait_visible("❯ 1\\. Yes, keep it");
    harness.press(Key::Char('3'));
    harness.send("only behind a flag");
    let screen = harness.wait_visible("only behind a flag");
    let typed = expect_row(&screen, "only behind a flag");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        typed < rule,
        "the line is inside the card:\n{}",
        screen.visible_text()
    );
    assert!(
        !screen.visible[rule + 1].contains("only behind a flag"),
        "not in the input"
    );
    harness.press(Key::Enter);
    let answer = harness.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "answer"}));
    assert_eq!(answer["text"], "only behind a flag");
    assert_eq!(answer["to"], checkpoint_target("cp-001"));
}

#[test]
fn card_tab_adds_note() {
    let mut run = RunScript::default();
    let script = Script::idle().all(
        0,
        [
            run.started("Add a --json flag to export", "crew"),
            run.plan_checkpoint("cp-001"),
            mode_parked(RUN, "checkpoint"),
        ],
    );
    let mut harness = Harness::start(script);
    harness.wait_visible("Approve with edits");
    harness.press(Key::Up);
    harness.wait_visible("❯ 1\\. Approve the plan");
    harness.press(Key::Down);
    harness.wait_visible("❯ 2\\. Approve with edits");
    harness.press(Key::Tab);
    harness.send("cap it at 100 a minute");
    harness.wait_visible("cap it at 100 a minute");
    harness.press(Key::Enter);
    let answer = harness.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "approve_with_edits"}));
    let note = answer["text"].as_str().unwrap_or_default();
    assert!(note.contains("cap it at 100 a minute"), "{answer}");
}

#[test]
fn keys_typed_as_card_appears_kept() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .all(0, events)
        .await_signal("card")
        .at(0, run.question_checkpoint("cp-001", &crew[0]));
    let mut harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    harness.send("keep go");
    harness.signal("card");
    harness.send("ing");
    harness.wait_visible("Keep a CSV fallback\\?");
    let screen = harness.wait_visible("❯ keep going");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        screen.visible[rule + 1].contains("keep going"),
        "{}",
        screen.visible_text()
    );
    harness.press(Key::Enter);
    harness.wait_command_where("submit", |c| c["text"] == "keep going");
    harness.assert_no_command("answer");
}

#[test]
fn bell_rings_when_needs_you() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .all(0, events)
        .await_signal("card")
        .at(0, run.question_checkpoint("cp-001", &crew[0]));
    let harness = Harness::start(script);
    harness.wait_visible("esc to interrupt");
    assert_eq!(harness.bells(), 0, "nothing needs you yet");
    harness.signal("card");
    harness.wait_visible("Keep a CSV fallback\\?");
    harness.wait_until("the bell", |_| harness.bells() > 0);
    assert_eq!(harness.bells(), 1);
}

#[test]
fn answer_sends_rendered_hash() {
    let (_, script) = merge_waiting();
    let mut harness = Harness::start(script);
    harness.wait_visible(MERGE_CARD);
    harness.press(Key::Up);
    harness.wait_visible("❯ 1\\. Merge");
    harness.press(Key::Char('1'));
    harness.wait_visible("Enter to merge · Esc to cancel");
    harness.assert_no_command("answer");
    harness.press(Key::Enter);
    let answer = harness.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "merge"}));
    assert_eq!(answer["evidence_sha256"], EVIDENCE);
    assert_eq!(answer["to"], checkpoint_target("cp-001"));
}

#[test]
fn merge_request_changes_picks_cards() {
    let (_, script) = merge_waiting();
    let mut harness = Harness::start(script);
    harness.wait_visible(MERGE_CARD);
    harness.press(Key::Up);
    harness.wait_visible("❯ 1\\. Merge");
    harness.press(Key::Char('3'));
    harness.wait_visible("card-003");
    let screen = harness.screen();
    let first = expect_row(&screen, "card-002");
    let second = expect_row(&screen, "card-003");
    assert!(first < second, "{}", screen.visible_text());
    harness.press(Key::Char(' '));
    harness.press(Key::Enter);
    // The changes are what the agents act on, so Enter on the ticks asks for them.
    harness.wait_visible("Request changes on some cards, and add a note");
    harness.send("split the writer");
    harness.wait_visible("split the writer");
    harness.press(Key::Enter);
    let answer = harness.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "request_changes"}));
    assert_eq!(answer["cards"], json!(["card-002"]));
    assert_eq!(answer["text"], "split the writer");
    assert_eq!(answer["evidence_sha256"], EVIDENCE);
}

#[test]
fn spot_check_card_sends_spot_check() {
    let (mut run, _, events) = crew_run(1);
    let by_approver = json!({"party": "approver", "model": "codex/gpt-5.6"});
    let script = Script::idle().all(0, events).all(
        0,
        [
            run.plan_checkpoint("cp-001"),
            run.decided("cp-001", json!({"action": "approve"}), by_approver),
            run.spot_check_queued("cp-001"),
        ],
    );
    let mut harness = Harness::start(script);
    harness.wait_visible("Overrule");
    let screen = harness.screen();
    assert_order(&screen, &["Agree", "Overrule", "Show the evidence"]);
    harness.press(Key::Up);
    harness.wait_visible("❯ 1\\. Agree");
    harness.press(Key::Char('2'));
    let check = harness.wait_command("spot_check");
    assert_eq!(
        check,
        json!({"type": "spot_check", "run": RUN, "id": "cp-001", "correct": false})
    );
    harness.assert_no_command("answer");
}

#[test]
fn esc_sets_card_aside_then_pauses_then_stops_now() {
    let (mut run, crew, events) = crew_run(2);
    let script = Script::idle()
        .all(0, events)
        .at(0, run.question_checkpoint("cp-001", &crew[0]))
        .await_command("pause")
        .at(0, mode_pausing(RUN))
        .await_command("pause")
        .at(0, run.paused(2))
        .at(0, mode_parked(RUN, "paused"));
    let mut harness = Harness::start(script);
    harness.wait_visible("Keep a CSV fallback\\?");
    harness.press(Key::Esc);
    let screen = harness.wait_visible("· needs you · ↑ to answer");
    assert!(
        row_of(&screen, "^◆ ").is_some(),
        "{}",
        screen.visible_text()
    );
    harness.wait_visible("← 1 needs you");
    harness.assert_no_command("pause");
    harness.assert_no_command("set_aside");
    harness.press(Key::Esc);
    assert_eq!(harness.wait_command("pause")["now"], false);
    harness.wait_visible("Pausing… \\(esc again to stop now\\)");
    harness.press(Key::Esc);
    assert_eq!(harness.wait_command("pause")["now"], true);
    let pauses = harness
        .command_types()
        .iter()
        .filter(|t| *t == "pause")
        .count();
    assert_eq!(pauses, 2);
}

#[test]
fn guided_setup_steps() {
    let terms = choice(
        "ch-001",
        "terms_note",
        "Before you start",
        &["Argonauts runs the agent CLIs you already use, under your own plans."],
        vec![option('c', "Continue", choose("continue"), true)],
    );
    let agent_step = choice(
        "ch-002",
        "init",
        "Which agent answers your chat?",
        &["It answers questions and decides when to start a run."],
        vec![
            option('a', "claude-code/haiku", choose("claude-code/haiku"), true),
            option('b', "codex/gpt-5.6", choose("codex/gpt-5.6"), false),
        ],
    );
    let preset_step = choice(
        "ch-003",
        "init",
        "How should the crew work?",
        &[],
        vec![
            option('a', "peer-crew", choose("peer-crew"), true),
            option('b', "coordinated-crew", choose("coordinated-crew"), false),
        ],
    );
    let script = Script::new()
        .at(0, welcome())
        .at(0, terms)
        .await_command("answer")
        .at(0, choice_closed("ch-001", "Terms noted."))
        .at(0, agent_step)
        .await_command("answer")
        .at(0, choice_closed("ch-002", "Chat goes to codex/gpt-5.6."))
        .at(0, preset_step);
    // You set the setup off, so each step opens with the keys, as Claude Code's questions do.
    let mut harness = Harness::start(script);
    harness.wait_visible("Before you start");
    harness.wait_visible("❯ 1\\. Continue");
    harness.press(Key::Enter);
    let first = harness.wait_command("answer");
    assert_eq!(first["to"], json!({"target": "choice", "id": "ch-001"}));
    assert_eq!(first["action"], choose("continue"));

    let screen = harness.wait_visible("Which agent answers your chat\\?");
    assert_order(&screen, &["claude-code/haiku", "codex/gpt-5.6"]);
    harness.wait_visible("❯ 1\\. claude-code/haiku");
    // Letters and `/` never reach the input under it, and the number answers.
    harness.send("b/");
    harness.press(Key::Down);
    harness.wait_visible("❯ 2\\. codex/gpt-5.6");
    assert_eq!(harness.command_types(), ["answer"]);
    harness.press(Key::Char('2'));
    let second = harness.wait_command("answer");
    assert_eq!(second["to"], json!({"target": "choice", "id": "ch-002"}));
    assert_eq!(second["action"], choose("codex/gpt-5.6"));

    harness.wait_visible("How should the crew work\\?");
    harness.wait_visible("❯ 1\\. peer-crew");
    harness.press(Key::Esc);
    let aside = harness.wait_command("set_aside");
    assert_eq!(
        aside,
        json!({"type": "set_aside", "to": {"target": "choice", "id": "ch-003"}})
    );
    assert_eq!(harness.command_types(), ["answer", "answer", "set_aside"]);
}
