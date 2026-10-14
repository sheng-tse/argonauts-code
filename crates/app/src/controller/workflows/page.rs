//! The page `/workflow edit <name>` opens for a file of steps: the steps in order, each with
//! its keys. A change made there is checked, written into the file's text with the comments
//! kept, and answered with the page as it reads now.

use std::path::Path;

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::log::NoteLevel;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::workflow_page::{
    PageCheck, PageField, PageGroup, WorkflowChange, WorkflowPage,
};
use argonauts_runner::{Change, DryRun, FILE_FIELDS, STEP_FIELDS, WorkflowFile, dry_run};
use toml::{Table, Value};

use super::{State, editor, library, role_models, who_would_take};
use crate::controller::{Ctx, Effect};

/// The extension of a file of steps. A script has no keys to list, so it has no page.
pub(super) fn has_page(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("toml")
}

/// Sends the page as the file reads now. `refresh` is for the page already open.
pub(super) fn send(ctx: &Ctx, name: &str, path: &Path, said: Option<String>, refresh: bool) {
    let mut page = build(ctx, name, path);
    page.said = said;
    page.refresh = refresh;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::WorkflowPage {
            page: Box::new(page),
        }),
    });
}

/// One change from the page. The file is written only when its steps still run afterwards,
/// and the page comes back either way, saying which.
pub(super) fn change(
    ctx: &Ctx,
    state: &mut State,
    name: &str,
    change: WorkflowChange,
) -> anyhow::Result<Vec<Effect>> {
    change_with(ctx, state, name, change, editor(ctx))
}

/// [`change`] with the editor given, as `e` on the page would find it.
pub(super) fn change_with(
    ctx: &Ctx,
    state: &mut State,
    name: &str,
    change: WorkflowChange,
    editor: Option<Vec<String>>,
) -> anyhow::Result<Vec<Effect>> {
    let Some((_, Some(path))) = library(ctx).find(name) else {
        ctx.notice(
            NoteLevel::Warn,
            format!("There's no workflow file named {name} any more."),
        );
        return Ok(Vec::new());
    };
    let change = match change {
        WorkflowChange::OpenInEditor => {
            open_in_editor(ctx, name, &path, editor);
            return Ok(Vec::new());
        }
        WorkflowChange::Set { step, key, value } => {
            let value = Some(value).filter(|value| !value.trim().is_empty());
            match step {
                Some(step) => Change::Step {
                    step: step as usize,
                    key,
                    value,
                },
                None => Change::File { key, value },
            }
        }
        WorkflowChange::AddStep { after } => Change::Add {
            after: after.map(|after| after as usize),
        },
        WorkflowChange::RemoveStep { step } => Change::Remove {
            step: step as usize,
        },
        WorkflowChange::MoveStep { step, up } => Change::Move {
            step: step as usize,
            up,
        },
    };
    let said = match write(state, &path, &change) {
        Ok(()) => "Saved.".to_owned(),
        Err(why) => format!("Not saved: {why}"),
    };
    send(ctx, name, &path, Some(said), true);
    // The `/` menu shows the workflow's one line, which may be what changed.
    Ok(vec![Effect::CommandsChanged])
}

fn write(state: &mut State, path: &Path, change: &Change) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("can't read it: {err}"))?;
    let changed = argonauts_runner::change(&text, change).map_err(|err| err.to_string())?;
    std::fs::write(path, &changed).map_err(|err| format!("can't write it: {err}"))?;
    state.wrote = Some(changed);
    Ok(())
}

fn open_in_editor(ctx: &Ctx, name: &str, path: &Path, editor: Option<Vec<String>>) {
    match editor {
        Some(mut argv) => {
            argv.push(path.display().to_string());
            ctx.send(AppEvent::RunInTerminal {
                argv,
                cwd: ctx.env.repo_root.clone(),
            });
        }
        None => {
            let said = "Set $VISUAL or $EDITOR and e opens the file from here. A save from \
                        any editor is checked and shown on this page.";
            send(ctx, name, path, Some(said.to_owned()), true);
        }
    }
}

fn build(ctx: &Ctx, name: &str, path: &Path) -> WorkflowPage {
    let text = std::fs::read_to_string(path);
    let table: Option<Table> = text.as_ref().ok().and_then(|text| text.parse().ok());
    let parsed = match &text {
        Ok(text) => WorkflowFile::parse(text).map_err(|err| err.to_string()),
        Err(err) => Err(format!("can't read the file: {err}")),
    };
    let dry = match &parsed {
        Ok(Some(file)) => Some(dry_run(file, &|role| role_models(ctx, role))),
        _ => None,
    };
    let check = match (&parsed, &dry) {
        (Ok(_), Some(dry)) => PageCheck {
            valid: true,
            lines: vec![format!("the file is valid · {}", dry.estimate.words())],
        },
        (Ok(_), None) => PageCheck {
            valid: false,
            lines: vec!["a workflow needs at least one [[step]]. a adds one".to_owned()],
        },
        (Err(why), _) => PageCheck {
            valid: false,
            lines: why.lines().map(str::to_owned).collect(),
        },
    };
    let table = table.unwrap_or_default();
    let mut groups = vec![file_group(&table)];
    groups.extend(step_groups(ctx, &table, dry.as_ref()));
    WorkflowPage {
        name: name.to_owned(),
        path: ctx.env.shown(path),
        about: written(&table, "about"),
        check,
        groups,
        said: None,
        refresh: false,
    }
}

