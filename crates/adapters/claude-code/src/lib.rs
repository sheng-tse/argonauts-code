//! Claude Code adapter for Argonauts.

mod adapter;
mod command;
mod commands;
mod context;
mod errors;
mod json;
mod parser;
mod quota;
mod sessions;
mod settings;
mod subagents;
mod todo;
mod tweaks;
mod usage;

pub use adapter::{ClaudeCodeAdapter, TESTED_VERSIONS};
pub use command::SETTINGS_FILE;
pub use commands::BUILTIN_COMMANDS;
pub use parser::ClaudeStreamParser;
pub use settings::STATUS_SERVER;
