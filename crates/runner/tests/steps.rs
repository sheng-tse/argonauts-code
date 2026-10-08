//! Three workflow files, run as written against a host that answers from a table.

mod support;

use std::sync::atomic::{AtomicU32, Ordering};

use argonauts_core::events::session::{SubagentStatus, VendorAgentState};
use argonauts_core::settings::crew::CrewRole;
use argonauts_runner::{Checked, Script, WorkflowFile, built_in, run};
use serde_json::json;
use support::{Fake, accept, reject, request, tasks, text};

fn steps(text: &str) -> Script {
    let file = WorkflowFile::parse(text)
        .unwrap_or_else(|err| panic!("{err}"))
        .expect("a file with steps");
    Script::from_steps(&file)
}

#[tokio::test]
async fn orchestrator_plans_builds_each_task_and_sends_a_rejected_one_back() {
    let reviews = AtomicU32::new(0);
    let host = Fake::new(move |call| match call.phase.as_deref() {
        Some("Plan") => tasks(&["Add a due field", "Show due dates"]),
        Some("Build") => text("built"),
        // The first review of the first task finds a gap.
        Some("Review") => match reviews.fetch_add(1, Ordering::Relaxed) {
            0 => reject(&["no test for an empty date"]),
            _ => accept(),
        },
        _ => text("signed off"),
    });
    let script = steps(built_in("orchestrator").expect("built in"));
    let ended = run(&script, request("add due dates", 2, 2), &host).await;

    assert_eq!(
        host.asked(),
        [
            "plan orchestrator 1",
            "build:add-a-due workers 1",
            "build:show-due-dates workers 1",
            "review:add-a-due reviewers 1",
            // A label is 20 characters at most, like every label in a run.
            "review:show-due-date reviewers 1",
            "build:add-a-due workers 2",
            "review:add-a-due reviewers 2",
            "sign-off judge 1",
        ]
    );
    let again = host.call("build:add-a-due", 2);
    assert!(
        again
            .prompt
            .contains("Sent back by Review:\n- no test for an empty date"),
        "{}",
        again.prompt
    );
    assert_eq!(again.task.as_deref(), Some("add-a-due"));
    let plan = host.call("plan", 1);
    assert!(
        plan.prompt.starts_with("Split the request into tasks"),
        "{}",
        plan.prompt
    );
    assert!(
        plan.prompt.contains("The request:\nadd due dates"),
        "{}",
        plan.prompt
    );
    assert!(
        plan.schema.is_some(),
        "the planner answers with a task list"
    );
    let build = host.call("build:show-due-dates", 1);
    assert!(
        build
            .prompt
            .contains("The task: Show due dates\nDone when:\n- Show due dates works")
    );

    let result = ended.result.expect("it ran to its end");
    assert_eq!(result["accepted"], json!(true));
    assert_eq!(result["result"], json!("signed off"));
    assert_eq!(result["tasks"][1]["id"], json!("show-due-dates"));
    assert_eq!(ended.agents_run, 8);
}

#[tokio::test]
async fn every_workflow_reports_its_own_name_phases_and_agents() {
    let host = Fake::new(|call| match call.phase.as_deref() {
        Some("Plan") => tasks(&["One"]),
        Some("Review") => accept(),
        _ => text("done"),
    });
    let script = steps(built_in("orchestrator").expect("built in"));
    let ended = run(&script, request("x", 1, 1), &host).await;

    let report = ended.report;
    assert_eq!(report.id, "wf-1");
    assert_eq!(report.name, "orchestrator");
    assert_eq!(report.status, SubagentStatus::Done);
    let phases: Vec<(u32, &str)> = report
        .phases
        .iter()
        .map(|phase| (phase.index, phase.title.as_str()))
        .collect();
    assert_eq!(
        phases,
        [(1, "Plan"), (2, "Build"), (3, "Review"), (4, "Sign off")]
    );
    assert_eq!(report.phases[1].detail, "workers · one per task");
    let agents: Vec<String> = report
        .agents
        .iter()
        .map(|agent| {
            assert_eq!(agent.state, VendorAgentState::Done);
            let model = agent.model.as_deref().unwrap_or("?");
            let phase = agent.phase.unwrap_or(0);
            format!(
                "{} {} phase {phase} {model} {}",
                agent.index, agent.label, agent.tokens
            )
        })
        .collect();
    assert_eq!(
        agents,
        [
            "1 plan phase 1 GPT 5.6 1000",
            "2 build:one phase 2 GPT 5.6 1000",
            "3 review:one phase 3 GPT 5.6 1000",
            "4 sign-off phase 4 GPT 5.6 1000",
        ]
    );
    assert_eq!((report.tokens, report.tool_calls), (4_000, 8));
    assert_eq!(report.count(2), Some((1, 1)));

    // The first report already lists every phase, and a row is queued, then running, then done.
    let reports = host.reports.lock().unwrap();
    assert_eq!(reports[0].phases.len(), 4);
    assert!(reports[0].agents.is_empty());
    let states: Vec<VendorAgentState> = reports
        .iter()
        .filter_map(|report| report.agents.first().map(|agent| agent.state))
        .collect();
    assert_eq!(
        states[..3],
        [
            VendorAgentState::Queued,
            VendorAgentState::Running,
            VendorAgentState::Done
        ]
    );
}

