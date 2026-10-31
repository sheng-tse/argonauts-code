//! The workflow layer. A workflow is looked up beside the settings files: the project's
//! `.argonauts/workflows/<name>.toml`, then `~/.argonauts/workflows/<name>.toml`, then the
//! built-in, so a file always wins and deleting it brings the built-in back.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::settings::SettingScope;
use argonauts_core::settings::workflows::WORKFLOW_FILE_KEYS;
use toml::{Table, Value};

use super::fallback::drop_entry_billing;
use super::passing_over::Stop;
use crate::settings::catalog::setting_meta;
use crate::settings::resolve::{check_plain_keys, is_safety_key, key_matches, leaves};
use crate::workflows::{
    PHASES_EXTENSIONS, SCRIPT_EXTENSIONS, builtin, is_workflow_name, not_a_workflow_name,
    workflow_choices, workflow_choices_in,
};
use crate::{SettingsError, SettingsSources};

/// Whole tables about your machine and accounts, on top of the keys the catalog saves just for
/// you or everywhere.
const YOURS: [&str; 2] = ["agents.*", "endpoints.*"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A file in the repo's workflows folder, which anyone who can push to the repo can change.
    Repo,
    /// A file in your own workflows folder.
    Home,
    BuiltIn,
}

pub(crate) struct WorkflowLayer {
    pub name: String,
    /// `built-in`, or the file it came from.
    pub source: String,
    /// The keys it sets.
    pub table: Table,
}

impl WorkflowLayer {
    /// The file, or which built-in, for `/config` and for errors.
    pub fn label(&self) -> String {
        if self.source == "built-in" {
            format!("the built-in {} workflow", self.name)
        } else {
            self.source.clone()
        }
    }

    /// The workflow by name, and its file when it has one, for a sentence.
    pub fn named(&self) -> String {
        if self.source == "built-in" {
            self.label()
        } else {
            format!("the {} workflow in {}", self.name, self.source)
        }
    }
}

/// Finds the workflow `name` for these sources. `set_in` is where `workflow` was set, and
/// `in_a_file` says a settings file set it, so a mistake there falls back to the default with a
/// notice. A workflow that sets a safety key, or a key that isn't a plain name, is refused,
/// naming the key and the file. A fallback entry's `allow_api_billing` counts only from your own
/// layers, and a workflow from the repo can't set the keys that are yours, so both are dropped
/// here with a warning, which comes back beside the layer.
pub(crate) fn workflow_layer(
    name: &str,
    set_in: &str,
    in_a_file: bool,
    sources: &SettingsSources,
) -> Result<(WorkflowLayer, Vec<String>), Stop> {
    let stop = |error: SettingsError| Stop::at("workflow", in_a_file, error);
    let Some((origin, source, text)) = find(name, set_in, sources).map_err(stop)? else {
        // Such as a mode an older file names, which a later build renamed.
        let notice = format!(
            "workflow = {name:?} in {set_in} names no workflow, so the default is used; \
             /workflow lists the ones you can pick"
        );
        return Err(stop(no_such_workflow(name, set_in, sources)).noticed(notice));
    };
    layer_of(name, origin, source, text).map_err(stop)
}

/// The settings a workflow's file sets, less the ones it may not set.
fn layer_of(
    name: &str,
    origin: Origin,
    source: String,
    text: String,
) -> Result<(WorkflowLayer, Vec<String>), SettingsError> {
    let mut layer = WorkflowLayer {
        name: name.to_owned(),
        source,
        table: Table::new(),
    };
    layer.table = text
        .parse::<Table>()
        .map_err(|err| SettingsError::Invalid {
            layer: layer.label(),
            message: err.to_string(),
        })?;
    // Its name, its steps and its phases describe the workflow, and aren't settings.
    for key in WORKFLOW_FILE_KEYS {
        layer.table.remove(key);
    }
    check_plain_keys(&layer.table, &layer.label())?;
    if let Some((key, _)) = leaves(&layer.table)
        .into_iter()
        .find(|(key, _)| is_safety_key(key))
    {
        return Err(SettingsError::Refused(format!(
            "{key} is a safety setting, so no workflow may set it, and {} does; set it in your \
             own config instead",
            layer.label()
        )));
    }
    let mut warnings = Vec::new();
    if origin == Origin::Repo {
        warnings.extend(repo_warnings(&mut layer));
    }
    if layer.table.remove("workflow").is_some() {
        warnings.push(format!(
            "workflow in {} is ignored: a workflow can't pick another workflow",
            layer.label()
        ));
    }
    for key in drop_entry_billing(&mut layer.table) {
        warnings.push(format!(
            "{key} in {} is ignored: only your own settings let a fallback bill an API without \
             asking",
            layer.label()
        ));
    }
    Ok((layer, warnings))
}

