//! `/model`'s two levels. First the CLIs and endpoints a role may use, each with what it runs
//! and its plan usage, and for the main agent a last row, "Roles…", that opens the other roles.
//! Enter opens one, read like Claude Code's: Default with the model it runs, the models by name
//! with what each is for, newest first, the aliases on one line under Default, and
//! "Other model…" to type any name. Under the list, ←/→ set the highlighted model's effort, and
//! Enter saves both. Esc goes back a level, and closes from the first.

use std::time::Instant;

use time::OffsetDateTime;

use argonauts_core::app::{ModelChoice, UiCommand};
use argonauts_core::input::SlashCommand;
use argonauts_core::limits::{CostEstimate, PlanUsageReading, PlanUsageStatus};
use argonauts_core::logos::{Logo, logo_for};
use argonauts_core::model_catalog::display_name_from_id;
use argonauts_core::model_picker::{EffortLine, ListedFacts, ModelsLevel};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::registry::ModelAccess;
use argonauts_core::registry::provider::{is_known, provider_name};
use argonauts_core::window_size::window_words;

use super::flow::{self, Flow, Kept, RowAction, Stage, Step};
use super::memory_rows;
use super::words::{level_freshness, model_role, source_state, value_words, word_for_a_model};
use crate::bottom::{Picker, PickerCell, PickerColumn, PickerRow, RowPart};
use crate::format::{self, setting_value};
use crate::format::{ClockStyle, reset_time};
use crate::theme::Role;

const DEFAULT_LABEL: &str = "Default";
const RECOMMENDED_LABEL: &str = "Default (recommended)";
const OTHER_LABEL: &str = "Other model…";
const ROLES_LABEL: &str = "Roles…";
/// What "Other model…" takes on a CLI that names a model `provider/model`, with its level
/// after a `#`, as OpenCode does.
const OTHER_ROUTE: &str = "type provider/model, or provider/model#variant";
/// The CLIs that take a model as `provider/model#variant`.
const ROUTED_CLIS: &[&str] = &["opencode"];

/// The first level, from the controller's answer. Opened for a setting, the model that setting
/// holds is the one marked current, not the role's.
pub fn fill(picker: &mut Picker, role: Option<RoleName>, choices: &[ModelChoice]) {
    picker.flow.waiting = None;
    picker.flow.models = choices.to_vec();
    picker.flow.opened_with = Some(choices.iter().map(|choice| choice.worker.clone()).collect());
    if let Some(row) = picker
        .flow
        .model_for
        .and_then(|key| picker.flow.settings.get(key))
    {
        let held = setting_value(row.value.trim());
        for choice in &mut picker.flow.models {
            choice.current = choice.worker.to_string() == held;
        }
    }
    // The answer's own freshness follows through `show_freshness`.
    picker.flow.freshness = None;
    cli_list(picker, role);
}

/// A newer list for the open picker, at whichever level it shows, with its rows changed in
/// place: the pointer stays on the same model, the window stays where it was unless the
/// pointer would leave it, and a filter or a name being typed is kept. A model that wasn't
/// listed when the picker opened reads "new" until it closes.
pub fn refresh(picker: &mut Picker, choices: &[ModelChoice]) {
    let mut choices = choices.to_vec();
    // A model offered for download comes with the memory reading, after the picker opened,
    // so only one the CLI can run now reads "new".
    if let Some(opened_with) = &picker.flow.opened_with {
        for choice in &mut choices {
            if runnable(choice) && !opened_with.contains(&choice.worker) {
                choice.new = true;
            }
        }
    }
    picker.flow.models = choices;
    let pointed = picker
        .pointed()
        .map(|index| row_key(&picker.flow, &picker.rows, index));
    let shown = Picker {
        flow: picker.flow.clone(),
        session_key: picker.session_key.clone(),
        ..Picker::default()
    };
    let mut fresh = shown;
    match picker.flow.stage.clone() {
        Stage::Models { role } => cli_list(&mut fresh, role),
        Stage::ModelsOf { role, cli } => model_list(&mut fresh, role, &cli),
        Stage::OtherModel { .. } => {
            follow_pointer(picker);
            return;
        }
        _ => return,
    }
    picker.title = fresh.title;
    picker.about = fresh.about;
    // The machine's memory is read again with each list, so its line follows the newest.
    picker.banner = fresh.banner;
    picker.columns = fresh.columns;
    picker.placeholder = fresh.placeholder;
    picker.enter_hint = fresh.enter_hint;
    picker.esc_hint = fresh.esc_hint;
    picker.flow.actions = fresh.flow.actions;
    let at = pointed.and_then(|pointed| {
        (0..fresh.rows.len()).position(|index| row_key(&picker.flow, &fresh.rows, index) == pointed)
    });
    picker.replace_rows(fresh.rows, at);
    follow_pointer(picker);
}

/// What the pointer follows across a refresh: a model by its id, and a row with no model of
/// its own, such as Default or "Other model…", by its label.
#[derive(Debug, Clone, PartialEq)]
enum RowKey {
    Model(ModelRef),
    Label(String),
}

