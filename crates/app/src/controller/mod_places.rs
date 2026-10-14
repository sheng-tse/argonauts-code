//! `/mods`' changes, `/pin`, `/unpin`, and `pin` on `/status`, `/usage`, `/context`, and
//! `/limits`. A change is one setting, saved in the layer its value came from unless the
//! command's `--global`, `--project`, or `--local` picked one. `/mods` itself and a mod's page
//! are the mod files' ([`crate::mods`]).

use argonauts_core::NoteLevel;
use argonauts_core::input::{ModPlaceChange, ModsAction, ViewAction};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::ui::{ModName, Place};

use super::settings::set_where_it_came_from;
use super::{Ctx, Effect};

/// `pin` on one of the Overview's tabs. `Open` and `print` are each tab's own command, which
/// answers before it gets here.
pub(crate) fn tab(ctx: &mut Ctx, tab: Place, action: ViewAction) -> anyhow::Result<Vec<Effect>> {
    match action {
        ViewAction::Open | ViewAction::Print => Ok(Vec::new()),
        // The tab's first mod, pinned above the input or unpinned.
        ViewAction::Pin { scope } => {
            let Some(&the_mod) = ctx.settings().ui.place(tab).first() else {
                ctx.notice(
                    NoteLevel::Info,
                    format!("The {} tab shows no mods to pin", tab.word()),
                );
                return Ok(Vec::new());
            };
            let change = ModPlaceChange::Toggle {
                place: Place::AboveInput,
            };
            command(ctx, ModsAction::Change { the_mod, change }, scope)
        }
    }
}

/// `/stats pin`: the stats card above the input, or off it when it's there.
pub(crate) fn pin_stats(ctx: &mut Ctx, scope: Option<SettingScope>) -> anyhow::Result<Vec<Effect>> {
    let change = ModPlaceChange::Toggle {
        place: Place::AboveInput,
    };
    let the_mod = ModName::Stats;
    command(ctx, ModsAction::Change { the_mod, change }, scope)
}

/// `/mods` and its changes, `/pin`, and `/unpin`.
pub(crate) fn command(
    ctx: &mut Ctx,
    action: ModsAction,
    scope: Option<SettingScope>,
) -> anyhow::Result<Vec<Effect>> {
    let setting = match action {
        // The controller opens /mods and a mod's page itself, from the mod files it keeps.
        ModsAction::List | ModsAction::Page { .. } => return Ok(Vec::new()),
        ModsAction::Band { shown } => {
            let now = ctx.settings().ui.above_input.shown;
            (now != shown).then(|| ("ui.above_input.shown".to_owned(), shown.to_string()))
        }
        ModsAction::Change { the_mod, change } => {
            // A key binding reaches here without the parser's check.
            if let ModPlaceChange::Toggle { place } | ModPlaceChange::Show { place } = change
                && !the_mod.fits(place)
            {
                ctx.notice(
                    NoteLevel::Info,
                    format!("{the_mod} can't show {}", place.phrase()),
                );
                return Ok(Vec::new());
            }
            change.setting(the_mod, &ctx.settings().ui)
        }
    };
    match setting {
        Some((key, value)) => set_where_it_came_from(ctx, key, value, scope),
        None => {
            ctx.notice(NoteLevel::Info, "Already so; nothing changed");
            Ok(Vec::new())
        }
    }
}

/// Whether `/mods` asks for the list of mods or one mod's page, which the mod files answer.
pub(crate) fn opens_mods(action: &ModsAction) -> bool {
    matches!(action, ModsAction::List | ModsAction::Page { .. })
}

/// The mod whose page `/mods <name>` opens. `None` opens the list.
pub(crate) fn page_asked(action: &ModsAction) -> Option<String> {
    match action {
        ModsAction::Page { the_mod } => Some(mod_name(*the_mod).to_owned()),
        _ => None,
    }
}

/// The mod file a card's name stands for. Plan usage was `plan_usage` before mods were files.
pub(crate) fn mod_name(the_mod: ModName) -> &'static str {
    match the_mod {
        ModName::PlanUsage => "plan-usage",
        other => other.name(),
    }
}
