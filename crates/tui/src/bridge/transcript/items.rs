//! Single lines from the app rather than a run: the startup header, notices, recaps, narrator
//! lines, and settings you changed.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, StatusLine};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::WorkerRef;
use argonauts_core::policy::{NarratorLine, NarratorLineKind};
use argonauts_core::settings::SettingScope;

use crate::cell::{AgentReady, Cell, Notice, Recap, ResultLine, SessionHeader, Tone};
use crate::format;
use crate::history::CellId;
use crate::views::settings::words::model_role;

use super::{Cx, emit, quiet};

#[derive(Debug, Default)]
pub(super) struct Items {
    header: Header,
    /// The header last printed, so a welcome that changes nothing prints nothing.
    shown: Option<SessionHeader>,
    /// The controller repeats some warnings, such as the always-on agent's, and each shows once.
    warnings_seen: BTreeSet<String>,
}

/// The controller sends the status line with the default agent just after the welcome, so the
/// header waits in the live region for it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Header {
    #[default]
    NotYet,
    Waiting(CellId),
    Printed,
}

impl Items {
    /// The first history cell. A later welcome comes when the setup saves, with the new agent
    /// and preset, and changes the header printed rather than printing another.
    pub(super) fn welcome(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        let AppEvent::Welcome {
            version,
            repo,
            preset,
            agents,
            runs_going,
            default_agent,
            effort,
            ..
        } = event
        else {
            return;
        };
        // An older app's welcome names no agent, so the status line's fills it in.
        let default_agent = default_agent
            .as_ref()
            .or_else(|| cx.state.status.as_ref().map(|status| &status.default_agent));
        let names = &cx.state.names;
        let header = SessionHeader {
            app: "Argonauts".to_owned(),
            version: version.clone(),
            default_agent: default_agent.map(|agent| names.harness(&agent.worker)),
            model: default_agent
                .and_then(|agent| names.model(&agent.worker, agent.model.as_deref())),
            effort: effort.clone(),
            preset: preset.clone(),
            folder: repo.clone(),
            agents: agents
                .iter()
                .map(|agent| AgentReady {
                    name: names.harness(&agent.id),
                    ready: agent.ready,
                    note: if agent.ready {
                        agent.version.clone()
                    } else {
                        agent.note.clone()
                    },
                })
                .collect(),
            runs_going: *runs_going,
        };
        match self.header {
            Header::NotYet if default_agent.is_none() => {
                self.header = Header::Waiting(cx.model.history.push_active(header));
            }
            Header::NotYet => {
                self.shown = Some(header.clone());
                cx.model.history.push(header);
                self.header = Header::Printed;
            }
            Header::Waiting(id) => {
                if let Some(Cell::Header(waiting)) = cx.model.history.active_mut(id) {
                    *waiting = header;
                }
                if default_agent.is_some() {
                    self.print_header(cx);
                }
            }
            Header::Printed if self.shown.as_ref() == Some(&header) => {}
            Header::Printed => {
                // A second header would stack under the first, so the one printed changes
                // where it stands.
                self.shown = Some(header.clone());
                cx.model.history.replace_header(header);
            }
        }
    }

    /// Fills in the default agent and prints the header waiting for it.
    pub(super) fn status(&mut self, status: &StatusLine, cx: &mut Cx<'_>) {
        let Header::Waiting(id) = self.header else {
            return;
        };
        let agent = &status.default_agent;
        let cli = cx.state.names.harness(&agent.worker);
        let model = cx.state.names.model(&agent.worker, agent.model.as_deref());
        if let Some(Cell::Header(header)) = cx.model.history.active_mut(id) {
            header.default_agent = Some(cli);
            header.model = model;
        }
        self.print_header(cx);
    }

    /// Prints a header still waiting for the status line, so nothing prints above it.
    pub(super) fn print_header(&mut self, cx: &mut Cx<'_>) {
        if let Header::Waiting(id) = self.header {
            if let Some(Cell::Header(header)) = cx.model.history.active(id) {
                self.shown = Some(header.clone());
            }
            cx.model.history.finish_active(id);
            self.header = Header::Printed;
        }
    }

    /// `AppEvent::Notice` and a run's `Note`: dim, or in the warning or error color.
    pub(super) fn notice(&mut self, level: NoteLevel, text: &str, cx: &mut Cx<'_>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let tone = match level {
            NoteLevel::Info if quiet(cx) => return,
            NoteLevel::Info => Tone::Dim,
            NoteLevel::Warn => {
                if !self.warnings_seen.insert(text.to_owned()) {
                    return;
                }
                Tone::Warning
            }
            NoteLevel::Error => Tone::Error,
        };
        emit(cx, Notice::new(tone, text));
    }
}

/// A command's findings, such as `/doctor`'s: the title, then one dim line each under `⎿`.
/// You asked for it, so it prints even at `quiet`.
pub(super) fn report(title: &str, lines: &[String], cx: &mut Cx<'_>) {
    let mut notice = Notice::new(Tone::Normal, title.trim());
    for line in lines {
        notice = notice.with_detail(line.trim_end());
    }
    emit(cx, notice);
}

