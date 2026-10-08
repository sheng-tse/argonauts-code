//! ◆ cards above the input: focus, keys, notes, the evidence hash, the bell, Esc, and the
//! guided setup's steps. A card that needs you takes the keys as it shows, and a draft being
//! typed goes aside until the card is answered or handed on.

mod pty_support;

use pty_support::*;
use serde_json::{Value, json};

const MERGE_CARD: &str = "◆ Merge";
/// The card's own key line while it has the keys.
const CARD_KEYS: &str =
    "^  Enter to choose.* · Esc to carry on without me( · shift\\+tab to cycle modes)?$";
/// The key line once something was typed at the card.
const TYPED_AT: &str =
    "^  Typing waits while the question has the keys · ↑↓ to choose · Esc to carry on without me$";

/// The card has the keys and stands in for the input, whose rules are gone.
fn card_holds_keys(harness: &Harness) {
    harness.wait_visible(CARD_KEYS);
    harness.wait_gone(TOP_RULE);
}

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
fn a_card_that_needs_you_takes_the_keys_as_it_shows() {
    let (mut run, script) = merge_waiting();
    let decided = run.decided(
        "cp-001",
        json!({"action": "pull_request"}),
        json!({"party": "user"}),
    );
    let script = script.await_command("answer").at(0, decided);
    let mut harness = Harness::start(script);
    harness.wait_visible(MERGE_CARD);
    harness.wait_visible("❯ 1\\. Merge");
    card_holds_keys(&harness);
    harness.press(Key::Down);
    harness.wait_visible("❯ 2\\. Open a pull request");
    past_the_card_guard();
    harness.press(Key::Enter);
    let answer = harness.wait_command("answer");
    assert_eq!(answer["to"], checkpoint_target("cp-001"));
    assert_eq!(answer["action"], json!({"action": "pull_request"}));
    harness.wait_gone("Enter to choose");
}

#[test]
fn chat_typed_at_a_card_never_answers_it() {
    let (_, script) = merge_waiting();
    let mut harness = Harness::start(script);
    harness.wait_visible("❯ 1\\. Merge");
    // The pointer is on Merge, so the Enter that ends a chat line must not confirm it, and
    // neither may a number in the next line or another Enter.
    harness.send("yes, let's merge");
    harness.wait_visible(TYPED_AT);
    harness.press(Key::Enter);
    harness.send("also 2 more");
    harness.press(Key::Enter);
    harness.wait_visible(TYPED_AT);
    harness.assert_no_command("answer");
    harness.assert_no_command("submit");
    // A key meant for the card ends it.
    harness.press(Key::Down);
    card_holds_keys(&harness);
    harness.wait_visible("❯ 2\\. Open a pull request");
    harness.assert_no_command("answer");

    // Handed on, it leaves the screen, a line says who decides now, and the input takes
    // typing again.
    past_the_card_guard();
    harness.press(Key::Esc);
    harness.wait_visible("◆ Merge approval left to the main agent · you, ");
    harness.wait_gone("2\\. Open a pull request");
    assert_eq!(
        harness.wait_command("set_aside")["to"],
        checkpoint_target("cp-001")
    );
    harness.submit("keep the CSV path");
    harness.assert_no_command("answer");
}

