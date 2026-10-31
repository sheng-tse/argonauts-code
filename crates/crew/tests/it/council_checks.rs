//! The built-in council's checks of the tasks it planned, each done in its own session: code
//! picks the checker by the ladder and records it with its rung, and the check reads the
//! council's frame. With one maker set up, a fresh session of the task's own model checks it,
//! never another model of that maker, and the run says so. No seat and no check reads who
//! sits in which seat or who made the work, in its prompt or through a status tool.

use std::sync::Arc;

use serde_json::json;

use argonauts_core::agents::session_runner::SessionSpec;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::approval::ApprovalEvent;
use argonauts_core::runs::events::council::{CheckPick, CheckRung, CouncilEvent, CouncilEventKind};
use argonauts_core::runs::events::session::{InputPartKind, SessionEvent};
use argonauts_core::runs::events::task::TaskEvent;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::council::SameModelCheck;
use argonauts_core::workflows::steps::StepName;
use argonauts_crew::council::lexicon::Lexicon;
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};

use crate::engine_council_talk::{Asked, asked, drive, one_task, plan, points, stance, turn};
use crate::support::{self, smallest_answer};

/// Every seat agrees with every point, and the plan is one task, the limiter, which its worker
/// builds in its own session.
fn agreeing(spec: &SessionSpec) -> FakeSession {
    match asked(spec) {
        Some(Asked::First) => turn("A token bucket per key.", json!([]), json!({})),
        Some(Asked::Points) => points(),
        Some(Asked::Plan) => plan(),
        Some(Asked::Tapback | Asked::Message) => turn(
            "",
            json!([stance(1, "agree", None), stance(2, "agree", None)]),
            json!({}),
        ),
        Some(Asked::Private) => turn("", json!([]), json!({})),
        None if spec.step == Some(StepName::PlanToTasks) => one_task(),
        None if spec.step == Some(StepName::Build) => {
            let src = spec.workdir.join("src");
            std::fs::create_dir_all(&src).expect("the task's folder takes a file");
            std::fs::write(src.join("limiter.rs"), "pub struct Limiter;\n")
                .expect("the task's folder takes a file");
            FakeSession::structured(json!({
                "status": "done",
                "summary": "Wrote src/limiter.rs",
                "files_changed": ["src/limiter.rs"],
                "tests_run": [],
                "concerns": [],
            }))
        }
        None => smallest_answer(spec),
    }
}

fn council_of(seats: &[(&str, &str)]) -> anyhow::Result<(support::Crew, Vec<Arc<FakeRunner>>)> {
    let mut crew = support::Crew::new()?;
    let mut fakes = Vec::new();
    for &(worker, provider) in seats {
        fakes.push(crew.enlist(FakeRunner::cli(worker, provider).respond_with(agreeing)));
    }
    crew.settings.workflow = "council".to_owned();
    crew.settings.workflows.adapt_at_phase_ends = false;
    crew.settings.divide.contracts_first = false;
    Ok((crew, fakes))
}

fn worker_of_the_task(events: &[RunEvent]) -> ModelRef {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Task(TaskEvent::Created(spec)) => spec.worker.clone(),
            _ => None,
        })
        .expect("the plan made a task with a worker")
}

fn picks(events: &[RunEvent]) -> Vec<CheckPick> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Council(CouncilEvent {
                kind: CouncilEventKind::Check(pick),
                ..
            }) => Some(pick.clone()),
            _ => None,
        })
        .collect()
}

fn checkers(events: &[RunEvent]) -> Vec<ModelRef> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started))
                if started.role == RoleName::Reviewer =>
            {
                Some(started.worker.clone())
            }
            _ => None,
        })
        .collect()
}

fn check_specs(fakes: &[Arc<FakeRunner>]) -> Vec<SessionSpec> {
    fakes
        .iter()
        .flat_map(|fake| fake.specs())
        .filter(|spec| spec.role == RoleName::Reviewer)
        .collect()
}