/// `※ recap: … (disable recaps in /config)`, unless recaps are off.
pub(super) fn recap(text: &str, cx: &mut Cx<'_>) {
    if !cx.settings.recaps || quiet(cx) || text.trim().is_empty() {
        return;
    }
    emit(cx, Recap::new(text.trim()));
}

/// Narration at a phase's end and answers to `/ask` and summaries go to scrollback; interval
/// updates only replace the live line.
pub(super) fn narrator(line: &NarratorLine, cx: &mut Cx<'_>) {
    let asked = match &line.kind {
        NarratorLineKind::Update => return,
        NarratorLineKind::PhaseEnd { .. } | NarratorLineKind::Notice => false,
        NarratorLineKind::Answer { .. } | NarratorLineKind::Summary => true,
    };
    if (!asked && quiet(cx)) || line.text.trim().is_empty() {
        return;
    }
    emit(cx, Recap::narrator(line.text.trim()));
}

/// `● Set approvals.merge.mode = auto · this project · was supervised`, or after a reset
/// `● Reset ui.theme · everywhere · now dark`. A model or an effort, as `/model` and `/effort`
/// set them, reads like Claude Code's `⎿  Set model to Opus 5.5`.
pub(super) fn setting_changed(
    key: &str,
    value: &str,
    scope: SettingScope,
    previous: Option<&str>,
    reset: bool,
    cx: &mut Cx<'_>,
) {
    let words = Words {
        picked: &cx.state.model_words,
        names: &cx.state.names,
    };
    if !reset && let Some(line) = command_result(key, value, scope, previous, &words) {
        emit(cx, Cell::Custom(Box::new(line)));
        return;
    }
    let value = format::setting_value(value);
    let value = if value.is_empty() { "unset" } else { &value };
    let text = if reset {
        format!("Reset {key} · {} · now {value}", scope_words(scope))
    } else {
        let mut text = format!("Set {key} = {value} · {}", scope_words(scope));
        if let Some(previous) = previous {
            let previous = format::setting_value(previous);
            let previous = if previous.is_empty() {
                "unset"
            } else {
                &previous
            };
            text.push_str(&format!(" · was {previous}"));
        }
        text
    };
    emit(cx, Notice::new(Tone::Success, text));
}

/// `⎿  Set model to Opus 5.5 · just you · was Claude Code's default`, the value by its display
/// name in the accent color, for the keys `/model` and `/effort` write.
fn command_result(
    key: &str,
    value: &str,
    scope: SettingScope,
    previous: Option<&str>,
    names: &Words<'_>,
) -> Option<ResultLine> {
    let (name, model) = match model_role(key) {
        Some(None) => ("model".to_owned(), true),
        Some(Some(role)) => (format!("{}'s model", role.as_str()), true),
        None => {
            let role = key.strip_prefix("roles.")?.strip_suffix(".effort")?;
            let name = match role {
                "default" => "effort".to_owned(),
                role => format!("{role}'s effort"),
            };
            (name, false)
        }
    };
    let words = |toml: &str| {
        if model {
            workers_words(toml, names)
        } else {
            format::setting_value(toml)
        }
    };
    let shown = words(value);
    if shown.is_empty() {
        return None;
    }
    let mut after = format!("· {}", scope_words(scope));
    if let Some(previous) = previous.map(words).filter(|previous| !previous.is_empty()) {
        after.push_str(&format!(" · was {previous}"));
    }
    Some(ResultLine::valued(format!("Set {name} to"), shown).with_after(after))
}

/// How a result line names models: as the model list named the one you picked, else by the
/// app's names.
struct Words<'a> {
    picked: &'a BTreeMap<String, String>,
    names: &'a DisplayNames,
}

/// A worker setting's TOML text as the model list named it, else by the app's names:
/// `"claude-code/claude-opus-5-5"` reads `Opus 5.5` and `"claude-code"` reads `Claude Code's
/// default`. A list names each in turn.
fn workers_words(toml: &str, words: &Words<'_>) -> String {
    let words: Vec<String> = toml
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|item| item.trim().trim_matches(['"', '\'']))
        .filter(|item| !item.is_empty())
        .map(|item| match item.parse::<WorkerRef>() {
            Ok(worker) => words
                .picked
                .get(&worker.to_string())
                .cloned()
                .or_else(|| words.names.model(&worker.worker, worker.model.as_deref()))
                .unwrap_or_else(|| format!("{}'s default", words.names.harness(&worker.worker))),
            Err(_) => item.to_owned(),
        })
        .collect();
    words.join(", ")
}

pub(super) fn scope_words(scope: SettingScope) -> &'static str {
    match scope {
        SettingScope::Session => "this session",
        SettingScope::ProjectLocal => "just you",
        SettingScope::Project => "this project",
        SettingScope::Global => "everywhere",
    }
}
