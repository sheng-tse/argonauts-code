//! Codex adapter for Argonauts.

mod adapter;
mod classify;
mod parser;

pub use adapter::{
    API_KEY_ENV_VARS, CHEAPEST_EFFORT, CHEAPEST_MODEL, CodexAdapter, FALLBACK_CHEAPEST_MODEL, ID,
    OUTPUT_SCHEMA_FILE, SUBSCRIPTION_ENV_VARS, TESTED_VERSIONS,
};
pub use classify::{classify_error, is_retry_notice, retry_reason};
pub use parser::CodexParser;
