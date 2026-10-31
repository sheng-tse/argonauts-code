//! A worker on its CLI's wrap-up allowance finishes. Its agent is out for new
//! work from the first sign, but nothing stops the running session or gives its task to a fallback.

use crate::support;
use serde_json::json;

use argonauts_core::agents::session_runner::{RunnerEvent, SessionRunner, SessionSpec};
use argonauts_core::base::names::AgentId;
use argonauts_core::models::limits::{PlanUsageReading, PlanUsageSource, PlanUsageStatus};
use argonauts_core::runs::engine::EngineExit;
use argonauts_core::runs::events::run::{RunMode, RunOutcome};
use argonauts_core::runs::events::{AgentStatusEvent, RunEventKind};
use argonauts_core::workflows::steps::StepName;
use argonauts_testkit::engine::runner::{FakeRunner, FakeSession};
use support::{Crew, fill, names};

const BUILT: &str = "VERBOSE.md";
const CLAUDE: &str = "claude-code/opus";
const CODEX: &str = "codex/gpt-x";

fn wrapping_up(agent_id: &str, wrap_up: bool) -> PlanUsageReading {
    PlanUsageReading {
        agent_id: AgentId(agent_id.to_owned()),
        status: PlanUsageStatus::Rejected,
        used: Some(1.0),
        resets_at: Some(time::OffsetDateTime::now_utc() + time::Duration::hours(3)),
        window: Some("five_hour".to_owned()),
        source: PlanUsageSource::RateLimitEvent,
        observed_at: time::OffsetDateTime::now_utc(),
        detail: None,
        wrap_up,
    }
}

/// Every step answers; a build session reads its agent out mid-turn, wraps up, and finishes.
fn answer(agent_id: &'static str, spec: &SessionSpec) -> FakeSession {
    match (spec.step, &spec.json_schema) {
        (Some(StepName::PlanToTasks), _) => FakeSession::structured(json!({
            "tasks": [{
                "key": "flag",
                "title": "The --verbose flag",
                "task": "Add a --verbose flag.",
                "kind": "build",
                "owned_paths": [BUILT],
                "read_only_paths": [],
                "depends_on": [],
                "criteria": [],
            }]
        })),
        (Some(StepName::Build), Some(schema)) => {
            std::fs::write(spec.workdir.join(BUILT), "--verbose\n")
                .expect("the build session writes its file");
            let mut report = fill(schema);
            report["status"] = json!("done");
            FakeSession::structured(report)
                .with_event(RunnerEvent::AgentStatus(AgentStatusEvent::PlanUsage(
                    wrapping_up(agent_id, false),
                )))
                .with_event(RunnerEvent::AgentStatus(AgentStatusEvent::PlanUsage(
                    wrapping_up(agent_id, true),
                )))
        }
        (_, Some(schema)) => FakeSession::structured(fill(schema)),
        (_, None) => FakeSession::completed("done"),
    }
}

#[tokio::test]
async fn a_session_on_its_wrap_up_allowance_is_neither_stopped_nor_fell_back() -> anyhow::Result<()>
{
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    crew.settings.divide.contracts_first = false;
    for (worker, provider, agent_id) in [
        (CLAUDE, "anthropic", "claude-code"),
        (CODEX, "openai", "codex"),
    ] {
        let fake = crew.services.workers.add(
            FakeRunner::cli(worker, provider)
                .respond_with(move |spec: &SessionSpec| answer(agent_id, spec)),
        );
        let reference = fake.model_ref().clone();
        if crew.settings.crew.is_empty() {
            crew.settings.crew.main = reference.clone();
        }
        crew.settings.crew.add(reference);
    }

    let outcomes = crew
        .run_to_end(RunMode::Crew, "add a --verbose flag", 12)
        .await?;
    let last = outcomes.last().expect("an outcome");
    assert_eq!(
        last.exit,
        EngineExit::Finished(RunOutcome::Merged),
        "{:?}",
        names(&last.events)
    );
    let events: Vec<_> = outcomes.iter().flat_map(|o| o.events.iter()).collect();
    let fallbacks: Vec<String> = names(&events.iter().map(|e| (*e).clone()).collect::<Vec<_>>())
        .into_iter()
        .filter(|name| name.starts_with("fallback"))
        .collect();
    assert!(fallbacks.is_empty(), "{fallbacks:?}");
    // The agent was out for new work from the first sign.
    let out = events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(reading))
                if reading.status == PlanUsageStatus::Rejected
        )
    });
    assert!(out, "{:?}", names(&last.events));
    Ok(())
}
