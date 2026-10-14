//! Prompt templates, as Markdown files compiled in, and how an agent's prompt is put together
//! from them. What an agent reads first is the frame for its role, then its instructions (the
//! orchestrator's own words, or the template for its step), then your file for the role and
//! what your request said for it ([`assemble`]). Every template is listed in [`catalog`], so
//! each can be written out and replaced by a file of yours. The session view shows exactly
//! what was sent.

pub mod assemble;
pub mod board;
pub mod catalog;
pub mod divide;
pub mod fallback;
pub mod files;
pub mod frame;
pub mod goal;
pub mod instructions;
pub mod merge;
pub mod plan;
pub mod review;
pub mod task;
pub mod trust;
pub mod understand;
pub mod warnings;
pub mod workflow;
pub mod writer;
