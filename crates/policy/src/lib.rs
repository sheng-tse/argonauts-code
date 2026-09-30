//! Approvals, hard limits, and the narrator. The placeholders fail closed, so nothing is
//! approved and no build session starts until the real checks exist.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use argonauts_core::events::card::CardSpec;
use argonauts_core::future::BoxFuture;
use argonauts_core::names::WorkerRef;
use argonauts_core::policy::{
    ApprovalPolicy, ApprovalsView, DecisionPoint, EvidencePacket, HardLimit, HardLimits, Narrator,
    NarratorError, NarratorLine, NarratorStatus, PolicyOutcome, UserDecision, Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::steps::StepRunner;
use argonauts_core::worker::{SessionSpec, WorkerDirectory};
use argonauts_core::{RunId, Spend};
use tokio::sync::broadcast;

/// Re-reads the settings, so each checkpoint can use the stricter of the run-start and current
/// approval settings.
pub type SettingsReloader = Arc<dyn Fn() -> Option<Settings> + Send + Sync>;

const NOT_BUILT: &str = "is not implemented yet (approvals and narrator stream)";

pub fn approval_policy(
    settings: Arc<Settings>,
    reload: SettingsReloader,
    state_dir: &Path,
    workers: Arc<dyn WorkerDirectory>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn ApprovalPolicy> {
    let _ = (settings, reload, state_dir, workers, steps);
    Arc::new(PlaceholderPolicy {
        limits: PlaceholderLimits,
    })
}

/// The narrator for one run, or for the app when `run` has no events yet.
pub fn narrator(
    settings: Arc<Settings>,
    run: RunId,
    run_dir: &Path,
    workers: Arc<dyn WorkerDirectory>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn Narrator> {
    let _ = (run, run_dir, workers, steps);
    let model = settings
        .narrator
        .model
        .clone()
        .unwrap_or_else(|| settings.default_agent.clone());
    let (lines, _) = broadcast::channel(16);
    Arc::new(PlaceholderNarrator {
        state: Mutex::new(NarratorState {
            mode: settings.ui.narrator,
            model,
        }),
        cap: settings.narrator.spend_cap.clone(),
        interval_s: settings
            .narrator
            .interval
            .parse()
            .ok()
            .flatten()
            .map_or(0, |d| d.as_secs()),
        lines,
    })
}

fn not_built(what: &str, limit: HardLimit) -> Violation {
    Violation {
        limit,
        detail: format!("{what} {NOT_BUILT}"),
        paths: Vec::new(),
    }
}

struct PlaceholderPolicy {
    limits: PlaceholderLimits,
}

impl ApprovalPolicy for PlaceholderPolicy {
    fn evaluate<'a>(
        &'a self,
        point: &'a DecisionPoint,
        evidence: &'a EvidencePacket,
    ) -> BoxFuture<'a, PolicyOutcome> {
        Box::pin(async move {
            PolicyOutcome::AskUser {
                violations: self.limits.check_point(point, evidence),
                shadow: None,
            }
        })
    }

    fn user_decided(&self, decision: &UserDecision) {
        let _ = decision;
        unimplemented!("shadow records {NOT_BUILT}");
    }

    fn spot_checked(&self, point: &DecisionPoint, correct: bool) {
        let _ = (point, correct);
        unimplemented!("spot checks {NOT_BUILT}");
    }

    fn auto_off(&self) {
        // Already true: the placeholder asks you at every decision point.
    }

    fn hard_limits(&self) -> &dyn HardLimits {
        &self.limits
    }

    fn view(&self) -> ApprovalsView {
        unimplemented!("the approvals view {NOT_BUILT}");
    }
}

/// Fails every check closed, so nothing passes on an unchecked limit.
struct PlaceholderLimits;

impl HardLimits for PlaceholderLimits {
    fn check_point(&self, point: &DecisionPoint, evidence: &EvidencePacket) -> Vec<Violation> {
        let _ = (point, evidence);
        vec![not_built("checking hard limits", HardLimit::GatesFailed)]
    }

    fn check_card_diff(&self, card: &CardSpec, unified_diff: &str) -> Vec<Violation> {
        let _ = (card, unified_diff);
        vec![not_built(
            "the built-in diff checks",
            HardLimit::GatesFailed,
        )]
    }

    fn check_session(&self, spec: &SessionSpec) -> Vec<Violation> {
        let _ = spec;
        vec![not_built(
            "checking a session's sandbox",
            HardLimit::NoSandbox,
        )]
    }
}

struct NarratorState {
    mode: NarratorMode,
    model: WorkerRef,
}

/// Keeps its mode and model so the lane header is right; every request gets an error.
struct PlaceholderNarrator {
    state: Mutex<NarratorState>,
    cap: String,
    interval_s: u64,
    lines: broadcast::Sender<NarratorLine>,
}

impl PlaceholderNarrator {
    fn state(&self) -> std::sync::MutexGuard<'_, NarratorState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn refuse(&self) -> BoxFuture<'_, Result<NarratorLine, NarratorError>> {
        let off = self.state().mode == NarratorMode::Off;
        Box::pin(async move {
            if off {
                Err(NarratorError::Off)
            } else {
                Err(NarratorError::Failed(format!("the narrator {NOT_BUILT}")))
            }
        })
    }
}

impl Narrator for PlaceholderNarrator {
    fn observe(&self, event: &argonauts_core::events::RunEvent) {
        // Nothing is ever narrated, so there is no digest to keep.
        let _ = event;
    }

    fn summary(&self) -> BoxFuture<'_, Result<NarratorLine, NarratorError>> {
        self.refuse()
    }

    fn ask(&self, question: String) -> BoxFuture<'_, Result<NarratorLine, NarratorError>> {
        let _ = question;
        self.refuse()
    }

    fn set_mode(&self, mode: NarratorMode) {
        self.state().mode = mode;
    }

    fn set_model(&self, model: WorkerRef) {
        self.state().model = model;
    }

    fn status(&self) -> NarratorStatus {
        let state = self.state();
        NarratorStatus {
            mode: state.mode,
            model: state.model.clone(),
            spent: Spend::default(),
            cap: self.cap.clone(),
            capped: false,
            interval_s: self.interval_s,
        }
    }

    fn lines(&self) -> broadcast::Receiver<NarratorLine> {
        self.lines.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::checkpoint::CheckpointKind;
    use argonauts_core::names::CheckpointId;
    use argonauts_core::policy::EvidenceFacts;
    use argonauts_core::steps::SchemaStepRunner;
    use argonauts_core::worker::WorkerError;

    struct NoWorkers;

    impl WorkerDirectory for NoWorkers {
        fn get(
            &self,
            worker: &WorkerRef,
        ) -> Result<Arc<dyn argonauts_core::worker::Worker>, WorkerError> {
            Err(WorkerError::Unknown(worker.clone()))
        }
        fn available(&self) -> Vec<WorkerRef> {
            Vec::new()
        }
    }

    fn point() -> DecisionPoint {
        DecisionPoint {
            run: RunId::new("run-01").unwrap(),
            checkpoint: CheckpointId::numbered(1),
            kind: CheckpointKind::Merge,
            recipe: None,
            authors: Vec::new(),
            mode_override: None,
        }
    }

    fn evidence() -> EvidencePacket {
        EvidencePacket {
            kind: CheckpointKind::Merge,
            goal: None,
            facts: EvidenceFacts::default(),
            agent_outputs: Vec::new(),
            documents: Vec::new(),
        }
    }

    #[tokio::test]
    async fn the_placeholder_policy_always_asks_and_fails_closed() {
        let reload: SettingsReloader = Arc::new(|| None);
        let policy = approval_policy(
            Arc::new(Settings::default()),
            reload,
            Path::new("/nowhere"),
            Arc::new(NoWorkers),
            Arc::new(SchemaStepRunner),
        );
        match policy.evaluate(&point(), &evidence()).await {
            PolicyOutcome::AskUser { violations, shadow } => {
                assert_eq!(violations.len(), 1);
                assert!(violations[0].detail.contains("not implemented"));
                assert!(shadow.is_none());
            }
            other => panic!("{other:?}"),
        }
        policy.auto_off();
    }

    #[tokio::test]
    async fn the_placeholder_narrator_refuses_and_keeps_its_settings() {
        let narrator = narrator(
            Arc::new(Settings::default()),
            RunId::new("run-01").unwrap(),
            Path::new("/nowhere"),
            Arc::new(NoWorkers),
            Arc::new(SchemaStepRunner),
        );
        assert!(matches!(
            narrator.summary().await,
            Err(NarratorError::Failed(_))
        ));
        narrator.set_mode(NarratorMode::Off);
        assert!(matches!(
            narrator.ask("why?".into()).await,
            Err(NarratorError::Off)
        ));
        let model: WorkerRef = "ollama/qwen3".parse().unwrap();
        narrator.set_model(model.clone());
        let status = narrator.status();
        assert_eq!((status.mode, status.model), (NarratorMode::Off, model));
        assert_eq!(status.interval_s, 45);
    }
}
