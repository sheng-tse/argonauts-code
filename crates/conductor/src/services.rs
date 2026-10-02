//! The conductor's services as the app builds them: the quota and health boards, the limits
//! service, the handoff planner, and the gate runner.

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::Paths;
use argonauts_core::limits::{
    GateRunner, HandoffPlanner, HealthBoard, LimitsService, QuotaBoard, TrackRecords,
};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::settings::sessions::HealthSettings;

use crate::gates::SandboxedGates;
use crate::handoff::Planner;
use crate::health::FileHealthBoard;
use crate::quota::FileQuotaBoard;
use crate::recovery::Limits;
use crate::sandbox;
use crate::slots::Slots;

/// Shared by every process through `~/.argonauts/vendors.json`, with the built-in `health.*`
/// thresholds.
pub fn boards(home: &Paths) -> (Arc<dyn QuotaBoard>, Arc<dyn HealthBoard>) {
    boards_with(home, &HealthSettings::default())
}

/// Like [`boards`], with your `health.*` thresholds.
pub fn boards_with(
    home: &Paths,
    health: &HealthSettings,
) -> (Arc<dyn QuotaBoard>, Arc<dyn HealthBoard>) {
    (
        Arc::new(FileQuotaBoard::new(home)),
        Arc::new(FileHealthBoard::new(home, health)),
    )
}

/// The registry lets a retired model hand off to its successor and the retry ladder tell
/// families apart.
pub fn limits_service(
    settings: Arc<Settings>,
    registry: Arc<dyn ModelRegistry>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    handoff: Arc<dyn HandoffPlanner>,
) -> Arc<dyn LimitsService> {
    Arc::new(Limits::new(settings, quota, health, handoff).with_registry(registry))
}

/// `spread` weighs the concurrency slots every process shares under `~/.argonauts/`. Without a
/// home to find them in, it goes by quota and track record alone. With the run's folder, a
/// handoff's packet comes with the old session's transcript.
pub fn handoff_planner(
    settings: Arc<Settings>,
    registry: Arc<dyn ModelRegistry>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    records: Arc<dyn TrackRecords>,
    run_dir: Option<PathBuf>,
) -> Arc<dyn HandoffPlanner> {
    let slots = match Paths::from_env() {
        Ok(home) => Some(Slots::new(&home, &settings.concurrency)),
        Err(error) => {
            tracing::warn!(%error, "spread can't see the concurrency slots");
            None
        }
    };
    let mut planner = Planner::new(settings, registry, quota, health, records);
    if let Some(run_dir) = run_dir {
        planner = planner.with_run_dir(run_dir);
    }
    Arc::new(match slots {
        Some(slots) => planner.with_slots(Arc::new(slots)),
        None => planner,
    })
}

pub fn gate_runner(settings: Arc<Settings>) -> Arc<dyn GateRunner> {
    let sandbox = sandbox::for_settings(&settings.sandbox);
    Arc::new(SandboxedGates::new(settings, sandbox))
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::events::card::CardState;
    use argonauts_core::events::session::{EndReason, SessionEnd};
    use argonauts_core::limits::{RecoveryAction, StopContext};
    use argonauts_core::names::RoleName;

    use super::*;
    use crate::handoff::testing::{CARD, Fixture, card, model, run, worker};

    #[tokio::test]
    async fn a_retired_model_hands_off_to_its_successor() {
        let fixture = Fixture::with_models();
        let mut retired = model("claude-code", "opus-4", "claude");
        retired.retired = true;
        retired.successor = Some("opus-5".to_owned());
        fixture.registry.add(retired);
        fixture
            .registry
            .add(model("claude-code", "opus-5", "claude"));

        let settings = Arc::new(fixture.settings.clone());
        let planner = handoff_planner(
            settings.clone(),
            fixture.registry.clone(),
            fixture.quota.clone(),
            fixture.health.clone(),
            fixture.records.clone(),
            None,
        );
        let limits = limits_service(
            settings,
            fixture.registry.clone(),
            fixture.quota.clone(),
            fixture.health.clone(),
            planner,
        );

        let dir = tempfile::tempdir().expect("a temp dir");
        let events = dir.path().join("events.jsonl");
        std::fs::write(&events, "").expect("the journal is written");
        let context = StopContext {
            run: run(),
            card: card(CARD),
            state: CardState::Running,
            role: RoleName::Owner,
            worker: worker("claude-code/opus-4"),
            end: SessionEnd {
                vendor_session: None,
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
            attempts: Vec::new(),
            reassigned_once: false,
            events,
        };

        match limits.after_stop(&context).await {
            RecoveryAction::HandOff { to, .. } => assert_eq!(to.to_string(), "claude-code/opus-5"),
            other => panic!("expected a handoff to the successor, got {other:?}"),
        }
    }
}
