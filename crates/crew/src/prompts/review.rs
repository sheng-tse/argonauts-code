//! Review: reviewers, the judge settling them, and endpoint reviewers writing acceptance tests.
//! Fixed core: no setting changes them.

pub const REVIEW: &str = argonauts_core::template!("prompts/review/review.md");
pub const SETTLE: &str = argonauts_core::template!("prompts/review/settle.md");
pub const ACCEPTANCE_TESTS: &str = argonauts_core::template!("prompts/review/acceptance-tests.md");
