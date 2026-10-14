//! The commands the agent CLIs share, through the real controller loop with a fake main agent:
//! each works the same whatever CLI the main agent is.

use argonauts_core::NoteLevel;
use argonauts_core::app::{
    AppEvent, AppMode, ChoiceKind, ConversationEvent, TurnReply, UiCommand, ViewData,
};
use argonauts_core::events::goal::GoalAction;
use argonauts_core::events::run::RunMode;
use argonauts_core::input::SlashCommand;
use argonauts_core::names::ConversationId;
use argonauts_testkit::{FakeRunner, FakeSession};
use serde_json::json;

use crate::controller::test_support::{Call, CliOptions, TestDriver};
use crate::conversation_store::ConversationStore;

fn answer(text: &str) -> FakeSession {
    FakeSession::structured(json!({"kind": "answer", "answer_markdown": text, "estimate": null}))
}

/// For the steps that only answer: a side question, a name, a summary, a review.
fn said(text: &str) -> FakeSession {
    FakeSession::structured(json!({"answer_markdown": text}))
}

fn change(text: &str) -> FakeSession {
    FakeSession::structured(json!({
        "kind": "change",
        "answer_markdown": text,
        "estimate": {
            "task_type": "api-feature",
            "summary": "Add caching",
            "files_likely": ["src/cache.rs"],
            "file_count": 2,
            "risk_labels": [],
            "tests_exist": true,
            "unknowns": []
        }
    }))
}

async fn slash(driver: &TestDriver, command: SlashCommand) {
    driver.send(UiCommand::Slash { command }).await;
}

fn finished(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Conversation {
            event: ConversationEvent::TurnFinished { .. } | ConversationEvent::TurnFailed { .. }
        }
    )
}

fn idle(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Mode {
            mode: AppMode::Idle
        }
    )
}

/// The finished turn's answer, once the app is idle again.
async fn turn_done(driver: &mut TestDriver) -> String {
    let event = driver.until(finished).await;
    driver.until(idle).await;
    match event {
        AppEvent::Conversation {
            event:
                ConversationEvent::TurnFinished {
                    reply: TurnReply::Answer { markdown },
                    ..
                },
        } => markdown,
        other => panic!("{other:?}"),
    }
}

