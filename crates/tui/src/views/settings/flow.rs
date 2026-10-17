//! What a pick means in a settings picker: a key opens its values, a value asks where to save
//! it, a role asks the controller for its models, a CLI opens its models (`models.rs`), a
//! model sends the argument form, and Reset drops the key from its layer.
//! The picker draws and moves; [`advance`] does the rest and says what the screen should do
//! through a [`Step`].

use argonauts_core::app::{MissingCli, ModelChoice, SettingRow, UiCommand, ViewRequest};
use argonauts_core::input::{EFFORT_LEVELS, SLASH_COMMANDS, SlashCommand};
use argonauts_core::model_picker::{EffortLine, EffortSlider};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::SettingScope;

use super::words::{
    SCOPES, capitalized, hidden_from_files, hidden_note, model_role, quote, reset_note,
    reset_scope, role_words, row_value_words, same_value, toml_text, value_note, value_words,
    workers_words,
};
use super::{fallback, models, permissions};
use crate::bottom::{Picker, PickerEvent, PickerRow};
use crate::format::setting_value;
use crate::kit::State;

/// The keys whose values change the palette, so `/theme` shows each one as you move to it.
pub const PALETTE_KEYS: [&str; 5] = [
    "ui.theme",
    "ui.accent",
    "ui.color",
    "ui.motion",
    "ui.glyphs",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Flow {
    /// The command whose picker this is, such as `theme`, for the echo and the Kept line.
    pub command: String,
    /// The answer the picker waits for, so a late or unrelated answer can't land in it.
    pub waiting: Option<Asked>,
    /// Every row of the answer that opened it.
    pub settings: Vec<SettingRow>,
    /// For `/agents`: the agent CLIs this machine doesn't have.
    pub not_installed: Vec<MissingCli>,
    /// What picking each row does, in the picker's row order.
    pub actions: Vec<RowAction>,
    pub stage: Stage,
    /// A palette key's value shows in place of the saved one.
    pub previewing: bool,
    /// Transcript cells once the command's echo was pushed, so the Kept line hangs under the
    /// echo only when nothing printed between them.
    pub echoed_at: Option<usize>,
    /// For `/model`: the controller's whole answer, which both levels draw from.
    pub models: Vec<ModelChoice>,
    /// For `/model`: how fresh the model lists are, after what the choice does.
    pub freshness: Option<String>,
    /// For `/model`'s models level: the highlighted model's effort, under the list. `None`
    /// elsewhere, and on a row whose model takes no effort setting.
    pub effort: Option<EffortLine>,
    /// The level you moved the effort line to, which carries over as the pointer moves.
    pub effort_wanted: Option<String>,
    /// For `/effort`: the slider, drawn in place of rows.
    pub slider: Option<EffortSlider>,
    /// The levels this one opened from, nearest last, for ← to go back to. `/model`'s own
    /// levels are rebuilt from `models` instead, so a refreshed list shows through.
    pub parents: Vec<Level>,
    /// The setting a model pick fills, by its index in `settings`, when the model picker
    /// opened from that setting's row, such as the second judge, rather than for a role.
    pub model_for: Option<usize>,
}

impl Flow {
    /// Whether ←/→ change a value here rather than move between levels.
    pub fn arrows_adjust(&self) -> bool {
        self.slider.is_some() || self.effort.is_some()
    }

    /// Whether ← has a level to go back to.
    pub fn can_go_back(&self) -> bool {
        match &self.stage {
            Stage::ModelsOf { .. } | Stage::OtherModel { .. } => true,
            Stage::Models { .. } => models::opened_from_roles(self) || self.model_for.is_some(),
            Stage::Keys if models::on_roles(self) => true,
            _ => !self.parents.is_empty(),
        }
    }
}

/// What a level showed, kept while a level under it is open.
#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    title: String,
    about: String,
    rows: Vec<PickerRow>,
    selected: usize,
    offset: usize,
    input: Option<String>,
    enter_hint: String,
    esc_hint: String,
    actions: Vec<RowAction>,
    stage: Stage,
}

impl Level {
    fn of(picker: &Picker) -> Self {
        Self {
            title: picker.title.clone(),
            about: picker.about.clone(),
            rows: picker.rows.clone(),
            selected: picker.selected,
            offset: picker.offset,
            input: picker.input.clone(),
            enter_hint: picker.enter_hint.clone(),
            esc_hint: picker.esc_hint.clone(),
            actions: picker.flow.actions.clone(),
            stage: picker.flow.stage.clone(),
        }
    }

    fn restore(self, picker: &mut Picker) {
        picker.set_rows(self.rows);
        picker.title = self.title;
        picker.about = self.about;
        picker.selected = self.selected;
        picker.offset = self.offset;
        picker.input = self.input;
        picker.enter_hint = self.enter_hint;
        picker.esc_hint = self.esc_hint;
        picker.flow.actions = self.actions;
        picker.flow.stage = self.stage;
    }
}

