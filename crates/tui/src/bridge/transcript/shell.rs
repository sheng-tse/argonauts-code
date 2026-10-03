//! `!` commands: a live block while one runs, printed into the conversation once it ends.

use argonauts_core::app::ShellEvent;

use crate::cell::{Cell, ShellEnd, ShellRun};
use crate::history::CellId;

use super::Cx;

#[derive(Debug, Default)]
pub struct ShellBlocks {
    /// Each running command's live cell, by the command's id.
    open: Vec<(u32, CellId)>,
}

impl ShellBlocks {
    pub fn on_event(&mut self, event: &ShellEvent, cx: &mut Cx<'_>) {
        match event {
            ShellEvent::Started { id, command } => {
                let cell = cx
                    .model
                    .history
                    .push_active(ShellRun::new(command.clone(), cx.now));
                self.open.push((*id, cell));
            }
            ShellEvent::Output { id, text } => {
                if let Some(run) = self.run(*id, cx) {
                    run.push_output(text);
                }
            }
            ShellEvent::Finished {
                id,
                exit_code,
                stopped,
            } => {
                let end = match (*stopped, *exit_code) {
                    (true, _) => ShellEnd::Stopped,
                    (false, Some(code)) => ShellEnd::Exit(code),
                    (false, None) => ShellEnd::Signal,
                };
                if let Some(run) = self.run(*id, cx) {
                    run.finish(end);
                }
                if let Some(at) = self.open.iter().position(|(open, _)| open == id) {
                    let (_, cell) = self.open.remove(at);
                    cx.model.history.finish_active(cell);
                }
            }
        }
    }

    fn run<'a>(&self, id: u32, cx: &'a mut Cx<'_>) -> Option<&'a mut ShellRun> {
        let (_, cell) = self.open.iter().find(|(open, _)| *open == id)?;
        cx.model
            .history
            .active_mut(*cell)
            .and_then(Cell::as_shell_mut)
    }
}
