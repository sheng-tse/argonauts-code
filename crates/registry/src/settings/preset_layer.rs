//! The preset layer. A preset is looked up beside the settings files: the project's
//! `.argonauts/presets/<name>.toml`, then `~/.argonauts/presets/<name>.toml`, then the
//! built-in, so a file always wins and deleting it brings the built-in back.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::settings::SettingScope;
use toml::{Table, Value};

use super::handoff::drop_entry_billing;
use crate::catalog::setting_meta;
use crate::presets::{builtin, is_preset_name, not_a_preset_name, presets, presets_in};
use crate::resolve::{check_plain_keys, is_safety_key, key_matches, leaves};
use crate::{SettingsError, SettingsSources};

/// Whole tables about your machine and accounts, on top of the keys the catalog saves just for
/// you or everywhere.
const YOURS: [&str; 2] = ["agents.*", "endpoints.*"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A file in the repo's presets folder, which anyone who can push to the repo can change.
    Repo,
    /// A file in your own presets folder.
    Home,
    BuiltIn,
}

pub(crate) struct PresetLayer {
    pub name: String,
    /// `built-in`, or the file it came from.
    pub source: String,
    /// The keys it sets.
    pub table: Table,
}

impl PresetLayer {
    /// The file, or which built-in, for `/config` and for errors.
    pub fn label(&self) -> String {
        if self.source == "built-in" {
            format!("the built-in {} preset", self.name)
        } else {
            self.source.clone()
        }
    }

    /// The preset by name, and its file when it has one, for a sentence.
    pub fn named(&self) -> String {
        if self.source == "built-in" {
            self.label()
        } else {
            format!("the {} preset in {}", self.name, self.source)
        }
    }
}

/// Finds the preset `name` for these sources. `set_in` is where `preset` was set, for the
/// error when no preset has that name. A preset that sets a safety key, or a key that isn't a
/// plain name, is refused, naming the key and the file. A fallback entry's `allow_api_billing`
/// counts only from your own layers, and a preset from the repo can't set the keys that are
/// yours, so both are dropped here with a warning, which comes back beside the layer.
pub(crate) fn preset_layer(
    name: &str,
    set_in: &str,
    sources: &SettingsSources,
) -> Result<(PresetLayer, Vec<String>), SettingsError> {
    let (origin, source, text) = find(name, set_in, sources)?;
    let mut layer = PresetLayer {
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
    check_plain_keys(&layer.table, &layer.label())?;
    if let Some((key, _)) = leaves(&layer.table)
        .into_iter()
        .find(|(key, _)| is_safety_key(key))
    {
        return Err(SettingsError::Refused(format!(
            "{key} is a safety setting, so no preset may set it, and {} does; set it in your \
             own config instead",
            layer.label()
        )));
    }
    let mut warnings = Vec::new();
    if origin == Origin::Repo {
        warnings.extend(repo_warnings(&mut layer));
    }
    if layer.table.remove("preset").is_some() {
        warnings.push(format!(
            "preset in {} is ignored: a preset can't pick another preset",
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

/// A preset from the repo sits above your own config.local.toml, so the keys that are yours
/// are dropped from it with a warning. Replacing a built-in preset gets a warning too, since
/// it changes the protocol for everyone who clones the repo.
fn repo_warnings(layer: &mut PresetLayer) -> Vec<String> {
    let mut warnings = Vec::new();
    if builtin(&layer.name).is_some() {
        warnings.push(format!(
            "the {} preset comes from {}, which replaces the built-in one; delete the file to \
             get the built-in back",
            layer.name, layer.source
        ));
    }
    let mut dropped = Vec::new();
    drop_yours(&mut layer.table, "", &mut dropped);
    for key in dropped {
        warnings.push(format!(
            "{key} in {} is ignored: a preset from the repo can't change your own choices; the \
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

/// The presets folders beside the project files and beside the global file, in lookup order.
fn preset_dirs(sources: &SettingsSources) -> (Option<PathBuf>, Option<PathBuf>) {
    let beside = |file: Option<&PathBuf>| Some(file?.parent()?.join("presets"));
    let project = sources.project.as_ref().or(sources.project_local.as_ref());
    (beside(project), beside(sources.global.as_ref()))
}

/// Returns where the preset came from and its text.
fn find(
    name: &str,
    set_in: &str,
    sources: &SettingsSources,
) -> Result<(Origin, String, String), SettingsError> {
    if !is_preset_name(name) {
        return Err(SettingsError::Invalid {
            layer: set_in.to_owned(),
            message: not_a_preset_name(name),
        });
    }
    let (project, global) = preset_dirs(sources);
    let dirs = [(Origin::Repo, &project), (Origin::Home, &global)];
    for (origin, dir) in dirs {
        let Some(dir) = dir else { continue };
        if let Some((source, text)) = read_preset(&dir.join(format!("{name}.toml")))? {
            return Ok((origin, source, text));
        }
    }
    if let Some(preset) = builtin(name) {
        return Ok((
            Origin::BuiltIn,
            "built-in".to_owned(),
            preset.toml.to_owned(),
        ));
    }
    let known = match (&global, &project) {
        (Some(global), project) => presets_in(global, project.as_deref()),
        (None, Some(project)) => presets_in(project, None),
        (None, None) => presets(),
    };
    let names: Vec<String> = known.into_iter().map(|preset| preset.name).collect();
    Err(SettingsError::Invalid {
        layer: set_in.to_owned(),
        message: format!(
            "preset {name:?} isn't a built-in preset or a file in a presets folder; pick one of \
             {}",
            names.join(", ")
        ),
    })
}

fn read_preset(path: &Path) -> Result<Option<(String, String)>, SettingsError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some((path.display().to_string(), text))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(SettingsError::Io {
            path: path.to_owned(),
            message: err.to_string(),
        }),
    }
}