/// What a picker asked the controller for.
#[derive(Debug, Clone, PartialEq)]
pub enum Asked {
    Models(Option<RoleName>),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Stage {
    /// Keys, roles, or CLIs to open.
    #[default]
    Keys,
    /// The values of one key, by its index in `settings`.
    Values { key: usize },
    /// A value typed for one key.
    Typing { key: usize },
    /// The CLIs and endpoints a role may use. `None` is the main agent.
    Models { role: Option<RoleName> },
    /// One CLI's or endpoint's models, by its id.
    ModelsOf { role: Option<RoleName>, cli: String },
    /// A model name being typed for one CLI or endpoint.
    OtherModel { role: Option<RoleName>, cli: String },
    /// A new endpoint: its id, base URL, and the env var holding its key.
    Endpoint,
    /// Where to save a picked value.
    Scope(Change),
    /// `/effort`'s slider for the main agent or a role, and the level it opened on.
    /// `was` is `None` when the CLI uses its own effort, with no level set.
    Effort {
        role: Option<RoleName>,
        was: Option<String>,
    },
}

/// A value picked and waiting for its scope.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub key: String,
    /// TOML text.
    pub value: String,
    /// The layer that sets the key now, for the warning when it would hide the change.
    pub layer: String,
    /// Where the workflow was picked, when `layer` is the workflow and a file picked it.
    pub workflow_from: Option<String>,
    /// The Kept line if you cancel.
    pub kept: Kept,
}

/// The line left under the command's echo when the picker closes with no change, such as
/// `Kept model as Opus 5.5`, the value drawn in the accent color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub words: String,
    pub value: Option<String>,
}

impl Kept {
    pub fn as_value(name: &str, value: impl Into<String>) -> Self {
        Self {
            words: format!("Kept {name} as"),
            value: Some(value.into()),
        }
    }

