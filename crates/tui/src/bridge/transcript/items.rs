//! Single lines from the app rather than a run: the startup header, notices, recaps, narrator
//! lines, and settings you changed.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::app::{AppEvent, CommandReply, CrewRow, HeaderAndBottomLine};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::log::{
    COULDNT_UPDATE_TO, DOING_THIS_ALONE, NO_LONGER_OFFERS, NO_SECOND_TRY, SAME_CLI_IS_ON,
};
use argonauts_core::names::{ConversationId, ModelRef};
use argonauts_core::policy::{NarratorLine, NarratorLineKind};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::Settings;
use argonauts_core::{Note, NoteLevel};

use crate::cell::{
    AgentReady, Cell, CrewForRequest, DetailLogo, HungLine, Notice, Recap, ResultLine,
    SessionHeader, Tone, Trailing, UserMessage,
};
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
    /// A report still filling in, by its title, live under the conversation until it's whole.
    growing: Option<(String, CellId)>,
}

/// The controller sends the bottom line with the main agent just after the welcome, so the
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
    /// and workflow, and changes the header printed rather than printing another.
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
        // An older app's welcome names no agent, so the bottom line's fills it in.
        let main_agent = main_agent
            .as_ref()
            .or_else(|| cx.state.status.as_ref().map(|status| &status.main_agent));
        let names = &cx.state.names;
        let header = SessionHeader {
            app: "Argonauts Code".to_owned(),
            version: version.clone(),
            main_agent: main_agent.map(|agent| names.cli(&agent.agent)),
            model: main_agent.and_then(|agent| names.model(&agent.agent, agent.model.as_deref())),
            effort: effort.clone(),
            workflow: workflow.clone(),
            ultra: cx.state.status.as_ref().and_then(|status| status.ultra),
            folder: repo.clone(),
            agents: agents
                .iter()
                .map(|agent| AgentReady {
                    name: names.cli(&agent.id),
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
            way_back: None,
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

    /// `/clear`: the screen starts over under the header, as a start prints it with the agent
    /// you talk to now, and the way back to the old conversation under it when it had turns.
    /// The runs a start found are the old screen's news, so they go from it.
    pub(super) fn start_over(&mut self, way_back: Option<&ConversationId>, cx: &mut Cx<'_>) {
        let Some(mut header) = self.shown.clone() else {
            return;
        };
        if let Some(status) = cx.state.status.as_ref() {
            let agent = &status.main_agent;
            let names = &cx.state.names;
            header.main_agent = Some(names.cli(&agent.agent));
            header.model = names.model(&agent.agent, agent.model.as_deref());
            header.effort = status.line.main_effort.clone().or(header.effort);
            // The mascot is the one the workflow and the agent's ultra look call for now.
            header.workflow.clone_from(&status.workflow);
            header.ultra = status.ultra;
        }
        header.runs_going = 0;
        header.runs_need_you = 0;
        header.way_back = way_back.map(ToString::to_string);
        cx.model.history.start_over();
        cx.model.history.push(header.clone());
        self.shown = Some(header);
    }

    /// Fills in the main agent and prints the header waiting for it. Once it printed, a new
    /// workflow, the main agent's ultra mode coming or going, or a new effort redraws it where it
    /// still shows, mascot and all.
    pub(super) fn status(&mut self, status: &HeaderAndBottomLine, cx: &mut Cx<'_>) {
        match self.header {
            Header::NotYet => {}
            Header::Waiting(id) => {
                let agent = &status.main_agent;
                let cli = cx.state.names.cli(&agent.agent);
                let model = cx.state.names.model(&agent.agent, agent.model.as_deref());
                if let Some(Cell::Header(header)) = cx.model.history.active_mut(id) {
                    header.main_agent = Some(cli);
                    header.model = model;
                    header.ultra = status.ultra;
                }
                self.print_header(cx);
            }
            Header::Printed => {
                let Some(shown) = &self.shown else {
                    return;
                };
                let mut header = shown.clone();
                header.workflow.clone_from(&status.workflow);
                header.ultra = status.ultra;
                if let Some(effort) = &status.line.main_effort {
                    header.effort = Some(effort.clone());
                }
                if header != *shown {
                    self.shown = Some(header.clone());
                    cx.model.history.replace_header(header);
                }
            }
        }
    }

    /// Prints a header still waiting for the bottom line, so nothing prints above it.
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
        self.notice_with_more(level, text, &[], None, cx);
    }

    /// A run's `Note`: its first line as a notice with its rows under it, and the lines after
    /// them, such as a validator's errors, kept for Ctrl+O.
    pub(super) fn note(&mut self, note: &Note, cx: &mut Cx<'_>) {
        let rows: Vec<&str> = note.rows().collect();
        self.notice_with_more(note.level, note.line(), &rows, note.detail(), cx);
    }

    fn notice_with_more(
        &mut self,
        level: NoteLevel,
        text: &str,
        rows: &[&str],
        more: Option<&str>,
        cx: &mut Cx<'_>,
    ) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        // The offer under the reply is there at every verbosity, since it says how to have the
        // crew do what the main agent took alone.
        if let Some(offer) = offer(level, text) {
            emit(cx, offer);
            return;
        }
        let tone = match level {
            NoteLevel::Info | NoteLevel::Aside | NoteLevel::Quiet if quiet(cx) => return,
            // Rows are there to be read, so the line over them isn't dimmed away.
            NoteLevel::Info if !rows.is_empty() => Tone::Normal,
            NoteLevel::Info | NoteLevel::Aside | NoteLevel::Quiet => Tone::Dim,
            NoteLevel::Warn | NoteLevel::Caution => {
                if !self.warnings_seen.insert(text.to_owned()) {
                    return;
                }
                Tone::Warning
            }
            NoteLevel::Error => Tone::Error,
        };
        // A fallback that skips the retry: who steps in, in the warning's tone, and why there's
        // no retry, dim.
        let fallback = text
            .strip_suffix(NO_SECOND_TRY)
            .and_then(|head| head.strip_suffix(" · "))
            .filter(|_| level == NoteLevel::Warn);
        // A model that runs in place of one its CLI dropped: the mark warns, the words read
        // plain.
        let in_place = level == NoteLevel::Warn && text.contains(NO_LONGER_OFFERS);
        // An update that failed: the mark warns, the words read plain and the command to try
        // again reads as code.
        let update_failed = text
            .rsplit_once(" · ")
            .filter(|_| level == NoteLevel::Warn && text.starts_with(COULDNT_UPDATE_TO));
        let mut notice = match (fallback, update_failed) {
            (Some(head), _) => Notice {
                decided: Some(NO_SECOND_TRY.to_owned()),
                ..Notice::new(tone, head)
            },
            (None, Some((why, command))) => Notice::plain_with_after(tone, format!("{why} · "), "")
                .with_trailing([Trailing::Code(command.to_owned())]),
            (None, None) if in_place => Notice::plain_with_after(tone, text, ""),
            (None, None) => Notice::new(tone, text),
        };
        match level {
            NoteLevel::Aside => notice = notice.with_glyph(ASIDE),
            NoteLevel::Quiet | NoteLevel::Caution => notice = notice.with_glyph(NO_MARK),
            _ => {}
        }
        for row in rows {
            notice = notice.with_detail(*row);
        }
        for line in more.into_iter().flat_map(str::lines) {
            let line = line.trim_end();
            // Why a fallback stayed on its CLI is part of what happened, so it shows.
            notice = if line.starts_with(SAME_CLI_IS_ON) {
                notice.with_detail(line)
            } else {
                notice.with_more(line)
            };
        }
        emit(cx, notice);
    }
}

