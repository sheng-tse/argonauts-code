//! What a settings picker keeps while you go through it, and what a pick asks the screen to do.

use argonauts_core::app::model_picker::{EffortLine, EffortSlider, ModelSource};
use argonauts_core::app::{MissingCli, ModelChoice, SettingRow, UiCommand, ViewRequest};
use argonauts_core::base::names::{ModelRef, RoleName};

use super::{Picker, PickerRow};

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
    /// For `/model`: each list the choices come from, with when it was read and whether it's
    /// being asked again, so each level says how fresh its own list is.
    pub sources: Vec<ModelSource>,
    /// For `/model`: what this machine gives models, for the level of a CLI that runs some
    /// here.
    pub memory: Option<argonauts_core::app::model_picker::MachineMemory>,
    /// When the controller's answer came and the terminal's zone, which those times are
    /// worded from.
    pub told_at: Option<(time::OffsetDateTime, time::UtcOffset)>,
    /// For `/model`: the models its first answer listed, so one that arrives while it's open
    /// reads "new" until it closes.
    pub opened_with: Option<Vec<ModelRef>>,
    /// A model not installed whose download Enter asked about, which Enter again starts.
    pub download_asked: Option<ModelRef>,
    /// For `/model`'s models level: the highlighted model's effort, under the list, with no
    /// levels for a model that takes no effort setting. `None` elsewhere.
    pub effort: Option<EffortLine>,
    /// The level you moved the effort line to, which carries over as the pointer moves.
    pub effort_wanted: Option<String>,
    /// On `/model`'s models level, the model the pointer was on last, which "Other model…"
    /// opens holding so it can be edited into a full id.
    pub pointed_model: Option<String>,
    /// For `/effort`: the slider, drawn in place of rows.
    pub slider: Option<EffortSlider>,
    /// The levels this one opened from, nearest last, for ← to go back to. `/model`'s own
    /// levels are rebuilt from `models` instead, so a refreshed list shows through.
    pub parents: Vec<Level>,
    /// The setting a model pick fills, by its index in `settings`, when the model picker
    /// opened from that setting's row, such as the second judge, rather than for a role.
    pub model_for: Option<usize>,
    /// A settings picker, where ctrl+s picks as Enter does and keeps the change for this
    /// session only.
    pub saves_settings: bool,
}

impl Flow {
    /// Whether ←/→ change a value here rather than move between levels.
    pub fn arrows_adjust(&self) -> bool {
        self.slider.is_some()
            || self
                .effort
                .as_ref()
                .is_some_and(|line| !line.levels.is_empty())
    }

    /// Whether ← has a level to go back to.
    pub fn can_go_back(&self) -> bool {
        match &self.stage {
            Stage::ModelsOf { .. } | Stage::OtherModel { .. } => true,
            Stage::Models { .. } => opened_from_roles(self) || self.model_for.is_some(),
            Stage::Keys if on_roles(self) => true,
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
    pub fn of(picker: &Picker) -> Self {
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

    pub fn restore(self, picker: &mut Picker) {
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
    /// `/effort`'s slider for the main agent or a role, and the level it opened on.
    /// `was` is `None` when the CLI uses its own effort, with no level set.
    Effort {
        role: Option<RoleName>,
        was: Option<String>,
    },
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
    /// Send this and stay open, as a download does, so the list can show what it brought.
    Send(UiCommand),
    /// Close with nothing sent, leaving this line under the command's echo.
    Cancel {
        kept: Kept,
    },
}

/// In `/model`, a role's CLIs sit under the roles, so they go back to them.
pub fn opened_from_roles(flow: &Flow) -> bool {
    flow.command == "model"
        && !flow.settings.is_empty()
        && matches!(flow.stage, Stage::Models { role: Some(_) })
}

/// Whether `/model` shows its roles, which sit under the main agent's CLIs.
pub fn on_roles(flow: &Flow) -> bool {
    flow.command == "model" && flow.stage == Stage::Keys
}
