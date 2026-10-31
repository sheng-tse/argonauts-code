//! What `[outside]` gives a turn for the conversation's approval mode.

use argonauts_core::Asks;
use argonauts_core::safety::outside::Outside;
use argonauts_core::settings::approvals::ApprovalMode;

use super::super::ConversationState;
use crate::controller::Ctx;
use crate::controller::turns::turn::TurnStep;

/// As `[outside]` says for the conversation's mode. Where the mode works outside only once
/// asked, a turn that asks nothing keeps to its folder, and one whose CLI can't ask before it
/// writes reads outside and writes only its folder: a write there it was turned from waits for
/// you, and the turn you let carry on by answering writes there.
pub(in crate::controller) fn for_turn(
    ctx: &Ctx,
    conversation: &ConversationState,
    step: &TurnStep,
    agent: &str,
) -> Outside {
    let outside = for_mode(ctx, conversation, step);
    if !outside.after_asking {
        return outside;
    }
    if step.writes_outside() {
        return Outside {
            read: true,
            write: true,
            after_asking: false,
            ..outside
        };
    }
    if !asks_you(step) {
        return Outside::default();
    }
    if super::asks_each(agent) {
        outside
    } else {
        outside.without_asking()
    }
}

/// Whether the conversation's mode lets a turn work outside once asked, so a write there a
/// turn was turned from may be allowed by answering.
pub(in crate::controller) fn opens_once_asked(
    ctx: &Ctx,
    conversation: &ConversationState,
    step: &TurnStep,
) -> bool {
    for_mode(ctx, conversation, step).after_asking
}

/// `[outside]` for the conversation's mode. A turn your words asked to be asked about first
/// counts as supervised, as its asking does.
fn for_mode(ctx: &Ctx, conversation: &ConversationState, step: &TurnStep) -> Outside {
    let mode = match conversation.direct.approval {
        mode if mode.goes_ahead() && asks_you(step) => ApprovalMode::Supervised,
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

fn asks_you(step: &TurnStep) -> bool {
    step.direct_asking()
        .and_then(|asking| asking.as_ref())
        .is_some_and(|asking| asking.asks != Asks::Never)
}
