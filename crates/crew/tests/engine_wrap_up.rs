//! An agent on its CLI's wrap-up allowance finishes. Its vendor is out for new
//! work from the first sign, but nothing stops the running session or hands its card off.

mod support;

use serde_json::json;

use argonauts_core::engine::EngineExit;
use argonauts_core::events::run::{RunMode, RunOutcome};
use argonauts_core::events::{RunEventKind, VendorEvent};
use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::VendorId;
use argonauts_core::steps::StepName;
use argonauts_core::worker::{SessionSpec, Worker, WorkerEvent};
use argonauts_testkit::worker::{FakeSession, FakeWorker};
use support::{Crew, fill, names};

const BUILT: &str = "VERBOSE.md";
const CLAUDE: &str = "claude-code/opus";
const CODEX: &str = "codex/gpt-x";

fn wrapping_up(vendor: &str, wrap_up: bool) -> QuotaReading {
    QuotaReading {
        vendor: VendorId(vendor.to_owned()),
        status: QuotaStatus::Rejected,
        used: Some(1.0),
        resets_at: Some(time::OffsetDateTime::now_utc() + time::Duration::hours(3)),
        window: Some("five_hour".to_owned()),
        source: QuotaSource::RateLimitEvent,
        observed_at: time::OffsetDateTime::now_utc(),
        detail: None,
        wrap_up,
    }
}

/// Every step answers; a build session reads its vendor out mid-turn, wraps up, and finishes.
fn answer(vendor: &'static str, spec: &SessionSpec) -> FakeSession {
    match (spec.step, &spec.json_schema) {
        (Some(StepName::PlanToCards), _) => FakeSession::structured(json!({
            "cards": [{
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
                .with_event(WorkerEvent::Vendor(VendorEvent::Quota(wrapping_up(
                    vendor, false,
                ))))
                .with_event(WorkerEvent::Vendor(VendorEvent::Quota(wrapping_up(
                    vendor, true,
                ))))
        }
        (_, Some(schema)) => FakeSession::structured(fill(schema)),
        (_, None) => FakeSession::completed("done"),
    }
}

#[tokio::test]
async fn a_session_on_its_wrap_up_allowance_is_neither_stopped_nor_handed_off() -> anyhow::Result<()>
{
    let mut crew = Crew::new()?;
    crew.settings.evolution.propose_every = 0;
    crew.settings.divide.contracts_first = false;
    for (worker, family, vendor) in [(CLAUDE, "claude", "claude-code"), (CODEX, "gpt", "codex")] {
        let fake = crew.services.workers.add(
            FakeWorker::harness(worker, family)
                .respond_with(move |spec: &SessionSpec| answer(vendor, spec)),
        );
        let reference = fake.worker_ref().clone();
        if crew.settings.crew.is_empty() {
            crew.settings.default_agent = reference.clone();
        }
        crew.settings.crew.push(reference);
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
    let handoffs: Vec<String> = names(&events.iter().map(|e| (*e).clone()).collect::<Vec<_>>())
        .into_iter()
        .filter(|name| name.starts_with("handoff"))
        .collect();
    assert!(handoffs.is_empty(), "{handoffs:?}");
    // The vendor was out for new work from the first sign.
    let out = events.iter().any(|event| {
        matches!(
            &event.kind,
            RunEventKind::Vendor(VendorEvent::Quota(reading))
                if reading.status == QuotaStatus::Rejected
        )
    });
    assert!(out, "{:?}", names(&last.events));
    Ok(())
}
