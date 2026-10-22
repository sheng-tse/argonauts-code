//! The Config tab: Claude Code's friendly list of settings with a search box over it, and
//! Argonauts Code's own rows after Claude Code's. Each row changes in place, as Claude Code's
//! do, and a personal row saves at once to your global file. A row that changes safety or what
//! the team shares asks where to save, and a value that a project file, the workflow, or
//! `--set` holds says so and opens All settings, where it can be changed where it lives.
//!
//! A row that asks where steps in place like any other, then asks where to save over the rows.
//! Its change waits for the app, which may ask whether to change a safety setting while the
//! tab stays up, and Esc says it only once it is saved.
//!
//! Nothing prints while the tab is open. Esc leaves one line under the command that opened
//! it, `Set Editor mode to vim`, or `Config dialog dismissed` when nothing changed.

mod draw;
mod rows;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use argonauts_core::app::{SettingRow, UiCommand};
use argonauts_core::input::{ParsedInput, parse_input};
use argonauts_core::names::ModelRef;
use argonauts_core::settings::mods::ModPlace;
use argonauts_core::settings::mods::builtin;
use argonauts_core::settings::mods::view::ModRowKind;
use argonauts_core::settings::ui::ModName;
use argonauts_core::settings::{SettingScope, asks_where, layer};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::bottom::Picker;
use crate::format;
use crate::state::{AppState, SettingOutcome};
use crate::views::ViewOutcome;
use crate::views::list;
use crate::views::settings::all_settings::AllSettingsView;
use crate::views::settings::words::row_value_words;
use crate::views::settings::{edit_key, flow};

pub use draw::{body_rows, draw};
use rows::{Control, ROWS, Row, START_MODE};

/// What Esc says when nothing changed, as Claude Code says it.
const DISMISSED: &str = "Config dialog dismissed";

/// The keys Config's rows read and save, in row order. A row that opens a command and holds no
/// key of its own has none.
pub fn row_keys() -> impl Iterator<Item = &'static str> {
    ROWS.iter().map(|row| row.key).filter(|key| !key.is_empty())
}

/// Where the tab's keys are, under the panel's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Focus {
    /// Typing filters the rows, as the tab opens from `/config`.
    #[default]
    Search,
    List,
}

#[derive(Debug, Default)]
pub struct ConfigTab {
    /// Every key with its value and where it came from, as All settings lists them. Empty until
    /// the controller answers.
    settings: Vec<SettingRow>,
    /// Each key's place in `settings`.
    by_key: HashMap<String, usize>,
    query: String,
    focus: Focus,
    /// The pointer, among the rows the search keeps.
    pointer: usize,
    /// The first row on screen.
    offset: usize,
    /// Each row changed while the tab was open, by its label, with its newest words.
    changed: Vec<(String, String)>,
    /// The command that opened the panel on this tab, for Esc's line.
    opened_by: Option<String>,
    /// The approval mode now, which Last used brings back at the next start.
    mode_now: Option<String>,
    /// Where to save a change to a row that asks, over the rows, with the row and its value.
    editor: Option<(Picker, &'static Row, String)>,
    /// Changes sent from rows that ask where, until the app saves them or leaves them as they
    /// were.
    waiting: Vec<Waiting>,
    /// How many of the state's setting outcomes the tab has read, once it has looked.
    outcomes_read: Option<usize>,
    /// A Behaviors row was followed to the map, which the panel shows on its own tab.
    to_map: bool,
}

/// A change sent from a row that asks where, waiting for the app's word.
#[derive(Debug, Clone)]
struct Waiting {
    label: &'static str,
    key: String,
    value: String,
    /// How the row says the new value, for Esc's line.
    words: String,
    /// The value it had, which comes back if the change isn't made.
    was: String,
}

/// A row as it shows now: its table entry and the key's value with where it came from.
#[derive(Debug, Clone, Copy)]
struct Shown<'a> {
    row: &'static Row,
    setting: Option<&'a SettingRow>,
}

