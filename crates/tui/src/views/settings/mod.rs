//! Settings screens: the pickers under the input for `/model`, `/effort`, `/workflow`, and the
//! other settings commands, the scope question, `/config`, and `/approvals`.
//!
//! Keys reach a picker through the runtime: while `bottom.picker` is open it gets every key
//! first, and each [`PickerEvent`] but `Unhandled` comes back here through [`on_picker`].

pub mod all_settings;
pub mod approvals;
mod fallback;
pub mod flow;
mod memory_rows;
mod models;
mod panel;
pub mod permissions;
mod table;
pub mod words;

pub(crate) use panel::{draw_editor, edit_key};

#[cfg(test)]
mod picker_pictures;
#[cfg(test)]
mod tests;

use argonauts_core::app::{MissingCli, ModelChoice, SettingRow, UiCommand, ViewData, ViewRequest};
use argonauts_core::model_picker::{MachineMemory, ModelSource};
use argonauts_core::names::RoleName;
use argonauts_core::settings::ui::UiSettings;
use serde_json::Value;
use time::{OffsetDateTime, UtcOffset};

use crate::bottom::{Picker, PickerEvent};
use crate::bridge::Cx;
use crate::bridge::settings::theme_for;
use crate::cell::{ResultLine, Tone, UserMessage};
use crate::theme::{ColorEnv, Theme};

use all_settings::AllSettingsView;
use approvals::ApprovalsView;
use flow::{Asked, Kept, PALETTE_KEYS, Step};

/// Routes a settings answer to the picker that asked for it (by `command`, `section`, or
/// `role`), or opens `/config` or `/approvals`. An open view that asked gets its answer
/// before this runs.
pub fn open_for(data: &ViewData, cx: &mut Cx<'_>) {
    if cx.replaying() {
        return;
    }
    match data {
        ViewData::Settings {
            rows,
            command,
            section,
            not_installed,
        } => settings_answer(
            rows,
            command.as_deref(),
            section.as_deref(),
            not_installed,
            cx,
        ),
        ViewData::Models {
            choices,
            role,
            refreshed,
            refreshing,
            update,
            sources,
            memory,
        } => {
            let now = OffsetDateTime::now_utc();
            let note = words::freshness(*refreshed, *refreshing, now);
            let role = role.filter(|role| *role != RoleName::Main);
            let told = Told {
                sources,
                memory: *memory,
                at: (now, cx.state.clock_offset()),
            };
            if *update {
                models_update(choices, role, note.as_deref(), told, cx);
            } else {
                models_answer(choices, role, note.as_deref(), told, cx);
            }
        }
        ViewData::Effort { slider } => {
            let command = match slider.role.filter(|role| *role != RoleName::Main) {
                None => "effort".to_owned(),
                Some(role) => format!("effort {}", role.as_str()),
            };
            open_picker(flow::effort_picker(&command, slider.clone()), cx);
        }
        ViewData::Config { entries } => cx.out.open(AllSettingsView::from_entries(entries)),
        ViewData::Approvals { view } => {
            cx.out.open(ApprovalsView::new(view.clone()));
            cx.out.send(ApprovalsView::rows_request());
        }
        _ => {}
    }
}

fn settings_answer(
    rows: &[SettingRow],
    command: Option<&str>,
    section: Option<&str>,
    not_installed: &[MissingCli],
    cx: &mut Cx<'_>,
) {
    match (command, section) {
        (Some("config"), _) => cx.out.open(AllSettingsView::new(rows.to_vec())),
        (Some("approvals"), _) | (None, Some("approvals")) => {
            cx.out.open(ApprovalsView::with_rows(rows.to_vec()));
            cx.out.send(UiCommand::Request {
                view: ViewRequest::Approvals,
            });
        }
        (Some(command), _) => match cx.model.bottom.picker.as_mut() {
            // The same command's rows again, such as after a change: fresh rows, one echo.
            Some(picker)
                if picker.flow.command == command && picker.flow.stage == flow::Stage::Keys =>
            {
                picker.flow.not_installed = not_installed.to_vec();
                flow::fill_settings(picker, rows.to_vec());
            }
            _ => {
                let picker =
                    flow::settings_picker_noting(command, rows.to_vec(), not_installed.to_vec());
                let asking = flow::asking(&picker);
                open_picker(picker, cx);
                if let Some(view) = asking {
                    cx.out.send(UiCommand::Request { view });
                }
            }
        },
        (None, section) => {
            if cx.model.bottom.picker.is_some() {
                // Another picker is open, so this answers one that was closed.
                return;
            }
            match section {
                None => cx.out.open(AllSettingsView::new(rows.to_vec())),
                Some(section) => {
                    let command = command_for(section);
                    // The `ui` section answers with every key, and `/theme` picks the theme.
                    let rows: Vec<SettingRow> = rows
                        .iter()
                        .filter(|row| command != "theme" || row.key == THEME_KEY)
                        .cloned()
                        .collect();
                    open_picker(flow::settings_picker(command, rows), cx);
                }
            }
        }
    }
}

