//! Understand: reading the request back, comparing readings, and the main agent answering the open
//! questions.

pub const READ: &str = argonauts_core::template!("prompts/understand/read.md");
pub const COMPARE: &str = argonauts_core::template!("prompts/understand/compare.md");
pub const ANSWER: &str = argonauts_core::template!("prompts/understand/answer.md");