    pub fn words(words: impl Into<String>) -> Self {
        Self {
            words: words.into(),
            value: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowAction {
    OpenKey(usize),
    /// The keys under a prefix, such as one CLI's `agents.codex`.
    OpenGroup(String),
    /// `/model`'s roles, one level under the main agent's CLIs.
    OpenRoles,
    AskModels(Option<RoleName>),
    /// The model picker for a setting that takes a model, listing what `role` may use.
    ModelFor {
        key: usize,
        role: RoleName,
    },
    Value {
        key: usize,
        value: String,
    },
    Type {
        key: usize,
    },
    Reset {
        key: usize,
    },
    OpenCli {
        role: Option<RoleName>,
        cli: String,
    },
    Worker {
        role: Option<RoleName>,
        worker: ModelRef,
    },
    OtherModel {
        role: Option<RoleName>,
        cli: String,
    },
    AddEndpoint,
    Scope(SettingScope),
}

/// What the screen does after a key in a picker.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Stay,
    /// Show the palette with this value of a palette key.
    Preview {
        key: String,
        value: String,
    },
    /// Ask the controller; the picker now waits for the answer.
    Ask(ViewRequest),
    /// Send this and close.
    Done(UiCommand),
    /// Close with nothing sent, leaving this line under the command's echo.
    Cancel {
        kept: Kept,
    },
}

/// The picker for a settings command's rows, or a section's.
pub fn settings_picker(command: &str, rows: Vec<SettingRow>) -> Picker {
    settings_picker_noting(command, rows, Vec::new())
}

/// Like [`settings_picker`], with the agent CLIs `/agents` lists as not installed.
pub fn settings_picker_noting(
    command: &str,
    rows: Vec<SettingRow>,
    not_installed: Vec<MissingCli>,
) -> Picker {
    let mut picker = Picker::new(capitalized(command), command_help(command));
    picker.flow.command = command.to_owned();
    picker.flow.not_installed = not_installed;
    fill_settings(&mut picker, rows);
    // `/model` opens on the main agent's CLIs, with the roles a row away.
    if command == "model" {
        ask_models(&mut picker, None);
    }
    picker
}

/// What the picker waits for from the controller, to ask for it.
pub fn asking(picker: &Picker) -> Option<ViewRequest> {
    picker
        .flow
        .waiting
        .as_ref()
        .map(|Asked::Models(role)| ViewRequest::Models { role: *role })
}

/// New rows for a picker, keeping its command and echo.
pub fn fill_settings(picker: &mut Picker, rows: Vec<SettingRow>) {
    picker.flow.waiting = None;
    picker.flow.parents.clear();
    picker.flow.settings = rows;
    picker.placeholder = "Nothing to set here".to_owned();
    let command = picker.flow.command.clone();
    match command.as_str() {
        "model" => role_list(picker),
        "agents" => agents_list(picker),
        "permissions" => permissions::list(picker),
        _ if picker.flow.settings.len() == 1 => {
            picker.title = capitalized(&command);
            open_key(picker, 0);
        }
        "fallback" => fallback::list(picker),
        _ => key_list(picker, None),
    }
}

/// `/config`'s editor for one row: its values, or a typed line, then the scope question.
pub fn edit_picker(row: SettingRow) -> Picker {
    let mut picker = Picker::new(row.key.clone(), "");
    picker.flow.command = row.key.clone();
    picker.flow.settings = vec![row];
    open_key(&mut picker, 0);
    picker
}

/// Where to save `value` for `row`, as a row that changes in place and then asks where opens
/// it: straight at the question, its value picked already. Esc leaves nothing changed.
pub fn scope_picker(row: SettingRow, value: String) -> Picker {
    let mut picker = Picker::new(row.key.clone(), "");
    picker.flow.command = row.key.clone();
    let change = Change {
        key: row.key.clone(),
        value,
        layer: row.layer.clone(),
        workflow_from: row.workflow_from.clone(),
        kept: Kept::words("Nothing changed"),
    };
    let default = row.default_scope;
    picker.flow.settings = vec![row];
    scope_question(&mut picker, change, default);
    picker
}

/// The model picker for a role, from the controller's `Models` answer.
pub fn models_picker(command: &str, role: Option<RoleName>, choices: &[ModelChoice]) -> Picker {
    let mut picker = Picker::default();
    picker.flow.command = command.to_owned();
    fill_models(&mut picker, role, choices);
    picker
}

/// The first level of the model picker: the CLIs and endpoints the role may use.
pub fn fill_models(picker: &mut Picker, role: Option<RoleName>, choices: &[ModelChoice]) {
    let role = role.filter(|role| *role != RoleName::Main);
    models::fill(picker, role, choices);
}

/// Adds how fresh the model lists are after what the choice does.
pub fn show_freshness(picker: &mut Picker, note: Option<&str>) {
    picker.flow.freshness = note.map(str::to_owned);
    if let Some(note) = note {
        let about = picker.about.trim_end_matches('.');
        picker.about = format!("{about}. {note}");
    }
}

/// Takes a picker event and says what the screen should do.
pub fn advance(picker: &mut Picker, event: PickerEvent) -> Step {
    let step = match event {
        PickerEvent::Moved => preview(picker),
        PickerEvent::Picked(index) | PickerEvent::Reset(index) => {
            match picker.flow.actions.get(index).cloned() {
                Some(action) => act(picker, action),
                None => Step::Stay,
            }
        }
        PickerEvent::Entered(text) => entered(picker, &text),
        PickerEvent::Left => sideways(picker, false),
        PickerEvent::Right => sideways(picker, true),
        PickerEvent::Confirmed => confirmed(picker),
        PickerEvent::Toggled => {
            if let Some(slider) = picker.flow.slider.as_mut() {
                slider.toggle_ultracode();
            }
            Step::Stay
        }
        PickerEvent::Closed => back(picker).unwrap_or_else(|| Step::Cancel { kept: kept(picker) }),
        PickerEvent::Unhandled => Step::Stay,
    };
    models::follow_pointer(picker);
    step
}

/// ←/→: the value on the arrows when there is one, else back a level or into the row.
fn sideways(picker: &mut Picker, right: bool) -> Step {
    if let Some(slider) = picker.flow.slider.as_mut() {
        if right {
            slider.raise();
        } else {
            slider.lower();
        }
        return Step::Stay;
    }
    if let Some(line) = picker.flow.effort.as_mut() {
        // A model's only level is no choice of yours to carry to the next model.
        if line.is_fixed() {
            return Step::Stay;
        }
        if right {
            line.raise();
        } else {
            line.lower();
        }
        picker.flow.effort_wanted = line.level().map(str::to_owned);
        return Step::Stay;
    }
    if !right {
        return go_back(picker);
    }
    let pointed = picker
        .pointed()
        .and_then(|index| picker.flow.actions.get(index))
        .cloned();
    match pointed {
        Some(
            action @ (RowAction::OpenKey(_)
            | RowAction::OpenGroup(_)
            | RowAction::OpenRoles
            | RowAction::AskModels(_)
            | RowAction::OpenCli { .. }
            | RowAction::OtherModel { .. }
            | RowAction::Type { .. }
            | RowAction::AddEndpoint),
        ) => act(picker, action),
        // A row that sets a value waits for Enter.
        _ => Step::Stay,
    }
}

/// ← goes up a level where Esc would, and elsewhere back to the level this one opened from.
/// The first level stays.
fn go_back(picker: &mut Picker) -> Step {
    if let Some(step) = back(picker) {
        return step;
    }
    if let Some(level) = picker.flow.parents.pop() {
        level.restore(picker);
    }
    Step::Stay
}

/// Keeps the level on screen for ← to come back to.
fn push_level(picker: &mut Picker) {
    let level = Level::of(picker);
    picker.flow.parents.push(level);
}

/// Enter on the effort slider sends the level, and Ultracode when Tab changed it, which then
/// asks where to save them. With only Ultracode changed, the level stays as it was rather than
/// being saved, so flipping the toggle never pins the model's default. A model that takes one
/// level, or none, has no level to save, so Enter sends only a changed toggle, and otherwise
/// closes as Esc does.
fn confirmed(picker: &Picker) -> Step {
    match (&picker.flow.stage, &picker.flow.slider) {
        (Stage::Effort { role, was }, Some(slider)) => {
            let ultracode = slider
                .ultracode
                .filter(|toggle| toggle.changed())
                .map(|toggle| toggle.on);
            let level = slider.level().filter(|_| slider.moves());
            let unmoved = slider.cli_default || level.is_none() || was.as_deref() == level;
            let level = level.filter(|_| ultracode.is_none() || !unmoved);
            if level.is_none() && ultracode.is_none() {
                return Step::Cancel { kept: kept(picker) };
            }
            Step::Done(UiCommand::Slash {
                command: SlashCommand::Effort {
                    role: *role,
                    level: level.map(str::to_owned),
                    ultracode,
                },
            })
        }
        _ => Step::Stay,
    }
}

/// `/effort`'s slider, for the main agent or a role.
pub fn effort_picker(command: &str, slider: EffortSlider) -> Picker {
    let role = slider.role.filter(|role| *role != RoleName::Main);
    let title = match role {
        None => "Effort".to_owned(),
        Some(role) => format!("Effort for {}", role.as_str()),
    };
    let model = match &slider.worker.model {
        Some(_) => slider.model.clone(),
        None => format!(
            "{}'s default model",
            crate::format::agent_label(&slider.worker.agent)
        ),
    };
    let who = match role {
        None => "the main agent".to_owned(),
        Some(role) => format!("the {} role", role.as_str()),
    };
    let mut picker = Picker::new(
        title,
        format!("How long {model} thinks before it answers, for {who}"),
    );
    picker.flow.command = command.to_owned();
    picker.flow.stage = Stage::Effort {
        role,
        was: slider
            .level()
            .filter(|_| !slider.cli_default)
            .map(str::to_owned),
    };
    picker.flow.slider = Some(slider);
    picker.enter_hint = "Enter to confirm".to_owned();
    picker
}

fn act(picker: &mut Picker, action: RowAction) -> Step {
    match action {
        RowAction::OpenKey(key) => {
            push_level(picker);
            open_key(picker, key);
            preview(picker)
        }
        RowAction::OpenGroup(prefix) => {
            push_level(picker);
            if picker.flow.command == "permissions" {
                permissions::role_list(picker, &prefix);
            } else if picker.flow.command == "fallback" {
                fallback::role_list(picker, &prefix);
            } else {
                key_list(picker, Some(&prefix));
            }
            Step::Stay
        }
        RowAction::OpenRoles => {
            role_list(picker);
            Step::Stay
        }
        RowAction::AskModels(role) => ask_models(picker, role),
        RowAction::ModelFor { key, role } => {
            push_level(picker);
            picker.flow.model_for = Some(key);
            ask_models(picker, Some(role))
        }
        RowAction::Value { key, value } => set_value(picker, key, value),
        RowAction::Type { key } => {
            push_level(picker);
            typing(picker, key);
            Step::Stay
        }
        RowAction::Reset { key } => {
            let Some(row) = picker.flow.settings.get(key) else {
                return Step::Stay;
            };
            match reset_scope(&row.layer) {
                Some(scope) => Step::Done(UiCommand::ResetSetting {
                    key: row.key.clone(),
                    scope,
                }),
                None => Step::Stay,
            }
        }
        RowAction::OpenCli { role, cli } => {
            models::open_cli(picker, role, &cli);
            Step::Stay
        }
        RowAction::Worker { role, worker } => match picker.flow.model_for {
            Some(key) => set_value(picker, key, quote(&worker.to_string())),
            None => models::pick_listed(picker, role, worker),
        },
        RowAction::OtherModel { role, cli } => {
            models::other_model(picker, role, &cli);
            Step::Stay
        }
        RowAction::AddEndpoint => {
            push_level(picker);
            picker.title = "Add an endpoint".to_owned();
            picker.about = "Type its id, the base URL, and the env var holding its key \
                            (leave that out for a local server)"
                .to_owned();
            picker.set_rows(Vec::new());
            picker.flow.actions.clear();
            picker.input = Some(String::new());
            picker.enter_hint = "Enter to continue".to_owned();
            picker.flow.stage = Stage::Endpoint;
            Step::Stay
        }
        RowAction::Scope(scope) => match &picker.flow.stage {
            Stage::Scope(change) => Step::Done(UiCommand::SetSetting {
                key: change.key.clone(),
                value: change.value.clone(),
                scope,
            }),
            _ => Step::Stay,
        },
    }
}

/// A value picked for the setting at `key`, which then asks where to save it.
fn set_value(picker: &mut Picker, key: usize, value: String) -> Step {
    let Some(row) = picker.flow.settings.get(key).cloned() else {
        return Step::Stay;
    };
    let kept = kept(picker);
    let change = Change {
        key: row.key,
        value,
        layer: row.layer,
        workflow_from: row.workflow_from,
        kept,
    };
    push_level(picker);
    scope_question(picker, change, row.default_scope);
    Step::Stay
}

fn entered(picker: &mut Picker, text: &str) -> Step {
    match picker.flow.stage.clone() {
        Stage::Typing { key } => {
            let Some(row) = picker.flow.settings.get(key).cloned() else {
                return Step::Stay;
            };
            let kept = kept(picker);
            let change = Change {
                value: toml_text(text, &row.value),
                key: row.key,
                layer: row.layer,
                workflow_from: row.workflow_from,
                kept,
            };
            push_level(picker);
            scope_question(picker, change, row.default_scope);
            Step::Stay
        }
        Stage::Endpoint => match endpoint(text) {
            Ok((id, value)) => {
                let change = Change {
                    key: format!("endpoints.{id}"),
                    value,
                    layer: String::new(),
                    workflow_from: None,
                    kept: kept(picker),
                };
                push_level(picker);
                scope_question(picker, change, SettingScope::Global);
                Step::Stay
            }
            Err(why) => {
                picker.warning = Some(why);
                Step::Stay
            }
        },
        Stage::OtherModel { role, cli } => match models::typed(picker, &cli, text) {
            Some(worker) => match picker.flow.model_for {
                Some(key) => set_value(picker, key, quote(&worker.to_string())),
                None => models::pick(role, worker),
            },
            None => Step::Stay,
        },
        _ => Step::Stay,
    }
}

/// `id base-url [KEY_ENV]` as the endpoint's id and its table.
fn endpoint(text: &str) -> Result<(String, String), String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let [id, url, rest @ ..] = words.as_slice() else {
        return Err(
            "Type the endpoint's id and its base URL, then the env var if it has a key.".into(),
        );
    };
    let id_ok = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if !id_ok {
        return Err("An endpoint id takes letters, digits, - and _ only.".into());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("The base URL starts with http:// or https://.".into());
    }
    let value = match rest {
        [] => format!("{{ base_url = {} }}", quote(url)),
        [env] => format!(
            "{{ base_url = {}, api_key_env = {} }}",
            quote(url),
            quote(env)
        ),
        _ => {
            return Err("Type at most three things: the id, the base URL, and the env var.".into());
        }
    };
    Ok(((*id).to_owned(), value))
}

/// The line left under the command's echo when you close without a change.
fn kept(picker: &Picker) -> Kept {
    let flow = &picker.flow;
    match &flow.stage {
        // `/effort` names the main agent's, as `/model` names its model.
        Stage::Keys if flow.command == "effort" => flow
            .settings
            .iter()
            .find(|row| row.key == "roles.main.effort")
            .map(|row| match value_words(&row.value).as_str() {
                "unset" => Kept::words("Kept effort unset"),
                value => Kept::as_value("effort", value),
            })
            .unwrap_or_else(|| Kept::words("Nothing changed")),
        Stage::Values { key } | Stage::Typing { key } => match flow.settings.get(*key) {
            Some(row) => {
                let name = if flow.settings.len() == 1 && !flow.command.is_empty() {
                    flow.command.clone()
                } else {
                    row.key.clone()
                };
                match value_words(&row.value).as_str() {
                    "unset" => Kept::words(format!("Kept {name} unset")),
                    value if model_role(&row.key).is_some() => {
                        Kept::as_value(&name, models::worker_words(value, &flow.models))
                    }
                    value => Kept::as_value(&name, value),
                }
            }
            None => Kept::words("Nothing changed"),
        },
        Stage::Scope(change) => change.kept.clone(),
        Stage::Models { role } | Stage::ModelsOf { role, .. } | Stage::OtherModel { role, .. } => {
            models::kept(picker, *role)
        }
        // The model takes no effort setting, so only Ultracode was there to change.
        Stage::Effort { .. }
            if flow
                .slider
                .as_ref()
                .is_some_and(|slider| slider.level().is_none()) =>
        {
            Kept::words("Nothing changed")
        }
        Stage::Effort { was: None, .. } => Kept::words("Kept the CLI's own effort"),
        Stage::Effort {
            role,
            was: Some(was),
        } => match role {
            None => Kept::as_value("effort", was.clone()),
            Some(role) => Kept::as_value(&format!("{}'s effort", role.as_str()), was.clone()),
        },
        Stage::Keys | Stage::Endpoint => Kept::words("Nothing changed"),
    }
}

/// The value under the pointer, while it's a palette key's.
fn preview(picker: &Picker) -> Step {
    let Stage::Values { key } = picker.flow.stage else {
        return Step::Stay;
    };
    let Some(row) = picker.flow.settings.get(key) else {
        return Step::Stay;
    };
    if !PALETTE_KEYS.contains(&row.key.as_str()) {
        return Step::Stay;
    }
    let pointed = picker
        .pointed()
        .and_then(|index| picker.flow.actions.get(index));
    match pointed {
        Some(RowAction::Value { value, .. }) => Step::Preview {
            key: row.key.clone(),
            value: value.clone(),
        },
        _ => Step::Stay,
    }
}

fn command_help(command: &str) -> String {
    SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == command)
        .map(|spec| spec.help.to_owned())
        .unwrap_or_default()
}

