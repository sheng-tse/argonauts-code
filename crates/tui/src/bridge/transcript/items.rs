//! Single lines from the app rather than a run: the startup header, notices, recaps, narrator
//! lines, and settings you changed.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::app::{AppEvent, StatusLine};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::log::NO_SECOND_TRY;
use argonauts_core::names::WorkerRef;
use argonauts_core::policy::{NarratorLine, NarratorLineKind};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::Settings;
use argonauts_core::{Note, NoteLevel};

use crate::cell::{AgentReady, Cell, HungLine, Notice, Recap, ResultLine, SessionHeader, Tone};
use crate::format;
use crate::history::CellId;
use crate::views::settings::words::{model_role, word_for_a_model};

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
            workflow,
            agents,
            runs_going,
            runs_need_you,
            main_agent,
            effort,
            ..
        } = event
        else {
            return;
        };
        // An older app's welcome names no agent, so the status line's fills it in.
        let main_agent = main_agent
            .as_ref()
            .or_else(|| cx.state.status.as_ref().map(|status| &status.main_agent));
        let names = &cx.state.names;
        let header = SessionHeader {
            app: "Argonauts Code".to_owned(),
            version: version.clone(),
            main_agent: main_agent.map(|agent| names.harness(&agent.worker)),
            model: main_agent.and_then(|agent| names.model(&agent.worker, agent.model.as_deref())),
            effort: effort.clone(),
            workflow: workflow.clone(),
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
            runs_need_you: *runs_need_you,
        };
        match self.header {
            Header::NotYet if main_agent.is_none() => {
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
                if main_agent.is_some() {
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
        let agent = &status.main_agent;
        let cli = cx.state.names.harness(&agent.worker);
        let model = cx.state.names.model(&agent.worker, agent.model.as_deref());
        if let Some(Cell::Header(header)) = cx.model.history.active_mut(id) {
            header.main_agent = Some(cli);
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

    /// `AppEvent::Notice`: dim, or in the warning or error color.
    pub(super) fn notice(&mut self, level: NoteLevel, text: &str, cx: &mut Cx<'_>) {
        self.notice_with_more(level, text, None, cx);
    }

    /// A run's `Note`: its first line as a notice, and the lines after it, such as a
    /// validator's errors, kept for Ctrl+O.
    pub(super) fn note(&mut self, note: &Note, cx: &mut Cx<'_>) {
        self.notice_with_more(note.level, note.line(), note.detail(), cx);
    }

    fn notice_with_more(
        &mut self,
        level: NoteLevel,
        text: &str,
        more: Option<&str>,
        cx: &mut Cx<'_>,
    ) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let tone = match level {
            NoteLevel::Info | NoteLevel::Aside if quiet(cx) => return,
            NoteLevel::Info | NoteLevel::Aside => Tone::Dim,
            NoteLevel::Warn => {
                if !self.warnings_seen.insert(text.to_owned()) {
                    return;
                }
                Tone::Warning
            }
            NoteLevel::Error => Tone::Error,
        };
        // A takeover that skips the retry: who steps in, plain, and why there's no retry, dim.
        let takeover = text
            .strip_suffix(NO_SECOND_TRY)
            .and_then(|head| head.strip_suffix(" · "))
            .filter(|_| level == NoteLevel::Warn);
        let mut notice = match takeover {
            Some(head) => Notice::plain_with_after(tone, head, NO_SECOND_TRY),
            None => Notice::new(tone, text),
        };
        if level == NoteLevel::Aside {
            notice = notice.with_glyph(ASIDE);
        }
        for line in more.into_iter().flat_map(str::lines) {
            notice = notice.with_more(line.trim_end());
        }
        emit(cx, notice);
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

/// Like a report, with `more` behind Ctrl+O, as the summary a new main model starts from is.
pub(super) fn folded(title: &str, lines: &[String], more: &[String], cx: &mut Cx<'_>) {
    let mut notice = Notice::new(Tone::Normal, title.trim());
    for line in lines {
        notice = notice.with_detail(line.trim_end());
    }
    for line in more {
        notice = notice.with_more(line.trim_end());
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

/// A dim line under the one before, as what a switch of the main model carried over hangs
/// under `⎿  Set model to …`. It follows a line you caused, so it prints even at `quiet`.
pub(super) fn continued(text: &str, cx: &mut Cx<'_>) {
    let text = text.trim();
    if !text.is_empty() {
        emit(cx, Cell::Custom(Box::new(HungLine::new(text))));
    }
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
    // The running answer keeps the model, effort and workflow it started with.
    let next_request = applies_at_next_request(key) && cx.model.is_working();
    if !reset && let Some(line) = command_result(key, value, scope, previous, &words, next_request)
    {
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
        if next_request {
            text.push_str(NEXT_REQUEST);
        }
        text
    };
    emit(cx, Notice::new(Tone::Success, text));
}

/// Said after a change made while an answer runs, which that answer doesn't take up.
const NEXT_REQUEST: &str = " · applies at the next request";
/// The mark of an aside, as Claude Code draws its own. The ASCII glyph set draws the bullet.
const ASIDE: &str = "※";

/// The keys `/model`, `/effort` and `/workflow` write: each sets how a request is run, so a
/// change made while one runs waits for the next.
fn applies_at_next_request(key: &str) -> bool {
    key == "workflow"
        || model_role(key).is_some()
        || (key.starts_with("roles.") && key.ends_with(".effort"))
}

/// `⎿  Set model to Opus 5.5 · just you · was Claude Code's default`, the value by its display
/// name in the accent color, for the keys `/model` and `/effort` write. All hands reads
/// `⎿  Turned All hands on · just you`.
fn command_result(
    key: &str,
    value: &str,
    scope: SettingScope,
    previous: Option<&str>,
    names: &Words<'_>,
    next_request: bool,
) -> Option<ResultLine> {
    let (name, model) = match model_role(key) {
        Some(None) => ("model".to_owned(), true),
        Some(Some(role)) => (format!("{}'s model", role.as_str()), true),
        None if key == Settings::ULTRACODE => {
            let on = if value.trim() == "true" { "on" } else { "off" };
            let after = format!("· {}", scope_words(scope));
            return Some(ResultLine::valued("Turned Ultracode", on).with_after(after));
        }
        None => {
            let role = key.strip_prefix("roles.")?.strip_suffix(".effort")?;
            let name = match role {
                "main" => "effort".to_owned(),
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
    let mut shown = words(value);
    if shown.is_empty() {
        return None;
    }
    let mut was = previous.map(words).filter(|previous| !previous.is_empty());
    // The main model on another CLI moves the conversation there, so both sides name their CLI.
    if model_role(key) == Some(None)
        && let Some((now_cli, was_cli)) = cli_moved(value, previous, names.names)
    {
        shown = on_cli(by_its_model(shown, &now_cli), &now_cli);
        was = was.map(|was| on_cli(by_its_model(was, &was_cli), &was_cli));
    }
    let mut after = format!("· {}", scope_words(scope));
    if let Some(previous) = was {
        after.push_str(&format!(" · was {previous}"));
    }
    if next_request {
        after.push_str(NEXT_REQUEST);
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
        .map(|item| {
            if let Some(word) = word_for_a_model(item) {
                return word.to_owned();
            }
            match item.parse::<WorkerRef>() {
                Ok(worker) => words
                    .picked
                    .get(&worker.to_string())
                    .cloned()
                    .or_else(|| words.names.model(&worker.worker, worker.model.as_deref()))
                    .unwrap_or_else(|| {
                        format!("{}'s default", words.names.harness(&worker.worker))
                    }),
                Err(_) => item.to_owned(),
            }
        })
        .collect();
    words.join(", ")
}

/// The CLIs of a model setting that moved from one CLI to another, by name.
fn cli_moved(
    value: &str,
    previous: Option<&str>,
    names: &DisplayNames,
) -> Option<(String, String)> {
    let worker = |toml: &str| {
        toml.trim()
            .trim_matches(['"', '\''])
            .parse::<WorkerRef>()
            .ok()
    };
    let (now, was) = (worker(value)?, worker(previous?)?);
    (now.worker != was.worker).then(|| (names.harness(&now.worker), names.harness(&was.worker)))
}

/// `Claude Code's default (Opus 5.5)` as the model it runs, `Opus 5.5`, for a line that names
/// the CLI after it anyway. A default whose model isn't known stays as it is.
fn by_its_model(words: String, cli: &str) -> String {
    match words
        .strip_prefix(&format!("{cli}'s default ("))
        .and_then(|rest| rest.strip_suffix(')'))
    {
        Some(model) if !model.is_empty() => model.to_owned(),
        _ => words,
    }
}

/// `GPT-6.1-Sol on Codex`, unless the words name the CLI already, as `Codex's default` does.
fn on_cli(words: String, cli: &str) -> String {
    if words.contains(cli) {
        words
    } else {
        format!("{words} on {cli}")
    }
}

pub(super) fn scope_words(scope: SettingScope) -> &'static str {
    match scope {
        SettingScope::Session => "this session",
        SettingScope::ProjectLocal => "just you",
        SettingScope::Project => "this project",
        SettingScope::Global => "everywhere",
    }
}