/// The line that says the main agent takes a change alone, as the offer it is: its words dim
/// and the command after its last ` · ` in the text color.
fn offer(level: NoteLevel, text: &str) -> Option<Notice> {
    if level != NoteLevel::Info || !text.starts_with(DOING_THIS_ALONE) {
        return None;
    }
    let (why, rest) = text.rsplit_once(" · ")?;
    let (command, after) = rest.split_once(' ').unwrap_or((rest, ""));
    command.starts_with('/').then(|| {
        let after = if after.is_empty() {
            String::new()
        } else {
            format!(" {after}")
        };
        Notice::offer(format!("{why} · "), command, after)
    })
}

impl Items {
    /// A command's findings, such as `/doctor`'s: the title, then one dim line each under `⎿`.
    /// You asked for it, so it prints even at `quiet`. The report filling in under the same
    /// title becomes this one where it stands.
    pub(super) fn report(&mut self, title: &str, lines: &[String], cx: &mut Cx<'_>) {
        let notice = report_notice(title, lines, &cx.state.names);
        match self.growing.take_if(|(growing, _)| growing == title) {
            Some((_, id)) => {
                if let Some(cell) = cx.model.history.active_mut(id) {
                    *cell = notice.into();
                }
                cx.model.history.finish_active(id);
            }
            None => emit(cx, notice),
        }
    }

