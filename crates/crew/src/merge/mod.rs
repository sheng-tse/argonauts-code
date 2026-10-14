//! Merging tasks and the run. Approved tasks merge into `argonauts/<run>/merged` in dependency
//! order; when every task is in, main is merged in if it moved, the final check runs, and the
//! merge approval decides how the run lands. Only the engine writes git state.

use argonauts_core::TaskId;

use crate::engine::host::Host;
use crate::error::EngineResult;

pub mod absorb;
pub mod branch;
pub mod commit_message;
pub mod conductor;
pub mod copies;
pub mod deliver;
pub mod detached;
pub mod diff;
pub mod final_check;
pub mod in_the_way;
pub mod kept;
pub mod land;
pub mod merge_packet;
pub mod merger;
pub mod step;
pub mod task_merge;
pub mod unborn;
pub mod your_folder;

/// What merging one approved task came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskMerge {
    /// Merged and its affected tests pass.
    Merged { sha: String },
    /// A conflict went to the interface's worker (or the merge chore's) as a
    /// `ResolveConflict` session; the task waits for it.
    Conflict { files: Vec<String> },
    /// The affected tests failed; the task goes back to `Running` with the output.
    TestsFailed { output: String },
}

/// Called by the task pipeline in `Merging`.
pub async fn merge_task(host: &Host, task: &TaskId) -> EngineResult<TaskMerge> {
    task_merge::merge(host, task).await
}
