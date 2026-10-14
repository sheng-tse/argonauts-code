//! Codex adapter for Argonauts Code.

mod adapter;
mod classify;
mod command;
mod commands;
mod limit;
mod local_zone;
mod parser;
mod probe;
mod sessions;
mod subagents;
mod tweaks;

pub use adapter::{
    API_KEY_ENV_VARS, CHEAPEST_EFFORT, CHEAPEST_MODEL, CodexAdapter, FALLBACK_CHEAPEST_MODEL, ID,
    OUTPUT_SCHEMA_FILE, SUBSCRIPTION_ENV_VARS, TESTED_VERSIONS,
};
pub use classify::{classify_error, is_retry_notice, retry_reason};
pub use commands::BUILTIN_COMMANDS;
pub use parser::CodexParser;
pub use probe::WARNING_FROM;
pub use tweaks::LATEST_AUTO_COMPACT_TOKENS;