fn row_key(flow: &Flow, rows: &[PickerRow], index: usize) -> RowKey {
    match flow.actions.get(index) {
        Some(RowAction::Worker { worker, .. }) if worker.model.is_some() => {
            RowKey::Model(worker.clone())
        }
        _ => RowKey::Label(
            rows.get(index)
                .map(|row| row.label.clone())
                .unwrap_or_default(),
        ),
    }
}

/// How fresh the list at the picker's level is, from that level's own source, when the
/// controller says which sources it asked.
pub(super) fn level_note(picker: &Picker) -> Option<String> {
    let (now, offset) = picker.flow.told_at?;
    let cli = match &picker.flow.stage {
        Stage::Models { .. } => None,
        Stage::ModelsOf { cli, .. } => Some(cli.as_str()),
        _ => return None,
    };
    level_freshness(&picker.flow.sources, cli, now, offset)
}

fn cli_list(picker: &mut Picker, role: Option<RoleName>) {
    picker.flow.stage = Stage::Models { role };
    picker.banner = None;
    picker.note = None;
    // Opened for a setting, the picker keeps that setting's name.
    let for_setting = picker.flow.model_for.is_some();
    picker.title_logo = None;
    if !for_setting {
        picker.title = match role {
            None => "Main agent".to_owned(),
            Some(role) => format!("Model for {}", role.as_str()),
        };
    }
    picker.about = with_freshness(
        picker,
        match role {
            _ if for_setting => "Every agent's CLIs and endpoints you can run, best fit first",
            None => {
                "Who answers in conversation, works as Captain, and steps in last when others stop"
            }
            Some(_) => "The CLIs and endpoints this role's rules allow, best fit first",
        },
    );
    picker.placeholder = "No models fit this role".to_owned();
    picker.enter_hint = "Enter to see its models".to_owned();
    if opened_from_roles(&picker.flow) {
        picker.esc_hint = "Esc to go back".to_owned();
    } else {
        picker.esc_hint.clear();
    }
    let choices = picker.flow.models.clone();
    let groups = groups(&choices);
    let offset = picker.flow.told_at.map(|(_, offset)| offset);
    let told = picker.flow.told_at.map(|(now, _)| now);
    let mut rows: Vec<PickerRow> = groups
        .iter()
        .map(|(cli, models)| {
            let state = picker
                .flow
                .sources
                .iter()
                .find(|source| source.cli == *cli)
                .zip(offset)
                .and_then(|(source, offset)| source_state(source, offset));
            let logo = logo_for(cli, None);
            PickerRow {
                current: models.iter().any(|choice| choice.current),
                cells: cli_cells(models, state, told, &logo),
                logo: Some(logo),
                ..PickerRow::new(format::agent_label(cli), "")
            }
        })
        .collect();
    let mut actions: Vec<RowAction> = groups
        .iter()
        .map(|(cli, _)| RowAction::OpenCli {
            role,
            cli: cli.clone(),
        })
        .collect();
    if role.is_none() && has_roles(picker) {
        rows.push(PickerRow {
            cells: vec![PickerCell::new("each role's own model")],
            ..PickerRow::new(ROLES_LABEL, "")
        });
        actions.push(RowAction::OpenRoles);
    }
    let mut columns = vec![PickerColumn::words(""); CLI_COLUMNS];
    columns[PERCENT] = PickerColumn::numbers("");
    picker.set_table(columns, rows);
    picker.flow.actions = actions;
    if let Some(at) = groups
        .iter()
        .position(|(_, models)| models.iter().any(|choice| choice.current))
    {
        picker.point_at(at);
    }
}

/// One CLI's or endpoint's models.
pub fn open_cli(picker: &mut Picker, role: Option<RoleName>, cli: &str) {
    model_list(picker, role, cli);
    if let Some(at) = picker.rows.iter().position(|row| row.current) {
        picker.point_at(at);
    }
}

