//! What a Claude Code mod pushes, shown where Claude Code shows it: a toast
//! on the bottom line's notice for its time, a log line dim in the transcript under the plugin's
//! name, and the CLI's own notices. A status line is kept in the conversation state and drawn among
//! the chips. `ui.plugin_output = "transcript"` makes each a transcript line, and `"off"` shows
//! none. The notice line is your conversation's: a crew session's pushes stay in its agent's block.

use std::time::Duration;

use argonauts_core::PluginOutputKind;
use argonauts_core::settings::ui::PluginOutput;

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
    kind: PluginOutputKind,
    plugin: Option<&str>,
    text: Option<&str>,
    cx: &Cx<'_>,
) -> Option<ResultLine> {
    let text = sanitize_line(text?.trim()).into_owned();
    if text.is_empty() || cx.settings.plugin_output == PluginOutput::Off {
        return None;
    }
    let named = match plugin.filter(|plugin| !plugin.is_empty()) {
        Some(plugin) => format!("{}: {text}", sanitize_line(plugin)),
        None => text,
    };
    match kind {
        // Drawn among your conversation's chips only.
        PluginOutputKind::Status => None,
        PluginOutputKind::Warning | PluginOutputKind::Urgent => {
            Some(ResultLine::new(Tone::Warning, named))
        }
        PluginOutputKind::Toast
        | PluginOutputKind::Notice
        | PluginOutputKind::Log
        | PluginOutputKind::Info => Some(ResultLine::new(Tone::Dim, named)),
    }
}

/// What a push shows. `Some` is a line for the transcript, under the item it came with.
pub fn show(
    kind: PluginOutputKind,
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
    match (cx.settings.plugin_output, kind) {
        (PluginOutput::Off, _) => None,
        (PluginOutput::Transcript, PluginOutputKind::Warning) => {
            Some(ResultLine::new(Tone::Warning, named))
        }
        (PluginOutput::Transcript, _) => Some(ResultLine::new(Tone::Dim, named)),
        (
            PluginOutput::Show,
            PluginOutputKind::Toast | PluginOutputKind::Notice | PluginOutputKind::Urgent,
        ) => {
            // A notice from an earlier session says nothing now.
            if !cx.replaying() {
                let tone = if kind == PluginOutputKind::Urgent {
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
        (PluginOutput::Show, PluginOutputKind::Status) => None,
        (PluginOutput::Show, PluginOutputKind::Log) => Some(ResultLine::new(Tone::Dim, named)),
        (PluginOutput::Show, PluginOutputKind::Info) => Some(ResultLine::new(Tone::Dim, text)),
        (PluginOutput::Show, PluginOutputKind::Warning) => {
            Some(ResultLine::new(Tone::Warning, text))
        }
    }
}