#[tokio::test]
async fn council_has_everyone_propose_and_challenge_then_builds_and_cross_reviews() {
    let host = Fake::new(|call| match call.phase.as_deref() {
        Some("Propose") => text(&format!("plan from seat {}", call.seat)),
        Some("Challenge") => text("the other plan skips migration"),
        Some("Decide") => tasks(&["Migrate"]),
        Some("Cross-review") => accept(),
        _ => text("ok"),
    });
    let script = steps(built_in("council").expect("built in"));
    let ended = run(&script, request("move to v2", 2, 1), &host).await;

    assert_eq!(
        host.asked(),
        [
            "propose:1 workers 1",
            "propose:2 workers 1",
            "challenge:1 workers 1",
            "challenge:2 workers 1",
            "decide judge 1",
            "build:migrate workers 1",
            "cross-review:migrate workers 1",
            "sign-off judge 1",
        ]
    );
    let propose = host.call("propose:2", 1);
    assert!(propose.independent);
    assert_eq!(propose.seat, 1);
    let challenge = host.call("challenge:1", 1);
    assert!(
        challenge
            .prompt
            .contains("From Propose:\n1) plan from seat 0\n2) plan from seat 1"),
        "{}",
        challenge.prompt
    );
    let decide = host.call("decide", 1);
    assert!(
        decide.prompt.contains("From Challenge:"),
        "{}",
        decide.prompt
    );
    assert!(decide.schema.is_some());
    let review = host.call("cross-review:migrate", 1);
    assert!(review.not_the_author);
    assert_eq!(review.role, CrewRole::Workers);
    assert_eq!(ended.result.expect("ended")["accepted"], json!(true));
}

