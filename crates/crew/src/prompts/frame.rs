//! The frame each crew role reads first: who it is, what it may touch, and the shape of its
//! answer. A frame stays short, because the instructions that follow say what to do.

pub const MAIN: &str = argonauts_core::template!("prompts/frames/main.md");
pub const ORCHESTRATOR: &str = argonauts_core::template!("prompts/frames/orchestrator.md");
pub const WORKER: &str = argonauts_core::template!("prompts/frames/worker.md");
pub const REVIEWER: &str = argonauts_core::template!("prompts/frames/reviewer.md");
pub const JUDGE: &str = argonauts_core::template!("prompts/frames/judge.md");

/// Where a frame of yours may name whoever wrote the work. The built-in reviewers' and judge's
/// frames say "You aren't told who wrote it" instead, and leave this out.
pub const AUTHOR: &str = "{{author}}";

/// Where the workers' frame says whether anyone else is building beside this worker.
pub const OTHERS: &str = "{{others}}";
pub const OTHERS_BUILD_TOO: &str =
    "Other workers build the run's other tasks, each in its own worktree.";
pub const NOBODY_ELSE_BUILDS: &str = "You make this change on your own.";