    /// A report still filling in, such as `/update`'s as each CLI is done: live, its rows
    /// added where it stands until the whole report comes.
    pub(super) fn report_so_far(&mut self, title: &str, lines: &[String], cx: &mut Cx<'_>) {
        let notice = report_notice(title, lines, &cx.state.names);
        match &self.growing {
            Some((growing, id)) if growing == title => {
                if let Some(cell) = cx.model.history.active_mut(*id) {
                    *cell = notice.into();
                }
            }
            _ => {
                let id = cx.model.history.push_active(notice);
                self.growing = Some((title.to_owned(), id));
            }
        }
    }
}

/// A report's rows under its title. A row that starts with a CLI's or a host's name and a
/// column's blanks, as `/update` and `/doctor` write theirs, has its logo before the name.
fn report_notice(title: &str, lines: &[String], names: &DisplayNames) -> Notice {
    let mut notice = Notice::new(Tone::Normal, title.trim());
    for (row, line) in lines.iter().enumerate() {
        notice.detail_logos.extend(detail_logo(row, line, names));
        notice = notice.with_detail(line.trim_end());
    }
    // Argonauts Code's own row keeps the logo's cells blank where the rest have logos, so the
    // names line up.
    if !notice.detail_logos.is_empty() {
        let own = "Argonauts Code";
        for (row, line) in lines.iter().enumerate() {
            if line
                .strip_prefix(own)
                .is_some_and(|rest| rest.starts_with("   "))
            {
                notice.detail_logos.push(DetailLogo {
                    row,
                    logo: None,
                    name: own.len(),
                });
            }
        }
    }
    notice
}

/// The agents a report's row may start with that have logos of their own.
const WITH_LOGOS: [&str; 6] = [
    "claude-code",
    "codex",
    "antigravity-cli",
    "opencode",
    "openrouter",
    "ollama",
];

fn detail_logo(row: usize, line: &str, names: &DisplayNames) -> Option<DetailLogo> {
    WITH_LOGOS.iter().find_map(|agent| {
        let name = names.cli(agent);
        let rest = line.strip_prefix(name.as_str())?;
        // A column's blanks after the name, or its version, as the doctor writes it, so a
        // sentence that starts with the name gets none.
        let version = rest
            .strip_prefix(' ')
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
        (rest.starts_with("   ") || version).then(|| DetailLogo {
            row,
            logo: Some(argonauts_core::logos::logo_for(agent, None)),
            name: name.len(),
        })
    })
}