#[tokio::test]
async fn spec_first_keeps_building_until_its_goal_holds_and_revises_until_both_accept() {
    let reviews = AtomicU32::new(0);
    let checks = AtomicU32::new(0);
    let host = Fake::new(move |call| match call.phase.as_deref() {
        Some("Spec") => tasks(&["Due dates"]),
        // Of the first pair of reviews, the second asks for a change.
        Some("Review") => match reviews.fetch_add(1, Ordering::Relaxed) {
            1 => reject(&["the spec says ISO dates"]),
            _ => accept(),
        },
        Some(phase) => text(&format!("{phase} done")),
        None => text("?"),
    })
    .checking(move |_| match checks.fetch_add(1, Ordering::Relaxed) {
        0 => Checked {
            holds: false,
            failed: "test_due_date fails".to_owned(),
            stop: false,
        },
        _ => Checked {
            holds: true,
            ..Checked::default()
        },
    });
    let script = steps(include_str!("fixtures/spec-first.toml"));
    let ended = run(&script, request("add due dates", 1, 2), &host).await;

    assert_eq!(
        host.asked(),
        [
            "spec orchestrator 1",
            "build:due-dates workers 1",
            "build:due-dates workers 2",
            "review:due-dates:1 reviewers 1",
            "review:due-dates:2 reviewers 1",
            "revise:due-dates workers 1",
            "review:due-dates:1 reviewers 2",
            "review:due-dates:2 reviewers 2",
            "sign-off judge 1",
        ]
    );
    let spec = host.call("spec", 1);
    assert!(
        spec.prompt
            .starts_with("Write a short spec and acceptance tests for: add due dates."),
        "{}",
        spec.prompt
    );
    let first = host.call("build:due-dates", 1);
    assert!(
        first.prompt.starts_with("Implement Due dates."),
        "{}",
        first.prompt
    );
    assert_eq!(first.until.as_deref(), Some("its acceptance tests pass"));
    let pressed = host.call("build:due-dates", 2);
    assert!(
        pressed
            .prompt
            .contains("Not true yet: its acceptance tests pass\ntest_due_date fails"),
        "{}",
        pressed.prompt
    );
    let asked = host.checks.lock().unwrap().clone();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].condition, "its acceptance tests pass");
    assert_eq!(asked[0].step.as_deref(), Some("Build"));
    assert_eq!(asked[0].task.as_deref(), Some("due-dates"));
    assert_eq!((asked[0].attempt, asked[1].attempt), (Some(1), Some(2)));
    let revise = host.call("revise:due-dates", 1);
    assert!(
        revise
            .prompt
            .contains("Sent back by Review:\n- the spec says ISO dates"),
        "{}",
        revise.prompt
    );
    assert_eq!(host.call("review:due-dates:2", 1).seat, 1);
    assert_eq!(
        host.call("sign-off", 1).model.as_deref(),
        Some("codex/gpt-5.6")
    );
    assert_eq!(ended.result.expect("ended")["accepted"], json!(true));
    // A step nothing sent work back to never shows an agent.
    let report = ended.report;
    assert_eq!(report.count(4), Some((1, 1)));
    assert_eq!(report.phases[3].title, "Revise");
}

#[tokio::test]
async fn max_rounds_ends_a_loop_that_never_accepts_and_says_so() {
    let host = Fake::new(|call| match call.phase.as_deref() {
        Some("Review") => reject(&["still wrong"]),
        _ => text("tried"),
    });
    let script = steps(
        "name = \"strict\"\n[[step]]\nname = \"Build\"\nrole = \"workers\"\nmax_rounds = 2\n\
         [[step]]\nname = \"Review\"\nrole = \"reviewers\"\nsend_back_to = \"Build\"\n\
         [[step]]\nname = \"Sign off\"\nrole = \"judge\"\n",
    );
    let ended = run(&script, request("fix it", 1, 1), &host).await;
    assert_eq!(
        host.asked(),
        [
            "build workers 1",
            "review reviewers 1",
            "build workers 2",
            "review reviewers 2",
            "sign-off judge 1",
        ]
    );
    let result = ended.result.expect("ended");
    assert_eq!(result["accepted"], json!(false));
    assert_eq!(
        result["notes"],
        json!(["Build stopped after 2 rounds without acceptance"])
    );
    assert_eq!(
        host.logs.lock().unwrap().as_slice(),
        ["Build: stopped after 2 rounds without acceptance"]
    );
}

#[tokio::test]
async fn a_reviewer_that_never_answers_ends_the_task_instead_of_sending_it_back() {
    use argonauts_runner::AgentEnd;
    let host = Fake::new(|call| match call.phase.as_deref() {
        Some("Plan") => tasks(&["Add a due field"]),
        Some("Review") => AgentEnd::failed("the reviewer's CLI is out of plan usage"),
        _ => text("built"),
    });
    let script = steps(built_in("orchestrator").expect("built in"));
    let ended = run(&script, request("add due dates", 1, 1), &host).await;

    let builds = host
        .asked()
        .iter()
        .filter(|asked| asked.starts_with("build:"))
        .count();
    assert_eq!(builds, 1, "{:?}", host.asked());
    let result = ended.result.expect("ended");
    assert_eq!(result["accepted"], json!(false));
    assert_eq!(
        result["notes"],
        json!([
            "Add a due field: Review gave no answer, so the work was neither accepted nor sent back"
        ])
    );
}

