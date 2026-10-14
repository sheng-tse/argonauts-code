//! Ultracode, and which engine runs a workflow. Ultracode changes
//! how often a workflow is used, never what one is: the engine follows the crew.

use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;

use crate::roles;

/// The one CLI with a workflow of its own that Argonauts can switch on.
pub const CLAUDE_CODE: &str = "claude-code";

/// Whether the agent's CLI runs workflows of its own: Claude Code does, and Codex and
/// Antigravity CLI have none.
pub fn has_own_workflow(worker: &WorkerRef) -> bool {
    worker.worker == CLAUDE_CODE
}

/// A crew whose workflow Claude Code runs as its own: the orchestrator plans it, and its
/// subagents do the work on the workers' model. The reviewers and the judge are asked through
/// Argonauts, each on its own model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnCrew {
    pub orchestrator: WorkerRef,
    /// The seat whose model Claude Code's subagents run on.
    pub workers: WorkerRef,
    pub reviewers: Vec<WorkerRef>,
    /// Empty when the judge follows the reviewers, whose answer then signs off too.
    pub judge: Vec<WorkerRef>,
}

/// Claude Code's own workflow runs the crew's when the orchestrator and every worker are on
/// Claude Code, whatever their models; otherwise Argonauts' runner does.
/// `available` are the agents that can work now, which go first.
pub fn claude_codes_own(settings: &Settings, available: &[WorkerRef]) -> Option<OwnCrew> {
    let seats = |role: CrewRole| roles::candidates(settings, role.fine_role());
    let first = |seats: &[WorkerRef]| {
        seats
            .iter()
            .find(|seat| available.contains(seat))
            .or(seats.first())
            .cloned()
    };
    let orchestrator = first(&seats(CrewRole::Orchestrator))?;
    let workers = seats(CrewRole::Workers);
    let all_on_claude_code = !workers.is_empty() && workers.iter().all(has_own_workflow);
    if !has_own_workflow(&orchestrator) || !all_on_claude_code {
        return None;
    }
    let judge_follows = settings.crew.followed(CrewRole::Judge).is_some()
        && settings
            .roles
            .get(&CrewRole::Judge.fine_role())
            .is_none_or(|role| role.workers.is_empty());
    Some(OwnCrew {
        orchestrator,
        workers: first(&workers)?,
        reviewers: seats(CrewRole::Reviewers),
        judge: if judge_follows {
            Vec::new()
        } else {
            seats(CrewRole::Judge)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(name: &str) -> WorkerRef {
        name.parse().unwrap()
    }

    fn crew(crew: serde_json::Value) -> Settings {
        serde_json::from_value(serde_json::json!({ "crew": crew })).unwrap()
    }

    /// Fable orchestrating and Opus working are both Claude Code, so the workflow is Claude
    /// Code's own, and Astra reviews and signs off through Argonauts.
    #[test]
    fn claude_code_runs_the_workflow_when_the_orchestrator_and_workers_are_on_it() {
        let settings = crew(serde_json::json!({
            "orchestrator": "claude-code/fable-5-1",
            "workers": ["claude-code/opus-5-5"],
            "reviewers": ["astra"],
        }));
        let own = claude_codes_own(&settings, &[]).expect("Claude Code's own");
        assert_eq!(own.orchestrator, worker("claude-code/fable-5-1"));
        assert_eq!(own.workers, worker("claude-code/opus-5-5"));
        assert_eq!(own.reviewers, [worker("astra")]);
        assert!(own.judge.is_empty(), "the judge follows the reviewers");

        // Workers that follow the orchestrator are on Claude Code too.
        let settings = crew(serde_json::json!({
            "orchestrator": "claude-code/opus-5-5",
            "reviewers": ["astra"],
        }));
        assert!(claude_codes_own(&settings, &[]).is_some());
    }

    #[test]
    fn workers_elsewhere_or_an_orchestrator_elsewhere_go_to_the_runner() {
        let mixed = crew(serde_json::json!({
            "orchestrator": "claude-code/opus-5-5",
            "workers": ["claude-code/opus-5-5", "codex"],
        }));
        assert!(claude_codes_own(&mixed, &[]).is_none());
        let codex = crew(serde_json::json!({"orchestrator": "codex", "workers": ["claude-code"]}));
        assert!(claude_codes_own(&codex, &[]).is_none());
        // The default crew spreads its workers across three CLIs.
        assert!(claude_codes_own(&Settings::default(), &[]).is_none());
    }

    #[test]
    fn only_claude_code_has_a_workflow_of_its_own() {
        assert!(has_own_workflow(&"claude-code/opus-5-5".parse().unwrap()));
        assert!(!has_own_workflow(&"codex".parse().unwrap()));
        assert!(!has_own_workflow(&"antigravity-cli".parse().unwrap()));
    }
}