fn model_list(picker: &mut Picker, role: Option<RoleName>, cli: &str) {
    picker.flow.stage = Stage::ModelsOf {
        role,
        cli: cli.to_owned(),
    };
    let label = format::agent_label(cli);
    picker.title = label.clone();
    picker.title_logo = Some(logo_for(cli, None));
    let level = ModelsLevel::new(cli, &picker.flow.models);
    // A CLI that lists its models by provider gets one table, its groups' headings naming
    // their columns.
    let table = level.rows().any(|choice| choice.listed.is_some());
    let same_cost = table
        .then(|| memory_rows::same_cost(level.rows()))
        .flatten();
    let mut about = if picker.flow.model_for.is_some() {
        format!("The models {label} runs")
    } else {
        format!("The model {} runs on {label}", who(role))
    };
    if let Some(same) = &same_cost {
        about = format!("{about}. {same}");
    }
    picker.about = with_freshness(picker, &about);
    picker.placeholder = "No models listed".to_owned();
    picker.enter_hint = flow::save_hint(picker);
    picker.esc_hint = "Esc to go back".to_owned();
    // Older models come last with no heading of their own, as Claude Code lists them. A CLI
    // that lists its models by provider heads each provider's group.
    let mut group: Option<&str> = None;
    let mut rows: Vec<PickerRow> = level
        .rows()
        .map(|choice| {
            let starts = listed_group(choice).filter(|heading| group != Some(*heading));
            group = listed_group(choice);
            let heading = starts
                .zip(choice.listed.as_ref())
                .map(|(group, listed)| memory_rows::heading(group, listed));
            // A model's row is named by its model, and a default's by the model it runs.
            let model_in = match choice.worker.model {
                Some(_) => RowPart::Label,
                None => RowPart::Description,
            };
            PickerRow {
                current: choice.current,
                heading,
                warning: no_tools_warning(choice),
                also_matches: choice.worker.model.clone(),
                model_in: Some(model_in),
                ..PickerRow::new(model_label(choice), model_description(choice))
            }
        })
        .collect();
    let memory = picker.flow.memory.filter(|_| {
        level.rows().any(|choice| {
            choice
                .listed
                .as_ref()
                .is_some_and(|listed| listed.memory.is_some())
        })
    });
    picker.banner = memory.as_ref().map(memory_rows::banner);
    for (row, choice) in rows.iter_mut().zip(level.rows()).filter(|_| table) {
        let Some(listed) = &choice.listed else {
            continue;
        };
        if choice.worker.model.is_none() {
            row.description = memory_rows::default_description(choice, listed);
            continue;
        }
        row.cells = memory_rows::cells(choice, listed, memory.as_ref(), same_cost.is_some());
    }
    let other = if ROUTED_CLIS.contains(&cli) {
        OTHER_ROUTE
    } else {
        "type any model name it takes"
    };
    rows.push(PickerRow::new(OTHER_LABEL, other));
    // The aliases hang under Default, or under "Other model…", where you'd type one.
    if let Some(aliases) = alias_line(&level.aliases) {
        let under = if level.default.is_some() {
            0
        } else {
            rows.len() - 1
        };
        rows[under].under = Some(aliases);
    }
    let columns = if table {
        memory_rows::columns()
    } else {
        Vec::new()
    };
    picker.set_table(columns, rows);
    picker.flow.actions = level
        .rows()
        .map(|choice| RowAction::Worker {
            role,
            worker: choice.worker.clone(),
        })
        .chain(std::iter::once(RowAction::OtherModel {
            role,
            cli: cli.to_owned(),
        }))
        .collect();
}

/// Examples of what "Other model…" takes on OpenCode.
const ROUTE_EXAMPLES: &str = "ollama/qwen3-coder:30b or opencode/deepseek-v4.1-flash-free#high";

/// A line for a model name the list doesn't have. It opens holding the model the pointer was
/// on last, so a listed id can be edited into a full version id.
pub fn other_model(picker: &mut Picker, role: Option<RoleName>, cli: &str) {
    let label = format::agent_label(cli);
    picker.title = format!("Other model on {label}");
    picker.title_logo = None;
    picker.about = if ROUTED_CLIS.contains(&cli) {
        format!(
            "Type the model as {label} takes it, provider/model or provider/model#variant, such \
             as {ROUTE_EXAMPLES}"
        )
    } else {
        format!("Type the model's name as {label} takes it, such as a full version id")
    };
    let held = picker.flow.pointed_model.clone().unwrap_or_default();
    picker.set_rows(Vec::new());
    picker.flow.actions.clear();
    picker.input = Some(held);
    picker.enter_hint = flow::save_hint(picker);
    picker.esc_hint = "Esc to go back".to_owned();
    picker.flow.stage = Stage::OtherModel {
        role,
        cli: cli.to_owned(),
    };
}

/// The typed name as a worker on `cli`, with the level a `#variant` names on a CLI that takes
/// one, or `None` with a warning when it can't name one.
pub fn typed(picker: &mut Picker, cli: &str, text: &str) -> Option<(ModelRef, Option<String>)> {
    let name = text.trim();
    if name.is_empty() {
        picker.warning = Some("Type a model name, or Esc to go back.".to_owned());
        return None;
    }
    let (name, level) = if ROUTED_CLIS.contains(&cli) {
        match routed(picker, cli, name) {
            Ok(split) => split,
            Err(why) => {
                picker.warning = Some(why);
                return None;
            }
        }
    } else {
        (name, None)
    };
    match format!("{cli}/{name}").parse::<ModelRef>() {
        Ok(worker) if worker.agent == cli && worker.model.as_deref() == Some(name) => {
            Some((worker, level))
        }
        _ => {
            picker.warning = Some(format!(
                "{name:?} can't name a model; type it without spaces."
            ));
            None
        }
    }
}

