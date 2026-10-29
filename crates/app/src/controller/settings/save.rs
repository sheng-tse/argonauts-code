//! Saving: where a change goes, a session override or a file through `write_setting` with
//! trust carried forward, then re-resolving, `SettingChanged`, and applying the change at once.
//!
//! No change asks where it goes. It saves where `save_changes_to` says, unless a layer above
//! would hide it there: then it goes where it takes effect, never into the team's file, and
//! the line after it says why.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::KeyScheme;
use argonauts_core::app::AppEvent;
use argonauts_core::settings::saving::SaveChangesTo;
use argonauts_core::settings::{GLOBAL_ONLY_KEYS, LAYERS, SettingScope, Settings, layer};
use argonauts_registry::{remove_setting, write_setting};

use super::confirm::safety_lines;
use crate::controller::{Ctx, Effect};

/// What a save does to its scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// Keys and TOML values, written together.
    Set(Vec<(String, String)>),
    /// Keys and TOML values your own words asked to keep, written as `Set` writes them. The
    /// line after names each key alone: "Kept checks.run".
    Kept(Vec<(String, String)>),
    /// Drops the key, so the layer below shows through.
    Reset(String),
}

impl Change {
    pub(crate) fn keys(&self) -> Vec<String> {
        match self {
            Self::Set(pairs) | Self::Kept(pairs) => {
                pairs.iter().map(|(key, _)| key.clone()).collect()
            }
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

/// The settings, values and layers before a change, for what the change announces.
pub(crate) struct Before {
    settings: Settings,
    previous: Vec<Option<String>>,
    /// Each key's layer under this session's own overrides, and the file that picked the
    /// workflow when that's the layer.
    layers: Vec<(String, Option<String>)>,
}

pub(crate) fn before(ctx: &Ctx, keys: &[String]) -> Before {
    Before {
        settings: ctx.settings().clone(),
        previous: keys.iter().map(|key| value_of(ctx, key)).collect(),
        layers: keys
            .iter()
            .map(|key| layer_below_session(ctx, key))
            .collect(),
    }
}

/// Where a change saves, and why when that isn't where `save_changes_to` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    pub scope: SettingScope,
    pub why: Option<String>,
}

/// Keys that only make sense in one repository, naming its commands, files or paths. They save
/// to this project, just for you, while your changes otherwise save everywhere.
const ONE_REPO_KEYS: &[&str] = &[
    "checks.run",
    "checks.setup",
    "checks.full_suite",
    "checks.copy_untracked",
    "merge.target",
    "merge.leave_out",
    "limits.protected_paths",
];

/// Where `save_changes_to` sends your changes. Only your global file may set it, so a
/// repository you clone can't send your changes into its shared file: the settings refuse it
/// from any other file.
pub(crate) fn save_changes_to(ctx: &Ctx) -> SaveChangesTo {
    ctx.settings().save_changes_to
}

/// Where a change to `key` saves when nothing names a place for it.
pub(crate) fn place(ctx: &Ctx, key: &str) -> Place {
    let (below, workflow_from) = layer_below_session(ctx, key);
    place_over(ctx, key, &below, workflow_from.as_deref())
}

/// Where a change to `key` saves on purpose, in `asked`, as a flag or ctrl+s says. A key only
/// your global file may hold refuses any other place.
pub(crate) fn asked_place(key: &str, asked: SettingScope) -> anyhow::Result<SettingScope> {
    if global_only(key) && asked != SettingScope::Global {
        anyhow::bail!("{key} is saved only in your global file, ~/.argonauts/config.toml");
    }
    Ok(asked)
}

/// [`place`] over a value now from `layer`. A layer that would hide the change sends it to the
/// first place above where it takes effect: this project just for you, else this session.
fn place_over(ctx: &Ctx, key: &str, layer: &str, workflow_from: Option<&str>) -> Place {
    if global_only(key) {
        return Place {
            scope: SettingScope::Global,
            why: None,
        };
    }
    let usual = usual_place(ctx, key);
    if !usual.hidden_by_value(layer, workflow_from) {
        return Place {
            scope: usual,
            why: None,
        };
    }
    let scope = [SettingScope::ProjectLocal, SettingScope::Session]
        .into_iter()
        .find(|scope| above(*scope, usual) && !scope.hidden_by_value(layer, workflow_from))
        .unwrap_or(SettingScope::Session);
    Place {
        scope,
        why: since(ctx, layer, scope),
    }
}

/// Where `save_changes_to` says, with a key for one repository kept in it.
fn usual_place(ctx: &Ctx, key: &str) -> SettingScope {
    usual_scope(save_changes_to(ctx), key)
}

/// Where a change to `key` saves by `save_changes_to`, before any layer above it is weighed: a
/// key only your global file holds there, a key for one repository in this project just for
/// you while your changes otherwise go everywhere, any other where `save_changes_to` says.
/// `argonauts config set` saves by it too.
pub(crate) fn usual_scope(save_changes_to: SaveChangesTo, key: &str) -> SettingScope {
    if global_only(key) {
        return SettingScope::Global;
    }
    match save_changes_to {
        SaveChangesTo::Everywhere if ONE_REPO_KEYS.contains(&key) => SettingScope::ProjectLocal,
        place => place.scope(),
    }
}

/// Whether `scope` saves into a layer that wins over `under`'s.
fn above(scope: SettingScope, under: SettingScope) -> bool {
    let rank = |scope: SettingScope| LAYERS.iter().position(|known| *known == scope.layer());
    rank(scope) > rank(under)
}

fn global_only(key: &str) -> bool {
    key == "save_changes_to"
        || GLOBAL_ONLY_KEYS
            .iter()
            .any(|pattern| match pattern.strip_suffix(".*") {
                Some(section) => key
                    .strip_prefix(section)
                    .is_some_and(|rest| rest.starts_with('.')),
                None => *pattern == key,
            })
}

/// Why a change moved to `scope`: what sets it in `layer`.
fn since(ctx: &Ctx, layer: &str, scope: SettingScope) -> Option<String> {
    Some(match layer {
        layer::PROJECT => "since the team's file sets it".to_owned(),
        layer::PROJECT_LOCAL if scope == SettingScope::ProjectLocal => {
            "since it's set there already".to_owned()
        }
        layer::PROJECT_LOCAL => "since your own file for this project sets it".to_owned(),
        layer::WORKFLOW => format!("since the {} workflow sets it", ctx.settings().workflow),
        layer::FLAG => "since a --set flag sets it".to_owned(),
        _ => return None,
    })
}

/// Why a change saved in `scope` doesn't show yet: what still wins over it, from `layer`.
fn still_wins(ctx: &Ctx, layer: &str) -> Option<String> {
    Some(match layer {
        layer::PROJECT => "though the team's file wins here".to_owned(),
        layer::PROJECT_LOCAL => "though your own file for this project wins here".to_owned(),
        layer::WORKFLOW => format!("though the {} workflow wins here", ctx.settings().workflow),
        layer::FLAG => "though a --set flag wins until you quit".to_owned(),
        layer::SESSION => "though this session's change wins until you quit".to_owned(),
        _ => return None,
    })
}

/// The layer `key` would come from without this session's own overrides, which a change
/// saved to a file replaces, and the file that picked the workflow when that's the layer.
fn layer_below_session(ctx: &Ctx, key: &str) -> (String, Option<String>) {
    let below = ctx
        .settings
        .layer_without_session(&ctx.env, key)
        .unwrap_or_else(|_| ctx.settings.layer(key));
    let workflow_from = ctx.settings.workflow_from(&below);
    (below, workflow_from)
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
        (SettingScope::Session, Change::Set(pairs) | Change::Kept(pairs)) => {
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
            let held = drop_session_overrides(ctx, change)?;
            if let Err(err) = write_file(ctx, &file, change, scope != SettingScope::Global, trust) {
                let env = ctx.env.clone();
                if !held.is_empty() {
                    let _ = ctx.settings.set_session_all(&env, &held);
                }
                return Err(err);
            }
        }
    }
    Ok(())
}

/// A change saved to a file replaces this session's own override of the key, which would
/// otherwise still win over it. Returns the overrides dropped, to put back if the write fails.
fn drop_session_overrides(ctx: &mut Ctx, change: &Change) -> anyhow::Result<Vec<(String, String)>> {
    let (Change::Set(pairs) | Change::Kept(pairs)) = change else {
        return Ok(Vec::new());
    };
    let held: Vec<(String, String)> = ctx
        .settings
        .session_overrides()
        .iter()
        .filter(|(set, _)| pairs.iter().any(|(key, _)| key == set))
        .cloned()
        .collect();
    let env = ctx.env.clone();
    for (key, _) in &held {
        ctx.settings.remove_session(&env, key)?;
    }
    Ok(held)
}

/// `SettingChanged` for each key, `UiSettings` when a `ui.` value changed, and the effects that
/// apply the change: a new status, the narrator, and a fresh conversation for a new main agent.
pub(crate) fn announce(
    ctx: &mut Ctx,
    before: Before,
    change: &Change,
    scope: SettingScope,
) -> Vec<Effect> {
    let keys = change.keys();
    for ((key, previous), (was_layer, was_from)) in
        keys.iter().zip(before.previous).zip(before.layers)
    {
        let why = match change {
            Change::Set(_) | Change::Kept(_) => {
                why_there(ctx, key, scope, &was_layer, was_from.as_deref())
            }
            Change::Reset(_) => None,
        };
        let (value, reset) = match change {
            Change::Set(pairs) | Change::Kept(pairs) => (
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
            why,
            kept: matches!(change, Change::Kept(_)),
        });
    }
    if ctx.settings().ui != before.settings.ui {
        send_ui(ctx);
    }
    let mut effects = Vec::new();
    if ctx.settings().crew.main != before.settings.crew.main {
        effects.push(Effect::FreshConversation);
    }
    effects.push(Effect::SettingsApplied { keys });
    effects
}

/// Why a change to `key`, the one key `before` was taken for, went to `scope`, as the line
/// after it says: what still wins over it there, or what moved it from where `save_changes_to`
/// says.
pub(crate) fn why_saved(
    ctx: &Ctx,
    before: &Before,
    key: &str,
    scope: SettingScope,
) -> Option<String> {
    let (was_layer, was_from) = before.layers.first()?;
    why_there(ctx, key, scope, was_layer, was_from.as_deref())
}

/// What the line after a change saved in `scope` ends with: what still wins over it there,
/// or why it went there rather than where `save_changes_to` says, from the layer that set it
/// before.
fn why_there(
    ctx: &Ctx,
    key: &str,
    scope: SettingScope,
    was_layer: &str,
    was_from: Option<&str>,
) -> Option<String> {
    let now = ctx.settings.layer(key);
    let why = if scope.hidden_by_value(&now, ctx.settings.workflow_from(&now).as_deref()) {
        still_wins(ctx, &now)
    } else {
        let moved = place_over(ctx, key, was_layer, was_from);
        (moved.scope == scope).then_some(moved.why).flatten()
    };
    // Your own file is kept out of git, unless this repository tracks it anyway.
    const TRACKED: &str = "though git tracks it, so your team sees it";
    let tracked = scope == SettingScope::ProjectLocal
        && crate::git::tracks(&ctx.env.repo_root, &ctx.env.project_local_config()).unwrap_or(false);
    match (why, tracked) {
        (Some(why), true) => Some(format!("{why}, {TRACKED}")),
        (None, true) => Some(TRACKED.to_owned()),
        (why, false) => why,
    }
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
        let workflow_from = ctx.settings.workflow_from(&layer);
        SettingScope::for_change(&layer, workflow_from.as_deref())
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
        Change::Set(pairs) | Change::Kept(pairs) => {
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
    use argonauts_core::settings::ui::ModName;

    use super::super::tests::Fixture;
    use super::*;

    fn set(key: &str, value: &str) -> Change {
        Change::Set(vec![(key.to_owned(), value.to_owned())])
    }

    /// A setting that names one repository's commands, files or paths saves for this project,
    /// just you, while other changes save everywhere.
    #[test]
    fn each_setting_of_one_repository_saves_for_this_project_just_you() {
        let fixture = Fixture::new(false, |_| {});
        for key in [
            "checks.run",
            "checks.setup",
            "checks.full_suite",
            "checks.copy_untracked",
            "merge.target",
            "merge.leave_out",
            "limits.protected_paths",
        ] {
            assert_eq!(
                place(&fixture.ctx, key).scope,
                SettingScope::ProjectLocal,
                "{key}"
            );
        }
        assert_eq!(
            place(&fixture.ctx, "ui.views.status.mods").scope,
            SettingScope::Global
        );
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
            [ModName::PlanUsage, ModName::Context]
        );

        // Still at its default, so it goes to everywhere.
        let parts = "ui.views.status.mods";
        let scope = scope_for_change(ctx, parts, None);
        assert_eq!(scope, SettingScope::Global);
        make(ctx, &set(parts, "[\"runs\"]"), scope, TrustAfter::Carry).unwrap();
        assert!(file_sets(&global, parts));
        assert_eq!(ctx.settings().ui.views.status.mods, [ModName::Runs]);
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
        use argonauts_core::input::{ModPlaceChange, ModsAction, ViewAction};
        use argonauts_core::settings::ui::Place;

        use crate::controller::mod_places;

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
        mod_places::tab(ctx, Place::Context, pin).unwrap();
        assert_eq!(
            ctx.settings().ui.above_input.mods,
            [ModName::PlanUsage, ModName::Context]
        );
        let project = std::fs::read_to_string(ctx.env.project_config()).unwrap();
        assert!(project.contains("\"context\""), "{project}");
        assert!(!file_sets(&ctx.env.global_config(), "ui.above_input.mods"));
        mod_places::tab(ctx, Place::Context, pin).unwrap();
        assert_eq!(ctx.settings().ui.above_input.mods, [ModName::PlanUsage]);

        // /pin run-limits --global writes your global file, under the project's list.
        let show = ModsAction::Change {
            the_mod: ModName::RunLimits,
            change: ModPlaceChange::Show {
                place: Place::AboveInput,
            },
        };
        mod_places::command(ctx, show, Some(SettingScope::Global)).unwrap();
        let global = std::fs::read_to_string(ctx.env.global_config()).unwrap();
        assert!(global.contains("\"run-limits\""), "{global}");

        // Folding a mod whose `open` is still its default goes to everywhere.
        let fold = ModsAction::Change {
            the_mod: ModName::PlanUsage,
            change: ModPlaceChange::Fold,
        };
        mod_places::command(ctx, fold, None).unwrap();
        assert!(file_sets(
            &ctx.env.global_config(),
            "ui.mods.plan-usage.open"
        ));
        assert!(!ctx.settings().ui.the_mod(ModName::PlanUsage).open);
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
