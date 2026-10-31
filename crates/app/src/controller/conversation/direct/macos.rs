//! macOS keeping a direct turn's CLI out of your folder: a `◆` line once per CLI in the
//! conversation, in place of the CLI's own words.

use argonauts_core::base::names::ModelRef;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::event::AgentEvent;
use argonauts_core::safety::macos_folders::FolderBlocked;
use argonauts_crew::engine::folder_blocks::{blocked_text, said};

use super::{DirectState, Running};

/// macOS kept `worker`'s CLI out of a folder, said once per CLI in the conversation.
pub(super) struct Blocked {
    blocked: FolderBlocked,
    worker: ModelRef,
}

impl Blocked {
    /// The warning's text, naming the worker and its CLI as the screen does.
    pub(super) fn text(&self, names: &DisplayNames) -> String {
        let cli = names.cli(&self.worker.agent);
        blocked_text(&self.blocked, &names.worker(&self.worker), &cli)
    }
}

/// What `event` shows when it says macOS kept `worker`'s CLI out of a folder and the
/// conversation hasn't said so for that CLI yet. A failed call that says it loses its raw
/// words, which the warning says better.
pub(super) fn follow(
    direct: &mut DirectState,
    running: Option<&Running>,
    worker: &ModelRef,
    event: &mut AgentEvent,
) -> Option<Blocked> {
    let running = running?;
    let home = running.home.as_deref();
    let blocked = direct
        .folder_blocks
        .read(said(event)?, &running.root, home)?;
    if let AgentEvent::ToolFinished { output, .. } = event {
        *output = None;
    }
    direct.folder_blocks.tell(&worker.agent).then(|| Blocked {
        blocked,
        worker: worker.clone(),
    })
}