async fn notice(driver: &mut TestDriver, part: &str) -> String {
    let event = driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
        .await;
    match event {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
}

/// A notice already read, such as one sent while a turn finished.
fn noticed(driver: &TestDriver, part: &str) -> bool {
    driver
        .seen()
        .iter()
        .any(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
}

async fn report(driver: &mut TestDriver, title: &str) -> Vec<String> {
    let event = driver
        .until(|event| matches!(event, AppEvent::Report { title: t, .. } if t == title))
        .await;
    match event {
        AppEvent::Report { lines, .. } => lines,
        _ => unreachable!(),
    }
}

fn prompt(driver: &TestDriver, index: usize) -> String {
    driver.agent.specs()[index]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect()
}

fn journal(driver: &TestDriver, conversation: u32) -> Vec<ConversationEvent> {
    ConversationStore::open(
        &driver.env.conversation_dir(),
        &ConversationId::numbered(conversation),
    )
    .events()
    .unwrap()
}

/// One turn asked and answered, in a resumable CLI session.
async fn one_turn(driver: &mut TestDriver) {
    driver
        .agent
        .push(answer("The cookie is set twice. Then it redirects.").with_cli_session("v-1"));
    driver.submit("why does login loop?").await;
    turn_done(driver).await;
}

#[tokio::test]
async fn btw_answers_from_its_own_session_and_stays_out_of_the_conversation() {
    let mut driver = TestDriver::start().await;
    one_turn(&mut driver).await;
    driver
        .agent
        .push(said("It's in src/auth.rs.").with_cli_session("v-side"));
    slash(
        &driver,
        SlashCommand::Btw {
            question: Some("where is the cookie set?".into()),
        },
    )
    .await;
    assert_eq!(turn_done(&mut driver).await, "It's in src/auth.rs.");
    driver
        .agent
        .push(answer("Fixed plan.").with_cli_session("v-1"));
    driver.submit("so what now?").await;
    turn_done(&mut driver).await;

    let specs = driver.agent.specs();
    assert_eq!(
        specs[1].resume, None,
        "a side question never resumes the conversation"
    );
    assert!(
        prompt(&driver, 1).contains("why does login loop?"),
        "primed with the conversation"
    );
    assert_eq!(
        specs[2].resume.as_deref(),
        Some("v-1"),
        "nor takes its session over"
    );
    assert!(!prompt(&driver, 2).contains("where is the cookie set?"));
    let recorded = format!("{:?}", journal(&driver, 1));
    assert!(!recorded.contains("where is the cookie set?"), "{recorded}");
    assert!(recorded.contains("so what now?"));
    driver.exit().await;
}

#[tokio::test]
async fn compact_sums_up_and_the_next_turn_starts_fresh_from_the_summary() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Compact { instructions: None }).await;
    notice(&mut driver, "Nothing to compact yet").await;
    one_turn(&mut driver).await;
    driver
        .agent
        .push(said("We found a double cookie.").with_cli_session("v-1"));
    slash(
        &driver,
        SlashCommand::Compact {
            instructions: Some("keep the file names".into()),
        },
    )
    .await;
    turn_done(&mut driver).await;
    assert!(noticed(
        &driver,
        "Compacted. The next turn starts a fresh session"
    ));
    driver.agent.push(answer("Next.").with_cli_session("v-2"));
    driver.submit("carry on").await;
    turn_done(&mut driver).await;

    let specs = driver.agent.specs();
    assert_eq!(
        specs[1].resume.as_deref(),
        Some("v-1"),
        "the summary sees it all"
    );
    assert!(prompt(&driver, 1).contains("as you were asked: keep the file names"));
    assert_eq!(specs[2].resume, None);
    let primed = prompt(&driver, 2);
    assert!(primed.contains("We found a double cookie."), "{primed}");
    assert!(!primed.contains("The cookie is set twice"), "{primed}");
    driver.exit().await;
}

#[tokio::test]
async fn rename_names_the_conversation_or_lets_the_main_agent_suggest_off_the_record() {
    let mut driver = TestDriver::start().await;
    one_turn(&mut driver).await;
    let recorded = journal(&driver, 1);
    slash(
        &driver,
        SlashCommand::Rename {
            name: Some("Login loop".into()),
        },
    )
    .await;
    notice(&mut driver, "Renamed this conversation to \"Login loop\"").await;
    let store =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(1));
    assert_eq!(store.title().as_deref(), Some("Login loop"));

    driver.agent.push(said("\"Double cookie redirect.\""));
    slash(&driver, SlashCommand::Rename { name: None }).await;
    turn_done(&mut driver).await;
    assert!(noticed(&driver, "\"Double cookie redirect\""));
    assert_eq!(store.title().as_deref(), Some("Double cookie redirect"));
    assert_eq!(
        journal(&driver, 1),
        recorded,
        "the suggestion stays off the record"
    );
    driver.exit().await;
}

#[tokio::test]
async fn branch_carries_on_in_a_copy_and_fork_leaves_one_behind() {
    let mut driver = TestDriver::start().await;
    one_turn(&mut driver).await;
    let recorded = journal(&driver, 1);
    slash(&driver, SlashCommand::Fork { prompt: None }).await;
    notice(&mut driver, "Forked this conversation to conversation-002").await;
    slash(&driver, SlashCommand::Branch { name: None }).await;
    notice(&mut driver, "Branched into conversation-003").await;
    driver
        .agent
        .push(answer("On the branch.").with_cli_session("v-3"));
    driver.submit("try another fix").await;
    turn_done(&mut driver).await;

    let specs = driver.agent.specs();
    assert_eq!(
        specs[1].resume, None,
        "the branch never shares the original's session"
    );
    assert!(prompt(&driver, 1).contains("why does login loop?"));
    assert_eq!(
        journal(&driver, 1),
        recorded,
        "the original stays as it was"
    );
    assert_eq!(journal(&driver, 2), recorded);
    let branched = journal(&driver, 3);
    assert!(branched.len() > recorded.len() && branched.starts_with(&recorded));
    let titles =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(3))
            .title();
    assert!(titles.is_some_and(|title| title.ends_with("(branch)")));

    slash(
        &driver,
        SlashCommand::Fork {
            prompt: Some("write the regression test".into()),
        },
    )
    .await;
    driver
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = driver.conductor.starts();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].mode, RunMode::Captain);
    assert!(started[0].request.starts_with("write the regression test"));
    assert!(started[0].request.contains("Asked: try another fix"));
    driver.exit().await;
}

