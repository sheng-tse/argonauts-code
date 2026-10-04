//! The commands the agent CLIs share, through the real controller loop with a fake main agent:
//! each works the same whatever CLI the main agent is.

use argonauts_core::NoteLevel;
use argonauts_core::app::{
    AppEvent, AppMode, ChatEvent, ChoiceKind, TurnReply, UiCommand, ViewData,
};
use argonauts_core::events::goal::GoalAction;
use argonauts_core::events::run::RunMode;
use argonauts_core::input::SlashCommand;
use argonauts_core::names::ChatId;
use argonauts_testkit::{FakeSession, FakeWorker};
use serde_json::json;

use crate::chat_store::ChatStore;
use crate::controller::test_support::{Call, Harness, HarnessOptions};

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

async fn slash(harness: &Harness, command: SlashCommand) {
    harness.send(UiCommand::Slash { command }).await;
}

fn finished(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Chat {
            event: ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. }
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
async fn turn_done(harness: &mut Harness) -> String {
    let event = harness.until(finished).await;
    harness.until(idle).await;
    match event {
        AppEvent::Chat {
            event:
                ChatEvent::TurnFinished {
                    reply: TurnReply::Answer { markdown },
                    ..
                },
        } => markdown,
        other => panic!("{other:?}"),
    }
}

async fn notice(harness: &mut Harness, part: &str) -> String {
    let event = harness
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
        .await;
    match event {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
}

/// A notice already read, such as one sent while a turn finished.
fn noticed(harness: &Harness, part: &str) -> bool {
    harness
        .seen()
        .iter()
        .any(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
}

async fn report(harness: &mut Harness, title: &str) -> Vec<String> {
    let event = harness
        .until(|event| matches!(event, AppEvent::Report { title: t, .. } if t == title))
        .await;
    match event {
        AppEvent::Report { lines, .. } => lines,
        _ => unreachable!(),
    }
}

fn prompt(harness: &Harness, index: usize) -> String {
    harness.agent.specs()[index]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect()
}

fn journal(harness: &Harness, chat: u32) -> Vec<ChatEvent> {
    ChatStore::open(&harness.env.chat_dir(), &ChatId::numbered(chat))
        .events()
        .unwrap()
}

/// One turn asked and answered, in a resumable vendor session.
async fn one_turn(harness: &mut Harness) {
    harness
        .agent
        .push(answer("The cookie is set twice. Then it redirects.").with_vendor_session("v-1"));
    harness.submit("why does login loop?").await;
    turn_done(harness).await;
}

#[tokio::test]
async fn btw_answers_from_its_own_session_and_stays_out_of_the_conversation() {
    let mut harness = Harness::start().await;
    one_turn(&mut harness).await;
    harness
        .agent
        .push(said("It's in src/auth.rs.").with_vendor_session("v-side"));
    slash(
        &harness,
        SlashCommand::Btw {
            question: Some("where is the cookie set?".into()),
        },
    )
    .await;
    assert_eq!(turn_done(&mut harness).await, "It's in src/auth.rs.");
    harness
        .agent
        .push(answer("Fixed plan.").with_vendor_session("v-1"));
    harness.submit("so what now?").await;
    turn_done(&mut harness).await;

    let specs = harness.agent.specs();
    assert_eq!(
        specs[1].resume, None,
        "a side question never resumes the conversation"
    );
    assert!(
        prompt(&harness, 1).contains("why does login loop?"),
        "primed with the chat"
    );
    assert_eq!(
        specs[2].resume.as_deref(),
        Some("v-1"),
        "nor takes its session over"
    );
    assert!(!prompt(&harness, 2).contains("where is the cookie set?"));
    let recorded = format!("{:?}", journal(&harness, 1));
    assert!(!recorded.contains("where is the cookie set?"), "{recorded}");
    assert!(recorded.contains("so what now?"));
    harness.exit().await;
}

#[tokio::test]
async fn compact_sums_up_and_the_next_turn_starts_fresh_from_the_summary() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Compact { instructions: None }).await;
    notice(&mut harness, "Nothing to compact yet").await;
    one_turn(&mut harness).await;
    harness
        .agent
        .push(said("We found a double cookie.").with_vendor_session("v-1"));
    slash(
        &harness,
        SlashCommand::Compact {
            instructions: Some("keep the file names".into()),
        },
    )
    .await;
    turn_done(&mut harness).await;
    assert!(noticed(
        &harness,
        "Compacted. The next turn starts a fresh session"
    ));
    harness
        .agent
        .push(answer("Next.").with_vendor_session("v-2"));
    harness.submit("carry on").await;
    turn_done(&mut harness).await;

    let specs = harness.agent.specs();
    assert_eq!(
        specs[1].resume.as_deref(),
        Some("v-1"),
        "the summary sees it all"
    );
    assert!(prompt(&harness, 1).contains("as you were asked: keep the file names"));
    assert_eq!(specs[2].resume, None);
    let primed = prompt(&harness, 2);
    assert!(primed.contains("We found a double cookie."), "{primed}");
    assert!(!primed.contains("The cookie is set twice"), "{primed}");
    harness.exit().await;
}

#[tokio::test]
async fn rename_names_the_conversation_or_lets_the_lead_suggest_off_the_record() {
    let mut harness = Harness::start().await;
    one_turn(&mut harness).await;
    let recorded = journal(&harness, 1);
    slash(
        &harness,
        SlashCommand::Rename {
            name: Some("Login loop".into()),
        },
    )
    .await;
    notice(&mut harness, "Renamed this conversation to \"Login loop\"").await;
    let store = ChatStore::open(&harness.env.chat_dir(), &ChatId::numbered(1));
    assert_eq!(store.title().as_deref(), Some("Login loop"));

    harness.agent.push(said("\"Double cookie redirect.\""));
    slash(&harness, SlashCommand::Rename { name: None }).await;
    turn_done(&mut harness).await;
    assert!(noticed(&harness, "\"Double cookie redirect\""));
    assert_eq!(store.title().as_deref(), Some("Double cookie redirect"));
    assert_eq!(
        journal(&harness, 1),
        recorded,
        "the suggestion stays off the record"
    );
    harness.exit().await;
}

#[tokio::test]
async fn branch_carries_on_in_a_copy_and_fork_leaves_one_behind() {
    let mut harness = Harness::start().await;
    one_turn(&mut harness).await;
    let recorded = journal(&harness, 1);
    slash(&harness, SlashCommand::Fork { prompt: None }).await;
    notice(&mut harness, "Forked this conversation to chat-002").await;
    slash(&harness, SlashCommand::Branch { name: None }).await;
    notice(&mut harness, "Branched into chat-003").await;
    harness
        .agent
        .push(answer("On the branch.").with_vendor_session("v-3"));
    harness.submit("try another fix").await;
    turn_done(&mut harness).await;

    let specs = harness.agent.specs();
    assert_eq!(
        specs[1].resume, None,
        "the branch never shares the original's session"
    );
    assert!(prompt(&harness, 1).contains("why does login loop?"));
    assert_eq!(
        journal(&harness, 1),
        recorded,
        "the original stays as it was"
    );
    assert_eq!(journal(&harness, 2), recorded);
    let branched = journal(&harness, 3);
    assert!(branched.len() > recorded.len() && branched.starts_with(&recorded));
    let titles = ChatStore::open(&harness.env.chat_dir(), &ChatId::numbered(3)).title();
    assert!(titles.is_some_and(|title| title.ends_with("(branch)")));

    slash(
        &harness,
        SlashCommand::Fork {
            prompt: Some("write the regression test".into()),
        },
    )
    .await;
    harness
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = harness.conductor.starts();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].mode, RunMode::Solo);
    assert!(started[0].request.starts_with("write the regression test"));
    assert!(started[0].request.contains("Asked: try another fix"));
    harness.exit().await;
}

