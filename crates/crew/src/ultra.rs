//! Ultracode, and which engine runs a workflow. Ultracode changes
//! how often a workflow is used, never what one is: the engine follows the crew.

use argonauts_core::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;

use crate::roles;

/// The one CLI with a workflow of its own that Argonauts Code can switch on.
pub const CLAUDE_CODE: &str = "claude-code";

/// Whether the agent's CLI runs workflows of its own: Claude Code does, and Codex and
/// agy have none.
pub fn has_own_workflow(worker: &ModelRef) -> bool {
    worker.agent == CLAUDE_CODE
}

/// A crew whose workflow Claude Code runs as its own: the orchestrator plans it, and its
/// subagents do the work on the workers' model. The reviewers and the judge are asked through
/// Argonauts Code, each on its own model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnCrew {
    pub orchestrator: ModelRef,
    /// The worker whose model Claude Code's subagents run on.
    pub worker: ModelRef,
    pub reviewer: Vec<ModelRef>,
    /// Empty when the judge follows the reviewers, whose answer then signs off too.
    pub judge: Vec<ModelRef>,
}

/// Whether Claude Code's own workflow can stand for this one: a workflow the orchestrator
/// shapes that opens with the orchestrator planning and handing out the work, as Claude Code's
/// does. One with no single planner, or one that runs as written, keeps its own steps.
pub fn plans_and_delegates(file: &argonauts_runner::WorkflowFile) -> bool {
    file.adapt
        && file
            .steps
            .first()
            .is_some_and(|step| step.role == CrewRole::Orchestrator)
}

/// Claude Code's own workflow runs the crew's when the orchestrator and every worker are on
/// Claude Code, whatever their models; otherwise Argonauts Code's runner does.
/// `available` are the agents that can work now, which go first.
pub fn claude_codes_own(settings: &Settings, available: &[ModelRef]) -> Option<OwnCrew> {
    let models_of = |role: CrewRole| roles::candidates(settings, role.fine_role());
    let first = |models: &[ModelRef]| {
        models
            .iter()
            .find(|model| available.contains(model))
            .or(models.first())
            .cloned()
    };
    let orchestrator = first(&models_of(CrewRole::Orchestrator))?;
    let workers = models_of(CrewRole::Worker);
    let all_on_claude_code = !workers.is_empty() && workers.iter().all(has_own_workflow);
    if !has_own_workflow(&orchestrator) || !all_on_claude_code {
        return None;
    }
    let judge_follows = settings.crew.followed(CrewRole::Judge).is_some()
        && settings
            .roles
            .get(&CrewRole::Judge.fine_role())
            .is_none_or(|role| role.models.is_empty());
    Some(OwnCrew {
        orchestrator,
        worker: first(&workers)?,
        reviewer: models_of(CrewRole::Reviewer),
        judge: if judge_follows {
            Vec::new()
        } else {
            models_of(CrewRole::Judge)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(name: &str) -> ModelRef {
        name.parse().unwrap()
    }

    fn crew(crew: serde_json::Value) -> Settings {
        serde_json::from_value(serde_json::json!({ "crew": crew })).unwrap()
    }

    /// Fable orchestrating and Opus working are both Claude Code, so the workflow is Claude
    /// Code's own, and Astra reviews and signs off through Argonauts Code.
    #[test]
    fn claude_code_runs_the_workflow_when_the_orchestrator_and_workers_are_on_it() {
        let settings = crew(serde_json::json!({
            "orchestrator": "claude-code/fable-5-1",
            "worker": ["claude-code/opus-5-5"],
            "reviewer": ["astra"],
        }));
        let own = claude_codes_own(&settings, &[]).expect("Claude Code's own");
        assert_eq!(own.orchestrator, worker("claude-code/fable-5-1"));
        assert_eq!(own.worker, worker("claude-code/opus-5-5"));
        assert_eq!(own.reviewer, [worker("astra")]);
        assert!(own.judge.is_empty(), "the judge follows the reviewers");

        // Workers that follow the orchestrator are on Claude Code too.
        let settings = crew(serde_json::json!({
            "orchestrator": "claude-code/opus-5-5",
            "reviewer": ["astra"],
        }));
        assert!(claude_codes_own(&settings, &[]).is_some());
    }

    #[test]
    fn workers_elsewhere_or_an_orchestrator_elsewhere_go_to_the_runner() {
        let mixed = crew(serde_json::json!({
            "orchestrator": "claude-code/opus-5-5",
            "worker": ["claude-code/opus-5-5", "codex"],
        }));
        assert!(claude_codes_own(&mixed, &[]).is_none());
        let codex = crew(serde_json::json!({"orchestrator": "codex", "worker": ["claude-code"]}));
        assert!(claude_codes_own(&codex, &[]).is_none());
        // The default crew spreads its workers across three CLIs.
        assert!(claude_codes_own(&Settings::default(), &[]).is_none());
    }

    /// The orchestrator is the first of its models that can work now, so one that can't is
    /// passed over for the Claude Code model after it, and whoever asks must say what can work.
    #[test]
    fn an_orchestrator_that_cannot_work_now_is_passed_over() {
        let settings = crew(serde_json::json!({
            "orchestrator": ["codex", "claude-code/opus-5-5"],
            "worker": ["claude-code/opus-5-5"],
        }));
        let available = [worker("claude-code/opus-5-5")];
        let own = claude_codes_own(&settings, &available).expect("Claude Code's own");
        assert_eq!(own.orchestrator, worker("claude-code/opus-5-5"));
        assert!(claude_codes_own(&settings, &[]).is_none());
    }

    /// Claude Code's own workflow plans and delegates, which is what the orchestrator's does;
    /// a council, a maker and its checker, and several tries with a pick run their own steps.
    #[test]
    fn only_a_workflow_that_plans_and_delegates_becomes_claude_codes_own() {
        let planned: Vec<&str> = argonauts_runner::built_ins()
            .filter(|(_, text)| {
                let file = argonauts_runner::WorkflowFile::parse(text)
                    .unwrap()
                    .unwrap();
                plans_and_delegates(&file)
            })
            .map(|(name, _)| name)
            .collect();
        assert_eq!(planned, ["orchestrator"]);
    }

    #[test]
    fn only_claude_code_has_a_workflow_of_its_own() {
        assert!(has_own_workflow(&"claude-code/opus-5-5".parse().unwrap()));
        assert!(!has_own_workflow(&"codex".parse().unwrap()));
        assert!(!has_own_workflow(&"antigravity-cli".parse().unwrap()));
    }
}
