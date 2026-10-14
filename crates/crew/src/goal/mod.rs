//! Goals as conditions, "keep going until this is true": `check` decides whether one holds
//! (one entry point, [`check::goal`]), `pursue` keeps one around a run's workflow, `task` keeps
//! one around a single task, `cli` hands one to a CLI that has a goal of its own, `now` runs
//! a task's checks when you ask, beside its try, and `until` answers a workflow whose own
//! steps make the tries: a step's `until`, or the `goal` at the top of its file.

pub mod check;
pub mod cli;
pub mod now;
pub mod pursue;
pub mod report;
pub mod task;
pub mod until;
