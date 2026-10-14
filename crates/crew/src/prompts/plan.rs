//! Planning: drafts, maps, critique, revision, experiments, judging, and the orchestrator. The
//! judging rubric and scoring rule are fixed core: no setting changes them.

pub const DRAFT: &str = argonauts_core::template!("prompts/plan/draft.md");
pub const MAP: &str = argonauts_core::template!("prompts/plan/map.md");
pub const CRITIQUE: &str = argonauts_core::template!("prompts/plan/critique.md");
pub const REVISE: &str = argonauts_core::template!("prompts/plan/revise.md");
pub const PROPOSE_EXPERIMENT: &str =
    argonauts_core::template!("prompts/plan/propose-experiment.md");
pub const EXPERIMENT_REPORT: &str = argonauts_core::template!("prompts/plan/experiment-report.md");
pub const JUDGE: &str = argonauts_core::template!("prompts/plan/judge.md");
pub const RUBRIC: &str = argonauts_core::template!("prompts/plan/rubric.md");
pub const MERGE_DRAFTS: &str = argonauts_core::template!("prompts/plan/merge-drafts.md");
pub const MERGE_CHECK: &str = argonauts_core::template!("prompts/plan/merge-check.md");
pub const PICK_DRAFT: &str = argonauts_core::template!("prompts/plan/pick-draft.md");
pub const SYNTHESIZE: &str = argonauts_core::template!("prompts/plan/synthesize.md");
pub const ORCHESTRATE: &str = argonauts_core::template!("prompts/plan/orchestrate.md");
