//! ◆ questions above the input: focus, keys, notes, the evidence hash, the bell, Esc, and the
//! guided setup's steps. A question that needs you takes the keys as it shows, and a draft being
//! typed goes aside until the question is answered or passed on.

use crate::pty_support;
use argonauts_core::app::AppEvent;
use argonauts_testkit::screen::{Grid, Look, REAL_SIZES, Size, laws};
use pty_support::*;
use serde_json::{Value, json};

const MERGE_QUESTION: &str = "◆ Merge";
/// The question's own key line while it has the keys.
const QUESTION_KEYS: &str =
    "^  Enter to choose.* · Esc back to the list( · shift\\+tab to cycle modes)?$";

/// The question has the keys and stands in for the input, whose rules are gone.
fn question_holds_keys(driver: &TestDriver) {
    driver.wait_visible(QUESTION_KEYS);
    driver.wait_gone(TOP_RULE);
}

/// A finished build waiting at its merge approval `approval-001`.
fn merge_waiting() -> (RunScript, Script) {
    let mut run = RunScript::default();
    let events = vec![
        status_with(json!({"run": RUN, "title": "Add a --json flag to export"})),
        run.started("Add a --json flag to export", "crew"),
        run.merge_approval("approval-001"),
        run.waiting("approval"),
        mode_waiting(RUN, "approval"),
    ];
    (run, Script::idle().all(0, events))
}

fn approval_target(id: &str) -> Value {
    json!({"target": "approval", "run": RUN, "id": id})
}

#[test]
fn a_question_that_needs_you_takes_the_keys_as_it_shows() {
    let (mut run, script) = merge_waiting();
    let decided = run.decided(
        "approval-001",
        json!({"action": "pull_request"}),
        json!({"party": "user"}),
    );
    let script = script.await_command("answer").at(0, decided);
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MERGE_QUESTION);
    driver.wait_visible("❯ 1\\. Merge");
    question_holds_keys(&driver);
    driver.press(Key::Down);
    driver.wait_visible("❯ 2\\. Open a pull request");
    past_the_question_guard();
    driver.press(Key::Enter);
    let answer = driver.wait_command("answer");
    assert_eq!(answer["to"], approval_target("approval-001"));
    assert_eq!(answer["action"], json!({"action": "pull_request"}));
    driver.wait_gone("Enter to choose");
}

#[test]
fn letters_typed_at_a_question_do_nothing() {
    let (_, script) = merge_waiting();
    let mut driver = TestDriver::start(script);
    driver.wait_visible("❯ 1\\. Merge");
    past_the_question_guard();
    // Letters reach neither the question nor the input under it, and the key line stays.
    driver.send("yes");
    question_holds_keys(&driver);
    driver.assert_no_command("answer");
    driver.assert_no_command("submit");
    driver.press(Key::Down);
    question_holds_keys(&driver);
    driver.wait_visible("❯ 2\\. Open a pull request");
    driver.assert_no_command("answer");

    // Carried on, it leaves the screen, a line says who decides now, and the input takes
    // typing again.
    past_the_question_guard();
    carry_on(&mut driver);
    driver.wait_visible("◆ Merge approval left to the main agent · you, ");
    driver.wait_gone("2\\. Open a pull request");
    let answer = driver.wait_command("answer");
    assert_eq!(answer["to"], approval_target("approval-001"));
    assert_eq!(answer["action"]["action"], "carry_on");
    // What's typed then goes to the input, and answers nothing more.
    driver.submit("keep the CSV path");
    let answers = driver
        .command_types()
        .iter()
        .filter(|kind| *kind == "answer")
        .count();
    assert_eq!(answers, 1, "{:?}", driver.command_types());
}

