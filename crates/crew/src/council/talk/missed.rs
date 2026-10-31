//! A seat's turn that ran out or failed: the seat sits that turn out and the talk goes on.

use std::time::Duration;

use argonauts_core::base::names::SeatId;
use argonauts_core::models::meter::LimitDimension;
use argonauts_core::runs::events::council::{CouncilEventKind, Missed, MissedTurn};
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::workflows::steps::StepError;
use argonauts_core::workflows::workflow::StepRef;

use super::{Council, council_now, record};
use crate::council::points;
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;

/// How a seat's turn went wrong, when it is the seat's own trouble: `Some(true)` when it ran
/// past its time, `Some(false)` for any other failure of its own. `None` for what the run
/// answers as a whole, as it would for any other step: a pause, your stop, a plan's usage
/// limit, signing in, credits, the rate limit, the run's spend, a full context.
fn own_trouble(err: &EngineError) -> Option<bool> {
    let end = match err {
        EngineError::Stalled(_) | EngineError::Step(StepError::SchemaMismatch { .. }) => {
            return Some(false);
        }
        EngineError::Step(StepError::Session(end)) => end,
        _ => return None,
    };
    match &end.reason {
        EndReason::LimitHit {
            dimension: LimitDimension::Minutes,
        } => Some(true),
        EndReason::LimitHit {
            dimension: LimitDimension::ToolCalls,
        }
        | EndReason::Crashed { .. }
        | EndReason::CliError { .. }
        | EndReason::StartFailed { .. }
        | EndReason::AdapterBroken { .. }
        | EndReason::Stuck { .. } => Some(false),
        _ => None,
    }
}

/// A seat's turn that went wrong, `limit` being the turn's own time limit. Trouble of the
/// seat's own is a missed turn: the seat sits that turn out and the talk goes on, and a second
/// miss leaves it out for the rest of the talk, where the seats still in agree without it.
/// Anything else, and anything once the run pauses or stops, goes back to the run.
pub(super) fn missed(
    host: &Host,
    talk: &StepRef,
    council: &Council,
    seat: &SeatId,
    turn: MissedTurn,
    limit: Option<Duration>,
    err: EngineError,
) -> EngineResult<()> {
    let Some(ran_out) = own_trouble(&err).filter(|_| !host.pausing()) else {
        return Err(err);
    };
    let now = council_now(host, talk).unwrap_or_else(|| council.clone());
    let for_good = now.missed.get(seat).is_some_and(|misses| *misses >= 1);
    let limit_secs = limit
        .filter(|_| ran_out)
        .map(|limit| u32::try_from(limit.as_secs()).unwrap_or(u32::MAX));
    record(
        host,
        talk,
        CouncilEventKind::Missed(Missed {
            seat: seat.clone(),
            turn,
            limit_secs,
            for_good,
        }),
    )?;
    if !for_good {
        return Ok(());
    }
    let now = council_now(host, talk).unwrap_or_else(|| council.clone());
    let mut points = now.points.clone();
    if points::recount(&mut points, &now.stances, &now.table()) {
        record(host, talk, CouncilEventKind::Points { points })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::host::crashed_end;

    fn ended(reason: EndReason) -> EngineError {
        let mut end = crashed_end("");
        end.reason = reason;
        EngineError::Step(StepError::Session(Box::new(end)))
    }

    #[test]
    fn only_a_seats_own_trouble_is_a_missed_turn() {
        let minutes = LimitDimension::Minutes;
        assert_eq!(
            own_trouble(&ended(EndReason::LimitHit { dimension: minutes })),
            Some(true)
        );
        let crashed = EndReason::Crashed {
            message: "segfault".to_owned(),
        };
        assert_eq!(own_trouble(&ended(crashed)), Some(false));
        let tools = LimitDimension::ToolCalls;
        assert_eq!(
            own_trouble(&ended(EndReason::LimitHit { dimension: tools })),
            Some(false)
        );
        let spend = LimitDimension::Spend;
        for the_runs in [
            EndReason::Paused,
            EndReason::Stopped,
            EndReason::RateLimited,
            EndReason::ContextFull,
            EndReason::UsageLimit { resets_at: None },
            EndReason::Auth {
                message: "signed out".to_owned(),
            },
            EndReason::Billing {
                message: "no credits".to_owned(),
            },
            EndReason::LimitHit { dimension: spend },
        ] {
            assert_eq!(own_trouble(&ended(the_runs.clone())), None, "{the_runs:?}");
        }
        assert_eq!(own_trouble(&EngineError::Interrupted), None);
    }
}
