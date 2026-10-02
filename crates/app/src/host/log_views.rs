//! Status, diff, and raw views built from a run's files alone, for when no engine is running:
//! a parked host answers with these, and so does a client that finds no supervisor.

use std::path::{Path, PathBuf};

use argonauts_core::api::{ApiError, DiffScope, DiffView, RawView, StatusSummary};
use argonauts_core::events::card::CardEvent;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal;
use argonauts_core::layout::{run_files, session_dir, session_files};
use argonauts_core::names::SessionKey;
use argonauts_core::{CardId, DiffStat, RunId};
use argonauts_crew::status::{self, Viewer};

/// How much of a session's raw output a view holds.
const RAW_LINES: usize = 400;

pub(crate) fn status(run_dir: &Path, run: &RunId) -> Result<StatusSummary, ApiError> {
    let events = events_of(run_dir)?;
    if events.is_empty() {
        return Ok(StatusSummary {
            run: Some(run.clone()),
            ..StatusSummary::default()
        });
    }
    Ok(status::summary(&events, &Viewer::User))
}

/// The card's last diff. `shown` writes a path for an error message.
pub(crate) fn diff(
    run_dir: &Path,
    card: &CardId,
    scope: DiffScope,
    shown: impl Fn(&Path) -> String,
) -> Result<DiffView, ApiError> {
    if scope == DiffScope::LastTurn {
        return Err(ApiError::NotNow(
            "the last turn's diff needs the run's supervisor; /resume it first".to_owned(),
        ));
    }
    let (stat, path) = last_card_diff(&events_of(run_dir)?, card)
        .ok_or_else(|| ApiError::NotNow(format!("{card} has no diff yet")))?;
    let path = run_dir.join(path);
    let text = std::fs::read_to_string(&path)
        .map_err(|err| ApiError::Supervisor(format!("reading {}: {err}", shown(&path))))?;
    Ok(DiffView {
        card: card.clone(),
        stat,
        text,
        path,
    })
}

pub(crate) fn raw(run_dir: &Path, run: &RunId, session: &SessionKey) -> Result<RawView, ApiError> {
    let dir = session_dir(run_dir, session);
    if !dir.is_dir() {
        return Err(ApiError::Refused(format!(
            "run {run} has no session {session}"
        )));
    }
    let path = dir.join(session_files::RAW_STDOUT);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(unreadable(err)),
    };
    let all: Vec<&str> = text.lines().collect();
    let keep = all.len().saturating_sub(RAW_LINES);
    let vendor_transcript = events_of(run_dir)?
        .iter()
        .rev()
        .filter(|event| event.session.as_ref() == Some(session))
        .find_map(|event| match &event.kind {
            RunEventKind::Session(SessionEvent::VendorSession { transcript, .. }) => {
                transcript.clone()
            }
            _ => None,
        });
    Ok(RawView {
        session: session.clone(),
        lines: all[keep..].iter().map(|line| (*line).to_owned()).collect(),
        truncated: keep > 0,
        path,
        vendor_transcript,
    })
}

fn events_of(run_dir: &Path) -> Result<Vec<RunEvent>, ApiError> {
    let path = run_dir.join(run_files::EVENTS);
    if !path.exists() {
        return Ok(Vec::new());
    }
    journal::read_all(&path)
        .map(|back| back.events)
        .map_err(unreadable)
}

fn last_card_diff(events: &[RunEvent], card: &CardId) -> Option<(DiffStat, PathBuf)> {
    events
        .iter()
        .rev()
        .filter(|event| event.card_id.as_ref() == Some(card))
        .find_map(|event| match &event.kind {
            RunEventKind::Card(CardEvent::Diff { stat, path }) => Some((*stat, path.clone())),
            _ => None,
        })
}

fn unreadable(err: impl std::fmt::Display) -> ApiError {
    ApiError::Supervisor(format!("{err:#}"))
}