#[test]
fn own_answer_option_opens_line_in_question() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .all(0, events)
        .at(0, run.question_approval("approval-001", &crew[0]));
    let mut driver = TestDriver::start(script);
    driver.wait_visible("Keep a CSV fallback\\?");
    driver.wait_visible("❯ 1\\. Yes, keep it");
    past_the_question_guard();
    driver.press(Key::Char('3'));
    driver.wait_visible("Enter to answer · Esc to close");
    driver.send("only behind a flag");
    let screen = driver.wait_visible("only behind a flag");
    let typed = expect_row(&screen, "only behind a flag");
    let keys = expect_row(&screen, "Enter to answer · Esc to close");
    assert!(
        typed < keys,
        "the line is inside the question:\n{}",
        screen.visible_text()
    );
    assert!(
        !screen.visible_text().contains(" council ─"),
        "the input is hidden while the question has the keys:\n{}",
        screen.visible_text()
    );
    driver.press(Key::Enter);
    let answer = driver.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "answer"}));
    assert_eq!(answer["text"], "only behind a flag");
    assert_eq!(answer["to"], approval_target("approval-001"));
}

#[test]
fn question_tab_adds_note() {
    let mut run = RunScript::default();
    let script = Script::idle().all(
        0,
        [
            run.started("Add a --json flag to export", "crew"),
            run.plan_approval("approval-001"),
            mode_waiting(RUN, "approval"),
        ],
    );
    let mut driver = TestDriver::start(script);
    driver.wait_visible("Approve with edits");
    driver.wait_visible("❯ 1\\. Approve the plan");
    past_the_question_guard();
    driver.press(Key::Down);
    driver.wait_visible("❯ 2\\. Approve with edits");
    driver.press(Key::Tab);
    driver.send("cap it at 100 a minute");
    driver.wait_visible("cap it at 100 a minute");
    driver.press(Key::Enter);
    let answer = driver.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "approve_with_edits"}));
    let note = answer["text"].as_str().unwrap_or_default();
    assert!(note.contains("cap it at 100 a minute"), "{answer}");
}

#[test]
fn a_draft_typed_as_a_question_appears_comes_back_after() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .all(0, events)
        .await_signal("question")
        .at(0, run.question_approval("approval-001", &crew[0]));
    let mut driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    driver.send("keep go");
    driver.wait_visible("❯ keep go");
    driver.signal("question");
    driver.wait_visible("Keep a CSV fallback\\?");
    question_holds_keys(&driver);
    driver.assert_no_command("submit");
    // Set aside, the draft comes back as it was.
    past_the_question_guard();
    driver.press(Key::Esc);
    let screen = driver.wait_visible("❯ keep go");
    let rule = expect_row(&screen, TOP_RULE);
    assert!(
        screen.visible[rule + 1].contains("keep go"),
        "{}",
        screen.visible_text()
    );
    driver.send("ing");
    driver.press(Key::Enter);
    driver.wait_command_where("submit", |c| c["text"] == "keep going");
    driver.assert_no_command("answer");
}

#[test]
fn bell_rings_when_needs_you() {
    let (mut run, crew, events) = crew_run(1);
    let script = Script::idle()
        .all(0, events)
        .await_signal("question")
        .at(0, run.question_approval("approval-001", &crew[0]));
    let driver = TestDriver::start(script);
    driver.wait_visible(WORKING);
    assert_eq!(driver.bells(), 0, "nothing needs you yet");
    driver.signal("question");
    driver.wait_visible("Keep a CSV fallback\\?");
    driver.wait_until("the bell", |_| driver.bells() > 0);
    assert_eq!(driver.bells(), 1);
}

#[test]
fn answer_sends_rendered_hash() {
    let (_, script) = merge_waiting();
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MERGE_QUESTION);
    driver.wait_visible("❯ 1\\. Merge");
    past_the_question_guard();
    driver.press(Key::Char('1'));
    driver.wait_visible("Enter to merge · Esc to cancel");
    driver.assert_no_command("answer");
    driver.press(Key::Enter);
    let answer = driver.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "merge"}));
    assert_eq!(answer["evidence_sha256"], EVIDENCE);
    assert_eq!(answer["to"], approval_target("approval-001"));
}