#[tokio::test]
async fn export_writes_the_conversation_as_plain_text_and_copy_hands_back_an_answer() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Export { file: None }).await;
    notice(&mut driver, "Nothing to export yet").await;
    one_turn(&mut driver).await;
    slash(
        &driver,
        SlashCommand::Export {
            file: Some("notes/conversation.txt".into()),
        },
    )
    .await;
    let said = driver
        .until(|event| matches!(event, AppEvent::Notice { level: NoteLevel::Info | NoteLevel::Warn | NoteLevel::Error, text } if text.contains("conversation.txt") || text.contains("writing")))
        .await;
    assert!(
        matches!(&said, AppEvent::Notice { text, .. } if text.contains("writing")),
        "a missing folder fails: {said:?}"
    );
    slash(&driver, SlashCommand::Export { file: None }).await;
    let saved = notice(&mut driver, "Saved the conversation to").await;
    let name = saved
        .rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches('.')
        .to_owned();
    let text = std::fs::read_to_string(driver.repo.root().join(&name)).unwrap();
    assert!(text.contains("> why does login loop?"), "{text}");
    assert!(text.contains("The cookie is set twice."), "{text}");

    slash(&driver, SlashCommand::Copy { nth: None }).await;
    let lines = report(&mut driver, "Answer to copy").await;
    assert_eq!(lines, ["The cookie is set twice. Then it redirects."]);
    slash(&driver, SlashCommand::Copy { nth: Some(2) }).await;
    notice(&mut driver, "only 1 answer").await;
    driver.exit().await;
}

#[tokio::test]
async fn recap_sums_up_the_conversation_at_no_cost() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Recap).await;
    notice(&mut driver, "Nothing to recap yet").await;
    one_turn(&mut driver).await;
    slash(&driver, SlashCommand::Recap).await;
    let recap = driver
        .until(|event| matches!(event, AppEvent::Recap { .. }))
        .await;
    let AppEvent::Recap { text, model } = recap else {
        unreachable!()
    };
    assert_eq!(model, None);
    assert!(
        text.contains("1 exchange") && text.contains("The cookie is set twice."),
        "{text}"
    );
    assert_eq!(driver.agent.specs().len(), 1);
    driver.exit().await;
}

#[tokio::test]
async fn fast_and_output_style_change_how_the_main_agent_answers() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Fast { on: None }).await;
    notice(&mut driver, "Fast is on").await;
    slash(
        &driver,
        SlashCommand::OutputStyle {
            style: Some("brief".into()),
        },
    )
    .await;
    notice(&mut driver, "brief style").await;
    one_turn(&mut driver).await;
    slash(&driver, SlashCommand::Fast { on: Some(false) }).await;
    notice(&mut driver, "Fast is off").await;
    slash(&driver, SlashCommand::Clear).await;
    driver.agent.push(answer("Plain."));
    driver.submit("again").await;
    turn_done(&mut driver).await;

    let specs = driver.agent.specs();
    assert!(specs[0].effort.is_some(), "fast picks the lowest effort");
    assert_ne!(specs[0].effort, specs[1].effort);
    assert!(prompt(&driver, 0).contains("Write the answer briefly"));
    assert!(
        prompt(&driver, 1).contains("Write the answer briefly"),
        "kept across /clear"
    );

    slash(&driver, SlashCommand::OutputStyle { style: None }).await;
    let lines = report(&mut driver, "Output style").await;
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("brief") && line.ends_with("in use"))
    );
    slash(
        &driver,
        SlashCommand::OutputStyle {
            style: Some("terse".into()),
        },
    )
    .await;
    notice(&mut driver, "There's no \"terse\" output style").await;
    driver.exit().await;
}

