//! A settings command's argument turned into the key and TOML value it sets, such as
//! `/effort reviewer high` into `roles.reviewer.effort = "high"`.

use argonauts_core::base::layout::project_files;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::input::{
    EFFORT_LEVELS, NarratorChange, PERMISSION_KEYS, ROLE_SANDBOX_KEYS, SlashCommand, WorkflowVerb,
    is_permission_key,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_registry::{setting_meta, settings_catalog, workflow_choices_in};

use super::models;
use crate::controller::Ctx;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingWrite {
    pub key: String,
    /// TOML text.
    pub value: String,
    /// The command whose default scope applies.
    pub command: &'static str,
    /// Keys saved along with it, such as the effort picked with a model.
    pub also: Vec<(String, String)>,
}

/// `Ok(None)` when the command has no argument and opens its picker. An error names what's
/// wrong, such as an effort level the model doesn't take.
pub(crate) fn setting_write(
    ctx: &Ctx,
    command: &SlashCommand,
) -> anyhow::Result<Option<SettingWrite>> {
    let mut also = Vec::new();
    let (key, value, name) = match command {
        SlashCommand::Model {
            role,
            worker: Some(worker),
            effort,
        } => {
            let role = role.filter(|role| *role != RoleName::Main);
            if let Some(why) = models::refusal(ctx, role, worker) {
                anyhow::bail!("{why}");
            }
            if let Some(level) = effort {
                check_level(ctx, worker, level)?;
                let name = role.unwrap_or(RoleName::Main).as_str();
                also.push((format!("roles.{name}.effort"), quoted(level)));
            }
            let (key, value) = model_key(role, worker);
            (key, value, "model")
        }
        SlashCommand::Effort {
            role,
            level: Some(level),
            ultracode,
        } => {
            if let Some(on) = ultracode {
                also.push((Settings::ULTRACODE.to_owned(), on.to_string()));
            }
            let name = role.unwrap_or(RoleName::Main).as_str();
            (format!("roles.{name}.effort"), quoted(level), "effort")
        }
        SlashCommand::Effort {
            level: None,
            ultracode: Some(on),
            ..
        } => (Settings::ULTRACODE.to_owned(), on.to_string(), "effort"),
        SlashCommand::Workflow {
            name: Some(name),
            action: WorkflowVerb::Use,
        } => ("workflow".to_owned(), quoted(name), "workflow"),
        SlashCommand::Approvals {
            key: Some(key),
            value: Some(value),
        } => (under("approvals", key), value.clone(), "approvals"),
        SlashCommand::Limits {
            key: Some(key),
            value: Some(value),
            ..
        } => (under("limits", key), value.clone(), "limits"),
        SlashCommand::Fallback {
            role,
            key: Some(key),
            value: Some(value),
        } => {
            let section = match role {
                Some(role) => fallback_section(*role),
                None => "fallback".to_owned(),
            };
            (under(&section, key), value.clone(), "fallback")
        }
        SlashCommand::Permissions {
            role,
            key: Some(key),
            value: Some(value),
        } => {
            let key = match role {
                Some(role) if ROLE_SANDBOX_KEYS.contains(&key.as_str()) => {
                    format!("roles.{}.sandbox.{key}", role.as_str())
                }
                Some(_) => anyhow::bail!(
                    "/permissions <role> sets {}; use /config for {key}",
                    ROLE_SANDBOX_KEYS.join(", ")
                ),
                None if is_permission_key(key) => key.clone(),
                None => anyhow::bail!(
                    "/permissions sets sandbox.* and {}; use /config for {key}",
                    PERMISSION_KEYS
                        .iter()
                        .filter(|key| !key.starts_with("sandbox."))
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            (key, value.clone(), "permissions")
        }
        SlashCommand::Narrator {
            change: Some(change),
        } => match change {
            NarratorChange::Mode { mode } => {
                let value = toml::Value::try_from(mode)?.to_string();
                ("ui.narrator".to_owned(), value, "narrator")
            }
            NarratorChange::Model { model } => (
                "narrator.model".to_owned(),
                quoted(&model.to_string()),
                "narrator",
            ),
        },
        SlashCommand::Theme { name: Some(name) } => ("ui.theme".to_owned(), quoted(name), "theme"),
        SlashCommand::Config {
            key: Some(key),
            value: Some(value),
            reset: false,
        } => (key.clone(), value.clone(), "config"),
        _ => return Ok(None),
    };
    let value = check_known(ctx, &key, &value)?;
    Ok(Some(SettingWrite {
        key,
        value,
        command: name,
        also,
    }))
}

/// The key a model pick sets and its TOML value: the worker you pick replaces the role's list.
pub(crate) fn model_key(role: Option<RoleName>, worker: &ModelRef) -> (String, String) {
    let worker = worker.to_string();
    match role.unwrap_or(RoleName::Main) {
        RoleName::Main => ("crew.main".to_owned(), quoted(&worker)),
        RoleName::Narrator => ("narrator.model".to_owned(), quoted(&worker)),

        role => (
            format!("roles.{}.models", role.as_str()),
            toml::Value::Array(vec![toml::Value::String(worker)]).to_string(),
        ),
    }
}

/// [`checked_value`], plus the checks that need the app: a workflow that exists, and an effort
/// level the role's model takes.
pub(crate) fn check_known(ctx: &Ctx, key: &str, value: &str) -> anyhow::Result<String> {
    let value = checked_value(key, value)?;
    let text = toml_value_text(&value);
    if key == "workflow" {
        check_workflow_choice(ctx, &text)?;
    }
    if let ["roles", role, "effort"] = key.split('.').collect::<Vec<_>>().as_slice() {
        let role = super::rows::role_named(role).filter(|role| *role != RoleName::Main);
        check_effort(ctx, role, &text)?;
    }
    Ok(value)
}

/// The string inside TOML text, or the text itself when it isn't a string.
fn toml_value_text(value: &str) -> String {
    format!("value = {value}")
        .parse::<toml::Table>()
        .ok()
        .and_then(|table| table.get("value")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| value.to_owned())
}

/// `value` as canonical TOML text for a known key, refusing a value its picker doesn't offer.
pub(crate) fn checked_value(key: &str, value: &str) -> anyhow::Result<String> {
    let value = toml_value(value);
    let Some(meta) = setting_meta(key) else {
        // The catalog lists keys with a built-in value, so a table such as a new endpoint, or
        // a key unset by default such as `fallback.strategy`, is left to the resolve after the
        // write, which undoes it when the key turns out unknown.
        if known_section(key) {
            return Ok(value);
        }
        anyhow::bail!("{key} isn't a setting; /config lists them");
    };
    // Your own workflows aren't in the catalog's list, so `/workflow` checks names itself.
    if key != "workflow" && !meta.allowed.is_empty() && !meta.allowed.contains(&value) {
        anyhow::bail!("{key} takes {}, not {value}", meta.allowed.join(", "));
    }
    Ok(value)
}

/// Whether `key` is in a section the catalog knows, such as `fallback`.
fn known_section(key: &str) -> bool {
    let section = key.split('.').next().unwrap_or_default();
    key.contains('.')
        && settings_catalog()
            .iter()
            .any(|meta| meta.key.split('.').next() == Some(section))
}

/// TOML text as it will be written. A bare word, like `high` in `/effort high`, is a string, as
/// with `--set`.
pub(crate) fn toml_value(text: &str) -> String {
    let text = text.trim();
    match format!("value = {text}").parse::<toml::Table>() {
        Ok(table) if table.len() == 1 => match table.get("value") {
            Some(value) => value.to_string(),
            None => quoted(text),
        },
        _ => quoted(text),
    }
}

pub(crate) fn quoted(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

fn check_effort(ctx: &Ctx, role: Option<RoleName>, level: &str) -> anyhow::Result<()> {
    let (worker, levels) = models::efforts(ctx, role);
    takes_level(worker.as_ref(), &levels, level)
}

/// For a model picked with an effort: the level must be one that model takes.
fn check_level(ctx: &Ctx, worker: &ModelRef, level: &str) -> anyhow::Result<()> {
    takes_level(Some(worker), &models::levels_of(ctx, Some(worker)), level)
}

fn takes_level(worker: Option<&ModelRef>, levels: &[String], level: &str) -> anyhow::Result<()> {
    if !EFFORT_LEVELS.contains(&level) {
        anyhow::bail!("{level:?} isn't a level; use {}", EFFORT_LEVELS.join(", "));
    }
    if levels.iter().any(|known| known == level) {
        return Ok(());
    }
    let who = worker.map_or_else(|| "That model".to_owned(), |worker| worker.to_string());
    match levels {
        [] => anyhow::bail!("{who} takes no effort setting"),
        [only] => anyhow::bail!("{who} takes only {only} effort, fixed by the model"),
        _ => {}
    }
    anyhow::bail!(
        "{who} doesn't take {level} effort; it takes {}",
        levels.join(", ")
    )
}

fn check_workflow_choice(ctx: &Ctx, name: &str) -> anyhow::Result<()> {
    let names: Vec<String> = workflow_list(ctx)
        .into_iter()
        .map(|info| info.name)
        .collect();
    if names.iter().any(|known| known == name) {
        return Ok(());
    }
    anyhow::bail!(
        "There's no workflow named {name}; pick one of {}",
        names.join(", ")
    )
}

/// Every workflow you can pick: the built-ins, yours, then the project's.
pub(crate) fn workflow_list(ctx: &Ctx) -> Vec<argonauts_registry::WorkflowChoice> {
    // `.argonauts/workflows`, next to the project's `config.toml`.
    let project = ctx
        .env
        .project_config()
        .with_file_name(project_files::WORKFLOWS);
    workflow_choices_in(&ctx.env.paths.global_workflows(), Some(&project))
}

/// Where a role's own fallback settings sit: the table of the crew role it follows, such as
/// `fallback.worker` for the worker.
pub(crate) fn fallback_section(role: RoleName) -> String {
    format!("fallback.{}", CrewRole::fallback_of(role).as_str())
}

/// `section.key`, unless you typed the section already.
pub(crate) fn under(section: &str, key: &str) -> String {
    if super::rows::in_section(key, section) {
        key.to_owned()
    } else {
        format!("{section}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_words_become_strings_and_toml_stays_toml() {
        assert_eq!(toml_value("high"), "\"high\"");
        assert_eq!(toml_value("\"high\""), "\"high\"");
        assert_eq!(toml_value("true"), "true");
        assert_eq!(toml_value("5"), "5");
        assert_eq!(toml_value("codex/gpt-5"), "\"codex/gpt-5\"");
        assert_eq!(toml_value("[\"a\", \"b\"]"), "[\"a\", \"b\"]");
        // A second key smuggled onto a new line is text, not a table.
        assert_eq!(
            toml_value("1\n[sandbox]\nmode = \"off\""),
            toml::Value::String("1\n[sandbox]\nmode = \"off\"".to_owned()).to_string()
        );
    }

    #[test]
    fn a_value_the_picker_doesnt_offer_is_refused() {
        assert_eq!(checked_value("ui.theme", "dark").unwrap(), "\"dark\"");
        let err = checked_value("ui.theme", "plaid").unwrap_err().to_string();
        assert!(err.contains("\"dark\""), "{err}");
        let err = checked_value("nothing.here", "1").unwrap_err().to_string();
        assert!(err.contains("isn't a setting"), "{err}");
        let endpoint = "{ base_url = \"http://localhost:11434/v1\" }";
        assert_eq!(
            checked_value("endpoints.ollama", endpoint).unwrap(),
            endpoint
        );
        assert_eq!(
            checked_value("fallback.strategy", "spread").unwrap(),
            "\"spread\""
        );
    }

    #[test]
    fn a_model_pick_replaces_the_role_list() {
        let worker: ModelRef = "codex/gpt-5".parse().unwrap();
        assert_eq!(
            model_key(Some(RoleName::Reviewer), &worker),
            (
                "roles.reviewer.models".to_owned(),
                "[\"codex/gpt-5\"]".to_owned()
            )
        );
        assert_eq!(model_key(None, &worker).0, "crew.main");
        assert_eq!(
            model_key(Some(RoleName::Narrator), &worker).0,
            "narrator.model"
        );
        assert_eq!(
            model_key(Some(RoleName::Judge), &worker).0,
            "roles.judge.models"
        );
    }
}