/// A workflow from the repo sits above your own config.local.toml, so the keys that are yours
/// are dropped from it with a warning. Replacing a built-in workflow gets a warning too, since
/// it changes the protocol for everyone who clones the repo.
fn repo_warnings(layer: &mut WorkflowLayer) -> Vec<String> {
    let mut warnings = Vec::new();
    if builtin(&layer.name).is_some() {
        warnings.push(format!(
            "the {} workflow comes from {}, which replaces the built-in one; delete the file to \
             get the built-in back",
            layer.name, layer.source
        ));
    }
    let mut dropped = Vec::new();
    drop_yours(&mut layer.table, "", &mut dropped);
    for key in dropped {
        warnings.push(format!(
            "{key} in {} is ignored: a workflow from the repo can't change your own choices; the \
             team can set it in .argonauts/config.toml, where your config.local.toml still wins",
            layer.source
        ));
    }
    warnings
}

/// Removes every leaf under `table` that is yours to set, and every table it leaves empty.
fn drop_yours(table: &mut Table, prefix: &str, dropped: &mut Vec<String>) {
    table.retain(|key, value| {
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Table(inner) if !inner.is_empty() => {
                drop_yours(inner, &path, dropped);
                !inner.is_empty()
            }
            _ if is_yours(&path) => {
                dropped.push(path);
                false
            }
            _ => true,
        }
    });
}

fn is_yours(key: &str) -> bool {
    YOURS.iter().any(|pattern| key_matches(pattern, key))
        || setting_meta(key).is_some_and(|meta| {
            matches!(
                meta.default_scope,
                SettingScope::ProjectLocal | SettingScope::Global
            )
        })
}

/// The workflows folders beside the project files and beside the global file, in lookup order.
fn workflow_file_dirs(sources: &SettingsSources) -> (Option<PathBuf>, Option<PathBuf>) {
    let beside = |file: Option<&PathBuf>| Some(file?.parent()?.join("workflows"));
    let project = sources.project.as_ref().or(sources.project_local.as_ref());
    (beside(project), beside(sources.global.as_ref()))
}

/// Returns where the workflow came from and its text, or `None` when nothing has its name.
fn find(
    name: &str,
    set_in: &str,
    sources: &SettingsSources,
) -> Result<Option<(Origin, String, String)>, SettingsError> {
    if !is_workflow_name(name) {
        return Err(SettingsError::Invalid {
            layer: set_in.to_owned(),
            message: not_a_workflow_name(name),
        });
    }
    let (project, global) = workflow_file_dirs(sources);
    let dirs = [(Origin::Repo, &project), (Origin::Home, &global)];
    for (origin, dir) in dirs {
        let Some(dir) = dir else { continue };
        let file = dir.join(format!("{name}.toml"));
        if !sources.waiting.contains(&file)
            && let Some((source, text)) = read_workflow(&file)?
        {
            return Ok(Some((origin, source, text)));
        }
        // A workflow written as a script carries no settings of its own.
        let script = SCRIPT_EXTENSIONS
            .iter()
            .map(|extension| dir.join(format!("{name}.{extension}")))
            .find(|path| path.is_file() && !sources.waiting.contains(path));
        if let Some(script) = script {
            return Ok(Some((origin, script.display().to_string(), String::new())));
        }
    }
    if let Some(workflow) = builtin(name) {
        return Ok(Some((
            Origin::BuiltIn,
            "built-in".to_owned(),
            workflow.text.to_owned(),
        )));
    }
    // Nor does one that is its phases alone. It comes last, so a file of phases named after
    // another workflow changes that workflow's phases and leaves its settings in place.
    for (origin, dir) in dirs {
        let Some(dir) = dir else { continue };
        let phases = PHASES_EXTENSIONS
            .iter()
            .map(|extension| dir.join(format!("{name}.{extension}")))
            .find(|path| path.is_file() && !sources.waiting.contains(path));
        if let Some(phases) = phases {
            return Ok(Some((origin, phases.display().to_string(), String::new())));
        }
    }
    Ok(None)
}

/// The error when no workflow has the name, listing every one you can pick.
fn no_such_workflow(name: &str, set_in: &str, sources: &SettingsSources) -> SettingsError {
    let (project, global) = workflow_file_dirs(sources);
    let known = match (&global, &project) {
        (Some(global), project) => workflow_choices_in(global, project.as_deref()),
        (None, Some(project)) => workflow_choices_in(project, None),
        (None, None) => workflow_choices(),
    };
    let names: Vec<String> = known.into_iter().map(|workflow| workflow.name).collect();
    SettingsError::Invalid {
        layer: set_in.to_owned(),
        message: format!(
            "workflow {name:?} isn't a built-in workflow or a file in a workflows folder; pick one of \
             {}",
            names.join(", ")
        ),
    }
}

fn read_workflow(path: &Path) -> Result<Option<(String, String)>, SettingsError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some((path.display().to_string(), text))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(SettingsError::Io {
            path: path.to_owned(),
            message: err.to_string(),
        }),
    }
}
