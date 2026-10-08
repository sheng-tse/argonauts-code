//! `/mods`' changes, `/pin`, `/unpin`, and `pin` on `/status`, `/usage`, `/context`, and
//! `/limits`. A change is one setting, saved in the layer its value came from unless the
//! command's `--global`, `--project`, or `--local` picked one. `/mods` itself and a mod's page
//! are the mod files' ([`crate::mods`]).

use argonauts_core::NoteLevel;
use argonauts_core::input::{ViewAction, WidgetChange, WidgetsAction};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::ui::{Place, WidgetName};

use super::settings::set_where_it_came_from;
use super::{Ctx, Effect};

/// `pin` on one of the Overview's tabs. `Open` and `print` are each tab's own command, which
/// answers before it gets here.
pub(crate) fn tab(ctx: &mut Ctx, tab: Place, action: ViewAction) -> anyhow::Result<Vec<Effect>> {
    match action {
        ViewAction::Open | ViewAction::Print => Ok(Vec::new()),
        // The tab's first widget, pinned above the input or unpinned.
        ViewAction::Pin { scope } => {
            let Some(&widget) = ctx.settings().ui.place(tab).first() else {
                ctx.notice(
                    NoteLevel::Info,
                    format!("The {} tab shows no mods to pin", tab.word()),
                );
                return Ok(Vec::new());
            };
            let change = WidgetChange::Toggle {
                place: Place::AboveInput,
            };
            command(ctx, WidgetsAction::Change { widget, change }, scope)
        }
    }
}

/// `/stats pin`: the stats card above the input, or off it when it's there.
pub(crate) fn pin_stats(ctx: &mut Ctx, scope: Option<SettingScope>) -> anyhow::Result<Vec<Effect>> {
    let change = WidgetChange::Toggle {
        place: Place::AboveInput,
    };
    let widget = WidgetName::Stats;
    command(ctx, WidgetsAction::Change { widget, change }, scope)
}

/// `/widgets` and its changes, `/pin`, and `/unpin`.
pub(crate) fn command(
    ctx: &mut Ctx,
    action: WidgetsAction,
    scope: Option<SettingScope>,
) -> anyhow::Result<Vec<Effect>> {
    let setting = match action {
        // The controller opens /mods and a mod's page itself, from the mod files it keeps.
        WidgetsAction::List | WidgetsAction::Page { .. } => return Ok(Vec::new()),
        WidgetsAction::Band { shown } => {
            let now = ctx.settings().ui.above_input.shown;
            (now != shown).then(|| ("ui.above_input.shown".to_owned(), shown.to_string()))
        }
        WidgetsAction::Change { widget, change } => {
            // A key binding reaches here without the parser's check.
            if let WidgetChange::Toggle { place } | WidgetChange::Show { place } = change
                && !widget.fits(place)
            {
                ctx.notice(
                    NoteLevel::Info,
                    format!("{widget} can't show {}", place.phrase()),
                );
                return Ok(Vec::new());
            }
            change.setting(widget, &ctx.settings().ui)
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
pub(crate) fn opens_mods(action: &WidgetsAction) -> bool {
    matches!(action, WidgetsAction::List | WidgetsAction::Page { .. })
}

/// The mod whose page `/mods <name>` opens. `None` opens the list.
pub(crate) fn page_asked(action: &WidgetsAction) -> Option<String> {
    match action {
        WidgetsAction::Page { widget } => Some(mod_name(*widget).to_owned()),
        _ => None,
    }
}

/// The mod file a card's name stands for. Plan usage was `quota` before mods were files.
pub(crate) fn mod_name(widget: WidgetName) -> &'static str {
    match widget {
        WidgetName::Quota => "plan-usage",
        other => other.name(),
    }
}