/// The file's own keys.
fn file_group(table: &Table) -> PageGroup {
    let adapt = table.get("adapt").and_then(Value::as_bool).unwrap_or(false);
    let steps = if adapt {
        "the orchestrator writes the steps and keeps these roles"
    } else {
        "the steps run exactly as written"
    };
    let goal = match written(table, "goal") {
        goal if goal.is_empty() => "no goal".to_owned(),
        goal => format!("round again until {goal}"),
    };
    PageGroup {
        step: None,
        title: "Workflow".to_owned(),
        role: String::new(),
        who: String::new(),
        how: format!("{steps} · {goal}"),
        fields: FILE_FIELDS
            .iter()
            .map(|key| field(key, table, &[], true))
            .collect(),
    }
}

/// One group per `[[step]]` as written, so a file that doesn't check yet can be mended here.
fn step_groups(ctx: &Ctx, table: &Table, dry: Option<&DryRun>) -> Vec<PageGroup> {
    let Some(Value::Array(steps)) = table.get("step") else {
        return Vec::new();
    };
    let empty = Table::new();
    let steps: Vec<&Table> = steps
        .iter()
        .map(|step| step.as_table().unwrap_or(&empty))
        .collect();
    let names: Vec<String> = steps.iter().map(|step| written(step, "name")).collect();
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let others: Vec<&str> = names
                .iter()
                .enumerate()
                .filter(|(other, name)| *other != index && !name.is_empty())
                .map(|(_, name)| name.as_str())
                .collect();
            let row = dry.and_then(|dry| dry.rows.get(index));
            PageGroup {
                step: u32::try_from(index).ok(),
                title: names[index].clone(),
                role: written(step, "role"),
                who: row.map(|row| who_would_take(ctx, row)).unwrap_or_default(),
                how: row.map(|row| row.how.clone()).unwrap_or_default(),
                fields: STEP_FIELDS
                    .iter()
                    .map(|key| field(key, step, &others, false))
                    .collect(),
            }
        })
        .collect()
}

/// A key as the page lists it: its label, the value written, and the values it takes when
/// they are few. `others` are the names of the other steps.
fn field(key: &str, table: &Table, others: &[&str], of_file: bool) -> PageField {
    const FLAG: &[&str] = &["false", "true"];
    let roles: Vec<&str> = CrewRole::ALL
        .iter()
        .filter(|role| **role != CrewRole::Main)
        .chain([&CrewRole::Main])
        .map(|role| role.as_str())
        .collect();
    let (label, choices, optional, about): (&str, &[&str], bool, &str) = match key {
        "about" => (
            "About",
            &[],
            false,
            "One line on what the workflow is for, shown in the / menu.",
        ),
        "adapt" => (
            "Adapt",
            FLAG,
            false,
            "true: the orchestrator writes the steps for each request and keeps these roles. \
             false: the steps run exactly as written.",
        ),
        "goal" => (
            "Goal",
            &[],
            true,
            "Go round the whole workflow until this holds.",
        ),
        "max_rounds" if of_file => ("Max rounds", &[], true, "The most rounds the goal gets."),
        "name" => (
            "Name",
            &[],
            false,
            "What the step is called. Other steps point at it by this name.",
        ),
        "role" => (
            "Role",
            &roles,
            false,
            "Who takes it. The [crew] settings say which model fills each role.",
        ),
        "does" => (
            "Does",
            &[],
            true,
            "What its agents are asked. {request} is your request and {task} the task's title.",
        ),
        "for_each" => (
            "For each",
            &["task"],
            true,
            "task: one agent per task, side by side, each writer in its own worktree.",
        ),
        "count" => (
            "Count",
            &[],
            true,
            "How many agents: a number, or all for every model the role lists.",
        ),
        "independent" => (
            "Independent",
            FLAG,
            false,
            "Its agents don't see what the steps before them made.",
        ),
        "not_the_author" => (
            "Not the author",
            FLAG,
            false,
            "Each task goes to an agent that didn't build it.",
        ),
        "send_back_to" => (
            "Sends back to",
            others,
            true,
            "The step the work returns to when this one doesn't accept it.",
        ),
        "then" => (
            "Then",
            others,
            true,
            "The step that runs next, in place of the one below.",
        ),
        "until" => (
            "Until",
            &[],
            true,
            "A goal on this step: its agent keeps going until this is true.",
        ),
        "kept_by" => (
            "Kept by",
            &["cli", "argonauts"],
            true,
            "Who keeps an until: the CLI's own goal where it has one, or Argonauts Code.",
        ),
        "max_rounds" => (
            "Max rounds",
            &[],
            true,
            "The most times the step runs for one task. Without it only acceptance ends a loop.",
        ),
        "model" => (
            "Model",
            &[],
            true,
            "A model of its own, such as codex/gpt-5.6, in place of the role's.",
        ),
        "own_workflow" => (
            "Own workflow",
            FLAG,
            false,
            "Its workers may each run a workflow of their own, where their CLI has one.",
        ),
        other => (other, &[], true, ""),
    };
    let mut value = written(table, key);
    // A flag nobody wrote is false.
    if value.is_empty() && choices == FLAG {
        "false".clone_into(&mut value);
    }
    PageField {
        key: key.to_owned(),
        label: label.to_owned(),
        value,
        choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
        optional,
        about: about.to_owned(),
    }
}

/// A key's value as you would type it: text without its quotes. Empty when it isn't set.
fn written(table: &Table, key: &str) -> String {
    match table.get(key) {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}
