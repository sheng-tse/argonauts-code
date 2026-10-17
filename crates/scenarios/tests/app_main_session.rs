//! The main session is the CLI you chose, through the real binary and a fake `claude` that takes
//! Argonauts Code's tools: a question is answered and starts nothing, and real work starts the
//! workflow in use when the session calls `argonauts_start_workflow`.

mod app_support;

use std::path::{Path, PathBuf};

use app_support::*;
use argonauts_core::events::run::RunMode;
use argonauts_scenarios::{as_set_up, scenario_file};
use argonauts_testkit::scenario::StdinPrompt;
use argonauts_testkit::{FakeRule, FakeScenario, FakeStep};
use serde_json::json;

const QUESTION: &str = "what does parse_due do when the date has no year?";
const WORK: &str = "add support for dates without a year, across the cli, the store and the tests";
const ANSWER: &str = "It returns an error. parse_due only accepts full ISO dates.";
const HANDING_OVER: &str = "That touches three parts, so I'm handing it to the crew.";
const ACCOUNT: &str = "Accept dates without a year in parse_due, in the cli, the store and tests.";

/// The app scenarios' settings with the main session's tools left on, as they are by default.
fn config() -> String {
    GLOBAL_CONFIG.replace("[main_session]\ntools = false\n", "")
}

/// One turn of the main session: what it says, as Claude Code's stream ends a turn.
fn says(rule: FakeRule, session: &str, text: &str) -> FakeRule {
    let init = json!({
        "type": "system", "subtype": "init", "session_id": session, "cwd": ".",
        "tools": ["Read", "Edit", "Bash"], "model": "claude-haiku-4-5"
    });
    let assistant = json!({
        "type": "assistant", "parent_tool_use_id": null, "session_id": session,
        "message": {"id": format!("msg-{session}"), "content": [{"type": "text", "text": text}]}
    });
    let result = json!({
        "type": "result", "subtype": "success", "is_error": false, "session_id": session,
        "result": text, "total_cost_usd": 0.001,
        "usage": {"input_tokens": 12, "output_tokens": 20}
    });
    rule.step(FakeStep::Emit(init.to_string()))
        .step(FakeStep::Emit(assistant.to_string()))
        .step(FakeStep::Emit(result.to_string()))
        .step(FakeStep::Exit(0))
}

/// The app's fakes, with a `claude` that answers the question, and hands the work over by
/// calling its tool before it says so.
fn fakes_file(dir: &Path) -> anyhow::Result<PathBuf> {
    let mut fakes = FakeScenario::load(&scenario_file(AGENTS))?;
    let main_session = |prompt: &str| {
        FakeRule::for_program("claude")
            .args(&["--append-system-prompt"])
            .prompt(prompt)
            .stdin(StdinPrompt::Line)
    };
    let hands_over = main_session("dates without a year").step(FakeStep::CallTool {
        name: "argonauts_start_workflow".to_owned(),
        arguments: json!({ "request": ACCOUNT }),
    });
    fakes
        .rules
        .insert(0, says(hands_over, "sess-main", HANDING_OVER));
    fakes
        .rules
        .insert(0, says(main_session("parse_due"), "sess-main", ANSWER));
    let path = dir.join("main-session.toml");
    fakes.save(&path)?;
    Ok(path)
}

#[test]
fn app_main_session_answers_and_hands_real_work_over() -> anyhow::Result<()> {
    let fakes = tempfile::tempdir()?;
    let scenario = scenario_playing(&fakes_file(fakes.path())?)?;
    std::fs::write(scenario.repo.global_config(), as_set_up(&config()))?;
    let mut pty = start(&scenario)?;

    // A question: the session answers it, and nothing starts.
    submit(&mut pty, QUESTION)?;
    wait_turns_finished(&scenario, 1)?;
    pty.wait_for("It returns an error", argonauts_scenarios::WAIT)?;
    assert!(run_dirs(&scenario).is_empty(), "a question started a run");
    let sessions = sessions_of(&scenario, "claude")?;
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let args = &sessions[0].args;
    let standing = args
        .iter()
        .position(|arg| arg == "--append-system-prompt")
        .and_then(|at| args.get(at + 1))
        .expect("standing lines");
    assert!(
        standing.contains("`argonauts_start_workflow`"),
        "{standing}"
    );
    assert!(
        !args.iter().any(|arg| arg.contains("answer_markdown")),
        "the reply still ends with a verdict: {args:?}"
    );
    let turn = turns_started(&scenario)?.remove(0);
    assert_eq!(turn.text, QUESTION);
    assert!(!turn.read_only, "the main session works as its CLI does");

    // Real work: the session calls its tool, and the workflow in use starts.
    submit(&mut pty, WORK)?;
    let run_dir = wait_run_dir(&scenario)?;
    let started = run_started(&run_dir)?;
    assert_eq!(started.mode, RunMode::Crew);
    assert_eq!(
        started.request,
        format!("{WORK}\n\nFrom the conversation, as the main agent put it: {ACCOUNT}")
    );
    assert!(started.conversation_turn.is_some());
    pty.wait_for("handing it to the crew", argonauts_scenarios::WAIT)?;
    scenario.shot(&pty, "app_main_session_hands_real_work_over")?;
    Ok(())
}
