//! A crew request under the settings as they ship: the workflow in use is council, whose
//! orchestrator writes the steps for the request as a script, and the runner runs them. Each
//! agent goes to the CLI its role names, the run reports the script's own name, phases and
//! agents, and the merge approval follows.

mod app_support;

use app_support::*;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::session::{SessionEvent, SubagentStatus, VendorAgentState};
use argonauts_core::events::{RunEventKind, WorkflowEvent};
use argonauts_scenarios::WAIT;
use argonauts_testkit::Key;

/// Waits for the run to finish merged. Nobody answers a merge approval here: the workflow's
/// reviewer approved the task, from another provider than its worker, so your rules merge it.
fn wait_merged(run_dir: &std::path::Path) -> anyhow::Result<()> {
    wait_journal(run_dir, "the run to finish merged", |event| {
        matches!(
            event.kind,
            RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::Merged,
                ..
            })
        )
    })
    .map(|_| ())
}

/// The shared config with a check that passes and Codex's model named, and the built-in
/// workflows left to their orchestrator, as they ship.
fn shipped_config() -> String {
    let config = run_config().replace("adapt_with = \"phases\"", "adapt_with = \"script\"");
    assert!(config.contains("adapt_with = \"script\""), "{config}");
    // A spot check of the merge your rules decided would take the keys before the workflow
    // view is opened, so none is drawn here.
    format!(
        "{config}\n[agents.codex]\nmodel = \"gpt-5.6-luna\"\n\n[approvals]\nspot_check_sample = 0.0\n"
    )
}

#[test]
fn app_crew_request_runs_the_steps_its_orchestrator_writes() -> anyhow::Result<()> {
    let scenario = scenario_with(&shipped_config())?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::CREW_TASK)?;

    let run_dir = wait_run_dir(&scenario)?;
    wait_merged(&run_dir)?;
    let events = run_events(&run_dir)?;
    assert_eq!(run_started(&run_dir)?.mode, RunMode::Crew);

    // The orchestrator is the main agent, the worker the first of the crew, and its reviewer
    // a worker from another provider, since council has its workers review each other.
    let sessions: Vec<(String, String)> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => Some((
                started.label.clone().unwrap_or_default(),
                started.worker.worker.clone(),
            )),
            _ => None,
        })
        .collect();
    let ran = |label: &str| {
        sessions
            .iter()
            .find(|(started, _)| started == label)
            .map(|(_, cli)| cli.as_str())
    };
    assert_eq!(
        ran("plan:orchestrator"),
        Some("claude-code"),
        "{sessions:?}"
    );
    assert_eq!(ran("build:rate-limit"), Some("claude-code"), "{sessions:?}");
    assert_eq!(ran("review:rate-limit"), Some("codex"), "{sessions:?}");

    // What the views draw: the script's own name, its phases in the order it declared them,
    // and each agent with its phase, model and state.
    let report = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::Reported(report)) => Some(report.clone()),
            _ => None,
        })
        .expect("the runner reported");
    assert_eq!(report.workflow.name, "rate-limit-per-key");
    assert_eq!(report.workflow.status, SubagentStatus::Done);
    let phases: Vec<&str> = report
        .workflow
        .phases
        .iter()
        .map(|phase| phase.title.as_str())
        .collect();
    assert_eq!(phases, ["Build", "Review"]);
    let agents: Vec<(&str, Option<u32>, VendorAgentState)> = report
        .workflow
        .agents
        .iter()
        .map(|agent| (agent.label.as_str(), agent.phase, agent.state))
        .collect();
    assert_eq!(
        agents,
        [
            ("build:rate-limit", Some(1), VendorAgentState::Done),
            ("review:rate-limit", Some(2), VendorAgentState::Done),
        ]
    );
    assert!(
        report
            .workflow
            .agents
            .iter()
            .all(|agent| agent.model.is_some()),
        "each row names the model that ran it: {:?}",
        report.workflow.agents
    );

    // The reviewer's verdict is a review of the task, so your merge rules could decide.
    pty.wait_for(r"Approved merging the run into main · your rules", WAIT)?;
    pty.wait_for(r"✻ \w+ for \d", WAIT)?;
    scenario.shot(
        &pty,
        "app_crew_request_runs_the_steps_its_orchestrator_writes",
    )?;
    let limiter = scenario.repo.git(&["show", "main:rate-limit.md"])?;
    assert!(limiter.contains("a token bucket per API key"), "{limiter}");

    // `s` in the workflow view keeps the script the orchestrator wrote as a workflow of yours.
    pty.press(Key::Ctrl('t'))?;
    pty.wait_for(r"esc back · s save", WAIT)?;
    pty.press(Key::Char('s'))?;
    pty.wait_for(r"Saved the script this run followed as", WAIT)?;
    let yours = scenario.repo.home().join("workflows");
    let saved = wait_until("the saved script", || {
        Ok(std::fs::read_dir(&yours)?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|extension| extension == "js")))
    })?;
    let script = std::fs::read_to_string(&saved)?;
    assert!(script.contains("rate-limit-per-key"), "{script}");
    Ok(())
}

