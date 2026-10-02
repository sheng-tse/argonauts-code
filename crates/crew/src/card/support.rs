//! Small reads and records the card modules share.

use std::time::Duration;

use argonauts_core::CardId;
use argonauts_core::events::card::{CardEvent, CardState};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::run_files;
use argonauts_core::names::{SessionKey, WorkerRef};
use argonauts_core::steps::StepName;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::state::cards::{CardProgress, CardRecord};
use crate::state::sessions::SessionRecord;

/// How often a wait on something outside the card (an answer, a vendor, budget) looks again.
const POLL: Duration = Duration::from_millis(100);

pub(crate) fn record_of(host: &Host, card: &CardId) -> EngineResult<CardRecord> {
    host.read(|state| state.cards.get(card).cloned())
        .ok_or_else(|| EngineError::Refused(format!("there is no card {card}")))
}

/// The pipeline's bookkeeping for `card`.
pub(crate) fn progress_of(host: &Host, card: &CardId) -> CardProgress {
    host.read(|state| state.cards.progress(card).clone())
}

pub(crate) fn state_of(host: &Host, card: &CardId) -> EngineResult<CardState> {
    record_of(host, card).map(|record| record.state)
}

pub(crate) fn record_card(host: &Host, card: &CardId, event: CardEvent) -> EngineResult<RunEvent> {
    host.record(Some(card), None, RunEventKind::Card(event))
}

pub(crate) fn record_card_in(
    host: &Host,
    card: &CardId,
    session: &SessionKey,
    event: CardEvent,
) -> EngineResult<RunEvent> {
    host.record(Some(card), Some(session), RunEventKind::Card(event))
}

/// The card's own events so far, for its handoff packet. None when the run keeps no log on
/// disk, as with an in-memory sink; a log that can't be read is an error.
pub(crate) fn card_events(host: &Host, card: &CardId) -> EngineResult<Vec<RunEvent>> {
    let path = host.repo().run_dir.join(run_files::EVENTS);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let back = argonauts_core::journal::read_all(&path)?;
    Ok(back
        .events
        .into_iter()
        .filter(|event| event.card_id.as_ref() == Some(card))
        .collect())
}

/// The card's owner: the one recorded, else the planned one, else the default agent.
pub(crate) fn owner_of(host: &Host, record: &CardRecord) -> WorkerRef {
    record
        .owner
        .clone()
        .or_else(|| record.spec.owner.clone())
        .unwrap_or_else(|| host.settings().default_agent.clone())
}

/// The card's newest owner session, finished or not.
pub(crate) fn newest_owner_session<'a>(
    state: &'a RunState,
    card: &CardId,
) -> Option<&'a SessionRecord> {
    state
        .sessions
        .by_key
        .values()
        .filter(|s| s.card.as_ref() == Some(card) && s.started.step == Some(StepName::Build))
        .max_by_key(|s| s.since)
}

pub(crate) fn part(
    kind: InputPartKind,
    label: impl Into<String>,
    text: impl Into<String>,
    source: Option<String>,
) -> InputPart {
    let text = text.into();
    InputPart {
        kind,
        label: label.into(),
        approx_tokens: u32::try_from(text.chars().count().div_ceil(4)).unwrap_or(u32::MAX),
        text,
        source,
    }
}

/// Returns once anything is recorded after `seq`, or `Err(Interrupted)` when the run stops.
pub(crate) async fn changed_since(host: &Host, seq: Option<u64>) -> EngineResult<()> {
    loop {
        if host.read(|state| state.seq) != seq {
            return Ok(());
        }
        tokio::select! {
            () = host.interrupted().cancelled() => return Err(EngineError::Interrupted),
            () = tokio::time::sleep(POLL) => {}
        }
    }
}

/// Waits until `done` holds for the state, or the run stops.
pub(crate) async fn wait_until(host: &Host, done: impl Fn(&RunState) -> bool) -> EngineResult<()> {
    loop {
        let (holds, seq) = host.read(|state| (done(state), state.seq));
        if holds {
            return Ok(());
        }
        changed_since(host, seq).await?;
    }
}
