//! Understand: restating the request, comparing restatements, and the lead answering the open
//! questions. And a goal's two prompts: the judge reading the work against each line, and the
//! orchestrator planning the next round from what the check found failing.

pub const RESTATE: &str = include_str!("goal/restate.md");
pub const DIFFER: &str = include_str!("goal/differ.md");
pub const ANSWER: &str = include_str!("goal/answer.md");
pub const CHECK: &str = include_str!("goal/check.md");
pub const ATTEMPT: &str = include_str!("goal/attempt.md");
