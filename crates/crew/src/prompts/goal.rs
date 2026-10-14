//! A goal's two prompts: the judge reading the work against each line, and the orchestrator
//! planning the next round from what the check found failing.

pub const CHECK: &str = argonauts_core::template!("prompts/goal/check.md");
pub const NEXT_TRY: &str = argonauts_core::template!("prompts/goal/next-try.md");
