//! "From now on": a request that says to keep doing something applies it to its own run, and
//! is kept for later requests where your changes save, through the same steps `/config` takes.
//! Nothing asks where. Words that set the crew without a "from now on" are for that request
//! only, and say the crew understood.

use argonauts_core::NoteLevel;
use argonauts_core::app::AppEvent;

use crate::controller::instructions::Applied;
use crate::controller::{Ctx, Internal, crew_lines};

/// Keeps what the request said to keep doing, when it said so of anything that was followed.
pub(super) fn offer(ctx: &mut Ctx, applied: &Applied) {
    offer_in_run(ctx, applied, None);
}

/// [`offer`] for a run that adds models of its own around your words: `run` is its whole
/// request layer, so the crew shown is the one that runs.
pub(super) fn offer_in_run(ctx: &mut Ctx, applied: &Applied, run: Option<&[String]>) {
    if let Some(lines) = crew_lines::understood(ctx.settings(), applied, run) {
        let rows = crew_lines::rows(&lines, &ctx.names.names());
        ctx.send(AppEvent::CrewForRequest { rows });
    }
    if applied.keep.is_empty() {
        return;
    }
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Kept for later requests: {}.",
            applied.keep_said.join(" · ")
        ),
    );
    // Saved once the request's own start is handled, each with its line saying where.
    let _ = ctx.internal.send(Internal::SaveChanges {
        pairs: applied.keep.clone(),
    });
}