/// `provider/model#variant` split into the model and its level. The launch would refuse a
/// name without one `/` with something on each side, and a listed model given a variant it
/// doesn't have. A model the list doesn't have is taken as typed.
fn routed<'a>(
    picker: &Picker,
    cli: &str,
    typed: &'a str,
) -> Result<(&'a str, Option<String>), String> {
    let (route, variant) = match typed.split_once('#') {
        Some((route, variant)) => (route, Some(variant)),
        None => (typed, None),
    };
    let label = format::agent_label(cli);
    let shaped = route
        .split_once('/')
        .is_some_and(|(provider, model)| !provider.is_empty() && !model.is_empty())
        && variant.is_none_or(|variant| !variant.is_empty() && !variant.contains('#'));
    if !shaped {
        return Err(format!(
            "{label} takes a model as provider/model, or provider/model#variant, such as \
             {ROUTE_EXAMPLES}."
        ));
    }
    let Some(variant) = variant else {
        return Ok((route, None));
    };
    let listed =
        picker.flow.models.iter().find(|choice| {
            choice.worker.agent == cli && choice.worker.model.as_deref() == Some(route)
        });
    match listed {
        Some(choice) if !choice.efforts.iter().any(|level| level == variant) => {
            let name = choice.display_name();
            Err(match choice.efforts.as_slice() {
                [] => format!("{name} has no variants; type it without #{variant}."),
                levels => format!("{name} takes {}.", or_list(levels)),
            })
        }
        _ => Ok((route, Some(variant.to_owned()))),
    }
}