#[test]
fn merge_request_changes_picks_tasks() {
    let (_, script) = merge_waiting();
    let mut driver = TestDriver::start(script);
    driver.wait_visible(MERGE_QUESTION);
    driver.wait_visible("❯ 1\\. Merge");
    past_the_question_guard();
    driver.press(Key::Char('3'));
    driver.wait_visible("task-003");
    let screen = driver.screen();
    let first = expect_row(&screen, "task-002");
    let second = expect_row(&screen, "task-003");
    assert!(first < second, "{}", screen.visible_text());
    driver.press(Key::Char(' '));
    driver.press(Key::Enter);
    // The changes are what the agents act on, so Enter on the ticks asks for them.
    driver.wait_visible("Request changes on some tasks, and add a note");
    driver.send("split the writer");
    driver.wait_visible("split the writer");
    driver.press(Key::Enter);
    let answer = driver.wait_command("answer");
    assert_eq!(answer["action"], json!({"action": "request_changes"}));
    assert_eq!(answer["tasks"], json!(["task-002"]));
    assert_eq!(answer["text"], "split the writer");
    assert_eq!(answer["evidence_sha256"], EVIDENCE);
}

#[test]
fn spot_check_question_sends_spot_check() {
    let (mut run, _, events) = crew_run(1);
    let by_judge = json!({"party": "judge", "model": "codex/gpt-5.6"});
    let script = Script::idle().all(0, events).all(
        0,
        [
            run.plan_approval("approval-001"),
            run.decided("approval-001", json!({"action": "approve"}), by_judge),
            run.spot_check_queued("approval-001"),
        ],
    );
    let mut driver = TestDriver::start(script);
    driver.wait_visible("Overrule");
    let screen = driver.screen();
    assert_order(&screen, &["Agree", "Overrule", "Show the evidence"]);
    driver.wait_visible("❯ 1\\. Agree");
    past_the_question_guard();
    driver.press(Key::Char('2'));
    let check = driver.wait_command("spot_check");
    assert_eq!(
        check,
        json!({"type": "spot_check", "run": RUN, "id": "approval-001", "correct": false})
    );
    driver.assert_no_command("answer");
}

/// Esc on a run's question hands it on: the screen says so to the engine, whose
/// decision closes the question, so nothing is left under "needs you". The next Esc leaves the
/// run's agents going and says how to stop them. The run's row in the list
/// pauses it, where its stop is asked, and once the run is pausing Esc stops it now.
#[test]
fn esc_hands_the_question_on_leaves_the_run_going_and_stops_a_pausing_one_now() {
    let (mut run, crew, events) = crew_run(2);
    let question = run.question_approval("approval-001", &crew[0]);
    let left = run.decided(
        "approval-001",
        json!({"action": "answer"}),
        json!({"party": "declined"}),
    );
    let script = Script::idle()
        .all(0, events)
        .at(0, question)
        .await_command("answer")
        .at(0, left)
        .await_command("pause")
        .at(0, mode_pausing(RUN))
        .await_command("pause")
        .at(0, run.paused(2))
        .at(0, mode_waiting(RUN, "paused"));
    let mut driver = TestDriver::start(script);
    driver.wait_visible("Keep a CSV fallback\\?");
    past_the_question_guard();
    carry_on(&mut driver);
    driver.wait_visible("◆ Question left to the agent · you, ");
    driver.wait_gone("Keep a CSV fallback\\?");
    let answer = driver.wait_command("answer");
    assert_eq!(answer["to"], approval_target("approval-001"));
    assert_eq!(answer["action"]["action"], "carry_on");
    // The engine's decision closes it, so nothing is left under "needs you".
    driver.wait_gone("needs you");
    driver.assert_no_command("pause");
    driver.press(Key::Esc);
    driver.wait_visible("The run keeps going · ctrl\\+x ctrl\\+k to stop all agents");
    driver.assert_no_command("pause");
    // x on the run's row asks before it stops, and p there pauses it instead.
    driver.press(Key::Down);
    driver.press(Key::Char('x'));
    driver.wait_visible("x again to stop run-01 · p to pause it · esc to keep");
    driver.press(Key::Char('p'));
    assert_eq!(driver.wait_command("pause")["now"], false);
    driver.wait_visible("Pausing… \\(esc again to stop now\\)");
    // The first Esc gives the list's keys back to the input, and the next stops the run now.
    driver.press(Key::Esc);
    driver.wait_gone("enter to view");
    driver.press(Key::Esc);
    assert_eq!(driver.wait_command("pause")["now"], true);
    let pauses = driver
        .command_types()
        .iter()
        .filter(|t| *t == "pause")
        .count();
    assert_eq!(pauses, 2);
}