#[tokio::test]
async fn export_writes_the_conversation_as_plain_text_and_copy_hands_back_an_answer() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Export { file: None }).await;
    notice(&mut harness, "Nothing to export yet").await;
    one_turn(&mut harness).await;
    slash(
        &harness,
        SlashCommand::Export {
            file: Some("notes/chat.txt".into()),
        },
    )
    .await;
    let said = harness
        .until(|event| matches!(event, AppEvent::Notice { level: NoteLevel::Info | NoteLevel::Warn | NoteLevel::Error, text } if text.contains("chat.txt") || text.contains("writing")))
        .await;
    assert!(
        matches!(&said, AppEvent::Notice { text, .. } if text.contains("writing")),
        "a missing folder fails: {said:?}"
    );
    slash(&harness, SlashCommand::Export { file: None }).await;
    let saved = notice(&mut harness, "Saved the conversation to").await;
    let name = saved
        .rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches('.')
        .to_owned();
    let text = std::fs::read_to_string(harness.repo.root().join(&name)).unwrap();
    assert!(text.contains("> why does login loop?"), "{text}");
    assert!(text.contains("The cookie is set twice."), "{text}");

    slash(&harness, SlashCommand::Copy { nth: None }).await;
    let lines = report(&mut harness, "Answer to copy").await;
    assert_eq!(lines, ["The cookie is set twice. Then it redirects."]);
    slash(&harness, SlashCommand::Copy { nth: Some(2) }).await;
    notice(&mut harness, "only 1 answer").await;
    harness.exit().await;
}