/// The runs started, once there are `count` of them.
async fn starts(driver: &TestDriver, count: usize) -> Vec<argonauts_core::api::StartRun> {
    let deadline = tokio::time::Instant::now() + crate::controller::test_support::WAIT;
    loop {
        let started = driver.conductor.starts();
        if started.len() >= count {
            return started;
        }
        assert!(tokio::time::Instant::now() < deadline, "{started:?}");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn planned(start: &argonauts_core::api::StartRun) -> bool {
    start.mode == RunMode::Crew
        && start
            .overrides
            .contains(&"approvals.plan.mode=\"supervised\"".to_owned())
}

#[tokio::test]
async fn plan_with_a_task_has_its_run_plan_first_and_wait_for_you() {
    let mut driver = TestDriver::start().await;
    driver.agent.push(change("I'd add a cache layer."));
    slash(
        &driver,
        SlashCommand::Plan {
            task: Some("add caching".into()),
        },
    )
    .await;
    driver
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = driver.conductor.starts();
    assert!(planned(&started[0]), "{:?}", started[0]);
    assert!(prompt(&driver, 0).contains("add caching"));
    driver.exit().await;
}

#[tokio::test]
async fn plan_alone_has_every_run_from_the_conversation_plan_first() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Plan { task: None }).await;
    notice(&mut driver, "Plan first is on").await;
    driver.agent.push(change("I'd add a cache layer."));
    driver.submit("add caching").await;
    driver
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = starts(&driver, 1).await;
    assert!(started.iter().all(planned), "{started:?}");
    // Captain you choose works in your checkout, where plan first plans in the session itself and
    // waits for your go-ahead, so no run starts.
    driver.agent.push(FakeSession::completed("1. Rename it."));
    slash(
        &driver,
        SlashCommand::Captain {
            task: "rename the module".into(),
        },
    )
    .await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::GoAhead)
        })
        .await;
    assert_eq!(driver.conductor.starts().len(), 1);
    slash(&driver, SlashCommand::Plan { task: None }).await;
    notice(&mut driver, "Plan first is off").await;
    driver.exit().await;
}

/// Whether the run's settings carry `condition` as its goal.
fn carries_goal(start: &argonauts_core::api::StartRun, condition: &str) -> bool {
    start
        .overrides
        .contains(&format!("goal.until=\"{condition}\""))
}

/// `/goal <condition>` starts nothing: it sets a condition for the work that follows, which
/// every run from the conversation then carries, whatever the workflow and the CLI. Pause holds
/// it, resume picks it up, and clear drops it, each reaching the run working toward it.
#[tokio::test]
async fn a_goal_is_a_condition_the_work_that_follows_carries() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Goal { text: None }).await;
    notice(&mut driver, "No goal is set").await;
    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("every test passes and the README names the flag".into()),
        },
    )
    .await;
    // Stated back with what checks it; no check command is set up here.
    let set = notice(&mut driver, "Goal set").await;
    assert_eq!(set, "Goal set · checked by the judge");
    assert!(driver.conductor.starts().is_empty(), "a goal starts no run");
    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("show".into()),
        },
    )
    .await;
    let shown = notice(&mut driver, "Goal · checked by").await;
    assert!(shown.contains("every test passes · the judge"), "{shown}");
    assert!(
        shown.contains("the README names the flag · the judge"),
        "{shown}"
    );
    assert!(shown.contains("No limit on rounds"), "{shown}");

    slash(
        &driver,
        SlashCommand::Crew {
            task: "fix slugify".into(),
        },
    )
    .await;
    driver
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = driver.conductor.starts();
    assert_eq!(started[0].request, "fix slugify");
    assert!(
        carries_goal(
            &started[0],
            "every test passes and the README names the flag"
        ),
        "{:?}",
        started[0].overrides
    );

    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("pause".into()),
        },
    )
    .await;
    notice(&mut driver, "Goal paused").await;
    slash(&driver, SlashCommand::Goal { text: None }).await;
    notice(&mut driver, "Goal paused: every test passes").await;
    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("resume".into()),
        },
    )
    .await;
    notice(&mut driver, "Goal resumed").await;
    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("clear".into()),
        },
    )
    .await;
    notice(&mut driver, "Goal cleared").await;
    slash(&driver, SlashCommand::Goal { text: None }).await;
    notice(&mut driver, "No goal is set").await;
    let calls = driver.conductor.calls();
    for action in [
        GoalAction::Pause { task: None },
        GoalAction::Resume { task: None },
        GoalAction::Clear { land: true },
    ] {
        assert!(
            calls
                .iter()
                .any(|call| matches!(call, Call::Goal { action: sent, .. } if *sent == action)),
            "{action:?}: {calls:?}"
        );
    }
    // The run itself is never paused: only its goal is.
    assert!(
        !calls.iter().any(|call| matches!(call, Call::Pause { .. })),
        "{calls:?}"
    );
    driver.exit().await;
}