/// The templates `/templates` lists: seventeen names, each with its kind.
const TEMPLATES: [(&str, &str); 17] = [
    ("Council", "workflow · in use"),
    ("Orchestrator", "workflow"),
    ("Spec-first", "workflow"),
    ("Maker-checker", "workflow"),
    ("Best-of-n", "workflow"),
    ("Draft-and-vote", "workflow"),
    ("Ablate-no-blind", "workflow"),
    ("Ablate-no-anonymize", "workflow"),
    ("Ablate-no-critique", "workflow"),
    ("Ablate-fixed-roles", "workflow"),
    ("Ablate-same-provider", "workflow"),
    ("Ablate-no-experiments", "workflow"),
    ("main", "prompts"),
    ("orchestrator", "prompts"),
    ("workers", "prompts"),
    ("reviewers", "prompts"),
    ("judge", "prompts"),
];

/// The templates as the question lists them, numbered.
fn listed_templates() -> Vec<String> {
    TEMPLATES
        .iter()
        .zip(1..)
        .map(|((name, _), number)| format!("{:<4}{name}", format!("{number}.")))
        .collect()
}

/// What `/templates` asks once it has listed them.
fn which_template() -> AppEvent {
    let options = TEMPLATES
        .iter()
        .zip("123456789abcdfghi".chars())
        .map(|((name, kind), key)| {
            let mut option = option(key, name, choose(name), false);
            option["detail"] = json!(kind);
            option
        })
        .collect();
    choice("ch-001", "templates", "Copy which one?", &[], options)
}

/// The app at `size` asking which template to copy.
fn asking_which_template(size: Size) -> TestDriver {
    let script = Script::new()
        .at(0, welcome())
        .at(0, which_template())
        .await_command("answer")
        .at(0, choice_closed("ch-001", "Copying Ablate-same-provider."));
    TestDriver::launch(script)
        .size(size.rows, size.cols)
        .start()
}

/// A question with more names than it lists at once keeps its title, its pointer and its
/// keys on the screen, on a small terminal and a larger one, and going down past the last
/// name shown brings the next ones up.
#[test]
fn a_long_list_of_options_scrolls_under_its_title_and_over_its_keys() {
    let listed = listed_templates();
    for size in REAL_SIZES {
        let (rows, cols) = (size.rows, size.cols);
        let mut driver = asking_which_template(size);
        let screen = driver.wait_visible("❯ 1\\.  Council +workflow · in use");
        let at = |screen: &Screen, pattern: &str| expect_row(screen, pattern);
        let title = at(&screen, "^◆ Copy which one\\?$");
        let keys = at(&screen, "^  Enter to choose · Esc to cancel +1–10 of 17 ↓$");
        assert!(is_rule(&screen.visible[title - 1]), "{rows}x{cols}");
        assert_eq!(at(&screen, "❯ 1\\.  Council"), title + 2, "{rows}x{cols}");
        assert_eq!(
            at(&screen, "  10\\. Ablate-fixed-roles +workflow$"),
            title + 11
        );
        assert_eq!(keys, title + 13, "{rows}x{cols}");
        assert_eq!(row_of(&screen, "11\\. Ablate-same-provider"), None);
        assert_frames(&screen, cols);
        let grid = Grid::from_text(cols, &screen.visible);
        let look = Look::new().selected("1.  Council").list(&listed);
        laws::assert_kept(&grid, &look, &format!("the question at {size}"));

        // The question stands in for the input.
        driver.wait_gone(TOP_RULE);
        for _ in 0..10 {
            driver.press(Key::Down);
        }
        let screen = driver.wait_visible("❯ 11\\. Ablate-same-provider +workflow$");
        assert_eq!(at(&screen, "^◆ Copy which one\\?$"), title, "{rows}x{cols}");
        assert_eq!(
            at(
                &screen,
                "^  Enter to choose · Esc to cancel +↑  6–15 of 17 ↓$"
            ),
            keys
        );
        assert_eq!(row_of(&screen, "  5\\.  Best-of-n"), None, "{rows}x{cols}");
        assert_eq!(at(&screen, "  15\\. workers +prompts$"), title + 11);

        past_the_question_guard();
        driver.press(Key::Enter);
        let answer = driver.wait_command("answer");
        assert_eq!(
            answer["action"],
            json!({"action": "choose", "value": "Ablate-same-provider"})
        );
        driver.wait_gone("Enter to choose");
    }
}

