//! How a Claude Code session starts and is steered: its command line, settings, control
//! requests, and what it may do.

pub(crate) mod adapter;
pub(crate) mod command;
pub(crate) mod control;
pub(crate) mod instructions;
pub(crate) mod isolation;
pub(crate) mod outside;
pub(crate) mod settings;
pub(crate) mod tweaks;
