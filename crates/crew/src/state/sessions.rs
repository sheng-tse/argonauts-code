//! Every session the run started, with its CLI session id and how it ended.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::base::names::{ModelRef, SessionKey};
use argonauts_core::runs::event::AgentEvent;
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::events::session::{
    CliWorkflowRun, EndReason, SessionEnd, SessionEvent, SessionStarted,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sessions {
    pub by_key: BTreeMap<SessionKey, SessionRecord>,
    /// The exact model each session's CLI reported at its start, which catches an alias
    /// that moved to a new model. The outcome log keeps it per task.
    #[serde(default)]
    pub models: BTreeMap<SessionKey, String>,
    /// Whether each worker's newest session ended on a sign-in error, so a resumed run asks
    /// that worker's CLI whether it's logged out before starting it again.
    #[serde(default)]
    pub sign_in_failed: BTreeMap<ModelRef, bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub key: SessionKey,
    pub task: Option<TaskId>,
    /// The newest start. A resumed session keeps its key and records `Started` again.
    pub started: SessionStarted,
    /// The seq of its first `Session::Started`.
    pub since: u64,
    /// How many times it started, 1 unless it was resumed after a pause or a crash.
    pub starts: u32,
    pub cli_session: Option<String>,
    pub transcript: Option<PathBuf>,
    /// The newest context fill the CLI reported, in tokens.
    pub context_used: Option<u64>,
    /// The window that fill was of, when it came with one.
    #[serde(default)]
    pub context_window: Option<u64>,
    pub end: Option<SessionEnd>,
    /// It ended `Stopped` because the whole run stopped, so a reopened run resumes it.
    #[serde(default)]
    pub stopped_with_run: bool,
    /// The worker its step asked, once the session fell back to another agent after an agent
    /// failure, or another ran it from the start because the asked one was out. The step finds the
    /// session by it.
    #[serde(default)]
    pub asked: Option<ModelRef>,
    /// Each time it ended for an agent reason: the worker then, and the seq of the end.
    #[serde(default)]
    pub agent_failures: Vec<(ModelRef, u64)>,
    /// The files its edit tools wrote, as its CLI named them: from its folder, or absolute.
    #[serde(default)]
    pub written: BTreeSet<String>,
    /// How many times its CLI retried after losing the network, which says why one that ran
    /// out of time never answered.
    #[serde(default)]
    pub network_retries: u32,
    /// The workflow its own CLI ran, as it last reported it, which the steps you require are
    /// checked against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<Box<CliWorkflowRun>>,
    /// The folder its CLI last worked in, as its launch says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
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

    /// The worker its step asked for, whoever runs it now.
    pub fn asked_worker(&self) -> &ModelRef {
        self.asked.as_ref().unwrap_or(&self.started.worker)
    }

    /// Still running, paused, stopped with the run, or cut by a spend limit with its CLI
    /// session kept, so its step resumes it when re-entered: one a limit cut carries on once
    /// you raise the limit, rather than being asked again from the start.
    pub fn resumable(&self) -> bool {
        self.end.is_none() || self.paused() || self.stopped_with_run || self.cut_by_limit()
    }

    /// Ended at a spend limit, such as its CLI's own cap, with a CLI session to carry on.
    fn cut_by_limit(&self) -> bool {
        matches!(
            self.end,
            Some(SessionEnd {
                reason: EndReason::LimitHit { .. },
                cli_session: Some(_),
                ..
            })
        )
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
                    if record.asked.is_none() {
                        record.asked = started.asked.clone().or_else(|| {
                            (started.worker != record.started.worker)
                                .then(|| record.started.worker.clone())
                        });
                    }
                    record.started = (**started).clone();
                    record.starts += 1;
                    record.end = None;
                    record.stopped_with_run = false;
                    if started.resumed_from.is_some() {
                        record.cli_session = started.resumed_from.clone();
                    }
                }
                None => {
                    let record = SessionRecord {
                        key: key.clone(),
                        task: envelope.task_id.clone(),
                        started: (**started).clone(),
                        since: envelope.seq,
                        starts: 1,
                        cli_session: started.resumed_from.clone(),
                        transcript: None,
                        context_used: None,
                        context_window: None,
                        end: None,
                        stopped_with_run: false,
                        asked: started.asked.clone(),
                        agent_failures: Vec::new(),
                        written: BTreeSet::new(),
                        network_retries: 0,
                        workflow: None,
                        workdir: None,
                    };
                    self.by_key.insert(key.clone(), record);
                }
            },
            SessionEvent::CliSession { id, transcript } => {
                if let Some(record) = self.by_key.get_mut(key) {
                    record.cli_session = Some(id.clone());
                    if transcript.is_some() {
                        record.transcript = transcript.clone();
                    }
                }
            }
            SessionEvent::Ended(end) => {
                if let Some(record) = self.by_key.get_mut(key) {
                    if end.cli_session.is_some() {
                        record.cli_session = end.cli_session.clone();
                    }
                    let worker = record.started.worker.clone();
                    self.sign_in_failed
                        .insert(worker.clone(), end.reason.is_sign_in_error());
                    if end.reason.is_agent_failure() {
                        record.agent_failures.push((worker, envelope.seq));
                    }
                    record.end = Some((**end).clone());
                }
            }
            SessionEvent::FileDiff(diff) => {
                if let Some(record) = self.by_key.get_mut(key) {
                    record.written.insert(diff.path.clone());
                }
            }
            SessionEvent::Input(input) => {
                if let Some(record) = self.by_key.get_mut(key) {
                    record.workdir = Some(input.launch.cwd.clone());
                }
            }
            SessionEvent::CarryOn(_)
            | SessionEvent::Queued(_)
            | SessionEvent::Delivered { .. }
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
            } if record.cli_session.is_none() => record.cli_session = Some(id.clone()),
            AgentEvent::ContextFill { used, size, .. } => {
                record.context_used = Some(*used);
                record.context_window = size.or(record.context_window);
            }
            AgentEvent::FileChanged { path, .. } => {
                record.written.insert(path.clone());
            }
            AgentEvent::Workflow { workflow } => record.workflow = Some(workflow.clone()),
            AgentEvent::Retry {
                reason: argonauts_core::RetryReason::Network,
                ..
            } => record.network_retries += 1,
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
