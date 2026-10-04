//! Claude Code adapter for Argonauts.

mod adapter;
mod command;
mod commands;
mod context;
mod errors;
mod json;
mod keywords;
mod parser;
mod quota;
mod script_meta;
mod sessions;
mod settings;
mod subagents;
mod todo;
mod tweaks;
mod usage;
mod workflows;

pub use adapter::{ClaudeCodeAdapter, TESTED_VERSIONS};
pub use command::SETTINGS_FILE;
pub use commands::BUILTIN_COMMANDS;
pub use keywords::KEYWORDS;
pub use parser::ClaudeStreamParser;
pub use settings::STATUS_SERVER;