/// A workflow of your own on the real screen: `/workflow new` writes it, `/workflow try` shows
/// what it would run on a card, and Esc closes the card with nothing run.
#[test]
fn app_workflow_of_your_own_is_made_tried_and_put_in_use() -> anyhow::Result<()> {
    let scenario = scenario_with(&shipped_config())?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, "/workflow new spec-first")?;
    pty.wait_for(
        r"Wrote spec-first · \.argonauts/workflows/spec-first\.toml",
        WAIT,
    )?;
    let file = scenario
        .repo
        .root()
        .join(".argonauts/workflows/spec-first.toml");
    let text = std::fs::read_to_string(&file)?;
    assert!(text.contains("name  = \"spec-first\""), "{text}");
    assert!(
        scenario
            .repo
            .root()
            .join(".argonauts/workflows/README.md")
            .exists(),
        "the README that explains each key is written beside it"
    );

    submit(&mut pty, "/workflow try spec-first add due dates")?;
    let screen = pty.wait_for(r"Run it now\? y / N", WAIT)?.text();
    let order = [
        "spec-first for \"add due dates\" · a preview, nothing runs yet",
        "1 Plan",
        "2 Build",
        "3 Review",
        "Run it now? y / N",
    ];
    let found: Vec<Option<usize>> = order.iter().map(|needle| screen.find(needle)).collect();
    assert!(
        found.iter().all(Option::is_some) && found.windows(2).all(|pair| pair[0] < pair[1]),
        "expected {order:?} in order:\n{screen}"
    );
    scenario.shot(
        &pty,
        "app_workflow_of_your_own_is_made_tried_and_put_in_use",
    )?;

    // Only its own key answers it: n, or y to run it.
    pty.press(Key::Char('n'))?;
    pty.wait_for(
        r"Spec-first preview for 'add due dates' · nothing ran",
        WAIT,
    )?;
    assert!(run_dirs(&scenario).is_empty(), "nothing ran");

    // `/workflow spec-first` makes it the workflow in use, and the rule above the input says so.
    submit(&mut pty, "/workflow spec-first")?;
    answer_scope(&scenario, &mut pty, 's', None)?;
    let changed = wait_until("the workflow in use to change", || {
        Ok(setting_changes(&scenario)?
            .into_iter()
            .find(|(key, ..)| key == "workflow"))
    })?;
    assert_eq!(changed.1, "\"spec-first\"");
    // The input's rule names the workflow in use first, then the main model.
    pty.wait_for(r"─ Spec-first · \S.* ─", WAIT)?;
    Ok(())
}

/// `/workflow edit <name>` on the real screen: the page lists the steps in order, a step
/// opens to its keys, a change is checked and saved to the file, and one that would stop the
/// steps from running is refused with the reason.
#[test]
fn app_workflow_page_changes_the_file_and_refuses_what_would_not_run() -> anyhow::Result<()> {
    let scenario = scenario_with(&shipped_config())?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, "/workflow new spec-first")?;
    pty.wait_for(
        r"Wrote spec-first · \.argonauts/workflows/spec-first\.toml",
        WAIT,
    )?;
    let file = scenario
        .repo
        .root()
        .join(".argonauts/workflows/spec-first.toml");

    submit(&mut pty, "/workflow edit spec-first")?;
    let screen = pty.wait_for(r"the file is valid", WAIT)?.text();
    let order = [
        "Workflows › Spec-first",
        "who would take it",
        "the steps run exactly as written · no goal",
        "❯ 1 Plan",
        "2 Build",
        "3 Review",
        "✓ the file is valid",
        "esc back",
    ];
    let found: Vec<Option<usize>> = order.iter().map(|needle| screen.find(needle)).collect();
    assert!(
        found.iter().all(Option::is_some) && found.windows(2).all(|pair| pair[0] < pair[1]),
        "expected {order:?} in order:\n{screen}"
    );

    // Build opens to its keys, and For each goes from task to unset and back.
    pty.press(Key::Down)?;
    pty.press(Key::Enter)?;
    pty.wait_for(r"Sends back to", WAIT)?;
    for _ in 0..4 {
        pty.press(Key::Down)?;
    }
    pty.wait_for(r"❯ +For each", WAIT)?;
    scenario.shot(
        &pty,
        "app_workflow_page_changes_the_file_and_refuses_what_would_not_run",
    )?;
    let written = || -> anyhow::Result<usize> {
        Ok(std::fs::read_to_string(&file)?.matches("for_each").count())
    };
    assert_eq!(written()?, 2, "Build and Review each say for_each");
    pty.press(Key::Right)?;
    pty.wait_for(r"Saved\.", WAIT)?;
    assert_eq!(written()?, 1, "Build no longer says it");
    pty.press(Key::Right)?;
    wait_until("for_each to be written again", || {
        Ok((written()? == 2).then_some(()))
    })?;

    // Typed keys take a line. A count that is no number is refused, and the file stays.
    pty.press(Key::Down)?;
    pty.wait_for(r"❯ +Count", WAIT)?;
    pty.press(Key::Enter)?;
    pty.send("many")?;
    pty.press(Key::Enter)?;
    pty.wait_for(
        r"Not saved: count takes a whole number, 1 or more, or all",
        WAIT,
    )?;
    assert!(!std::fs::read_to_string(&file)?.contains("count ="));
    pty.press(Key::Enter)?;
    pty.send("2")?;
    pty.press(Key::Enter)?;
    pty.wait_for(r"2 per task", WAIT)?;
    assert!(std::fs::read_to_string(&file)?.contains("count = 2"));

    pty.press(Key::Esc)?;
    pty.wait_for(
        r"Wrote spec-first · \.argonauts/workflows/spec-first\.toml",
        WAIT,
    )?;
    Ok(())
}