/// One row per key, or per key under `prefix`.
fn key_list(picker: &mut Picker, prefix: Option<&str>) {
    let settings = &picker.flow.settings;
    let chosen: Vec<usize> = (0..settings.len())
        .filter(|&index| {
            prefix.is_none_or(|prefix| {
                settings[index]
                    .key
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
        })
        .collect();
    let keys: Vec<&str> = chosen.iter().map(|&i| settings[i].key.as_str()).collect();
    let labels = key_labels(&keys);
    let rows = chosen
        .iter()
        .zip(labels)
        .map(|(&index, label)| {
            let row = &settings[index];
            PickerRow::new(label, key_note(row))
        })
        .collect();
    if let Some(prefix) = prefix {
        picker.title = prefix
            .rsplit('.')
            .next()
            .map(str::to_owned)
            .unwrap_or_default();
        picker.about = "Pick a setting to change".to_owned();
    }
    picker.flow.stage = Stage::Keys;
    picker.enter_hint.clear();
    picker.set_rows(rows);
    picker.flow.actions = chosen.into_iter().map(RowAction::OpenKey).collect();
}

/// `/model`'s roles, each with its current worker, a level under the main agent's CLIs.
pub(super) fn role_list(picker: &mut Picker) {
    let settings = &picker.flow.settings;
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    for (index, row) in settings.iter().enumerate() {
        match model_role(&row.key) {
            // The main agent is the level above.
            Some(None) => {}
            Some(role) => {
                rows.push(PickerRow::new(role_words(role), workers_words(&row.value)));
                actions.push(RowAction::AskModels(role));
            }
            None => {
                rows.push(PickerRow::new(row.key.clone(), key_note(row)));
                actions.push(RowAction::OpenKey(index));
            }
        }
    }
    picker.title = "Roles".to_owned();
    picker.about = "Pick a role, then its model".to_owned();
    picker.placeholder = "No roles to set".to_owned();
    picker.flow.stage = Stage::Keys;
    picker.flow.waiting = None;
    picker.enter_hint = "Enter to see its models".to_owned();
    picker.esc_hint = "Esc to go back".to_owned();
    picker.set_rows(rows);
    picker.flow.actions = actions;
}

/// Asks the controller for a role's models; the picker waits for them.
fn ask_models(picker: &mut Picker, role: Option<RoleName>) -> Step {
    if picker.flow.model_for.is_none() {
        picker.title = match role {
            None => role_words(role),
            Some(role) => format!("Model for {}", role.as_str()),
        };
    }
    picker.about.clear();
    picker.placeholder = "Asking which models fit…".to_owned();
    picker.set_rows(Vec::new());
    picker.flow.actions.clear();
    picker.flow.stage = Stage::Models { role };
    picker.flow.waiting = Some(Asked::Models(role));
    Step::Ask(ViewRequest::Models { role })
}

/// Esc goes up a level: a model list back to its CLIs, and in `/model` a role's CLIs back to
/// the roles and the roles back to the main agent's CLIs. `None` on the first level.
fn back(picker: &mut Picker) -> Option<Step> {
    if models::back(picker) {
        return Some(Step::Stay);
    }
    // The CLIs listed for a setting go back to that setting's values.
    if picker.flow.model_for.is_some() && matches!(picker.flow.stage, Stage::Models { .. }) {
        let level = picker.flow.parents.pop()?;
        level.restore(picker);
        picker.flow.model_for = None;
        picker.flow.waiting = None;
        return Some(Step::Stay);
    }
    models::on_roles(&picker.flow).then(|| ask_models(picker, None))
}

/// `/agents`: each CLI and endpoint once, with its state, then "Add an endpoint".
fn agents_list(picker: &mut Picker) {
    let settings = &picker.flow.settings;
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    for (index, row) in settings.iter().enumerate() {
        let group = group_of(&row.key);
        match group {
            Some(group) if groups.contains(&group) => {}
            Some(group) => {
                let (kind, id) = group.split_once('.').unwrap_or(("", group.as_str()));
                let label = if kind == "endpoints" {
                    format!("{id} (endpoint)")
                } else {
                    id.to_owned()
                };
                let missing = picker
                    .flow
                    .not_installed
                    .iter()
                    .find(|cli| kind == "agents" && cli.id == id);
                let (state, note) = match missing {
                    Some(cli) => (Some(State::Failed), not_installed_note(cli)),
                    None => (group_state(settings, &group), group_note(settings, &group)),
                };
                rows.push(PickerRow {
                    state,
                    ..PickerRow::new(label, note)
                });
                actions.push(RowAction::OpenGroup(group.clone()));
                groups.push(group);
            }
            None => {
                rows.push(PickerRow::new(row.key.clone(), key_note(row)));
                actions.push(RowAction::OpenKey(index));
            }
        }
    }
    rows.push(PickerRow::new(
        "Add an endpoint",
        "an OpenAI-compatible server, by its base URL",
    ));
    actions.push(RowAction::AddEndpoint);
    picker.title = "Agents".to_owned();
    picker.about = command_help("agents");
    picker.flow.stage = Stage::Keys;
    picker.enter_hint.clear();
    picker.set_rows(rows);
    picker.flow.actions = actions;
}

/// `agents.codex` for `agents.codex.model`, the unit `/agents` lists.
fn group_of(key: &str) -> Option<String> {
    let mut parts = key.splitn(3, '.');
    let (kind, id, _) = (parts.next()?, parts.next()?, parts.next()?);
    matches!(kind, "agents" | "endpoints").then(|| format!("{kind}.{id}"))
}

/// `not installed · npm i -g opencode-ai@latest`, as the setup words it, in place of a state
/// that would read as ready to use.
fn not_installed_note(cli: &MissingCli) -> String {
    match cli.install.as_deref().map(str::trim) {
        Some(install) if !install.is_empty() => format!("not installed · {install}"),
        _ => "not installed".to_owned(),
    }
}

/// Whether an agent is switched on, where it has a switch: an endpoint has none.
fn switched_on(settings: &[SettingRow], group: &str) -> Option<bool> {
    settings
        .iter()
        .find(|row| row.key == format!("{group}.enabled"))
        .map(|row| value_words(&row.value))
        .filter(|value| value != "unset")
        .map(|value| value != "false")
}

/// The state every view draws for an agent that is on, or off: the mark before `on` and `off`
/// in its note.
fn group_state(settings: &[SettingRow], group: &str) -> Option<State> {
    switched_on(settings, group).map(|on| if on { State::Done } else { State::Stopped })
}

/// A CLI's state in a few words: on or off, model, effort, and auth.
fn group_note(settings: &[SettingRow], group: &str) -> String {
    let value = |leaf: &str| {
        settings
            .iter()
            .find(|row| row.key == format!("{group}.{leaf}"))
            .map(|row| value_words(&row.value))
            .filter(|value| value != "unset")
    };
    let mut parts = Vec::new();
    if let Some(on) = switched_on(settings, group) {
        parts.push(if on { "on" } else { "off" }.to_owned());
    }
    for leaf in ["model", "effort", "auth", "base_url"] {
        if let Some(value) = value(leaf) {
            parts.push(value);
        }
    }
    if let Some(env) = value("api_key_env") {
        parts.push(format!("key in {env}"));
    }
    parts.join(" · ")
}

fn key_note(row: &SettingRow) -> String {
    let description = row.description.trim();
    let value = row_value_words(&row.key, &row.value);
    if description.is_empty() {
        value
    } else {
        format!("{value} · {description}")
    }
}

/// Short labels for keys that share a prefix: each role's name for `roles.<role>.effort`, else
/// the keys without the dotted prefix they share.
fn key_labels(keys: &[&str]) -> Vec<String> {
    let per_role: Option<Vec<(&str, &str)>> = keys
        .iter()
        .map(|key| {
            let rest = key.strip_prefix("roles.")?;
            rest.split_once('.')
        })
        .collect();
    if let Some(per_role) = per_role.filter(|pairs| pairs.len() > 1)
        && per_role.iter().all(|(_, tail)| *tail == per_role[0].1)
    {
        return per_role
            .iter()
            .map(|(role, _)| match *role {
                "main" => "Main agent".to_owned(),
                role => role.to_owned(),
            })
            .collect();
    }
    let split: Vec<Vec<&str>> = keys.iter().map(|key| key.split('.').collect()).collect();
    let shortest = split.iter().map(Vec::len).min().unwrap_or(0);
    let mut shared = 0;
    while shared + 1 < shortest
        && keys.len() > 1
        && split.iter().all(|parts| parts[shared] == split[0][shared])
    {
        shared += 1;
    }
    split
        .iter()
        .map(|parts| parts[shared..].join("."))
        .collect()
}

/// The values of one key, or a typed line when it has none to offer.
fn open_key(picker: &mut Picker, key: usize) {
    let Some(row) = picker.flow.settings.get(key).cloned() else {
        return;
    };
    let resettable = reset_scope(&row.layer);
    let mut allowed = row.allowed.clone();
    // A model that lists no levels takes every one.
    if allowed.is_empty() && row.key.ends_with(".effort") {
        allowed = EFFORT_LEVELS.iter().map(|level| quote(level)).collect();
    }
    if let Some(role) = model_setting(&row.key) {
        if picker.flow.settings.len() > 1 || picker.title.is_empty() {
            picker.title = row.key.clone();
        }
        model_values(picker, key, &row, role);
        return;
    }
    if allowed.is_empty() && resettable.is_none() {
        typing(picker, key);
        return;
    }
    if picker.flow.settings.len() > 1 || picker.title.is_empty() {
        picker.title = row.key.clone();
    }
    picker.about = row.description.clone();
    match picker.flow.command.as_str() {
        "permissions" => {
            (picker.title, picker.about) = permissions::heading(&picker.flow.settings, &row);
        }
        "fallback" if picker.flow.settings.len() > 1 => {
            (picker.title, picker.about) = fallback::heading(&picker.flow.settings, &row);
        }
        _ => {}
    }
    picker.enter_hint.clear();
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    if allowed.is_empty() {
        rows.push(PickerRow::new(
            "Type a new value",
            format!("now {}", value_words(&row.value)),
        ));
        actions.push(RowAction::Type { key });
    }
    for value in &allowed {
        let note = value_note(&row.key, &setting_value(value));
        rows.push(PickerRow {
            current: same_value(value, &row.value),
            ..PickerRow::new(permissions::value_in_words(&row.key, value), note)
        });
        actions.push(RowAction::Value {
            key,
            value: value.clone(),
        });
    }
    if let Some(scope) = resettable {
        rows.push(PickerRow {
            reset: true,
            ..PickerRow::new("Reset", reset_note(scope))
        });
        actions.push(RowAction::Reset { key });
    }
    picker.set_rows(rows);
    picker.flow.actions = actions;
    picker.flow.stage = Stage::Values { key };
    if let Some(current) = picker.rows.iter().position(|row| row.current) {
        picker.point_at(current);
    }
}

/// A setting that takes a model, such as the second judge: its words, if it has any, then
/// the model picker of every agent's models `role` may use, then Reset.
fn model_values(picker: &mut Picker, key: usize, row: &SettingRow, role: RoleName) {
    picker.about = row.description.clone();
    picker.enter_hint.clear();
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    let words = model_setting_words(&row.key);
    for (word, label, note) in words {
        let value = quote(word);
        rows.push(PickerRow {
            current: same_value(&value, &row.value),
            ..PickerRow::new(*label, *note)
        });
        actions.push(RowAction::Value { key, value });
    }
    let set = setting_value(row.value.trim());
    let picked = !set.is_empty() && !words.iter().any(|(word, ..)| *word == set);
    let note = if picked {
        format!("now {set}")
    } else {
        "From every agent's models".to_owned()
    };
    rows.push(PickerRow {
        current: picked,
        ..PickerRow::new("Pick a model…", note)
    });
    actions.push(RowAction::ModelFor { key, role });
    if let Some(scope) = reset_scope(&row.layer) {
        rows.push(PickerRow {
            reset: true,
            ..PickerRow::new("Reset", reset_note(scope))
        });
        actions.push(RowAction::Reset { key });
    }
    picker.set_rows(rows);
    picker.flow.actions = actions;
    picker.flow.stage = Stage::Values { key };
    if let Some(current) = picker.rows.iter().position(|row| row.current) {
        picker.point_at(current);
    }
}

/// The settings that take a model from the model picker, with the role whose models it lists.
pub fn model_setting(key: &str) -> Option<RoleName> {
    match key {
        "approvals.answers" => Some(RoleName::TieBreaker),
        "approvals.decides" | "approvals.judge.second_model" => Some(RoleName::Judge),
        _ => None,
    }
}

/// The words such a setting takes besides a model, each with its label and what it means.
fn model_setting_words(key: &str) -> &'static [(&'static str, &'static str, &'static str)] {
    match key {
        "approvals.answers" => &[
            ("you", "You", "Questions wait for you, except on autopilot"),
            (
                "main",
                "The main agent",
                "The main agent answers; you review its assumptions",
            ),
        ],
        "approvals.decides" => &[(
            "judge",
            "The judge",
            "The judge your crew names, else the cheapest model from another provider",
        )],
        _ => &[],
    }
}