/// A switch you flipped, such as `/awake`: what's on, how to undo it, and what it can't do.
/// You asked for it, so it prints even at `quiet`.
pub(super) fn switched(text: &str, undo: Option<&str>, caveat: Option<&str>, cx: &mut Cx<'_>) {
    let notice = Notice::switched(
        text.trim(),
        undo.map(str::to_owned),
        caveat.map(str::to_owned),
    );
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

/// The crew your words set for a request, shown before its work starts. A screen that keeps
/// to fewer lines leaves it out with the other notices.
pub(super) fn crew_for_request(rows: &[CrewRow], cx: &mut Cx<'_>) {
    if rows.is_empty() || quiet(cx) {
        return;
    }
    emit(
        cx,
        Cell::Custom(Box::new(CrewForRequest {
            rows: rows.to_vec(),
        })),
    );
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

/// A setting saved or reset, as `SettingChanged` tells it.
pub(super) struct Changed<'a> {
    pub key: &'a str,
    pub value: &'a str,
    pub scope: SettingScope,
    pub previous: Option<&'a str>,
    pub reset: bool,
    pub why: Option<&'a str>,
    pub kept: bool,
}

/// `● Set approvals.merge.mode = auto · this project (.argonauts/config.toml)`, or after a
/// reset `● Reset ui.theme · everywhere · now dark`. A model or an effort, as `/model` and
/// `/effort` set them, reads like Claude Code's `⎿  Set model to Opus 5.5`. Each says where it
/// went, and why when that isn't where your changes usually go. One your own words kept, as
/// "keep these checks" does, reads `⎿  Kept checks.run · just you (…)` under your message.
pub(super) fn setting_changed(changed: Changed<'_>, cx: &mut Cx<'_>) {
    let Changed {
        key,
        value,
        scope,
        previous,
        reset,
        why,
        kept,
    } = changed;
    // The view that saved it says so itself when it closes.
    if !reset && cx.model.said_by_view.remove(key) {
        return;
    }
    let saved = Saved {
        scope,
        why,
        previous,
    };
    // Your own words kept it, so the line under your message says so and where it went.
    if kept && !reset {
        let line = format!("Kept {key} {}", saved.after(None));
        replied(ResultLine::new(Tone::Dim, line), cx);
        return;
    }
    // `/config key=value`, as Claude Code's says it: the key, its value, and the file.
    if !reset && let Some((_, typed)) = cx.model.typed_config.take_if(|(typed, _)| typed == key) {
        let shown = format::setting_value(value);
        let mut line = format!("Set {key} to {shown} ({})", typed_place(scope));
        if let Some(why) = why {
            line.push_str(&format!(", {why}"));
        }
        emit(cx, UserMessage::command(typed));
        replied(ResultLine::new(Tone::Dim, line), cx);
        return;
    }
    // A workflow's command, as typed, goes over the line its switch leaves.
    if key == "workflow"
        && !reset
        && let Some(typed) = cx.model.typed_workflow.take()
    {
        emit(cx, UserMessage::command(typed));
    }
    let words = Words {
        picked: &cx.state.model_words,
        names: &cx.state.names,
    };
    // The running answer keeps the model, effort and workflow it started with.
    let next_request = applies_at_next_request(key) && cx.model.is_working();
    if !reset && let Some(line) = command_result(key, value, &saved, &words, next_request) {
        replied(line, cx);
        return;
    }
    let value = format::setting_value(value);
    let value = if value.is_empty() { "unset" } else { &value };
    let text = if reset {
        format!("Reset {key} · {} · now {value}", scope_words(scope))
    } else {
        let was = previous.map(|previous| {
            let words = format::setting_value(previous);
            if words.is_empty() {
                "unset".to_owned()
            } else {
                words
            }
        });
        let mut text = format!("Set {key} = {value} {}", saved.after(was));
        if next_request {
            text.push_str(NEXT_REQUEST);
        }
        text
    };
    emit(cx, Notice::new(Tone::Success, text));
}

/// A line a command left that changed nothing, under the command when it was a workflow's:
/// `⎿  Council is the workflow already · Opus 5.5 and Codex propose`.
pub(super) fn command_replied(reply: &CommandReply, cx: &mut Cx<'_>) {
    if let Some(typed) = cx.model.typed_workflow.take() {
        emit(cx, UserMessage::command(typed));
    }
    replied(ResultLine::of_reply(reply), cx);
}

/// Where a change went, as the line after it ends: `· everywhere (~/.argonauts/config.toml)`,
/// with why when a layer above moved it there, or `· this session · was high`, since a change
/// for this session goes back to that when you quit.
struct Saved<'a> {
    scope: SettingScope,
    why: Option<&'a str>,
    previous: Option<&'a str>,
}

