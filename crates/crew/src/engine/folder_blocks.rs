//! macOS keeping a session's CLI out of your Desktop, Documents or Downloads folder, said once
//! per run and CLI, in place of the CLI's own words.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use argonauts_core::agents::session_runner::RunnerEvent;
use argonauts_core::base::names::SessionKey;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::council::{CouncilEvent, CouncilEventKind};
use argonauts_core::safety::macos_folders::{FolderBlocked, TO_LET_IT_IN, terminal_app};
use argonauts_core::{AgentEvent, Note, NoteLevel};

use crate::engine::host::Host;
use crate::engine::shared::lock;

#[derive(Debug)]
pub struct FolderBlocks {
    /// The CLIs already told about, by id.
    told: HashSet<String>,
    /// Your home and the app macOS names for your terminal, read once when the run or the
    /// conversation starts.
    home: Option<PathBuf>,
    terminal: String,
}

impl Default for FolderBlocks {
    /// Reads your home and your terminal from this process's environment.
    fn default() -> Self {
        Self {
            told: HashSet::new(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            terminal: terminal_app(std::env::var("TERM_PROGRAM").ok().as_deref()),
        }
    }
}

/// The words of `event` that can say macOS blocked a folder: a failed call's output or an
/// error. A call that went through shows only what it read, which may quote these words.
pub fn said(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::ToolFinished {
            ok: false,
            output: Some(output),
            ..
        } => Some(output),
        AgentEvent::Error { message, .. } => Some(message),
        _ => None,
    }
}

impl FolderBlocks {
    /// The block `said` shows, from a session working in `workdir`, `home` being your home when
    /// the caller knows it better than the environment did.
    pub fn read(&self, said: &str, workdir: &Path, home: Option<&Path>) -> Option<FolderBlocked> {
        let home = home.or(self.home.as_deref())?;
        FolderBlocked::read(said, workdir, home, &self.terminal)
    }

    pub fn told(&self, cli: &str) -> bool {
        self.told.contains(cli)
    }

    /// True the first time `cli` is told.
    pub fn tell(&mut self, cli: &str) -> bool {
        self.told.insert(cli.to_owned())
    }
}

/// A note's text: who can't read the folder, and where to let its CLI in on the row under it.
pub fn blocked_text(blocked: &FolderBlocked, who: &str, cli: &str) -> String {
    Note::text_with_rows(
        &blocked.words(who, cli),
        &[format!("{TO_LET_IT_IN}{}", blocked.where_to_allow())],
    )
}

impl Host {
    /// What a session's CLI says when macOS keeps it out of a folder, the first time in the
    /// run: a seat's two lines in its talk, or a note for any other session.
    pub(super) fn folder_blocked(
        &self,
        key: &SessionKey,
        event: &RunnerEvent,
    ) -> Option<RunEventKind> {
        let RunnerEvent::Agent(event) = event else {
            return None;
        };
        let said = said(event)?;
        let (worker, seat, step) = self.read(|state| {
            let started = &state.sessions.get(key)?.started;
            let seat = started.seat.clone();
            Some((started.worker.clone(), seat, started.workflow_step.clone()))
        })?;
        let cli = &worker.agent;
        if lock(&self.shared().folder_blocks).told(cli) {
            return None;
        }
        let workdir = lock(&self.shared().outside_writes)
            .folder(key)?
            .to_path_buf();
        let mut blocks = lock(&self.shared().folder_blocks);
        let blocked = blocks.read(said, &workdir, None)?;
        blocks.tell(cli);
        drop(blocks);
        if let (Some(seat), Some(talk)) = (seat, step) {
            let kind = CouncilEventKind::FolderBlocked {
                seat,
                cli: cli.clone(),
                blocked,
            };
            return Some(RunEventKind::Council(CouncilEvent { talk, kind }));
        }
        let names = self.names();
        Some(RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: blocked_text(&blocked, &names.worker(&worker), &names.cli(cli)),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::safety::macos_folders::Folder;

    #[test]
    fn only_a_failed_call_or_an_error_can_say_macos_blocked_a_folder() {
        let quoted = "failed to read directory. macOS privacy settings are blocking Antigravity";
        let finished = |ok| AgentEvent::ToolFinished {
            id: "read".to_owned(),
            ok,
            output: Some(quoted.to_owned()),
            parent: None,
        };
        assert_eq!(said(&finished(true)), None);
        assert_eq!(said(&finished(false)), Some(quoted));
    }

    #[test]
    fn each_cli_is_told_once_per_run() {
        let mut blocks = FolderBlocks::default();
        assert!(blocks.tell("antigravity-cli"));
        assert!(!blocks.tell("antigravity-cli"));
        assert!(blocks.tell("opencode"));
    }

    #[test]
    fn the_note_says_where_to_let_the_cli_in_on_its_row() {
        let blocked = FolderBlocked {
            folder: Folder::Downloads,
            app: "Antigravity".to_owned(),
        };
        let note = Note {
            level: NoteLevel::Warn,
            text: blocked_text(&blocked, "3.8 Flash", "agy"),
        };
        assert_eq!(
            note.line(),
            "3.8 Flash can't read this folder: macOS keeps agy out of Downloads"
        );
        assert_eq!(
            note.rows().collect::<Vec<_>>(),
            [
                "To let it in: System Settings › Privacy & Security › Files and Folders › \
              Antigravity › Downloads Folder"
            ]
        );
    }
}