/// A condition only commands may check, with no command to check it, is refused when it's
/// typed, and says how to fix it.
#[tokio::test]
async fn a_goal_nothing_can_check_is_refused() {
    let mut driver = TestDriver::start_with(CliOptions {
        flags: vec!["goal.check=\"command\"".to_owned()],
        ..CliOptions::default()
    })
    .await;
    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("the README names the flag".into()),
        },
    )
    .await;
    let refused = notice(&mut driver, "That goal can't be set").await;
    assert!(refused.contains("name one in backticks"), "{refused}");
    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("`make docs-check` passes".into()),
        },
    )
    .await;
    let set = notice(&mut driver, "Goal set").await;
    assert_eq!(set, "Goal set · checked by make docs-check");
    driver.exit().await;
}

/// A new conversation starts without the last one's goal.
#[tokio::test]
async fn clear_leaves_the_goal_with_its_conversation() {
    let mut driver = TestDriver::start().await;
    slash(
        &driver,
        SlashCommand::Goal {
            text: Some("every test passes".into()),
        },
    )
    .await;
    notice(&mut driver, "Goal set").await;
    slash(&driver, SlashCommand::Clear).await;
    notice(&mut driver, "Started a new conversation").await;
    slash(&driver, SlashCommand::Goal { text: None }).await;
    notice(&mut driver, "No goal is set").await;
    driver.exit().await;
}

#[tokio::test]
async fn workflow_starts_a_crew_run() {
    let mut driver = TestDriver::start().await;
    slash(
        &driver,
        SlashCommand::WorkflowRun {
            task: Some("fix slugify".into()),
        },
    )
    .await;
    driver
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = driver.conductor.starts();
    assert_eq!(started[0].mode, RunMode::Crew);
    assert_eq!(started[0].request, "fix slugify");
    // No goal comes with it unless you set one.
    assert!(
        !started[0]
            .overrides
            .iter()
            .any(|set| set.starts_with("goal.")),
        "{:?}",
        started[0].overrides
    );
    driver.exit().await;
}

#[tokio::test]
async fn a_review_goes_to_an_agent_from_another_provider() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Review { target: None }).await;
    notice(&mut driver, "Nothing to review").await;
    let codex = driver.directory.add(FakeRunner::cli("codex", "openai"));
    codex.push(said("One bug: the cookie path is wrong."));
    driver
        .repo
        .write("src/auth.rs", "fn set_cookie() {}\n")
        .unwrap();
    slash(&driver, SlashCommand::SecurityReview { target: None }).await;
    assert_eq!(
        turn_done(&mut driver).await,
        "One bug: the cookie path is wrong."
    );
    let specs = codex.specs();
    assert_eq!(specs.len(), 1);
    let asked: String = specs[0]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(asked.contains("security problems only"), "{asked}");
    assert!(asked.contains("from another provider"), "{asked}");
    assert!(asked.contains("+fn set_cookie() {}"), "{asked}");
    assert!(
        driver.agent.specs().is_empty(),
        "the main agent didn't review"
    );
    let recorded = format!("{:?}", journal(&driver, 1));
    assert!(
        recorded.contains("/security-review"),
        "the review joins the conversation"
    );
    driver.exit().await;
}

