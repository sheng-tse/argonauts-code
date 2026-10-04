//! Claude Code adapter for Argonauts.

mod adapter;
mod background;
mod command;
mod commands;
mod context;
mod control;
mod errors;
mod instructions;
mod isolation;
mod json;
mod keywords;
mod models;
mod mods;
mod parser;
mod quota;
mod script_meta;
mod sessions;
mod settings;
mod subagents;
mod surface;
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
