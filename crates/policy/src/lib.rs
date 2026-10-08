//! Approvals, hard limits, the approver model, and the live narrator. Every decision point
//! checks the hard limits first, and only then does your mode and `decide_with` pick who
//! answers. Agents never approve anything, and nothing here can raise autonomy.

use std::path::Path;
use std::sync::Arc;

use argonauts_core::RunId;
use argonauts_core::clock::{Clock, SystemClock};
use argonauts_core::policy::{ApprovalPolicy, Narrator};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::steps::StepRunner;
use argonauts_core::worker::WorkerDirectory;

pub mod approvals;
pub mod approver;
pub mod autonomy;
pub mod daily;
pub mod limits;
pub mod narrator;
pub mod parts;
pub mod rules;
pub mod session;
pub mod templates;

/// Re-reads the settings, so each checkpoint can use the stricter of the run-start and current
/// approval settings.
pub type SettingsReloader = Arc<dyn Fn() -> Option<Settings> + Send + Sync>;

/// The policy a run's supervisor hands the engine. `state_dir` is the repo's
/// `~/.argonauts/state/<repo>`, where shadow records and spot checks live. It must already
/// exist, or every auto checkpoint asks, since a lost record could raise autonomy. An unset
/// approver is picked from each agent's cheapest model; [`approval_policy_with_models`] adds the
/// live model lists.
pub fn approval_policy(
    settings: Arc<Settings>,
    reload: SettingsReloader,
    state_dir: &Path,
    workers: Arc<dyn WorkerDirectory>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn ApprovalPolicy> {
    build(settings, reload, state_dir, workers, None, steps)
}

/// [`approval_policy`], picking an unset approver from `models`' live lists as well.
pub fn approval_policy_with_models(
    settings: Arc<Settings>,
    reload: SettingsReloader,
    state_dir: &Path,
    workers: Arc<dyn WorkerDirectory>,
    models: Arc<dyn ModelRegistry>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn ApprovalPolicy> {
    build(settings, reload, state_dir, workers, Some(models), steps)
}

fn build(
    settings: Arc<Settings>,
    reload: SettingsReloader,
    state_dir: &Path,
    workers: Arc<dyn WorkerDirectory>,
    models: Option<Arc<dyn ModelRegistry>>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn ApprovalPolicy> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let source = approvals::settings::SettingsSource::new(settings, reload);
    let limits = Arc::new(limits::BuiltinLimits::new(source.clone(), workers.clone()));
    let mut approver = approver::StepApprover::new(workers, steps, state_dir.to_owned());
    if let Some(models) = models {
        approver = approver.with_models(models);
    }
    let approver = Arc::new(approver);
    let autonomy = Arc::new(autonomy::StateAutonomy::open(state_dir, clock.clone()));
    Arc::new(approvals::ApprovalsPolicy::from_parts(parts::PolicyParts {
        source,
        limits,
        approver,
        autonomy,
        clock,
    }))
}

/// The narrator for one run, or for the app when `run` has no events yet.
pub fn narrator(
    settings: Arc<Settings>,
    run: RunId,
    run_dir: &Path,
    workers: Arc<dyn WorkerDirectory>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn Narrator> {
    narrator::LiveNarrator::start(settings, run, run_dir.to_owned(), workers, steps)
}

// These go through the real approvals, autonomy, and limits parts, not fakes.
#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::checkpoint::CheckpointKind;
    use argonauts_core::names::CheckpointId;
    use argonauts_core::policy::{
        DecisionPoint, EvidenceFacts, EvidencePacket, NarratorError, PolicyOutcome, Verdict,
    };
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith, SandboxChoice};
    use argonauts_core::settings::ui::NarratorMode;
    use argonauts_core::steps::SchemaStepRunner;
    use argonauts_testkit::FakeDirectory;

    fn point(kind: CheckpointKind) -> DecisionPoint {
        DecisionPoint {
            run: RunId::new("run-01").unwrap(),
            checkpoint: CheckpointId::numbered(1),
            kind,
            recipe: None,
            authors: Vec::new(),
            mode_override: None,
            lead: None,
        }
    }

    fn evidence(kind: CheckpointKind, facts: EvidenceFacts) -> EvidencePacket {
        EvidencePacket {
            kind,
            goal: None,
            facts,
            agent_outputs: Vec::new(),
            documents: Vec::new(),
        }
    }

    fn policy(settings: Settings, home: &Path) -> Arc<dyn ApprovalPolicy> {
        // A run's state folder exists before its first checkpoint.
        let state_dir = home.join("state").join("repo-01");
        std::fs::create_dir_all(&state_dir).unwrap();
        let reload: SettingsReloader = Arc::new(|| None);
        approval_policy(
            Arc::new(settings),
            reload,
            &state_dir,
            Arc::new(FakeDirectory::new()),
            Arc::new(SchemaStepRunner),
        )
    }

    #[tokio::test]
    async fn approval_policy_wires_the_real_parts() {
        let home = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.sandbox.mode = SandboxChoice::Os;
        settings.approvals.goal.mode = ApprovalMode::Auto;
        settings.approvals.goal.decide_with = DecideWith::Rules;
        let policy = policy(settings, home.path());

        let no_questions = EvidenceFacts {
            open_questions: Some(0),
            ..EvidenceFacts::default()
        };
        let goal = evidence(CheckpointKind::Goal, no_questions);
        match policy.evaluate(&point(CheckpointKind::Goal), &goal).await {
            PolicyOutcome::Decided { verdict, by, .. } => {
                assert_eq!((verdict, by), (Verdict::Approve, DecideWith::Rules));
            }
            other => panic!("{other:?}"),
        }

        // A merge packet that names no commits is a hard-limit failure, so it asks.
        let merge = evidence(CheckpointKind::Merge, EvidenceFacts::default());
        match policy.evaluate(&point(CheckpointKind::Merge), &merge).await {
            PolicyOutcome::AskUser { violations, .. } => assert!(!violations.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn the_narrator_is_off_when_the_settings_say_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.ui.narrator = NarratorMode::Off;
        let narrator = narrator(
            Arc::new(settings),
            RunId::new("run-01").unwrap(),
            dir.path(),
            Arc::new(FakeDirectory::new()),
            Arc::new(SchemaStepRunner),
        );
        assert!(matches!(narrator.summary().await, Err(NarratorError::Off)));
    }
}
