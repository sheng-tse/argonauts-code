//! The CLIs' model lists, read again in the background at start and whenever `/model` opens.
//! The picker shows the cached list with when it was read, and takes the new one when it lands.

use std::sync::Arc;

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::names::RoleName;
use time::OffsetDateTime;

use super::{Handled, SettingsState, models};
use crate::controller::{Ctx, Internal};

/// The model list last sent, so a refresh that lands knows which picker to bring up to date.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ModelsShown {
    /// `None` is the default agent.
    role: Option<RoleName>,
    refreshed: Option<OffsetDateTime>,
}

/// Starts a refresh unless one is running or discovery is off. Discovery never calls a model.
pub(crate) fn start(ctx: &Ctx, settings: &mut SettingsState) {
    if settings.refreshing || !ctx.settings().registry.discover {
        return;
    }
    settings.refreshing = true;
    let registry = Arc::clone(&ctx.services.registry);
    let internal = ctx.internal.clone();
    tokio::spawn(async move {
        registry.discover().await;
        let _ = internal.send(Internal::ModelsRefreshed);
    });
}

/// The models `role` may use, with how fresh the list is.
pub(crate) fn view(ctx: &Ctx, settings: &mut SettingsState, role: Option<RoleName>) -> ViewData {
    let role = role.filter(|role| *role != RoleName::Main);
    let refreshed = ctx.services.registry.refreshed();
    settings.shown = Some(ModelsShown { role, refreshed });
    ViewData::Models {
        choices: models::choices(ctx, role),
        role,
        refreshed,
        refreshing: settings.refreshing,
        update: false,
    }
}

/// The refresh landed: the picker showing a model list gets the new one. A list read no later
/// than the one it shows changes nothing, so nothing goes out.
pub(crate) fn landed(ctx: &Ctx, settings: &mut SettingsState) -> Handled {
    settings.refreshing = false;
    let refreshed = ctx.services.registry.refreshed();
    let Some(shown) = settings.shown.as_mut() else {
        return Ok(Vec::new());
    };
    if shown.refreshed == refreshed {
        return Ok(Vec::new());
    }
    shown.refreshed = refreshed;
    let role = shown.role;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Models {
            choices: models::choices(ctx, role),
            role,
            refreshed,
            refreshing: false,
            update: true,
        }),
    });
    Ok(Vec::new())
}