#[tokio::test]
async fn with_no_other_provider_in_the_crew_the_main_agent_reviews() {
    let mut driver = TestDriver::start().await;
    driver.repo.write("a.txt", "a\n").unwrap();
    driver.agent.push(said("Looks fine."));
    slash(&driver, SlashCommand::Review { target: None }).await;
    notice(&mut driver, "No agent from another provider").await;
    assert_eq!(turn_done(&mut driver).await, "Looks fine.");
    let asked = prompt(&driver, 0);
    assert!(asked.contains("Look for bugs first") && !asked.contains("another provider"));
    driver.exit().await;
}

#[tokio::test]
async fn diff_shows_the_working_tree_in_the_diff_view() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Diff).await;
    notice(&mut driver, "No changes in the working tree").await;
    driver.repo.write("README.md", "# Changed\n").unwrap();
    slash(&driver, SlashCommand::Diff).await;
    let view = driver
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await;
    let AppEvent::View { data } = view else {
        unreachable!()
    };
    let ViewData::Diff { diff } = *data else {
        panic!("{data:?}")
    };
    assert_eq!(diff.task.as_str(), "working-tree");
    assert_eq!(
        (
            diff.stat.files_changed,
            diff.stat.insertions,
            diff.stat.deletions
        ),
        (1, 1, 1)
    );
    driver.exit().await;
}

#[tokio::test]
async fn add_dir_asks_where_to_save_one_more_writable_folder() {
    let mut driver = TestDriver::start().await;
    let shared = driver.repo.scratch().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    slash(
        &driver,
        SlashCommand::AddDir {
            path: shared.clone(),
        },
    )
    .await;
    let choice = driver
        .until(|event| matches!(event, AppEvent::Choice { .. }))
        .await;
    let AppEvent::Choice { choice } = choice else {
        unreachable!()
    };
    assert_eq!(choice.kind, ChoiceKind::SettingScope);
    let body = choice.body.join("\n");
    assert!(
        body.contains("sandbox.extra_writable") || choice.title.contains("extra_writable"),
        "{choice:?}"
    );
    slash(
        &driver,
        SlashCommand::AddDir {
            path: driver.repo.root().join("src"),
        },
    )
    .await;
    notice(&mut driver, "isn't a folder").await;
    std::fs::create_dir_all(driver.repo.root().join("src")).unwrap();
    slash(&driver, SlashCommand::AddDir { path: "src".into() }).await;
    notice(&mut driver, "is in this repo").await;
    driver.exit().await;
}

/// `/usage` answers with what its tab draws, and never prints into the conversation.
/// `/usage print` still does.
/// `/stats` alone opens the tab for the period `mods/stats.toml` sets, and its numbers carry
/// the crew's stats and how the file asks for them to be shown. A period typed wins.
#[tokio::test]
async fn stats_opens_for_the_period_its_file_sets() {
    use argonauts_core::api::UsagePeriod;
    use argonauts_core::input::ViewAction;
    use argonauts_core::settings::stats::{StatsGroup, StatsSource};

    let mut driver = TestDriver::start().await;
    let mods = driver.repo.root().join(".argonauts").join("mods");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::write(
        mods.join("stats.toml"),
        "period = \"week\"\ngroup = \"model\"\n",
    )
    .unwrap();
    // A file of yours that doesn't read is left out alone, and named for the stats page.
    let yours = driver.env.paths.home().join("mods");
    std::fs::create_dir_all(&yours).unwrap();
    std::fs::write(yours.join("stats.toml"), "measure = \"fortnight\"\n").unwrap();
    for (typed, period) in [(None, UsagePeriod::Week), (Some("all"), UsagePeriod::All)] {
        let action = ViewAction::Open;
        let period_typed = typed.map(str::to_owned);
        slash(
            &driver,
            SlashCommand::Stats {
                period: period_typed,
                action,
            },
        )
        .await;
        // Writing the file reloads the mods, which send their own view as well.
        let event = driver
            .until(|event| match event {
                AppEvent::View { data } => matches!(data.as_ref(), ViewData::Usage { .. }),
                AppEvent::Report { .. } => true,
                _ => false,
            })
            .await;
        let AppEvent::View { data } = event else {
            panic!("/stats printed: {event:?}");
        };
        let ViewData::Usage { summary } = *data else {
            panic!("not the usage numbers: {data:?}");
        };
        assert_eq!(summary.period, period);
        let stats = summary.stats.expect("the crew's stats");
        assert_eq!(stats.settings.group, StatsGroup::Model);
        // The stats mod's page says the project's file set these.
        assert_eq!(stats.sources.of("group"), StatsSource::Project);
        assert_eq!(stats.sources.of("measure"), StatsSource::BuiltIn);
        let [problem] = stats.problems.as_slice() else {
            panic!("one file left out: {:?}", stats.problems);
        };
        assert!(problem.contains("mods/stats.toml: "), "{problem}");
        assert!(problem.contains("fortnight"), "{problem}");
    }
}