fn typing(picker: &mut Picker, key: usize) {
    let Some(row) = picker.flow.settings.get(key).cloned() else {
        return;
    };
    if picker.flow.settings.len() > 1 || picker.title.is_empty() {
        picker.title = row.key.clone();
    }
    picker.about = if row.description.trim().is_empty() {
        "Type the new value".to_owned()
    } else {
        row.description.clone()
    };
    if picker.flow.command == "fallback" && picker.flow.settings.len() > 1 {
        (picker.title, picker.about) = fallback::heading(&picker.flow.settings, &row);
    }
    picker.set_rows(Vec::new());
    picker.flow.actions.clear();
    let current = row.value.trim();
    picker.input = Some(if current.is_empty() {
        String::new()
    } else {
        setting_value(current)
    });
    picker.enter_hint = "Enter to continue".to_owned();
    picker.flow.stage = Stage::Typing { key };
}

/// Where to save a change: four numbered rows, the default picked, and a warning on each
/// scope a higher layer would hide it in, or once over them all when it hides every file.
fn scope_question(picker: &mut Picker, change: Change, default: SettingScope) {
    picker.title = "Where to save it".to_owned();
    picker.about = format!("{} = {}", change.key, value_words(&change.value));
    let workflow_from = change.workflow_from.as_deref();
    let once = hidden_from_files(&change.layer, workflow_from);
    let rows = SCOPES
        .iter()
        .map(|&(scope, label, place)| PickerRow {
            warning: (once.is_none() && scope.hidden_by_value(&change.layer, workflow_from))
                .then(|| hidden_note(&change.layer, workflow_from, scope)),
            ..PickerRow::new(label, place)
        })
        .collect();
    picker.set_rows(rows);
    picker.warning = once;
    picker.enter_hint = "Enter to save".to_owned();
    picker.flow.actions = SCOPES
        .iter()
        .map(|&(scope, ..)| RowAction::Scope(scope))
        .collect();
    if let Some(at) = SCOPES.iter().position(|(scope, ..)| *scope == default) {
        picker.point_at(at);
    }
    picker.flow.stage = Stage::Scope(change);
}
