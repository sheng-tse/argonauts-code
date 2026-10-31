//! Approvals, hard limits, the judge, and the live narrator. Every decision point
//! checks the hard limits first, and only then does your mode and `decide_with` pick who
//! answers. Agents never approve anything, and nothing here can raise autonomy.

use std::path::Path;
use std::sync::Arc;

use argonauts_core::RunId;
use argonauts_core::agents::session_runner::RunnerDirectory;
use argonauts_core::base::clock::{Clock, SystemClock};
use argonauts_core::models::registry::ModelRegistry;
use argonauts_core::safety::policy::{ApprovalPolicy, Narrator};
use argonauts_core::settings::Settings;
use argonauts_core::workflows::steps::StepRunner;

pub mod approvals;
pub mod autonomy;
pub mod judge;
pub mod limits;
pub mod narrator;
pub mod rules;
pub mod sessions;

/// Re-reads the settings, so each approval can use the stricter of the run-start and current
/// approval settings.
pub type SettingsReloader = Arc<dyn Fn() -> Option<Settings> + Send + Sync>;

/// The policy a run's run host hands the engine. `state_dir` is the repo's
/// `~/.argonauts/state/<repo>`, where auto preview records and spot checks live. It must already
/// exist, or every auto approval asks, since a lost record could raise autonomy. An unset
/// judge is picked from each agent's cheapest model; [`approval_policy_with_models`] adds the
/// live model lists.
pub fn approval_policy(
    settings: Arc<Settings>,
    reload: SettingsReloader,
    state_dir: &Path,
    workers: Arc<dyn RunnerDirectory>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn ApprovalPolicy> {
    build(settings, reload, state_dir, workers, None, steps)
}

/// [`approval_policy`], picking an unset judge from `models`' live lists as well.
pub fn approval_policy_with_models(
    settings: Arc<Settings>,
    reload: SettingsReloader,
    state_dir: &Path,
    workers: Arc<dyn RunnerDirectory>,
    models: Arc<dyn ModelRegistry>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn ApprovalPolicy> {
    build(settings, reload, state_dir, workers, Some(models), steps)
}

fn build(
    settings: Arc<Settings>,
    reload: SettingsReloader,
    state_dir: &Path,
    workers: Arc<dyn RunnerDirectory>,
    models: Option<Arc<dyn ModelRegistry>>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn ApprovalPolicy> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let source = approvals::settings::SettingsSource::new(settings, reload);
    let limits = Arc::new(limits::BuiltinLimits::new(source.clone(), workers.clone()));
    let mut judge = judge::StepJudge::new(workers, steps, state_dir.to_owned());
    if let Some(models) = models {
        judge = judge.with_models(models);
    }
    let judge = Arc::new(judge);
    let autonomy = Arc::new(autonomy::StateAutonomy::open(state_dir, clock.clone()));
    Arc::new(approvals::ApprovalsPolicy::from_parts(
        approvals::parts::PolicyParts {
            source,
            limits,
            judge,
            autonomy,
            clock,
        },
    ))
}

/// The narrator for one run, or for the app when `run` has no events yet.
pub fn narrator(
    settings: Arc<Settings>,
    run: RunId,
    run_dir: &Path,
    workers: Arc<dyn RunnerDirectory>,
    steps: Arc<dyn StepRunner>,
) -> Arc<dyn Narrator> {
    narrator::LiveNarrator::start(settings, run, run_dir.to_owned(), workers, steps)
}

// These go through the real approvals, autonomy, and limits parts, not fakes.
#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::base::names::ApprovalId;
    use argonauts_core::runs::events::approval::ApprovalKind;
    use argonauts_core::safety::policy::{
        DecisionPoint, EvidenceFacts, EvidencePacket, NarratorError, PolicyOutcome, Verdict,
    };
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith, SandboxChoice};
    use argonauts_core::settings::ui::NarratorMode;
    use argonauts_core::workflows::steps::SchemaStepRunner;
    use argonauts_testkit::FakeDirectory;

    fn point(kind: ApprovalKind) -> DecisionPoint {
        DecisionPoint {
            run: RunId::new("run-01").unwrap(),
            approval: ApprovalId::numbered(1),
            kind,
            learned_workflow: None,
            authors: Vec::new(),
            mode_override: None,
            main_agent: None,
        }
    }

    fn evidence(kind: ApprovalKind, facts: EvidenceFacts) -> EvidencePacket {
        EvidencePacket {
            kind,
            goal: None,
            facts,
            agent_outputs: Vec::new(),
            documents: Vec::new(),
        }
    }

    fn policy(settings: Settings, home: &Path) -> Arc<dyn ApprovalPolicy> {
        // A run's state folder exists before its first approval.
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
        settings.approvals.understand.mode = ApprovalMode::Auto;
        settings.approvals.understand.decide_with = DecideWith::Rules;
        let policy = policy(settings, home.path());

        let no_questions = EvidenceFacts {
            open_questions: Some(0),
            ..EvidenceFacts::default()
        };
        let goal = evidence(ApprovalKind::Understand, no_questions);
        match policy
            .evaluate(&point(ApprovalKind::Understand), &goal)
            .await
        {
            PolicyOutcome::Decided { verdict, by, .. } => {
                assert_eq!((verdict, by), (Verdict::Approve, DecideWith::Rules));
            }
            other => panic!("{other:?}"),
        }

        // A merge packet that names no commits is a hard-limit failure, so it asks.
        let merge = evidence(ApprovalKind::Merge, EvidenceFacts::default());
        match policy.evaluate(&point(ApprovalKind::Merge), &merge).await {
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