#[test]
fn own_answer_option_opens_line_in_card() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .all(0, events)
        .at(0, run.question_checkpoint("cp-001", &crew[0]));
    let mut harness = Harness::start(script);
    harness.wait_visible("Keep a CSV fallback\\?");
    harness.wait_visible("❯ 1\\. Yes, keep it");
    past_the_card_guard();
    harness.press(Key::Char('3'));
    harness.wait_visible("Enter to answer · Esc to close");
    harness.send("only behind a flag");
    let screen = harness.wait_visible("only behind a flag");
    let typed = expect_row(&screen, "only behind a flag");
    let keys = expect_row(&screen, "Enter to answer · Esc to close");
    assert!(
        typed < keys,
        "the line is inside the task:\n{}",
        screen.visible_text()
    );
    assert!(
        !screen.visible_text().contains("council · Haiku ─"),
        "the input is hidden while the question has the keys:\n{}",
        screen.visible_text()
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
    harness.wait_visible("❯ 1\\. Approve the plan");
    past_the_card_guard();
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
fn a_draft_typed_as_a_card_appears_comes_back_after() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .all(0, events)
        .await_signal("card")
        .at(0, run.question_checkpoint("cp-001", &crew[0]));
    let mut harness = Harness::start(script);
    harness.wait_visible(WORKING);
    harness.send("keep go");
    harness.wait_visible("❯ keep go");
    harness.signal("card");
    harness.wait_visible("Keep a CSV fallback\\?");
    card_holds_keys(&harness);
    // The rest of the line lands on the card as stray typing, and its Enter answers nothing.
    harness.send("ing");
    harness.press(Key::Enter);
    harness.wait_visible(TYPED_AT);
    harness.assert_no_command("answer");
    harness.assert_no_command("submit");
    // Set aside, the draft comes back as it was.
    past_the_card_guard();
    harness.press(Key::Esc);
    let screen = harness.wait_visible("❯ keep go");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        screen.visible[rule + 1].contains("keep go"),
        "{}",
        screen.visible_text()
    );
    harness.send("ing");
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
    harness.wait_visible(WORKING);
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
    harness.wait_visible("❯ 1\\. Merge");
    past_the_card_guard();
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
    harness.wait_visible("❯ 1\\. Merge");
    past_the_card_guard();
    harness.press(Key::Char('3'));
    harness.wait_visible("task-003");
    let screen = harness.screen();
    let first = expect_row(&screen, "task-002");
    let second = expect_row(&screen, "task-003");
    assert!(first < second, "{}", screen.visible_text());
    harness.press(Key::Char(' '));
    harness.press(Key::Enter);
    // The changes are what the agents act on, so Enter on the ticks asks for them.
    harness.wait_visible("Request changes on some tasks, and add a note");
    harness.send("split the writer");
    harness.wait_visible("split the writer");
    harness.press(Key::Enter);
    let answer = harness.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "request_changes"}));
    assert_eq!(answer["cards"], json!(["task-002"]));
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
    harness.wait_visible("❯ 1\\. Agree");
    past_the_card_guard();
    harness.press(Key::Char('2'));
    let check = harness.wait_command("spot_check");
    assert_eq!(
        check,
        json!({"type": "spot_check", "run": RUN, "id": "cp-001", "correct": false})
    );
    harness.assert_no_command("answer");
}

/// Esc on a run's question hands it on: the screen says so to the engine, whose
/// decision closes the question, so nothing is left under "needs you". The next Esc leaves the
/// run's agents going and says how to stop them. The run's row in the list
/// pauses it, where its stop is asked, and once the run is pausing Esc stops it now.
#[test]
fn esc_hands_the_question_on_leaves_the_run_going_and_stops_a_pausing_one_now() {
    let (mut run, crew, events) = crew_run(2);
    let question = run.question_checkpoint("cp-001", &crew[0]);
    let left = run.decided(
        "cp-001",
        json!({"action": "answer"}),
        json!({"party": "declined"}),
    );
    let script = Script::idle()
        .all(0, events)
        .at(0, question)
        .await_command("set_aside")
        .at(0, left)
        .await_command("pause")
        .at(0, mode_pausing(RUN))
        .await_command("pause")
        .at(0, run.paused(2))
        .at(0, mode_parked(RUN, "paused"));
    let mut harness = Harness::start(script);
    harness.wait_visible("Keep a CSV fallback\\?");
    past_the_card_guard();
    harness.press(Key::Esc);
    harness.wait_visible("◆ Question left to the agent · you, ");
    harness.wait_gone("Keep a CSV fallback\\?");
    assert_eq!(
        harness.wait_command("set_aside")["to"],
        checkpoint_target("cp-001")
    );
    // The engine's decision closes it, so nothing is left under "needs you".
    harness.wait_gone("needs you");
    harness.assert_no_command("pause");
    harness.press(Key::Esc);
    harness.wait_visible("The run keeps going · ctrl\\+x ctrl\\+k to stop all agents");
    harness.assert_no_command("pause");
    // x on the run's row asks before it stops, and p there pauses it instead.
    harness.press(Key::Down);
    harness.press(Key::Char('x'));
    harness.wait_visible("x again to stop run-01 · p to pause it · esc to keep");
    harness.press(Key::Char('p'));
    assert_eq!(harness.wait_command("pause")["now"], false);
    harness.wait_visible("Pausing… \\(esc again to stop now\\)");
    // The first Esc gives the list's keys back to the input, and the next stops the run now.
    harness.press(Key::Esc);
    harness.wait_gone("enter to view");
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
            option('a', "council", choose("council"), true),
            option('b', "orchestrator", choose("orchestrator"), false),
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
    harness.wait_visible("❯ 1\\. council");
    harness.press(Key::Esc);
    let aside = harness.wait_command("set_aside");
    assert_eq!(
        aside,
        json!({"type": "set_aside", "to": {"target": "choice", "id": "ch-003"}})
    );
    assert_eq!(harness.command_types(), ["answer", "answer", "set_aside"]);
}