#[tokio::test]
async fn recap_sums_up_the_conversation_at_no_cost() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Recap).await;
    notice(&mut harness, "Nothing to recap yet").await;
    one_turn(&mut harness).await;
    slash(&harness, SlashCommand::Recap).await;
    let recap = harness
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
    assert_eq!(harness.agent.specs().len(), 1);
    harness.exit().await;
}

#[tokio::test]
async fn fast_and_output_style_change_how_the_main_agent_answers() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Fast { on: None }).await;
    notice(&mut harness, "Fast is on").await;
    slash(
        &harness,
        SlashCommand::OutputStyle {
            style: Some("brief".into()),
        },
    )
    .await;
    notice(&mut harness, "brief style").await;
    one_turn(&mut harness).await;
    slash(&harness, SlashCommand::Fast { on: Some(false) }).await;
    notice(&mut harness, "Fast is off").await;
    slash(&harness, SlashCommand::Clear).await;
    harness.agent.push(answer("Plain."));
    harness.submit("again").await;
    turn_done(&mut harness).await;

    let specs = harness.agent.specs();
    assert!(specs[0].effort.is_some(), "fast picks the lowest effort");
    assert_ne!(specs[0].effort, specs[1].effort);
    assert!(prompt(&harness, 0).contains("Write the answer briefly"));
    assert!(
        prompt(&harness, 1).contains("Write the answer briefly"),
        "kept across /clear"
    );

    slash(&harness, SlashCommand::OutputStyle { style: None }).await;
    let lines = report(&mut harness, "Output style").await;
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("brief") && line.ends_with("in use"))
    );
    slash(
        &harness,
        SlashCommand::OutputStyle {
            style: Some("terse".into()),
        },
    )
    .await;
    notice(&mut harness, "There's no \"terse\" output style").await;
    harness.exit().await;
}

/// The runs started, once there are `count` of them.
async fn starts(harness: &Harness, count: usize) -> Vec<argonauts_core::api::StartRun> {
    let deadline = tokio::time::Instant::now() + crate::controller::test_support::WAIT;
    loop {
        let started = harness.conductor.starts();
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
    let mut harness = Harness::start().await;
    harness.agent.push(change("I'd add a cache layer."));
    slash(
        &harness,
        SlashCommand::Plan {
            task: Some("add caching".into()),
        },
    )
    .await;
    harness
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = harness.conductor.starts();
    assert!(planned(&started[0]), "{:?}", started[0]);
    assert!(prompt(&harness, 0).contains("add caching"));
    harness.exit().await;
}

#[tokio::test]
async fn plan_alone_has_every_run_from_the_conversation_plan_first() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Plan { task: None }).await;
    notice(&mut harness, "Plan first is on").await;
    harness.agent.push(change("I'd add a cache layer."));
    harness.submit("add caching").await;
    harness
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    slash(
        &harness,
        SlashCommand::Solo {
            task: "rename the module".into(),
        },
    )
    .await;
    let started = starts(&harness, 2).await;
    assert!(started.iter().all(planned), "{started:?}");
    slash(&harness, SlashCommand::Plan { task: None }).await;
    notice(&mut harness, "Plan first is off").await;
    harness.exit().await;
}

