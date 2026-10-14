//! Argonauts Code's services as the app builds them: the plan usage and health boards, the limits
//! service, the fallback planner, and the check runner.

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::Paths;
use argonauts_core::limits::{
    CheckRunner, FallbackPlanner, HealthBoard, LimitsService, PlanUsageBoard, TrackRecords,
};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::session_runner::RunnerDirectory;
use argonauts_core::settings::Settings;
use argonauts_core::settings::sessions::HealthSettings;

use crate::checks::SandboxedChecks;
use crate::fallback::Planner;
use crate::health::FileHealthBoard;
use crate::plan_usage::FilePlanUsageBoard;
use crate::recovery::Limits;
use crate::sandbox;
use crate::slots::Slots;

/// Shared by every process through `~/.argonauts/agents.json`, with the built-in `health.*`
/// thresholds.
pub fn boards(home: &Paths) -> (Arc<dyn PlanUsageBoard>, Arc<dyn HealthBoard>) {
    (
        Arc::new(FilePlanUsageBoard::new(home)),
        Arc::new(FileHealthBoard::new(home, &HealthSettings::default())),
    )
}

/// Like [`boards`], with your `health.*` thresholds, and with each model's health kept under
/// the one name `models` gives it.
pub fn boards_with(
    home: &Paths,
    health: &HealthSettings,
    models: Arc<dyn ModelRegistry>,
) -> (Arc<dyn PlanUsageBoard>, Arc<dyn HealthBoard>) {
    (
        Arc::new(FilePlanUsageBoard::new(home)),
        Arc::new(FileHealthBoard::new(home, health).naming_models_with(models)),
    )
}

/// The registry lets a retired model move to its successor and the retry ladder tell
/// providers apart, and the runner directory says which agents keep your plugins out of a blind
/// role.
pub fn limits_service(
    settings: Arc<Settings>,
    registry: Arc<dyn ModelRegistry>,
    workers: Arc<dyn RunnerDirectory>,
    plan_usage: Arc<dyn PlanUsageBoard>,
    health: Arc<dyn HealthBoard>,
    fallback: Arc<dyn FallbackPlanner>,
) -> Arc<dyn LimitsService> {
    Arc::new(
        Limits::new(settings, plan_usage, health, fallback)
            .with_registry(registry)
            .with_workers(workers),
    )
}

/// `spread` weighs the concurrency slots every process shares under `~/.argonauts/`. Without a
/// home to find them in, it goes by plan usage and track record alone. With the run's folder, a
/// fallback's note comes with the old session's transcript. The runner directory says which
/// CLIs would refuse to start, so none of them is a candidate.
pub fn fallback_planner(
    settings: Arc<Settings>,
    registry: Arc<dyn ModelRegistry>,
    workers: Arc<dyn RunnerDirectory>,
    plan_usage: Arc<dyn PlanUsageBoard>,
    health: Arc<dyn HealthBoard>,
    records: Arc<dyn TrackRecords>,
    run_dir: Option<PathBuf>,
) -> Arc<dyn FallbackPlanner> {
    let slots = match Paths::from_env() {
        Ok(home) => Some(Slots::new(&home, &settings.concurrency)),
        Err(error) => {
            tracing::warn!(%error, "spread can't see the concurrency slots");
            None
        }
    };
    let mut planner =
        Planner::new(settings, registry, plan_usage, health, records).with_workers(workers);
    if let Some(run_dir) = run_dir {
        planner = planner.with_run_dir(run_dir);
    }
    Arc::new(match slots {
        Some(slots) => planner.with_slots(Arc::new(slots)),
        None => planner,
    })
}

pub fn check_runner(settings: Arc<Settings>) -> Arc<dyn CheckRunner> {
    let sandbox = sandbox::for_settings(&settings.sandbox);
    Arc::new(SandboxedChecks::new(settings, sandbox))
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::events::session::{EndReason, SessionEnd};
    use argonauts_core::events::task::TaskState;
    use argonauts_core::limits::{RecoveryAction, StopContext};
    use argonauts_core::names::RoleName;

    use super::*;
    use crate::fallback::testing::{Fixture, TASK, model, run, task, worker};

    #[tokio::test]
    async fn a_retired_model_falls_back_to_its_successor() {
        let fixture = Fixture::with_models();
        let mut retired = model("claude-code", "opus-4", "anthropic");
        retired.retired = true;
        retired.successor = Some("opus-5".to_owned());
        fixture.registry.add(retired);
        fixture
            .registry
            .add(model("claude-code", "opus-5", "anthropic"));

        let settings = Arc::new(fixture.settings.clone());
        let planner = fallback_planner(
            settings.clone(),
            fixture.registry.clone(),
            Arc::new(argonauts_testkit::FakeDirectory::new()),
            fixture.plan_usage.clone(),
            fixture.health.clone(),
            fixture.records.clone(),
            None,
        );
        let limits = limits_service(
            settings,
            fixture.registry.clone(),
            Arc::new(argonauts_testkit::FakeDirectory::new()),
            fixture.plan_usage.clone(),
            fixture.health.clone(),
            planner,
        );

        let dir = tempfile::tempdir().expect("a temp dir");
        let events = dir.path().join("events.jsonl");
        std::fs::write(&events, "").expect("the journal is written");
        let context = StopContext {
            run: run(),
            task: task(TASK),
            state: TaskState::Running,
            role: RoleName::Worker,
            worker: worker("claude-code/opus-4"),
            end: SessionEnd {
                cli_session: None,
                exit_code: Some(1),
                signal: None,
                duration_ms: 1_000,
                reason: EndReason::ModelUnavailable {
                    model: "opus-4".to_owned(),
                },
                spend: Spend::default(),
                tool_calls: 0,
                result_text: None,
                structured: None,
                last_message: None,
                raw_log: None,
            },
            tries: Vec::new(),
            reassigned_once: false,
            events,
            logged_in: None,
        };

        match limits.after_stop(&context).await {
            RecoveryAction::Fallback { to, .. } => assert_eq!(to.to_string(), "claude-code/opus-5"),
            other => panic!("expected a fallback to the successor, got {other:?}"),
        }
    }
}