impl Saved<'_> {
    /// The words after the value, `was` the previous value in the line's own words, said for a
    /// change for this session.
    fn after(&self, was: Option<String>) -> String {
        let was = was.filter(|_| self.scope == SettingScope::Session);
        self.after_saying(was)
    }

    /// [`after`](Self::after), saying what it was whatever the scope.
    fn after_saying(&self, was: Option<String>) -> String {
        let mut after = format!("· {}", scope_words(self.scope));
        if let Some(file) = scope_file(self.scope) {
            after.push_str(&format!(" ({file})"));
        }
        if let Some(why) = self.why {
            after.push_str(&format!(", {why}"));
        }
        if let Some(was) = was.filter(|was| !was.is_empty()) {
            after.push_str(&format!(" · was {was}"));
        }
        after
    }
}

/// `/config key=value` left as it was: why, in the error colour under the command as typed,
/// where a saved change says where it went. Without the command, a notice says why.
pub(super) fn setting_refused(key: &str, why: &str, cx: &mut Cx<'_>) {
    match cx.model.typed_config.take_if(|(typed, _)| typed == key) {
        Some((_, typed)) => {
            emit(cx, UserMessage::command(typed));
            replied(ResultLine::new(Tone::Error, why), cx);
        }
        None => emit(cx, Notice::new(Tone::Error, why)),
    }
}

/// Prints a command's answer under it, which the conversation's record keeps after the
/// command so the conversation drawn again prints it too.
fn replied(line: ResultLine, cx: &mut Cx<'_>) {
    if !cx.replaying() {
        cx.out.replies.push(line.reply());
    }
    emit(cx, Cell::Custom(Box::new(line)));
}

/// Where a scope saves, as `/config key=value` names it.
fn typed_place(scope: SettingScope) -> String {
    match scope_file(scope) {
        Some(file) => format!("in {file}"),
        None => "for this session".to_owned(),
    }
}

/// The file a scope saves into, as the line after a change names it.
fn scope_file(scope: SettingScope) -> Option<&'static str> {
    match scope {
        SettingScope::Global => Some("~/.argonauts/config.toml"),
        SettingScope::Project => Some(".argonauts/config.toml"),
        SettingScope::ProjectLocal => Some(".argonauts/config.local.toml"),
        SettingScope::Session => None,
    }
}

/// Said after a change made while an answer runs, which that answer doesn't take up.
const NEXT_REQUEST: &str = " · applies at the next request";
/// The mark of an aside, as Claude Code draws its own. The ASCII glyph set draws the bullet.
const ASIDE: &str = "※";
/// A quiet line has no mark: its words start where a marked line's do.
const NO_MARK: &str = " ";

/// The keys `/model`, `/effort` and `/workflow` write: each sets how a request is run, so a
/// change made while one runs waits for the next.
fn applies_at_next_request(key: &str) -> bool {
    key == "workflow"
        || model_role(key).is_some()
        || (key.starts_with("roles.") && key.ends_with(".effort"))
}