#[tokio::test]
async fn a_goal_starts_a_crew_run_that_pause_and_resume_hold() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Goal { text: None }).await;
    notice(&mut harness, "No goal is set").await;
    slash(
        &harness,
        SlashCommand::Goal {
            text: Some("every test passes".into()),
        },
    )
    .await;
    notice(&mut harness, "Goal set").await;
    harness
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = harness.conductor.starts();
    assert_eq!(started[0].mode, RunMode::Crew);
    assert_eq!(started[0].request, "every test passes");
    // A goal whoever leads: the run's own command wins over workflows.style.
    assert!(
        started[0]
            .overrides
            .contains(&"workflows.style=\"goal\"".to_owned()),
        "{:?}",
        started[0].overrides
    );
    slash(&harness, SlashCommand::Goal { text: None }).await;
    notice(&mut harness, "run-001 works toward it").await;
    slash(
        &harness,
        SlashCommand::Goal {
            text: Some("pause".into()),
        },
    )
    .await;
    slash(
        &harness,
        SlashCommand::Goal {
            text: Some("clear".into()),
        },
    )
    .await;
    notice(&mut harness, "Stopped pursuing the goal").await;
    let calls = harness.conductor.calls();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::Pause { now: false, .. })),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|call| matches!(
            call,
            Call::Goal {
                action: GoalAction::Clear { land: true },
                ..
            }
        )),
        "{calls:?}"
    );
    harness.exit().await;
}

#[tokio::test]
async fn workflow_starts_a_crew_run_that_is_a_workflow_whoever_leads() {
    let mut harness = Harness::start().await;
    slash(
        &harness,
        SlashCommand::Workflow {
            task: Some("fix slugify".into()),
        },
    )
    .await;
    harness
        .until(|event| matches!(event, AppEvent::Attached { .. }))
        .await;
    let started = harness.conductor.starts();
    assert_eq!(started[0].mode, RunMode::Crew);
    assert_eq!(started[0].request, "fix slugify");
    assert!(
        started[0]
            .overrides
            .contains(&"workflows.style=\"workflow\"".to_owned()),
        "{:?}",
        started[0].overrides
    );
    harness.exit().await;
}

#[tokio::test]
async fn a_review_goes_to_an_agent_from_another_company() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Review { target: None }).await;
    notice(&mut harness, "Nothing to review").await;
    let codex = harness.directory.add(FakeWorker::harness("codex", "gpt"));
    codex.push(said("One bug: the cookie path is wrong."));
    harness
        .repo
        .write("src/auth.rs", "fn set_cookie() {}\n")
        .unwrap();
    slash(&harness, SlashCommand::SecurityReview { target: None }).await;
    assert_eq!(
        turn_done(&mut harness).await,
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
    assert!(asked.contains("from another company"), "{asked}");
    assert!(asked.contains("+fn set_cookie() {}"), "{asked}");
    assert!(
        harness.agent.specs().is_empty(),
        "the main agent didn't review"
    );
    let recorded = format!("{:?}", journal(&harness, 1));
    assert!(
        recorded.contains("/security-review"),
        "the review joins the conversation"
    );
    harness.exit().await;
}

#[tokio::test]
async fn with_no_other_company_in_the_crew_the_main_agent_reviews() {
    let mut harness = Harness::start().await;
    harness.repo.write("a.txt", "a\n").unwrap();
    harness.agent.push(said("Looks fine."));
    slash(&harness, SlashCommand::Review { target: None }).await;
    notice(&mut harness, "No agent from another company").await;
    assert_eq!(turn_done(&mut harness).await, "Looks fine.");
    let asked = prompt(&harness, 0);
    assert!(asked.contains("Look for bugs first") && !asked.contains("another company"));
    harness.exit().await;
}

