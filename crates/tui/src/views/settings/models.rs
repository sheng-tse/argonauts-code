//! `/model`'s two levels. First the CLIs and endpoints a role may use, each with what it runs
//! and its plan usage, and for the main agent a last row, "Roles…", that opens the other roles.
//! Enter opens one, read like Claude Code's: Default with the model it runs, the models by name
//! with what each is for, newest first, the aliases on one line under Default, and
//! "Other model…" to type any name. Under the list, ←/→ set the highlighted model's effort, and
//! Enter saves both. Esc goes back a level, and closes from the first.

use argonauts_core::app::{ModelChoice, UiCommand};
use argonauts_core::input::SlashCommand;
use argonauts_core::limits::{CostEstimate, PlanUsageStatus};
use argonauts_core::model_catalog::display_name_from_id;
use argonauts_core::model_picker::{EffortLine, ModelsLevel};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::registry::ModelAccess;
use argonauts_core::registry::provider::{is_known, provider_name};

use super::flow::{self, Flow, Kept, RowAction, Stage, Step};
use super::words::{model_role, value_words, word_for_a_model};
use crate::bottom::{Picker, PickerRow};
use crate::format::{self, setting_value};

const DEFAULT_LABEL: &str = "Default";
const RECOMMENDED_LABEL: &str = "Default (recommended)";
const OTHER_LABEL: &str = "Other model…";
const ROLES_LABEL: &str = "Roles…";