impl Shown<'_> {
    fn value(&self) -> &str {
        self.setting.map_or("", |setting| setting.value.as_str())
    }

    /// What its value comes to, dim after it: the model auto picked when that's known, else
    /// what the key's values mean.
    fn meaning(&self) -> Option<&str> {
        self.setting
            .and_then(|setting| setting.picked.as_deref())
            .or_else(|| rows::described(self.row.key, self.value()))
    }

    /// Where a higher layer holds the value of a row that saves at once to your global file,
    /// which a save there wouldn't change. The rows that ask where to save, and the pickers the
    /// links open, save where the value lives.
    fn held_by(&self) -> Option<&'static str> {
        let setting = self.setting?;
        let at_once = matches!(self.row.control, Control::Toggle(_) | Control::Choice(_))
            && !asks_where(&setting.key, setting.safety);
        at_once.then(|| held_by(&setting.layer)).flatten()
    }
}

/// Where a value above your global file comes from, which a save there wouldn't change:
/// `the project`, `the workflow`, `--set`. `None` where a save takes effect.
fn held_by(from: &str) -> Option<&'static str> {
    Some(match from {
        layer::PROJECT => "the project",
        layer::PROJECT_LOCAL => "your project-local file",
        layer::WORKFLOW => "the workflow",
        layer::FLAG => "--set",
        layer::SESSION => "this session",
        _ => return None,
    })
}

impl ConfigTab {
    /// The tab as `/config` or `/settings` opens it, the search box taking the keys.
    pub fn opened_by(command: &str) -> Self {
        Self {
            opened_by: Some(command.to_owned()),
            ..Self::default()
        }
    }

    pub fn has_rows(&self) -> bool {
        !self.settings.is_empty()
    }

    /// Whether the last key followed a Behaviors row to the map, once.
    pub fn followed_to_map(&mut self) -> bool {
        std::mem::take(&mut self.to_map)
    }

    /// The approval mode the app runs in now, which Default approval mode's Last used names.
    pub fn set_mode_now(&mut self, mode: Option<String>) {
        self.mode_now = mode;
    }

    /// Every key from the controller, fresh. The pointer stays where it was, as far as the
    /// rows reach once they are drawn.
    pub fn set_rows(&mut self, settings: Vec<SettingRow>) {
        self.by_key = settings
            .iter()
            .enumerate()
            .map(|(at, row)| (row.key.clone(), at))
            .collect();
        self.settings = settings;
    }

    fn setting(&self, key: &str) -> Option<&SettingRow> {
        self.by_key.get(key).map(|&at| &self.settings[at])
    }

