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
    if ctx.settings().main_agent != before.settings.main_agent {
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

/// Where a change to `key` is saved: `picked`, when a flag or the scope question chose, or else
/// the layer its value came from, so the change takes effect and never hides under one that
/// wins. A value still at its default goes to everywhere.
pub(crate) fn scope_for_change(ctx: &Ctx, key: &str, picked: Option<SettingScope>) -> SettingScope {
    picked.unwrap_or_else(|| {
        let layer = ctx.settings.layer(key);
        let preset_from = ctx.settings.preset_from(&layer);
        SettingScope::for_change(&layer, preset_from.as_deref())
    })
}

/// Saves `key = value`, its value TOML text, where [`scope_for_change`] puts it, and
/// announces it.
pub(crate) fn set_where_it_came_from(
    ctx: &mut Ctx,
    key: String,
    value: String,
    picked: Option<SettingScope>,
) -> anyhow::Result<Vec<Effect>> {
    let scope = scope_for_change(ctx, &key, picked);
    apply(
        ctx,
        &Change::Set(vec![(key, value)]),
        scope,
        TrustAfter::Carry,
    )
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
    use argonauts_core::settings::ui::WidgetName;

    use super::super::tests::Fixture;
    use super::*;

    fn set(key: &str, value: &str) -> Change {
        Change::Set(vec![(key.to_owned(), value.to_owned())])
    }

    #[test]
    fn a_change_is_written_to_the_layer_its_value_came_from() {
        let mut fixture = Fixture::new(false, |repo| {
            repo.write(
                ".argonauts/config.toml",
                "[ui.above_input]\nmods = [\"plan-usage\"]\n",
            )
            .unwrap();
        });
        let ctx = &mut fixture.ctx;
        let project = ctx.env.project_config();
        let global = ctx.env.global_config();
        let pinned = "ui.above_input.mods";

        // From the project file, so /context pin edits the project file.
        let scope = scope_for_change(ctx, pinned, None);
        assert_eq!(scope, SettingScope::Project);
        make(
            ctx,
            &set(pinned, "[\"plan-usage\", \"context\"]"),
            scope,
            TrustAfter::Carry,
        )
        .unwrap();
        assert!(file_sets(&project, pinned));
        assert!(!file_sets(&global, pinned));
        assert_eq!(
            ctx.settings().ui.above_input.mods,
            [WidgetName::Quota, WidgetName::Context]
        );

        // Still at its default, so it goes to everywhere.
        let parts = "ui.views.status.mods";
        let scope = scope_for_change(ctx, parts, None);
        assert_eq!(scope, SettingScope::Global);
        make(ctx, &set(parts, "[\"runs\"]"), scope, TrustAfter::Carry).unwrap();
        assert!(file_sets(&global, parts));
        assert_eq!(ctx.settings().ui.views.status.mods, [WidgetName::Runs]);
        assert_eq!(scope_for_change(ctx, parts, None), SettingScope::Global);

        // Each flag picks its layer, whatever set the value.
        for picked in [
            SettingScope::Global,
            SettingScope::Project,
            SettingScope::ProjectLocal,
        ] {
            assert_eq!(scope_for_change(ctx, pinned, Some(picked)), picked);
        }
        let local = ctx.env.project_local_config();
        let scope = scope_for_change(ctx, pinned, Some(SettingScope::ProjectLocal));
        make(ctx, &set(pinned, "[]"), scope, TrustAfter::Carry).unwrap();
        assert!(file_sets(&local, pinned));
        assert!(ctx.settings().ui.above_input.mods.is_empty());
        // Now your own file sets it, and the next change follows it there.
        assert_eq!(
            scope_for_change(ctx, pinned, None),
            SettingScope::ProjectLocal
        );

        // A hide for the session stays in the session.
        let shown = "ui.above_input.shown";
        make(
            ctx,
            &set(shown, "false"),
            SettingScope::Session,
            TrustAfter::Carry,
        )
        .unwrap();
        assert_eq!(scope_for_change(ctx, shown, None), SettingScope::Session);
        assert!(!file_sets(&global, shown));
    }

    #[test]
    fn pinning_edits_the_file_the_band_came_from() {
        use argonauts_core::input::{ViewAction, WidgetChange, WidgetsAction};
        use argonauts_core::settings::ui::Place;

        use crate::controller::widgets;

        let mut fixture = Fixture::new(false, |repo| {
            repo.write(
                ".argonauts/config.toml",
                "[ui.above_input]\nmods = [\"plan-usage\"]\n",
            )
            .unwrap();
        });
        let ctx = &mut fixture.ctx;
        let project = std::fs::read_to_string(ctx.env.project_config()).unwrap();
        assert!(project.contains("[\"plan-usage\"]"), "{project}");

        // /context pin, then again to unpin.
        let pin = ViewAction::Pin { scope: None };
        widgets::tab(ctx, Place::Context, pin).unwrap();
        assert_eq!(
            ctx.settings().ui.above_input.mods,
            [WidgetName::Quota, WidgetName::Context]
        );
        let project = std::fs::read_to_string(ctx.env.project_config()).unwrap();
        assert!(project.contains("\"context\""), "{project}");
        assert!(!file_sets(&ctx.env.global_config(), "ui.above_input.mods"));
        widgets::tab(ctx, Place::Context, pin).unwrap();
        assert_eq!(ctx.settings().ui.above_input.mods, [WidgetName::Quota]);

        // /pin run-limits --global writes your global file, under the project's list.
        let show = WidgetsAction::Change {
            widget: WidgetName::Budget,
            change: WidgetChange::Show {
                place: Place::AboveInput,
            },
        };
        widgets::command(ctx, show, Some(SettingScope::Global)).unwrap();
        let global = std::fs::read_to_string(ctx.env.global_config()).unwrap();
        assert!(global.contains("\"run-limits\""), "{global}");

        // Folding a widget whose `open` is still its default goes to everywhere.
        let fold = WidgetsAction::Change {
            widget: WidgetName::Quota,
            change: WidgetChange::Fold,
        };
        widgets::command(ctx, fold, None).unwrap();
        assert!(file_sets(
            &ctx.env.global_config(),
            "ui.mods.plan-usage.open"
        ));
        assert!(!ctx.settings().ui.widget(WidgetName::Quota).open);
    }

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