/// The first level, from the controller's answer. Opened for a setting, the model that setting
/// holds is the one marked current, not the role's.
pub fn fill(picker: &mut Picker, role: Option<RoleName>, choices: &[ModelChoice]) {
    picker.flow.waiting = None;
    picker.flow.models = choices.to_vec();
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

/// A newer list for the open picker, at whichever level it shows, keeping the pointer on the
/// same row. A name being typed stays as it is.
pub fn refresh(picker: &mut Picker, choices: &[ModelChoice]) {
    picker.flow.models = choices.to_vec();
    let pointed = picker
        .pointed()
        .and_then(|index| picker.rows.get(index))
        .map(|row| row.label.clone());
    match picker.flow.stage.clone() {
        Stage::Models { role } => cli_list(picker, role),
        Stage::ModelsOf { role, cli } => model_list(picker, role, &cli),
        _ => return,
    }
    if let Some(at) =
        pointed.and_then(|label| picker.rows.iter().position(|row| row.label == label))
    {
        picker.point_at(at);
    }
    follow_pointer(picker);
}

fn cli_list(picker: &mut Picker, role: Option<RoleName>) {
    picker.flow.stage = Stage::Models { role };
    // Opened for a setting, the picker keeps that setting's name.
    let for_setting = picker.flow.model_for.is_some();
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
    let mut rows: Vec<PickerRow> = groups
        .iter()
        .map(|(cli, models)| PickerRow {
            current: models.iter().any(|choice| choice.current),
            ..PickerRow::new(format::agent_label(cli), cli_note(models))
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
        rows.push(PickerRow::new(ROLES_LABEL, "each role's own model"));
        actions.push(RowAction::OpenRoles);
    }
    picker.set_rows(rows);
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
    let about = if picker.flow.model_for.is_some() {
        format!("The models {label} runs")
    } else {
        format!("The model {} runs on {label}", who(role))
    };
    picker.about = with_freshness(picker, &about);
    picker.placeholder = "No models listed".to_owned();
    picker.enter_hint.clear();
    picker.esc_hint = "Esc to go back".to_owned();
    let level = ModelsLevel::new(cli, &picker.flow.models);
    // Older models come last with no heading of their own, as Claude Code lists them.
    let mut rows: Vec<PickerRow> = level
        .rows()
        .map(|choice| PickerRow {
            current: choice.current,
            ..PickerRow::new(model_label(choice), model_description(choice))
        })
        .collect();
    rows.push(PickerRow::new(OTHER_LABEL, "type any model name it takes"));
    // The aliases hang under Default, or under "Other model…", where you'd type one.
    if let Some(aliases) = alias_line(&level.aliases) {
        let under = if level.default.is_some() {
            0
        } else {
            rows.len() - 1
        };
        rows[under].under = Some(aliases);
    }
    picker.set_rows(rows);
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

/// A line for a model name the list doesn't have.
pub fn other_model(picker: &mut Picker, role: Option<RoleName>, cli: &str) {
    let label = format::agent_label(cli);
    picker.title = format!("Other model on {label}");
    picker.about = format!("Type the model's name as {label} takes it, such as a full version id");
    picker.set_rows(Vec::new());
    picker.flow.actions.clear();
    picker.input = Some(String::new());
    picker.enter_hint = "Enter to use it".to_owned();
    picker.esc_hint = "Esc to go back".to_owned();
    picker.flow.stage = Stage::OtherModel {
        role,
        cli: cli.to_owned(),
    };
}

/// The typed name as a worker on `cli`, or `None` with a warning when it can't name one.
pub fn typed(picker: &mut Picker, cli: &str, text: &str) -> Option<ModelRef> {
    let name = text.trim();
    if name.is_empty() {
        picker.warning = Some("Type a model name, or Esc to go back.".to_owned());
        return None;
    }
    match format!("{cli}/{name}").parse::<ModelRef>() {
        Ok(worker) if worker.agent == cli && worker.model.as_deref() == Some(name) => Some(worker),
        _ => {
            picker.warning = Some(format!(
                "{name:?} can't name a model; type it without spaces."
            ));
            None
        }
    }
}

pub fn pick(role: Option<RoleName>, worker: ModelRef) -> Step {
    pick_with(role, worker, None)
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
            .map(|choice| EffortLine::for_choice(choice, flow.effort_wanted.as_deref()))
            .filter(|line| !line.levels.is_empty()),
        _ => None,
    };
    if !matches!(
        flow.stage,
        Stage::ModelsOf { .. } | Stage::OtherModel { .. }
    ) {
        picker.flow.effort_wanted = None;
    }
    picker.flow.effort = line;
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
    match picker.flow.freshness.as_deref() {
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

/// What the role runs here now, else how many models there are, then access and plan usage.
fn cli_note(models: &[&ModelChoice]) -> String {
    let mut parts = Vec::new();
    match models.iter().find(|choice| choice.current) {
        Some(current) => parts.push(match current.worker.model.as_deref() {
            Some(alias) if current.alias => alias.to_owned(),
            Some(_) => current.display_name(),
            None => match (current.runs.as_deref(), current.note.as_deref()) {
                (Some(runs), _) => format!("default, {runs}"),
                (None, Some(note)) => format!("default, {note}"),
                (None, None) => "default".to_owned(),
            },
        }),
        None => {
            let count = models.iter().filter(|c| c.worker.model.is_some()).count();
            match count {
                0 => {}
                1 => parts.push("1 model".to_owned()),
                n => parts.push(format!("{n} models")),
            }
        }
    }
    if let Some(first) = models.first() {
        parts.push(access_words(first.access).to_owned());
        if let Some(plan_usage) = plan_usage_words(first) {
            parts.push(plan_usage);
        }
    }
    parts.join(" · ")
}

/// "Default (recommended)", or the model's display name, such as "Opus 5.5".
fn model_label(choice: &ModelChoice) -> String {
    match &choice.worker.model {
        Some(_) => choice.display_name(),
        None if choice.recommended => RECOMMENDED_LABEL.to_owned(),
        None => DEFAULT_LABEL.to_owned(),
    }
}

/// What the model is for, from the catalog or the CLI, with the model Default runs before it.
/// A model neither knows is marked new. Without a description: what it stands for, its cost,
/// and its strengths, else what kind of model it is, so no row's description is blank.
pub(super) fn model_description(choice: &ModelChoice) -> String {
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
        ModelAccess::Local => "on this machine",
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

fn plan_usage_words(choice: &ModelChoice) -> Option<String> {
    let plan_usage = choice.plan_usage.as_ref()?;
    match (plan_usage.status, plan_usage.used) {
        (PlanUsageStatus::Rejected, _) => Some("out of plan usage".to_owned()),
        // Plan usage in the one wording every view has: `71% used`.
        (_, Some(used)) => Some(format::plan_usage(
            used,
            format::PlanUsageWords::Used,
            false,
        )),
        _ => None,
    }
}
