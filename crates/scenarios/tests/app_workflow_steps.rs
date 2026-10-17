//! A crew request under the settings as they ship: the workflow in use is council, whose
//! orchestrator writes the steps for the request as a script, and the runner runs them. Each
//! agent goes to the CLI its role names, the run reports the script's own name, phases and
//! agents, and the merge approval follows.

mod app_support;

use app_support::*;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::session::{CliAgentState, SessionEvent, SubagentStatus};
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

/// Whether the Phases pane's row of `phase` shows `count` after its name.
fn phase_counts(view: &str, phase: &str, count: &str) -> bool {
    view.lines()
        .filter_map(|line| line.split('\u{2502}').nth(1))
        .any(|row| {
            let words: Vec<&str> = row.split_whitespace().collect();
            words.windows(2).any(|pair| pair == [phase, count])
        })
}

/// The first row of `screen` that holds `words`, without the blanks at its end.
fn line_with(screen: &str, words: &str) -> String {
    screen
        .lines()
        .find(|line| line.contains(words))
        .unwrap_or_else(|| panic!("no row says {words:?}:\n{screen}"))
        .trim_end()
        .to_owned()
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
                started.worker.agent.clone(),
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
    let agents: Vec<(&str, Option<u32>, CliAgentState)> = report
        .workflow
        .agents
        .iter()
        .map(|agent| (agent.label.as_str(), agent.phase, agent.state))
        .collect();
    assert_eq!(
        agents,
        [
            ("build:rate-limit", Some(1), CliAgentState::Done),
            ("review:rate-limit", Some(2), CliAgentState::Done),
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

    // One finished run reads the same wherever it is drawn. Its closing row in the
    // conversation counts the workflow's agents: the builder, the reviewer, and the merger
    // when it wrote the commit message in time. The agent that wrote the steps is none of
    // them. Its row under the input, kept a while after the end, says the same.
    let ended = pty
        .wait_for(
            r"Completed in \S+ · \d+ agents · \S+ tokens · merged onto main",
            WAIT,
        )?
        .text();
    let closing = line_with(&ended, "Completed in ");
    let counts: Vec<&str> = closing.split(" · ").collect();
    let (agents, tokens) = (counts[1].trim_end_matches(" agents"), counts[2]);
    assert!(agents == "2" || agents == "3", "{closing}");
    let row = line_with(&ended, "\u{25b0}");
    assert!(row.contains(&format!("  {agents}/{agents} · ")), "{row}");
    assert!(
        row.ends_with(&format!(" · \u{2193} {tokens} · merged onto main")),
        "{row}\n{closing}"
    );

    // Its view counts the same in its header, and its phases add up to that. It names the
    // workflow in use before what the run does, and says what the merge changed.
    pty.press(Key::Ctrl('t'))?;
    let view = pty.wait_for(r"esc back · s save", WAIT)?.visible_text();
    let header = line_with(&view, &format!("  {agents}/{agents} agents · "));
    // A council is the mode itself, named before what the run does.
    assert!(header.starts_with(" Council · "), "{view}");
    assert!(header.ends_with(" · done"), "{view}");
    let merged = line_with(&view, "Merged onto main · ");
    assert!(
        merged.starts_with(" \u{2714} Merged onto main · "),
        "{view}"
    );
    assert!(merged.ends_with(" · 1 file +1 \u{2212}0"), "{view}");
    // A phase's count sits at the Phases pane's edge, however long its name.
    assert!(phase_counts(&view, "Build", "1/1"), "{view}");
    assert!(phase_counts(&view, "Review", "1/1"), "{view}");
    let in_phases: u32 = view
        .lines()
        .filter_map(|line| line.split('\u{2502}').nth(1))
        .filter_map(|phase| phase.split_whitespace().last()?.split_once('/'))
        .filter_map(|(_, total)| total.parse::<u32>().ok())
        .sum();
    assert_eq!(in_phases.to_string(), agents, "{view}");
    assert!(!view.contains("plan:orchestrator"), "{view}");

    // `s` in the workflow view keeps the script the orchestrator wrote as a workflow of yours.
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

/// The same request in a folder that is no git repository. The run's view names the workflow
/// in use, says what was written into the folder and what puts it back, and its Merge phase
/// lists the folder's own step, with why no commit message was written under it.
#[test]
fn app_crew_run_in_a_folder_without_git_says_what_it_wrote() -> anyhow::Result<()> {
    let scenario = scenario_with(&shipped_config())?;
    std::fs::remove_dir_all(scenario.repo.root().join(".git"))?;
    let mut pty = scenario.launch_sized(&[], 36, 124)?;
    wait_ready(&scenario, &pty)?;
    submit(&mut pty, requests::CREW_TASK)?;

    let run_dir = wait_run_dir(&scenario)?;
    wait_merged(&run_dir)?;
    let written = std::fs::read_to_string(scenario.repo.root().join("rate-limit.md"))?;
    assert!(written.contains("a token bucket per API key"), "{written}");
    pty.wait_for(
        r"Completed in \S+ · \d+ agents · \S+ tokens · written into your folder",
        WAIT,
    )?;
    pty.wait_for(r"✻ \w+ for \d", WAIT)?;

    pty.press(Key::Ctrl('t'))?;
    let view = pty.wait_for(r"esc back · s save", WAIT)?.visible_text();
    let header = line_with(&view, "Council · ");
    // A council is the mode itself, named before what the run does.
    assert!(header.starts_with(" Council · "), "{view}");
    assert!(
        header.contains("/2 agents · ") && header.ends_with(" · done"),
        "{view}"
    );
    assert_eq!(
        line_with(&view, "/rewind puts it back"),
        " \u{2714} Written into your folder · 1 file +1 \u{2212}0 · /rewind puts it back",
        "{view}"
    );
    // The view opens on the last phase, the merge, whose steps fill the pane beside it.
    let rows: Vec<&str> = view.lines().collect();
    let step = rows
        .iter()
        .position(|row| row.contains("write into your folder"))
        .unwrap_or_else(|| panic!("no row for the folder's step:\n{view}"));
    assert!(
        rows[step].contains("\u{2714} write into your folder  history kept by Argonauts · 1 task"),
        "{view}"
    );
    let why = rows[step + 1..]
        .iter()
        .find(|row| row.contains("no model writes"))
        .unwrap_or_else(|| panic!("no row says why no message was written:\n{view}"));
    assert!(
        why.contains("no model writes a commit message here: only /rewind reads the history"),
        "{view}"
    );
    scenario.shot(
        &pty,
        "app_crew_run_in_a_folder_without_git_says_what_it_wrote",
    )?;
    Ok(())
}

/// `/templates` on a small terminal. Its question lists ten of the names at a time, so the
/// title, the pointer and the keys are all on the screen, going down past the last name
/// shown brings up the next, and Enter copies the picked one into this folder.
#[test]
fn app_templates_asks_which_one_on_a_small_screen_and_copies_it() -> anyhow::Result<()> {
    let scenario = scenario_with(&shipped_config())?;
    let mut pty = scenario.launch_sized(&[], 24, 80)?;
    wait_ready(&scenario, &pty)?;
    submit(&mut pty, "/templates")?;

    let asked = pty.wait_for(r"1–10 of \d+ ↓", WAIT)?.visible_text();
    let rows: Vec<&str> = asked.lines().map(str::trim_end).collect();
    let at = |words: &str| {
        rows.iter()
            .position(|row| row.contains(words))
            .unwrap_or_else(|| panic!("no row says {words:?}:\n{asked}"))
    };
    let title = at("◆ Copy which one?");
    assert_eq!(at("❯ 1.  Council"), title + 2, "{asked}");
    assert!(rows[title + 2].ends_with("workflow · in use"), "{asked}");
    assert_eq!(at("  10. "), title + 11, "{asked}");
    let keys = at("Enter to choose · Esc to cancel");
    assert_eq!(keys, title + 13, "{asked}");
    assert!(!asked.contains("  11. "), "{asked}");
    // The end of what it listed is still above the question.
    assert!(
        rows[..title].iter().any(|row| row.contains("└─ mods/")),
        "{asked}"
    );

    argonauts_scenarios::past_the_question_guard();
    for _ in 0..10 {
        pty.press(Key::Down)?;
    }
    let moved = pty.wait_for(r"↑ 6–15 of \d+ ↓", WAIT)?.visible_text();
    assert!(moved.contains("❯ 11. "), "{moved}");
    assert!(moved.contains("◆ Copy which one?"), "{moved}");
    assert!(!moved.contains("  5.  "), "{moved}");

    // Back up to the second name, and into this folder.
    for _ in 0..9 {
        pty.press(Key::Up)?;
    }
    pty.wait_for(r"❯ 2\.  Orchestrator", WAIT)?;
    pty.press(Key::Enter)?;
    let place = pty
        .wait_for(r"◆ Copy Orchestrator into", WAIT)?
        .visible_text();
    assert!(
        line_with(&place, "❯ 1. This folder").ends_with("   .argonauts/"),
        "{place}"
    );
    argonauts_scenarios::past_the_question_guard();
    pty.press(Key::Enter)?;
    let copy = scenario
        .repo
        .root()
        .join(".argonauts/workflows/orchestrator.toml");
    wait_until("the copy to be written", || Ok(copy.exists().then_some(())))?;
    scenario.shot(
        &pty,
        "app_templates_asks_which_one_on_a_small_screen_and_copies_it",
    )?;
    Ok(())
}

/// A workflow of your own on the real screen: `/workflow new` writes it, `/workflow try` shows
/// what it would run on a question, and Esc closes the question with nothing run.
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
    argonauts_scenarios::past_the_question_guard();
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
    // The page has the answer too once Build's row says one per task again. An answer that
    // came later would close the line the next keys open, and what was typed would be lost.
    pty.wait_for(r"2 Build +Workers .*one per task", WAIT)?;

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
role     = "worker"
for_each = "task"

[[step]]
name         = "Review"
role         = "reviewer"
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
                started.worker.agent.clone(),
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
            .all(|agent| agent.state == CliAgentState::Done),
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
