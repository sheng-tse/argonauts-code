//! A council's checks through a run: code picks each task's checker by the ladder, the check
//! is a fresh session that never learns who made the change, and work that signs itself goes
//! back to its maker before any check reads it.

use crate::support;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

use argonauts_core::display_names::DisplayNames;
use argonauts_core::engine::EngineExit;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
};
use argonauts_core::events::council::{CheckRung, CouncilEventKind};
use argonauts_core::events::run::{RunMode, RunOutcome};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::RoleName;
use argonauts_core::session_runner::SessionSpec;
use argonauts_core::settings::council::SameModelCheck;
use argonauts_core::settings::crew::{CrewSettings, Orchestrator};
use argonauts_core::settings::workflows::AdaptWith;
use argonauts_core::steps::StepName;
use argonauts_crew::council::lexicon::Lexicon;
use argonauts_testkit::runner::{FakeRunner, FakeSession};

use support::Crew;

const REQUEST: &str = "Add due dates to tasks";
const MAIN: &str = "claude-code/opus";
const WORKER: &str = "codex/gpt";
const OTHER: &str = "antigravity-cli/gemini";

/// The council's build and check steps alone, so a run reaches its checks at once.
const COUNCIL: &str = r#"
name  = "council"
about = "Build each task, and code picks who checks it."
adapt = false

[[step]]
name = "Plan"
role = "orchestrator"
does = "Split the request into tasks: {request}"

[[step]]
name     = "Build"
role     = "worker"
for_each = "task"

[[step]]
name           = "Cross-review"
role           = "worker"
for_each       = "task"
not_the_author = true
send_back_to   = "Build"
"#;

fn report(file: &str) -> Value {
    json!({
        "status": "done",
        "summary": format!("Wrote {file}"),
        "files_changed": [file],
        "tests_run": [],
        "concerns": [],
    })
}

/// A crew of one task's maker and one model of another maker, running the council above.
/// The crew, its maker's runner and the other maker's.
type Council = (Crew, Arc<FakeRunner>, Arc<FakeRunner>);

fn council(builds: fn(&SessionSpec) -> FakeSession) -> anyhow::Result<Council> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(|_| {
        FakeSession::structured(json!({
            "summary": "One task",
            "tasks": [{"title": "Due field", "done_when": ["a task has a due date"]}],
        }))
    });
    crew.services.workers.add(main);
    let maker = crew
        .services
        .workers
        .add(FakeRunner::cli(WORKER, "openai").respond_with(builds));
    let other = crew
        .services
        .workers
        .add(FakeRunner::cli(OTHER, "google").respond_with(builds));
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![WORKER.parse()?, OTHER.parse()?],
        reviewer: vec![WORKER.parse()?],
        judge: vec![WORKER.parse()?],
        ..CrewSettings::default()
    };
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    let folder = crew.repo.root().join(".argonauts/workflows");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("council.toml"), COUNCIL)?;
    Ok((crew, maker, other))
}

