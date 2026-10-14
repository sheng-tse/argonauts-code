//! Dividing the work, predictions, the run review, and the closing answer.

pub const PLAN_TO_TASKS: &str = argonauts_core::template!("prompts/divide/plan-to-tasks.md");
pub const PREDICT: &str = argonauts_core::template!("prompts/task/predict.md");
pub const RUN_REVIEW: &str = argonauts_core::template!("prompts/run/review.md");
pub const CLOSE_RUN: &str = argonauts_core::template!("prompts/run/closing-answer.md");
