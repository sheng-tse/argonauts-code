//! Saving: a session override, or a file through `write_setting` with trust carried forward;
//! then re-resolving, `SettingChanged`, and applying the change at once.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::KeyScheme;
use argonauts_core::app::AppEvent;
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_registry::{remove_setting, write_setting};

use super::confirm::safety_lines;
use crate::controller::{Ctx, Effect};

/// What a save does to its scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// Keys and TOML values, written together.
    Set(Vec<(String, String)>),
    /// Drops the key, so the layer below shows through.
    Reset(String),
}

impl Change {
    pub(crate) fn keys(&self) -> Vec<String> {
        match self {
            Self::Set(pairs) => pairs.iter().map(|(key, _)| key.clone()).collect(),
            Self::Reset(key) => vec![key.clone()],
        }
    }
}

/// What happens to a project file's trust after the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustAfter {
    /// Trusted when it was trusted before, didn't exist, or held no safety key, since then
    /// nothing in it is something you didn't write.
    Carry,
    /// You said to trust it.
    Approved,
    /// You said not now, so it stays untrusted.
    Declined,
}

/// The settings and values before a change, for what the change announces.
pub(crate) struct Before {
    settings: Settings,
    previous: Vec<Option<String>>,
}

pub(crate) fn before(ctx: &Ctx, keys: &[String]) -> Before {
    Before {
        settings: ctx.settings().clone(),
        previous: keys.iter().map(|key| value_of(ctx, key)).collect(),
    }
}

/// Makes `change` in `scope`, the safety confirm and trust question already answered, then
/// announces it. On failure nothing changes.
pub(crate) fn apply(
    ctx: &mut Ctx,
    change: &Change,
    scope: SettingScope,
    trust: TrustAfter,
) -> anyhow::Result<Vec<Effect>> {
    let before = before(ctx, &change.keys());
    make(ctx, change, scope, trust)?;
    Ok(announce(ctx, before, change, scope))
}

/// [`apply`] without announcing it.
pub(crate) fn make(
    ctx: &mut Ctx,
    change: &Change,
    scope: SettingScope,
    trust: TrustAfter,
) -> anyhow::Result<()> {
    match (scope, change) {
        (SettingScope::Session, Change::Set(pairs)) => {
            let env = ctx.env.clone();
            ctx.settings.set_session_all(&env, pairs)?;
        }
        (SettingScope::Session, Change::Reset(key)) => {
            let env = ctx.env.clone();
            if !ctx.settings.remove_session(&env, key)? {
                anyhow::bail!("{key} has no override in this session");
            }
        }
        (scope, change) => {
            let file = file_for(ctx, scope).context("a file scope")?;
            write_file(ctx, &file, change, scope != SettingScope::Global, trust)?;
        }
    }
    Ok(())
}

/// `SettingChanged` for each key, `UiSettings` when a `ui.` value changed, and the effects that
/// apply the change: a new status, the narrator, and a fresh chat for a new default agent.
pub(crate) fn announce(
    ctx: &mut Ctx,
    before: Before,
    change: &Change,
    scope: SettingScope,
) -> Vec<Effect> {
    let keys = change.keys();
    for (key, previous) in keys.iter().zip(before.previous) {
        let (value, reset) = match change {
            Change::Set(pairs) => (
                pairs
                    .iter()
                    .find(|(set, _)| set == key)
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default(),
                false,
            ),
            Change::Reset(_) => (value_of(ctx, key).unwrap_or_default(), true),
        };
        ctx.send(AppEvent::SettingChanged {
            key: key.clone(),
            value,
            scope,
            previous,
            reset,
        });
    }
    if ctx.settings().ui != before.settings.ui {
        send_ui(ctx);
    }
    let mut effects = Vec::new();
    if ctx.settings().default_agent != before.settings.default_agent {
        effects.push(Effect::FreshChat);
    }
    effects.push(Effect::SettingsApplied { keys });
    effects
}

/// The `[ui]` section in effect, which the TUI applies at once.
pub(crate) fn send_ui(ctx: &Ctx) {
    let ui = &ctx.settings().ui;
    ctx.send(AppEvent::UiSettings {
        vim: ui.keys == KeyScheme::Vim,
        verbosity: ui.verbosity,
        ui: Some(Box::new(ui.clone())),
    });
}

