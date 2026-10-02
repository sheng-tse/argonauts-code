//! `/vim` and `/verbosity <level>`: saved everywhere without asking, or held for this session
//! when a project file or flag sets the key, with a notice saying so.

use argonauts_core::NoteLevel;
use argonauts_core::settings::SettingScope;

use super::save::{self, Change, TrustAfter};
use crate::controller::{Ctx, Effect};

/// `key` is `ui.keys` or `ui.verbosity`; `value` is TOML text.
pub(crate) fn save_toggle(ctx: &mut Ctx, key: &str, value: &str) -> anyhow::Result<Vec<Effect>> {
    let env = ctx.env.clone();
    let change = Change::Set(vec![(key.to_owned(), value.to_owned())]);
    let below = ctx.settings.layer_without_session(&env, key)?;
    let preset_from = ctx.settings.preset_from(&below);
    if SettingScope::Global.hidden_by_value(&below, preset_from.as_deref()) {
        let effects = save::apply(ctx, &change, SettingScope::Session, TrustAfter::Carry)?;
        ctx.notice(
            NoteLevel::Info,
            format!(
                "{} sets {key}, so saving it everywhere wouldn't change this repo. It holds for \
                 this session instead.",
                who_sets(&below)
            ),
        );
        return Ok(effects);
    }

    let before = save::before(ctx, &change.keys());
    // A session override from earlier would still win over the file you're changing.
    let held = ctx
        .settings
        .session_overrides()
        .iter()
        .find(|(set, _)| set == key)
        .map(|(_, value)| value.clone());
    if held.is_some() {
        ctx.settings.remove_session(&env, key)?;
    }
    if let Err(err) = save::make(ctx, &change, SettingScope::Global, TrustAfter::Carry) {
        if let Some(held) = held {
            let _ = ctx.settings.set_session(&env, key, &held);
        }
        return Err(err);
    }
    Ok(save::announce(ctx, before, &change, SettingScope::Global))
}

fn who_sets(layer: &str) -> &'static str {
    match layer {
        "project" => "This project's file",
        "project-local" => "Your own project file (config.local.toml)",
        "preset" => "The preset",
        _ => "A --set flag",
    }
}
