//! What `[outside]` gives a turn for the conversation's approval mode.

use argonauts_core::Asks;
use argonauts_core::outside::Outside;
use argonauts_core::settings::approvals::ApprovalMode;

use super::super::ConversationState;
use crate::controller::Ctx;
use crate::controller::turn::TurnStep;

/// As `[outside]` says for the conversation's mode. A turn your words asked to be asked about
/// first counts as supervised, as its asking does.
pub(in crate::controller) fn for_turn(
    ctx: &Ctx,
    conversation: &ConversationState,
    step: &TurnStep,
) -> Outside {
    let asks = step
        .direct_asking()
        .map(|asking| asking.as_ref().map_or(Asks::Never, |asking| asking.asks));
    let mode = match conversation.direct.approval {
        mode if mode.goes_ahead() && asks.is_some_and(|asks| asks != Asks::Never) => {
            ApprovalMode::Supervised
        }
        mode => mode,
    };
    let Some(home) = ctx.env.user_home.as_deref() else {
        return Outside::default();
    };
    Outside::for_mode(
        &ctx.settings().outside,
        mode,
        Some(conversation.folder(ctx)),
        home,
    )
}