#[tokio::test]
async fn diff_shows_the_working_tree_in_the_diff_view() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Diff).await;
    notice(&mut harness, "No changes in the working tree").await;
    harness.repo.write("README.md", "# Changed\n").unwrap();
    slash(&harness, SlashCommand::Diff).await;
    let view = harness
        .until(|event| matches!(event, AppEvent::View { .. }))
        .await;
    let AppEvent::View { data } = view else {
        unreachable!()
    };
    let ViewData::Diff { diff } = *data else {
        panic!("{data:?}")
    };
    assert_eq!(diff.card.as_str(), "working-tree");
    assert_eq!(
        (
            diff.stat.files_changed,
            diff.stat.insertions,
            diff.stat.deletions
        ),
        (1, 1, 1)
    );
    harness.exit().await;
}

#[tokio::test]
async fn add_dir_asks_where_to_save_one_more_writable_folder() {
    let mut harness = Harness::start().await;
    let shared = harness.repo.scratch().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    slash(
        &harness,
        SlashCommand::AddDir {
            path: shared.clone(),
        },
    )
    .await;
    let choice = harness
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
        &harness,
        SlashCommand::AddDir {
            path: harness.repo.root().join("src"),
        },
    )
    .await;
    notice(&mut harness, "isn't a folder").await;
    std::fs::create_dir_all(harness.repo.root().join("src")).unwrap();
    slash(&harness, SlashCommand::AddDir { path: "src".into() }).await;
    notice(&mut harness, "is in this repo").await;
    harness.exit().await;
}

#[tokio::test]
async fn usage_shows_quotas_and_spend_and_takes_the_vendors_periods() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Usage { period: None }).await;
    let lines = report(&mut harness, "Usage, last 24 hours").await;
    assert!(
        lines.iter().any(|line| line.starts_with("Claude · ")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("Runs in this repo · 0 runs · $0.00"))
    );
    slash(
        &harness,
        SlashCommand::Usage {
            period: Some("weekly".into()),
        },
    )
    .await;
    report(&mut harness, "Usage, last 7 days").await;
    slash(
        &harness,
        SlashCommand::Usage {
            period: Some("monthly".into()),
        },
    )
    .await;
    notice(&mut harness, "/usage takes day, week, or all").await;
    harness.exit().await;
}

#[tokio::test]
async fn login_and_logout_hand_the_terminal_to_the_agents_own_command() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::Login { agent: None }).await;
    let event = harness
        .until(|event| matches!(event, AppEvent::RunInTerminal { .. }))
        .await;
    let AppEvent::RunInTerminal { argv, cwd } = event else {
        unreachable!()
    };
    assert_eq!(argv, ["claude", "auth", "login"]);
    assert_eq!(cwd, harness.repo.root());
    slash(
        &harness,
        SlashCommand::Logout {
            agent: Some("codex".into()),
        },
    )
    .await;
    let event = harness
        .until(|event| matches!(event, AppEvent::RunInTerminal { .. }))
        .await;
    assert!(matches!(event, AppEvent::RunInTerminal { argv, .. } if argv == ["codex", "logout"]));
    slash(
        &harness,
        SlashCommand::Login {
            agent: Some("nobody".into()),
        },
    )
    .await;
    notice(&mut harness, "nobody isn't an agent CLI").await;
    harness.exit().await;
}

#[tokio::test]
async fn without_a_terminal_login_says_what_to_run() {
    let mut harness = Harness::start_with(HarnessOptions {
        interactive: false,
        ..HarnessOptions::default()
    })
    .await;
    slash(&harness, SlashCommand::Login { agent: None }).await;
    notice(&mut harness, "Run it in one").await;
    harness.exit().await;
}

#[tokio::test]
async fn release_notes_and_feedback_are_about_argonauts() {
    let mut harness = Harness::start().await;
    slash(&harness, SlashCommand::ReleaseNotes).await;
    let lines = report(&mut harness, "Release notes").await;
    assert!(lines[0].starts_with("This is Argonauts "), "{lines:?}");
    slash(&harness, SlashCommand::Feedback { text: None }).await;
    notice(&mut harness, "Put your feedback after /feedback").await;
    slash(
        &harness,
        SlashCommand::Feedback {
            text: Some("the menu is slow".into()),
        },
    )
    .await;
    notice(&mut harness, "Nothing leaves this machine").await;
    let saved = std::fs::read_to_string(harness.repo.home().join("feedback.jsonl")).unwrap();
    assert!(saved.contains("\"text\":\"the menu is slow\""), "{saved}");
    harness.exit().await;
}