/// The file a scope saves into; `None` for the session.
pub(crate) fn file_for(ctx: &Ctx, scope: SettingScope) -> Option<PathBuf> {
    match scope {
        SettingScope::Session => None,
        SettingScope::ProjectLocal => Some(ctx.env.project_local_config()),
        SettingScope::Project => Some(ctx.env.project_config()),
        SettingScope::Global => Some(ctx.env.global_config()),
    }
}

/// The value in effect as TOML text, `None` when unset.
pub(crate) fn value_of(ctx: &Ctx, key: &str) -> Option<String> {
    ctx.settings
        .entry(key)
        .map(|entry| entry.value.clone())
        .filter(|value| !value.is_empty())
}

/// Whether the file sets `key` itself.
pub(crate) fn file_sets(file: &Path, key: &str) -> bool {
    let Some(table) = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
    else {
        return false;
    };
    let mut current = &table;
    let segments: Vec<&str> = key.split('.').collect();
    for (at, segment) in segments.iter().enumerate() {
        match current.get(*segment) {
            Some(toml::Value::Table(inner)) if at + 1 < segments.len() => current = inner,
            Some(_) => return at + 1 == segments.len(),
            None => return false,
        }
    }
    false
}

/// Writes the change and re-resolves, putting the file and its trust back when either fails,
/// so a value that doesn't resolve never stays on disk to break the next start.
fn write_file(
    ctx: &mut Ctx,
    file: &Path,
    change: &Change,
    project: bool,
    trust: TrustAfter,
) -> anyhow::Result<()> {
    let original = std::fs::read(file).ok();
    let recorded = ctx.settings.trust().recorded(file);
    let trust_after = project
        && match trust {
            TrustAfter::Approved => true,
            TrustAfter::Declined => false,
            TrustAfter::Carry => {
                original.is_none()
                    || ctx.settings.trust().was_trusted(file)
                    || safety_lines(file).is_empty()
            }
        };
    let Err(err) = write_and_reload(ctx, file, change, trust_after) else {
        return Ok(());
    };
    let restored = match &original {
        Some(bytes) => std::fs::write(file, bytes),
        None => std::fs::remove_file(file).or_else(|err| match err.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(err),
        }),
    };
    let trust_restored = if trust_after {
        ctx.settings.trust_mut().put_back(file, recorded)
    } else {
        Ok(())
    };
    let env = ctx.env.clone();
    // Only files you can't read fail here, and the first error says why.
    let _ = ctx.settings.reload(&env);
    let mut err = err;
    if let Err(restore) = restored {
        err = err.context(format!(
            "and putting {} back failed: {restore}",
            ctx.env.shown(file)
        ));
    }
    if let Err(restore) = trust_restored {
        err = err.context(format!("and putting its trust back failed: {restore:#}"));
    }
    Err(err)
}

fn write_and_reload(
    ctx: &mut Ctx,
    file: &Path,
    change: &Change,
    trust_after: bool,
) -> anyhow::Result<()> {
    match change {
        Change::Set(pairs) => {
            for (key, value) in pairs {
                write_setting(file, key, value)?;
            }
        }
        Change::Reset(key) => {
            if !remove_setting(file, key)? {
                anyhow::bail!("{key} isn't set in {}", ctx.env.shown(file));
            }
        }
    }
    if trust_after {
        ctx.settings.trust_mut().trust(file)?;
    }
    let env = ctx.env.clone();
    ctx.settings.reload(&env)?;
    if let Some(unknown) = ctx.settings.unknown_among(&change.keys()) {
        anyhow::bail!("{unknown} isn't a setting; /config lists them");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_sets_only_the_keys_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[ui]\ntheme = \"dark\"\n").unwrap();
        assert!(file_sets(&file, "ui.theme"));
        // A table counts, as `endpoints.ollama = { ... }` does.
        assert!(file_sets(&file, "ui"));
        assert!(!file_sets(&file, "ui.keys"));
        assert!(!file_sets(&file, "ui.theme.x"));
        assert!(!file_sets(&dir.path().join("missing.toml"), "ui.theme"));
    }
}