/// `⎿  Set model to Opus 5.5 · everywhere (~/.argonauts/config.toml)`, the value by its display
/// name in the accent color, for the keys `/model` and `/effort` write. Ultracode reads
/// `⎿  Turned Ultracode on · just you`.
fn command_result(
    key: &str,
    value: &str,
    saved: &Saved<'_>,
    names: &Words<'_>,
    next_request: bool,
) -> Option<ResultLine> {
    if key == "workflow" {
        return Some(switched_workflow(value, saved, next_request));
    }
    let (name, model) = match model_role(key) {
        Some(None) => ("model".to_owned(), true),
        Some(Some(role)) => (format!("{}'s model", role.as_str()), true),
        None if key == Settings::ULTRACODE => {
            let on = if value.trim() == "true" { "on" } else { "off" };
            let after = saved.after(None);
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
    let previous = saved.previous;
    let was = previous.map(words).filter(|previous| !previous.is_empty());
    // The main model on another CLI moves the conversation there, so both sides name their CLI,
    // whatever the scope.
    let mut after = match cli_moved(value, previous, names.names) {
        Some((now_cli, was_cli)) if model_role(key) == Some(None) => {
            shown = on_cli(by_its_model(shown, &now_cli), &now_cli);
            saved.after_saying(was.map(|was| on_cli(by_its_model(was, &was_cli), &was_cli)))
        }
        _ => saved.after(was),
    };
    if next_request {
        after.push_str(NEXT_REQUEST);
    }
    Some(ResultLine::valued(format!("Set {name} to"), shown).with_after(after))
}

/// `⎿  Switched to Council · everywhere (~/.argonauts/config.toml) · was Captain`, the line a
/// workflow's command or `/workflow <name>` leaves, with why when a layer above moved it. Saved
/// while it was already the workflow, as its setup sheet does, it reads `Set up Council · just
/// you (.argonauts/config.local.toml)`.
fn switched_workflow(value: &str, saved: &Saved<'_>, next_request: bool) -> ResultLine {
    let now = workflow_title(value);
    let was = saved
        .previous
        .map(workflow_title)
        .filter(|was| !was.is_empty());
    let mut text = match &was {
        Some(was) if *was == now => format!("Set up {now} {}", saved.after_saying(None)),
        _ => format!("Switched to {now} {}", saved.after_saying(was)),
    };
    if next_request {
        text.push_str(NEXT_REQUEST);
    }
    ResultLine::new(Tone::Normal, text)
}

/// `Council` for the TOML text `"council"`.
fn workflow_title(toml: &str) -> String {
    let name = format::setting_value(toml);
    let mut chars = name.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
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
            match item.parse::<ModelRef>() {
                Ok(worker) => words
                    .picked
                    .get(&worker.to_string())
                    .cloned()
                    .or_else(|| words.names.model(&worker.agent, worker.model.as_deref()))
                    .unwrap_or_else(|| format!("{}'s default", words.names.cli(&worker.agent))),
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
            .parse::<ModelRef>()
            .ok()
    };
    let (now, was) = (worker(value)?, worker(previous?)?);
    (now.agent != was.agent).then(|| (names.cli(&now.agent), names.cli(&was.agent)))
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

#[cfg(test)]
#[path = "report_logos_tests.rs"]
mod report_logos_tests;

#[cfg(test)]
mod offer_tests {
    use std::time::Instant;

    use argonauts_core::NoteLevel;
    use argonauts_core::app::AppEvent;
    use argonauts_core::settings::ui::{UiSettings, Verbosity};

    use super::super::Transcript;
    use crate::bridge::{Cx, Effects};
    use crate::model::ViewModel;
    use crate::render::{Clock, RenderCtx};
    use crate::state::AppState;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    const ALONE: &str =
        "Doing this alone: about 1 file, and the repo has tests · /crew to have Council do it";

    fn printed(verbosity: Verbosity, text: &str) -> String {
        let mut model = ViewModel::default();
        let state = AppState::default();
        let settings = UiSettings {
            verbosity,
            ..UiSettings::default()
        };
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut model,
            state: &state,
            settings: &settings,
            now: Instant::now(),
            out: &mut out,
        };
        let notice = AppEvent::Notice {
            level: NoteLevel::Info,
            text: text.to_owned(),
        };
        Transcript::default().on_event(&notice, &mut cx);
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
        plain_text(&model.history.drain_printable(&ctx))
    }

    #[test]
    fn the_alone_line_is_an_offer_at_every_verbosity() {
        for verbosity in [Verbosity::Full, Verbosity::Quiet] {
            assert_eq!(
                printed(verbosity, ALONE).trim(),
                format!("◆ {ALONE}"),
                "{verbosity:?}"
            );
        }
        assert_eq!(printed(Verbosity::Quiet, "Kept the plan").trim(), "");
    }
}