const THEME_KEY: &str = "ui.theme";

/// The command a section's picker stands for, such as `theme` for `ui`, which is how
/// `/theme` asks for its rows.
fn command_for(section: &str) -> &str {
    match section {
        "ui" => "theme",
        other => other,
    }
}

/// Each list a model answer came from, and when it came, in the terminal's zone.
struct Told<'a> {
    sources: &'a [ModelSource],
    memory: Option<MachineMemory>,
    at: (OffsetDateTime, UtcOffset),
}

impl Told<'_> {
    fn onto(&self, picker: &mut Picker) {
        picker.flow.sources = self.sources.to_vec();
        picker.flow.memory = self.memory;
        picker.flow.told_at = Some(self.at);
    }
}

fn models_answer(
    choices: &[ModelChoice],
    role: Option<RoleName>,
    freshness: Option<&str>,
    told: Told<'_>,
    cx: &mut Cx<'_>,
) {
    let asked = Asked::Models(role);
    if let Some(picker) = waiting_for(cx, &asked) {
        told.onto(picker);
        flow::fill_models(picker, role, choices);
        flow::show_freshness(picker, freshness);
        return;
    }
    if cx
        .model
        .bottom
        .picker
        .as_ref()
        .is_some_and(|picker| picker.flow.waiting.is_some() || models::on_roles(&picker.flow))
    {
        // The open picker waits for another role's models, or went back to the roles from
        // this one's: either way this one's picker was left.
        return;
    }
    let command = match role {
        None => "model".to_owned(),
        Some(role) => format!("model {}", role.as_str()),
    };
    let mut picker = flow::models_picker(&command, role, choices);
    told.onto(&mut picker);
    flow::fill_models(&mut picker, role, choices);
    flow::show_freshness(&mut picker, freshness);
    open_picker(picker, cx);
}

/// A refreshed list reaches the picker showing this role's models, at whichever level, keeping
/// the pointer on the same row, and opens nothing when that picker is gone.
fn models_update(
    choices: &[ModelChoice],
    role: Option<RoleName>,
    freshness: Option<&str>,
    told: Told<'_>,
    cx: &mut Cx<'_>,
) {
    let Some(picker) = cx.model.bottom.picker.as_mut() else {
        return;
    };
    let showing = match &picker.flow.stage {
        flow::Stage::Models { role }
        | flow::Stage::ModelsOf { role, .. }
        | flow::Stage::OtherModel { role, .. } => Some(*role),
        _ => None,
    };
    if showing != Some(role) {
        return;
    }
    picker.flow.freshness = freshness.map(str::to_owned);
    told.onto(picker);
    models::refresh(picker, choices);
}

fn waiting_for<'a>(cx: &'a mut Cx<'_>, asked: &Asked) -> Option<&'a mut Picker> {
    cx.model
        .bottom
        .picker
        .as_mut()
        .filter(|picker| picker.flow.waiting.as_ref() == Some(asked))
}

/// `/model`'s picker, while it's open, has the local servers asked every few seconds.
fn watches_models(picker: &Picker) -> bool {
    picker.flow.command.split_whitespace().next() == Some("model")
}

/// Tells the controller when `/model`'s picker opens or closes.
fn watch_models(was: Option<&Picker>, now: Option<&Picker>, cx: &mut Cx<'_>) {
    let (was, now) = (
        was.is_some_and(watches_models),
        now.is_some_and(watches_models),
    );
    if was != now {
        cx.out.send(UiCommand::WatchModels { watching: now });
    }
}