fn notes(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Note(note) => Some(note.text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_council_task_done_in_its_own_session_is_checked_by_another_maker() -> anyhow::Result<()>
{
    let seats = [("claude-code/opus", "anthropic"), ("codex/gpt-5", "openai")];
    let (crew, fakes) = council_of(&seats)?;
    let (exit, events) = drive(&crew, None).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let worker = worker_of_the_task(&events);
    let other = seats
        .iter()
        .map(|(model, _)| model.parse::<ModelRef>())
        .find(|model| model.as_ref().is_ok_and(|model| *model != worker))
        .expect("two seats")?;
    let picks = picks(&events);
    assert_eq!(picks.len(), 1, "{picks:?}");
    assert_eq!(picks[0].rung, CheckRung::OtherMaker);
    assert_eq!(picks[0].checker.as_ref(), Some(&other));
    assert_eq!(checkers(&events), vec![other]);

    let checks = check_specs(&fakes);
    assert_eq!(checks.len(), 1);
    // The task works in the folder its seat talked in, whose name the check never reads.
    let folder = checks[0].workdir.to_string_lossy().into_owned();
    assert!(!folder.contains("seat-"), "{folder}");
    assert!(
        checks[0]
            .prompt
            .iter()
            .any(|part| part.label == "The council's check"),
        "{:?}",
        checks[0]
            .prompt
            .iter()
            .map(|part| &part.label)
            .collect::<Vec<_>>()
    );
    assert!(
        !notes(&events)
            .iter()
            .any(|note| note.starts_with("Same maker"))
    );
    Ok(())
}

#[tokio::test]
async fn with_one_maker_a_fresh_session_of_the_tasks_own_model_checks_it() -> anyhow::Result<()> {
    let seats = [
        ("claude-code/opus", "anthropic"),
        ("claude-code/sonnet", "anthropic"),
    ];
    let (crew, fakes) = council_of(&seats)?;
    let (exit, events) = drive(&crew, None).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let worker = worker_of_the_task(&events);
    let picks = picks(&events);
    assert_eq!(picks.len(), 1, "{picks:?}");
    assert_eq!(picks[0].rung, CheckRung::SameMaker);
    assert_eq!(picks[0].checker.as_ref(), Some(&worker));
    assert_eq!(checkers(&events), vec![worker.clone()]);
    let checks = check_specs(&fakes);
    assert!(checks.iter().all(|spec| {
        spec.prompt
            .iter()
            .any(|part| part.label == "The council's check")
    }));
    let same_maker = format!(
        "Same maker: {}",
        argonauts_crew::roles::display_name(&worker)
    );
    assert!(
        notes(&events)
            .iter()
            .any(|note| note.starts_with(&same_maker)),
        "{:?}",
        notes(&events)
    );
    Ok(())
}

#[tokio::test]
async fn asked_first_the_plan_names_the_task_its_own_model_will_check() -> anyhow::Result<()> {
    let seats = [
        ("claude-code/opus", "anthropic"),
        ("claude-code/sonnet", "anthropic"),
    ];
    let (mut crew, _fakes) = council_of(&seats)?;
    crew.settings.council.same_model_check = SameModelCheck::Ask;
    let (exit, events) = drive(&crew, None).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let plan_asks: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                Some(serde_json::to_string(opened).unwrap_or_default())
            }
            _ => None,
        })
        .filter(|opened| opened.contains("Same maker: no other maker is set up"))
        .collect();
    assert!(
        plan_asks
            .iter()
            .any(|opened| opened.contains("checks Add the rate limiter")),
        "{plan_asks:?}"
    );
    Ok(())
}

#[tokio::test]
async fn no_seat_or_check_reads_a_model_maker_or_coding_tool() -> anyhow::Result<()> {
    let seats = [("claude-code/opus", "anthropic"), ("codex/gpt-5", "openai")];
    let (crew, fakes) = council_of(&seats)?;
    let (exit, _events) = drive(&crew, None).await?;
    assert!(matches!(exit, EngineExit::Finished(_)), "{exit:?}");

    let models: Vec<ModelRef> = seats
        .iter()
        .map(|(model, _)| model.parse())
        .collect::<Result<_, _>>()?;
    let provider = |model: &ModelRef| match model.agent.as_str() {
        "codex" => "openai".to_owned(),
        _ => "anthropic".to_owned(),
    };
    let lexicon = Lexicon::built_in()
        .with_models(&models, &DisplayNames::default(), &provider)
        .with_topic("add per-key rate limiting to the public API");
    let blind: Vec<SessionSpec> = fakes
        .iter()
        .flat_map(|fake| fake.specs())
        .filter(|spec| spec.step == Some(StepName::Talk) || spec.role == RoleName::Reviewer)
        .collect();
    assert!(blind.iter().any(|spec| spec.step == Some(StepName::Talk)));
    assert!(blind.iter().any(|spec| spec.role == RoleName::Reviewer));
    for spec in &blind {
        assert!(spec.status_file.is_none(), "{spec:?}");
        assert!(
            !spec
                .prompt
                .iter()
                .any(|part| part.kind == InputPartKind::Status),
            "{spec:?}"
        );
        let read: String = spec
            .prompt
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let named: Vec<&str> = lexicon
            .find(&read)
            .into_iter()
            .map(|hit| &read[hit.range])
            .collect();
        assert!(named.is_empty(), "{:?} reads {named:?}", spec.role);
    }
    Ok(())
}