/// A workflow of your own whose steps run exactly as written.
const LIMITER_FIRST: &str = r#"name  = "limiter-first"
about = "Plan, build each task, have another provider review it."
adapt = false

[[step]]
name = "Plan"
role = "orchestrator"
does = "Split the request into tasks, as the workflow file says: {request}"

[[step]]
name     = "Build"
role     = "workers"
for_each = "task"

[[step]]
name         = "Review"
role         = "reviewers"
for_each     = "task"
does         = "Review the rate limiter against the request: {request}"
send_back_to = "Build"
"#;

/// `/<name> <request>` runs one request with a workflow of your own: its steps are the phases,
/// each agent goes to the CLI its role names, and the workflow in use stays as it was.
#[test]
fn app_your_own_workflow_runs_one_request_by_its_steps_as_written() -> anyhow::Result<()> {
    let scenario = scenario_with(&shipped_config())?;
    scenario
        .repo
        .write(".argonauts/workflows/limiter-first.toml", LIMITER_FIRST)?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, &format!("/limiter-first {}", requests::CREW_TASK))?;

    let run_dir = wait_run_dir(&scenario)?;
    wait_merged(&run_dir)?;
    let started = run_started(&run_dir)?;
    assert_eq!(started.mode, RunMode::Crew);
    assert_eq!(started.workflow, "limiter-first");

    let events = run_events(&run_dir)?;
    let sessions: Vec<(String, String)> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => Some((
                started.label.clone().unwrap_or_default(),
                started.worker.worker.clone(),
            )),
            _ => None,
        })
        .collect();
    // The workflow's own agents come first. The run review and the closing answer follow.
    assert_eq!(
        sessions[..3],
        [
            ("plan".to_owned(), "claude-code".to_owned()),
            ("build:add-a-token".to_owned(), "claude-code".to_owned()),
            ("review:add-a-token".to_owned(), "codex".to_owned()),
        ],
        "{sessions:?}"
    );
    let report = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            RunEventKind::Workflow(WorkflowEvent::Reported(report)) => Some(report.clone()),
            _ => None,
        })
        .expect("the runner reported");
    assert_eq!(report.workflow.name, "limiter-first");
    assert_eq!(report.workflow.status, SubagentStatus::Done);
    let phases: Vec<&str> = report
        .workflow
        .phases
        .iter()
        .map(|phase| phase.title.as_str())
        .collect();
    assert_eq!(phases, ["Plan", "Build", "Review"]);
    assert!(
        report
            .workflow
            .agents
            .iter()
            .all(|agent| agent.state == VendorAgentState::Done),
        "{:?}",
        report.workflow.agents
    );

    pty.wait_for(r"Approved merging the run into main · your rules", WAIT)?;
    let screen = pty.wait_for(r"✻ \w+ for \d", WAIT)?.text();
    let row = screen
        .lines()
        .find(|line| line.trim_start().starts_with("build:add-a-token  "))
        .unwrap_or_default();
    assert!(
        row.trim_end().ends_with("approved"),
        "the merge table shows the workflow's own review: {row}\n{screen}"
    );
    scenario.shot(
        &pty,
        "app_your_own_workflow_runs_one_request_by_its_steps_as_written",
    )?;
    let limiter = scenario.repo.git(&["show", "main:rate-limit.md"])?;
    assert!(limiter.contains("a token bucket per API key"), "{limiter}");
    // The workflow in use is still the one your settings name.
    let config = std::fs::read_to_string(scenario.repo.global_config())?;
    assert!(!config.contains("limiter-first"), "{config}");
    Ok(())
}