#[tokio::test]
async fn a_loop_with_no_max_rounds_stops_after_two_send_backs() {
    let host = Fake::new(|call| match call.phase.as_deref() {
        Some("Review") => reject(&["still wrong"]),
        _ => text("tried"),
    });
    let script = steps(
        "name = \"loose\"\n[[step]]\nname = \"Build\"\nrole = \"workers\"\n\
         [[step]]\nname = \"Review\"\nrole = \"reviewers\"\nsend_back_to = \"Build\"\n",
    );
    let ended = run(&script, request("fix it", 1, 1), &host).await;
    assert_eq!(
        host.asked(),
        [
            "build workers 1",
            "review reviewers 1",
            "build workers 2",
            "review reviewers 2",
            "build workers 3",
            "review reviewers 3",
        ]
    );
    let result = ended.result.expect("ended");
    assert_eq!(result["accepted"], json!(false));
    assert_eq!(
        result["notes"],
        json!(["Review stopped after sending the work back 2 times without accepting it"])
    );
}

#[tokio::test]
async fn a_goal_on_the_file_goes_round_until_it_holds() {
    let rounds = AtomicU32::new(0);
    let host = Fake::new(|_| text("did it")).checking(move |check| {
        assert_eq!(check.condition, "every test passes");
        assert_eq!(check.step, None);
        Checked {
            holds: rounds.fetch_add(1, Ordering::Relaxed) == 1,
            failed: "2 tests fail".to_owned(),
            stop: false,
        }
    });
    let script = steps(
        "name = \"until-green\"\ngoal = \"every test passes\"\n\
         [[step]]\nname = \"Fix\"\nrole = \"workers\"\n",
    );
    let ended = run(&script, request("make it green", 1, 1), &host).await;
    assert_eq!(host.asked(), ["fix workers 1", "fix workers 1"]);
    let calls = host.calls.lock().unwrap();
    assert!(
        calls[1]
            .prompt
            .contains("From the goal:\nNot true yet: every test passes\n2 tests fail"),
        "{}",
        calls[1].prompt
    );
    assert_eq!(ended.result.expect("ended")["accepted"], json!(true));
}

#[tokio::test]
async fn best_of_n_tries_side_by_side_and_the_judge_keeps_one() {
    let host = Fake::new(|call| match call.phase.as_deref() {
        Some("Pick") => support::said(json!({"keep": 2, "why": "the only one with tests"})),
        _ => text(&format!("attempt {}", call.seat + 1)),
    });
    let script = steps(built_in("best-of-n").expect("built in"));
    let ended = run(&script, request("speed up search", 3, 1), &host).await;
    assert_eq!(
        host.asked(),
        [
            "try:1 workers 1",
            "try:2 workers 1",
            "try:3 workers 1",
            "pick judge 1"
        ]
    );
    let kept = host.kept.lock().unwrap().clone();
    assert_eq!(kept.len(), 1);
    assert_eq!((kept[0].step.as_str(), kept[0].seat), ("Try", 1));
    assert_eq!(kept[0].label, "try:2");
    assert_eq!(kept[0].why.as_deref(), Some("the only one with tests"));
    assert!(ended.result.is_ok());
}

#[tokio::test]
async fn a_fixed_task_list_replaces_the_planners() {
    let host = Fake::new(|call| match call.phase.as_deref() {
        Some("Plan") => tasks(&["Something else entirely"]),
        Some("Review") => accept(),
        _ => text("ok"),
    });
    let script = steps(built_in("orchestrator").expect("built in"));
    let mut input = request("three things", 1, 1);
    input.args["tasks"] = json!(["Rename the flag", "Update the docs"]);
    run(&script, input, &host).await;
    let asked = host.asked();
    assert_eq!(asked[1], "build:rename-the-fla workers 1");
    assert_eq!(asked[2], "build:update-the-doc workers 1");
    // A long title still fits a row: three words of it, cut to the row's width.
    let long = steps(built_in("orchestrator").expect("built in"));
    let wordy = Fake::new(|call| match call.phase.as_deref() {
        Some("Plan") => tasks(&["Internationalization of the settings screen"]),
        Some("Review") => accept(),
        _ => text("ok"),
    });
    run(&long, request("i18n", 1, 1), &wordy).await;
    assert_eq!(wordy.asked()[1], "build:internationali workers 1");
    assert_eq!(
        wordy.call("build:internationali", 1).task.as_deref(),
        Some("internationalization-of-the")
    );
    assert!(host.call("plan", 1).prompt.contains(
        "The tasks are fixed. Plan these and no others:\n- Rename the flag\n- Update the docs"
    ));
}