fn checks(events: &[RunEvent]) -> Vec<(Option<String>, CheckRung)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Council(council) => match &council.kind {
                CouncilEventKind::Check(pick) => {
                    Some((pick.checker.as_ref().map(ToString::to_string), pick.rung))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn prompt_text(spec: &SessionSpec) -> String {
    spec.prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Builds when asked to, and accepts what it checks.
fn builds_and_accepts(spec: &SessionSpec) -> FakeSession {
    if spec.step == Some(StepName::Build) {
        std::fs::write(spec.workdir.join("due.rs"), "pub struct Due;\n").expect("writes its file");
        FakeSession::structured(report("due.rs"))
    } else if spec.json_schema.is_some() {
        FakeSession::structured(json!({"accept": true, "findings": []}))
    } else {
        FakeSession::completed("Done.")
    }
}

/// The task's reviewers name only its maker, yet the check goes to the seat from another
/// maker, in a fresh session that reads the check's frame and never who made the change.
#[tokio::test]
async fn a_task_is_checked_by_another_maker_in_a_fresh_session_blind_to_its_maker()
-> anyhow::Result<()> {
    let (crew, maker, other) = council(builds_and_accepts)?;
    let _env = crew.repo.env_guard();
    let outcomes = crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;
    let events = &outcomes[outcomes.len() - 1].events;

    assert_eq!(
        checks(events),
        [(Some(OTHER.to_owned()), CheckRung::OtherMaker)]
    );
    let built: Vec<SessionSpec> = maker.specs();
    assert!(built.iter().any(|spec| spec.step == Some(StepName::Build)));
    let check = other
        .specs()
        .into_iter()
        .find(|spec| spec.json_schema.is_some())
        .expect("the other maker checks");
    assert_eq!(check.role, RoleName::Reviewer);
    assert_eq!(check.resume, None, "a check is always a fresh session");
    let labels: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.role == RoleName::Reviewer =>
            {
                started.label.clone()
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        ["check:due-field"],
        "a check goes by its task alone"
    );
    // The status names each task's model, so a check gets neither its part nor its tool.
    assert!(check.status_file.is_none());
    let read = prompt_text(&check);
    let reviewer = read
        .find("You are a reviewer")
        .expect("the reviewers' frame");
    let council = read
        .find("You check one task of a plan that a council agreed")
        .expect("the council's check");
    assert!(reviewer < council, "the check follows the reviewers' frame");
    assert!(read.contains("You aren't told who made the change"));
    let models: Vec<argonauts_core::names::ModelRef> = vec![WORKER.parse()?, OTHER.parse()?];
    let provider = |model: &argonauts_core::names::ModelRef| match model.agent.as_str() {
        "codex" => "openai".to_owned(),
        _ => "google".to_owned(),
    };
    let lexicon = Lexicon::built_in()
        .with_models(&models, &DisplayNames::default(), &provider)
        .with_topic(REQUEST);
    let named: Vec<&str> = lexicon
        .find(&read)
        .into_iter()
        .map(|hit| &read[hit.range])
        .collect();
    assert!(
        named.is_empty(),
        "the check never learns who made it: {named:?}"
    );
    Ok(())
}

/// Signs its first build with a comment that credits a model, and builds clean when sent back.
fn signs_once(spec: &SessionSpec) -> FakeSession {
    if spec.step == Some(StepName::Build) {
        let sent_back = prompt_text(spec).contains("Remove the lines that sign the work");
        let text = if sent_back {
            "pub struct Due;\n"
        } else {
            "// Written by GPT for the due dates\npub struct Due;\n"
        };
        std::fs::write(spec.workdir.join("due.rs"), text).expect("writes its file");
        FakeSession::structured(report("due.rs"))
    } else if spec.json_schema.is_some() {
        FakeSession::structured(json!({"accept": true, "findings": []}))
    } else {
        FakeSession::completed("Done.")
    }
}

/// Work that signs itself goes back to its maker with one line, and no check reads it until
/// the signature is gone.
#[tokio::test]
async fn signed_work_goes_back_to_its_maker_before_any_check() -> anyhow::Result<()> {
    let (crew, maker, other) = council(signs_once)?;
    let _env = crew.repo.env_guard();
    crew.run_to_end(RunMode::Crew, REQUEST, 3).await?;

    let builds: Vec<String> = maker
        .specs()
        .iter()
        .filter(|spec| spec.step == Some(StepName::Build))
        .map(prompt_text)
        .collect();
    assert_eq!(builds.len(), 2, "built, sent back, built again");
    assert!(builds[1].contains("Remove the lines that sign the work"));
    assert!(builds[1].contains("due.rs: // Written by GPT for the due dates"));
    let checked = other
        .specs()
        .into_iter()
        .filter(|spec| spec.json_schema.is_some())
        .count();
    assert_eq!(checked, 1, "only the clean build was checked");
    Ok(())
}

const SAME_MAKER_QUESTION: &str = "Check each task with the model that made it?";
const YOUR_CHECK: &str = "Accept build:due-field?";

/// A council whose only maker builds its one task, with `council.same_model_check = "ask"`.
/// The crew and its one maker's runner.
fn one_maker(builds: fn(&SessionSpec) -> FakeSession) -> anyhow::Result<(Crew, Arc<FakeRunner>)> {
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    let main = FakeRunner::cli(MAIN, "anthropic").respond_with(|_| {
        FakeSession::structured(json!({
            "summary": "One task",
            "tasks": [{"title": "Due field", "done_when": ["a task has a due date"]}],
        }))
    });
    crew.services.workers.add(main);
    let maker = crew
        .services
        .workers
        .add(FakeRunner::cli(WORKER, "openai").respond_with(builds));
    crew.settings.crew = CrewSettings {
        main: MAIN.parse()?,
        orchestrator: Orchestrator::Main,
        worker: vec![WORKER.parse()?],
        reviewer: vec![WORKER.parse()?],
        judge: vec![WORKER.parse()?],
        ..CrewSettings::default()
    };
    crew.settings.council.same_model_check = SameModelCheck::Ask;
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_with = AdaptWith::Script;
    let folder = crew.repo.root().join(".argonauts/workflows");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join("council.toml"), COUNCIL)?;
    Ok((crew, maker))
}

/// Builds when asked to, sends back the first change it checks and accepts the next.
fn builds_and_sends_back_once(spec: &SessionSpec) -> FakeSession {
    static CHECKED: AtomicUsize = AtomicUsize::new(0);
    if spec.step != Some(StepName::Build)
        && spec.json_schema.is_some()
        && CHECKED.fetch_add(1, Ordering::SeqCst) == 0
    {
        return FakeSession::structured(json!({"accept": false, "findings": ["No test"]}));
    }
    builds_and_accepts(spec)
}

fn newest_open(events: &[RunEvent]) -> Option<ApprovalOpened> {
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => open.push(opened.clone()),
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.retain(|opened| opened.id != decided.id);
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                open.retain(|opened| &opened.id != id);
            }
            _ => {}
        }
    }
    open.pop().map(|opened| *opened)
}

