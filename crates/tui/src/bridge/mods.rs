//! What a Claude Code mod pushes, shown where Claude Code shows it: a toast
//! on the mode line's notice for its time, a log line dim in the transcript under the plugin's
//! name, and the CLI's own notices. A status line is kept in the chat state and drawn among the
//! chips. `ui.mod_output = "transcript"` makes each a transcript line, and `"off"` shows none.
//! The notice line is your conversation's: a crew session's pushes stay in its agent's block.

use std::time::Duration;

use argonauts_core::ModOutputKind;
use argonauts_core::settings::ui::ModOutput;

use crate::bottom::ModeNotice;
use crate::cell::{ResultLine, Tone};
use crate::text::sanitize_line;

use super::Cx;

/// Claude Code's own toast time.
const TOAST_FOR: Duration = Duration::from_secs(4);

/// What a crew session's push shows: a line in its agent's block, dim but for a warning. Every
/// Claude Code session in the crew loads your mods, so none of them takes the notice line or
/// rings the bell.
pub fn in_block(
    kind: ModOutputKind,
    plugin: Option<&str>,
    text: Option<&str>,
    cx: &Cx<'_>,
) -> Option<ResultLine> {
    let text = sanitize_line(text?.trim()).into_owned();
    if text.is_empty() || cx.settings.mod_output == ModOutput::Off {
        return None;
    }
    let named = match plugin.filter(|plugin| !plugin.is_empty()) {
        Some(plugin) => format!("{}: {text}", sanitize_line(plugin)),
        None => text,
    };
    match kind {
        // Drawn among your conversation's chips only.
        ModOutputKind::Status => None,
        ModOutputKind::Warning | ModOutputKind::Urgent => {
            Some(ResultLine::new(Tone::Warning, named))
        }
        ModOutputKind::Toast | ModOutputKind::Notice | ModOutputKind::Log | ModOutputKind::Info => {
            Some(ResultLine::new(Tone::Dim, named))
        }
    }
}

/// What a push shows. `Some` is a line for the transcript, under the item it came with.
pub fn show(
    kind: ModOutputKind,
    plugin: Option<&str>,
    text: Option<&str>,
    timeout_ms: Option<u64>,
    cx: &mut Cx<'_>,
) -> Option<ResultLine> {
    let text = sanitize_line(text?.trim()).into_owned();
    if text.is_empty() {
        return None;
    }
    let named = match plugin.filter(|plugin| !plugin.is_empty()) {
        Some(plugin) => format!("{}: {text}", sanitize_line(plugin)),
        None => text.clone(),
    };
    match (cx.settings.mod_output, kind) {
        (ModOutput::Off, _) => None,
        (ModOutput::Transcript, ModOutputKind::Warning) => {
            Some(ResultLine::new(Tone::Warning, named))
        }
        (ModOutput::Transcript, _) => Some(ResultLine::new(Tone::Dim, named)),
        (ModOutput::Show, ModOutputKind::Toast | ModOutputKind::Notice | ModOutputKind::Urgent) => {
            // A notice from an earlier session says nothing now.
            if !cx.replaying() {
                let tone = if kind == ModOutputKind::Urgent {
                    cx.out.ring();
                    Tone::Warning
                } else {
                    Tone::Dim
                };
                let shown_for = timeout_ms.map_or(TOAST_FOR, Duration::from_millis);
                cx.model.bottom.mode.notice = Some(ModeNotice {
                    text: named,
                    tone,
                    until: cx.now + shown_for,
                });
            }
            None
        }
        (ModOutput::Show, ModOutputKind::Status) => None,
        (ModOutput::Show, ModOutputKind::Log) => Some(ResultLine::new(Tone::Dim, named)),
        (ModOutput::Show, ModOutputKind::Info) => Some(ResultLine::new(Tone::Dim, text)),
        (ModOutput::Show, ModOutputKind::Warning) => Some(ResultLine::new(Tone::Warning, text)),
    }
}