/// `low, high or max`.
fn or_list(levels: &[String]) -> String {
    match levels {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// A typed model, with the level its `#variant` named.
pub fn pick_typed(role: Option<RoleName>, worker: ModelRef, level: Option<String>) -> Step {
    pick_with(role, worker, level)
}

fn pick_with(role: Option<RoleName>, worker: ModelRef, effort: Option<String>) -> Step {
    Step::Done(UiCommand::Slash {
        command: SlashCommand::Model {
            role: role.filter(|role| *role != RoleName::Main),
            worker: Some(worker),
            effort,
        },
    })
}

/// A model from the list, with the effort its line shows when you moved it, or when the level
/// set now is one this model doesn't take. A model fixed at one level can't have been moved.
pub fn pick_listed(picker: &Picker, role: Option<RoleName>, worker: ModelRef) -> Step {
    let wanted = picker.flow.effort_wanted.as_deref();
    let effort = listed(&picker.flow, &worker).and_then(|choice| {
        let line = EffortLine::for_choice(choice, wanted);
        let level = line.level()?;
        let moved = wanted.is_some() && !line.is_fixed();
        let changed = moved || choice.effort.as_deref().is_some_and(|set| set != level);
        changed.then(|| level.to_owned())
    });
    pick_with(role, worker, effort)
}

/// The effort line follows the pointer on the models level, carrying over the level you moved
/// it to, and goes away everywhere else.
pub fn follow_pointer(picker: &mut Picker) {
    let flow = &picker.flow;
    let line = match &flow.stage {
        // A setting holds the model alone, so there's no effort to move.
        Stage::ModelsOf { .. } if flow.model_for.is_none() => picker
            .pointed()
            .and_then(|index| flow.actions.get(index))
            .and_then(|action| match action {
                RowAction::Worker { worker, .. } => listed(flow, worker),
                _ => None,
            })
            .map(|choice| EffortLine::for_choice(choice, flow.effort_wanted.as_deref())),
        _ => None,
    };
    if !matches!(
        flow.stage,
        Stage::ModelsOf { .. } | Stage::OtherModel { .. }
    ) {
        picker.flow.effort_wanted = None;
        picker.flow.pointed_model = None;
    }
    let pointed_model = match &picker.flow.stage {
        Stage::ModelsOf { .. } => picker
            .pointed()
            .and_then(|index| picker.flow.actions.get(index))
            .and_then(|action| match action {
                RowAction::Worker { worker, .. } => worker.model.clone(),
                _ => None,
            }),
        _ => None,
    };
    if pointed_model.is_some() {
        picker.flow.pointed_model = pointed_model;
    }
    picker.flow.effort = line;
    picker.note = memory_note(picker);
    let gauge = match (&picker.flow.stage, &picker.banner, picker.flow.memory) {
        (Stage::ModelsOf { .. }, Some(_), Some(machine)) => {
            Some(memory_rows::gauge(pointed_choice(picker), &machine))
        }
        _ => None,
    };
    picker.gauge =
        gauge.map(|gauge| memory_rows::gauge_after(gauge, picker.gauge.as_ref(), Instant::now()));
}

/// The listed choice under the pointer on a models level.
fn pointed_choice(picker: &Picker) -> Option<&ModelChoice> {
    let index = picker.pointed()?;
    match picker.flow.actions.get(index)? {
        RowAction::Worker { worker, .. } => listed(&picker.flow, worker),
        _ => None,
    }
}

/// What goes where the effort line does: the question before a download, or why the model
/// under the pointer won't fit as it is.
fn memory_note(picker: &Picker) -> Option<String> {
    if !matches!(picker.flow.stage, Stage::ModelsOf { .. }) {
        return None;
    }
    let choice = pointed_choice(picker)?;
    if picker.flow.download_asked.as_ref() == Some(&choice.worker) {
        return memory_rows::download_question(choice);
    }
    let machine = picker.flow.memory.as_ref()?;
    let suggested = picker.flow.models.iter().find(|other| {
        other.recommended
            && other.worker.agent == choice.worker.agent
            && other.worker.model.is_some()
    });
    memory_rows::warning(choice, machine, suggested)
}

/// A model listed as not installed, which Enter offers to download.
pub fn not_installed(flow: &Flow, worker: &ModelRef) -> bool {
    listed(flow, worker)
        .and_then(|choice| choice.listed.as_ref()?.memory.as_ref()?.download)
        .is_some()
}

/// Enter on a model not installed asks first; Enter again pulls it with Ollama, its progress
/// in the conversation. The picker stays open, and its tick of the local servers lists the
/// model once it's done.
pub fn download(picker: &mut Picker, worker: ModelRef) -> Step {
    if picker.flow.download_asked.as_ref() != Some(&worker) {
        picker.flow.download_asked = Some(worker);
        return Step::Stay;
    }
    picker.flow.download_asked = None;
    let tag = worker
        .model
        .as_deref()
        .and_then(|route| route.split_once('/'))
        .map(|(_, tag)| tag)
        .filter(|tag| is_plain_tag(tag));
    match tag {
        Some(tag) => Step::Send(UiCommand::Shell {
            command: format!("ollama pull {tag}"),
        }),
        None => Step::Stay,
    }
}

/// A tag the shell reads as one plain word, such as `qwen3.5:27b` or `hf.co/org/model:Q4_K_M`.
fn is_plain_tag(tag: &str) -> bool {
    !tag.is_empty()
        && !tag.starts_with('-')
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/-".contains(c))
}

fn listed<'a>(flow: &'a Flow, worker: &ModelRef) -> Option<&'a ModelChoice> {
    flow.models.iter().find(|choice| choice.worker == *worker)
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

/// Esc one level down goes up one. `false` on the first level, which closes.
pub fn back(picker: &mut Picker) -> bool {
    match picker.flow.stage.clone() {
        Stage::Models { role } if opened_from_roles(&picker.flow) => {
            picker.flow.waiting = None;
            flow::role_list(picker);
            let row = picker
                .flow
                .actions
                .iter()
                .position(|action| *action == RowAction::AskModels(role));
            if let Some(at) = row {
                picker.point_at(at);
            }
            true
        }
        Stage::ModelsOf { .. } if picker.flow.download_asked.is_some() => {
            // Esc keeps looking.
            picker.flow.download_asked = None;
            true
        }
        Stage::ModelsOf { role, cli } => {
            cli_list(picker, role);
            if let Some(at) = picker.flow.actions.iter().position(
                |action| matches!(action, RowAction::OpenCli { cli: open, .. } if *open == cli),
            ) {
                picker.point_at(at);
            }
            true
        }
        Stage::OtherModel { role, cli } => {
            model_list(picker, role, &cli);
            picker.point_at(picker.rows.len().saturating_sub(1));
            true
        }
        _ => false,
    }
}

/// `Kept model as Opus 5.5`, naming the role unless it's the main agent. Before the list
/// comes, the role's settings row says what it runs.
pub fn kept(picker: &Picker, role: Option<RoleName>) -> Kept {
    let name = match role {
        None => "model".to_owned(),
        Some(role) => format!("{}'s model", role.as_str()),
    };
    let flow = &picker.flow;
    let listed = flow
        .models
        .iter()
        .find(|choice| choice.current)
        .map(format::choice_words);
    let set = || {
        let row = flow
            .settings
            .iter()
            .find(|row| model_role(&row.key) == Some(role))?;
        let words = value_words(&row.value);
        if matches!(words.as_str(), "unset" | "none") {
            return None;
        }
        let workers: Vec<String> = words
            .split(", ")
            .map(|worker| worker_words(worker, &flow.models))
            .collect();
        Some(workers.join(", "))
    };
    match listed.or_else(set) {
        Some(worker) => Kept::as_value(&name, worker),
        None => Kept::words(format!("Kept {name} as it was")),
    }
}

/// A worker as a sentence names it: `Opus 5.5`, or `Claude Code's default (Opus 5.5)` when
/// the CLI picks and says what it runs. A worker that isn't one of `choices` gets the name
/// its id gives.
pub fn worker_words(worker: &str, choices: &[ModelChoice]) -> String {
    if let Some(word) = word_for_a_model(worker) {
        return word.to_owned();
    }
    let Ok(worker) = worker.parse::<ModelRef>() else {
        return worker.to_owned();
    };
    match choices.iter().find(|choice| choice.worker == worker) {
        Some(choice) => format::choice_words(choice),
        None => match &worker.model {
            Some(model) => display_name_from_id(model),
            None => format!("{}'s default", format::agent_label(&worker.agent)),
        },
    }
}

fn in_model_command(picker: &Picker) -> bool {
    picker.flow.command == "model"
}

/// Whether `/model` has roles to offer under the main agent's list.
fn has_roles(picker: &Picker) -> bool {
    in_model_command(picker)
        && picker
            .flow
            .settings
            .iter()
            .any(|row| matches!(model_role(&row.key), Some(Some(_))))
}

fn who(role: Option<RoleName>) -> String {
    match role {
        None | Some(RoleName::Main) => "the main agent".to_owned(),
        Some(role) => format!("the {} role", role.as_str()),
    }
}

fn with_freshness(picker: &Picker, about: &str) -> String {
    match level_note(picker).or_else(|| picker.flow.freshness.clone()) {
        Some(note) => format!("{}. {note}", about.trim_end_matches('.')),
        None => about.to_owned(),
    }
}

/// Each CLI or endpoint once, in the order its first model comes, with its models in theirs.
fn groups(choices: &[ModelChoice]) -> Vec<(String, Vec<&ModelChoice>)> {
    let mut groups: Vec<(String, Vec<&ModelChoice>)> = Vec::new();
    for choice in choices {
        let cli = &choice.worker.agent;
        match groups.iter_mut().find(|(id, _)| id == cli) {
            Some((_, models)) => models.push(choice),
            None => groups.push((cli.clone(), vec![choice])),
        }
    }
    groups
}

/// The CLI level's columns: what runs, how it's reached, the plan window's meter, its
/// percentage, when it resets, and whether its list is being asked for, a column of its own so
/// nothing moves when an answer lands.
const CLI_COLUMNS: usize = 6;
const PERCENT: usize = 3;
/// From here a plan window reads amber and says when it resets, as the bottom line does.
const NEAR_LIMIT: f64 = 0.9;

/// A CLI's row: what the role runs here now, else how many models there are, then how they're
/// reached, its plan usage, and the state of its list.
fn cli_cells(
    models: &[&ModelChoice],
    state: Option<String>,
    now: Option<OffsetDateTime>,
    logo: &Logo,
) -> Vec<PickerCell> {
    // The model in effect is named beside the row's logo, as a model's own row names it.
    let model = |name: String| PickerCell::model_beside(name, logo.clone());
    let runs = match models.iter().find(|choice| choice.current) {
        Some(current) => match current.worker.model.as_deref() {
            Some(alias) if current.alias => PickerCell::new(alias),
            Some(_) => model(current.display_name()),
            None => match (current.runs.as_deref(), current.note.as_deref()) {
                (Some(runs), _) => model(format!("{runs} · default")),
                (None, Some(note)) => PickerCell::new(format!("{note} · default")),
                (None, None) => PickerCell::new("default"),
            },
        },
        None => PickerCell::new(match models.iter().filter(|c| runnable(c)).count() {
            0 => String::new(),
            1 => "1 model".to_owned(),
            n => format!("{n} models"),
        }),
    };
    let mut cells = vec![PickerCell::default(); CLI_COLUMNS];
    cells[0] = runs;
    if let Some(first) = models.first() {
        cells[1] = PickerCell::new(
            reach_words(models).unwrap_or_else(|| access_words(first.access).to_owned()),
        );
        if let Some(plan) = first.plan_usage.as_ref() {
            plan_cells(&mut cells, plan, now.unwrap_or(plan.observed_at));
        }
    }
    cells[CLI_COLUMNS - 1] = PickerCell::new(state.unwrap_or_default());
    cells
}

/// A plan window's meter, `5h ▄▄▄▄▄▖`, its percentage, amber near the limit, and then when it
/// resets.
fn plan_cells(cells: &mut [PickerCell], plan: &PlanUsageReading, now: OffsetDateTime) {
    let out = plan.status == PlanUsageStatus::Rejected;
    let Some(used) = plan.used.or(out.then_some(1.0)) else {
        return;
    };
    let window = plan.window.as_deref().map(window_name).unwrap_or_default();
    cells[2] = PickerCell::meter(window, used);
    cells[PERCENT] = if out {
        PickerCell::in_role("out", Role::Danger)
    } else if used >= NEAR_LIMIT {
        PickerCell::in_role(whole_percent(used), Role::Warn)
    } else {
        PickerCell::new(whole_percent(used))
    };
    if out || used >= NEAR_LIMIT {
        cells[4] = plan
            .resets_at
            .map(|at| PickerCell::new(format!("resets {}", reset_time(at, now, ClockStyle::now()))))
            .unwrap_or_default();
    }
}

fn whole_percent(share: f64) -> String {
    format!("{:.0}%", (share * 100.0).clamp(0.0, 100.0))
}

/// `five_hour` reads `5h` and `seven_day` `7d`, as the bottom line names its windows.
fn window_name(window: &str) -> String {
    match window.trim() {
        "five_hour" | "5h" => "5h".to_owned(),
        "seven_day" | "weekly" | "7d" => "7d".to_owned(),
        "daily" | "one_day" => "1d".to_owned(),
        other => other.replace('_', " "),
    }
}

/// A model the CLI can run now: one it names, and not one offered for download.
fn runnable(choice: &ModelChoice) -> bool {
    let download = choice
        .listed
        .as_ref()
        .and_then(|listed| listed.memory.as_ref())
        .and_then(|memory| memory.download);
    choice.worker.model.is_some() && download.is_none()
}

/// How a CLI's models are reached, counted when they're reached more than one way: `7 local ·
/// 10 free`. All of them on this computer read `local`, and all free read `free`; `None` when
/// they're all reached another one way, which its access word says.
fn reach_words(models: &[&ModelChoice]) -> Option<String> {
    let reach = |choice: &ModelChoice| match (choice.access, &choice.cost) {
        (ModelAccess::Local, _) => "local",
        (_, CostEstimate::Free) => "free",
        (access, _) => access_words(access),
    };
    let named: Vec<&str> = models
        .iter()
        .filter(|choice| runnable(choice))
        .map(|choice| reach(choice))
        .collect();
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for word in &named {
        match counts.iter_mut().find(|(known, _)| known == word) {
            Some((_, count)) => *count += 1,
            None => counts.push((word, 1)),
        }
    }
    if let [(word, _)] = counts.as_slice() {
        return matches!(*word, "local" | "free").then(|| (*word).to_owned());
    }
    if counts.len() < 2 {
        return None;
    }
    // On this computer first, then free, then the rest as they come.
    counts.sort_by_key(|(word, _)| match *word {
        "local" => 0,
        "free" => 1,
        _ => 2,
    });
    let words: Vec<String> = counts
        .iter()
        .map(|(word, count)| format!("{count} {word}"))
        .collect();
    Some(words.join(" · "))
}

/// "Default (recommended)", or the model's display name, such as "Opus 5.5".
fn model_label(choice: &ModelChoice) -> String {
    match &choice.worker.model {
        Some(_) => choice.display_name(),
        None if choice.recommended => RECOMMENDED_LABEL.to_owned(),
        None => DEFAULT_LABEL.to_owned(),
    }
}

/// The provider group a listed model belongs to, such as "Ollama" on OpenCode's level.
fn listed_group(choice: &ModelChoice) -> Option<&str> {
    choice.listed.as_ref()?.group.as_deref()
}

/// While the pointer is on a model that can't use tools: it reads and edits no file on its
/// CLI, and where it can still help.
fn no_tools_warning(choice: &ModelChoice) -> Option<String> {
    let listed = choice.listed.as_ref().filter(|listed| listed.no_tools)?;
    let cli = format::agent_label(&choice.worker.agent);
    let runtime = listed
        .group
        .as_deref()
        .filter(|_| listed.hosted_by.is_none());
    let then = match runtime {
        Some(runtime) => format!(" Add {runtime} as an endpoint to use it for reviews."),
        None => String::new(),
    };
    Some(format!(
        "{} can't use tools, so on {cli} it can't read or edit a file.{then}",
        choice.display_name()
    ))
}

/// A listed model in its fields: "no tool use" for one that has none, its window, then where
/// it runs, "on this computer", or what it costs.
fn listed_description(choice: &ModelChoice, listed: &ListedFacts) -> String {
    let mut parts: Vec<String> = Vec::new();
    if choice.new {
        parts.push("new".to_owned());
    }
    if listed.no_tools {
        parts.push("no tool use".to_owned());
    }
    if let Some(window) = listed.window {
        parts.push(format!("{} context", window_words(window)));
    }
    if choice.access == ModelAccess::Local {
        parts.push("on this computer".to_owned());
    } else {
        let cost = model_note(&ModelChoice {
            note: None,
            strengths: Vec::new(),
            ..choice.clone()
        });
        if !cost.is_empty() {
            parts.push(cost);
        }
    }
    parts.join(" · ")
}

/// What the model is for, from the catalog or the CLI, with the model Default runs before it.
/// A model neither knows is marked new. Without a description: what it stands for, its cost,
/// and its strengths, else what kind of model it is, so no row's description is blank.
pub(super) fn model_description(choice: &ModelChoice) -> String {
    if let Some(listed) = choice
        .listed
        .as_ref()
        .filter(|_| choice.worker.model.is_some())
    {
        return listed_description(choice, listed);
    }
    let mut parts: Vec<String> = Vec::new();
    if choice.new {
        parts.push("new".to_owned());
    }
    if choice.worker.model.is_none()
        && let Some(runs) = &choice.runs
    {
        parts.push(runs.clone());
    }
    match &choice.description {
        Some(description) => parts.push(description.clone()),
        None if choice.runs.is_some() => {}
        None => {
            let note = model_note(choice);
            if !note.is_empty() {
                parts.push(note);
            }
        }
    }
    if parts.is_empty() && choice.worker.model.is_some() {
        parts.push(kind_of_model(choice));
    }
    parts.join(" · ")
}

/// `An Anthropic model on your subscription`: what Argonauts Code knows of a model nothing
/// describes. Never its id, which only repeats the name beside it.
fn kind_of_model(choice: &ModelChoice) -> String {
    let provider = Some(choice.provider.trim())
        .filter(|p| is_known(p))
        .map(provider_name);
    let what = match &provider {
        Some(provider) if provider.starts_with(['A', 'E', 'I', 'O', 'U']) => {
            format!("An {provider} model")
        }
        Some(provider) => format!("A {provider} model"),
        None => "A model".to_owned(),
    };
    let how = match choice.access {
        ModelAccess::Subscription => "on your subscription",
        ModelAccess::ApiKey => "on your API key",
        ModelAccess::Local => "on this computer",
    };
    format!("{what} {how}")
}

/// `Aliases: opus, sonnet (in use), haiku`.
fn alias_line(aliases: &[ModelChoice]) -> Option<String> {
    let names: Vec<String> = aliases
        .iter()
        .filter_map(|choice| {
            let name = choice.worker.model.as_deref()?;
            Some(if choice.current {
                format!("{name} (in use)")
            } else {
                name.to_owned()
            })
        })
        .collect();
    (!names.is_empty()).then(|| format!("Aliases: {}", names.join(", ")))
}

/// What the model stands for or runs, then its cost where it's in dollars, then its strengths.
fn model_note(choice: &ModelChoice) -> String {
    let mut parts: Vec<String> = choice.note.iter().cloned().collect();
    match choice.cost {
        CostEstimate::Usd(usd) => parts.push(format!("~{} a task", format::usd(usd))),
        CostEstimate::PlanUsage(share) => {
            parts.push(format!("~{} of plan usage a task", format::percent(share)))
        }
        CostEstimate::Free => parts.push("free".to_owned()),
        CostEstimate::Unknown => {}
    }
    if !choice.strengths.is_empty() {
        parts.push(choice.strengths.join(", "));
    }
    parts.join(" · ")
}

fn access_words(access: ModelAccess) -> &'static str {
    match access {
        ModelAccess::Subscription => "subscription",
        ModelAccess::ApiKey => "API key",
        ModelAccess::Local => "local",
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::limits::CostEstimate;

    use super::*;

    fn opencode_model(route: &str, name: &str, levels: &[&str]) -> ModelChoice {
        ModelChoice {
            worker: format!("opencode/{route}").parse().unwrap(),
            provider: String::new(),
            access: ModelAccess::Subscription,
            cost: CostEstimate::Free,
            plan_usage: None,
            efforts: levels.iter().map(|&level| level.to_owned()).collect(),
            strengths: Vec::new(),
            current: false,
            note: None,
            older: false,
            name: name.to_owned(),
            description: None,
            recommended: false,
            new: false,
            default_effort: None,
            effort: None,
            alias: false,
            runs: None,
            listed: None,
        }
    }

    fn picker() -> Picker {
        let mut picker = Picker::default();
        picker.flow.models = vec![
            opencode_model(
                "opencode/deepseek-v4.1-flash-free",
                "DeepSeek V4.1 Flash Free",
                &["low", "high", "max"],
            ),
            opencode_model("ollama/qwen3-coder:30b", "Qwen3 Coder 30B", &[]),
        ];
        picker
    }

    #[test]
    fn a_variant_after_the_model_is_its_level() {
        let mut picker = picker();
        let (worker, level) = typed(
            &mut picker,
            "opencode",
            "opencode/deepseek-v4.1-flash-free#high",
        )
        .unwrap();
        assert_eq!(
            worker.to_string(),
            "opencode/opencode/deepseek-v4.1-flash-free"
        );
        assert_eq!(level.as_deref(), Some("high"));
        let (worker, level) = typed(&mut picker, "opencode", "ollama/qwen3-coder:30b").unwrap();
        assert_eq!(worker.model.as_deref(), Some("ollama/qwen3-coder:30b"));
        assert_eq!(level, None);
    }

    #[test]
    fn a_name_opencode_would_refuse_gets_the_examples() {
        let mut picker = picker();
        for refused in ["qwen3-coder:30b", "/qwen", "ollama/", "ollama/qwen#"] {
            assert!(
                typed(&mut picker, "opencode", refused).is_none(),
                "{refused}"
            );
            let warning = picker.warning.take().unwrap();
            assert!(
                warning.contains("provider/model#variant") && warning.contains(ROUTE_EXAMPLES),
                "{warning}"
            );
        }
    }

    #[test]
    fn a_listed_model_given_a_variant_it_lacks_names_the_ones_it_has() {
        let mut picker = picker();
        assert!(
            typed(
                &mut picker,
                "opencode",
                "opencode/deepseek-v4.1-flash-free#xhigh"
            )
            .is_none()
        );
        assert_eq!(
            picker.warning.take().as_deref(),
            Some("DeepSeek V4.1 Flash Free takes low, high or max.")
        );
        assert!(typed(&mut picker, "opencode", "ollama/qwen3-coder:30b#high").is_none());
        assert_eq!(
            picker.warning.take().as_deref(),
            Some("Qwen3 Coder 30B has no variants; type it without #high.")
        );
        // One OpenCode doesn't list is taken as typed.
        let (_, level) = typed(&mut picker, "opencode", "openrouter/z-ai/glm-5#max").unwrap();
        assert_eq!(level.as_deref(), Some("max"));
    }

    #[test]
    fn other_model_opens_holding_the_model_the_pointer_was_on() {
        let mut picker = picker();
        picker.flow.pointed_model = Some("ollama/qwen3-coder:30b".to_owned());
        other_model(&mut picker, None, "opencode");
        assert_eq!(picker.input.as_deref(), Some("ollama/qwen3-coder:30b"));
        assert!(picker.about.contains(ROUTE_EXAMPLES), "{}", picker.about);
    }

    #[test]
    fn only_a_plain_tag_is_pulled() {
        for tag in ["qwen3.5:27b", "llama3.2", "hf.co/org/model:Q4_K_M"] {
            assert!(is_plain_tag(tag), "{tag}");
        }
        for tag in ["", "-h", "qwen; rm -rf ~", "a b", "$(x)", "q`x`"] {
            assert!(!is_plain_tag(tag), "{tag}");
        }
    }
}
