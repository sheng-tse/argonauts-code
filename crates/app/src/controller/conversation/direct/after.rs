//! What a direct turn's event leaves after it, drawn after the CLI's own words: the line a
//! write outside your folder or a refused call leaves, then the warning macOS keeping the CLI
//! out of your folder calls for.

use argonauts_core::NoteLevel;
use argonauts_core::agents::session::SessionRole;
use argonauts_core::runs::event::AgentEvent;
use argonauts_core::safety::outside::{Outside, both};
use serde_json::Value;

use super::{DirectState, Running, in_your_folder, macos};
use crate::controller::Ctx;
use crate::controller::conversation::Turn;

pub(in crate::controller::conversation) struct LinesAfter {
    line: Option<String>,
    blocked: Option<macos::Blocked>,
}

impl LinesAfter {
    /// Follows `event` of `turn`, a failed call that says macOS kept its CLI out losing its
    /// raw words.
    pub(in crate::controller::conversation) fn of(
        direct: &mut DirectState,
        turn: &mut Turn,
        event: &mut AgentEvent,
    ) -> Self {
        let line = turn
            .direct
            .as_mut()
            .and_then(|running| on_agent(running, &turn.outside, event));
        let blocked = macos::follow(direct, turn.direct.as_ref(), &turn.worker, event);
        Self { line, blocked }
    }

    pub(in crate::controller::conversation) fn show(self, ctx: &mut Ctx) {
        if let Some(line) = self.line {
            ctx.notice(NoteLevel::Quiet, line);
        }
        if let Some(blocked) = self.blocked {
            ctx.notice(NoteLevel::Ask, blocked.text(&ctx.names.names()));
        }
    }
}

/// Follows the turn's events for what comes after it: agy's refused commands, the writes outside
/// that were turned away where the turn reads there but writes only its folder, and what the
/// agent said, for a fallback. Gives the line a write outside your folder leaves, by a file
/// tool or a command, once it's done, and the one a read or write the never-list refused leaves.
fn on_agent(running: &mut Running, outside: &Outside, event: &AgentEvent) -> Option<String> {
    let home = running.home.as_deref();
    running.before_writes.note(&running.root, event, home);
    let wrote = running
        .before_writes
        .changed_by_command(&[&running.root], event, home);
    let kept_out = running
        .kept_out
        .follow(&outside.never, &running.root, event, home);
    if outside.read && !outside.writes_outside(SessionRole::Direct) {
        running
            .kept_in
            .follow(outside, &running.root, event, home, &running.before_writes);
    }
    match event {
        AgentEvent::ToolStarted { id, input, .. } => {
            let command = ["CommandLine", "command", "cmd"]
                .iter()
                .find_map(|key| input.get(*key).and_then(Value::as_str));
            if let Some(command) = command {
                running.commands.insert(id.clone(), command.to_owned());
            }
        }
        AgentEvent::FileChanged { path, .. } => {
            running.written.extend(in_your_folder(&running.root, path));
            return running
                .before_writes
                .changed_as_reported(&[&running.root], event, home);
        }
        AgentEvent::ToolFinished {
            id,
            ok: false,
            output,
            parent: None,
        } => {
            let refused = output.as_deref().is_some_and(|output| {
                output.contains("permission check failed")
                    || output.contains("required approval")
                    || output.contains("auto-denied")
            });
            if let Some(command) = running.commands.get(id).filter(|_| refused)
                && !running.refused.contains(command)
            {
                running.refused.push(command.clone());
            }
        }
        AgentEvent::TextDelta { text, parent: None } => running.said.push_str(text),
        AgentEvent::Message { text, parent: None } if running.said.is_empty() => {
            running.said.push_str(text);
        }
        _ => {}
    }
    both(kept_out, wrote)
}