/// Ten names down the long question, past the first ones it shows, the screen still keeps the
/// laws, and says which of the names it shows.
#[test]
fn a_long_list_of_options_keeps_the_screen_laws_scrolled_down() {
    let listed = listed_templates();
    for size in REAL_SIZES {
        let mut driver = asking_which_template(size);
        driver.wait_visible("❯ 1\\.  Council +workflow · in use");
        driver.wait_gone(TOP_RULE);
        for _ in 0..10 {
            driver.press(Key::Down);
        }
        let screen = driver.wait_visible("❯ 11\\. Ablate-same-provider +workflow$");
        let grid = Grid::from_text(size.cols, &screen.visible);
        let look = Look::new()
            .selected("11. Ablate-same-provider")
            .list(&listed);
        laws::assert_kept(&grid, &look, &format!("the question at {size}, ten down"));
    }
}

#[test]
fn guided_setup_steps() {
    let terms = choice(
        "ch-001",
        "terms_note",
        "Before you start",
        &["Argonauts Code runs the agent CLIs you already use, under your own plans."],
        vec![option('c', "Continue", choose("continue"), true)],
    );
    let agent_step = choice(
        "ch-002",
        "init",
        "Which agent answers your conversation?",
        &["It answers questions and decides when to start a run."],
        vec![
            option('a', "claude-code/haiku", choose("claude-code/haiku"), true),
            option('b', "codex/gpt-5.6", choose("codex/gpt-5.6"), false),
        ],
    );
    let workflow_step = choice(
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
        .at(
            0,
            choice_closed("ch-002", "Conversation goes to codex/gpt-5.6."),
        )
        .at(0, workflow_step);
    // You set the setup off, so each step opens with the keys, as Claude Code's questions do,
    // and drops the keys pressed in its first moment.
    let mut driver = TestDriver::start(script);
    driver.wait_visible("Before you start");
    driver.wait_visible("❯ 1\\. Continue");
    past_the_question_guard();
    driver.press(Key::Enter);
    let first = driver.wait_command("answer");
    assert_eq!(first["to"], json!({"target": "choice", "id": "ch-001"}));
    assert_eq!(first["action"], choose("continue"));

    let screen = driver.wait_visible("Which agent answers your conversation\\?");
    assert_order(&screen, &["claude-code/haiku", "codex/gpt-5.6"]);
    driver.wait_visible("❯ 1\\. claude-code/haiku");
    past_the_question_guard();
    // Letters and `/` never reach the input under it, and the number answers.
    driver.send("b/");
    driver.press(Key::Down);
    driver.wait_visible("❯ 2\\. codex/gpt-5.6");
    assert_eq!(driver.command_types(), ["answer"]);
    driver.press(Key::Char('2'));
    let second = driver.wait_command("answer");
    assert_eq!(second["to"], json!({"target": "choice", "id": "ch-002"}));
    assert_eq!(second["action"], choose("codex/gpt-5.6"));

    driver.wait_visible("How should the crew work\\?");
    driver.wait_visible("❯ 1\\. council");
    past_the_question_guard();
    driver.press(Key::Esc);
    let aside = driver.wait_command("set_aside");
    assert_eq!(
        aside,
        json!({"type": "set_aside", "to": {"target": "choice", "id": "ch-003"}})
    );
    assert_eq!(driver.command_types(), ["answer", "answer", "set_aside"]);
}