    /// The rows that show: each whose key the settings hold and each that holds none, kept by
    /// the search when there is one.
    fn shown(&self, state: &AppState) -> Vec<Shown<'_>> {
        let needle = self.query.trim().to_lowercase();
        ROWS.iter()
            .filter_map(|row| {
                let setting = self.setting(row.key);
                (row.key.is_empty() || setting.is_some()).then_some(Shown { row, setting })
            })
            .filter(|shown| {
                needle.is_empty()
                    || [
                        self.label(shown.row),
                        shown.row.key.to_owned(),
                        self.words(shown, state),
                    ]
                    .iter()
                    .any(|text| text.to_lowercase().contains(&needle))
            })
            .collect()
    }

    /// The row's label; the last one counts every key.
    fn label(&self, row: &Row) -> String {
        match row.control {
            Control::AllSettings => format!("{} · {} keys", row.label, self.settings.len()),
            _ => row.label.to_owned(),
        }
    }

    /// The value in words, as the row shows it.
    fn words(&self, shown: &Shown<'_>, state: &AppState) -> String {
        let value = shown.value();
        let key = shown.row.key;
        match shown.row.control {
            Control::Toggle(flip) => flip.reads_on(value).to_string(),
            Control::Choice(choices) => {
                let words = choices
                    .iter()
                    .find(|choice| same(choice.toml, value))
                    .map_or_else(|| format::setting_value(value), |c| c.words.to_owned());
                match &self.mode_now {
                    Some(mode) if key == START_MODE && same(value, "\"last\"") => {
                        format!("{words} ({mode})")
                    }
                    _ => words,
                }
            }
            Control::Opens(_) | Control::BehaviorsMap => link_words(key, value, state),
            Control::Typed => typed_words(key, value),
            Control::OnTheLine(card) => on_the_line(card, state).to_string(),
            Control::AllSettings => String::new(),
        }
    }

    /// The hint line for where the keys are, as Claude Code words each.
    pub fn hints(&self, on_tabs: bool) -> &'static str {
        match (on_tabs, self.focus) {
            (true, _) => "←/→/tab to switch · ↓  to return · Esc to close",
            (false, Focus::Search) => {
                "Type to filter · Enter/↓  to select · ↑  to tabs · Esc to clear"
            }
            (false, Focus::List) => "Enter/Space to change · / to search · Esc to close",
        }
    }

    /// From the tabs, ↓ comes back to the search box.
    pub fn enter_from_tabs(&mut self) {
        self.focus = Focus::Search;
    }

    /// A key while the tab's own rows have the keys. `None` hands it back to the panel, as ↑
    /// from the search box does to go up to the tabs.
    pub fn key(&mut self, key: KeyEvent, state: &AppState) -> Option<ViewOutcome> {
        self.read_outcomes(state);
        if self.editor.is_some() {
            return Some(self.editor_key(key, state));
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match self.focus {
            Focus::Search => match key.code {
                KeyCode::Up => None,
                KeyCode::Enter | KeyCode::Down => {
                    if !self.shown(state).is_empty() {
                        self.focus = Focus::List;
                        self.pointer = 0;
                        self.offset = 0;
                    }
                    Some(ViewOutcome::Consumed)
                }
                KeyCode::Esc if !self.query.is_empty() => {
                    self.search(String::clear);
                    Some(ViewOutcome::Consumed)
                }
                KeyCode::Esc => Some(self.close()),
                KeyCode::Backspace => {
                    self.search(|query| {
                        query.pop();
                    });
                    Some(ViewOutcome::Consumed)
                }
                KeyCode::Char(ch) if plain => {
                    self.search(|query| query.push(ch));
                    Some(ViewOutcome::Consumed)
                }
                _ => Some(ViewOutcome::Consumed),
            },
            Focus::List => Some(self.list_key(key, plain, state)),
        }
    }

    /// Pasted text goes into the search, as typing does.
    pub fn paste(&mut self, text: &str) {
        let line = text
            .split(['\n', '\r'])
            .next()
            .unwrap_or_default()
            .to_owned();
        self.focus = Focus::Search;
        self.search(|query| query.push_str(&line));
    }

    fn search(&mut self, change: impl FnOnce(&mut String)) {
        change(&mut self.query);
        self.pointer = 0;
        self.offset = 0;
    }

    fn list_key(&mut self, key: KeyEvent, plain: bool, state: &AppState) -> ViewOutcome {
        let count = self.shown(state).len();
        // Claude Code's Settings list moves with j and k, and with ctrl+n and ctrl+p, as with
        // the arrows.
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        let code = match key.code {
            KeyCode::Char('j') if plain => KeyCode::Down,
            KeyCode::Char('k') if plain => KeyCode::Up,
            KeyCode::Char('n') if ctrl => KeyCode::Down,
            KeyCode::Char('p') if ctrl => KeyCode::Up,
            code => code,
        };
        match code {
            KeyCode::Up if self.pointer == 0 => self.focus = Focus::Search,
            KeyCode::Up => self.pointer -= 1,
            KeyCode::Down => self.pointer = (self.pointer + 1).min(count.saturating_sub(1)),
            KeyCode::PageUp => self.pointer = self.pointer.saturating_sub(10),
            KeyCode::PageDown => self.pointer = (self.pointer + 10).min(count.saturating_sub(1)),
            KeyCode::Home => self.pointer = 0,
            KeyCode::End => self.pointer = count.saturating_sub(1),
            KeyCode::Esc => return self.close(),
            KeyCode::Enter | KeyCode::Char(' ') => return self.change(Step::On, state),
            KeyCode::Right => return self.change(Step::On, state),
            KeyCode::Left => return self.change(Step::Back, state),
            KeyCode::Char('/') => {
                self.focus = Focus::Search;
                self.search(String::clear);
            }
            // Any other key starts a search with it, as in Claude Code.
            KeyCode::Char(ch) if plain => {
                self.focus = Focus::Search;
                self.search(|query| {
                    query.clear();
                    query.push(ch);
                });
            }
            _ => {}
        }
        ViewOutcome::Consumed
    }

    /// Changes the row under the pointer: a toggle flips, a choice steps, and the rest open
    /// where they are changed.
    fn change(&mut self, step: Step, state: &AppState) -> ViewOutcome {
        let (row, setting) = match self.shown(state).get(self.pointer) {
            Some(shown) => (shown.row, shown.setting.cloned()),
            None => return ViewOutcome::Consumed,
        };
        let value = setting
            .as_ref()
            .map_or("", |setting| setting.value.as_str());
        let held = Shown {
            row,
            setting: setting.as_ref(),
        }
        .held_by()
        .is_some();
        let next = match row.control {
            // ← and → only flip and step; the other rows open on Enter and Space alone.
            Control::Opens(_)
            | Control::BehaviorsMap
            | Control::Typed
            | Control::AllSettings
            | Control::OnTheLine(_)
                if step == Step::Back =>
            {
                return ViewOutcome::Consumed;
            }
            Control::Toggle(flip) if !held => Some(
                if flip.reads_on(value) {
                    flip.off
                } else {
                    flip.on
                }
                .to_owned(),
            ),
            Control::Choice(choices) if !held => {
                let allowed: Vec<String> = match choices {
                    [] => setting
                        .as_ref()
                        .map(|s| s.allowed.clone())
                        .unwrap_or_default(),
                    choices => choices.iter().map(|c| c.toml.to_owned()).collect(),
                };
                stepped(&allowed, value, step)
            }
            _ => None,
        };
        match (row.control, next, setting) {
            (_, Some(value), Some(setting)) => self.save(row, &setting, value, state),
            (Control::Opens(command), ..) => self.open_command(command),
            (Control::BehaviorsMap, ..) => {
                self.to_map = true;
                ViewOutcome::Consumed
            }
            // `/mods <card> bottom-line` puts the card on the line or takes it off.
            (Control::OnTheLine(card), ..) => {
                match parse_input(&format!("/mods {card} bottom-line")) {
                    Ok(ParsedInput::Slash { command }) => {
                        let on = !on_the_line(card, state);
                        self.note(row.label, &on.to_string());
                        ViewOutcome::Send(UiCommand::Slash { command })
                    }
                    _ => ViewOutcome::Consumed,
                }
            }
            (Control::AllSettings, ..) => {
                ViewOutcome::Open(Box::new(AllSettingsView::new(self.settings.clone())))
            }
            // Typed values, and values a higher layer holds, change in All settings, which
            // asks where to save.
            (_, _, Some(setting)) => self.at_key(&setting.key),
            _ => ViewOutcome::Consumed,
        }
    }

    /// All settings over the tab, on `key`.
    fn at_key(&self, key: &str) -> ViewOutcome {
        ViewOutcome::Open(Box::new(AllSettingsView::at_key(
            self.settings.clone(),
            key,
        )))
    }

    /// Saves `value` for the row: at once to your global file for a personal key, or after
    /// the question of where for one that changes safety or what the team shares.
    fn save(
        &mut self,
        row: &'static Row,
        setting: &SettingRow,
        value: String,
        state: &AppState,
    ) -> ViewOutcome {
        if asks_where(&setting.key, setting.safety) {
            let mut editor = flow::scope_picker(setting.clone(), value.clone());
            editor.vim = list::vim(state);
            self.editor = Some((editor, row, value));
            return ViewOutcome::Consumed;
        }
        let key = setting.key.clone();
        if let Some(&at) = self.by_key.get(&key) {
            self.settings[at].value = value.clone();
            self.settings[at].layer = layer::GLOBAL.to_owned();
        }
        let words = self.words(
            &Shown {
                row,
                setting: self.setting(&key),
            },
            state,
        );
        self.note(row.label, &words);
        ViewOutcome::SaveQuietly(UiCommand::SetSetting {
            key,
            value,
            scope: SettingScope::Global,
        })
    }

    /// A key while the question of where to save shows. Its answer sends the change, which
    /// waits for the app's word; Esc and ← leave the row as it was.
    fn editor_key(&mut self, key: KeyEvent, state: &AppState) -> ViewOutcome {
        let Some((editor, row, value)) = self.editor.take() else {
            return ViewOutcome::Consumed;
        };
        let mut open = Some(editor);
        let outcome = edit_key(&mut open, key).unwrap_or(ViewOutcome::Consumed);
        if let Some(editor) = open {
            self.editor = Some((editor, row, value));
        }
        match outcome {
            ViewOutcome::Send(command @ UiCommand::SetSetting { .. }) => {
                if let UiCommand::SetSetting { key, value, .. } = &command {
                    let was = self
                        .setting(key)
                        .map(|setting| setting.value.clone())
                        .unwrap_or_default();
                    self.set_value(key, value);
                    let words = self.words(
                        &Shown {
                            row,
                            setting: self.setting(key),
                        },
                        state,
                    );
                    self.waiting.push(Waiting {
                        label: row.label,
                        key: key.clone(),
                        value: value.clone(),
                        words,
                        was,
                    });
                }
                ViewOutcome::SaveQuietly(command)
            }
            ViewOutcome::Ignored => ViewOutcome::Consumed,
            outcome => outcome,
        }
    }

    /// The question of where to save a row's change, while it shows.
    pub fn editor(&self) -> Option<&Picker> {
        self.editor.as_ref().map(|(editor, ..)| editor)
    }

    /// Whether a change waits for the app's word, which may come as a question the tab stays
    /// up over.
    pub fn waits(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// Takes in what the app made of the changes the tab sent: a saved one joins Esc's line,
    /// and one left as it was takes its old value back.
    pub fn read_outcomes(&mut self, state: &AppState) {
        let all = &state.setting_outcomes;
        let read = *self.outcomes_read.get_or_insert(all.len());
        self.outcomes_read = Some(all.len());
        for outcome in all.get(read..).unwrap_or_default() {
            match outcome {
                SettingOutcome::Changed { key, value } => {
                    let saved = self
                        .waiting
                        .iter()
                        .position(|waiting| waiting.key == *key && same(&waiting.value, value));
                    if let Some(at) = saved {
                        let waiting = self.waiting.remove(at);
                        self.note(waiting.label, &waiting.words);
                    }
                }
                SettingOutcome::NotChanged if !self.waiting.is_empty() => {
                    let waiting = self.waiting.remove(0);
                    self.set_value(&waiting.key, &waiting.was);
                }
                SettingOutcome::NotChanged => {}
            }
        }
    }

    fn set_value(&mut self, key: &str, value: &str) {
        if let Some(&at) = self.by_key.get(key) {
            self.settings[at].value = value.to_owned();
        }
    }

    /// Keeps a change for Esc's line, once per row with its newest words.
    fn note(&mut self, label: &str, words: &str) {
        // Last used names the mode it brings back only on the row, not in what changed.
        let words = words.split(" (").next().unwrap_or(words).to_owned();
        match self.changed.iter_mut().find(|(known, _)| known == label) {
            Some(change) => change.1 = words,
            None => self.changed.push((label.to_owned(), words)),
        }
    }

    /// What closing the panel leaves under the command: each change, or that nothing changed
    /// when the tab was what the command opened.
    pub fn close(&self) -> ViewOutcome {
        let line = if self.changed.is_empty() {
            DISMISSED.to_owned()
        } else {
            self.changed
                .iter()
                .map(|(label, words)| format!("Set {label} to {words}"))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        match &self.opened_by {
            Some(command) => ViewOutcome::CloseSaying {
                command: format!("/{command}"),
                line,
            },
            None if !self.changed.is_empty() => ViewOutcome::CloseSaying {
                command: "/config".to_owned(),
                line,
            },
            None => ViewOutcome::Close,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    On,
    Back,
}

/// The value after `now` among `allowed`, or before it, wrapping at either end.
fn stepped(allowed: &[String], now: &str, step: Step) -> Option<String> {
    if allowed.is_empty() {
        return None;
    }
    let at = allowed.iter().position(|value| same(value, now));
    let count = allowed.len();
    let next = match (at, step) {
        (None, _) => 0,
        (Some(at), Step::On) => (at + 1) % count,
        (Some(at), Step::Back) => (at + count - 1) % count,
    };
    Some(allowed[next].clone())
}

impl ConfigTab {
    /// A row that opens a command's picker leaves the panel for it, as the picker draws under
    /// the input. What the tab changed before stays said under the command that opened it.
    fn open_command(&self, command: &str) -> ViewOutcome {
        let Ok(ParsedInput::Slash { command }) = parse_input(&format!("/{command}")) else {
            return ViewOutcome::Consumed;
        };
        let then = UiCommand::Slash { command };
        match self.close() {
            ViewOutcome::CloseSaying { command, line } if !self.changed.is_empty() => {
                ViewOutcome::CloseSayingThen {
                    command,
                    line,
                    then,
                }
            }
            _ => ViewOutcome::SendToConversation(then),
        }
    }
}

/// Whether the card shows on the bottom line, as the mods the controller last sent say. The
/// built-in line shows the plan usage, so it reads as shown until they say otherwise.
fn on_the_line(card: &str, state: &AppState) -> bool {
    let Some(name) = ModName::from_name(card).and_then(builtin::mod_of_card) else {
        return true;
    };
    state
        .mods
        .as_ref()
        .and_then(|mods| mods.row(name))
        .is_none_or(|row| match &row.kind {
            ModRowKind::Card { file, .. } => file.places.contains(&ModPlace::BottomLine),
            _ => true,
        })
}

/// A link row's value in words: the main agent as it runs, a crew list by its CLIs and
/// models, the effort that applies, a workflow or a theme with a capital.
fn link_words(key: &str, value: &str, state: &AppState) -> String {
    let names = &state.names;
    let worker = |text: &str| {
        text.parse::<ModelRef>()
            .map_or_else(|_| text.to_owned(), |worker| names.worker(&worker))
    };
    match key {
        "crew.main" => main_agent_words(&format::setting_value(value), state),
        "roles.main.effort" => effort_words(key, value, state),
        _ if key.starts_with("crew.") => match value.trim().parse::<toml::Value>() {
            Ok(toml::Value::Array(list)) => list
                .iter()
                .filter_map(toml::Value::as_str)
                .map(worker)
                .collect::<Vec<_>>()
                .join(", "),
            _ => worker(&format::setting_value(value)),
        },
        "workflow" | "ui.theme" => capitalized(&format::setting_value(value)),
        "fallback.same_cli" if value.trim() == "true" => "the same CLI".to_owned(),
        "fallback.same_cli" => "any CLI".to_owned(),
        "" => String::new(),
        _ => row_value_words(key, value),
    }
}

/// The main agent as the header names it: the model the setting names, else the one the
/// main session runs on that CLI, else the CLI's own default.
fn main_agent_words(text: &str, state: &AppState) -> String {
    let names = &state.names;
    let Ok(set) = text.parse::<ModelRef>() else {
        return text.to_owned();
    };
    let runs = state
        .status
        .as_ref()
        .map(|status| &status.main_agent)
        .or_else(|| state.welcome.as_ref()?.main_agent.as_ref())
        .filter(|main| set.model.is_none() && main.agent == set.agent);
    names.worker(&names.running(runs.unwrap_or(&set)))
}

/// The main agent's effort: the level set, else the one its model runs at, as the default.
fn effort_words(key: &str, value: &str, state: &AppState) -> String {
    let runs_at = state
        .status
        .as_ref()
        .and_then(|status| status.line.main_effort.as_deref())
        .map(str::trim)
        .filter(|level| !level.is_empty());
    match runs_at {
        Some(level) if value.trim().is_empty() => format!("{level} (default)"),
        _ => row_value_words(key, value),
    }
}

/// A typed value in words: a spend limit in dollars, anything else as All settings says it.
fn typed_words(key: &str, value: &str) -> String {
    if key.starts_with("limits.spend_")
        && let Ok(amount) = value.trim().parse::<f64>()
    {
        return format!("${amount:.2}");
    }
    row_value_words(key, value)
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Whether two TOML texts hold the same value, however each was quoted.
fn same(left: &str, right: &str) -> bool {
    left.trim() == right.trim() || format::setting_value(left) == format::setting_value(right)
}