#[tokio::test]
async fn usage_opens_its_tab_for_the_period_and_print_still_prints() {
    use argonauts_core::api::UsagePeriod;
    use argonauts_core::input::ViewAction;

    let mut driver = TestDriver::start().await;
    let usage = |period: Option<&str>, action| SlashCommand::Usage {
        period: period.map(str::to_owned),
        action,
    };
    // With no word it opens on All, as every tab does.
    for (word, period) in [
        (None, UsagePeriod::All),
        (Some("today"), UsagePeriod::Today),
        (Some("weekly"), UsagePeriod::Week),
        (Some("monthly"), UsagePeriod::Month),
        (Some("all"), UsagePeriod::All),
    ] {
        slash(&driver, usage(word, ViewAction::Open)).await;
        let event = driver
            .until(|event| matches!(event, AppEvent::View { .. } | AppEvent::Report { .. }))
            .await;
        let AppEvent::View { data } = event else {
            panic!("/usage printed: {event:?}");
        };
        let ViewData::Usage { summary } = *data else {
            panic!("not the usage tab: {data:?}");
        };
        assert_eq!(summary.period, period);
        assert_eq!(summary.history.len(), 14);
        let main = summary.plans.first().expect("the main agent's CLI");
        assert!(main.in_use, "{main:?}");
        assert_eq!(main.name, "Claude Code");
        // The limits come with it, for the Limits tab's list, as `/limits` lists them.
        let limit = |key: &str| {
            let row = summary.limits.iter().find(|row| row.key == key);
            row.unwrap_or_else(|| panic!("{key}: {:?}", summary.limits))
        };
        assert!(
            summary
                .limits
                .iter()
                .all(|row| row.key.starts_with("limits."))
        );
        assert!(!limit("limits.when_reached").value.is_empty());
        let per_task = limit("limits.per_task_usd").value.parse::<f64>().ok();
        assert_eq!(summary.task_limit.map(|limit| limit.amount), per_task);
    }

    slash(&driver, usage(None, ViewAction::Print)).await;
    let lines = report(&mut driver, "Usage, all time").await;
    assert!(
        lines.iter().any(|line| line.starts_with("Claude · ")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("Runs in this repo · 0 runs · $0.00"))
    );
    slash(&driver, usage(Some("yearly"), ViewAction::Open)).await;
    notice(&mut driver, "/usage takes day, week, month, or all").await;
    driver.exit().await;
}

/// `print` on the Context and Limits tabs puts what the tab shows into the conversation once, and
/// opens nothing.
#[tokio::test]
async fn context_and_limits_print_in_the_conversation() {
    use argonauts_core::input::ViewAction;

    let mut driver = TestDriver::start().await;
    slash(
        &driver,
        SlashCommand::Context {
            all: false,
            action: ViewAction::Print,
        },
    )
    .await;
    // The screen prints the sessions itself, with the main session's own count.
    let printed = driver
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await;
    let AppEvent::View { data } = printed else {
        unreachable!()
    };
    let argonauts_core::app::ViewData::ContextPrint { view } = *data else {
        panic!("{data:?}");
    };
    assert!(view.rows.is_empty(), "nothing has run yet");

    slash(
        &driver,
        SlashCommand::Limits {
            key: None,
            value: None,
            action: ViewAction::Print,
        },
    )
    .await;
    let lines = report(&mut driver, "Limits").await;
    assert!(
        lines.iter().any(|line| line.starts_with("Claude · ")),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Plan usage · no limit set".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Spent today · $0.00 · no limit set".to_owned()),
        "{lines:?}"
    );
    driver.exit().await;
}