/// Opens a picker under the input, with the command's echo above it in the conversation.
fn open_picker(mut picker: Picker, cx: &mut Cx<'_>) {
    let old = cx.model.bottom.picker.take();
    watch_models(old.as_ref(), Some(&picker), cx);
    if old.is_some_and(|old| old.flow.previewing) {
        restore_palette(cx);
    }
    cx.model
        .history
        .push(UserMessage::command(format!("/{}", picker.flow.command)));
    picker.flow.echoed_at = Some(cx.model.history.cells().len());
    picker.vim = cx.model.bottom.mode.vim.is_some();
    cx.model.bottom.picker = Some(picker);
}

/// Acts on a key the open picker took: moves on to the next question, previews a palette,
/// asks the controller, sends a command and stays, or sends the change and closes.
pub fn on_picker(event: &PickerEvent, cx: &mut Cx<'_>) {
    let Some(picker) = cx.model.bottom.picker.as_mut() else {
        return;
    };
    let previewing = picker.flow.previewing;
    match flow::advance(picker, event.clone()) {
        Step::Stay => {}
        Step::Preview { key, value } => {
            if let Some(ui) = ui_with(cx.settings, &key, &value) {
                cx.out.theme = Some(palette(&ui, cx));
                if let Some(picker) = cx.model.bottom.picker.as_mut() {
                    picker.flow.previewing = true;
                }
            }
        }
        Step::Ask(view) => cx.out.send(UiCommand::Request { view }),
        Step::Send(command) => cx.out.send(command),
        Step::Done(command) => {
            let keeps_preview = matches!(&command, UiCommand::SetSetting { key, .. }
                if PALETTE_KEYS.contains(&key.as_str()));
            if previewing && !keeps_preview {
                restore_palette(cx);
            }
            let closed = cx.model.bottom.picker.take();
            watch_models(closed.as_ref(), None, cx);
            cx.out.send(command);
        }
        Step::Cancel { kept } => {
            if previewing {
                restore_palette(cx);
            }
            if let Some(picker) = cx.model.bottom.picker.take() {
                watch_models(Some(&picker), None, cx);
                leave_kept_line(&picker, kept, cx);
            }
        }
    }
}

/// `⎿  Kept theme as dark` under the command's echo, with the echo again when something
/// printed between them. The conversation's record keeps it after the command.
fn leave_kept_line(picker: &Picker, kept: Kept, cx: &mut Cx<'_>) {
    let history = &mut cx.model.history;
    let right_under = picker.flow.echoed_at == Some(history.cells().len());
    if !right_under {
        history.push(UserMessage::command(format!("/{}", picker.flow.command)));
    }
    let line = match kept.value {
        Some(value) => ResultLine::valued(kept.words, value),
        None => ResultLine::new(Tone::Normal, kept.words),
    };
    let reply = line.reply();
    let line: Box<dyn crate::cell::HistoryCell> = Box::new(line);
    history.push(line);
    cx.out.replies.push(reply);
}

/// The `[ui]` section with one key changed, when the value fits the key.
fn ui_with(ui: &UiSettings, key: &str, value: &str) -> Option<UiSettings> {
    let field = key.strip_prefix("ui.")?;
    let mut section = serde_json::to_value(ui).ok()?;
    let parsed = toml_value(value)?;
    section.as_object_mut()?.insert(field.to_owned(), parsed);
    serde_json::from_value(section).ok()
}

/// A TOML scalar as JSON, for the palette preview.
fn toml_value(text: &str) -> Option<Value> {
    let text = text.trim();
    if text.starts_with('"') || text.starts_with('\'') {
        return Some(Value::String(crate::format::setting_value(text)));
    }
    match text {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => serde_json::from_str(text).ok(),
    }
}

fn palette(ui: &UiSettings, cx: &Cx<'_>) -> Theme {
    let terminal = &cx.state.terminal;
    let env = terminal
        .color_env
        .clone()
        .unwrap_or_else(ColorEnv::from_process);
    theme_for(ui, &env, terminal.background_is_light)
}

/// Puts back the palette the settings in effect call for, after a preview.
fn restore_palette(cx: &mut Cx<'_>) {
    cx.out.theme = Some(palette(cx.settings, cx));
}
