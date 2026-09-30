//! Placeholder boards, limits service, handoff planner, and gate runner. They fail closed, and
//! anything with no safe answer panics saying it isn't built.

use std::sync::Arc;

use argonauts_core::Paths;
use argonauts_core::events::card::{CardSpec, GateKind, GateOutcome};
use argonauts_core::events::run::ParkReason;
use argonauts_core::events::{RunEvent, VendorEvent};
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{
    Candidate, CandidateQuery, GateRequest, GateRunner, HandoffPacket, HandoffPlanner, HealthBoard,
    HealthSignal, HealthState, LimitsService, QuotaBoard, QuotaReading, RecoveryAction,
    StopContext, TrackRecords, VendorHealth,
};
use argonauts_core::names::{RoleName, VendorId, WorkerRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::worker::SessionLimits;
use time::OffsetDateTime;
use tokio::sync::broadcast;

const NOT_BUILT: &str = "is not implemented yet";

/// Shared by every process through `~/.argonauts/vendors.json`.
pub fn boards(home: &Paths) -> (Arc<dyn QuotaBoard>, Arc<dyn HealthBoard>) {
    let _ = home;
    let (events, _) = broadcast::channel(16);
    (
        Arc::new(EmptyQuotaBoard),
        Arc::new(UncheckedHealthBoard { events }),
    )
}

pub fn limits_service(
    settings: Arc<Settings>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    handoff: Arc<dyn HandoffPlanner>,
) -> Arc<dyn LimitsService> {
    let _ = (settings, quota, health, handoff);
    Arc::new(EscalatingLimits)
}

pub fn handoff_planner(
    settings: Arc<Settings>,
    registry: Arc<dyn ModelRegistry>,
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    records: Arc<dyn TrackRecords>,
) -> Arc<dyn HandoffPlanner> {
    let _ = (settings, registry, quota, health, records);
    Arc::new(NoCandidates)
}

pub fn gate_runner(settings: Arc<Settings>) -> Arc<dyn GateRunner> {
    let _ = settings;
    Arc::new(FailingGates)
}

/// Holds no readings. Recording one would silently drop it, so that panics instead.
struct EmptyQuotaBoard;

impl QuotaBoard for EmptyQuotaBoard {
    fn observe(&self, reading: QuotaReading) {
        let _ = reading;
        unimplemented!("the quota board {NOT_BUILT}");
    }

    fn get(&self, vendor: &VendorId) -> Option<QuotaReading> {
        let _ = vendor;
        None
    }

    fn all(&self) -> Vec<QuotaReading> {
        Vec::new()
    }

    fn probe(&self, vendor: &VendorId) -> BoxFuture<'_, Option<QuotaReading>> {
        let _ = vendor;
        Box::pin(async { None })
    }
}

struct UncheckedHealthBoard {
    /// Kept so subscribers wait quietly instead of seeing a closed channel.
    events: broadcast::Sender<VendorEvent>,
}

impl HealthBoard for UncheckedHealthBoard {
    fn observe(&self, signal: HealthSignal) {
        let _ = signal;
        unimplemented!("the health board {NOT_BUILT}");
    }

    fn state(&self, vendor: &VendorId) -> VendorHealth {
        VendorHealth {
            vendor: vendor.clone(),
            state: HealthState::Healthy,
            since: OffsetDateTime::now_utc(),
            reason: None,
            next_test_at: None,
            disabled: Some(format!("the health board {NOT_BUILT}")),
        }
    }

    fn all(&self) -> Vec<VendorHealth> {
        Vec::new()
    }

    fn subscribe(&self) -> broadcast::Receiver<VendorEvent> {
        self.events.subscribe()
    }

    fn test_result(&self, vendor: &VendorId, ok: bool) {
        let _ = (vendor, ok);
        unimplemented!("the health board {NOT_BUILT}");
    }
}

/// Escalates every stop, so a card never continues on an unplanned recovery.
struct EscalatingLimits;

impl LimitsService for EscalatingLimits {
    fn session_limits(&self, card: Option<&CardSpec>, role: RoleName) -> SessionLimits {
        let _ = (card, role);
        unimplemented!("session limits {NOT_BUILT}");
    }

    fn after_stop<'a>(&'a self, context: &'a StopContext) -> BoxFuture<'a, RecoveryAction> {
        let _ = context;
        Box::pin(async {
            RecoveryAction::Escalate {
                reason: format!("the limits service {NOT_BUILT}"),
            }
        })
    }

    fn all_out(&self, workers: &[WorkerRef]) -> Option<ParkReason> {
        let _ = workers;
        unimplemented!("the all-vendors-out check {NOT_BUILT}");
    }
}

struct NoCandidates;

impl HandoffPlanner for NoCandidates {
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate> {
        let _ = query;
        Vec::new()
    }

    fn packet(&self, card: &CardSpec, events: &[RunEvent]) -> HandoffPacket {
        let _ = (card, events);
        unimplemented!("handoff packets {NOT_BUILT}");
    }
}

struct FailingGates;

impl GateRunner for FailingGates {
    fn run(&self, request: GateRequest) -> BoxFuture<'_, GateOutcome> {
        Box::pin(async move {
            GateOutcome {
                name: request.name,
                kind: GateKind::Command,
                passed: false,
                command: Some(request.command),
                exit_code: None,
                duration_ms: 0,
                summary: format!("the sandboxed gate runner {NOT_BUILT}"),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: false,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::worker::SandboxProfile;
    use argonauts_core::{CardId, RunId};
    use std::time::Duration;

    #[tokio::test]
    async fn the_placeholders_fail_closed() {
        let home = Paths::new("/nowhere");
        let (quota, health) = boards(&home);
        let vendor = VendorId("codex".into());
        assert!(quota.get(&vendor).is_none());
        assert!(health.state(&vendor).disabled.is_some());
        let gates = gate_runner(Arc::new(Settings::default()));
        let outcome = gates
            .run(GateRequest {
                run: RunId::new("run-01").unwrap(),
                card: CardId::numbered(1),
                name: "tests".into(),
                command: vec!["cargo".into(), "test".into()],
                workdir: "/nowhere".into(),
                sandbox: SandboxProfile::default(),
                timeout: Duration::from_secs(1),
                setup: false,
            })
            .await;
        assert!(!outcome.passed);
        assert!(outcome.summary.contains("not implemented"));
    }
}
