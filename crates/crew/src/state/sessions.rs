//! Every session the run started, with its vendor session id and how it ended.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::event::AgentEvent;
use argonauts_core::events::RunEvent;
use argonauts_core::events::session::{EndReason, SessionEnd, SessionEvent, SessionStarted};
use argonauts_core::names::SessionKey;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sessions {
    pub by_key: BTreeMap<SessionKey, SessionRecord>,
    /// The exact model each session's harness reported at its start, which catches an alias
    /// that moved to a new model. The outcome log keeps it per card.
    #[serde(default)]
    pub models: BTreeMap<SessionKey, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub key: SessionKey,
    pub card: Option<CardId>,
    /// The newest start. A resumed session keeps its key and records `Started` again.
    pub started: SessionStarted,
    /// The seq of its first `Session::Started`.
    pub since: u64,
    /// How many times it started, 1 unless it was resumed after a pause or a crash.
    pub starts: u32,
    pub vendor_session: Option<String>,
    pub transcript: Option<PathBuf>,
    /// The newest context fill the harness reported, in tokens.
    pub context_used: Option<u64>,
    pub end: Option<SessionEnd>,
    /// It ended `Stopped` because the whole run stopped, so a reopened run resumes it.
    #[serde(default)]
    pub stopped_with_run: bool,
}

impl SessionRecord {
    /// Ended because the run paused or shut down: its step resumes it, never restarts it.
    pub fn paused(&self) -> bool {
        matches!(
            self.end,
            Some(SessionEnd {
                reason: EndReason::Paused,
                ..
            })
        )
    }

    /// Still running, paused, or stopped with the run, so its step resumes it when re-entered.
    pub fn resumable(&self) -> bool {
        self.end.is_none() || self.paused() || self.stopped_with_run
    }
}

impl Sessions {
    pub fn apply(&mut self, event: &SessionEvent, envelope: &RunEvent) {
        let Some(key) = &envelope.session else {
            return;
        };
        match event {
            SessionEvent::Started(started) => match self.by_key.get_mut(key) {
                Some(record) => {
                    record.started = (**started).clone();
                    record.starts += 1;
                    record.end = None;
                    record.stopped_with_run = false;
                    if started.resumed_from.is_some() {
                        record.vendor_session = started.resumed_from.clone();
                    }
                }
                None => {
                    let record = SessionRecord {
                        key: key.clone(),
                        card: envelope.card_id.clone(),
                        started: (**started).clone(),
                        since: envelope.seq,
                        starts: 1,
                        vendor_session: started.resumed_from.clone(),
                        transcript: None,
                        context_used: None,
                        end: None,
                        stopped_with_run: false,
                    };
                    self.by_key.insert(key.clone(), record);
                }
            },
            SessionEvent::VendorSession { id, transcript } => {
                if let Some(record) = self.by_key.get_mut(key) {
                    record.vendor_session = Some(id.clone());
                    if transcript.is_some() {
                        record.transcript = transcript.clone();
                    }
                }
            }
            SessionEvent::Ended(end) => {
                if let Some(record) = self.by_key.get_mut(key) {
                    if end.vendor_session.is_some() {
                        record.vendor_session = end.vendor_session.clone();
                    }
                    record.end = Some((**end).clone());
                }
            }
            SessionEvent::Input(_)
            | SessionEvent::Queued(_)
            | SessionEvent::Delivered { .. }
            | SessionEvent::FileDiff(_)
            | SessionEvent::Limit(_) => {}
        }
    }

    pub fn apply_agent(&mut self, event: &AgentEvent, envelope: &RunEvent) {
        let Some(record) = envelope
            .session
            .as_ref()
            .and_then(|key| self.by_key.get_mut(key))
        else {
            return;
        };
        if let AgentEvent::Init {
            model: Some(model), ..
        } = event
        {
            self.models.insert(record.key.clone(), model.clone());
        }
        match event {
            AgentEvent::Init {
                session_id: Some(id),
                ..
            } if record.vendor_session.is_none() => record.vendor_session = Some(id.clone()),
            AgentEvent::ContextFill { used, .. } => record.context_used = Some(*used),
            _ => {}
        }
    }

    pub fn get(&self, key: &SessionKey) -> Option<&SessionRecord> {
        self.by_key.get(key)
    }

    /// Started and not ended: after a crash, these are the ones to resume.
    pub fn running(&self) -> impl Iterator<Item = &SessionRecord> {
        self.by_key.values().filter(|session| session.end.is_none())
    }

    /// Every session in the order it first started.
    pub fn in_order(&self) -> Vec<&SessionRecord> {
        let mut sessions: Vec<&SessionRecord> = self.by_key.values().collect();
        sessions.sort_by_key(|session| session.since);
        sessions
    }
}