/// Your answer to one question: what you pick, and the note you type with it.
type Pick = Option<(AnswerAction, Option<&'static str>)>;

/// Runs the request to its end, answering each question with `pick`'s answer for it, else its
/// recommended option. The last stretch's events, once the run finished.
async fn run_answering(
    crew: &Crew,
    mut pick: impl FnMut(&ApprovalOpened) -> Pick,
) -> anyhow::Result<Vec<RunEvent>> {
    let mut outcome = crew.start(RunMode::Crew, REQUEST).await?;
    for _ in 0..10 {
        if !matches!(outcome.exit, EngineExit::Waiting(_)) {
            break;
        }
        let Some(open) = newest_open(&outcome.events) else {
            break;
        };
        let (command, _reply) = match pick(&open) {
            Some((action, text)) => support::answer(&outcome.events, open.kind, action, text)?,
            None => support::recommended(&outcome.events).context("an answer")?,
        };
        outcome = crew.resume(outcome.events.clone(), vec![command]).await?;
    }
    let EngineExit::Finished(RunOutcome::Merged) = &outcome.exit else {
        bail!("the run should finish: {:?}", outcome.exit);
    };
    Ok(outcome.events)
}

/// The titles of the questions the council asked you, in the order they opened.
fn council_questions(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened))
                if opened.kind == ApprovalKind::Question =>
            {
                Some(opened.title.clone())
            }
            _ => None,
        })
        .collect()
}

fn choose(value: &str) -> AnswerAction {
    AnswerAction::Choose {
        value: value.to_owned(),
    }
}

/// With one maker and `same_model_check = "ask"`, you're asked once for the council, and your
/// yes lets a fresh session of the model that made the task check it, each time it's built.
#[tokio::test]
async fn one_yes_lets_the_same_model_check_for_the_whole_council() -> anyhow::Result<()> {
    let (crew, maker) = one_maker(builds_and_sends_back_once)?;
    let _env = crew.repo.env_guard();
    let events = run_answering(&crew, |_| None).await?;

    assert_eq!(council_questions(&events), [SAME_MAKER_QUESTION]);
    assert_eq!(
        checks(&events),
        [
            (Some(WORKER.to_owned()), CheckRung::SameMaker),
            (Some(WORKER.to_owned()), CheckRung::SameMaker),
        ]
    );
    let fresh: Vec<SessionSpec> = maker
        .specs()
        .into_iter()
        .filter(|spec| spec.json_schema.is_some() && spec.role == RoleName::Reviewer)
        .collect();
    assert_eq!(fresh.len(), 2, "the change is checked after each build");
    assert!(fresh.iter().all(|spec| spec.resume.is_none()));
    Ok(())
}

/// Your no makes the check yours: no session checks, the question names the folder the work
/// is in and what the checks said, and the screen's line says you checked it.
#[tokio::test]
async fn after_your_no_you_check_the_task_yourself() -> anyhow::Result<()> {
    let (crew, maker) = one_maker(builds_and_accepts)?;
    let _env = crew.repo.env_guard();
    let mut texts = Vec::new();
    let events = run_answering(&crew, |open| {
        if open.title == SAME_MAKER_QUESTION {
            return Some((choose("no"), None));
        }
        if let ApprovalPacket::SecondAgent(packet) = &open.packet {
            texts.push(packet.text.clone());
            return Some((choose("accept"), None));
        }
        None
    })
    .await?;

    assert_eq!(
        council_questions(&events),
        [SAME_MAKER_QUESTION, YOUR_CHECK]
    );
    assert_eq!(checks(&events), [(None, CheckRung::You)]);
    assert!(
        maker
            .specs()
            .iter()
            .all(|spec| spec.role != RoleName::Reviewer),
        "no session checks"
    );
    let [text] = texts.as_slice() else {
        bail!("one check of yours: {texts:#?}");
    };
    assert!(
        text.starts_with("GPT made it, and its work is in /"),
        "{text}"
    );
    assert!(text.contains("/task-due-field.\n"), "{text}");
    assert!(text.ends_with("\nAccept it, or send it back with what to change."));
    Ok(())
}

/// What you type as you send the task back is what its maker reads, and the next build is
/// yours to check again.
#[tokio::test]
async fn your_send_back_reaches_the_maker_and_asks_again() -> anyhow::Result<()> {
    let (crew, maker) = one_maker(builds_and_accepts)?;
    let _env = crew.repo.env_guard();
    let mut sent_back = false;
    let events = run_answering(&crew, |open| {
        if open.title == SAME_MAKER_QUESTION {
            return Some((choose("no"), None));
        }
        if open.title != YOUR_CHECK {
            return None;
        }
        if sent_back {
            return Some((choose("accept"), None));
        }
        sent_back = true;
        Some((
            AnswerAction::SendBack,
            Some("Add a test for a past due date"),
        ))
    })
    .await?;

    assert_eq!(
        council_questions(&events),
        [SAME_MAKER_QUESTION, YOUR_CHECK, YOUR_CHECK]
    );
    let read: Vec<bool> = maker
        .specs()
        .iter()
        .filter(|spec| spec.step == Some(StepName::Build))
        .map(|spec| prompt_text(spec).contains("Add a test for a past due date"))
        .collect();
    assert_eq!(read, [false, true], "the second build reads your note");
    Ok(())
}