/// Back from a CLI's login, the reading kept from before may be another account's: it stops
/// standing as the CLI's own, and the CLI is read again at once.
#[tokio::test]
async fn back_from_a_login_the_cli_is_read_again() {
    use argonauts_core::limits::PlanUsageBoard as _;
    use argonauts_core::names::AgentId;

    let mut driver = TestDriver::start().await;
    // A live read builds the CLI's worker first, which registers its probe.
    driver.directory.add(FakeRunner::cli("codex", "codex"));
    slash(
        &driver,
        SlashCommand::Login {
            agent: Some("codex".into()),
        },
    )
    .await;
    let event = driver
        .until(|event| matches!(event, AppEvent::RunInTerminal { .. }))
        .await;
    let AppEvent::RunInTerminal { argv, .. } = event else {
        unreachable!()
    };
    let codex = AgentId("codex".to_owned());
    assert_eq!(driver.plan_usage.doubted_at(&codex), None);
    driver
        .send(argonauts_core::app::UiCommand::TerminalReturned { argv })
        .await;
    for _ in 0..200 {
        if !driver.plan_usage.probed().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(driver.plan_usage.doubted_at(&codex).is_some());
    assert_eq!(driver.plan_usage.probed(), [codex]);
    driver.exit().await;
}

#[tokio::test]
async fn login_and_logout_hand_the_terminal_to_the_agents_own_command() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::Login { agent: None }).await;
    let event = driver
        .until(|event| matches!(event, AppEvent::RunInTerminal { .. }))
        .await;
    let AppEvent::RunInTerminal { argv, cwd } = event else {
        unreachable!()
    };
    assert_eq!(argv, ["claude", "auth", "login"]);
    assert_eq!(cwd, driver.repo.root());
    slash(
        &driver,
        SlashCommand::Logout {
            agent: Some("codex".into()),
        },
    )
    .await;
    let event = driver
        .until(|event| matches!(event, AppEvent::RunInTerminal { .. }))
        .await;
    assert!(matches!(event, AppEvent::RunInTerminal { argv, .. } if argv == ["codex", "logout"]));
    slash(
        &driver,
        SlashCommand::Login {
            agent: Some("nobody".into()),
        },
    )
    .await;
    notice(&mut driver, "nobody isn't an agent CLI").await;
    driver.exit().await;
}

#[tokio::test]
async fn without_a_terminal_login_says_what_to_run() {
    let mut driver = TestDriver::start_with(CliOptions {
        interactive: false,
        ..CliOptions::default()
    })
    .await;
    slash(&driver, SlashCommand::Login { agent: None }).await;
    notice(&mut driver, "Run it in one").await;
    driver.exit().await;
}

#[tokio::test]
async fn release_notes_and_feedback_are_about_argonauts() {
    let mut driver = TestDriver::start().await;
    slash(&driver, SlashCommand::ReleaseNotes).await;
    let lines = report(&mut driver, "Release notes").await;
    assert!(lines[0].starts_with("This is Argonauts Code "), "{lines:?}");
    slash(&driver, SlashCommand::Feedback { text: None }).await;
    notice(&mut driver, "Put your feedback after /feedback").await;
    slash(
        &driver,
        SlashCommand::Feedback {
            text: Some("the menu is slow".into()),
        },
    )
    .await;
    notice(&mut driver, "Nothing leaves this machine").await;
    let saved = std::fs::read_to_string(driver.repo.home().join("feedback.jsonl")).unwrap();
    assert!(saved.contains("\"text\":\"the menu is slow\""), "{saved}");
    driver.exit().await;
}
